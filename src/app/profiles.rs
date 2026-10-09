//! Switching between identities. Each profile keeps its own source and its
//! own cached rows, so switching back and forth is instant.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Frame, Level, Mode};
use crate::resources::Kind;
use crate::source::{Query, Scope};

impl App {
    pub(super) fn open_profiles(&mut self) {
        self.picker = self.active;
        self.mode = Mode::Profiles;
    }

    pub(super) fn key_profiles(&mut self, key: KeyEvent) {
        let n = self.profiles.len();
        match key.code {
            KeyCode::Esc => self.mode = Mode::Table,
            KeyCode::Char('j') | KeyCode::Down => self.picker = (self.picker + 1) % n,
            KeyCode::Char('k') | KeyCode::Up => self.picker = (self.picker + n - 1) % n,
            KeyCode::Enter => {
                self.mode = Mode::Table;
                self.switch_profile(self.picker);
            }
            _ => {}
        }
    }

    /// Find a profile by name or account (prefix match), for `:ctx <name>`.
    pub(super) fn find_profile(&self, name: &str) -> Option<usize> {
        let n = name.to_lowercase();
        self.profiles
            .iter()
            .position(|p| p.name.to_lowercase() == n)
            .or_else(|| {
                self.profiles.iter().position(|p| {
                    p.name.to_lowercase().starts_with(&n)
                        || p.account.to_lowercase().starts_with(&n)
                })
            })
    }

    pub fn switch_profile(&mut self, i: usize) {
        if i == self.active || i >= self.profiles.len() {
            return;
        }
        self.active = i;
        self.profile = self.profiles[i].clone();
        self.stack.clear();
        self.frame = Frame {
            query: Query::new(Kind::Workspaces, Scope::default()),
            filter: String::new(),
            selected: 0,
        };
        self.load();
        self.flash(
            Level::Info,
            format!(
                "switched to {} ({})",
                self.profile.name, self.profile.account
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{key, settle, type_str};
    use crate::app::{App, Effect, Mode, ProfileInfo};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn two() -> App {
        let p = |n: &str, a: &str| ProfileInfo {
            name: n.into(),
            account: a.into(),
            readonly: false,
            color: None,
        };
        let mut app = App::new(vec![p("fabric", "admin@x"), p("devops", "me@y")], 0);
        settle(&mut app);
        app
    }

    #[test]
    fn picker_switches_and_fetches_with_new_profile() {
        let mut app = two();
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, Mode::Profiles);
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.profile.name, "devops");
        let fetch = app.take_effects().pop().unwrap();
        assert!(matches!(fetch, Effect::Fetch { profile, .. } if profile == "devops"));
    }

    #[test]
    fn ctx_command_matches_name_or_account() {
        let mut app = two();
        type_str(&mut app, ":ctx me@");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.profile.name, "devops");
        type_str(&mut app, ":ctx fab");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.profile.name, "fabric");
        type_str(&mut app, ":ctx");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Profiles);
    }
}
