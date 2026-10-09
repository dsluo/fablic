//! The resource table, k9s style: `kind(scope)[count]` title, filter in the title.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::resources::Health;
use crate::theme::Theme;

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let q = &app.frame.query;
    let mut title = vec![
        Span::styled(
            format!(" {}", q.kind.name()),
            Style::new().fg(t.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("({})", q.scope.label()), Style::new().fg(t.accent)),
        Span::styled(format!("[{}] ", app.view.len()), Style::new().fg(t.text)),
    ];
    if !app.frame.filter.is_empty() {
        title.push(Span::styled(
            format!("</{}> ", app.frame.filter),
            Style::new().fg(t.warn),
        ));
    }
    if app.loading {
        title.push(Span::styled("⟳ ", Style::new().fg(t.pending)));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.faint))
        .title(Line::from(title).centered());

    if app.view.is_empty() {
        let msg = match (&app.error, app.loading) {
            (Some(e), _) => Line::styled(e.clone(), Style::new().fg(t.bad)),
            (None, true) => Line::styled("loading…", Style::new().fg(t.dim)),
            (None, false) if app.rows.is_empty() => {
                Line::styled("nothing here", Style::new().fg(t.dim))
            }
            (None, false) => Line::styled("no rows match the filter", Style::new().fg(t.dim)),
        };
        f.render_widget(Paragraph::new(msg.centered()).block(block), area);
        return;
    }

    let columns = q.kind.columns();
    let widths = column_widths(app, columns);
    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(*c).style(Style::new().fg(t.dim).add_modifier(Modifier::BOLD))),
    );
    let rows = app.view.iter().map(|&i| {
        let r = &app.rows[i];
        let color = match r.health {
            Health::Ok => t.ok,
            Health::Pending => t.pending,
            Health::Warn => t.warn,
            Health::Bad => t.bad,
            Health::Neutral => t.text,
        };
        Row::new(r.cells.iter().map(|c| Cell::from(c.as_str()))).style(Style::new().fg(color))
    });
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(t.select_bg).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(table, area, &mut app.table);
}

/// Each column as wide as its widest cell (capped), the last one takes the rest.
fn column_widths(app: &App, columns: &[&str]) -> Vec<Constraint> {
    let n = columns.len();
    (0..n)
        .map(|c| {
            if c == n - 1 {
                return Constraint::Min(4);
            }
            let widest = app
                .view
                .iter()
                .filter_map(|&i| app.rows[i].cells.get(c))
                .map(|s| s.chars().count())
                .chain(std::iter::once(columns[c].len()))
                .max()
                .unwrap_or(4);
            Constraint::Length(widest.min(48) as u16)
        })
        .collect()
}
