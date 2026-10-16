//! The resource table, k9s style: `kind(scope)[count]` title, filter in the title.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::resources::{self, Health, Kind};
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

    let columns = Kind::columns_for(q);
    let lead = if q.tree {
        guides(&app.rows, &app.view)
    } else {
        Vec::new()
    };
    let avail = area.width.saturating_sub(2) as usize;
    let mut widths = column_widths(app, columns, avail, &lead);
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
    let rows = app.view.iter().enumerate().map(|(pos, &i)| {
        let r = &app.rows[i];
        let color = match r.health {
            _ if r.dir => t.accent,
            Health::Ok => t.ok,
            Health::Pending => t.pending,
            Health::Warn => t.warn,
            Health::Bad => t.bad,
            Health::Neutral => t.text,
        };
        let cells =
            r.cells
                .iter()
                .enumerate()
                .map(|(c, text)| match (columns.get(c), lead.get(pos)) {
                    (_, Some((guide, open))) if c == 0 => {
                        Cell::from(tree_cell(guide, *open, text, widths[0]))
                    }
                    (Some(title), _) if is_path(title) => Cell::from(tail(text, widths[c])),
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
/// In a tree, `lead` holds each row's guides, which come on top of the cap
/// so deep names aren't cut shorter than shallow ones.
fn column_widths(app: &App, columns: &[&str], avail: usize, lead: &[(String, bool)]) -> Vec<usize> {
    let desired: Vec<usize> = (0..columns.len())
        .map(|c| {
            let cap = column_cap(c, columns[c]);
            app.view
                .iter()
                .enumerate()
                .filter_map(|(pos, &i)| {
                    let text = app.rows[i].cells.get(c)?.chars().count().min(cap);
                    let guide = match lead.get(pos) {
                        Some((g, _)) if c == 0 => g.chars().count(),
                        _ => 0,
                    };
                    Some(guide + text)
                })
                .chain(std::iter::once(columns[c].len()))
                .max()
                .unwrap_or(4)
        })
        .collect();
    fit_widths(&desired, columns, avail)
}

/// Lines for a tree view, per row of `view`, drawn from the rows that are
/// visible so a filtered tree still connects: the `├─` / `└─` guide, and
/// whether something shows below the row (an open folder).
pub(super) fn guides(rows: &[resources::Row], view: &[usize]) -> Vec<(String, bool)> {
    let mut out = vec![(String::new(), false); view.len()];
    // From the bottom up, `more[d]`: a later row at depth `d` shares the
    // current row's ancestor at that depth.
    let mut more: Vec<bool> = Vec::new();
    for (pos, &i) in view.iter().enumerate().rev() {
        let depth = rows[i].depth;
        let next = |d: usize| more.get(d).copied().unwrap_or(false);
        let mut guide = String::new();
        for d in 1..depth {
            guide.push_str(if next(d) { "│  " } else { "   " });
        }
        if depth > 0 {
            guide.push_str(if next(depth) { "├─ " } else { "└─ " });
        }
        let open = view.get(pos + 1).is_some_and(|&n| rows[n].depth > depth);
        out[pos] = (guide, open);
        more.resize(depth + 1, false);
        more[depth] = true;
    }
    out
}

/// The name cell of a tree row: guides, then the name cut to what is left.
/// An open folder's `▸` turns to `▾`.
fn tree_cell(guide: &str, open: bool, text: &str, width: usize) -> String {
    let text = match text.strip_prefix("▸ ") {
        Some(name) if open => format!("▾ {name}"),
        _ => text.to_owned(),
    };
    match width.checked_sub(guide.chars().count()) {
        Some(room) if room > 1 => format!("{guide}{}", head(&text, room)),
        _ => head(&format!("{guide}{text}"), width),
    }
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
    fn guides_follow_what_is_visible() {
        let rows: Vec<resources::Row> = [(0, "▸ a"), (1, "b"), (2, "c"), (1, "d"), (0, "e")]
            .into_iter()
            .map(|(depth, name)| resources::Row {
                depth,
                ..resources::Row::new(name, name, vec![name.into()])
            })
            .collect();
        let lines = |view: &[usize]| -> Vec<String> {
            guides(&rows, view)
                .into_iter()
                .zip(view)
                .map(|((g, open), &i)| tree_cell(&g, open, &rows[i].cells[0], 20))
                .collect()
        };
        assert_eq!(
            lines(&[0, 1, 2, 3, 4]),
            ["▾ a", "├─ b", "│  └─ c", "└─ d", "e"]
        );
        // With d filtered out, b is the last child; with nothing under a, it is closed.
        assert_eq!(lines(&[0, 1, 2, 4]), ["▾ a", "└─ b", "   └─ c", "e"]);
        assert_eq!(lines(&[0, 4]), ["▸ a", "e"]);
    }

    #[test]
    fn paths_keep_their_end() {
        assert_eq!(tail("/ETL_CFG_Tables_Insertion", 12), "…s_Insertion");
        assert_eq!(tail("/ingest", 12), "/ingest");
        assert_eq!(head("New notebook", 11), "New notebo…");
        assert_eq!(head("New notebook", 12), "New notebook");
    }
}
