//! The `:` command palette: fuzzy-matched resource kinds, aliases and commands.

use crossterm::event::{KeyCode, KeyEvent};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use super::{App, Level, Mode};
use crate::resources::{ITEM_TYPE_ALIASES, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub word: String,
    pub help: String,
    /// Words the fuzzy matcher searches: the command and its aliases.
    keys: String,
}

const COMMANDS: &[(&str, &str)] = &[
    ("ctx", "switch profile (identity)"),
    ("q", "quit"),
    ("help", "key bindings"),
];

fn candidates() -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Kind::ALL
        .iter()
        .map(|k| Suggestion {
            word: k.name().into(),
            help: k.aliases().join(" "),
            keys: format!("{} {}", k.name(), k.aliases().join(" ")),
        })
        .collect();
    // Only the long item-type aliases; the short ones would crowd the list.
    out.extend(
        ITEM_TYPE_ALIASES
            .iter()
            .filter(|(a, _)| a.len() > 3)
            .map(|(a, t)| Suggestion {
                word: (*a).into(),
                help: format!("items of type {t}"),
                keys: (*a).into(),
            }),
    );
    out.extend(COMMANDS.iter().map(|(w, h)| Suggestion {
        word: (*w).into(),
        help: (*h).into(),
        keys: (*w).into(),
    }));
    out
}

/// Candidates ranked by fuzzy score against the first word typed.
fn suggest(input: &str) -> Vec<Suggestion> {
    let word = input.split_whitespace().next().unwrap_or("");
    let all = candidates();
    if word.is_empty() {
        return all;
    }
    let pattern = Pattern::parse(word, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, Suggestion)> = all
        .into_iter()
        .filter_map(|s| {
            let score = s
                .keys
                .split(' ')
                .filter_map(|k| pattern.score(Utf32Str::new(k, &mut buf), &mut matcher))
                .max()?;
            // Exact and prefix hits on the word itself beat hits on an alias.
            let bonus = if s.word == word {
                10_000
            } else if s.word.starts_with(word) {
                1_000
            } else {
                0
            };
            Some((score + bonus, s))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.into_iter().map(|(_, s)| s).collect()
}

impl App {
    pub(super) fn open_command(&mut self) {
        self.input.clear();
        self.suggestions = suggest("");
        self.suggestion = 0;
        self.mode = Mode::Command;
    }

    pub(super) fn key_command(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Table,
            KeyCode::Enter => {
                self.mode = Mode::Table;
                let input = std::mem::take(&mut self.input);
                self.run_command(&input);
            }
            KeyCode::Tab => {
                if let Some(s) = self.suggestions.get(self.suggestion) {
                    self.input = s.word.clone();
                    self.suggestions = suggest(&self.input);
                    self.suggestion = 0;
                }
            }
            KeyCode::Down => {
                if !self.suggestions.is_empty() {
                    self.suggestion = (self.suggestion + 1) % self.suggestions.len();
                }
            }
            KeyCode::Up => {
                if !self.suggestions.is_empty() {
                    let n = self.suggestions.len();
                    self.suggestion = (self.suggestion + n - 1) % n;
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.suggestions = suggest(&self.input);
                self.suggestion = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.suggestions = suggest(&self.input);
                self.suggestion = 0;
            }
            _ => {}
        }
    }

    pub fn run_command(&mut self, input: &str) {
        let mut words = input.split_whitespace();
        let Some(first) = words.next() else {
            return;
        };
        // Unknown words fall back to the best fuzzy match, like k9s.
        let word = if Kind::lookup(first).is_some() || COMMANDS.iter().any(|(c, _)| *c == first) {
            first.to_owned()
        } else {
            match suggest(first).into_iter().next() {
                Some(s) => s.word,
                None => {
                    self.flash(Level::Error, format!("unknown command: {first}"));
                    return;
                }
            }
        };
        match word.as_str() {
            "q" | "quit" => self.quit = true,
            "help" => self.mode = Mode::Help,
            "ctx" => match words.next() {
                Some(name) => match self.find_profile(name) {
                    Some(i) => self.switch_profile(i),
                    None => self.flash(Level::Error, format!("no profile matches {name}")),
                },
                None => self.open_profiles(),
            },
            w => {
                let Some((kind, item_type)) = Kind::lookup(w) else {
                    return;
                };
                match words.collect::<Vec<_>>().join(" ").as_str() {
                    "" => self.goto(kind, item_type, false),
                    "all" => self.goto(kind, item_type, true),
                    name => match self.find_workspace(name) {
                        Ok(ws) => self.goto_in(kind, item_type, ws),
                        Err(e) => self.flash(Level::Error, e),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{fake_app, key, settle, type_str};

    #[test]
    fn exact_and_prefix_rank_first() {
        assert_eq!(suggest("jobs")[0].word, "jobs");
        assert_eq!(suggest("cap")[0].word, "capacities");
        assert_eq!(suggest("note")[0].word, "notebooks");
    }

    #[test]
    fn fuzzy_fallback_and_quit() {
        let mut app = fake_app();
        type_str(&mut app, ":capa");
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::Capacities);
        type_str(&mut app, ":q");
        app.on_key(key(KeyCode::Enter));
        assert!(app.quit);
    }
}
