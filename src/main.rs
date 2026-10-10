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

use app::{App, Effect, Msg, ProfileInfo};
use auth::AzCli;
use config::Config;
use effects::Ctx;
use fabric::FabricSource;
use fabric::fake::{FakeAuth, FakeSource};
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
    /// Sign in with `az login`, then make az's previously active account
    /// active again so other tools aren't switched over.
    Login {
        /// A profile (or account) to sign in as again; its tenant is used.
        profile: Option<String>,
        /// Entra tenant (id or domain) to sign in to.
        #[arg(long)]
        tenant: Option<String>,
    },
}

type Sources = Arc<effects::Sources>;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging();
    if let Some(Cmd::Login { profile, tenant }) = &args.command {
        return login(profile.as_deref(), tenant.as_deref()).await;
    }

    let (profiles, active, sources) = if args.fake {
        fake_profiles(args.readonly)
    } else {
        live_profiles(&args).await?
    };
    let sources: Sources = Arc::new(sources);
    let app = App::new(profiles, active);

    if let Some(steps) = args.snapshot {
        return snapshot(app, sources, &steps).await;
    }

    // The app checks every profile's sign-in on start, which also gets
    // their tokens ahead of time so switching is instant.
    let theme = Theme::detect();
    let config_path = if args.fake { None } else { Config::path() };
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app, sources, theme, config_path).await;
    ratatui::restore();
    result
}

async fn live_profiles(
    args: &Args,
) -> Result<(Vec<ProfileInfo>, usize, HashMap<String, SharedSource>)> {
    let (config, az, env) = profiles_config().await?;
    let active = config.initial(args.profile.as_deref())?;
    let mut sources = HashMap::new();
    let mut profiles = Vec::new();
    for p in config.profiles {
        let from_env = env
            .as_ref()
            .filter(|e| e.client_id.eq_ignore_ascii_case(&p.account));
        let tokens = match (from_env, env_az_dir()) {
            (Some(e), Some(dir)) => Arc::new(AzCli::from_env_login(e.clone(), dir)),
            _ => Arc::new(AzCli::new(&p.account)),
        };
        let src: SharedSource = Arc::new(FabricSource::new(tokens));
        sources.insert(p.account.clone(), src);
        let known = az.iter().find(|a| a.name.eq_ignore_ascii_case(&p.account));
        profiles.push(ProfileInfo {
            color: p.color.as_deref().and_then(|c| Color::from_str(c).ok()),
            auth: Default::default(),
            readonly: p.readonly || args.readonly,
            service_principal: from_env.is_some() || known.is_some_and(|a| a.service_principal),
            tenant: from_env
                .map(|e| e.tenant.clone())
                .or_else(|| known.map(|a| a.tenant.clone()))
                .unwrap_or_default(),
            from_env: from_env.is_some(),
            name: p.name,
            account: p.account,
        });
    }
    Ok((profiles, active, sources))
}

/// `--fake`: a made-up tenant behind one profile of each kind, so every
/// part of the picker shows. Writes work (on the fake data) except in the
/// read-only one; `contractor` needs signing in again (`ctrl-p`, `l`).
fn fake_profiles(readonly: bool) -> (Vec<ProfileInfo>, usize, HashMap<String, SharedSource>) {
    const SP: &str = "00000000-1111-2222-3333-444444444444";
    const ENV_SP: &str = "99999999-8888-7777-6666-555555555555";
    let tenant = "contoso.example".to_owned();
    let p = |name: &str, account: &str| ProfileInfo {
        name: name.into(),
        account: account.into(),
        readonly,
        tenant: tenant.clone(),
        ..Default::default()
    };
    let profiles = vec![
        ProfileInfo {
            color: Some(Color::Rgb(0x89, 0xb4, 0xfa)),
            ..p("dev", "dev@contoso.example")
        },
        ProfileInfo {
            readonly: true,
            color: Some(Color::Rgb(0xf3, 0x8b, 0xa8)),
            ..p("prod", "admin@contoso.example")
        },
        ProfileInfo {
            service_principal: true,
            ..p("sp-00000000", SP)
        },
        ProfileInfo {
            service_principal: true,
            from_env: true,
            ..p("env", ENV_SP)
        },
        p("contractor", "guest@fabrikam.example"),
        p("partner", "ops@partner.example"),
    ];
    let live = || FakeSource::default().live();
    let sources: [(&str, FakeSource); 6] = [
        ("dev@contoso.example", live().admin()),
        ("admin@contoso.example", live()),
        (SP, live()),
        (ENV_SP, live()),
        ("guest@fabrikam.example", live().auth(FakeAuth::Expired)),
        (
            "ops@partner.example",
            live().auth(FakeAuth::Broken(
                "AADSTS50020: User account 'ops@partner.example' from identity provider \
                 'live.com' does not exist in tenant 'Contoso' and cannot access the \
                 application in that tenant."
                    .into(),
            )),
        ),
    ];
    let sources = sources
        .into_iter()
        .map(|(a, s)| (a.to_owned(), Arc::new(s) as SharedSource))
        .collect();
    (profiles, 0, sources)
}

/// Where the environment's service principal keeps its own az logins.
fn env_az_dir() -> Option<std::path::PathBuf> {
    config::state_dir().map(|d| d.join("az").join("env"))
}

/// The profiles fablic offers: the config's (or one per az identity), plus
/// one for a service principal named by `AZURE_CLIENT_ID` & co. Also returns
/// what az knows about each identity (empty if az can't be asked).
async fn profiles_config() -> Result<(Config, Vec<auth::AzAccount>, Option<auth::EnvLogin>)> {
    let mut config = Config::load(Config::path().as_deref())?;
    let env = auth::EnvLogin::from_env();
    let az = AzCli::accounts().await;
    let az = if config.profiles.is_empty() {
        match az {
            Ok(az) => {
                config = config.with_az_accounts(&az);
                az
            }
            // The environment's service principal works without az logins.
            Err(_) if env.is_some() => Vec::new(),
            Err(e) => return Err(e.context("listing az accounts for default profiles")),
        }
    } else {
        az.unwrap_or_default()
    };
    if let Some(e) = &env {
        config = config.with_env_profile(&e.client_id);
    }
    Ok((config, az, env))
}

/// `fablic login [profile]`: `az login` for that profile's account.
async fn login(profile: Option<&str>, tenant: Option<&str>) -> Result<()> {
    let account = match profile {
        None => None,
        Some(wanted) => {
            let (config, _, env) = profiles_config().await?;
            match config.initial(Some(wanted)) {
                Ok(i)
                    if env.as_ref().is_some_and(|e| {
                        e.client_id
                            .eq_ignore_ascii_case(&config.profiles[i].account)
                    }) =>
                {
                    println!(
                        "{} signs in by itself from AZURE_CLIENT_ID and friends; nothing to do",
                        config.profiles[i].name
                    );
                    return Ok(());
                }
                Ok(i) => Some(config.profiles[i].account.clone()),
                // Not a profile yet: an account az hasn't seen.
                Err(_) if wanted.contains('@') => Some(wanted.to_owned()),
                Err(e) => return Err(e),
            }
        }
    };
    auth::login(std::path::Path::new("az"), account.as_deref(), tenant).await?;
    let names: Vec<String> = AzCli::accounts()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.name)
        .collect();
    println!("az is signed in as: {}", names.join(", "));
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
    config_path: Option<std::path::PathBuf>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Dropped while az login has the terminal, so it doesn't eat keys meant for az.
    let mut events = Some(EventStream::new());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let ctx = Arc::new(Ctx {
        open_urls: true,
        journal_dir: config::state_dir(),
        download_dir: std::env::current_dir().ok(),
        config_path,
    });
    loop {
        for effect in app.take_effects() {
            if let Effect::Login { profile } = effect {
                // The fake source signs itself in; no az, no terminal.
                if let Some(signing_in) = sources.get(&profile).and_then(|s| s.sign_in()) {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = signing_in.await.map_err(|e| format!("{e:#}"));
                        let _ = tx.send(Msg::LoggedIn { profile, result });
                    });
                    continue;
                }
                drop(events.take());
                suspend()?;
                println!("Signing {profile} in again with az…");
                let result = auth::login(std::path::Path::new("az"), Some(&profile), None).await;
                // Reuse the terminal, and repaint with a blank frame rather
                // than `terminal.clear()`: both a new terminal and `clear`
                // ask the terminal for the cursor position, and that answer
                // can get lost after az (fablic then gave up and exited).
                resume()?;
                terminal.draw(|f| f.render_widget(ratatui::widgets::Clear, f.area()))?;
                events = Some(EventStream::new());
                if let Some(src) = sources.get(&profile) {
                    src.forget();
                }
                app.on_msg(Msg::LoggedIn {
                    profile,
                    result: result.map_err(|e| format!("{e:#}")),
                });
                continue;
            }
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
        let Some(stream) = events.as_mut() else {
            continue;
        };
        tokio::select! {
            ev = stream.next() => match ev {
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

/// Hand the terminal back to the shell (for `az login`).
fn suspend() -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    Ok(())
}

/// Take the terminal back after `suspend`.
fn resume() -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
    )?;
    Ok(())
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
