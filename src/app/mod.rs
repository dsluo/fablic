//! App state and input handling. Pure: it never does I/O itself, it queues
//! `Effect`s that the event loop carries out and answers with `Msg`s.

mod command;
mod navigation;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::filter::Filter;
use crate::resources::{Health, Kind, Row};
use crate::source::{Query, Scope};

pub use command::Suggestion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Table,
    Command,
    Filter,
    Detail,
    Help,
}

/// One table view: what is listed, how it is filtered, and where the cursor was.
#[derive(Debug, Clone)]
pub struct Frame {
    pub query: Query,
    pub filter: String,
    pub selected: usize,
}

#[derive(Debug)]
pub enum Effect {
    Fetch { generation: u64, query: Query },
}

#[derive(Debug)]
pub enum Msg {
    Rows {
        generation: u64,
        result: Result<Vec<Row>, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub level: Level,
    pub at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub name: String,
    pub account: String,
    pub readonly: bool,
}

/// How often a jobs view with active runs refreshes itself.
const LIVE_REFRESH: Duration = Duration::from_secs(5);

pub struct App {
    pub mode: Mode,
    pub frame: Frame,
    pub stack: Vec<Frame>,
    pub rows: Vec<Row>,
    /// Indices into `rows` that pass the filter, in display order.
    pub view: Vec<usize>,
    pub table: TableState,
    pub input: String,
    pub suggestions: Vec<Suggestion>,
    pub suggestion: usize,
    pub detail: Detail,
    pub status: Option<Status>,
    pub loading: bool,
    pub error: Option<String>,
    pub profile: ProfileInfo,
    pub compact: bool,
    pub quit: bool,
    /// Rows of the body area at the last draw, for page up/down.
    pub page: usize,
    effects: Vec<Effect>,
    generation: u64,
    fetched_at: Option<Instant>,
    cache: HashMap<(String, Query), Vec<Row>>,
}

impl App {
    pub fn new(profile: ProfileInfo) -> App {
        let query = Query {
            kind: Kind::Workspaces,
            scope: Scope::default(),
            item_type: None,
        };
        let mut app = App {
            mode: Mode::Table,
            frame: Frame {
                query,
                filter: String::new(),
                selected: 0,
            },
            stack: Vec::new(),
            rows: Vec::new(),
            view: Vec::new(),
            table: TableState::default(),
            input: String::new(),
            suggestions: Vec::new(),
            suggestion: 0,
            detail: Detail::default(),
            status: None,
            loading: false,
            error: None,
            profile,
            compact: false,
            quit: false,
            page: 20,
            effects: Vec::new(),
            generation: 0,
            fetched_at: None,
            cache: HashMap::new(),
        };
        app.load();
        app
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn kind(&self) -> Kind {
        self.frame.query.kind
    }

    pub fn selected_row(&self) -> Option<&Row> {
        let i = self.table.selected()?;
        self.view.get(i).and_then(|&r| self.rows.get(r))
    }

    pub fn flash(&mut self, level: Level, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            level,
            at: Instant::now(),
        });
    }

    /// Show cached rows for the current frame (if any) and fetch fresh ones.
    fn load(&mut self) {
        let key = (self.profile.name.clone(), self.frame.query.clone());
        match self.cache.get(&key) {
            Some(rows) => {
                self.rows = rows.clone();
                self.apply_filter();
            }
            None => {
                self.rows.clear();
                self.view.clear();
                self.table.select(None);
            }
        }
        self.refresh();
    }

    pub fn refresh(&mut self) {
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.fetched_at = Some(Instant::now());
        self.effects.push(Effect::Fetch {
            generation: self.generation,
            query: self.frame.query.clone(),
        });
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Rows { generation, result } => {
                if generation != self.generation {
                    return; // answer to a view we already left
                }
                self.loading = false;
                match result {
                    Ok(rows) => {
                        let key = (self.profile.name.clone(), self.frame.query.clone());
                        self.cache.insert(key, rows.clone());
                        self.rows = rows;
                        self.apply_filter();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub fn on_tick(&mut self) {
        let live =
            self.kind() == Kind::Jobs && self.rows.iter().any(|r| r.health == Health::Pending);
        let due = self.fetched_at.is_none_or(|t| t.elapsed() >= LIVE_REFRESH);
        if live && due && !self.loading && self.mode == Mode::Table {
            self.refresh();
        }
    }

    /// Recompute `view` from the filter, keeping the cursor on the same row id if possible.
    pub fn apply_filter(&mut self) {
        let keep = self.selected_row().map(|r| r.id.clone());
        let filter = Filter::parse(&self.frame.filter).unwrap_or(Filter::All);
        self.view = filter.apply(&self.rows);
        let pos = keep
            .and_then(|id| self.view.iter().position(|&i| self.rows[i].id == id))
            .unwrap_or(self.frame.selected);
        self.select(pos);
    }

    fn select(&mut self, i: usize) {
        if self.view.is_empty() {
            self.table.select(None);
        } else {
            let i = i.min(self.view.len() - 1);
            self.table.select(Some(i));
            self.frame.selected = i;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.select((cur + delta).max(0) as usize);
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Table => self.key_table(key, ctrl),
            Mode::Command => self.key_command(key),
            Mode::Filter => self.key_filter(key),
            Mode::Detail => self.key_detail(key, ctrl),
            Mode::Help => self.mode = Mode::Table,
        }
    }

    fn key_table(&mut self, key: KeyEvent, ctrl: bool) {
        match (key.code, ctrl) {
            (KeyCode::Char('j') | KeyCode::Down, false) => self.move_by(1),
            (KeyCode::Char('k') | KeyCode::Up, false) => self.move_by(-1),
            (KeyCode::Char('g') | KeyCode::Home, false) => self.select(0),
            (KeyCode::Char('G') | KeyCode::End, false) => self.select(usize::MAX),
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) => self.move_by(self.page as isize),
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => {
                self.move_by(-(self.page as isize))
            }
            (KeyCode::Enter, _) => self.drill(),
            (KeyCode::Esc, _) => self.back(),
            (KeyCode::Char(':'), false) => self.open_command(),
            (KeyCode::Char('/'), false) => {
                self.input = self.frame.filter.clone();
                self.mode = Mode::Filter;
            }
            (KeyCode::Char('d'), false) => self.describe(),
            (KeyCode::Char('r'), true) => {
                self.refresh();
                self.flash(Level::Info, "refreshing");
            }
            (KeyCode::Char('e'), true) => self.compact = !self.compact,
            (KeyCode::Char('?'), false) => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.frame.filter.clear();
                self.input.clear();
                self.apply_filter();
                self.mode = Mode::Table;
            }
            KeyCode::Enter => {
                if let Err(e) = Filter::parse(&self.input) {
                    self.flash(Level::Error, e);
                    return;
                }
                self.mode = Mode::Table;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.live_filter();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.live_filter();
            }
            _ => {}
        }
    }

    /// Filter as you type; an unfinished regex keeps the last good result.
    fn live_filter(&mut self) {
        if Filter::parse(&self.input).is_ok() {
            self.frame.filter = self.input.clone();
            self.apply_filter();
        }
    }

    fn key_detail(&mut self, key: KeyEvent, ctrl: bool) {
        let max = self.detail.lines.len().saturating_sub(1);
        let d = &mut self.detail;
        match (key.code, ctrl) {
            (KeyCode::Esc | KeyCode::Char('q'), _) => self.mode = Mode::Table,
            (KeyCode::Char('j') | KeyCode::Down, false) => d.scroll = (d.scroll + 1).min(max),
            (KeyCode::Char('k') | KeyCode::Up, false) => d.scroll = d.scroll.saturating_sub(1),
            (KeyCode::Char('g') | KeyCode::Home, false) => d.scroll = 0,
            (KeyCode::Char('G') | KeyCode::End, false) => d.scroll = max,
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) => {
                d.scroll = (d.scroll + self.page).min(max)
            }
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => {
                d.scroll = d.scroll.saturating_sub(self.page)
            }
            _ => {}
        }
    }

    pub fn describe(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let body = serde_yaml::to_string(&row.raw).unwrap_or_else(|e| e.to_string());
        self.detail = Detail {
            title: format!("{} › {}", self.kind().name(), row.name),
            lines: body.lines().map(str::to_owned).collect(),
            scroll: 0,
        };
        self.mode = Mode::Detail;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::source::{DataSource, FakeSource};

    pub fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    pub fn fake_app() -> App {
        let mut app = App::new(ProfileInfo {
            name: "fake".into(),
            account: "me@example.com".into(),
            readonly: false,
        });
        settle(&mut app);
        app
    }

    /// Run queued fetches against `FakeSource` until nothing is pending.
    pub fn settle(app: &mut App) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        loop {
            let effects = app.take_effects();
            if effects.is_empty() {
                break;
            }
            for e in effects {
                let Effect::Fetch { generation, query } = e;
                let result = rt
                    .block_on(FakeSource.fetch(query))
                    .map_err(|e| e.to_string());
                app.on_msg(Msg::Rows { generation, result });
            }
        }
    }

    pub fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn starts_on_workspaces() {
        let app = fake_app();
        assert_eq!(app.kind(), Kind::Workspaces);
        assert_eq!(app.view.len(), 3);
        assert_eq!(app.table.selected(), Some(0));
    }

    #[test]
    fn filter_narrows_and_esc_clears() {
        let mut app = fake_app();
        type_str(&mut app, "/silver");
        assert_eq!(app.view.len(), 1);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Table);
        assert_eq!(app.selected_row().unwrap().id, "ws-2");
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.view.len(), 3);
    }

    #[test]
    fn stale_responses_are_dropped() {
        let mut app = fake_app();
        app.refresh();
        let old = app.generation;
        app.refresh();
        app.on_msg(Msg::Rows {
            generation: old,
            result: Ok(Vec::new()),
        });
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn describe_shows_yaml() {
        let mut app = fake_app();
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.mode, Mode::Detail);
        assert!(
            app.detail
                .lines
                .iter()
                .any(|l| l.contains("displayName: Urban1 Bronze"))
        );
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Table);
    }
}
