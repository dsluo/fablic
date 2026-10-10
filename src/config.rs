//! `~/.config/fablic/config.toml`. Without one, every account az has cached
//! becomes a profile, so fablic works straight after `az login`.
//!
//! ```toml
//! default_profile = "fabric"
//!
//! [[profiles]]
//! name = "fabric"
//! account = "admin123@fabric.example.net"   # az login to use
//! readonly = false
//! color = "#89b4fa"                          # optional header colour
//!
//! [[profiles]]
//! name = "partner"
//! login = "entra"                            # fablic's own sign-in: `fablic login partner`
//! tenant = "partner.onmicrosoft.com"         # optional; any work account by default
//! client_id = "…"                            # optional; the Azure CLI's public client by default
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// `default` when left out.
    #[serde(default = "default_name")]
    pub name: String,
    /// How the profile gets tokens.
    #[serde(default)]
    pub login: Login,
    /// The account (UPN) this profile signs in as. Required for az; for
    /// fablic's own sign-in it's only a label until the first `fablic login`.
    pub account: Option<String>,
    #[serde(default)]
    pub readonly: bool,
    pub color: Option<String>,
    /// fablic sign-in only: the Entra tenant (id or domain).
    pub tenant: Option<String>,
    /// fablic sign-in only: a public client app registration to sign in with.
    pub client_id: Option<String>,
}

/// The name of a profile that doesn't give one.
pub const DEFAULT_PROFILE: &str = "default";

fn default_name() -> String {
    DEFAULT_PROFILE.into()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Login {
    /// Tokens from the az CLI's login cache.
    #[default]
    Az,
    /// fablic's own sign-in (`fablic login`), kept in the OS keychain.
    /// `device` is accepted too, its first name.
    #[serde(alias = "device")]
    Entra,
}

impl Profile {
    /// The az account to use, for az profiles.
    pub fn az_account(&self) -> Result<&str> {
        self.account.as_deref().with_context(|| {
            format!(
                "profile {} signs in with az but names no account; add account = \"you@example.com\"",
                self.name
            )
        })
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("config.toml"))
    }

    /// Read the config file; a missing file is an empty config.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let Some(path) = path else {
            return Ok(Config::default());
        };
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Config>(&text)
                .map_err(anyhow::Error::from)
                .and_then(Config::check)
                .with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Profile names must be unique (two unnamed profiles are both `default`).
    fn check(self) -> Result<Config> {
        for (i, p) in self.profiles.iter().enumerate() {
            if self.profiles[..i].iter().any(|q| q.name == p.name) {
                anyhow::bail!("two profiles are named {}; give one a name", p.name);
            }
        }
        Ok(self)
    }

    /// Add the profiles signed in with `fablic login`, unless the file
    /// defines a profile of that name (its settings win).
    pub fn with_logins(mut self, logins: &[SavedLogin]) -> Config {
        for l in logins {
            if !self.profiles.iter().any(|p| p.name == l.name) {
                self.profiles.push(l.profile());
            }
        }
        self
    }

    /// Whether the file says which az accounts to use. If not, every cached
    /// az account becomes a profile (after any fablic-login profiles).
    pub fn wants_az_accounts(&self) -> bool {
        !self.profiles.iter().any(|p| p.login == Login::Az)
    }

    /// One profile per cached az account, unless the file names az profiles.
    pub fn with_az_accounts(mut self, accounts: Vec<String>) -> Config {
        if self.wants_az_accounts() {
            let from_az = accounts
                .into_iter()
                .map(|account| Profile {
                    name: account.split('@').next().unwrap_or(&account).to_owned(),
                    login: Login::Az,
                    account: Some(account),
                    readonly: false,
                    color: None,
                    tenant: None,
                    client_id: None,
                })
                .filter(|a| !self.profiles.iter().any(|p| p.name == a.name))
                .collect::<Vec<_>>();
            self.profiles.extend(from_az);
        }
        self
    }

    /// The profile to start on: the flag, then `default_profile`, then the first.
    pub fn initial(&self, wanted: Option<&str>) -> Result<usize> {
        let name = wanted.or(self.default_profile.as_deref());
        match name {
            Some(n) => self
                .profiles
                .iter()
                .position(|p| {
                    p.name == n
                        || p.account
                            .as_deref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(n))
                })
                .with_context(|| {
                    let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
                    format!("no profile named {n} (have: {})", names.join(", "))
                }),
            None if self.profiles.is_empty() => anyhow::bail!(
                "no profiles: sign in with `az login`, or add [[profiles]] to {}",
                Config::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            None => Ok(0),
        }
    }
}

/// `$VAR/fablic`, or `~/<fallback>/fablic`. XDG layout on every platform,
/// macOS included, like k9s and sofka.
pub fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().join(fallback)))?;
    Some(base.join("fablic"))
}

/// A sign-in made with `fablic login` for a profile the config file
/// doesn't define. The refresh token itself is in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLogin {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl SavedLogin {
    pub fn profile(&self) -> Profile {
        Profile {
            name: self.name.clone(),
            login: Login::Entra,
            account: None,
            readonly: false,
            color: None,
            tenant: self.tenant.clone(),
            client_id: self.client_id.clone(),
        }
    }
}

/// `fablic login`'s own record of sign-ins: `~/.local/state/fablic/logins.json`.
pub fn logins_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("logins.json"))
}

/// The recorded sign-ins; a missing or unreadable file is none.
pub fn load_logins(path: Option<&Path>) -> Vec<SavedLogin> {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_logins(path: Option<&Path>, logins: &[SavedLogin]) -> Result<()> {
    let path = path.context("no home directory to record the sign-in in")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(logins)? + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

/// Logs and the action journal: `~/.local/state/fablic`.
pub fn state_dir() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_profiles_and_picks_initial() {
        let c: Config = toml::from_str(
            r#"
            default_profile = "dev"
            [[profiles]]
            name = "prod"
            account = "admin@x"
            readonly = true
            [[profiles]]
            name = "dev"
            account = "me@x"
            "#,
        )
        .unwrap();
        assert_eq!(c.initial(None).unwrap(), 1);
        assert_eq!(c.initial(Some("ADMIN@x")).unwrap(), 0);
        assert!(c.initial(Some("nope")).is_err());
        assert!(c.profiles[0].readonly);
    }

    #[test]
    fn falls_back_to_az_accounts() {
        let c = Config::default().with_az_accounts(vec!["admin1@a.net".into(), "me@b.org".into()]);
        assert_eq!(c.profiles[0].name, "admin1");
        assert_eq!(c.profiles[1].account.as_deref(), Some("me@b.org"));
        assert_eq!(c.initial(None).unwrap(), 0);
        assert!(Config::default().initial(None).is_err());
    }

    #[test]
    fn entra_profiles_need_no_account() {
        let c: Config = toml::from_str(
            r#"
            [[profiles]]
            name = "partner"
            login = "entra"
            tenant = "partner.onmicrosoft.com"
            [[profiles]]
            name = "broken"
            "#,
        )
        .unwrap();
        assert_eq!(c.profiles[0].login, Login::Entra);
        assert_eq!(
            c.profiles[0].tenant.as_deref(),
            Some("partner.onmicrosoft.com")
        );
        assert_eq!(c.profiles[1].login, Login::Az);
        assert!(c.profiles[1].az_account().is_err());

        // fablic-login profiles alone don't hide the az accounts.
        let mut only_device = c.clone();
        only_device.profiles.truncate(1);
        let merged = only_device.with_az_accounts(vec!["me@b.org".into()]);
        let names: Vec<&str> = merged.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["partner", "me"]);
    }

    #[test]
    fn unnamed_profiles_are_default_and_names_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[profiles]]\nlogin = \"device\"\n").unwrap();
        let c = Config::load(Some(&path)).unwrap();
        assert_eq!(c.profiles[0].name, "default");

        std::fs::write(
            &path,
            "[[profiles]]\nlogin = \"device\"\n[[profiles]]\naccount = \"a@b\"\n",
        )
        .unwrap();
        let e = format!("{:#}", Config::load(Some(&path)).unwrap_err());
        assert!(
            e.ends_with("two profiles are named default; give one a name"),
            "{e}"
        );
    }

    #[test]
    fn signed_in_profiles_join_unless_the_file_names_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/logins.json");
        let logins = vec![
            SavedLogin {
                name: "default".into(),
                tenant: Some("partner.com".into()),
                client_id: None,
            },
            SavedLogin {
                name: "partner".into(),
                tenant: None,
                client_id: None,
            },
        ];
        save_logins(Some(&path), &logins).unwrap();
        assert_eq!(load_logins(Some(&path)), logins);
        assert!(load_logins(Some(&dir.path().join("none.json"))).is_empty());

        let file: Config = toml::from_str(
            "[[profiles]]\nname = \"partner\"\nlogin = \"device\"\nreadonly = true\n",
        )
        .unwrap();
        let c = file.with_logins(&logins);
        let names: Vec<&str> = c.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["partner", "default"]);
        assert!(c.profiles[0].readonly, "the file's settings win");
        assert_eq!(c.profiles[1].tenant.as_deref(), Some("partner.com"));
    }

    #[test]
    fn missing_file_is_empty() {
        let c = Config::load(Some(Path::new("/nonexistent/fablic.toml"))).unwrap();
        assert!(c.profiles.is_empty());
    }
}
