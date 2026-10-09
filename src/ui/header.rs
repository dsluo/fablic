//! Top box: profile / scope info on the left, key hints in columns, logo on the right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::resources::Kind;
use crate::theme::Theme;

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
            ("s", "schedules"),
            ("F", "flatten"),
            ("o", "open"),
        ],
        Kind::Jobs => &[
            ("enter", "details"),
            ("r", "run again"),
            ("ctrl-k", "cancel"),
            ("s", "schedules"),
        ],
        Kind::Schedules => &[("r", "run now")],
        Kind::OneLake => &[("enter", "open")],
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

pub fn draw(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    if app.compact {
        return draw_compact(f, area, app, t);
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.faint));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let logo_w = if inner.width >= 100 { 20 } else { 0 };
    // Wide enough for the longest value, but leave at least half for hints and logo.
    let longest = [
        app.frame.query.scope.label().chars().count(),
        app.profile.account.len(),
    ]
    .into_iter()
    .max()
    .unwrap_or(0) as u16;
    let info_w = (longest + 12).clamp(40, inner.width / 2);
    let [info, keys, logo] = Layout::horizontal([
        Constraint::Length(info_w),
        Constraint::Min(0),
        Constraint::Length(logo_w),
    ])
    .areas(inner);

    let label = |k: &str| Span::styled(format!("{k:<10}"), Style::new().fg(t.dim));
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
    let q = &app.frame.query;
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
        Line::from(vec![label("Scope"), value(q.scope.label())]),
        Line::from(vec![label("Resource"), value(resource)]),
        Line::from(vec![label("Count"), value(count)]),
    ];
    f.render_widget(Paragraph::new(lines), info);

    // Hints fill fixed-width columns top to bottom, as many columns as fit.
    let col_w = 18u16;
    let rows = keys.height.max(1) as usize;
    let cols = (keys.width / col_w).max(1) as usize;
    let mut grid: Vec<Vec<Span>> = vec![Vec::new(); rows];
    for (i, (k, d)) in hints(q.kind).into_iter().take(rows * cols).enumerate() {
        let line = &mut grid[i % rows];
        line.push(Span::styled(format!("<{k}>"), Style::new().fg(t.key)));
        let pad = (col_w as usize).saturating_sub(k.len() + 2);
        line.push(Span::styled(
            format!(" {d:<w$}", w = pad.saturating_sub(1)),
            Style::new().fg(t.dim),
        ));
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
        Span::styled(
            format!("  {}", app.frame.query.scope.label()),
            Style::new().fg(t.text),
        ),
    ];
    if app.profile.readonly {
        spans.push(Span::styled("  [read-only]", Style::new().fg(t.bad)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
