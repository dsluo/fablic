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
    /// Workspace, folders, item and OneLake path, outermost first.
    pub fn parts(&self) -> Vec<String> {
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
        parts
    }

    pub fn label(&self) -> String {
        let parts = self.parts();
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

/// A write. Every one goes through a confirm prompt, the read-only guard and
/// the journal before it reaches a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    RunJob {
        workspace: String,
        item: ItemRef,
        job_type: String,
    },
    CancelJob {
        workspace: String,
        item: ItemRef,
        job: String,
    },
}

impl Action {
    /// What the confirm prompt and journal call it.
    pub fn describe(&self) -> String {
        match self {
            Action::RunJob { item, job_type, .. } => {
                format!("run {} {} ({job_type})", item.item_type, item.name)
            }
            Action::CancelJob { item, job, .. } => format!("cancel run {job} of {}", item.name),
        }
    }
}

pub trait DataSource: Send + Sync {
    /// Rows for `q`. `force` skips any cache the source keeps.
    fn fetch(&self, q: Query, force: bool) -> BoxFuture<'static, Result<Vec<Row>>>;

    /// An item's definition files (notebook source, pipeline JSON, ...).
    fn definition(&self, workspace: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>>;

    /// The start of a OneLake file as text (or a note that it's binary).
    /// `total` is the file size when the listing knew it.
    fn preview(
        &self,
        workspace: String,
        item: String,
        path: String,
        total: Option<u64>,
    ) -> BoxFuture<'static, Result<Part>>;

    /// Columns of the Delta table at `path` (`Tables/<t>` or `Tables/<schema>/<t>`).
    fn schema(
        &self,
        workspace: String,
        item: String,
        path: String,
    ) -> BoxFuture<'static, Result<Part>>;

    /// Copy a OneLake file to a local path. Returns bytes written.
    fn download(
        &self,
        workspace: String,
        item: String,
        path: String,
        dest: std::path::PathBuf,
    ) -> BoxFuture<'static, Result<u64>>;

    /// Carry out a write. Returns a short result for the status line.
    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>>;

    /// Get a token ahead of time so switching to this profile is instant.
    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

pub type SharedSource = Arc<dyn DataSource>;

/// Bytes read for a file preview.
pub const PREVIEW_BYTES: usize = 64 * 1024;

/// `Tables/sales` -> (`dbo`, `sales`); `Tables/finance/sales` -> (`finance`, `sales`).
pub fn table_of(path: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = path.split('/').collect();
    match parts[..] {
        ["Tables", t] => Ok(("dbo".into(), t.into())),
        ["Tables", s, t] => Ok((s.into(), t.into())),
        _ => anyhow::bail!("s shows the schema of a table folder under Tables/"),
    }
}
