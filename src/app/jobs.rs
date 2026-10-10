//! Running and cancelling jobs, behind a confirm prompt and the read-only guard.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Level, Mode};
use crate::resources::{Health, Kind, Row, job_type_for};
use crate::source::{Action, ItemRef, Query, Scope};

/// How long a jobs view keeps polling after a run or cancel, so a new run
/// shows up even before it reports InProgress.
const WATCH_AFTER_ACTION: Duration = Duration::from_secs(60);

impl App {
    /// The item a jobs-related key applies to: the selected item row, the
    /// view's item, or (in a workspace-wide jobs view) the selected run's item.
    fn job_item(&self) -> Option<(String, ItemRef)> {
        let scope = &self.frame.query.scope;
        let row = self.selected_row();
        let ws_of = |r: Option<&Row>| {
            scope
                .workspace
                .as_ref()
                .map(|w| w.id.clone())
                .or_else(|| r.and_then(|r| r.field("workspaceId")).map(str::to_owned))
        };
        match self.kind() {
            Kind::Items => {
                let r = row.filter(|r| !r.dir)?;
                let item = ItemRef {
                    id: r.id.clone(),
                    name: r.name.clone(),
                    item_type: r.field("type")?.to_owned(),
                };
                Some((ws_of(Some(r))?, item))
            }
            Kind::Jobs | Kind::Schedules => match &scope.item {
                Some(item) => Some((ws_of(None)?, item.clone())),
                None => {
                    let r = row?;
                    let item = ItemRef {
                        id: r.field("itemId")?.to_owned(),
                        name: r.field("itemName")?.to_owned(),
                        item_type: r.field("itemType")?.to_owned(),
                    };
                    Some((ws_of(Some(r))?, item))
                }
            },
            _ => None,
        }
    }

    pub(super) fn run_job(&mut self) {
        let Some((workspace, item)) = self.job_item() else {
            return self.flash(
                Level::Warn,
                "select a notebook, pipeline or Spark job to run",
            );
        };
        let Some(job_type) = job_type_for(&item.item_type) else {
            return self.flash(
                Level::Warn,
                format!("{} items can't be run", item.item_type),
            );
        };
        self.confirm(Action::RunJob {
            workspace,
            item,
            job_type: job_type.to_owned(),
        });
    }

    pub(super) fn cancel_job(&mut self) {
        if self.kind() != Kind::Jobs {
            return self.flash(Level::Warn, "ctrl-k cancels a run in a jobs view");
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.health != Health::Pending {
            return self.flash(Level::Warn, "that run isn't running");
        }
        let job = row.id.clone();
        let Some((workspace, item)) = self.job_item() else {
            return;
        };
        self.confirm(Action::CancelJob {
            workspace,
            item,
            job,
        });
    }

    pub(super) fn key_confirm(&mut self, key: KeyEvent) {
        self.mode = Mode::Table;
        let Some(action) = self.pending.take() else {
            return;
        };
        if key.code != KeyCode::Char('y') {
            return self.flash(Level::Info, "cancelled");
        }
        // Running from an item list jumps to that item's runs to watch it.
        if let Action::RunJob {
            workspace, item, ..
        } = &action
            && self.kind() != Kind::Jobs
        {
            let ws = self.frame.query.scope.workspace.clone();
            let scope = Scope {
                workspace: ws.filter(|w| &w.id == workspace).or_else(|| {
                    Some(crate::source::Ref {
                        id: workspace.clone(),
                        name: workspace.clone(),
                    })
                }),
                item: Some(item.clone()),
                ..Scope::default()
            };
            self.push_query(Query::new(Kind::Jobs, scope));
        }
        self.act(action);
    }

    pub(super) fn on_acted(&mut self, action: Action, result: Result<String, String>) {
        match result {
            Ok(msg) => {
                self.flash(Level::Info, msg);
                if matches!(action, Action::Move { .. }) {
                    self.moving = None;
                }
                // Cached rows of other views may show what just changed.
                self.cache.clear();
                if self.kind() == Kind::Jobs {
                    self.live_until = Some(Instant::now() + WATCH_AFTER_ACTION);
                }
                self.refresh(true);
            }
            Err(e) => self.flash(Level::Error, format!("{} failed: {e}", action.describe())),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crate::app::{App, Effect, Mode, ProfileInfo};
    use crate::resources::Kind;
    use crate::source::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(app: &mut App, c: KeyCode) {
        app.on_key(key(c));
        settle(app);
    }

    fn ctrl(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    /// Urban1 Bronze, root folder; transform_silver (a notebook) is last.
    fn at_notebook() -> App {
        let mut app = fake_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('G'));
        app
    }

    fn acts(app: &mut App) -> Vec<Action> {
        app.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Act { action, .. } => Some(action),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn run_from_items_confirms_then_opens_its_jobs() {
        let mut app = at_notebook();
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.mode, Mode::Confirm);
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(app.kind(), Kind::Jobs);
        assert_eq!(
            app.frame.query.scope.label(),
            "Urban1 Bronze › transform_silver (Notebook)"
        );
        let a = acts(&mut app);
        assert!(matches!(&a[..], [Action::RunJob { job_type, .. }] if job_type == "RunNotebook"));
    }

    #[test]
    fn anything_but_y_cancels() {
        let mut app = at_notebook();
        app.on_key(key(KeyCode::Char('r')));
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.mode, Mode::Table);
        assert!(acts(&mut app).is_empty());
        assert_eq!(app.kind(), Kind::Items);
    }

    #[test]
    fn readonly_profile_blocks_writes() {
        let mut app = App::new(
            vec![ProfileInfo {
                name: "prod".into(),
                account: "a@x".into(),
                readonly: true,
                color: None,
            }],
            0,
        );
        settle(&mut app);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('G'));
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.mode, Mode::Table);
        assert!(app.status.as_ref().unwrap().text.contains("read-only"));
    }

    #[test]
    fn cancel_only_running_runs_in_workspace_jobs() {
        let mut app = fake_app();
        press(&mut app, KeyCode::Char('j'));
        type_str(&mut app, ":jobs");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Jobs);
        assert_eq!(app.rows[app.view[0]].cells[1], "InProgress");

        ctrl(&mut app, 'k');
        app.on_key(key(KeyCode::Char('y')));
        let a = acts(&mut app);
        let [Action::CancelJob { item, job, .. }] = &a[..] else {
            panic!("{a:?}");
        };
        assert_eq!(job, &format!("{}-3", item.id));
        assert!(!item.item_type.is_empty());

        press(&mut app, KeyCode::Char('G')); // a finished run
        ctrl(&mut app, 'k');
        assert_eq!(app.mode, Mode::Table);
        assert!(app.status.as_ref().unwrap().text.contains("isn't running"));
    }

    #[test]
    fn s_opens_schedules_of_selected_item() {
        let mut app = at_notebook();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.kind(), Kind::Schedules);
        assert_eq!(app.rows[app.view[0]].cells[2], "daily 01:00");
    }
}
