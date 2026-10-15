//! Top box: profile / scope info on the left, key hints in columns, logo on the right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::resources::Kind;
use crate::theme::Theme;

use super::table::head;

const LOGO_W: u16 = 20;
/// Width of one column of key hints.
const HINT_W: u16 = 18;

const LOGO: &[&str] = &[
    "  ┌─┐┌─┐┌┐ ┬  ┬┌─┐",
    "  ├┤ ├─┤├┴┐│  ││  ",
    "  └  ┴ ┴└─┘┴─┘┴└─┘",
];

/// Key hints for the current kind, on top of the global ones.
pub fn hints(kind: Kind) -> Vec<(&'static str, &'static str)> {
    let specific: &[(&str, &str)] = match kind {
        Kind::Workspaces => &[("enter", "items"), ("o", "open")],
        Kind::Items => &[
            ("enter", "open"),
            ("y", "definition"),
            ("r", "run"),
            ("F", "flatten"),
            ("e", "rename"),
            ("m/p", "move"),
            ("ctrl-d", "delete"),
        ],
        Kind::Roles => &[("a", "add"), ("e", "change role"), ("ctrl-d", "remove")],
        Kind::Shortcuts => &[("ctrl-d", "delete")],
        Kind::Jobs => &[
            ("enter", "details"),
            ("r", "run again"),
            ("ctrl-k", "cancel"),
            ("s", "schedules"),
        ],
        Kind::Schedules => &[("r", "run now")],
        Kind::OneLake => &[("enter", "open"), ("s", "table schema"), ("D", "download")],
        _ => &[],
    };
    let global = [
        (":", "command"),
        ("/", "filter"),
        ("?", "help"),
        ("d", "describe"),
        ("esc", "back"),
        ("ctrl-p", "profile"),
        ("ctrl-r", "refresh"),
    ];
    specific.iter().copied().chain(global).collect()
}

/// `<0> all` and `<1>`..`<9>` for recent workspaces, the active one brighter.
/// Empty until a workspace has been opened.
fn workspace_hints(app: &App, t: &Theme) -> Vec<Option<(String, String, Style)>> {
    let recent = app.recent_workspaces();
    if recent.is_empty() {
        return Vec::new();
    }
    let active = app.active_workspace().map(|w| w.id.as_str());
    let style = |on: bool| Style::new().fg(if on { t.text } else { t.dim });
    let all = Some(("0".to_owned(), "all".to_owned(), style(active.is_none())));
    let numbered = recent.iter().enumerate().map(|(i, ws)| {
        let on = active == Some(ws.id.as_str());
        Some(((i + 1).to_string(), ws.name.clone(), style(on)))
    });
    std::iter::once(all).chain(numbered).collect()
}

/// The header names only the workspace; the table title shows the full path.
fn workspace(app: &App) -> String {
    match &app.frame.query.scope.workspace {
        Some(ws) => ws.name.clone(),
        None => "all".into(),
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    if app.compact {
        return draw_compact(f, area, app, t);
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.faint));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Wide enough for the longest value, but leave at least half for hints and logo.
    let longest = [workspace(app).chars().count(), app.profile.account.len()]
        .into_iter()
        .max()
        .unwrap_or(0) as u16;
    let info_w = (longest + 13).max(40).min(inner.width / 2);
    // Hints fill fixed-width columns top to bottom. Recent workspaces come
    // first, in columns of their own, like k9s namespaces.
    let rows = inner.height.max(1) as usize;
    let mut entries = workspace_hints(app, t);
    if !entries.is_empty() {
        entries.resize_with(entries.len().div_ceil(rows) * rows, || None);
    }
    let dim = Style::new().fg(t.dim);
    let q = &app.frame.query;
    entries.extend(
        hints(q.kind)
            .into_iter()
            .map(|(k, d)| Some((k.to_owned(), d.to_owned(), dim))),
    );
    // The logo goes first when space is short: hints matter more.
    let hint_cols = entries.len().div_ceil(rows).max(2) as u16;
    let logo_w = if inner.width.saturating_sub(info_w + LOGO_W) >= hint_cols * HINT_W {
        LOGO_W
    } else {
        0
    };
    let [info, keys, logo] = Layout::horizontal([
        Constraint::Length(info_w),
        Constraint::Min(0),
        Constraint::Length(logo_w),
    ])
    .areas(inner);

    let label = |k: &str| Span::styled(format!("{k:<11}"), Style::new().fg(t.dim));
    let value = |v: String| Span::styled(v, Style::new().fg(t.text));
    let mut profile = vec![
        label("Profile"),
        Span::styled(
            app.profile.name.clone(),
            Style::new()
                .fg(app.profile.color.unwrap_or(t.accent))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if app.profile.readonly {
        profile.push(Span::styled(" [read-only]", Style::new().fg(t.bad)));
    }
    let resource = match &q.item_type {
        Some(ty) => format!("{} ({ty})", q.kind.name()),
        None => q.kind.name().to_owned(),
    };
    let count = if app.view.len() == app.rows.len() {
        app.rows.len().to_string()
    } else {
        format!("{}/{}", app.view.len(), app.rows.len())
    };
    let lines = vec![
        Line::from(profile),
        Line::from(vec![label("Account"), value(app.profile.account.clone())]),
        Line::from(vec![label("Workspace"), value(workspace(app))]),
        Line::from(vec![label("Resource"), value(resource)]),
        Line::from(vec![label("Count"), value(count)]),
    ];
    f.render_widget(Paragraph::new(lines), info);

    // As many hint columns as fit.
    let col_w = HINT_W;
    let cols = (keys.width / col_w).max(1) as usize;
    let mut grid: Vec<Vec<Span>> = vec![Vec::new(); rows];
    for (i, entry) in entries.into_iter().take(rows * cols).enumerate() {
        let line = &mut grid[i % rows];
        let Some((k, d, style)) = entry else {
            line.push(Span::raw(" ".repeat(col_w as usize)));
            continue;
        };
        let pad = (col_w as usize).saturating_sub(k.len() + 2);
        let w = pad.saturating_sub(1);
        line.push(Span::styled(format!("<{k}>"), Style::new().fg(t.key)));
        // One cell short, so a long name never runs into the next column.
        let d = head(&d, w.saturating_sub(1));
        line.push(Span::styled(format!(" {d:<w$}"), style));
    }
    f.render_widget(
        Paragraph::new(grid.into_iter().map(Line::from).collect::<Vec<_>>()),
        keys,
    );

    if logo_w > 0 {
        let lines: Vec<Line> = LOGO
            .iter()
            .map(|l| Line::styled(*l, Style::new().fg(t.title)))
            .collect();
        f.render_widget(Paragraph::new(lines), logo);
    }
}

fn draw_compact(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let mut spans = vec![
        Span::styled(
            app.profile.name.clone(),
            Style::new()
                .fg(app.profile.color.unwrap_or(t.accent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", workspace(app)), Style::new().fg(t.text)),
    ];
    if app.profile.readonly {
        spans.push(Span::styled("  [read-only]", Style::new().fg(t.bad)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
