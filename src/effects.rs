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
