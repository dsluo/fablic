//! `/` filter syntax: plain substring, `a|b` alternatives, `/regex/`,
//! `~fuzzy`, and a leading `!` to invert any of them. Case-insensitive.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use regex::{Regex, RegexBuilder};

use crate::resources::Row;

pub enum Filter {
    All,
    Substr(Vec<String>, bool),
    Regex(Regex, bool),
    Fuzzy(Pattern, bool),
}

impl Filter {
    /// Parse the filter text. An invalid regex is an error so the prompt can say so.
    pub fn parse(text: &str) -> Result<Filter, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Filter::All);
        }
        let (invert, body) = match text.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        if body.len() >= 2 && body.starts_with('/') && body.ends_with('/') {
            let re = RegexBuilder::new(&body[1..body.len() - 1])
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("bad regex: {e}"))?;
            return Ok(Filter::Regex(re, invert));
        }
        if let Some(f) = body.strip_prefix('~') {
            let p = Pattern::parse(f, CaseMatching::Ignore, Normalization::Smart);
            return Ok(Filter::Fuzzy(p, invert));
        }
        let alts = body
            .split('|')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Filter::Substr(alts, invert))
    }

    /// Indices of rows that pass, in their original order. `extra` is text
    /// the app shows for a row that isn't one of its cells (pipeline run
    /// parameters), matched along with them.
    pub fn apply_with(&self, rows: &[Row], extra: impl Fn(&Row) -> String) -> Vec<usize> {
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        rows.iter()
            .enumerate()
            .filter(|(_, r)| self.matches(r, &extra(r), &mut matcher, &mut buf))
            .map(|(i, _)| i)
            .collect()
    }

    fn matches(&self, row: &Row, extra: &str, m: &mut Matcher, buf: &mut Vec<char>) -> bool {
        let mut hay = row.cells.join(" ");
        if !extra.is_empty() {
            hay.push(' ');
            hay.push_str(extra);
        }
        match self {
            Filter::All => true,
            Filter::Substr(alts, inv) => {
                let lower = hay.to_lowercase();
                alts.iter().any(|a| lower.contains(a)) != *inv
            }
            Filter::Regex(re, inv) => re.is_match(&hay) != *inv,
            Filter::Fuzzy(p, inv) => p.score(Utf32Str::new(&row.name, buf), m).is_some() != *inv,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        [
            "ingest_shop Notebook",
            "bronze_lh Lakehouse",
            "load_all DataPipeline",
        ]
        .iter()
        .map(|s| {
            let mut p = s.split(' ');
            let name = p.next().unwrap();
            Row::new(name, name, vec![name.into(), p.next().unwrap().into()])
        })
        .collect()
    }

    fn run(f: &str) -> Vec<usize> {
        Filter::parse(f)
            .unwrap()
            .apply_with(&rows(), |_| String::new())
    }

    #[test]
    fn substring_and_alternatives() {
        assert_eq!(run(""), vec![0, 1, 2]);
        assert_eq!(run("NOTEBOOK"), vec![0]);
        assert_eq!(run("lake|pipe"), vec![1, 2]);
    }

    #[test]
    fn extra_text_is_searched_too() {
        let f = Filter::parse("orders").unwrap();
        assert!(f.apply_with(&rows(), |_| String::new()).is_empty());
        let extra = |r: &Row| match r.id.as_str() {
            "load_all" => "source_name=orders".to_owned(),
            _ => String::new(),
        };
        assert_eq!(f.apply_with(&rows(), extra), vec![2]);
        let f = Filter::parse("/name=ord/").unwrap();
        assert_eq!(f.apply_with(&rows(), extra), vec![2]);
    }

    #[test]
    fn invert_regex_fuzzy() {
        assert_eq!(run("!notebook"), vec![1, 2]);
        assert_eq!(run("/^b.*lh/"), vec![1]);
        assert_eq!(run("~ldal"), vec![2]);
        assert!(Filter::parse("/(/").is_err());
    }
}
