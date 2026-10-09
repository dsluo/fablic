//! Where rows come from. The app only sees `DataSource`, so the TUI can run
//! against Fabric or against fixed fake data in tests and `--fake`.

use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::json;

use crate::resources::{Health, Kind, Row};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Ref {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub id: String,
    pub name: String,
    pub item_type: String,
}

/// What the current view is looking inside of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Scope {
    pub workspace: Option<Ref>,
    pub item: Option<ItemRef>,
    /// Path inside an item's OneLake storage, e.g. `Files/raw`.
    pub path: Option<String>,
}

impl Scope {
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ws) = &self.workspace {
            parts.push(ws.name.clone());
        }
        if let Some(it) = &self.item {
            parts.push(format!("{} ({})", it.name, it.item_type));
        }
        if let Some(p) = &self.path {
            parts.push(p.clone());
        }
        if parts.is_empty() {
            "all".into()
        } else {
            parts.join(" › ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    pub kind: Kind,
    pub scope: Scope,
    /// Restrict `items` to one Fabric item type, e.g. `Notebook`.
    pub item_type: Option<String>,
}

pub trait DataSource: Send + Sync {
    fn fetch(&self, q: Query) -> BoxFuture<'static, Result<Vec<Row>>>;
}

pub type SharedSource = Arc<dyn DataSource>;

/// Deterministic data for tests, snapshots and `--fake`.
pub struct FakeSource;

impl DataSource for FakeSource {
    fn fetch(&self, q: Query) -> BoxFuture<'static, Result<Vec<Row>>> {
        let rows = fake_rows(&q);
        Box::pin(async move { Ok(rows) })
    }
}

fn fake_rows(q: &Query) -> Vec<Row> {
    let ws = |id: &str, name: &str, cap: &str| {
        Row::new(
            id,
            name,
            vec![name.into(), "Workspace".into(), cap.into(), id.into()],
        )
        .raw(json!({"id": id, "displayName": name, "type": "Workspace", "capacityId": cap}))
    };
    let item = |id: &str, name: &str, t: &str, desc: &str| {
        Row::new(
            id,
            name,
            vec![name.into(), t.into(), desc.into(), id.into()],
        )
        .raw(
            json!({"id": id, "displayName": name, "type": t, "description": desc,
                   "workspaceId": "ws-1"}),
        )
    };
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
    match q.kind {
        Kind::Workspaces => vec![
            ws("ws-1", "Urban1 Bronze", "cap-f64"),
            ws("ws-2", "Urban2 Silver", "cap-f64"),
            ws("ws-3", "Sandbox", "cap-trial"),
        ],
        Kind::Items => {
            let all = vec![
                item("it-1", "ingest_shop", "Notebook", "Nightly shop extract"),
                item("it-2", "bronze_lh", "Lakehouse", ""),
                item("it-3", "load_all", "DataPipeline", "Orchestrates ingest"),
                item("it-4", "Finance Model", "SemanticModel", ""),
                item("it-5", "transform_silver", "Notebook", ""),
            ];
            match &q.item_type {
                Some(t) => all.into_iter().filter(|r| r.cells[1] == *t).collect(),
                None => all,
            }
        }
        Kind::Jobs => vec![
            job("job-3", "InProgress", "2026-10-09 13:02", "4m"),
            job("job-2", "Failed", "2026-10-09 01:00", "12m"),
            job("job-1", "Completed", "2026-10-08 01:00", "9m"),
        ],
        Kind::OneLake => {
            let base = q.scope.path.clone().unwrap_or_default();
            let entry = |name: &str, kind: &str, size: &str| {
                let path = if base.is_empty() {
                    name.to_string()
                } else {
                    format!("{base}/{name}")
                };
                Row::new(
                    path.clone(),
                    name,
                    vec![
                        name.into(),
                        kind.into(),
                        size.into(),
                        "2026-10-09 01:12".into(),
                    ],
                )
                .raw(json!({"name": path, "isDirectory": kind == "dir"}))
            };
            if base.is_empty() {
                vec![entry("Tables", "dir", ""), entry("Files", "dir", "")]
            } else {
                vec![
                    entry("raw", "dir", ""),
                    entry("readme.txt", "file", "1.2 KB"),
                ]
            }
        }
        Kind::Capacities => vec![
            Row::new(
                "cap-f64",
                "Prod F64",
                vec![
                    "Prod F64".into(),
                    "F64".into(),
                    "East US 2".into(),
                    "Active".into(),
                    "cap-f64".into(),
                ],
            )
            .health(Health::Ok),
        ],
        _ => Vec::new(),
    }
}
