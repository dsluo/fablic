//! Where rows come from. The app only sees `DataSource`, so the TUI can run
//! against Fabric or against fixed fake data in tests and `--fake`.

use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;

use crate::resources::{Kind, Row};

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
    /// Workspace folder; `name` holds the full path, e.g. `ingest/shop`.
    pub folder: Option<Ref>,
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
        if let Some(f) = &self.folder {
            parts.extend(f.name.split('/').map(str::to_owned));
        }
        if let Some(it) = &self.item {
            parts.push(format!("{} ({})", it.name, it.item_type));
        }
        if let Some(p) = &self.path {
            parts.extend(p.split('/').map(str::to_owned));
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
    /// List every item under the folder instead of one tree level.
    pub flat: bool,
}

impl Query {
    pub fn new(kind: Kind, scope: Scope) -> Query {
        Query {
            kind,
            scope,
            item_type: None,
            flat: false,
        }
    }

    /// Items views that show a whole subtree rather than one folder level.
    pub fn is_flat(&self) -> bool {
        self.flat || self.item_type.is_some() || self.scope.workspace.is_none()
    }
}

/// One file of an item definition, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub path: String,
    pub text: String,
}

pub trait DataSource: Send + Sync {
    /// Rows for `q`. `force` skips any cache the source keeps.
    fn fetch(&self, q: Query, force: bool) -> BoxFuture<'static, Result<Vec<Row>>>;

    /// An item's definition files (notebook source, pipeline JSON, ...).
    fn definition(&self, workspace: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>>;

    /// Get a token ahead of time so switching to this profile is instant.
    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

pub type SharedSource = Arc<dyn DataSource>;
