//! The active workspace, used like a k9s namespace: `:kind` jumps stay in it,
//! `1`..`9` switch between recently used workspaces and `0` lists across all.

use super::{App, Level};
use crate::resources::{Kind, ScopeNeed};
use crate::source::{Query, Ref, Scope};

/// How many recent workspaces get a number key.
const RECENT: usize = 9;

/// One profile's workspaces: the one being worked in and the recent ones.
#[derive(Debug, Clone, Default)]
pub struct Spaces {
    /// `None` means all workspaces.
    pub active: Option<Ref>,
    /// Newest first. A workspace already listed keeps its number.
    pub recent: Vec<Ref>,
}

impl App {
    pub fn recent_workspaces(&self) -> &[Ref] {
        self.spaces
            .get(&self.profile.account)
            .map_or(&[], |s| &s.recent)
    }

    pub fn active_workspace(&self) -> Option<&Ref> {
        self.spaces
            .get(&self.profile.account)
            .and_then(|s| s.active.as_ref())
    }

    /// Runs whenever the view changes. A view inside a workspace makes it the
    /// active one, a list across workspaces makes "all" active, and
    /// tenant-wide views (capacities, ...) leave it as it was.
    pub(super) fn follow_workspace(&mut self) {
        let q = &self.frame.query;
        let ws = match (&q.scope.workspace, q.kind.needs()) {
            (Some(ws), _) => Some(ws.clone()),
            (None, ScopeNeed::Optional) => None,
            _ => return,
        };
        let spaces = self.spaces.entry(self.profile.account.clone()).or_default();
        if let Some(ws) = &ws {
            match spaces.recent.iter_mut().find(|r| r.id == ws.id) {
                Some(known) => known.name = ws.name.clone(),
                None => {
                    spaces.recent.insert(0, ws.clone());
                    spaces.recent.truncate(RECENT);
                }
            }
        }
        spaces.active = ws;
    }

    /// The workspace list came in: pick up renames.
    pub(super) fn rename_known_workspaces(&mut self) {
        let Some(spaces) = self.spaces.get_mut(&self.profile.account) else {
            return;
        };
        for ws in spaces.recent.iter_mut().chain(spaces.active.as_mut()) {
            if let Some(row) = self.rows.iter().find(|r| r.id == ws.id) {
                ws.name = row.name.clone();
            }
        }
    }

    /// `1`..`9`: this view in a recent workspace. `0`: across all of them;
    /// views that aren't per workspace (the workspace list, capacities, ...)
    /// are already across all, so they stay and only "all" becomes active.
    pub(super) fn switch_workspace(&mut self, n: usize) {
        if n == 0 && self.kind().needs() == ScopeNeed::None {
            if let Some(spaces) = self.spaces.get_mut(&self.profile.account) {
                spaces.active = None;
            }
            return self.flash(Level::Info, "all workspaces");
        }
        if n == 0 {
            return self.view_in(None);
        }
        match self.recent_workspaces().get(n - 1).cloned() {
            Some(ws) => self.view_in(Some(ws)),
            None => self.flash(Level::Warn, format!("nothing on {n} yet: open a workspace")),
        }
    }

    /// The same kind of view in another workspace (or all) where that kind
    /// can follow, otherwise that workspace's items.
    fn view_in(&mut self, ws: Option<Ref>) {
        let q = &self.frame.query;
        let keep = match q.kind {
            Kind::Items => true,
            Kind::Roles => ws.is_some(),
            Kind::Jobs => ws.is_some() && q.scope.item.is_none(),
            _ => false,
        };
        let scope = Scope {
            workspace: ws,
            ..Scope::default()
        };
        let next = if keep {
            Query { scope, ..q.clone() }
        } else {
            Query::new(Kind::Items, scope)
        };
        self.push_query(next);
    }

    /// A workspace named on the command line: its id, its exact name (any
    /// case), or the only name that contains it.
    pub(super) fn find_workspace(&self, name: &str) -> Result<Ref, String> {
        let list = Query::new(Kind::Workspaces, Scope::default());
        let mut known: Vec<Ref> = self
            .cache
            .get(&(self.profile.account.clone(), list))
            .into_iter()
            .flatten()
            .map(|r| Ref {
                id: r.id.clone(),
                name: r.name.clone(),
            })
            .collect();
        for ws in self.recent_workspaces() {
            if !known.iter().any(|k| k.id == ws.id) {
                known.push(ws.clone());
            }
        }
        let want = name.to_lowercase();
        if let Some(ws) = known
            .iter()
            .find(|w| w.id == name || w.name.to_lowercase() == want)
        {
            return Ok(ws.clone());
        }
        let hits: Vec<&Ref> = known
            .iter()
            .filter(|w| w.name.to_lowercase().contains(&want))
            .collect();
        match hits.as_slice() {
            [] => Err(format!("no workspace matches {name}")),
            [one] => Ok((*one).clone()),
            many => {
                let mut names: Vec<&str> = many.iter().take(3).map(|w| w.name.as_str()).collect();
                if many.len() > 3 {
                    names.push("…");
                }
                Err(format!(
                    "{} workspaces match {name}: {}",
                    many.len(),
                    names.join(", ")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crate::app::{App, ProfileInfo};
    use crate::resources::Kind;
    use crossterm::event::KeyCode;

    fn press(app: &mut App, c: KeyCode) {
        app.on_key(key(c));
        settle(app);
    }

    fn command(app: &mut App, s: &str) {
        type_str(app, s);
        press(app, KeyCode::Enter);
    }

    fn workspace(app: &App) -> Option<&str> {
        app.frame
            .query
            .scope
            .workspace
            .as_ref()
            .map(|w| w.name.as_str())
    }

    fn recent(app: &App) -> Vec<&str> {
        app.recent_workspaces()
            .iter()
            .map(|w| w.name.as_str())
            .collect()
    }

    /// Open Urban1 Bronze, then Urban2 Silver, from the workspace list.
    fn bronze_then_silver() -> App {
        let mut app = fake_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        app
    }

    #[test]
    fn opened_workspaces_are_numbered_newest_first() {
        let mut app = bronze_then_silver();
        assert_eq!(recent(&app), ["Urban2 Silver", "Urban1 Bronze"]);
        // Going back to one already listed doesn't renumber them.
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(workspace(&app), Some("Urban1 Bronze"));
        assert_eq!(recent(&app), ["Urban2 Silver", "Urban1 Bronze"]);
    }

    #[test]
    fn number_keys_keep_the_kind_and_zero_lists_all() {
        let mut app = bronze_then_silver();
        command(&mut app, ":nb");
        assert_eq!(workspace(&app), Some("Urban2 Silver"));
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(app.frame.query.item_type.as_deref(), Some("Notebook"));
        assert_eq!(workspace(&app), Some("Urban1 Bronze"));

        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.frame.query.item_type.as_deref(), Some("Notebook"));
        assert_eq!(workspace(&app), None);
        assert!(app.active_workspace().is_none());

        // Esc walks back through the switches.
        press(&mut app, KeyCode::Esc);
        assert_eq!(workspace(&app), Some("Urban1 Bronze"));
    }

    #[test]
    fn kinds_that_cant_follow_open_the_workspace_items() {
        let mut app = bronze_then_silver();
        command(&mut app, ":capacities");
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(workspace(&app), Some("Urban1 Bronze"));
        assert!(app.frame.query.item_type.is_none());

        press(&mut app, KeyCode::Char('9'));
        assert!(app.status.as_ref().unwrap().text.contains("nothing on 9"));
    }

    #[test]
    fn zero_on_a_view_across_all_stays_there() {
        let mut app = bronze_then_silver();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.kind(), Kind::Workspaces);
        let depth = app.stack.len();
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.kind(), Kind::Workspaces);
        assert_eq!(app.stack.len(), depth);
        assert!(app.active_workspace().is_none());
        // Jumps from here no longer fall back to Silver.
        command(&mut app, ":capacities");
        command(&mut app, ":items");
        assert_eq!(workspace(&app), None);
    }

    #[test]
    fn jumps_from_tenant_views_stay_in_the_active_workspace() {
        let mut app = bronze_then_silver();
        command(&mut app, ":capacities");
        command(&mut app, ":roles");
        assert_eq!(app.kind(), Kind::Roles);
        assert_eq!(workspace(&app), Some("Urban2 Silver"));

        // `all` sticks, like k9s.
        command(&mut app, ":nb all");
        command(&mut app, ":capacities");
        command(&mut app, ":items");
        assert_eq!(workspace(&app), None);
    }

    #[test]
    fn commands_take_a_workspace_name() {
        let mut app = fake_app();
        command(&mut app, ":nb urban1 bronze");
        assert_eq!(app.frame.query.item_type.as_deref(), Some("Notebook"));
        assert_eq!(workspace(&app), Some("Urban1 Bronze"));

        command(&mut app, ":ws sand");
        assert_eq!(app.kind(), Kind::Items);
        assert_eq!(workspace(&app), Some("Sandbox"));

        command(&mut app, ":items urban");
        let status = &app.status.as_ref().unwrap().text;
        assert!(status.starts_with("4 workspaces match urban"), "{status}");
        assert_eq!(workspace(&app), Some("Sandbox"));

        command(&mut app, ":capacities sandbox");
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("not per workspace")
        );
    }

    #[test]
    fn each_profile_has_its_own_workspaces() {
        let p = |n: &str, a: &str| ProfileInfo {
            name: n.into(),
            account: a.into(),
            ..Default::default()
        };
        let mut app = App::new(vec![p("one", "one@x"), p("two", "two@y")], 0);
        settle(&mut app);
        press(&mut app, KeyCode::Enter);
        assert_eq!(recent(&app), ["Sandbox"]);
        command(&mut app, ":ctx two");
        assert!(app.recent_workspaces().is_empty());
        command(&mut app, ":ctx one");
        assert_eq!(recent(&app), ["Sandbox"]);
    }
}
