//! Append-only record of every write: `~/.local/state/fablic/journal.log`,
//! one tab-separated line per action.

use std::io::Write;
use std::path::Path;

use crate::source::Action;

pub fn line(profile: &str, action: &Action, result: &Result<String, String>) -> String {
    let outcome = match result {
        Ok(m) => format!("ok: {m}"),
        Err(e) => format!("error: {e}"),
    };
    let flat = |s: String| s.replace(['\t', '\n'], " ");
    format!(
        "{}\t{profile}\t{}\t{}\t{}\n",
        chrono::Local::now().to_rfc3339(),
        flat(action.describe()),
        flat(action.target()),
        flat(outcome)
    )
}

/// Best effort: a journal that can't be written is logged, never fatal.
pub fn append(dir: Option<&Path>, profile: &str, action: &Action, result: &Result<String, String>) {
    let Some(dir) = dir else {
        return;
    };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("journal.log"))?;
        f.write_all(line(profile, action, result).as_bytes())
    };
    if let Err(e) = write() {
        tracing::warn!("writing journal: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ItemRef;

    #[test]
    fn appends_one_line_per_action() {
        let dir = tempfile::tempdir().unwrap();
        let action = Action::CancelJob {
            workspace: "w".into(),
            item: ItemRef {
                id: "i".into(),
                name: "load".into(),
                item_type: "Notebook".into(),
            },
            job: "j".into(),
        };
        append(Some(dir.path()), "prod", &action, &Ok("done".into()));
        append(Some(dir.path()), "prod", &action, &Err("403\tno".into()));
        let text = std::fs::read_to_string(dir.path().join("journal.log")).unwrap();
        let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0][1..],
            [
                "prod",
                "cancel run j of load",
                "workspace=w item=i job=j",
                "ok: done"
            ]
        );
        assert_eq!(lines[1][4], "error: 403 no");
    }
}
