//! Scrollable text view for describe output, with light YAML highlighting.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::theme::Theme;

pub fn draw(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let d = &app.detail;
    let pos = format!(" {}/{} ", (d.scroll + 1).min(d.lines.len()), d.lines.len());
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.faint))
        .title(
            Line::from(Span::styled(
                format!(" {} ", d.title),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ))
            .centered(),
        )
        .title_bottom(Line::styled(pos, Style::new().fg(t.dim)).right_aligned());
    let height = block.inner(area).height as usize;
    let lines: Vec<Line> = d
        .lines
        .iter()
        .skip(d.scroll)
        .take(height)
        .map(|l| highlight(l, t))
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// `key: value` gets the key coloured; everything else is plain text.
fn highlight<'a>(line: &'a str, t: &Theme) -> Line<'a> {
    let trimmed = line.trim_start_matches([' ', '-']);
    let indent = &line[..line.len() - trimmed.len()];
    match trimmed.split_once(':') {
        Some((k, v)) if !k.is_empty() && !k.contains(' ') => Line::from(vec![
            Span::styled(indent, Style::new().fg(t.faint)),
            Span::styled(k, Style::new().fg(t.accent)),
            Span::styled(":", Style::new().fg(t.faint)),
            Span::styled(v, Style::new().fg(t.text)),
        ]),
        _ => Line::styled(line, Style::new().fg(t.text)),
    }
}
