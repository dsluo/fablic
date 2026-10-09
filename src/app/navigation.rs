//! Drilling into rows, jumping between kinds, and going back.

use super::{App, Frame, Level, Mode};
use crate::resources::{Kind, Row, ScopeNeed, has_onelake, job_type_for};
use crate::source::{ItemRef, Query, Ref, Scope};

impl App {
    pub(super) fn push_query(&mut self, query: Query) {
        if query == self.frame.query {
            // `:ws` while already on workspaces: just refresh, don't stack a copy.
            self.mode = Mode::Table;
            return self.refresh(true);
        }
        let old = std::mem::replace(
            &mut self.frame,
            Frame {
                query,
                filter: String::new(),
                selected: 0,
            },
        );
        self.stack.push(old);
        self.mode = Mode::Table;
        self.load();
    }

    /// Esc: clear an active filter first, otherwise pop back to the previous view.
    pub fn back(&mut self) {
        if !self.frame.filter.is_empty() {
            self.frame.filter.clear();
            self.apply_filter();
            return;
        }
        if let Some(prev) = self.stack.pop() {
            self.frame = prev;
            self.load();
        }
    }

    pub fn drill(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let scope = self.frame.query.scope.clone();
        let next = match self.kind() {
            Kind::Workspaces => Some(Query::new(
                Kind::Items,
                Scope {
                    workspace: Some(Ref {
                        id: row.id.clone(),
                        name: row.name.clone(),
                    }),
                    ..Scope::default()
                },
            )),
            // A workspace folder: the next tree level, same workspace.
            Kind::Items if row.dir => Some(Query::new(
                Kind::Items,
                Scope {
                    folder: Some(folder_ref(&row)),
                    ..scope
                },
            )),
            Kind::Items => {
                let item = item_ref(&row);
                let scope = Scope {
                    workspace: Some(workspace_of(&scope, &row)),
                    item: Some(item.clone()),
                    ..Scope::default()
                };
                if has_onelake(&item.item_type) {
                    Some(Query::new(Kind::OneLake, scope))
                } else if job_type_for(&item.item_type).is_some() {
                    Some(Query::new(Kind::Jobs, scope))
                } else {
                    None
                }
            }
            Kind::OneLake if row.dir => Some(Query::new(
                Kind::OneLake,
                Scope {
                    path: Some(row.id.clone()),
                    ..scope
                },
            )),
            _ => None,
        };
        match next {
            Some(q) => {
                self.flash(Level::Info, format!("↳ {}", q.scope.label()));
                self.push_query(q);
            }
            None if self.kind() == Kind::OneLake => self.preview(),
            None => self.describe(),
        }
    }

    /// `:kind` jump. Keeps whatever scope still applies, and treats the selected
    /// workspace, folder or item as the scope when the target kind needs one.
    /// `all` drops the scope for kinds that can list across workspaces.
    pub fn goto(&mut self, kind: Kind, item_type: Option<&str>, all: bool) {
        let scope = self.context_scope();
        let scope = match kind.needs() {
            ScopeNeed::None => Scope::default(),
            ScopeNeed::Optional if all => Scope::default(),
            // Listing kinds search the folder being viewed, not a highlighted
            // subfolder; a highlighted workspace still picks the workspace.
            ScopeNeed::Optional => Scope {
                workspace: scope.workspace,
                folder: self.frame.query.scope.folder.clone(),
                ..Scope::default()
            },
            ScopeNeed::Workspace => match scope.workspace {
                Some(ws) => Scope {
                    workspace: Some(ws),
                    ..Scope::default()
                },
                None => return self.need("a workspace", kind),
            },
            ScopeNeed::Item => match scope.item {
                Some(_) => Scope {
                    folder: None,
                    path: None,
                    ..scope
                },
                None => return self.need("an item", kind),
            },
            ScopeNeed::WorkspaceOrItem => match scope.workspace {
                Some(_) => Scope {
                    folder: None,
                    path: None,
                    ..scope
                },
                None => return self.need("a workspace or item", kind),
            },
        };
        let mut q = Query::new(kind, scope);
        q.item_type = item_type.map(str::to_owned);
        self.push_query(q);
    }

    fn need(&mut self, what: &str, kind: Kind) {
        self.flash(
            Level::Warn,
            format!("{} needs {what}: select one first (:ws)", kind.name()),
        );
    }

    /// Current scope, narrowed by the highlighted row when it is a workspace,
    /// folder or item.
    fn context_scope(&self) -> Scope {
        let mut scope = self.frame.query.scope.clone();
        if let Some(row) = self.selected_row() {
            match self.kind() {
                Kind::Workspaces => {
                    scope.workspace = Some(Ref {
                        id: row.id.clone(),
                        name: row.name.clone(),
                    })
                }
                Kind::Items if row.dir => scope.folder = Some(folder_ref(row)),
                Kind::Items => {
                    scope.workspace = Some(workspace_of(&scope, row));
                    scope.item = Some(item_ref(row));
                }
                _ => {}
            }
        }
        scope
    }
}

fn item_ref(row: &Row) -> ItemRef {
    ItemRef {
        id: row.id.clone(),
        name: row.name.clone(),
        item_type: row.field("type").unwrap_or_default().to_owned(),
    }
}

/// A folder row as scope; the name is its full path so the label shows it.
fn folder_ref(row: &Row) -> Ref {
    Ref {
        id: row.id.clone(),
        name: row.field("path").unwrap_or(&row.name).to_owned(),
    }
}

/// The workspace an item row belongs to: the scope's, or the one recorded on
/// the row when it came from a list across all workspaces.
fn workspace_of(scope: &Scope, row: &Row) -> Ref {
    if let Some(ws) = &scope.workspace {
        return ws.clone();
    }
    let id = row.field("workspaceId").unwrap_or_default().to_owned();
    let name = row.field("workspaceName").unwrap_or(&id).to_owned();
    Ref { id, name }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crate::app::{App, Mode};
    use crate::resources::Kind;
    use crossterm::event::KeyCode;

    fn press(app: &mut App, c: KeyCode) {
        app.on_key(key(c));
        settle(app);
    }

    /// Workspaces are sorted, so Urban1 Bronze (the one with folders) is second.
    fn into_bronze() -> App {
        let mut app = fake_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        app
    }

    fn names(app: &App) -> Vec<String> {
        app.view.iter().map(|&i| app.rows[i].name.clone()).collect()
    }

    #[test]
    fn workspace_opens_at_root_folder() {
        let app = into_bronze();
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(
            names(&app),
            ["ingest", "bronze_lh", "Finance Model", "transform_silver"]
        );
    }

    #[test]
    fn drill_folders_then_jobs_and_back_out() {
        let mut app = into_bronze();
        press(&mut app, KeyCode::Enter); // ingest/
        assert_eq!(app.frame.query.scope.label(), "Urban1 Bronze › ingest");
        assert_eq!(names(&app), ["shop", "load_all"]);
        press(&mut app, KeyCode::Enter); // shop/
        assert_eq!(
            app.frame.query.scope.label(),
            "Urban1 Bronze › ingest › shop"
        );
        press(&mut app, KeyCode::Enter); // ingest_shop is a notebook: its jobs
        assert_eq!(app.kind(), Kind::Jobs);
        assert_eq!(app.view.len(), 3);

        for _ in 0..3 {
            press(&mut app, KeyCode::Esc);
        }
        assert_eq!(
            names(&app),
            ["ingest", "bronze_lh", "Finance Model", "transform_silver"]
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.kind(), Kind::Workspaces);
        assert!(app.stack.is_empty());
    }

    #[test]
    fn flatten_shows_subtree_with_folder_paths() {
        let mut app = into_bronze();
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.view.len(), 5);
        assert_eq!(app.rows[app.view[4]].cells[2], "/ingest/shop");
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.view.len(), 4);

        press(&mut app, KeyCode::Enter); // ingest/
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(names(&app), ["load_all", "ingest_shop"]);
    }

    #[test]
    fn drill_lakehouse_into_onelake_folders() {
        let mut app = into_bronze();
        press(&mut app, KeyCode::Char('j')); // bronze_lh
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::OneLake);
        // Files sorts first (directories, by name), so it's already selected.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.frame.query.scope.path.as_deref(), Some("Files"));
        // a file opens the detail view instead of drilling
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Detail);
    }

    #[test]
    fn goto_uses_selection_and_refuses_missing_scope() {
        let mut app = fake_app();
        type_str(&mut app, ":schedules");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.kind(), Kind::Workspaces);
        assert!(app.status.as_ref().unwrap().text.contains("needs an item"));

        press(&mut app, KeyCode::Char('j'));
        type_str(&mut app, ":nb");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(app.frame.query.item_type.as_deref(), Some("Notebook"));
        assert_eq!(app.frame.query.scope.workspace.as_ref().unwrap().id, "ws-1");
        assert_eq!(names(&app), ["transform_silver", "ingest_shop"]);
    }

    #[test]
    fn listing_kinds_ignore_a_highlighted_folder() {
        let mut app = into_bronze(); // "ingest" folder is highlighted
        type_str(&mut app, ":nb");
        press(&mut app, KeyCode::Enter);
        assert!(app.frame.query.scope.folder.is_none());
        assert_eq!(names(&app), ["transform_silver", "ingest_shop"]);
    }

    #[test]
    fn items_all_lists_across_workspaces_and_drills_with_its_name() {
        let mut app = fake_app();
        type_str(&mut app, ":nb all");
        press(&mut app, KeyCode::Enter);
        assert!(app.frame.query.scope.workspace.is_none());
        assert_eq!(app.rows[app.view[0]].cells[2], "Urban1 Bronze");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Jobs);
        assert_eq!(
            app.frame.query.scope.label(),
            "Urban1 Bronze › ingest_shop (Notebook)"
        );
    }
}
