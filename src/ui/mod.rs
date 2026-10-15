//! Rendering. `draw` lays out header / body / prompt / status and then draws
//! overlays (palette, help) on top of whatever body the mode shows.

mod detail;
mod header;
mod table;

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph};

use crate::app::{App, Auth, Level, Mode, Prompt, Purpose};
use crate::theme::Theme;

const STATUS_TTL: Duration = Duration::from_secs(5);

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let header_h = if app.compact { 1 } else { 7 };
    let [header, body, prompt, status] = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    header::draw(f, header, app, theme);
    match app.mode {
        Mode::Detail => detail::draw(f, body, app, theme),
        _ => table::draw(f, body, app, theme),
    }
    // Table borders plus the header row take three lines.
    app.page = body.height.saturating_sub(3).max(1) as usize;
    draw_prompt(f, prompt, app, theme);
    draw_status(f, status, app, theme);

    match app.mode {
        Mode::Command => draw_palette(f, body, app, theme),
        Mode::Help => draw_help(f, body, theme),
        Mode::Profiles => draw_profiles(f, body, app, theme),
        Mode::Confirm => draw_confirm(f, body, app, theme),
        Mode::Prompt => draw_input(f, body, app, theme),
        _ => {}
    }
}

fn draw_prompt(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let line = match app.mode {
        Mode::Command => {
            let ghost = app
                .suggestions
                .get(app.suggestion)
                .and_then(|s| s.word.strip_prefix(app.input.as_str()))
                .unwrap_or("");
            Line::from(vec![
                Span::styled(":", Style::new().fg(t.accent)),
                Span::styled(app.input.clone(), Style::new().fg(t.text)),
                Span::styled(ghost.to_owned(), Style::new().fg(t.faint)),
            ])
        }
        Mode::Filter => Line::from(vec![
            Span::styled("/", Style::new().fg(t.accent)),
            Span::styled(app.input.clone(), Style::new().fg(t.text)),
        ]),
        _ => breadcrumb(app, area.width as usize, t),
    };
    f.render_widget(Paragraph::new(line), area);
    if matches!(app.mode, Mode::Command | Mode::Filter) {
        let x = area.x + 1 + app.input.chars().count() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
    }
}

/// Where the view is: `Urban1 Bronze › ingest › load_all (Notebook)`. Leading segments give way to `…` when it's too wide.
fn breadcrumb(app: &App, width: usize, t: &Theme) -> Line<'static> {
    const SEP: &str = " › ";
    let mut parts = app.frame.query.scope.parts();
    if parts.is_empty() {
        parts.push("all".into());
    }
    let len = |ps: &[String]| {
        ps.iter().map(|p| p.chars().count()).sum::<usize>()
            + SEP.chars().count() * ps.len().saturating_sub(1)
    };
    let mut skip = 0;
    while skip + 1 < parts.len()
        && len(&parts[skip..]) + if skip > 0 { 1 + SEP.chars().count() } else { 0 } > width
    {
        skip += 1;
    }
    let mut spans = Vec::new();
    if skip > 0 {
        spans.push(Span::styled("…", Style::new().fg(t.faint)));
        spans.push(Span::styled(SEP, Style::new().fg(t.faint)));
    }
    let last = parts.len() - 1;
    for (i, p) in parts.into_iter().enumerate().skip(skip) {
        if i > skip {
            spans.push(Span::styled(SEP, Style::new().fg(t.faint)));
        }
        let style = if i == last {
            Style::new().fg(t.title).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.dim)
        };
        spans.push(Span::styled(p, style));
    }
    Line::from(spans)
}

fn draw_status(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let recent = app.status.as_ref().filter(|s| s.at.elapsed() < STATUS_TTL);
    let line = match (recent, &app.error) {
        (Some(s), _) => {
            let color = match s.level {
                Level::Info => t.dim,
                Level::Warn => t.warn,
                Level::Error => t.bad,
            };
            Line::styled(s.text.clone(), Style::new().fg(color))
        }
        (None, Some(e)) => Line::styled(format!("error: {e}"), Style::new().fg(t.bad)),
        (None, None) => match &app.moving {
            Some(m) => Line::styled(
                format!(
                    "moving {}: p drops it in the folder on screen, m on it puts it back",
                    m.what.describe()
                ),
                Style::new().fg(t.warn),
            ),
            None => Line::styled("? help  :q quit", Style::new().fg(t.faint)),
        },
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_palette(f: &mut Frame, body: Rect, app: &App, t: &Theme) {
    let n = app.suggestions.len().min(10) as u16;
    if n == 0 {
        return;
    }
    let w = 48.min(body.width);
    let area = Rect::new(body.x, body.bottom().saturating_sub(n + 2), w, n + 2);
    // Scroll so the highlighted suggestion stays visible.
    let skip = app.suggestion.saturating_sub(n as usize - 1);
    let items: Vec<ListItem> = app
        .suggestions
        .iter()
        .enumerate()
        .skip(skip)
        .take(n as usize)
        .map(|(i, s)| {
            let style = if i == app.suggestion {
                Style::new().bg(t.select_bg).fg(t.text)
            } else {
                Style::new().fg(t.text)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<14}", s.word),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(s.help.clone(), style.fg(t.faint)),
            ]))
        })
        .collect();
    f.render_widget(Clear, area);
    f.render_widget(List::new(items).block(overlay_block(" commands ", t)), area);
}

pub const HELP: &[(&str, &str)] = &[
    (":", "command palette (kinds, :ctx, :q)"),
    (":nb all", "a kind across every workspace"),
    (":nb <ws>", "a kind in a workspace (name or part)"),
    ("1-9 / 0", "this view in a recent workspace / all"),
    ("/", "filter: text, a|b, /re/, ~fuzzy, !invert"),
    ("enter", "drill in / open"),
    ("esc", "clear filter, then go back"),
    ("d", "describe (raw object as YAML)"),
    ("y", "item definition (notebook source, ...)"),
    ("F", "flatten folders: every item below"),
    ("o", "open in the Fabric portal"),
    ("r", "run the item (asks first)"),
    ("ctrl-k", "cancel the selected run (asks first)"),
    ("s", "schedules; in OneLake, a table's schema"),
    ("e / E", "rename; change a role / edit description"),
    ("ctrl-d", "delete (data stores: type the name)"),
    ("n", "new item or folder here"),
    ("m then p", "pick up an item or folder, drop it here"),
    ("a", "in roles: give someone a role"),
    ("D", "download a OneLake file to ./"),
    ("ctrl-p", "switch profile (identity)"),
    ("j k g G", "move, top, bottom"),
    ("ctrl-f ctrl-b", "page down / up"),
    ("ctrl-r", "refresh"),
    ("ctrl-e", "compact header"),
    ("ctrl-c", "quit"),
];

fn draw_help(f: &mut Frame, body: Rect, t: &Theme) {
    let h = (HELP.len() as u16 + 2).min(body.height);
    let w = 60.min(body.width);
    let area = Rect::new(
        body.x + (body.width - w) / 2,
        body.y + (body.height - h) / 2,
        w,
        h,
    );
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:>14}  "), Style::new().fg(t.key)),
                Span::styled(*d, Style::new().fg(t.text)),
            ])
        })
        .collect();
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(overlay_block(" help ", t)),
        area,
    );
}

fn draw_confirm(f: &mut Frame, body: Rect, app: &App, t: &Theme) {
    let Some(action) = &app.pending else {
        return;
    };
    let question = format!("{}?", action.describe());
    let w = (question.chars().count() as u16 + 6)
        .max(40)
        .min(body.width);
    let area = Rect::new(
        body.x + (body.width - w) / 2,
        body.y + body.height.saturating_sub(5) / 2,
        w,
        5.min(body.height),
    );
    let lines = vec![
        Line::styled(
            question,
            Style::new().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!("as {} ({})", app.profile.name, app.profile.account),
            Style::new().fg(t.dim),
        ),
        Line::from(vec![
            Span::styled("y", Style::new().fg(t.key).add_modifier(Modifier::BOLD)),
            Span::styled(" yes   ", Style::new().fg(t.dim)),
            Span::styled("any other key", Style::new().fg(t.key)),
            Span::styled(" no", Style::new().fg(t.dim)),
        ]),
    ];
    let block = overlay_block(" confirm ", t).border_style(Style::new().fg(t.warn));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).centered().block(block), area);
}

/// A typed prompt for a write: what enter does, the input, a filtered list
/// for prompts that pick from one, and a hint. The width never depends on
/// what's typed; a list only changes the height, and the top edge stays put.
fn draw_input(f: &mut Frame, body: Rect, app: &App, t: &Theme) {
    let Some(p) = &app.prompt else {
        return;
    };
    let all = p.all_choices().unwrap_or(&[]);
    let choices = p.choices(&app.input);
    let widest = [
        p.title.chars().count(),
        p.hint.chars().count(),
        all.iter().map(|c| c.len() + 4).max().unwrap_or(0),
    ]
    .into_iter()
    .max()
    .unwrap_or(0) as u16;
    let w = (widest + 4).max(50).min(body.width);
    // Four fixed lines plus the list, inside a border.
    let rows = |n: usize| 6 + n.max(usize::from(choices.is_some())) as u16;
    let tallest = rows(all.len()).min(body.height);
    let y = body.y + (body.height - tallest) / 2;
    let h = rows(choices.as_ref().map_or(0, Vec::len)).min(body.bottom() - y);
    let area = Rect::new(body.x + (body.width - w) / 2, y, w, h);

    let (title, border) = match p.purpose {
        Purpose::Delete { .. } => (" delete ", t.bad),
        _ => (" write ", t.warn),
    };
    let block = overlay_block(title, t).border_style(Style::new().fg(border));
    let inner = block.inner(area);
    // Long input scrolls: show its end.
    let room = inner.width.saturating_sub(3) as usize;
    let typed: String = {
        let n = app.input.chars().count();
        app.input.chars().skip(n.saturating_sub(room)).collect()
    };
    let mut lines = vec![
        prompt_title(p, t),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(t.accent)),
            Span::styled(typed.clone(), Style::new().fg(t.text)),
        ]),
    ];
    if let Some(choices) = &choices {
        if choices.is_empty() {
            lines.push(Line::styled("  nothing matches", Style::new().fg(t.faint)));
        }
        let fit = (inner.height as usize).saturating_sub(4).max(1);
        let skip = p.choice.saturating_sub(fit - 1);
        for (i, c) in choices.iter().enumerate().skip(skip).take(fit) {
            lines.push(if i == p.choice {
                Line::styled(
                    format!("▸ {c}"),
                    Style::new()
                        .fg(t.title)
                        .bg(t.select_bg)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::styled(format!("  {c}"), Style::new().fg(t.text))
            });
        }
    }
    lines.push(Line::styled(p.hint.clone(), Style::new().fg(t.dim)));
    lines.push(Line::styled(
        format!("as {} ({})", app.profile.name, app.profile.account),
        Style::new().fg(t.faint),
    ));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
    let x = inner.x + 2 + typed.chars().count() as u16;
    f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y + 1));
}

/// The prompt's title, with the folder it writes into in the folder colour.
fn prompt_title(p: &Prompt, t: &Theme) -> Line<'static> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let split = p
        .place
        .as_deref()
        .and_then(|at| Some((p.title.strip_suffix(at)?, at)));
    match split {
        Some((head, at)) => Line::from(vec![
            Span::styled(head.to_owned(), bold.fg(t.text)),
            Span::styled(at.to_owned(), bold.fg(t.accent)),
        ]),
        None => Line::styled(p.title.clone(), bold.fg(t.text)),
    }
}

/// Widest a profile name gets in the picker; longer ones (e.g. a service
/// principal named by its client id) end in `…`.
const PROFILE_NAME_W: usize = 24;

fn draw_profiles(f: &mut Frame, body: Rect, app: &App, t: &Theme) {
    let longest = |field: fn(&crate::app::ProfileInfo) -> &str| {
        app.profiles
            .iter()
            .map(|p| field(p).chars().count())
            .max()
            .unwrap_or(0)
    };
    let name_w = longest(|p| &p.name).min(PROFILE_NAME_W);
    let ro = app
        .profiles
        .iter()
        .map(|p| tags(p).len())
        .max()
        .unwrap_or(0);
    // "● " + name + gap + status + account + tag, inside the border.
    let fixed = 2 + name_w + 2 + AUTH_W + ro;
    let want = fixed + longest(|p| &p.account);
    let w = (want as u16 + 2)
        .max(PROFILE_HINTS.chars().count() as u16 + 4)
        .min(body.width);
    let account_w = (w as usize).saturating_sub(2 + fixed);
    let h = (app.profiles.len() as u16 + 2).min(body.height);
    let area = Rect::new(
        body.x + (body.width - w) / 2,
        body.y + (body.height - h) / 2,
        w,
        h,
    );
    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let base = if i == app.picker {
                Style::new().bg(t.select_bg)
            } else {
                Style::new()
            };
            let mark = if i == app.active { "● " } else { "  " };
            let mut spans = vec![
                Span::styled(mark, base.fg(t.ok)),
                Span::styled(
                    format!("{:<name_w$}  ", table::head(&p.name, name_w)),
                    base.fg(p.color.unwrap_or(t.accent))
                        .add_modifier(Modifier::BOLD),
                ),
                auth_cell(&p.auth, base, t),
                Span::styled(
                    format!("{:<account_w$}", table::head(&p.account, account_w)),
                    base.fg(t.text),
                ),
            ];
            if p.service_principal {
                spans.push(Span::styled(" [service principal]", base.fg(t.dim)));
            }
            if p.from_env {
                spans.push(Span::styled(" [from env]", base.fg(t.dim)));
            }
            if p.readonly {
                spans.push(Span::styled(" [read-only]", base.fg(t.bad)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    f.render_widget(Clear, area);
    let block = overlay_block(" profiles ", t).title_bottom(Span::styled(
        format!(" {PROFILE_HINTS} "),
        Style::new().fg(t.dim),
    ));
    f.render_widget(List::new(items).block(block), area);
}

/// The tags after a profile's account, for sizing the picker.
fn tags(p: &crate::app::ProfileInfo) -> String {
    let mut out = String::new();
    if p.service_principal {
        out.push_str(" [service principal]");
    }
    if p.from_env {
        out.push_str(" [from env]");
    }
    if p.readonly {
        out.push_str(" [read-only]");
    }
    out
}

const PROFILE_HINTS: &str = "enter switch · e rename · l sign in again · esc close";

/// Width of the sign-in column: the longest label plus a gap.
const AUTH_W: usize = 19;

/// A profile's sign-in state, padded to `AUTH_W` so the picker never
/// changes width as checks come back.
fn auth_cell(auth: &Auth, base: Style, t: &Theme) -> Span<'static> {
    let (text, color) = match auth {
        Auth::Unknown => (String::new(), t.dim),
        Auth::Checking => ("… checking".into(), t.pending),
        Auth::SignedIn => ("✓ signed in".into(), t.ok),
        Auth::Expired => ("✗ sign-in expired".into(), t.bad),
        Auth::Failed(e) => (
            format!(
                "✗ {}",
                table::head(e.lines().next().unwrap_or(""), AUTH_W - 4)
            ),
            t.bad,
        ),
    };
    Span::styled(format!("{text:<w$}", w = AUTH_W), base.fg(color))
}

fn overlay_block<'a>(title: &'a str, t: &Theme) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.accent))
        .title(Span::styled(title, Style::new().fg(t.title)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(110, 22)).unwrap();
        term.draw(|f| draw(f, app, &Theme::MOCHA)).unwrap();
        term.backend().to_string()
    }

    #[test]
    fn snapshot_workspaces() {
        let mut app = fake_app();
        insta::assert_snapshot!(render(&mut app));
    }

    fn steps(app: &mut App, keys: &[KeyCode]) {
        for k in keys {
            app.on_key(key(*k));
            settle(app);
        }
    }

    #[test]
    fn snapshot_folder_level() {
        let mut app = fake_app();
        steps(
            &mut app,
            &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Enter],
        );
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_flattened_items() {
        let mut app = fake_app();
        steps(
            &mut app,
            &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Char('F')],
        );
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_jobs_after_drill() {
        let mut app = fake_app();
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Enter,
        ];
        steps(&mut app, &keys);
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_confirm_run() {
        let mut app = fake_app();
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('G'),
            KeyCode::Char('r'),
        ];
        steps(&mut app, &keys);
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_onelake_files() {
        let mut app = fake_app();
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Enter,
        ];
        steps(&mut app, &keys);
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_recent_workspaces() {
        let mut app = fake_app();
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('G'),
            KeyCode::Enter,
            KeyCode::Char(':'),
        ];
        steps(&mut app, &keys);
        type_str(&mut app, "capacities");
        steps(&mut app, &[KeyCode::Enter]);
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_palette() {
        let mut app = fake_app();
        type_str(&mut app, ":no");
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_delete_prompt() {
        let mut app = fake_app();
        steps(
            &mut app,
            &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Char('j')],
        );
        app.on_key(crossterm::event::KeyEvent::new(
            KeyCode::Char('d'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        type_str(&mut app, "bron");
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn snapshot_new_type_picker() {
        let mut app = fake_app();
        steps(&mut app, &[KeyCode::Char('j'), KeyCode::Enter]);
        type_str(&mut app, "ne");
        let before = render(&mut app);
        type_str(&mut app, "v");
        app.on_key(key(KeyCode::Down));
        let after = render(&mut app);
        let edges = |s: &str| -> Vec<(usize, usize)> {
            s.lines()
                .filter_map(|l| Some((l.find("╭ write")?, l.rfind('╮')?)))
                .collect()
        };
        assert_eq!(edges(&before), edges(&after), "same width and top edge");
        insta::assert_snapshot!(after);
    }

    #[test]
    fn new_item_title_sets_the_folder_apart() {
        let mut app = fake_app();
        steps(
            &mut app,
            &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Enter],
        );
        app.on_key(key(KeyCode::Char('n')));
        let title = prompt_title(app.prompt.as_ref().unwrap(), &Theme::MOCHA);
        let parts: Vec<(&str, _)> = title
            .spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style.fg))
            .collect();
        assert_eq!(
            parts,
            [
                ("new … in ", Some(Theme::MOCHA.text)),
                ("ingest", Some(Theme::MOCHA.accent))
            ]
        );
    }

    #[test]
    fn snapshot_profile_picker_fits_long_names() {
        use crate::app::ProfileInfo;
        let p = |name: &str, account: &str, readonly| ProfileInfo {
            name: name.into(),
            account: account.into(),
            readonly,
            color: None,
            ..Default::default()
        };
        let mut app = App::new(
            vec![
                p("fabric-admin", "admin01@fabric.contoso.example", false),
                ProfileInfo {
                    service_principal: true,
                    ..p("sp-00000000", "00000000-1111-2222-3333-444444444444", true)
                },
                p("devops1", "me@devops.example", false),
            ],
            0,
        );
        settle(&mut app);
        app.on_key(crossterm::event::KeyEvent::new(
            KeyCode::Char('p'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        settle(&mut app);
        assert_eq!(app.profiles[0].auth, Auth::SignedIn);
        app.profiles[2].auth = Auth::Expired;
        insta::assert_snapshot!(render(&mut app));
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut app = fake_app();
        for steps in [":", "?"] {
            type_str(&mut app, steps);
            let mut term = Terminal::new(TestBackend::new(30, 8)).unwrap();
            term.draw(|f| draw(f, &mut app, &Theme::MOCHA)).unwrap();
            app.on_key(key(KeyCode::Esc));
        }
        // Profile picker, a confirm and a prompt, at a size no one uses.
        app.on_key(crossterm::event::KeyEvent::new(
            KeyCode::Char('p'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        for (w, h) in [(30, 8), (0, 0), (1, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &mut app, &Theme::MOCHA)).unwrap();
        }
        app.on_key(key(KeyCode::Esc));
        steps(
            &mut app,
            &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Char('G')],
        );
        app.on_key(key(KeyCode::Char('r')));
        let mut term = Terminal::new(TestBackend::new(30, 8)).unwrap();
        term.draw(|f| draw(f, &mut app, &Theme::MOCHA)).unwrap();
        app.on_key(key(KeyCode::Char('n')));
        app.on_key(key(KeyCode::Char('e')));
        term.draw(|f| draw(f, &mut app, &Theme::MOCHA)).unwrap();
    }

    #[test]
    fn breadcrumb_drops_leading_segments_when_narrow() {
        let mut app = fake_app();
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Enter,
        ];
        steps(&mut app, &keys);
        let text = |w| breadcrumb(&app, w, &Theme::MOCHA).to_string();
        assert_eq!(text(80), "Urban1 Bronze › bronze_lh (Lakehouse) › Files");
        assert_eq!(text(34), "… › bronze_lh (Lakehouse) › Files");
        assert_eq!(text(10), "… › Files");
    }
}
