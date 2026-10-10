//! Carrying out the app's effects. Shared by the event loop, `--snapshot`
//! and tests, so all three run the same code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::app::{DetailRequest, Effect, Level, Msg};
use crate::fabric::rows;
use crate::journal;
use crate::source::{Part, SharedSource};

pub type Sources = HashMap<String, SharedSource>;

/// Side effects a context allows. Tests and snapshots turn them off.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub open_urls: bool,
    pub journal_dir: Option<PathBuf>,
    /// Where `D` saves files; downloads are refused without one.
    pub download_dir: Option<PathBuf>,
    /// The config file profile renames are saved to; not saved without one.
    pub config_path: Option<PathBuf>,
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`... if that exists already.
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Carry out one effect. Returns the message to feed back into the app, if any.
pub async fn perform(effect: Effect, sources: &Sources, ctx: &Ctx) -> Option<Msg> {
    let source = |name: &str| {
        sources
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no source for profile {name}"))
    };
    let err = |e: anyhow::Error| format!("{e:#}");
    match effect {
        Effect::Fetch {
            profile,
            generation,
            query,
            force,
        } => {
            let result = match source(&profile) {
                Ok(s) => s.fetch(query, force).await.map_err(err),
                Err(e) => Err(e),
            };
            Some(Msg::Rows { generation, result })
        }
        Effect::Detail {
            profile,
            title,
            request,
        } => {
            let result = match source(&profile) {
                Ok(s) => match request {
                    DetailRequest::Definition { workspace, item } => {
                        s.definition(workspace, item).await.map_err(err)
                    }
                    DetailRequest::Preview {
                        workspace,
                        item,
                        path,
                        total,
                    } => s
                        .preview(workspace, item, path, total)
                        .await
                        .map(|p| vec![p])
                        .map_err(err),
                    DetailRequest::Schema {
                        workspace,
                        item,
                        path,
                    } => s
                        .schema(workspace, item, path)
                        .await
                        .map(|p: Part| vec![p])
                        .map_err(err),
                },
                Err(e) => Err(e),
            };
            Some(Msg::Detail { title, result })
        }
        Effect::Download {
            profile,
            workspace,
            item,
            path,
        } => {
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let Some(dir) = &ctx.download_dir else {
                return Some(Msg::Status(Level::Error, "downloads are off here".into()));
            };
            let dest = unique_dest(dir, &name);
            let msg = match source(&profile) {
                Ok(s) => match s.download(workspace, item, path, dest.clone()).await {
                    Ok(n) => (
                        Level::Info,
                        format!("saved {name} ({}) to {}", rows::size(n), dest.display()),
                    ),
                    Err(e) => {
                        // Don't leave a partial file behind.
                        let _ = std::fs::remove_file(&dest);
                        (Level::Error, format!("downloading {name}: {e:#}"))
                    }
                },
                Err(e) => (Level::Error, e),
            };
            Some(Msg::Status(msg.0, msg.1))
        }
        Effect::Act { profile, action } => {
            let result = match source(&profile) {
                Ok(s) => s.act(action.clone()).await.map_err(err),
                Err(e) => Err(e),
            };
            journal::append(ctx.journal_dir.as_deref(), &profile, &action, &result);
            Some(Msg::Acted { action, result })
        }
        Effect::Check { profile, fresh } => {
            let result = match source(&profile) {
                Ok(s) => {
                    if fresh {
                        s.forget();
                    }
                    s.warm().await.map_err(err)
                }
                Err(e) => Err(e),
            };
            Some(Msg::Auth { profile, result })
        }
        // The event loop signs in itself, since az needs the terminal;
        // anywhere else (tests, snapshots) there is no terminal to lend.
        Effect::Login { profile } => Some(Msg::LoggedIn {
            profile,
            result: Err("signing in needs the interactive terminal".into()),
        }),
        Effect::RenameProfile { profile, name } => Some(match &ctx.config_path {
            Some(path) => match crate::config::rename(path, &profile, &name) {
                Ok(()) => Msg::Status(
                    Level::Info,
                    format!("renamed to {name} (saved in {})", path.display()),
                ),
                Err(e) => Msg::Status(Level::Error, format!("saving the name: {e:#}")),
            },
            None => Msg::Status(Level::Info, format!("renamed to {name} (not saved)")),
        }),
        Effect::Open(url) => {
            if ctx.open_urls
                && let Err(e) = open::that_detached(&url)
            {
                tracing::warn!("opening {url}: {e}");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn profile_renames_are_saved_only_with_a_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let rename = || Effect::RenameProfile {
            profile: "00000000".into(),
            name: "etl-sp".into(),
        };
        let none = perform(rename(), &Sources::new(), &Ctx::default()).await;
        assert!(matches!(none, Some(Msg::Status(_, t)) if t.ends_with("(not saved)")));
        let ctx = Ctx {
            config_path: Some(path.clone()),
            ..Ctx::default()
        };
        perform(rename(), &Sources::new(), &ctx).await;
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("[names]") && text.contains("00000000 = \"etl-sp\""),
            "{text}"
        );
    }

    #[test]
    fn unique_dest_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_dest(dir.path(), "a.csv"), dir.path().join("a.csv"));
        std::fs::write(dir.path().join("a.csv"), "x").unwrap();
        std::fs::write(dir.path().join("a (1).csv"), "x").unwrap();
        assert_eq!(
            unique_dest(dir.path(), "a.csv"),
            dir.path().join("a (2).csv")
        );
        std::fs::write(dir.path().join("Makefile"), "x").unwrap();
        assert_eq!(
            unique_dest(dir.path(), "Makefile"),
            dir.path().join("Makefile (1)")
        );
    }
}
