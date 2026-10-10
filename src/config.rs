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
//! login = "device"                           # fablic's own sign-in: `fablic login partner`
//! tenant = "partner.onmicrosoft.com"         # optional; any work account by default
//! client_id = "…"                            # optional; the Azure CLI's public client by default
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

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
    pub name: String,
    /// How the profile gets tokens.
    #[serde(default)]
    pub login: Login,
    /// The account (UPN) this profile signs in as. Required for az; for
    /// device sign-in it's only a label until the first `fablic login`.
    pub account: Option<String>,
    #[serde(default)]
    pub readonly: bool,
    pub color: Option<String>,
    /// Device sign-in only: the Entra tenant (id or domain).
    pub tenant: Option<String>,
    /// Device sign-in only: a public client app registration to sign in with.
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Login {
    /// Tokens from the az CLI's login cache.
    #[default]
    Az,
    /// fablic's own device-code sign-in, kept in the OS keychain.
    Device,
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
            Ok(text) => toml::from_str(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Whether the file says which az accounts to use. If not, every cached
    /// az account becomes a profile (after any device profiles).
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
    fn device_profiles_need_no_account() {
        let c: Config = toml::from_str(
            r#"
            [[profiles]]
            name = "partner"
            login = "device"
            tenant = "partner.onmicrosoft.com"
            [[profiles]]
            name = "broken"
            "#,
        )
        .unwrap();
        assert_eq!(c.profiles[0].login, Login::Device);
        assert_eq!(
            c.profiles[0].tenant.as_deref(),
            Some("partner.onmicrosoft.com")
        );
        assert_eq!(c.profiles[1].login, Login::Az);
        assert!(c.profiles[1].az_account().is_err());

        // Device profiles alone don't hide the az accounts.
        let mut only_device = c.clone();
        only_device.profiles.truncate(1);
        let merged = only_device.with_az_accounts(vec!["me@b.org".into()]);
        let names: Vec<&str> = merged.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["partner", "me"]);
    }

    #[test]
    fn missing_file_is_empty() {
        let c = Config::load(Some(Path::new("/nonexistent/fablic.toml"))).unwrap();
        assert!(c.profiles.is_empty());
    }
}
