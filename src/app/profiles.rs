//! Switching between identities. Each profile keeps its own source and its
//! own cached rows, so switching back and forth is instant.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Auth, Effect, Frame, Level, Mode, Prompt, Purpose};
use crate::resources::Kind;
use crate::source::{Query, Scope};

impl App {
    pub(super) fn open_profiles(&mut self) {
        self.picker = self.active;
        self.mode = Mode::Profiles;
        self.check_all();
    }

    /// Ask every profile's source for a token, to show who is signed in.
    pub(super) fn check_all(&mut self) {
        for p in &mut self.profiles {
            p.auth = Auth::Checking;
            self.effects.push(Effect::Check {
                profile: p.account.clone(),
                fresh: false,
            });
        }
        self.profile.auth = Auth::Checking;
    }

    pub(super) fn set_auth(&mut self, account: &str, auth: Auth) {
        for p in &mut self.profiles {
            if p.account == account {
                p.auth = auth.clone();
            }
        }
        if self.profile.account == account {
            self.profile.auth = auth;
        }
    }

    pub(super) fn on_logged_in(&mut self, account: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.flash(Level::Info, format!("signed in as {account}"));
                self.set_auth(account, Auth::Checking);
                self.effects.push(Effect::Check {
                    profile: account.to_owned(),
                    fresh: false,
                });
                if self.profile.account == account {
                    self.cache.retain(|(a, _), _| a != account);
                    self.refresh(true);
                }
            }
            Err(e) => {
                self.set_auth(account, Auth::from_error(&e));
                self.flash(Level::Error, format!("signing in: {e}"));
            }
        }
    }

    /// `l` in the picker: sign the highlighted profile in again. A service
    /// principal needs its secret, so it gets the az command instead.
    fn login(&mut self) {
        let p = &self.profiles[self.picker];
        // It signs itself in from the environment: just do that again.
        if p.from_env {
            let account = p.account.clone();
            self.set_auth(&account, Auth::Checking);
            self.effects.push(Effect::Check {
                profile: account,
                fresh: true,
            });
            return;
        }
        if p.service_principal {
            let hint = crate::auth::service_principal_hint(&p.account, &p.tenant);
            self.detail = super::Detail {
                title: format!("sign in {}", p.name),
                lines: hint.lines().map(str::to_owned).collect(),
                scroll: 0,
            };
            self.mode = Mode::Detail;
            return;
        }
        let account = p.account.clone();
        self.set_auth(&account, Auth::Checking);
        self.effects.push(Effect::Login { profile: account });
    }

    /// `e` in the picker: rename the highlighted profile.
    fn rename_profile(&mut self) {
        let p = &self.profiles[self.picker];
        self.input = p.name.clone();
        self.prompt = Some(Prompt {
            title: format!("rename profile {}", p.name),
            hint: format!("{} · enter saves to the config · esc cancels", p.account),
            purpose: Purpose::RenameProfile {
                account: p.account.clone(),
            },
            choice: 0,
            place: None,
        });
        self.mode = Mode::Prompt;
    }

    /// Apply a new profile name, here and in the config file.
    pub(super) fn apply_profile_name(&mut self, account: &str, name: String) {
        if self
            .profiles
            .iter()
            .any(|p| p.name == name && p.account != account)
        {
            return self.flash(Level::Warn, format!("another profile is named {name}"));
        }
        for p in &mut self.profiles {
            if p.account == account {
                p.name = name.clone();
            }
        }
        if self.profile.account == account {
            self.profile.name = name.clone();
        }
        self.effects.push(Effect::RenameProfile {
            profile: account.to_owned(),
            name,
        });
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
            KeyCode::Char('e') => self.rename_profile(),
            KeyCode::Char('l') => self.login(),
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
            ..Default::default()
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
        assert!(matches!(fetch, Effect::Fetch { profile, .. } if profile == "me@y"));
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

    #[test]
    fn rename_from_the_picker_keeps_the_source() {
        let mut app = two();
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        settle(&mut app);
        assert_eq!(app.profiles[0].auth, crate::app::Auth::SignedIn);
        app.on_key(key(KeyCode::Char('e')));
        assert_eq!(app.mode, Mode::Prompt);
        assert_eq!(app.input, "fabric");
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_str(&mut app, "devops");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.profiles[0].name, "fabric", "names stay unique");
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("another profile")
        );
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_str(&mut app, "prod");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Profiles, "back to the picker");
        assert_eq!(
            (app.profile.name.as_str(), app.profiles[0].name.as_str()),
            ("prod", "prod")
        );
        let e = app.take_effects();
        assert!(matches!(&e[..], [Effect::RenameProfile { profile, name }]
            if profile == "admin@x" && name == "prod"));
        // Fetches still go to the same source.
        app.refresh(true);
        assert!(
            matches!(app.take_effects().pop(), Some(Effect::Fetch { profile, .. }) if profile == "admin@x")
        );
    }

    #[test]
    fn sign_in_failures_read_as_expired_or_errors() {
        use crate::app::Auth;
        let expired = "az account failed: ERROR: AADSTS70043: The refresh token has expired \
                       due to sign-in frequency checks by conditional access.";
        assert_eq!(Auth::from_error(expired), Auth::Expired);
        assert_eq!(
            Auth::from_error("az has no login for x: run `fablic login x`"),
            Auth::Expired
        );
        assert_eq!(
            Auth::from_error("running az (is the Azure CLI installed?)"),
            Auth::Failed("running az (is the Azure CLI installed?)".into())
        );
    }

    #[test]
    fn service_principals_get_the_az_command_not_a_browser() {
        let mut app = App::new(
            vec![ProfileInfo {
                name: "sp".into(),
                account: "00000000-1111".into(),
                service_principal: true,
                tenant: "t-1".into(),
                ..Default::default()
            }],
            0,
        );
        settle(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        settle(&mut app);
        app.on_key(key(KeyCode::Char('l')));
        assert!(
            app.take_effects()
                .iter()
                .all(|e| !matches!(e, Effect::Login { .. }))
        );
        assert_eq!(app.mode, Mode::Detail);
        let text = app.detail.lines.join("\n");
        assert!(
            text.contains(
                "az login --service-principal \\\n  --username 00000000-1111 \\\n  --tenant t-1"
            ),
            "{text}"
        );
        assert!(app.detail.lines.iter().all(|l| l.chars().count() < 80));
    }

    #[test]
    fn env_profiles_sign_in_again_without_the_terminal() {
        let mut app = App::new(
            vec![ProfileInfo {
                name: "env".into(),
                account: "48cd".into(),
                service_principal: true,
                from_env: true,
                ..Default::default()
            }],
            0,
        );
        settle(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        settle(&mut app);
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.mode, Mode::Profiles);
        let e = app.take_effects();
        assert!(matches!(&e[..], [Effect::Check { profile, fresh: true }] if profile == "48cd"));
    }
}
