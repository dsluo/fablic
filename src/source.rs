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

/// Something that can be moved between workspace folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Movable {
    Item(ItemRef),
    /// A folder; `name` is its full path.
    Folder(Ref),
}

impl Movable {
    pub fn describe(&self) -> String {
        match self {
            Movable::Item(i) => format!("{} {}", i.item_type, i.name),
            Movable::Folder(f) => format!("folder {}", f.name),
        }
    }
}

/// The workspace roles, in order of power.
pub const ROLES: [&str; 4] = ["Admin", "Member", "Contributor", "Viewer"];

/// A write. Every one goes through a confirm step, the read-only guard and
/// the journal before it reaches a source. `workspace` is always an id.
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
    RenameItem {
        workspace: String,
        item: ItemRef,
        name: String,
    },
    DescribeItem {
        workspace: String,
        item: ItemRef,
        description: String,
    },
    DeleteItem {
        workspace: String,
        item: ItemRef,
    },
    /// `parent` is `None` for the workspace root; its `name` is a full path.
    NewFolder {
        workspace: String,
        parent: Option<Ref>,
        name: String,
    },
    /// An item made from a name alone; `folder` is `None` for the root.
    NewItem {
        workspace: String,
        folder: Option<Ref>,
        item_type: String,
        name: String,
    },
    RenameFolder {
        workspace: String,
        folder: Ref,
        name: String,
    },
    DeleteFolder {
        workspace: String,
        folder: Ref,
    },
    /// `to` is `None` for the workspace root.
    Move {
        workspace: String,
        what: Movable,
        to: Option<Ref>,
    },
    /// `principal` as typed: an email/UPN, an object id, or an exact group
    /// or service principal name. The source resolves it.
    AddRole {
        workspace: String,
        principal: String,
        role: String,
    },
    /// `assignment` is the role assignment, named after its principal.
    SetRole {
        workspace: String,
        assignment: Ref,
        role: String,
    },
    RemoveRole {
        workspace: String,
        assignment: Ref,
    },
    /// `path` is the folder holding the shortcut, e.g. `Tables`.
    DeleteShortcut {
        workspace: String,
        item: ItemRef,
        path: String,
        name: String,
    },
}

fn place(folder: &Option<Ref>) -> String {
    match folder {
        Some(f) => f.name.clone(),
        None => "the workspace root".into(),
    }
}

impl Action {
    /// What the confirm prompt and journal call it.
    pub fn describe(&self) -> String {
        match self {
            Action::RunJob { item, job_type, .. } => {
                format!("run {} {} ({job_type})", item.item_type, item.name)
            }
            Action::CancelJob { item, job, .. } => format!("cancel run {job} of {}", item.name),
            Action::RenameItem { item, name, .. } => {
                format!("rename {} {} to \"{name}\"", item.item_type, item.name)
            }
            Action::DescribeItem { item, .. } => {
                format!("set the description of {} {}", item.item_type, item.name)
            }
            Action::DeleteItem { item, .. } => format!("delete {} {}", item.item_type, item.name),
            Action::NewFolder { parent, name, .. } => {
                format!("create folder \"{name}\" in {}", place(parent))
            }
            Action::NewItem {
                folder,
                item_type,
                name,
                ..
            } => format!("create {item_type} \"{name}\" in {}", place(folder)),
            Action::RenameFolder { folder, name, .. } => {
                format!("rename folder {} to \"{name}\"", folder.name)
            }
            Action::DeleteFolder { folder, .. } => format!("delete folder {}", folder.name),
            Action::Move { what, to, .. } => format!("move {} to {}", what.describe(), place(to)),
            Action::AddRole {
                principal, role, ..
            } => format!("give {principal} the {role} role"),
            Action::SetRole {
                assignment, role, ..
            } => format!("change {}'s role to {role}", assignment.name),
            Action::RemoveRole { assignment, .. } => {
                format!("remove {} from the workspace", assignment.name)
            }
            Action::DeleteShortcut { path, name, .. } => format!("delete shortcut {path}/{name}"),
        }
    }

    pub fn workspace(&self) -> &str {
        match self {
            Action::RunJob { workspace, .. }
            | Action::CancelJob { workspace, .. }
            | Action::RenameItem { workspace, .. }
            | Action::DescribeItem { workspace, .. }
            | Action::DeleteItem { workspace, .. }
            | Action::NewFolder { workspace, .. }
            | Action::NewItem { workspace, .. }
            | Action::RenameFolder { workspace, .. }
            | Action::DeleteFolder { workspace, .. }
            | Action::Move { workspace, .. }
            | Action::AddRole { workspace, .. }
            | Action::SetRole { workspace, .. }
            | Action::RemoveRole { workspace, .. }
            | Action::DeleteShortcut { workspace, .. } => workspace,
        }
    }

    /// Ids of what the action touches, for the journal.
    pub fn target(&self) -> String {
        let ws = format!("workspace={}", self.workspace());
        let rest = match self {
            Action::RunJob { item, .. } => format!("item={}", item.id),
            Action::CancelJob { item, job, .. } => format!("item={} job={job}", item.id),
            Action::RenameItem { item, .. }
            | Action::DescribeItem { item, .. }
            | Action::DeleteItem { item, .. } => format!("item={}", item.id),
            Action::NewFolder { parent, .. } => {
                format!("parent={}", parent.as_ref().map_or("root", |p| &p.id))
            }
            Action::NewItem {
                folder, item_type, ..
            } => format!(
                "folder={} type={item_type}",
                folder.as_ref().map_or("root", |f| &f.id)
            ),
            Action::RenameFolder { folder, .. } | Action::DeleteFolder { folder, .. } => {
                format!("folder={}", folder.id)
            }
            Action::Move { what, to, .. } => {
                let what = match what {
                    Movable::Item(i) => format!("item={}", i.id),
                    Movable::Folder(f) => format!("folder={}", f.id),
                };
                format!("{what} to={}", to.as_ref().map_or("root", |t| &t.id))
            }
            Action::AddRole { principal, .. } => format!("principal={principal}"),
            Action::SetRole { assignment, .. } | Action::RemoveRole { assignment, .. } => {
                format!("assignment={}", assignment.id)
            }
            Action::DeleteShortcut {
                item, path, name, ..
            } => format!("item={} shortcut={path}/{name}", item.id),
        };
        format!("{ws} {rest}")
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

    /// Drop cached credentials, e.g. after signing in again.
    fn forget(&self) {}

    /// Get a token ahead of time so switching to this profile is instant.
    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Sign in again without az, for a source that can (the fake one).
    /// `None` means signing in is `az login`'s job.
    fn sign_in(&self) -> Option<BoxFuture<'static, Result<()>>> {
        None
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
