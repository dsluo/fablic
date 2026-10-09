mod app;
mod filter;
mod resources;
mod source;
mod theme;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::TestBackend;
use ratatui::{DefaultTerminal, Terminal};
use tokio::sync::mpsc;

use app::{App, Effect, Msg, ProfileInfo};
use source::{FakeSource, SharedSource};
use theme::Theme;

/// A k9s-style terminal UI for Microsoft Fabric.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Use built-in fake data instead of Fabric.
    #[arg(long)]
    fake: bool,

    /// Render one frame to stdout after replaying these steps, then exit.
    /// Steps are space separated: `:cmd`, `/filter`, or a key (enter esc j k g G d).
    #[arg(long, value_name = "STEPS", allow_hyphen_values = true)]
    snapshot: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging();

    let source: SharedSource = Arc::new(FakeSource);
    let profile = ProfileInfo {
        name: "fake".into(),
        account: "fake data".into(),
        readonly: true,
    };
    let app = App::new(profile);

    if let Some(steps) = args.snapshot {
        return snapshot(app, source, &steps).await;
    }

    let theme = Theme::detect();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app, source, theme).await;
    ratatui::restore();
    result
}

fn init_logging() {
    let Some(dirs) = directories::ProjectDirs::from("", "", "fablic") else {
        return;
    };
    let dir = dirs.state_dir().unwrap_or(dirs.cache_dir());
    if std::fs::create_dir_all(dir).is_err() {
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

fn spawn_effects(app: &mut App, source: &SharedSource, tx: &mpsc::UnboundedSender<Msg>) {
    for effect in app.take_effects() {
        match effect {
            Effect::Fetch { generation, query } => {
                let fut = source.fetch(query);
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = fut.await.map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::Rows { generation, result });
                });
            }
        }
    }
}

async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    source: SharedSource,
    theme: Theme,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        spawn_effects(&mut app, &source, &tx);
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

/// Headless mode: replay steps, wait for every fetch, print one rendered frame.
async fn snapshot(mut app: App, source: SharedSource, steps: &str) -> Result<()> {
    async fn settle(app: &mut App, source: &SharedSource) {
        loop {
            let effects = app.take_effects();
            if effects.is_empty() {
                return;
            }
            for e in effects {
                let Effect::Fetch { generation, query } = e;
                let result = source.fetch(query).await.map_err(|e| format!("{e:#}"));
                app.on_msg(Msg::Rows { generation, result });
            }
        }
    }
    let press = |app: &mut App, code| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));

    settle(&mut app, &source).await;
    for step in steps.split_whitespace() {
        match step {
            "enter" => press(&mut app, KeyCode::Enter),
            "esc" => press(&mut app, KeyCode::Esc),
            s if s.starts_with(':') || s.starts_with('/') => {
                for c in s.chars() {
                    press(&mut app, KeyCode::Char(c));
                }
                press(&mut app, KeyCode::Enter);
            }
            s if s.chars().count() == 1 => {
                press(&mut app, KeyCode::Char(s.chars().next().unwrap()))
            }
            s => anyhow::bail!("unknown snapshot step: {s}"),
        }
        settle(&mut app, &source).await;
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
