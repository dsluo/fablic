//! Drilling into rows, jumping between kinds, and going back.

use super::{App, Frame, Level, Mode};
use crate::resources::{Kind, Row, ScopeNeed, has_onelake, job_type_for};
use crate::source::{ItemRef, Query, Ref, Scope};

impl App {
    fn push(&mut self, query: Query) {
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
            Kind::Workspaces => Some(Query {
                kind: Kind::Items,
                scope: Scope {
                    workspace: Some(Ref {
                        id: row.id.clone(),
                        name: row.name.clone(),
                    }),
                    ..Scope::default()
                },
                item_type: None,
            }),
            Kind::Items => {
                let item = item_ref(&row);
                let scope = Scope {
                    workspace: Some(workspace_of(&scope, &row)),
                    item: Some(item.clone()),
                    path: None,
                };
                if has_onelake(&item.item_type) {
                    Some(Query {
                        kind: Kind::OneLake,
                        scope,
                        item_type: None,
                    })
                } else if job_type_for(&item.item_type).is_some() {
                    Some(Query {
                        kind: Kind::Jobs,
                        scope,
                        item_type: None,
                    })
                } else {
                    None
                }
            }
            Kind::OneLake if row.raw.get("isDirectory").and_then(|v| v.as_bool()) == Some(true) => {
                Some(Query {
                    kind: Kind::OneLake,
                    scope: Scope {
                        path: Some(row.id.clone()),
                        ..scope
                    },
                    item_type: None,
                })
            }
            _ => None,
        };
        match next {
            Some(q) => {
                self.flash(Level::Info, format!("↳ {}", q.scope.label()));
                self.push(q);
            }
            None => self.describe(),
        }
    }

    /// `:kind` jump. Keeps whatever scope still applies, and treats the selected
    /// workspace or item as the scope when the target kind needs one.
    pub fn goto(&mut self, kind: Kind, item_type: Option<&str>) {
        let scope = self.context_scope();
        let scope = match kind.needs() {
            ScopeNeed::None => Scope::default(),
            ScopeNeed::Optional => Scope {
                workspace: scope.workspace,
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
                    path: None,
                    ..scope
                },
                None => return self.need("an item", kind),
            },
            ScopeNeed::WorkspaceOrItem => match scope.workspace {
                Some(_) => Scope {
                    path: None,
                    ..scope
                },
                None => return self.need("a workspace or item", kind),
            },
        };
        self.push(Query {
            kind,
            scope,
            item_type: item_type.map(str::to_owned),
        });
    }

    fn need(&mut self, what: &str, kind: Kind) {
        self.flash(
            Level::Warn,
            format!("{} needs {what}: select one first (:ws)", kind.name()),
        );
    }

    /// Current scope, narrowed by the highlighted row when it is a workspace or item.
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

/// The workspace an item row belongs to. Items listed across all workspaces
/// only carry the id, so the id doubles as the name there.
fn workspace_of(scope: &Scope, row: &Row) -> Ref {
    if let Some(ws) = &scope.workspace {
        return ws.clone();
    }
    let id = row.field("workspaceId").unwrap_or_default().to_owned();
    Ref {
        name: id.clone(),
        id,
    }
}

#[cfg(test)]
mod tests {
    use crate::app::Mode;
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crate::resources::Kind;
    use crossterm::event::KeyCode;

    #[test]
    fn drill_workspace_item_jobs_and_back() {
        let mut app = fake_app();
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(app.frame.query.scope.workspace.as_ref().unwrap().id, "ws-1");

        // ingest_shop is a notebook: drills to its jobs.
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::Jobs);
        assert_eq!(app.view.len(), 3);

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.kind(), Kind::Items);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.kind(), Kind::Workspaces);
        assert!(app.stack.is_empty());
    }

    #[test]
    fn drill_lakehouse_into_onelake_folders() {
        let mut app = fake_app();
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        app.on_key(key(KeyCode::Char('j'))); // bronze_lh
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::OneLake);
        app.on_key(key(KeyCode::Char('j'))); // Files
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.frame.query.scope.path.as_deref(), Some("Files"));
        // a file opens the detail view instead of drilling
        app.on_key(key(KeyCode::Char('G')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Detail);
    }

    #[test]
    fn goto_uses_selected_workspace_and_refuses_missing_scope() {
        let mut app = fake_app();
        type_str(&mut app, ":schedules");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.kind(), Kind::Workspaces);
        assert!(app.status.as_ref().unwrap().text.contains("needs an item"));

        type_str(&mut app, ":nb");
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(app.frame.query.item_type.as_deref(), Some("Notebook"));
        assert_eq!(app.frame.query.scope.workspace.as_ref().unwrap().id, "ws-1");
        assert_eq!(app.view.len(), 2);
    }
}
