//! Resource kinds, their table columns, and what drilling into a row does.

use serde_json::Value;

use crate::source::Query;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Workspaces,
    Items,
    Jobs,
    Schedules,
    OneLake,
    Shortcuts,
    Roles,
    Capacities,
    Connections,
    Gateways,
    Domains,
}

impl Kind {
    pub const ALL: [Kind; 11] = [
        Kind::Workspaces,
        Kind::Items,
        Kind::Jobs,
        Kind::Schedules,
        Kind::OneLake,
        Kind::Shortcuts,
        Kind::Roles,
        Kind::Capacities,
        Kind::Connections,
        Kind::Gateways,
        Kind::Domains,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Workspaces => "workspaces",
            Kind::Items => "items",
            Kind::Jobs => "jobs",
            Kind::Schedules => "schedules",
            Kind::OneLake => "onelake",
            Kind::Shortcuts => "shortcuts",
            Kind::Roles => "roles",
            Kind::Capacities => "capacities",
            Kind::Connections => "connections",
            Kind::Gateways => "gateways",
            Kind::Domains => "domains",
        }
    }

    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Kind::Workspaces => &["ws", "workspace"],
            Kind::Items => &["item", "it"],
            Kind::Jobs => &["job", "runs", "run"],
            Kind::Schedules => &["schedule", "sched"],
            Kind::OneLake => &["ol", "files", "tables"],
            Kind::Shortcuts => &["shortcut", "sc"],
            Kind::Roles => &["role", "ra"],
            Kind::Capacities => &["capacity", "cap"],
            Kind::Connections => &["connection", "conn"],
            Kind::Gateways => &["gateway", "gw"],
            Kind::Domains => &["domain", "dom"],
        }
    }

    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Kind::Workspaces => &["NAME", "TYPE", "CAPACITY", "ID"],
            Kind::Items => &["NAME", "TYPE", "DESCRIPTION", "ID"],
            Kind::Jobs => &[
                "STATUS",
                "JOB TYPE",
                "INVOKED BY",
                "START",
                "DURATION",
                "ID",
            ],
            Kind::Schedules => &["TYPE", "ENABLED", "WHEN", "TIMEZONE", "START", "END", "ID"],
            Kind::OneLake => &["NAME", "KIND", "SIZE", "MODIFIED"],
            Kind::Shortcuts => &["NAME", "PATH", "TARGET TYPE", "TARGET"],
            Kind::Roles => &["PRINCIPAL", "TYPE", "ROLE", "ID"],
            Kind::Capacities => &["NAME", "SKU", "REGION", "STATE", "ID"],
            Kind::Connections => &["NAME", "TYPE", "CONNECTIVITY", "PRIVACY", "ID"],
            Kind::Gateways => &["NAME", "TYPE", "MEMBERS", "VERSION", "ID"],
            Kind::Domains => &["NAME", "DESCRIPTION", "PARENT", "ID"],
        }
    }

    /// Columns for a query; item lists gain folder and workspace columns when flat.
    pub fn columns_for(q: &Query) -> &'static [&'static str] {
        match q.kind {
            Kind::Items if q.scope.workspace.is_none() => {
                &["NAME", "TYPE", "WORKSPACE", "FOLDER", "ID"]
            }
            Kind::Items if q.is_flat() => &["NAME", "TYPE", "FOLDER", "DESCRIPTION", "ID"],
            // Runs of every item in a workspace say which item they belong to.
            Kind::Jobs if q.scope.item.is_none() => &[
                "ITEM",
                "STATUS",
                "JOB TYPE",
                "INVOKED BY",
                "START",
                "DURATION",
                "ID",
            ],
            k => k.columns(),
        }
    }

    /// Whether this kind needs a workspace or item in scope before it can be listed.
    pub fn needs(self) -> ScopeNeed {
        match self {
            Kind::Workspaces
            | Kind::Capacities
            | Kind::Connections
            | Kind::Gateways
            | Kind::Domains => ScopeNeed::None,
            Kind::Items => ScopeNeed::Optional,
            Kind::Roles => ScopeNeed::Workspace,
            Kind::Jobs => ScopeNeed::WorkspaceOrItem,
            Kind::Schedules | Kind::OneLake | Kind::Shortcuts => ScopeNeed::Item,
        }
    }

    pub fn lookup(word: &str) -> Option<(Kind, Option<&'static str>)> {
        let w = word.to_ascii_lowercase();
        if let Some(t) = item_type_for_alias(&w) {
            return Some((Kind::Items, Some(t)));
        }
        Kind::ALL
            .into_iter()
            .find(|k| k.name() == w || k.aliases().contains(&w.as_str()))
            .map(|k| (k, None))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeNeed {
    None,
    Optional,
    Workspace,
    Item,
    WorkspaceOrItem,
}

/// `:notebooks`, `:pipelines` etc. are item lists filtered by Fabric item type.
pub const ITEM_TYPE_ALIASES: &[(&str, &str)] = &[
    ("notebooks", "Notebook"),
    ("nb", "Notebook"),
    ("pipelines", "DataPipeline"),
    ("pl", "DataPipeline"),
    ("lakehouses", "Lakehouse"),
    ("lh", "Lakehouse"),
    ("warehouses", "Warehouse"),
    ("wh", "Warehouse"),
    ("models", "SemanticModel"),
    ("sm", "SemanticModel"),
    ("reports", "Report"),
    ("rpt", "Report"),
    ("sparkjobs", "SparkJobDefinition"),
    ("sjd", "SparkJobDefinition"),
    ("dataflows", "Dataflow"),
    ("environments", "Environment"),
    ("env", "Environment"),
];

fn item_type_for_alias(w: &str) -> Option<&'static str> {
    ITEM_TYPE_ALIASES
        .iter()
        .find(|(a, _)| *a == w)
        .map(|(_, t)| *t)
}

/// Item types that can be run on demand, with the job type Fabric uses for
/// running, listing schedules and history.
pub const JOB_TYPES: &[(&str, &str)] = &[
    ("Notebook", "RunNotebook"),
    ("DataPipeline", "Pipeline"),
    ("SparkJobDefinition", "sparkjob"),
];

pub fn job_type_for(item_type: &str) -> Option<&'static str> {
    JOB_TYPES
        .iter()
        .find(|(t, _)| *t == item_type)
        .map(|(_, j)| *j)
}

/// Item types whose contents live in OneLake and can be browsed.
pub fn has_onelake(item_type: &str) -> bool {
    matches!(
        item_type,
        "Lakehouse" | "Warehouse" | "MirroredDatabase" | "SQLDatabase"
    )
}

/// Coarse health of a row, used to colour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    Pending,
    Warn,
    Bad,
    Neutral,
}

impl Health {
    pub fn from_status(s: &str) -> Health {
        match s.to_ascii_lowercase().as_str() {
            "completed" | "succeeded" | "active" | "online" | "true" => Health::Ok,
            "inprogress" | "running" | "notstarted" | "provisioning" => Health::Pending,
            "cancelled" | "deduped" | "paused" | "suspended" => Health::Warn,
            "failed" | "error" | "false" => Health::Bad,
            _ => Health::Neutral,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub cells: Vec<String>,
    pub health: Health,
    /// A container (workspace folder, OneLake directory) that Enter opens.
    pub dir: bool,
    /// The API object the row was built from, shown by describe.
    pub raw: Value,
}

impl Row {
    pub fn new(id: impl Into<String>, name: impl Into<String>, cells: Vec<String>) -> Row {
        Row {
            id: id.into(),
            name: name.into(),
            cells,
            health: Health::Neutral,
            dir: false,
            raw: Value::Null,
        }
    }

    pub fn dir(mut self) -> Row {
        self.dir = true;
        self
    }

    pub fn health(mut self, h: Health) -> Row {
        self.health = h;
        self
    }

    pub fn raw(mut self, v: Value) -> Row {
        self.raw = v;
        self
    }

    /// Field from the raw JSON, as a string.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_name_alias_and_type() {
        assert_eq!(Kind::lookup("ws"), Some((Kind::Workspaces, None)));
        assert_eq!(Kind::lookup("JOBS"), Some((Kind::Jobs, None)));
        assert_eq!(Kind::lookup("nb"), Some((Kind::Items, Some("Notebook"))));
        assert_eq!(Kind::lookup("nope"), None);
    }

    #[test]
    fn every_kind_has_columns() {
        for k in Kind::ALL {
            assert!(!k.columns().is_empty(), "{k:?}");
        }
    }
}
