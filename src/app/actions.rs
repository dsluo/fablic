//! Per-row actions: item definitions, open in browser, flatten folders.

use super::{App, Detail, DetailRequest, Effect, Level, Mode};
use crate::resources::{Kind, Row};

const PORTAL: &str = "https://app.fabric.microsoft.com";

/// URL path segment the Fabric portal uses for an item type.
fn portal_segment(item_type: &str) -> Option<&'static str> {
    Some(match item_type {
        "Notebook" => "synapsenotebooks",
        "Lakehouse" => "lakehouses",
        "DataPipeline" => "pipelines",
        "Report" => "reports",
        "SemanticModel" => "datasets",
        "Warehouse" => "datawarehouses",
        "SparkJobDefinition" => "sparkjobdefinitions",
        "Environment" => "sparkenvironments",
        "Eventhouse" => "eventhouses",
        "KQLDatabase" => "databases",
        _ => return None,
    })
}

impl App {
    /// Workspace id for a row: the scope's, or the item's own for cross-workspace lists.
    fn workspace_id(&self, row: &Row) -> Option<String> {
        self.frame
            .query
            .scope
            .workspace
            .as_ref()
            .map(|w| w.id.clone())
            .or_else(|| row.field("workspaceId").map(str::to_owned))
    }

    pub(super) fn definition(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if self.kind() != Kind::Items || row.dir {
            return self.flash(Level::Warn, "y shows definitions of items");
        }
        let Some(workspace) = self.workspace_id(&row) else {
            return;
        };
        self.open_detail(
            format!("{} › definition", row.name),
            DetailRequest::Definition {
                workspace,
                item: row.id,
            },
        );
    }

    /// Show the detail view with a placeholder and fetch its contents.
    pub(super) fn open_detail(&mut self, title: String, request: DetailRequest) {
        self.detail = Detail {
            title: title.clone(),
            lines: vec!["loading…".into()],
            scroll: 0,
        };
        self.mode = Mode::Detail;
        self.effects.push(Effect::Detail {
            profile: self.profile.account.clone(),
            title,
            request,
        });
    }

    pub(super) fn open_in_browser(&mut self) {
        let row = self.selected_row().cloned();
        let ws = row.as_ref().and_then(|r| self.workspace_id(r));
        let url = match (self.kind(), row) {
            (Kind::Workspaces, Some(r)) => Some(format!("{PORTAL}/groups/{}", r.id)),
            (Kind::Items, Some(r)) if !r.dir => {
                let ws = ws.unwrap_or_default();
                Some(match portal_segment(r.field("type").unwrap_or("")) {
                    Some(seg) => format!("{PORTAL}/groups/{ws}/{seg}/{}", r.id),
                    None => format!("{PORTAL}/groups/{ws}"),
                })
            }
            _ => self
                .frame
                .query
                .scope
                .workspace
                .as_ref()
                .map(|w| format!("{PORTAL}/groups/{}", w.id)),
        };
        match url {
            Some(u) => {
                self.flash(Level::Info, format!("opening {u}"));
                self.effects.push(Effect::Open(u));
            }
            None => self.flash(Level::Warn, "nothing to open here"),
        }
    }

    /// `F`: switch a workspace's items between one folder level and the whole subtree.
    pub(super) fn toggle_flat(&mut self) {
        let q = &self.frame.query;
        if q.kind != Kind::Items || q.scope.workspace.is_none() || q.item_type.is_some() {
            return self.flash(Level::Warn, "F flattens folders in a workspace's items");
        }
        self.frame.query.flat = !self.frame.query.flat;
        self.frame.selected = 0;
        let what = if self.frame.query.flat {
            "all items below this folder"
        } else {
            "one folder level"
        };
        self.flash(Level::Info, format!("showing {what}"));
        self.load();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle};
    use crate::app::{Effect, Mode};
    use crossterm::event::KeyCode;

    #[test]
    fn definition_loads_into_detail() {
        let mut app = fake_app();
        app.on_key(key(KeyCode::Char('j'))); // Urban1 Bronze
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        app.on_key(key(KeyCode::Char('G'))); // transform_silver
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(app.mode, Mode::Detail);
        settle(&mut app);
        assert_eq!(app.detail.lines[0], "── notebook-content.py ──");
        assert!(
            app.detail
                .lines
                .iter()
                .any(|l| l.contains("print(\"it-5\")"))
        );
    }

    #[test]
    fn open_builds_portal_urls() {
        let mut app = fake_app();
        app.on_key(key(KeyCode::Char('o')));
        let url = app.take_effects().into_iter().find_map(|e| match e {
            Effect::Open(u) => Some(u),
            _ => None,
        });
        assert_eq!(
            url.as_deref(),
            Some("https://app.fabric.microsoft.com/groups/ws-3")
        );
    }
}
