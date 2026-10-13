//! Pipeline run parameters, toggled with `P`. Fabric only has them behind
//! undocumented endpoints, so they're off until asked for. The jobs list
//! shows as soon as it's fetched; each pipeline run's parameters are then
//! fetched on their own and fill in as they arrive.

use serde_json::Value;

use super::{App, Effect, Level, Mode};
use crate::resources::{Kind, Row};

/// Spinner frames for a cell whose parameters are still on their way.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What is known about one run's parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum Params {
    Loading,
    Ready(Value),
    Failed(String),
}

/// A run that has (or will have) parameters: a pipeline run that started.
fn has_parameters(row: &Row) -> bool {
    row.field("jobType") == Some("Pipeline") && row.field("status") != Some("NotStarted")
}

impl App {
    /// `P`: show or hide the PARAMETERS column (and fetch what's missing).
    pub(super) fn toggle_parameters(&mut self) {
        self.params_on = !self.params_on;
        // Loaded parameters are searched only while shown.
        if !self.frame.filter.is_empty() {
            self.apply_filter();
        }
        if self.params_on {
            self.flash(
                Level::Info,
                "pipeline run parameters on (uses undocumented Fabric endpoints)",
            );
            self.want_parameters();
        } else {
            self.flash(Level::Info, "pipeline run parameters off");
        }
    }

    /// Whether the table shows the PARAMETERS column.
    pub fn show_parameters(&self) -> bool {
        self.params_on && self.kind() == Kind::Jobs
    }

    /// Ask for the parameters of every pipeline run on screen that isn't
    /// known or on its way already.
    pub(super) fn want_parameters(&mut self) {
        if !self.show_parameters() {
            return;
        }
        let scope_ws = self
            .frame
            .query
            .scope
            .workspace
            .as_ref()
            .map(|w| w.id.clone());
        let account = self.profile.account.clone();
        let wanted: Vec<(String, String)> = self
            .rows
            .iter()
            .filter(|r| has_parameters(r))
            .filter(|r| !self.params.contains_key(&(account.clone(), r.id.clone())))
            .filter_map(|r| {
                let ws = scope_ws
                    .clone()
                    .or_else(|| r.field("workspaceId").map(str::to_owned))?;
                Some((ws, r.id.clone()))
            })
            .collect();
        for (workspace, run) in wanted {
            self.params
                .insert((account.clone(), run.clone()), Params::Loading);
            self.effects.push(Effect::Parameters {
                profile: account.clone(),
                workspace,
                run,
            });
        }
    }

    pub(super) fn on_parameters(
        &mut self,
        profile: String,
        run: String,
        result: Result<Value, String>,
    ) {
        let params = match result {
            Ok(v) => Params::Ready(v),
            Err(e) => {
                tracing::warn!(%run, "pipeline run parameters: {e}");
                Params::Failed(e)
            }
        };
        let current = profile == self.profile.account;
        self.params.insert((profile, run.clone()), params);
        // A filter may match the new text.
        if current && !self.frame.filter.is_empty() {
            self.apply_filter();
        }
        if current && self.mode == Mode::Detail && self.detail.run.as_deref() == Some(&run) {
            self.detail.lines = self.run_detail_lines(&run);
        }
    }

    /// `ctrl-r`: let failed lookups be tried again when the rows come back.
    /// Only a refresh asked for by hand, so live refreshes don't retry
    /// every few seconds.
    pub(super) fn forget_failed_parameters(&mut self) {
        let account = &self.profile.account;
        self.params
            .retain(|(a, _), p| a != account || !matches!(p, Params::Failed(_)));
    }

    /// The parameters known for `row`, if it's a run that has any.
    pub fn parameters_of(&self, row: &Row) -> Option<&Params> {
        if !has_parameters(row) {
            return None;
        }
        self.params
            .get(&(self.profile.account.clone(), row.id.clone()))
    }

    /// The PARAMETERS cell: `name=value, …`, a spinner while loading.
    pub fn parameters_cell(&self, row: &Row) -> String {
        match self.parameters_of(row) {
            None => String::new(),
            Some(Params::Loading) => SPINNER[self.spinner % SPINNER.len()].to_owned(),
            Some(Params::Failed(_)) => "(failed; see describe)".into(),
            Some(Params::Ready(v)) => inline(v),
        }
    }

    /// What the filter searches besides a row's cells: its loaded
    /// parameters, while the column is shown.
    pub fn parameters_text(&self, row: &Row) -> String {
        match self.parameters_of(row) {
            Some(Params::Ready(v)) if self.show_parameters() => inline(v),
            _ => String::new(),
        }
    }

    /// Keep redrawing for the spinner while parameters are on their way.
    pub fn animating(&self) -> bool {
        self.show_parameters() && self.params.values().any(|p| *p == Params::Loading)
    }

    pub fn on_frame(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// Describe for a run: its JSON as YAML, then, for a pipeline run whose
    /// parameters are on or known, a `---` document with the parameters.
    pub(super) fn run_detail_lines(&self, run: &str) -> Vec<String> {
        let Some(row) = self.rows.iter().find(|r| r.id == run) else {
            return Vec::new();
        };
        let mut text = serde_yaml::to_string(&row.raw).unwrap_or_else(|e| e.to_string());
        let params = match self.parameters_of(row) {
            Some(Params::Ready(v)) => {
                Some(serde_yaml::to_string(v).unwrap_or_else(|e| e.to_string()))
            }
            Some(Params::Loading) => Some("# loading parameters…\n".into()),
            Some(Params::Failed(e)) => Some(format!("# parameters failed: {e}\n")),
            None if self.params_on && has_parameters(row) => {
                Some("# parameters not fetched yet\n".into())
            }
            None => None,
        };
        if let Some(p) = params {
            text.push_str("---\n");
            text.push_str(&p);
        }
        text.lines().map(str::to_owned).collect()
    }
}

/// `{"a": "1", "b": 2}` as `a=1, b=2`.
fn inline(v: &Value) -> String {
    let Some(map) = v.as_object() else {
        return v.to_string();
    };
    map.iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// The fake tenant's Urban2 Silver workspace, which has notebooks and a pipeline.
    fn workspace_jobs() -> App {
        let mut app = fake_app();
        type_str(&mut app, "/silver");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.frame.query.scope.label(), "Urban2 Silver");
        type_str(&mut app, ":jobs");
        app.on_key(key(KeyCode::Enter));
        settle(&mut app);
        assert_eq!(app.kind(), Kind::Jobs);
        app
    }

    fn pipeline_row(app: &App) -> Row {
        app.rows
            .iter()
            .find(|r| has_parameters(r))
            .expect("a pipeline run")
            .clone()
    }

    #[test]
    fn off_by_default_and_fetched_per_run_when_toggled() {
        let mut app = workspace_jobs();
        assert!(!app.show_parameters());
        assert!(app.params.is_empty(), "nothing asked for while off");

        app.on_key(key(KeyCode::Char('P')));
        assert!(app.show_parameters());
        // The list is already there; each pipeline run's lookup is queued.
        let row = pipeline_row(&app);
        assert!(!app.rows.is_empty());
        assert_eq!(app.parameters_of(&row), Some(&Params::Loading));
        assert!(SPINNER.contains(&app.parameters_cell(&row).as_str()));
        assert!(app.animating());
        let before = app.parameters_cell(&row);
        app.on_frame();
        assert_ne!(app.parameters_cell(&row), before, "the spinner turns");

        settle(&mut app);
        assert!(!app.animating());
        assert!(app.parameters_cell(&row).contains("source_name=orders"));
        // Notebook runs have none.
        let notebook = app
            .rows
            .iter()
            .find(|r| r.field("jobType") == Some("RunNotebook"));
        if let Some(nb) = notebook {
            assert_eq!(app.parameters_cell(nb), "");
        }
    }

    #[test]
    fn known_runs_are_not_fetched_again() {
        let mut app = workspace_jobs();
        app.on_key(key(KeyCode::Char('P')));
        settle(&mut app);
        app.refresh(true);
        settle(&mut app);
        app.want_parameters();
        assert!(
            !app.take_effects()
                .iter()
                .any(|e| matches!(e, Effect::Parameters { .. }))
        );
    }

    #[test]
    fn describe_puts_parameters_after_a_document_separator() {
        let mut app = workspace_jobs();
        let row = pipeline_row(&app);
        let at = app
            .view
            .iter()
            .position(|&i| app.rows[i].id == row.id)
            .unwrap();
        app.table.select(Some(at));

        // Off: just the run.
        app.on_key(key(KeyCode::Char('d')));
        assert!(!app.detail.lines.iter().any(|l| l == "---"));
        app.on_key(key(KeyCode::Esc));

        app.on_key(key(KeyCode::Char('P')));
        app.on_key(key(KeyCode::Char('d')));
        let sep = app.detail.lines.iter().position(|l| l == "---").unwrap();
        assert_eq!(app.detail.lines[sep + 1], "# loading parameters…");
        let raw = serde_yaml::to_string(&row.raw).unwrap();
        assert_eq!(app.detail.lines[..sep].join("\n"), raw.trim_end());

        // Fills in when the answer comes, without leaving the view.
        settle(&mut app);
        assert_eq!(app.mode, Mode::Detail);
        let after = &app.detail.lines[sep + 1..];
        assert!(
            after.contains(&"source_name: orders".to_owned()),
            "{after:?}"
        );
    }

    #[test]
    fn the_filter_searches_loaded_parameters() {
        let mut app = workspace_jobs();
        type_str(&mut app, "/load_mode=full");
        app.on_key(key(KeyCode::Enter));
        assert!(app.view.is_empty(), "off: parameters aren't searched");

        // Answers arriving re-run the filter.
        app.on_key(key(KeyCode::Char('P')));
        settle(&mut app);
        assert!(!app.view.is_empty());
        for &i in &app.view {
            assert!(app.parameters_cell(&app.rows[i]).contains("load_mode=full"));
        }

        // Hidden again: not searched.
        app.on_key(key(KeyCode::Char('P')));
        assert!(app.view.is_empty());
    }

    #[test]
    fn a_failed_lookup_says_so() {
        let mut app = workspace_jobs();
        app.on_key(key(KeyCode::Char('P')));
        let row = pipeline_row(&app);
        let account = app.profile.account.clone();
        assert_eq!(app.parameters_text(&row), "", "nothing to search yet");
        // The lookup's own answer first, so the failure below stays.
        settle(&mut app);
        app.on_msg(Msg::Parameters {
            profile: account,
            run: row.id.clone(),
            result: Err("401 /metadata/v201606/generatemwctoken".into()),
        });
        assert_eq!(app.parameters_cell(&row), "(failed; see describe)");

        // A live refresh leaves it; ctrl-r tries again.
        app.refresh(true);
        settle(&mut app);
        assert_eq!(app.parameters_cell(&row), "(failed; see describe)");
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        settle(&mut app);
        assert!(app.parameters_cell(&row).contains("source_name=orders"));
    }
}
