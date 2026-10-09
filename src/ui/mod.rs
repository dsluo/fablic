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

use crate::app::{App, Level, Mode};
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
        _ => {
            // Breadcrumbs: every frame on the stack, then the current one.
            let mut spans = Vec::new();
            let frames = app.stack.iter().chain(std::iter::once(&app.frame));
            for (i, fr) in frames.enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" › ", Style::new().fg(t.faint)));
                }
                spans.push(Span::styled(fr.query.kind.name(), Style::new().fg(t.dim)));
            }
            if let Some(last) = spans.last_mut() {
                last.style = Style::new().fg(t.title).add_modifier(Modifier::BOLD);
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
    if matches!(app.mode, Mode::Command | Mode::Filter) {
        let x = area.x + 1 + app.input.chars().count() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
    }
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
        (None, None) => Line::styled("? help  :q quit", Style::new().fg(t.faint)),
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
    ("/", "filter: text, a|b, /re/, ~fuzzy, !invert"),
    ("enter", "drill in / open"),
    ("esc", "clear filter, then go back"),
    ("d", "describe (raw object as YAML)"),
    ("y", "item definition (notebook source, ...)"),
    ("F", "flatten folders: every item below"),
    ("o", "open in the Fabric portal"),
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

fn draw_profiles(f: &mut Frame, body: Rect, app: &App, t: &Theme) {
    let h = (app.profiles.len() as u16 + 2).min(body.height);
    let w = 64.min(body.width);
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
                    format!("{:<16}", p.name),
                    base.fg(p.color.unwrap_or(t.accent))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(p.account.clone(), base.fg(t.text)),
            ];
            if p.readonly {
                spans.push(Span::styled(" [read-only]", base.fg(t.bad)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    f.render_widget(Clear, area);
    f.render_widget(List::new(items).block(overlay_block(" profiles ", t)), area);
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
    fn snapshot_palette() {
        let mut app = fake_app();
        type_str(&mut app, ":no");
        insta::assert_snapshot!(render(&mut app));
    }
}
