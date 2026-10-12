//! The resource table, k9s style: `kind(scope)[count]` title, filter in the title.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::resources::{Health, Kind};
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

    let mut columns: Vec<&str> = Kind::columns_for(q).to_vec();
    let params = app.show_parameters();
    if params {
        columns.push("PARAMETERS");
    }
    // Each shown row's cell texts, with the parameters cell when it's on.
    let texts: Vec<Vec<String>> = app
        .view
        .iter()
        .map(|&i| {
            let r = &app.rows[i];
            let mut cells = r.cells.clone();
            if params {
                cells.resize(columns.len() - 1, String::new());
                cells.push(app.parameters_cell(r));
            }
            cells
        })
        .collect();
    let avail = area.width.saturating_sub(2) as usize;
    let mut widths = column_widths(&texts, &columns, avail);
    // The last column takes whatever is left over.
    if let Some((last, others)) = widths.split_last_mut() {
        let used = others.iter().sum::<usize>() + 2 * others.len();
        *last = (*last).max(avail.saturating_sub(used));
    }
    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(*c).style(Style::new().fg(t.dim).add_modifier(Modifier::BOLD))),
    );
    let rows = app.view.iter().zip(&texts).map(|(&i, cells)| {
        let r = &app.rows[i];
        let color = match r.health {
            _ if r.dir => t.accent,
            Health::Ok => t.ok,
            Health::Pending => t.pending,
            Health::Warn => t.warn,
            Health::Bad => t.bad,
            Health::Neutral => t.text,
        };
        let cells = cells
            .iter()
            .enumerate()
            .map(|(c, text)| match columns.get(c) {
                Some(title) if is_path(title) => Cell::from(tail(text, widths[c])),
                _ => Cell::from(head(text, widths[c])),
            });
        Row::new(cells).style(Style::new().fg(color))
    });
    let constraints = widths.iter().map(|&w| Constraint::Length(w as u16));
    let table = Table::new(rows, constraints)
        .header(header)
        .block(block)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(t.select_bg).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(table, area, &mut app.table);
}

/// Widest a column may grow. The first column is the name, so it gets a lot;
/// paths get more, since their end is the useful part; free text like
/// descriptions gets the least, since describe shows it in full.
fn column_cap(index: usize, title: &str) -> usize {
    match (index, title) {
        (_, "FOLDER" | "PATH") => 64,
        (0, _) => 48,
        (_, "DESCRIPTION") => 24,
        _ => 32,
    }
}

/// Columns whose cells are paths: cut from the left, keeping the deepest part.
fn is_path(title: &str) -> bool {
    matches!(title, "FOLDER" | "PATH")
}

/// How narrow a column may get when the table doesn't fit: ids can vanish,
/// everything else keeps its header (and names a usable minimum).
fn column_floor(index: usize, title: &str) -> usize {
    match (index, title) {
        (_, "ID") => 0,
        (0, _) => title.len().max(20),
        _ => title.len(),
    }
}

/// Fit `desired` widths into `avail` columns (with two spaces between
/// columns). Ids give way first, then descriptions; after that space comes
/// from whichever column is widest at the time, so a long name and a long
/// path shrink toward each other. Nothing goes below its floor.
fn fit_widths(desired: &[usize], columns: &[&str], avail: usize) -> Vec<usize> {
    let n = desired.len();
    let mut w = desired.to_vec();
    let needed = w.iter().sum::<usize>() + 2 * n.saturating_sub(1);
    let mut over = needed.saturating_sub(avail);
    let floor = |i: usize| column_floor(i, columns[i]);
    for t in ["ID", "DESCRIPTION"] {
        if let Some(i) = columns.iter().position(|c| *c == t) {
            let take = over.min(w[i].saturating_sub(floor(i)));
            w[i] -= take;
            over -= take;
        }
    }
    while over > 0 {
        let Some(i) = (0..n)
            .filter(|&i| w[i] > floor(i))
            .max_by_key(|&i| (w[i], n - i))
        else {
            break;
        };
        w[i] -= 1;
        over -= 1;
    }
    w
}

/// Each column as wide as its widest cell (capped), shrunk to fit `avail`.
fn column_widths(texts: &[Vec<String>], columns: &[&str], avail: usize) -> Vec<usize> {
    let desired: Vec<usize> = (0..columns.len())
        .map(|c| {
            texts
                .iter()
                .filter_map(|cells| cells.get(c))
                .map(|s| s.chars().count())
                .chain(std::iter::once(columns[c].len()))
                .max()
                .unwrap_or(4)
                .min(column_cap(c, columns[c]))
        })
        .collect();
    fit_widths(&desired, columns, avail)
}

/// Text cut to `width` with a trailing `…`, so a cut is visible.
pub(super) fn head(s: &str, width: usize) -> String {
    if s.chars().count() <= width || width == 0 {
        return s.to_owned();
    }
    let keep: String = s.chars().take(width - 1).collect();
    format!("{keep}…")
}

/// A path cut to `width` from the left: `…/ingest/shop`.
fn tail(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width || width == 0 {
        return s.to_owned();
    }
    let keep: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{keep}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_descriptions_give_way_before_folders() {
        let cols = ["NAME", "TYPE", "FOLDER", "DESCRIPTION", "ID"];
        let desired = [43, 16, 45, 24, 36];
        // 164 + 8 spacing wanted, 118 available: 54 to give back.
        let w = fit_widths(&desired, &cols, 118);
        assert_eq!(
            w,
            [41, 16, 42, 11, 0],
            "ids go, descriptions keep their header"
        );
        // Narrower: name and folder shrink toward each other.
        let w = fit_widths(&desired, &cols, 90);
        assert_eq!(w, [27, 16, 28, 11, 0]);
        assert_eq!(w.iter().sum::<usize>() + 8, 90);
        assert_eq!(w.iter().sum::<usize>() + 8, 90);
    }

    #[test]
    fn paths_keep_their_end() {
        assert_eq!(tail("/ETL_CFG_Tables_Insertion", 12), "…s_Insertion");
        assert_eq!(tail("/ingest", 12), "/ingest");
        assert_eq!(head("New notebook", 11), "New notebo…");
        assert_eq!(head("New notebook", 12), "New notebook");
    }
}
