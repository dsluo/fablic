mod app;
mod auth;
mod config;
mod fabric;
mod filter;
mod resources;
mod source;
mod theme;
mod ui;

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::{DefaultTerminal, Terminal};
use tokio::sync::mpsc;

use app::{App, Effect, Msg, ProfileInfo};
use auth::AzCli;
use config::Config;
use fabric::FabricSource;
use fabric::fake::FakeSource;
use source::SharedSource;
use theme::Theme;

/// A k9s-style terminal UI for Microsoft Fabric.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Profile to start on (name or az account). Defaults to `default_profile`.
    #[arg(short, long)]
    profile: Option<String>,

    /// Block every write, whatever the profile says.
    #[arg(long)]
    readonly: bool,

    /// Use built-in fake data instead of Fabric.
    #[arg(long)]
    fake: bool,

    /// Render one frame to stdout after replaying these steps, then exit.
    /// Steps are space separated: `:cmd`, `/filter`, or a key (enter esc j k g G d y F).
    /// Quote a command with arguments: `':nb all'`.
    #[arg(long, value_name = "STEPS", allow_hyphen_values = true)]
    snapshot: Option<String>,
}

type Sources = Arc<HashMap<String, SharedSource>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging();

    let (profiles, active, sources) = if args.fake {
        let p = ProfileInfo {
            name: "fake".into(),
            account: "fake data".into(),
            readonly: true,
            color: None,
        };
        let src: SharedSource = Arc::new(FakeSource);
        (vec![p], 0, HashMap::from([("fake".to_owned(), src)]))
    } else {
        live_profiles(&args).await?
    };
    let sources: Sources = Arc::new(sources);
    let app = App::new(profiles, active);

    if let Some(steps) = args.snapshot {
        return snapshot(app, sources, &steps).await;
    }

    // Get every profile's token in the background so switching is instant.
    for (name, src) in sources.iter() {
        let (name, fut) = (name.clone(), src.warm());
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                tracing::warn!(profile = %name, "token prefetch failed: {e:#}");
            }
        });
    }

    let theme = Theme::detect();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app, sources, theme).await;
    ratatui::restore();
    result
}

async fn live_profiles(
    args: &Args,
) -> Result<(Vec<ProfileInfo>, usize, HashMap<String, SharedSource>)> {
    let mut config = Config::load(Config::path().as_deref())?;
    if config.profiles.is_empty() {
        let accounts = AzCli::accounts()
            .await
            .context("listing az accounts for default profiles")?;
        config = config.with_az_accounts(accounts);
    }
    let active = config.initial(args.profile.as_deref())?;
    let mut sources = HashMap::new();
    let mut profiles = Vec::new();
    for p in config.profiles {
        let tokens = Arc::new(AzCli::new(&p.account));
        let src: SharedSource = Arc::new(FabricSource::new(tokens));
        sources.insert(p.name.clone(), src);
        profiles.push(ProfileInfo {
            color: p.color.as_deref().and_then(|c| Color::from_str(c).ok()),
            readonly: p.readonly || args.readonly,
            name: p.name,
            account: p.account,
        });
    }
    Ok((profiles, active, sources))
}

fn init_logging() {
    let Some(dir) = config::state_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = tracing_appender::rolling::never(dir, "fablic.log");
    let _ = tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FABLIC_LOG")
                .unwrap_or_else(|_| "fablic=info".into()),
        )
        .try_init();
}

/// Carry out one effect. Returns the message to feed back into the app, if any.
async fn perform(effect: Effect, sources: Sources, open_urls: bool) -> Option<Msg> {
    let source = |name: &str| {
        sources
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no source for profile {name}"))
    };
    match effect {
        Effect::Fetch {
            profile,
            generation,
            query,
            force,
        } => {
            let result = match source(&profile) {
                Ok(s) => s.fetch(query, force).await.map_err(|e| format!("{e:#}")),
                Err(e) => Err(e),
            };
            Some(Msg::Rows { generation, result })
        }
        Effect::Definition {
            profile,
            workspace,
            item,
            title,
        } => {
            let result = match source(&profile) {
                Ok(s) => s
                    .definition(workspace, item)
                    .await
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(e),
            };
            Some(Msg::Detail { title, result })
        }
        Effect::Open(url) => {
            if open_urls && let Err(e) = open::that_detached(&url) {
                tracing::warn!("opening {url}: {e}");
            }
            None
        }
    }
}

async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    sources: Sources,
    theme: Theme,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        for effect in app.take_effects() {
            let (tx, sources) = (tx.clone(), sources.clone());
            tokio::spawn(async move {
                if let Some(msg) = perform(effect, sources, true).await {
                    let _ = tx.send(msg);
                }
            });
        }
        terminal.draw(|f| ui::draw(f, &mut app, &theme))?;
        if app.quit {
            return Ok(());
        }
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => app.on_key(k),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            Some(msg) = rx.recv() => app.on_msg(msg),
            _ = tick.tick() => app.on_tick(),
        }
    }
}

/// Split snapshot steps on spaces, keeping single-quoted steps together.
fn split_steps(steps: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in steps.chars() {
        match c {
            '\'' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Headless mode: replay steps, wait for every fetch, print one rendered frame.
async fn snapshot(mut app: App, sources: Sources, steps: &str) -> Result<()> {
    async fn settle(app: &mut App, sources: &Sources) {
        loop {
            let effects = app.take_effects();
            if effects.is_empty() {
                return;
            }
            for e in effects {
                if let Some(msg) = perform(e, sources.clone(), false).await {
                    app.on_msg(msg);
                }
            }
        }
    }
    let press = |app: &mut App, code| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));

    settle(&mut app, &sources).await;
    for step in split_steps(steps) {
        match step.as_str() {
            "enter" => press(&mut app, KeyCode::Enter),
            "esc" => press(&mut app, KeyCode::Esc),
            s if s.starts_with(':') || s.starts_with('/') => {
                for c in s.chars() {
                    press(&mut app, KeyCode::Char(c));
                }
                press(&mut app, KeyCode::Enter);
            }
            s if s.chars().count() == 1 => {
                press(&mut app, KeyCode::Char(s.chars().next().unwrap_or(' ')))
            }
            s => anyhow::bail!("unknown snapshot step: {s}"),
        }
        settle(&mut app, &sources).await;
    }
    let mut term = Terminal::new(TestBackend::new(120, 30))?;
    term.draw(|f| ui::draw(f, &mut app, &Theme::MOCHA))?;
    let buf = term.backend().buffer();
    for y in 0..buf.area.height {
        let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        println!("{}", line.trim_end());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn steps_keep_quoted_commands_together() {
        assert_eq!(
            super::split_steps("j ':nb all' enter"),
            ["j", ":nb all", "enter"]
        );
    }
}
