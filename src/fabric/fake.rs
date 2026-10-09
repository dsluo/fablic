//! Deterministic data for tests, snapshots and `--fake`. Fixture JSON goes
//! through the same row builders as live responses.

use anyhow::{Result, bail};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::rows::{self, WsData};
use crate::resources::{Health, Kind, Row};
use crate::source::{DataSource, Part, Query};

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
        Kind::Jobs => jobs(),
        Kind::OneLake => onelake(q.scope.path.as_deref().unwrap_or("")),
        _ => Vec::new(),
    })
}

fn jobs() -> Vec<Row> {
    let job = |id: &str, status: &str, start: &str, dur: &str| {
        Row::new(
            id,
            id,
            vec![
                id.into(),
                status.into(),
                "RunNotebook".into(),
                "Scheduled".into(),
                start.into(),
                dur.into(),
            ],
        )
        .health(Health::from_status(status))
        .raw(json!({"id": id, "status": status, "jobType": "RunNotebook",
                    "invokeType": "Scheduled", "startTimeUtc": start}))
    };
    vec![
        job("job-3", "InProgress", "2026-10-09 13:02", "4m"),
        job("job-2", "Failed", "2026-10-09 01:00", "12m"),
        job("job-1", "Completed", "2026-10-08 01:00", "9m"),
    ]
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
