//! Fabric-backed `DataSource`.

pub mod client;
pub mod fake;
pub mod graph;
pub mod onelake;
pub mod rows;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use futures::future::BoxFuture;
use futures::{StreamExt, TryStreamExt};
use reqwest::Method;
use serde_json::{Value, json};

use crate::auth::{FABRIC, TokenProvider};
use crate::resources::{Kind, Row, job_type_for};
use crate::source::{Action, DataSource, Movable, PREVIEW_BYTES, Part, Query, table_of};
pub use client::Client;
use graph::Graph;
use onelake::OneLake;
use rows::WsData;

/// How long a workspace's folder/item listing is reused while navigating its tree.
const WS_TTL: Duration = Duration::from_secs(60);
/// Parallel requests when listing items across every workspace.
const FAN_OUT: usize = 6;
/// Parallel requests when listing runs of every runnable item in a workspace.
const JOB_FAN_OUT: usize = 4;
/// Pages of Spark sessions read to spot cancelled runs; newest come first.
const SESSION_PAGES: usize = 3;
/// Capacity names change rarely, and listing capacities can take seconds.
const CAP_TTL: Duration = Duration::from_secs(600);
/// How long the workspace list waits for capacity names before showing ids.
const CAP_WAIT: Duration = Duration::from_millis(1500);

pub struct FabricSource {
    inner: Arc<Inner>,
}

type Cached<T> = Arc<Mutex<Option<(Instant, T)>>>;

struct Inner {
    client: Client,
    lake: OneLake,
    graph: Graph,
    tokens: Arc<dyn TokenProvider>,
    spaces: Mutex<HashMap<String, (Instant, Arc<WsData>)>>,
    capacities: Cached<Vec<Value>>,
}

impl FabricSource {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> FabricSource {
        FabricSource::with_clients(
            Client::new(tokens.clone()),
            OneLake::new(tokens.clone()),
            Graph::new(tokens.clone()),
            tokens,
        )
    }

    pub fn with_clients(
        client: Client,
        lake: OneLake,
        graph: Graph,
        tokens: Arc<dyn TokenProvider>,
    ) -> FabricSource {
        FabricSource {
            inner: Arc::new(Inner {
                client,
                lake,
                graph,
                tokens,
                spaces: Mutex::new(HashMap::new()),
                capacities: Arc::default(),
            }),
        }
    }
}

impl Inner {
    async fn workspace(&self, id: &str, force: bool) -> Result<Arc<WsData>> {
        if !force
            && let Some((at, data)) = self.spaces.lock().unwrap().get(id)
            && at.elapsed() < WS_TTL
        {
            return Ok(data.clone());
        }
        let folders_url = format!("workspaces/{id}/folders?recursive=true");
        let items_url = format!("workspaces/{id}/items?recursive=true");
        let (folders, items) = futures::try_join!(
            self.client.list(&folders_url, "value"),
            self.client.list(&items_url, "value"),
        )?;
        let data = Arc::new(WsData { folders, items });
        self.spaces
            .lock()
            .unwrap()
            .insert(id.to_owned(), (Instant::now(), data.clone()));
        Ok(data)
    }

    /// Capacities for naming workspace capacities. Never holds up the caller
    /// for long: a slow listing keeps running in the background and fills the
    /// cache for the next refresh, and until then the old (or no) names are used.
    async fn capacity_names(&self, force: bool) -> Vec<Value> {
        let cached = self.capacities.lock().unwrap().clone();
        if !force
            && let Some((at, caps)) = &cached
            && at.elapsed() < CAP_TTL
        {
            return caps.clone();
        }
        let (client, cache) = (self.client.clone(), self.capacities.clone());
        let task = tokio::spawn(async move {
            let caps = client.list("capacities", "value").await?;
            *cache.lock().unwrap() = Some((Instant::now(), caps.clone()));
            Ok::<_, anyhow::Error>(caps)
        });
        match tokio::time::timeout(CAP_WAIT, task).await {
            Ok(Ok(Ok(caps))) => caps,
            _ => cached.map(|(_, c)| c).unwrap_or_default(),
        }
    }

    async fn fetch(&self, q: Query, force: bool) -> Result<Vec<Row>> {
        let c = &self.client;
        let ws = q.scope.workspace.as_ref().map(|w| w.id.as_str());
        let folder = q.scope.folder.as_ref().map(|f| f.id.as_str());
        Ok(match q.kind {
            Kind::Workspaces => {
                let (list, caps) =
                    futures::join!(c.list("workspaces", "value"), self.capacity_names(force));
                rows::workspaces(&list?, &caps)
            }
            Kind::Items => match ws {
                Some(id) => {
                    let data = self.workspace(id, force).await?;
                    if q.tree {
                        rows::items_tree(&data, folder)
                    } else if q.is_flat() {
                        rows::items_flat(&data, folder, q.item_type.as_deref())
                    } else {
                        rows::items_level(&data, folder)
                    }
                }
                None => {
                    let list = c.list("workspaces", "value").await?;
                    let spaces: Vec<(Value, WsData)> = futures::stream::iter(list)
                        .map(|w| async move {
                            let id = w.get("id").and_then(Value::as_str).unwrap_or_default();
                            let data = self.workspace(id, force).await?;
                            Ok::<_, anyhow::Error>((w, (*data).clone()))
                        })
                        .buffer_unordered(FAN_OUT)
                        .try_collect()
                        .await?;
                    rows::items_all(&spaces, q.item_type.as_deref())
                }
            },
            Kind::Roles => {
                let id = ws.context("roles need a workspace")?;
                rows::roles(
                    &c.list(&format!("workspaces/{id}/roleAssignments"), "value")
                        .await?,
                )
            }
            Kind::Capacities => rows::capacities(&c.list("capacities", "value").await?),
            Kind::Connections => rows::connections(&c.list("connections", "value").await?),
            Kind::Gateways => rows::gateways(&c.list("gateways", "value").await?),
            Kind::Domains => rows::domains(&c.list("admin/domains", "domains").await?),
            Kind::Jobs => {
                let ws = ws.context("jobs need a workspace")?;
                match &q.scope.item {
                    Some(item) => {
                        let list = self.runs(ws, &item.id, &item.item_type).await?;
                        rows::jobs(&list, None, chrono::Utc::now())
                    }
                    None => self.workspace_runs(ws, force).await?,
                }
            }
            Kind::Schedules => {
                let ws = ws.context("schedules need a workspace")?;
                let item = q.scope.item.as_ref().context("schedules need an item")?;
                let job_type = job_type_for(&item.item_type)
                    .with_context(|| format!("{} items have no schedules", item.item_type))?;
                let path = format!(
                    "workspaces/{ws}/items/{}/jobs/{job_type}/schedules",
                    item.id
                );
                rows::schedules(&c.list(&path, "value").await?)
            }
            Kind::OneLake => {
                let ws = ws.context("OneLake needs a workspace")?;
                let item = q.scope.item.as_ref().context("OneLake needs an item")?;
                let dir = q.scope.path.as_deref().unwrap_or("");
                let paths = self.lake.list(ws, &item.id, dir, q.tree).await?;
                if q.tree {
                    rows::onelake_tree(&paths, &item.id, dir)
                } else {
                    rows::onelake(&paths, &item.id)
                }
            }
            Kind::Shortcuts => {
                let ws = ws.context("shortcuts need a workspace")?;
                let item = q.scope.item.as_ref().context("shortcuts need an item")?;
                let path = format!("workspaces/{ws}/items/{}/shortcuts", item.id);
                rows::shortcuts(&c.list(&path, "value").await?)
            }
        })
    }

    async fn instances(&self, ws: &str, item: &str) -> Result<Vec<Value>> {
        let path = format!("workspaces/{ws}/items/{item}/jobs/instances");
        self.client.list(&path, "value").await
    }

    /// Recent Spark (Livy) sessions from `path`, newest first. They are the
    /// only record of a user cancelling a notebook or Spark job run, so a
    /// failure here is logged and the runs show as Fabric reports them.
    async fn sessions(&self, path: &str) -> Vec<Value> {
        match self.client.list_pages(path, "value", SESSION_PAGES).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(path, "listing Spark sessions: {e:#}");
                Vec::new()
            }
        }
    }

    /// Runs of one item, with user-cancelled Spark runs marked as such.
    async fn runs(&self, ws: &str, item: &str, item_type: &str) -> Result<Vec<Value>> {
        let sessions = match item_type {
            "Notebook" => Some(format!("workspaces/{ws}/notebooks/{item}/livySessions")),
            "SparkJobDefinition" => Some(format!(
                "workspaces/{ws}/sparkJobDefinitions/{item}/livySessions"
            )),
            _ => None,
        };
        let recent = async {
            match &sessions {
                Some(path) => self.sessions(path).await,
                None => Vec::new(),
            }
        };
        let (runs, sessions) = futures::join!(self.instances(ws, item), recent);
        let mut runs = runs?;
        rows::mark_cancelled(&mut runs, &sessions);
        Ok(runs)
    }

    /// Runs of every runnable item in a workspace. There is no workspace-wide
    /// runs endpoint, so this asks each item; Spark sessions do have one, so
    /// cancellations take a single extra request. Items that fail (e.g. no
    /// permission) are skipped unless every one of them fails.
    async fn workspace_runs(&self, ws: &str, force: bool) -> Result<Vec<Row>> {
        let sessions_path = format!("workspaces/{ws}/spark/livySessions");
        let (data, sessions) =
            futures::join!(self.workspace(ws, force), self.sessions(&sessions_path));
        let data = data?;
        let runnable: Vec<Value> = data
            .items
            .iter()
            .filter(|i| job_type_for(i["type"].as_str().unwrap_or("")).is_some())
            .cloned()
            .collect();
        let results: Vec<(Value, Result<Vec<Value>>)> = futures::stream::iter(runnable)
            .map(|item| async move {
                let id = item["id"].as_str().unwrap_or_default().to_owned();
                let res = self.instances(ws, &id).await;
                (item, res)
            })
            .buffer_unordered(JOB_FAN_OUT)
            .collect()
            .await;
        let now = chrono::Utc::now();
        let (mut rows, mut errors) = (Vec::new(), Vec::new());
        let total = results.len();
        for (item, res) in results {
            match res {
                Ok(mut list) => {
                    rows::mark_cancelled(&mut list, &sessions);
                    rows.extend(rows::jobs(&list, Some(&item), now));
                }
                Err(e) => {
                    tracing::warn!(item = %item["displayName"], "listing runs: {e:#}");
                    errors.push(e);
                }
            }
        }
        if total > 0 && errors.len() == total {
            return Err(errors.remove(0));
        }
        rows::sort_runs(&mut rows);
        Ok(rows)
    }

    async fn act(&self, action: Action) -> Result<String> {
        let result = self.write(&action).await;
        // Folder and item listings of this workspace are stale now, even if
        // the write failed halfway.
        if !matches!(action, Action::RunJob { .. } | Action::CancelJob { .. }) {
            self.spaces.lock().unwrap().remove(action.workspace());
        }
        result
    }

    async fn write(&self, action: &Action) -> Result<String> {
        let c = &self.client;
        match action {
            Action::RunJob {
                workspace,
                item,
                job_type,
            } => {
                let path = format!(
                    "workspaces/{workspace}/items/{}/jobs/instances?jobType={job_type}",
                    item.id
                );
                let location = c.post_accepted(&path).await?;
                let run = location
                    .as_deref()
                    .and_then(|l| l.rsplit('/').next())
                    .unwrap_or("?");
                Ok(format!("started {} run {run}", item.name))
            }
            Action::CancelJob {
                workspace,
                item,
                job,
            } => {
                let path = format!(
                    "workspaces/{workspace}/items/{}/jobs/instances/{job}/cancel",
                    item.id
                );
                c.post_accepted(&path).await?;
                Ok(format!("cancel requested for {} run {job}", item.name))
            }
            Action::RenameItem {
                workspace,
                item,
                name,
            } => {
                let path = format!("workspaces/{workspace}/items/{}", item.id);
                c.call(Method::PATCH, &path, Some(json!({"displayName": name})))
                    .await?;
                Ok(format!("renamed {} to {name}", item.name))
            }
            Action::DescribeItem {
                workspace,
                item,
                description,
            } => {
                let path = format!("workspaces/{workspace}/items/{}", item.id);
                c.call(
                    Method::PATCH,
                    &path,
                    Some(json!({"description": description})),
                )
                .await?;
                Ok(format!("updated the description of {}", item.name))
            }
            Action::DeleteItem { workspace, item } => {
                let path = format!("workspaces/{workspace}/items/{}", item.id);
                c.call(Method::DELETE, &path, None).await?;
                Ok(format!("deleted {} {}", item.item_type, item.name))
            }
            Action::NewFolder {
                workspace,
                parent,
                name,
            } => {
                let mut body = json!({"displayName": name});
                if let Some(p) = parent {
                    body["parentFolderId"] = p.id.clone().into();
                }
                let path = format!("workspaces/{workspace}/folders");
                c.call(Method::POST, &path, Some(body)).await?;
                Ok(format!("created folder {name}"))
            }
            Action::NewItem {
                workspace,
                folder,
                item_type,
                name,
            } => {
                let mut body = json!({"displayName": name, "type": item_type});
                if let Some(f) = folder {
                    body["folderId"] = f.id.clone().into();
                }
                // 201 with the item, or 202 while it's provisioned.
                let path = format!("workspaces/{workspace}/items");
                c.post_lro(&path, Some(body)).await?;
                Ok(format!("created {item_type} {name}"))
            }
            Action::RenameFolder {
                workspace,
                folder,
                name,
            } => {
                let path = format!("workspaces/{workspace}/folders/{}", folder.id);
                c.call(Method::PATCH, &path, Some(json!({"displayName": name})))
                    .await?;
                Ok(format!("renamed folder {} to {name}", folder.name))
            }
            Action::DeleteFolder { workspace, folder } => {
                // Fabric refuses a non-empty folder too; checking first gives
                // a clearer message.
                let data = self.workspace(workspace, true).await?;
                let inside = |v: &Value, key: &str| v[key].as_str() == Some(&folder.id);
                let n = data.items.iter().filter(|i| inside(i, "folderId")).count()
                    + data
                        .folders
                        .iter()
                        .filter(|f| inside(f, "parentFolderId"))
                        .count();
                if n > 0 {
                    anyhow::bail!("folder {} isn't empty ({n} inside)", folder.name);
                }
                let path = format!("workspaces/{workspace}/folders/{}", folder.id);
                c.call(Method::DELETE, &path, None).await?;
                Ok(format!("deleted folder {}", folder.name))
            }
            Action::Move {
                workspace,
                what,
                to,
            } => {
                let path = match what {
                    Movable::Item(i) => format!("workspaces/{workspace}/items/{}/move", i.id),
                    Movable::Folder(f) => format!("workspaces/{workspace}/folders/{}/move", f.id),
                };
                let body = match to {
                    Some(t) => json!({"targetFolderId": t.id}),
                    None => json!({}),
                };
                c.call(Method::POST, &path, Some(body)).await?;
                let dest = to
                    .as_ref()
                    .map_or("the workspace root", |t| t.name.as_str());
                Ok(format!("moved {} to {dest}", what.describe()))
            }
            Action::AddRole {
                workspace,
                principal,
                role,
            } => {
                let p = self.graph.resolve(principal).await?;
                let path = format!("workspaces/{workspace}/roleAssignments");
                let body = json!({"principal": {"id": p.id, "type": p.kind}, "role": role});
                c.call(Method::POST, &path, Some(body)).await?;
                Ok(format!("gave {} ({}) the {role} role", p.name, p.kind))
            }
            Action::SetRole {
                workspace,
                assignment,
                role,
            } => {
                let path = format!("workspaces/{workspace}/roleAssignments/{}", assignment.id);
                c.call(Method::PATCH, &path, Some(json!({"role": role})))
                    .await?;
                Ok(format!("{} is now {role}", assignment.name))
            }
            Action::RemoveRole {
                workspace,
                assignment,
            } => {
                let path = format!("workspaces/{workspace}/roleAssignments/{}", assignment.id);
                c.call(Method::DELETE, &path, None).await?;
                Ok(format!("removed {}", assignment.name))
            }
            Action::DeleteShortcut {
                workspace,
                item,
                path,
                name,
            } => {
                let at: Vec<String> = path
                    .split('/')
                    .chain([name.as_str()])
                    .map(|p| urlencoding::encode(p).into_owned())
                    .collect();
                let url = format!(
                    "workspaces/{workspace}/items/{}/shortcuts/{}",
                    item.id,
                    at.join("/")
                );
                c.call(Method::DELETE, &url, None).await?;
                Ok(format!("deleted shortcut {path}/{name}"))
            }
        }
    }

    async fn definition(&self, ws: &str, item: &str) -> Result<Vec<Part>> {
        let res = self
            .client
            .post_lro(&format!("workspaces/{ws}/items/{item}/getDefinition"), None)
            .await?;
        decode_parts(&res)
    }
}

/// Decode the base64 parts of a `getDefinition` result.
pub fn decode_parts(res: &Value) -> Result<Vec<Part>> {
    let parts = res
        .pointer("/definition/parts")
        .and_then(Value::as_array)
        .context("definition has no parts")?;
    parts
        .iter()
        .map(|p| {
            let path = p
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned();
            let payload = p.get("payload").and_then(Value::as_str).unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .with_context(|| format!("decoding {path}"))?;
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => format!("<{} bytes of binary data>", e.as_bytes().len()),
            };
            Ok(Part { path, text })
        })
        .collect()
}

impl DataSource for FabricSource {
    fn fetch(&self, q: Query, force: bool) -> BoxFuture<'static, Result<Vec<Row>>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.fetch(q, force).await })
    }

    fn definition(&self, ws: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.definition(&ws, &item).await })
    }

    fn preview(
        &self,
        ws: String,
        item: String,
        path: String,
        total: Option<u64>,
    ) -> BoxFuture<'static, Result<Part>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let bytes = inner.lake.head(&ws, &item, &path, PREVIEW_BYTES).await?;
            let text = rows::preview_text(&bytes, total, PREVIEW_BYTES);
            Ok(Part { path, text })
        })
    }

    fn schema(&self, ws: String, item: String, path: String) -> BoxFuture<'static, Result<Part>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let (schema, table) = table_of(&path)?;
            let meta = inner.lake.table(&ws, &item, &schema, &table).await?;
            Ok(Part {
                path: format!("{schema}.{table}"),
                text: rows::schema_text(&meta),
            })
        })
    }

    fn download(
        &self,
        ws: String,
        item: String,
        path: String,
        dest: std::path::PathBuf,
    ) -> BoxFuture<'static, Result<u64>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.lake.download(&ws, &item, &path, &dest).await })
    }

    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.act(action).await })
    }

    fn forget(&self) {
        self.inner.tokens.forget();
    }

    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.tokens.token(FABRIC).await.map(|_| ()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Ref, Scope};
    use client::tests::StaticToken;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn source(server: &MockServer) -> FabricSource {
        let tokens: Arc<dyn TokenProvider> = Arc::new(StaticToken);
        let lake = OneLake::with_bases(tokens.clone(), &server.uri(), &server.uri());
        let graph = Graph::with_base(tokens.clone(), format!("{}/graph", server.uri()));
        FabricSource::with_clients(
            Client::with_base(tokens.clone(), server.uri()),
            lake,
            graph,
            tokens,
        )
    }

    #[tokio::test]
    async fn folder_levels_reuse_one_listing() {
        let server = MockServer::start().await;
        Mock::given(path("/workspaces/w1/folders"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value": [{"id": "f1", "displayName": "ingest"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "i1", "displayName": "nb", "type": "Notebook", "folderId": "f1"}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let mut q = Query::new(
            Kind::Items,
            Scope {
                workspace: Some(Ref {
                    id: "w1".into(),
                    name: "W".into(),
                }),
                ..Scope::default()
            },
        );
        let root = src.fetch(q.clone(), false).await.unwrap();
        assert_eq!(root[0].name, "ingest");
        q.scope.folder = Some(Ref {
            id: "f1".into(),
            name: "ingest".into(),
        });
        let inside = src.fetch(q, false).await.unwrap();
        assert_eq!(inside[0].name, "nb");
        // `expect(1)` on each mock is verified when the server drops.
    }

    fn ws_scope() -> Scope {
        Scope {
            workspace: Some(Ref {
                id: "w1".into(),
                name: "W".into(),
            }),
            ..Scope::default()
        }
    }

    #[tokio::test]
    async fn workspace_jobs_fan_out_and_skip_failing_items() {
        let server = MockServer::start().await;
        Mock::given(path("/workspaces/w1/folders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "nb", "displayName": "load", "type": "Notebook"},
                {"id": "pl", "displayName": "orchestrate", "type": "DataPipeline"},
                {"id": "rpt", "displayName": "sales", "type": "Report"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items/nb/jobs/instances"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "r1", "status": "Completed", "startTimeUtc": "2026-10-09T01:00:00",
                 "endTimeUtc": "2026-10-09T01:05:00"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items/pl/jobs/instances"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"errorCode": "Forbidden"})),
            )
            .mount(&server)
            .await;
        let src = source(&server).await;
        let rows = src
            .fetch(Query::new(Kind::Jobs, ws_scope()), false)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "load");
        assert_eq!(rows[0].cells[5], "5m00s");
    }

    #[tokio::test]
    async fn workspace_jobs_read_cancels_from_a_few_pages_of_workspace_sessions() {
        let server = MockServer::start().await;
        mount_workspace(
            &server,
            json!([]),
            json!([
                {"id": "nb1", "displayName": "a", "type": "Notebook"},
                {"id": "nb2", "displayName": "b", "type": "Notebook"}
            ]),
            1,
        )
        .await;
        for nb in ["nb1", "nb2"] {
            Mock::given(path(format!("/workspaces/w1/items/{nb}/jobs/instances")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                    {"id": format!("{nb}-run"), "status": "Completed",
                     "startTimeUtc": "2026-10-09T01:00:00", "endTimeUtc": "2026-10-09T01:00:05"}
                ]})))
                .mount(&server)
                .await;
            // Per-item sessions must not be asked for in the workspace view.
            Mock::given(path(format!("/workspaces/w1/notebooks/{nb}/livySessions")))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        }
        // Every page points at another; only SESSION_PAGES are read.
        let next = format!("{}/workspaces/w1/spark/livySessions", server.uri());
        Mock::given(path("/workspaces/w1/spark/livySessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{"jobInstanceId": "nb2-run",
                           "cancellationReason": "User cancelled the Spark session"}],
                "continuationUri": next,
            })))
            .expect(SESSION_PAGES as u64)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let rows = src
            .fetch(Query::new(Kind::Jobs, ws_scope()), false)
            .await
            .unwrap();
        let by_item: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.cells[0].as_str(),
                    r.cells[1].as_str(),
                    r.cells[6].as_str(),
                )
            })
            .collect();
        assert!(by_item.contains(&("a", "Completed", "")));
        assert!(by_item.contains(&("b", "Cancelled", "User cancelled the Spark session")));
    }

    #[tokio::test]
    async fn run_and_cancel_post_to_job_endpoints() {
        let server = MockServer::start().await;
        let started = format!(
            "{}/workspaces/w1/items/nb/jobs/instances/run-9",
            server.uri()
        );
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items/nb/jobs/instances"))
            .and(query_param("jobType", "RunNotebook"))
            .respond_with(ResponseTemplate::new(202).insert_header("location", started.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items/nb/jobs/instances/run-9/cancel"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let item = crate::source::ItemRef {
            id: "nb".into(),
            name: "load".into(),
            item_type: "Notebook".into(),
        };
        let msg = src
            .act(Action::RunJob {
                workspace: "w1".into(),
                item: item.clone(),
                job_type: "RunNotebook".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "started load run run-9");
        let msg = src
            .act(Action::CancelJob {
                workspace: "w1".into(),
                item,
                job: "run-9".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "cancel requested for load run run-9");
    }

    fn item(id: &str, name: &str) -> crate::source::ItemRef {
        crate::source::ItemRef {
            id: id.into(),
            name: name.into(),
            item_type: "Notebook".into(),
        }
    }

    fn folder(id: &str, name: &str) -> Ref {
        Ref {
            id: id.into(),
            name: name.into(),
        }
    }

    async fn mount_workspace(server: &MockServer, folders: Value, items: Value, times: u64) {
        Mock::given(path("/workspaces/w1/folders"))
            .and(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": folders})))
            .expect(times)
            .mount(server)
            .await;
        Mock::given(path("/workspaces/w1/items"))
            .and(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": items})))
            .expect(times)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_write_drops_the_workspace_listing() {
        let server = MockServer::start().await;
        mount_workspace(
            &server,
            json!([]),
            json!([{"id": "nb", "displayName": "a"}]),
            2,
        )
        .await;
        Mock::given(method("PATCH"))
            .and(path("/workspaces/w1/items/nb"))
            .and(wiremock::matchers::body_json(json!({"displayName": "b"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "nb"})))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let q = Query::new(Kind::Items, ws_scope());
        src.fetch(q.clone(), false).await.unwrap();
        let msg = src
            .act(Action::RenameItem {
                workspace: "w1".into(),
                item: item("nb", "a"),
                name: "b".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "renamed a to b");
        // Not forced, but the cached listing is gone: a second GET each.
        src.fetch(q, false).await.unwrap();
    }

    #[tokio::test]
    async fn non_empty_folders_are_not_deleted() {
        let server = MockServer::start().await;
        mount_workspace(
            &server,
            json!([{"id": "f1", "displayName": "ingest"}, {"id": "f2", "displayName": "empty"}]),
            json!([{"id": "nb", "displayName": "a", "folderId": "f1"}]),
            2,
        )
        .await;
        Mock::given(method("DELETE"))
            .and(path("/workspaces/w1/folders/f2"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let del = |f: Ref| Action::DeleteFolder {
            workspace: "w1".into(),
            folder: f,
        };
        let e = src.act(del(folder("f1", "ingest"))).await.unwrap_err();
        assert_eq!(e.to_string(), "folder ingest isn't empty (1 inside)");
        src.act(del(folder("f2", "empty"))).await.unwrap();
    }

    #[tokio::test]
    async fn moves_create_and_shortcuts_hit_their_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items/nb/move"))
            .and(wiremock::matchers::body_json(
                json!({"targetFolderId": "f1"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/folders/f2/move"))
            .and(wiremock::matchers::body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "f2"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/folders"))
            .and(wiremock::matchers::body_json(
                json!({"displayName": "new", "parentFolderId": "f1"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "f3"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(
                "/workspaces/w1/items/lh/shortcuts/Files/my%20data/claims",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let ws = || "w1".to_owned();
        src.act(Action::Move {
            workspace: ws(),
            what: Movable::Item(item("nb", "a")),
            to: Some(folder("f1", "ingest")),
        })
        .await
        .unwrap();
        let msg = src
            .act(Action::Move {
                workspace: ws(),
                what: Movable::Folder(folder("f2", "ingest/shop")),
                to: None,
            })
            .await
            .unwrap();
        assert_eq!(msg, "moved folder ingest/shop to the workspace root");
        src.act(Action::NewFolder {
            workspace: ws(),
            parent: Some(folder("f1", "ingest")),
            name: "new".into(),
        })
        .await
        .unwrap();
        src.act(Action::DeleteShortcut {
            workspace: ws(),
            item: item("lh", "bronze"),
            path: "Files/my data".into(),
            name: "claims".into(),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn new_items_post_type_and_folder() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items"))
            .and(wiremock::matchers::body_json(
                json!({"displayName": "lh2", "type": "Lakehouse", "folderId": "f1"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "new"})))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let msg = src
            .act(Action::NewItem {
                workspace: "w1".into(),
                folder: Some(folder("f1", "ingest")),
                item_type: "Lakehouse".into(),
                name: "lh2".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "created Lakehouse lh2");
    }

    #[tokio::test]
    async fn add_role_resolves_the_principal_first() {
        let server = MockServer::start().await;
        Mock::given(path("/graph/users/jane@x.org"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "u1", "displayName": "Jane"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/roleAssignments"))
            .and(wiremock::matchers::body_json(
                json!({"principal": {"id": "u1", "type": "User"}, "role": "Viewer"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "u1"})))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let msg = src
            .act(Action::AddRole {
                workspace: "w1".into(),
                principal: "jane@x.org".into(),
                role: "Viewer".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "gave Jane (User) the Viewer role");
    }

    #[test]
    fn decodes_definition_parts() {
        let res = json!({"definition": {"parts": [
            {"path": "notebook-content.py", "payload": "cHJpbnQoMSk=", "payloadType": "InlineBase64"}
        ]}});
        let parts = decode_parts(&res).unwrap();
        assert_eq!(parts[0].text, "print(1)");
    }
}
