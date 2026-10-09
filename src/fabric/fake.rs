//! Deterministic data for tests, snapshots and `--fake`. Fixture JSON goes
//! through the same row builders as live responses.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::rows::{self, WsData};
use crate::resources::{Kind, Row, job_type_for};
use crate::source::{Action, DataSource, Part, Query};

pub struct FakeSource;

fn workspaces() -> Vec<Value> {
    vec![
        json!({"id": "ws-1", "displayName": "Urban1 Bronze", "type": "Workspace", "capacityId": "cap-f64"}),
        json!({"id": "ws-2", "displayName": "Urban2 Silver", "type": "Workspace", "capacityId": "cap-f64"}),
        json!({"id": "ws-3", "displayName": "Sandbox", "type": "Workspace", "capacityId": "cap-trial"}),
    ]
}

fn capacities() -> Vec<Value> {
    vec![
        json!({"id": "cap-f64", "displayName": "Prod F64", "sku": "F64",
                "region": "East US 2", "state": "Active"}),
    ]
}

fn workspace(id: &str) -> WsData {
    let item = |id: &str, name: &str, t: &str, folder: Option<&str>, desc: &str| {
        let mut v = json!({"id": id, "displayName": name, "type": t, "description": desc,
                           "workspaceId": "ws-1"});
        if let Some(f) = folder {
            v["folderId"] = f.into();
        }
        v
    };
    match id {
        "ws-1" => WsData {
            folders: vec![
                json!({"id": "f-1", "displayName": "ingest", "workspaceId": "ws-1"}),
                json!({"id": "f-2", "displayName": "shop", "workspaceId": "ws-1", "parentFolderId": "f-1"}),
            ],
            items: vec![
                item(
                    "it-1",
                    "ingest_shop",
                    "Notebook",
                    Some("f-2"),
                    "Nightly shop extract",
                ),
                item("it-2", "bronze_lh", "Lakehouse", None, ""),
                item(
                    "it-3",
                    "load_all",
                    "DataPipeline",
                    Some("f-1"),
                    "Orchestrates ingest",
                ),
                item("it-4", "Finance Model", "SemanticModel", None, ""),
                item("it-5", "transform_silver", "Notebook", None, ""),
            ],
        },
        _ => WsData::default(),
    }
}

fn fake_rows(q: &Query) -> Result<Vec<Row>> {
    let ws = q.scope.workspace.as_ref().map(|w| w.id.as_str());
    let folder = q.scope.folder.as_ref().map(|f| f.id.as_str());
    Ok(match q.kind {
        Kind::Workspaces => rows::workspaces(&workspaces(), &capacities()),
        Kind::Items => match ws {
            Some(id) if q.is_flat() => {
                rows::items_flat(&workspace(id), folder, q.item_type.as_deref())
            }
            Some(id) => rows::items_level(&workspace(id), folder),
            None => {
                let spaces: Vec<_> = workspaces()
                    .into_iter()
                    .map(|w| {
                        let d = workspace(w["id"].as_str().unwrap_or_default());
                        (w, d)
                    })
                    .collect();
                rows::items_all(&spaces, q.item_type.as_deref())
            }
        },
        Kind::Capacities => rows::capacities(&capacities()),
        Kind::Roles => rows::roles(&[json!({"id": "r-1", "role": "Admin",
            "principal": {"displayName": "Data Engineering", "type": "Group"}})]),
        Kind::Domains => bail!("403 /admin/domains: InsufficientPrivileges"),
        Kind::Jobs => jobs(q),
        Kind::Schedules => schedules(),
        Kind::OneLake => onelake(q.scope.path.as_deref().unwrap_or("")),
        _ => Vec::new(),
    })
}

/// Runs at fixed times relative to a fixed "now", so durations are stable.
pub fn now() -> DateTime<Utc> {
    rows::parse_utc("2026-10-09T13:10:00").unwrap_or_default()
}

fn runs(item: &str, job_type: &str) -> Vec<Value> {
    let run = |id: &str, status: &str, start: &str, end: Option<&str>| {
        let mut v = json!({"id": id, "itemId": item, "status": status,
                           "jobType": job_type, "invokeType": "Scheduled",
                           "startTimeUtc": start});
        if let Some(e) = end {
            v["endTimeUtc"] = e.into();
        }
        if status == "Failed" {
            v["failureReason"] = json!({"errorCode": "NotebookExecutionFailed",
                                        "message": "Py4JJavaError: table not found"});
        }
        v
    };
    vec![
        run(
            &format!("{item}-3"),
            "InProgress",
            "2026-10-09T13:02:00",
            None,
        ),
        run(
            &format!("{item}-2"),
            "Failed",
            "2026-10-09T01:00:00",
            Some("2026-10-09T01:12:00"),
        ),
        run(
            &format!("{item}-1"),
            "Completed",
            "2026-10-08T01:00:00",
            Some("2026-10-08T01:09:00"),
        ),
    ]
}

fn jobs(q: &Query) -> Vec<Row> {
    match &q.scope.item {
        Some(item) => {
            let job_type = job_type_for(&item.item_type).unwrap_or("RunNotebook");
            rows::jobs(&runs(&item.id, job_type), None, now())
        }
        None => {
            let ws = q
                .scope
                .workspace
                .as_ref()
                .map(|w| w.id.as_str())
                .unwrap_or("");
            let mut out = Vec::new();
            for i in workspace(ws).items {
                let t = i["type"].as_str().unwrap_or("");
                if let Some(job_type) = job_type_for(t) {
                    let id = i["id"].as_str().unwrap_or("");
                    out.extend(rows::jobs(&runs(id, job_type), Some(&i), now()));
                }
            }
            rows::sort_runs(&mut out);
            out
        }
    }
}

fn schedules() -> Vec<Row> {
    rows::schedules(&[
        json!({"id": "sch-1", "enabled": true, "configuration": {"type": "Daily",
               "startDateTime": "2026-06-09T00:00:00", "endDateTime": "2027-06-09T00:00:00",
               "localTimeZoneId": "Eastern Standard Time", "times": ["01:00"]}}),
        json!({"id": "sch-2", "enabled": false, "configuration": {"type": "Cron", "interval": 30},
               "enabledDetail": {"enabledControlType": "AutoDisabled"}}),
    ])
}

fn onelake(base: &str) -> Vec<Row> {
    let entry = |name: &str, dir: bool, size: &str| {
        let path = if base.is_empty() {
            name.to_string()
        } else {
            format!("{base}/{name}")
        };
        let kind = if dir { "dir" } else { "file" };
        let row = Row::new(
            path.clone(),
            name,
            vec![
                name.into(),
                kind.into(),
                size.into(),
                "2026-10-09 01:12".into(),
            ],
        )
        .raw(json!({"name": path, "isDirectory": dir}));
        if dir { row.dir() } else { row }
    };
    if base.is_empty() {
        vec![entry("Tables", true, ""), entry("Files", true, "")]
    } else {
        vec![entry("raw", true, ""), entry("readme.txt", false, "1.2 KB")]
    }
}

impl DataSource for FakeSource {
    fn fetch(&self, q: Query, _force: bool) -> BoxFuture<'static, Result<Vec<Row>>> {
        let rows = fake_rows(&q);
        Box::pin(async move { rows })
    }

    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>> {
        Box::pin(async move { Ok(format!("(fake) {}", action.describe())) })
    }

    fn definition(&self, _ws: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>> {
        Box::pin(async move {
            Ok(vec![
                Part {
                    path: "notebook-content.py".into(),
                    text: format!("# Fabric notebook source\n\n# CELL\nprint(\"{item}\")\n"),
                },
                Part {
                    path: ".platform".into(),
                    text: "{\n  \"metadata\": {\"type\": \"Notebook\"}\n}\n".into(),
                },
            ])
        })
    }
}
