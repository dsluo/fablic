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
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::auth::AzAccount;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Names for profiles made from az accounts: account -> name. Renaming
    /// such a profile in the picker writes here.
    #[serde(default)]
    pub names: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// `default` when left out.
    #[serde(default = "default_name")]
    pub name: String,
    /// The az CLI account (UPN) this profile signs in as.
    pub account: String,
    #[serde(default)]
    pub readonly: bool,
    pub color: Option<String>,
}

/// The name of a profile that doesn't give one.
pub const DEFAULT_PROFILE: &str = "default";

fn default_name() -> String {
    DEFAULT_PROFILE.into()
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
            if self.profiles[..i]
                .iter()
                .any(|q| q.account.eq_ignore_ascii_case(&p.account))
            {
                anyhow::bail!("two profiles use the account {}", p.account);
            }
        }
        Ok(self)
    }

    /// Profiles from the file, or one per cached az account when the file has none.
    pub fn with_az_accounts(mut self, accounts: &[AzAccount]) -> Config {
        if self.profiles.is_empty() {
            self.profiles = accounts
                .iter()
                .map(|a| Profile {
                    name: self
                        .names
                        .get(&a.name)
                        .cloned()
                        .unwrap_or_else(|| auto_name(a)),
                    account: a.name.clone(),
                    readonly: false,
                    color: None,
                })
                .collect();
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
                .position(|p| p.name == n || p.account.eq_ignore_ascii_case(n))
                .with_context(|| {
                    let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
                    format!("no profile named {n} (have: {})", names.join(", "))
                }),
            None if self.profiles.is_empty() => anyhow::bail!(
                "no profiles: sign in with `fablic login` (or `az login`), or add [[profiles]] to {}",
                Config::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            None => Ok(0),
        }
    }
}

/// A profile name for an az identity: the part of a UPN before the `@`, or
/// `sp-` and the first block of a service principal's client id.
fn auto_name(a: &AzAccount) -> String {
    if a.service_principal {
        format!("sp-{}", a.name.split('-').next().unwrap_or(&a.name))
    } else {
        a.name.split('@').next().unwrap_or(&a.name).to_owned()
    }
}

/// Rename the profile for `account` in the config file at `path`, keeping
/// the rest of the file (comments, order, formatting) as it is. A profile
/// from `[[profiles]]` gets its `name` changed; one made from an az account
/// gets an entry in `[names]`.
pub fn rename(path: &Path, account: &str, name: &str) -> Result<()> {
    use toml_edit::{DocumentMut, Item, Table, value};
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("in {}", path.display()))?;
    let in_profiles = doc
        .get_mut("profiles")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|list| {
            list.iter_mut().find(|t| {
                t.get("account")
                    .and_then(Item::as_str)
                    .is_some_and(|a| a.eq_ignore_ascii_case(account))
            })
        });
    match in_profiles {
        Some(t) => {
            t["name"] = value(name);
        }
        None => {
            if !doc.contains_table("names") {
                doc["names"] = Item::Table(Table::new());
            }
            doc["names"][account] = value(name);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
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

    fn az(name: &str) -> AzAccount {
        AzAccount {
            name: name.into(),
            tenant: "t".into(),
            service_principal: !name.contains('@'),
        }
    }

    #[test]
    fn service_principals_get_short_names() {
        let c = Config::default().with_az_accounts(&[az("00000000-1111-2222")]);
        assert_eq!(c.profiles[0].name, "sp-00000000");
    }

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
        let c = Config::default().with_az_accounts(&[az("admin1@a.net"), az("me@b.org")]);
        assert_eq!(c.profiles[0].name, "admin1");
        assert_eq!(c.profiles[1].account, "me@b.org");
        assert_eq!(c.initial(None).unwrap(), 0);
        assert!(Config::default().initial(None).is_err());
    }

    #[test]
    fn unnamed_profiles_are_default_and_names_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[profiles]]\naccount = \"a@b\"\n").unwrap();
        let c = Config::load(Some(&path)).unwrap();
        assert_eq!(c.profiles[0].name, "default");

        std::fs::write(
            &path,
            "[[profiles]]\naccount = \"a@b\"\n[[profiles]]\naccount = \"c@d\"\n",
        )
        .unwrap();
        let e = format!("{:#}", Config::load(Some(&path)).unwrap_err());
        assert!(
            e.ends_with("two profiles are named default; give one a name"),
            "{e}"
        );
    }

    #[test]
    fn renames_keep_the_file_and_name_az_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fablic/config.toml");
        // No file yet: an az-made profile gets a [names] entry.
        rename(&path, "00000000-1111", "etl-sp").unwrap();
        let c = Config::load(Some(&path))
            .unwrap()
            .with_az_accounts(&[az("00000000-1111"), az("me@b.org")]);
        let names: Vec<&str> = c.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["etl-sp", "me"]);

        // A configured profile is renamed in place, comments and all.
        std::fs::write(
            &path,
            "# mine\n[[profiles]]\nname = \"old\" # keep\naccount = \"A@x\"\n",
        )
        .unwrap();
        rename(&path, "a@x", "new").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("name = \"new\""), "{text}");
        assert!(!text.contains("[names]"), "{text}");
    }

    #[test]
    fn missing_file_is_empty() {
        let c = Config::load(Some(Path::new("/nonexistent/fablic.toml"))).unwrap();
        assert!(c.profiles.is_empty());
    }
}
