//! OneLake files and tables: preview, schema, download.

use super::{App, DetailRequest, Effect, Level};
use crate::resources::{Kind, Row};

impl App {
    /// Workspace and item of the current OneLake view, with the selected row.
    fn onelake_target(&self) -> Option<(String, String, Row)> {
        if self.kind() != Kind::OneLake {
            return None;
        }
        let scope = &self.frame.query.scope;
        let ws = scope.workspace.as_ref()?.id.clone();
        let item = scope.item.as_ref()?.id.clone();
        Some((ws, item, self.selected_row()?.clone()))
    }

    /// Enter on a file: the first 64 KB as text.
    pub(super) fn preview(&mut self) {
        let Some((workspace, item, row)) = self.onelake_target() else {
            return;
        };
        let total = row.raw.get("size").and_then(|v| v.as_u64());
        self.open_detail(
            row.id.clone(),
            DetailRequest::Preview {
                workspace,
                item,
                path: row.id,
                total,
            },
        );
    }

    /// `s` on a table folder: its columns.
    pub(super) fn schema(&mut self) {
        let Some((workspace, item, row)) = self.onelake_target() else {
            return;
        };
        if !row.dir || !row.id.starts_with("Tables/") {
            return self.flash(
                Level::Warn,
                "s shows the schema of a table folder under Tables/",
            );
        }
        self.open_detail(
            format!("{} › schema", row.id),
            DetailRequest::Schema {
                workspace,
                item,
                path: row.id,
            },
        );
    }

    /// `D` on a file: save it to the current directory.
    pub(super) fn download(&mut self) {
        let Some((workspace, item, row)) = self.onelake_target() else {
            return self.flash(Level::Warn, "D downloads a file in a OneLake view");
        };
        if row.dir {
            return self.flash(Level::Warn, "D downloads files, not folders");
        }
        self.flash(Level::Info, format!("downloading {}…", row.name));
        self.effects.push(Effect::Download {
            profile: self.profile.account.clone(),
            workspace,
            item,
            path: row.id,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle};
    use crate::app::{App, Effect, Mode};
    use crate::resources::Kind;
    use crossterm::event::KeyCode;

    fn press(app: &mut App, c: KeyCode) {
        app.on_key(key(c));
        settle(app);
    }

    /// Urban1 Bronze › bronze_lh, at the lakehouse root (Files, Tables).
    fn at_lakehouse() -> App {
        let mut app = fake_app();
        for k in [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Enter,
        ] {
            press(&mut app, k);
        }
        assert_eq!(app.kind(), Kind::OneLake);
        app
    }

    fn names(app: &App) -> Vec<String> {
        app.view.iter().map(|&i| app.rows[i].name.clone()).collect()
    }

    #[test]
    fn browse_files_dirs_first_then_preview() {
        let mut app = at_lakehouse();
        assert_eq!(names(&app), ["Files", "Tables"]);
        press(&mut app, KeyCode::Enter); // Files/
        assert_eq!(names(&app), ["raw", "extract.parquet", "readme.txt"]);
        press(&mut app, KeyCode::Char('G')); // readme.txt
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Detail);
        assert_eq!(app.detail.lines[0], "── Files/readme.txt ──");
        assert_eq!(app.detail.lines[1], "Bronze landing zone.");
    }

    #[test]
    fn binary_files_point_at_download() {
        let mut app = at_lakehouse();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('j')); // extract.parquet
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.detail.lines[1],
            "(binary file, 8.8 KB; press D to download it)"
        );
    }

    #[test]
    fn schema_of_a_table_folder() {
        let mut app = at_lakehouse();
        press(&mut app, KeyCode::Char('j')); // Tables
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('s')); // customers
        assert_eq!(app.mode, Mode::Detail);
        assert_eq!(app.detail.title, "Tables/customers › schema");
        assert_eq!(app.detail.lines[0], "── dbo.customers ──");
        assert!(app.detail.lines[2].starts_with("id "));
    }

    #[test]
    fn download_files_only() {
        let mut app = at_lakehouse();
        app.on_key(key(KeyCode::Char('D'))); // Files is a folder
        assert!(app.status.as_ref().unwrap().text.contains("not folders"));
        press(&mut app, KeyCode::Enter);
        app.on_key(key(KeyCode::Char('G')));
        app.on_key(key(KeyCode::Char('D')));
        let e = app.take_effects();
        assert!(matches!(&e[..], [Effect::Download { path, .. }] if path == "Files/readme.txt"));
        // fake downloads report a size without writing
        app.effects = e;
        settle(&mut app);
        let status = &app.status.as_ref().unwrap().text;
        assert!(
            status.starts_with("saved readme.txt (57 B) to "),
            "{status}"
        );
    }
}
