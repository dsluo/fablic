mod app;
mod auth;
mod config;
mod effects;
mod fabric;
mod filter;
mod journal;
mod resources;
mod source;
mod theme;
mod ui;

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::{DefaultTerminal, Terminal};
use tokio::sync::mpsc;

use app::{App, ProfileInfo};
use auth::{AzCli, EntraLogin, Keychain, TokenProvider, entra};
use config::{Config, Login, Profile, SavedLogin};
use effects::Ctx;
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

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Sign in with fablic's own login (no az needed): opens your browser to
    /// Microsoft's sign-in page and keeps the sign-in in the OS keychain.
    Login {
        /// Profile name (or account). Needs no config: a new name becomes a
        /// fablic-login profile.
        #[arg(default_value = config::DEFAULT_PROFILE)]
        profile: String,
        /// Entra tenant (id or domain). Defaults to any work account.
        #[arg(long)]
        tenant: Option<String>,
        /// Public client app registration to sign in with.
        #[arg(long)]
        client_id: Option<String>,
        /// Show a code to enter at microsoft.com/devicelogin instead of
        /// opening a browser (for SSH sessions; some tenants block it).
        #[arg(long)]
        device_code: bool,
    },
    /// Forget a fablic-login profile's stored sign-in.
    Logout {
        /// Profile name (or account).
        #[arg(default_value = config::DEFAULT_PROFILE)]
        profile: String,
    },
}

type Sources = Arc<effects::Sources>;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging();
    match &args.command {
        Some(Cmd::Login {
            profile,
            tenant,
            client_id,
            device_code,
        }) => return login(profile, tenant.clone(), client_id.clone(), *device_code).await,
        Some(Cmd::Logout { profile }) => return logout(profile),
        None => {}
    }

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
    let logins = config::load_logins(config::logins_path().as_deref());
    let mut config = Config::load(Config::path().as_deref())?.with_logins(&logins);
    if config.wants_az_accounts() {
        match AzCli::accounts().await {
            Ok(accounts) => config = config.with_az_accounts(accounts),
            // Device profiles work without az.
            Err(e) if !config.profiles.is_empty() => {
                tracing::info!("no az accounts added: {e:#}")
            }
            Err(e) => return Err(e.context("listing az accounts for default profiles")),
        }
    }
    let active = config.initial(args.profile.as_deref())?;
    let mut sources = HashMap::new();
    let mut profiles = Vec::new();
    for p in config.profiles {
        let tokens = token_provider(&p)?;
        let account = tokens.account().to_owned();
        let src: SharedSource = Arc::new(FabricSource::new(tokens));
        sources.insert(p.name.clone(), src);
        profiles.push(ProfileInfo {
            color: p.color.as_deref().and_then(|c| Color::from_str(c).ok()),
            readonly: p.readonly || args.readonly,
            name: p.name,
            account,
        });
    }
    Ok((profiles, active, sources))
}

fn token_provider(p: &Profile) -> Result<Arc<dyn TokenProvider>> {
    Ok(match p.login {
        Login::Az => Arc::new(AzCli::new(p.az_account()?)),
        Login::Entra => Arc::new(entra_login(p)),
    })
}

fn entra_login(p: &Profile) -> EntraLogin {
    let settings = entra::Settings {
        tenant: p
            .tenant
            .clone()
            .unwrap_or_else(|| entra::DEFAULT_TENANT.into()),
        client_id: p
            .client_id
            .clone()
            .unwrap_or_else(|| entra::DEFAULT_CLIENT_ID.into()),
        authority: entra::AUTHORITY.into(),
    };
    EntraLogin::new(&p.name, p.account.as_deref(), settings, Arc::new(Keychain))
}

/// The profile `login`/`logout` act on: one from the config file or an
/// earlier `fablic login`, or else a new fablic-login profile of that name.
/// Returns it and whether the config file defines it.
fn login_profile(wanted: &str) -> Result<(Profile, bool)> {
    let file = Config::load(Config::path().as_deref())?;
    let logins = config::load_logins(config::logins_path().as_deref());
    let in_file = file
        .initial(Some(wanted))
        .ok()
        .map(|i| file.profiles[i].clone());
    let p = match in_file.clone() {
        Some(p) => p,
        None => logins
            .iter()
            .find(|l| l.name == wanted)
            .map(SavedLogin::profile)
            .unwrap_or_else(|| {
                SavedLogin {
                    name: wanted.to_owned(),
                    tenant: None,
                    client_id: None,
                }
                .profile()
            }),
    };
    if p.login != Login::Entra {
        anyhow::bail!(
            "profile {} signs in through az; use `az login` for it (or set login = \"entra\")",
            p.name
        );
    }
    Ok((p, in_file.is_some()))
}

async fn login(
    wanted: &str,
    tenant: Option<String>,
    client_id: Option<String>,
    device_code: bool,
) -> Result<()> {
    let (mut p, in_file) = login_profile(wanted)?;
    p.tenant = tenant.or(p.tenant);
    p.client_id = client_id.or(p.client_id);
    let login = entra_login(&p);
    let account = if device_code {
        let code = login.start_device_code().await?;
        println!("{}", code.message);
        println!("\n    code: {}\n", code.user_code);
        if open::that_detached(&code.verification_uri).is_ok() {
            println!("(opened {} in your browser)", code.verification_uri);
        }
        println!("waiting for you to finish signing in…");
        login.finish_device_code(&code).await?
    } else {
        let b = login.start_browser(p.account.as_deref()).await?;
        if open::that_detached(&b.url).is_ok() {
            println!("Sign in in the browser window that just opened.");
            println!("If none did, open this link:\n\n{}\n", b.url);
        } else {
            println!("Open this link to sign in:\n\n{}\n", b.url);
        }
        println!("waiting for you to finish signing in…");
        login.finish_browser(b).await?
    };
    if !in_file {
        // Remember the profile so the TUI offers it without any config.
        let path = config::logins_path();
        let mut logins = config::load_logins(path.as_deref());
        logins.retain(|l| l.name != p.name);
        logins.push(SavedLogin {
            name: p.name.clone(),
            tenant: p.tenant.clone(),
            client_id: p.client_id.clone(),
        });
        config::save_logins(path.as_deref(), &logins)?;
    }
    println!("profile {} is signed in as {account}", p.name);
    Ok(())
}

fn logout(wanted: &str) -> Result<()> {
    let (p, _) = login_profile(wanted)?;
    entra_login(&p).logout()?;
    let path = config::logins_path();
    let mut logins = config::load_logins(path.as_deref());
    if logins.iter().any(|l| l.name == p.name) {
        logins.retain(|l| l.name != p.name);
        config::save_logins(path.as_deref(), &logins)?;
    }
    println!("profile {} is signed out", p.name);
    Ok(())
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

async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    sources: Sources,
    theme: Theme,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let ctx = Arc::new(Ctx {
        open_urls: true,
        journal_dir: config::state_dir(),
        download_dir: std::env::current_dir().ok(),
    });
    loop {
        for effect in app.take_effects() {
            let (tx, sources, ctx) = (tx.clone(), sources.clone(), ctx.clone());
            tokio::spawn(async move {
                if let Some(msg) = effects::perform(effect, &sources, &ctx).await {
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
    // No browser, no journal, no downloads: a snapshot only looks.
    async fn settle(app: &mut App, sources: &Sources) {
        let ctx = Ctx::default();
        loop {
            let effects = app.take_effects();
            if effects.is_empty() {
                return;
            }
            for e in effects {
                if let Some(msg) = effects::perform(e, sources, &ctx).await {
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
