//! A made-up tenant for tests, snapshots and `--fake`. Fixture JSON goes
//! through the same row builders as live responses, and writes change it,
//! so everything fablic does can be tried without Fabric.

mod data;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::rows::{self, WsData};
use crate::resources::{Kind, Row, job_type_for};
use crate::source::{Action, DataSource, Movable, PREVIEW_BYTES, Part, Query, Ref, table_of};

/// How a fake profile's sign-in goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeAuth {
    SignedIn,
    /// Needs `l`; signing in fixes it.
    Expired,
    /// Fails for a reason signing in won't fix.
    Broken(String),
}

pub struct FakeSource {
    state: Mutex<State>,
    /// Set when time passes for real (`--fake`); tests keep a frozen clock.
    started: Option<Instant>,
    /// Whether this identity may list domains (a Fabric administrator).
    admin: bool,
}

/// Everything writes can change.
struct State {
    spaces: HashMap<String, WsData>,
    roles: HashMap<String, Vec<Value>>,
    shortcuts: HashMap<String, Vec<Value>>,
    /// Runs started with `r`, and when.
    started_runs: Vec<(Value, DateTime<Utc>)>,
    cancelled: HashSet<String>,
    auth: FakeAuth,
    next_id: u32,
}

impl Default for FakeSource {
    fn default() -> FakeSource {
        FakeSource {
            state: Mutex::new(State {
                spaces: data::spaces().into_iter().collect(),
                roles: data::roles().into_iter().collect(),
                shortcuts: data::shortcuts().into_iter().collect(),
                started_runs: Vec::new(),
                cancelled: HashSet::new(),
                auth: FakeAuth::SignedIn,
                next_id: 100,
            }),
            started: None,
            admin: false,
        }
    }
}

/// The fixed moment fake runs are timed against.
pub fn now() -> DateTime<Utc> {
    rows::parse_utc("2026-10-09T13:10:00").unwrap_or_default()
}

/// A run started with `r` waits this long, then runs for `RUN_FOR`.
const QUEUED_FOR: i64 = 3;
const RUN_FOR: i64 = 20;

impl FakeSource {
    /// Time passes: in-progress runs grow longer and runs started with `r`
    /// queue, run, and finish.
    pub fn live(mut self) -> FakeSource {
        self.started = Some(Instant::now());
        self
    }

    /// Can list domains.
    pub fn admin(mut self) -> FakeSource {
        self.admin = true;
        self
    }

    pub fn auth(self, auth: FakeAuth) -> FakeSource {
        self.lock().auth = auth;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> DateTime<Utc> {
        let passed = self.started.map_or(0, |s| s.elapsed().as_millis() as i64);
        now() + chrono::Duration::milliseconds(passed)
    }

    fn rows(&self, q: &Query) -> Result<Vec<Row>> {
        self.check()?;
        let ws = q.scope.workspace.as_ref().map(|w| w.id.as_str());
        let folder = q.scope.folder.as_ref().map(|f| f.id.as_str());
        let state = self.lock();
        let space = |id: &str| state.spaces.get(id).cloned().unwrap_or_default();
        Ok(match q.kind {
            Kind::Workspaces => rows::workspaces(&data::workspaces(), &data::capacities()),
            Kind::Items => match ws {
                Some(id) if q.is_flat() => {
                    rows::items_flat(&space(id), folder, q.item_type.as_deref())
                }
                Some(id) => rows::items_level(&space(id), folder),
                None => {
                    let spaces: Vec<_> = data::workspaces()
                        .into_iter()
                        .map(|w| {
                            let d = space(w["id"].as_str().unwrap_or_default());
                            (w, d)
                        })
                        .collect();
                    rows::items_all(&spaces, q.item_type.as_deref())
                }
            },
            Kind::Capacities => rows::capacities(&data::capacities()),
            Kind::Connections => rows::connections(&data::connections()),
            Kind::Gateways => rows::gateways(&data::gateways()),
            Kind::Domains if self.admin => rows::domains(&data::domains()),
            Kind::Domains => bail!("403 /admin/domains: InsufficientPrivileges"),
            Kind::Roles => rows::roles(
                state
                    .roles
                    .get(ws.unwrap_or_default())
                    .map_or(&[][..], Vec::as_slice),
            ),
            Kind::Jobs => match &q.scope.item {
                Some(item) => {
                    let job_type = job_type_for(&item.item_type).unwrap_or("RunNotebook");
                    rows::jobs(&self.runs(&state, &item.id, job_type), None, self.now())
                }
                None => {
                    let mut out = Vec::new();
                    for i in space(ws.unwrap_or_default()).items {
                        if let Some(job_type) = job_type_for(i["type"].as_str().unwrap_or("")) {
                            let id = i["id"].as_str().unwrap_or("");
                            let runs = self.runs(&state, id, job_type);
                            out.extend(rows::jobs(&runs, Some(&i), self.now()));
                        }
                    }
                    rows::sort_runs(&mut out);
                    out
                }
            },
            Kind::Schedules => {
                let item = q.scope.item.as_ref().map(|i| i.id.as_str()).unwrap_or("");
                rows::schedules(&data::schedules(item))
            }
            Kind::OneLake => {
                let item = q.scope.item.as_ref().map(|i| i.id.as_str()).unwrap_or("");
                onelake(item, q.scope.path.as_deref().unwrap_or(""))
            }
            Kind::Shortcuts => {
                let item = q.scope.item.as_ref().map(|i| i.id.as_str()).unwrap_or("");
                rows::shortcuts(state.shortcuts.get(item).map_or(&[][..], Vec::as_slice))
            }
        })
    }

    /// An item's runs as Fabric would list them right now: the seeded
    /// history plus runs started with `r`, cancels applied.
    fn runs(&self, state: &State, item: &str, job_type: &str) -> Vec<Value> {
        let now = self.now();
        let mut out: Vec<Value> = state
            .started_runs
            .iter()
            .filter(|(r, _)| r["itemId"] == item)
            .map(|(r, at)| progress(r.clone(), *at, now))
            .collect();
        out.extend(data::runs(item, job_type));
        for r in out.iter_mut() {
            let id = r["id"].as_str().unwrap_or_default();
            if state.cancelled.contains(id) && rows_pending(r) {
                r["status"] = "Cancelled".into();
                r["endTimeUtc"] = now.format("%Y-%m-%dT%H:%M:%S").to_string().into();
            }
        }
        rows::mark_cancelled(&mut out, &data::sessions());
        out
    }

    fn check(&self) -> Result<()> {
        match &self.lock().auth {
            FakeAuth::SignedIn => Ok(()),
            FakeAuth::Expired => bail!(
                "AADSTS700082: The refresh token has expired due to inactivity. \
                 Run `fablic login` to sign in again."
            ),
            FakeAuth::Broken(why) => bail!("{why}"),
        }
    }

    fn act_now(&self, action: &Action) -> Result<()> {
        let mut state = self.lock();
        let state = &mut *state;
        let id = {
            state.next_id += 1;
            state.next_id
        };
        let ws = action.workspace().to_owned();
        let space = state.spaces.entry(ws.clone()).or_default();
        match action {
            Action::RunJob { item, job_type, .. } => {
                let run = json!({"id": format!("{}-run{id}", item.id), "itemId": item.id,
                                 "jobType": job_type, "invokeType": "Manual"});
                state.started_runs.insert(0, (run, self.now()));
            }
            Action::CancelJob { job, .. } => {
                state.cancelled.insert(job.clone());
            }
            Action::RenameItem { item, name, .. } => {
                let taken = space.items.iter().any(|i| {
                    i["id"] != item.id.as_str()
                        && i["type"] == item.item_type.as_str()
                        && i["displayName"] == name.as_str()
                });
                if taken {
                    bail!(
                        "409 /workspaces/{ws}/items/{}: ItemDisplayNameAlreadyInUse \
                         Requested '{name}' is already in use",
                        item.id
                    );
                }
                find(&mut space.items, &item.id)?["displayName"] = name.as_str().into();
            }
            Action::DescribeItem {
                item, description, ..
            } => {
                find(&mut space.items, &item.id)?["description"] = description.as_str().into();
            }
            Action::DeleteItem { item, .. } => {
                find(&mut space.items, &item.id)?;
                space.items.retain(|i| i["id"] != item.id.as_str());
            }
            Action::NewFolder { parent, name, .. } => {
                let parent = parent.as_ref().map(|p| p.id.as_str());
                if siblings(space, parent).any(|f| f["displayName"] == name.as_str()) {
                    bail!(
                        "409 /workspaces/{ws}/folders: FolderDisplayNameAlreadyInUse \
                         There's already a folder named '{name}' here"
                    );
                }
                let mut f = json!({"id": format!("f-{id}"), "displayName": name,
                                   "workspaceId": ws});
                if let Some(p) = parent {
                    f["parentFolderId"] = p.into();
                }
                space.folders.push(f);
            }
            Action::NewItem {
                folder,
                item_type,
                name,
                ..
            } => {
                let taken = space
                    .items
                    .iter()
                    .any(|i| i["type"] == item_type.as_str() && i["displayName"] == name.as_str());
                if taken {
                    bail!(
                        "409 /workspaces/{ws}/items: ItemDisplayNameAlreadyInUse \
                         Requested '{name}' is already in use"
                    );
                }
                let mut i = json!({"id": format!("it-{id}"), "displayName": name,
                                   "type": item_type, "description": "", "workspaceId": ws});
                if let Some(f) = folder {
                    i["folderId"] = f.id.as_str().into();
                }
                space.items.push(i);
            }
            Action::RenameFolder { folder, name, .. } => {
                find(&mut space.folders, &folder.id)?["displayName"] = name.as_str().into();
            }
            Action::DeleteFolder { folder, .. } => {
                find(&mut space.folders, &folder.id)?;
                let used = space
                    .items
                    .iter()
                    .any(|i| i["folderId"] == folder.id.as_str())
                    || space
                        .folders
                        .iter()
                        .any(|f| f["parentFolderId"] == folder.id.as_str());
                if used {
                    bail!(
                        "400 /workspaces/{ws}/folders/{}: FolderNotEmpty \
                         The folder must be empty to delete it",
                        folder.id
                    );
                }
                space.folders.retain(|f| f["id"] != folder.id.as_str());
            }
            Action::Move { what, to, .. } => move_to(space, &ws, what, to.as_ref())?,
            Action::AddRole {
                principal, role, ..
            } => {
                let list = state.roles.entry(ws.clone()).or_default();
                let has = |r: &&Value| {
                    r["principal"]["displayName"] == principal.as_str()
                        || r["principal"]["userDetails"]["userPrincipalName"] == principal.as_str()
                };
                if list.iter().any(|r| has(&r)) {
                    bail!(
                        "409 /workspaces/{ws}/roleAssignments: \
                         PrincipalAlreadyHasWorkspaceRolePermissions \
                         {principal} already has a role in this workspace"
                    );
                }
                let p = if principal.contains('@') {
                    json!({"id": format!("p-{id}"), "type": "User",
                           "userDetails": {"userPrincipalName": principal}})
                } else {
                    json!({"id": format!("p-{id}"), "displayName": principal, "type": "Group"})
                };
                list.push(json!({"id": format!("r-{id}"), "role": role, "principal": p}));
            }
            Action::SetRole {
                assignment, role, ..
            } => {
                let list = state.roles.entry(ws.clone()).or_default();
                find(list, &assignment.id)?["role"] = role.as_str().into();
            }
            Action::RemoveRole { assignment, .. } => {
                let list = state.roles.entry(ws.clone()).or_default();
                find(list, &assignment.id)?;
                list.retain(|r| r["id"] != assignment.id.as_str());
            }
            Action::DeleteShortcut {
                item, path, name, ..
            } => {
                let list = state.shortcuts.entry(item.id.clone()).or_default();
                let before = list.len();
                list.retain(|s| !(s["path"] == path.as_str() && s["name"] == name.as_str()));
                if list.len() == before {
                    bail!("404 shortcuts/{path}/{name}: EntityNotFound");
                }
            }
        }
        Ok(())
    }
}

/// A started run's state `at` seconds after it was started.
fn progress(mut run: Value, at: DateTime<Utc>, now: DateTime<Utc>) -> Value {
    let fmt = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let began = at + chrono::Duration::seconds(QUEUED_FOR);
    let done = began + chrono::Duration::seconds(RUN_FOR);
    let status = if now < began {
        "NotStarted"
    } else if now < done {
        "InProgress"
    } else {
        "Completed"
    };
    run["status"] = status.into();
    if now >= began {
        run["startTimeUtc"] = fmt(began).into();
    }
    if now >= done {
        run["endTimeUtc"] = fmt(done).into();
    }
    run
}

fn rows_pending(run: &Value) -> bool {
    matches!(run["status"].as_str(), Some("NotStarted" | "InProgress"))
}

fn find<'a>(list: &'a mut [Value], id: &str) -> Result<&'a mut Value> {
    list.iter_mut()
        .find(|v| v["id"] == id)
        .ok_or_else(|| anyhow::anyhow!("404 {id}: EntityNotFound"))
}

fn siblings<'a>(space: &'a WsData, parent: Option<&'a str>) -> impl Iterator<Item = &'a Value> {
    space
        .folders
        .iter()
        .filter(move |f| f["parentFolderId"].as_str() == parent)
}

fn move_to(space: &mut WsData, ws: &str, what: &Movable, to: Option<&Ref>) -> Result<()> {
    let to = to.map(|t| t.id.as_str());
    match what {
        Movable::Item(item) => {
            let i = find(&mut space.items, &item.id)?;
            match to {
                Some(t) => i["folderId"] = t.into(),
                None => {
                    if let Some(o) = i.as_object_mut() {
                        o.remove("folderId");
                    }
                }
            }
        }
        Movable::Folder(f) => {
            // Not into itself or anything under it.
            let mut at = to.map(str::to_owned);
            while let Some(id) = at {
                if id == f.id {
                    bail!(
                        "400 /workspaces/{ws}/folders/{}/move: InvalidFolderHierarchy \
                         A folder can't move into itself",
                        f.id
                    );
                }
                at = space
                    .folders
                    .iter()
                    .find(|x| x["id"] == id.as_str())
                    .and_then(|x| x["parentFolderId"].as_str())
                    .map(str::to_owned);
            }
            let name = f.name.rsplit('/').next().unwrap_or(&f.name);
            if siblings(space, to).any(|x| x["displayName"] == name && x["id"] != f.id.as_str()) {
                bail!(
                    "409 /workspaces/{ws}/folders/{}/move: FolderDisplayNameAlreadyInUse \
                     There's already a folder named '{name}' there",
                    f.id
                );
            }
            let folder = find(&mut space.folders, &f.id)?;
            match to {
                Some(t) => folder["parentFolderId"] = t.into(),
                None => {
                    if let Some(o) = folder.as_object_mut() {
                        o.remove("parentFolderId");
                    }
                }
            }
        }
    }
    Ok(())
}

/// DFS-style listing of a directory in a fake lakehouse or warehouse.
fn onelake(item: &str, dir: &str) -> Vec<Row> {
    let paths: Vec<Value> = data::listing(item, dir)
        .into_iter()
        .map(|(name, is_dir, len)| {
            let path = if dir.is_empty() {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            };
            json!({"name": format!("{item}/{path}"), "isDirectory": is_dir.to_string(),
                   "contentLength": len.to_string(),
                   "lastModified": "Fri, 09 Oct 2026 05:12:00 GMT"})
        })
        .collect();
    rows::onelake(&paths, item)
}

impl DataSource for FakeSource {
    fn fetch(&self, q: Query, _force: bool) -> BoxFuture<'static, Result<Vec<Row>>> {
        let rows = self.rows(&q);
        Box::pin(async move { rows })
    }

    fn preview(
        &self,
        _ws: String,
        _item: String,
        path: String,
        total: Option<u64>,
    ) -> BoxFuture<'static, Result<Part>> {
        let bytes = data::file(&path);
        let bytes = &bytes[..bytes.len().min(PREVIEW_BYTES)];
        let text = rows::preview_text(bytes, total, PREVIEW_BYTES);
        Box::pin(async move { Ok(Part { path, text }) })
    }

    fn schema(&self, _ws: String, _item: String, path: String) -> BoxFuture<'static, Result<Part>> {
        Box::pin(async move {
            let (schema, table) = table_of(&path)?;
            Ok(Part {
                path: format!("{schema}.{table}"),
                text: rows::schema_text(&data::columns(&schema, &table)),
            })
        })
    }

    /// Reports the size without writing anything, so tests don't touch disk.
    fn download(
        &self,
        _ws: String,
        _item: String,
        path: String,
        _dest: std::path::PathBuf,
    ) -> BoxFuture<'static, Result<u64>> {
        let n = data::file(&path).len() as u64;
        Box::pin(async move { Ok(n) })
    }

    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>> {
        let result = self
            .act_now(&action)
            .map(|()| format!("(fake) {}", action.describe()));
        Box::pin(async move { result })
    }

    fn definition(&self, ws: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>> {
        let found = self
            .lock()
            .spaces
            .get(&ws)
            .and_then(|d| d.items.iter().find(|i| i["id"] == item.as_str()).cloned());
        let result = match found {
            Some(i) => data::definition(&i),
            None => Err(anyhow::anyhow!(
                "404 /workspaces/{ws}/items/{item}: ItemNotFound"
            )),
        };
        Box::pin(async move { result })
    }

    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        let result = self.check();
        Box::pin(async move { result })
    }

    fn sign_in(&self) -> Option<BoxFuture<'static, Result<()>>> {
        let mut state = self.lock();
        let result = match &state.auth {
            FakeAuth::Broken(why) => Err(anyhow::anyhow!("{why}")),
            _ => {
                state.auth = FakeAuth::SignedIn;
                Ok(())
            }
        };
        Some(Box::pin(async move { result }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{ItemRef, Scope};

    fn ws(id: &str) -> Scope {
        Scope {
            workspace: Some(Ref {
                id: id.into(),
                name: id.into(),
            }),
            ..Scope::default()
        }
    }

    fn names(src: &FakeSource, q: Query) -> Vec<String> {
        src.rows(&q).unwrap().into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn writes_change_what_is_listed() {
        let src = FakeSource::default();
        let flat = Query {
            flat: true,
            ..Query::new(Kind::Items, ws("ws-3"))
        };
        src.act_now(&Action::NewItem {
            workspace: "ws-3".into(),
            folder: None,
            item_type: "Lakehouse".into(),
            name: "new_lh".into(),
        })
        .unwrap();
        assert!(names(&src, flat.clone()).contains(&"new_lh".to_owned()));
        // The same name twice is refused, as Fabric does.
        let again = src.act_now(&Action::NewItem {
            workspace: "ws-3".into(),
            folder: None,
            item_type: "Lakehouse".into(),
            name: "new_lh".into(),
        });
        assert!(format!("{:#}", again.unwrap_err()).contains("AlreadyInUse"));

        // Folders that hold something can't be deleted.
        let folder = Ref {
            id: "f-30".into(),
            name: "experiments".into(),
        };
        let delete = Action::DeleteFolder {
            workspace: "ws-3".into(),
            folder: folder.clone(),
        };
        assert!(src.act_now(&delete).is_err());
        let item = ItemRef {
            id: "it-42".into(),
            name: "try_window_functions".into(),
            item_type: "Notebook".into(),
        };
        src.act_now(&Action::Move {
            workspace: "ws-3".into(),
            what: Movable::Item(item),
            to: None,
        })
        .unwrap();
        src.act_now(&delete).unwrap();
        assert!(!names(&src, Query::new(Kind::Items, ws("ws-3"))).contains(&folder.name));
    }

    #[test]
    fn folders_cannot_move_under_themselves() {
        let src = FakeSource::default();
        let r = src.act_now(&Action::Move {
            workspace: "ws-2".into(),
            what: Movable::Folder(Ref {
                id: "f-20".into(),
                name: "silver".into(),
            }),
            to: Some(Ref {
                id: "f-22".into(),
                name: "silver/conformed/customer 360".into(),
            }),
        });
        assert!(format!("{:#}", r.unwrap_err()).contains("InvalidFolderHierarchy"));
    }

    #[test]
    fn started_runs_queue_then_run_and_can_be_cancelled() {
        let src = FakeSource::default();
        let item = ItemRef {
            id: "it-40".into(),
            name: "scratch".into(),
            item_type: "Notebook".into(),
        };
        src.act_now(&Action::RunJob {
            workspace: "ws-3".into(),
            item: item.clone(),
            job_type: "RunNotebook".into(),
        })
        .unwrap();
        let state = src.lock();
        let at = state.started_runs[0].1;
        let run = state.started_runs[0].0.clone();
        drop(state);
        let status = |secs| {
            progress(run.clone(), at, at + chrono::Duration::seconds(secs))["status"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(status(0), "NotStarted");
        assert_eq!(status(QUEUED_FOR + 1), "InProgress");
        assert_eq!(status(QUEUED_FOR + RUN_FOR), "Completed");

        let id = run["id"].as_str().unwrap().to_owned();
        src.act_now(&Action::CancelJob {
            workspace: "ws-3".into(),
            item,
            job: id.clone(),
        })
        .unwrap();
        let runs = src.runs(&src.lock(), "it-40", "RunNotebook");
        assert_eq!(runs[0]["id"], id.as_str());
        assert_eq!(runs[0]["status"], "Cancelled");
    }

    #[test]
    fn a_cancel_only_the_spark_session_knows_about_shows() {
        let src = FakeSource::default();
        let runs = src.runs(&src.lock(), "it-21", "RunNotebook");
        let run = runs.iter().find(|r| r["id"] == "it-21-11").unwrap();
        assert_eq!(run["status"], "Cancelled");
    }

    #[test]
    fn expired_sign_in_is_fixed_by_signing_in() {
        let src = FakeSource::default().auth(FakeAuth::Expired);
        let e = format!("{:#}", src.check().unwrap_err());
        assert_eq!(crate::app::Auth::from_error(&e), crate::app::Auth::Expired);
        assert!(
            src.rows(&Query::new(Kind::Workspaces, Scope::default()))
                .is_err()
        );
        drop(src.sign_in());
        assert!(src.check().is_ok());
    }
}
