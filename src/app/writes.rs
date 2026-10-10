//! Writes other than jobs: rename, describe, delete, folders, moves, roles,
//! shortcuts. Each passes the read-only guard first, then either a typed
//! prompt (the prompt is the confirmation) or the y/N confirm.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::navigation::{folder_ref, item_ref, workspace_of};
use super::{App, Effect, Level, Mode};
use crate::resources::{CREATABLE, Kind, creatable_matches};
use crate::source::{Action, ItemRef, Movable, ROLES, Ref};

/// What a typed prompt is collecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    RenameItem {
        workspace: String,
        item: ItemRef,
    },
    DescribeItem {
        workspace: String,
        item: ItemRef,
    },
    /// First step of `n`: a folder or which item type.
    NewType {
        workspace: String,
        parent: Option<Ref>,
    },
    /// Second step: its name.
    NewName {
        workspace: String,
        parent: Option<Ref>,
        item_type: String,
    },
    RenameFolder {
        workspace: String,
        folder: Ref,
    },
    /// First step of adding a role: who.
    Principal {
        workspace: String,
    },
    /// A role for a new principal, or a new role for an assignment.
    Role {
        workspace: String,
        principal: Option<String>,
        assignment: Option<Ref>,
    },
    /// A new name for a profile (local: not a Fabric write).
    RenameProfile {
        account: String,
    },
    /// The action runs once `name` is typed exactly.
    Delete {
        action: Action,
        name: String,
    },
}

#[derive(Debug, Clone)]
pub struct Prompt {
    /// What happens on enter, e.g. `rename Notebook load_all`.
    pub title: String,
    /// The line under the input.
    pub hint: String,
    pub purpose: Purpose,
    /// Highlighted entry of `choices`, for prompts that pick from a list.
    pub choice: usize,
    /// The folder a new thing lands in, which ends `title` and is drawn
    /// apart from the rest of it.
    pub place: Option<String>,
}

impl Prompt {
    /// Everything a picking prompt can offer, regardless of the filter.
    pub fn all_choices(&self) -> Option<&'static [&'static str]> {
        match self.purpose {
            Purpose::NewType { .. } => Some(CREATABLE),
            Purpose::Role { .. } => Some(&ROLES),
            _ => None,
        }
    }

    /// What's left to pick from after filtering by `input`.
    pub fn choices(&self, input: &str) -> Option<Vec<&'static str>> {
        match self.purpose {
            Purpose::NewType { .. } => Some(creatable_matches(input)),
            Purpose::Role { .. } => {
                let t = input.trim().to_lowercase();
                Some(
                    ROLES
                        .iter()
                        .copied()
                        .filter(|r| r.to_lowercase().starts_with(&t))
                        .collect(),
                )
            }
            _ => None,
        }
    }
}

/// The hint under a picking prompt.
const PICK_HINT: &str = "type to filter · ↑↓ choose · enter picks · esc cancels";

/// `m` / `p`: what is being moved, and from which workspace.
#[derive(Debug, Clone)]
pub struct Moving {
    pub workspace: String,
    pub what: Movable,
}

fn place(folder: &Option<Ref>) -> String {
    match folder {
        Some(f) => f.name.clone(),
        None => "the workspace root".into(),
    }
}

impl App {
    /// Refuse writes on a read-only profile. True when writing is allowed.
    pub(super) fn writable(&mut self, what: &str) -> bool {
        if self.profile.readonly {
            self.flash(
                Level::Error,
                format!("{} is read-only: won't {what}", self.profile.name),
            );
        }
        !self.profile.readonly
    }

    /// Ask y/N before a write.
    pub(super) fn confirm(&mut self, action: Action) {
        if self.writable(&action.describe()) {
            self.pending = Some(action);
            self.mode = Mode::Confirm;
        }
    }

    fn ask(&mut self, title: String, hint: &str, input: &str, purpose: Purpose) {
        if !self.writable(&title) {
            return;
        }
        self.input = input.to_owned();
        self.prompt = Some(Prompt {
            title,
            hint: hint.to_owned(),
            purpose,
            choice: 0,
            place: None,
        });
        self.mode = Mode::Prompt;
    }

    /// Hand a confirmed write to the event loop.
    pub(super) fn act(&mut self, action: Action) {
        self.flash(Level::Info, format!("{}…", action.describe()));
        self.effects.push(Effect::Act {
            profile: self.profile.account.clone(),
            action,
        });
    }

    /// The selected item row and its workspace id (items views only).
    fn selected_item(&self) -> Option<(String, ItemRef)> {
        if self.kind() != Kind::Items {
            return None;
        }
        let row = self.selected_row().filter(|r| !r.dir)?;
        let ws = workspace_of(&self.frame.query.scope, row).id;
        Some((ws, item_ref(row)))
    }

    /// The selected folder row and the view's workspace id.
    fn selected_folder(&self) -> Option<(String, Ref)> {
        if self.kind() != Kind::Items {
            return None;
        }
        let row = self.selected_row().filter(|r| r.dir)?;
        let ws = self.frame.query.scope.workspace.as_ref()?.id.clone();
        Some((ws, folder_ref(row)))
    }

    fn view_workspace(&self) -> Option<String> {
        Some(self.frame.query.scope.workspace.as_ref()?.id.clone())
    }

    /// `e`: rename an item or folder; in roles, change the role.
    pub(super) fn rename(&mut self) {
        if self.kind() == Kind::Roles {
            return self.change_role();
        }
        if let Some((workspace, folder)) = self.selected_folder() {
            let current = folder.name.rsplit('/').next().unwrap_or("").to_owned();
            let title = format!("rename folder {}", folder.name);
            return self.ask(
                title,
                "enter renames · esc cancels",
                &current,
                Purpose::RenameFolder { workspace, folder },
            );
        }
        let Some((workspace, item)) = self.selected_item() else {
            return self.flash(
                Level::Warn,
                "e renames an item or folder, or changes a role",
            );
        };
        let title = format!("rename {} {}", item.item_type, item.name);
        let current = item.name.clone();
        self.ask(
            title,
            "enter renames · esc cancels",
            &current,
            Purpose::RenameItem { workspace, item },
        );
    }

    /// `E`: edit an item's description.
    pub(super) fn edit_description(&mut self) {
        let Some((workspace, item)) = self.selected_item() else {
            return self.flash(Level::Warn, "E edits the description of an item");
        };
        let current = self
            .selected_row()
            .and_then(|r| r.field("description"))
            .unwrap_or("")
            .to_owned();
        let title = format!("description of {} {}", item.item_type, item.name);
        self.ask(
            title,
            "enter saves (empty clears it) · esc cancels",
            &current,
            Purpose::DescribeItem { workspace, item },
        );
    }

    /// `n`: a new folder or item in the folder on screen. A view of one
    /// item type (`:nb`) skips straight to the name.
    pub(super) fn create(&mut self) {
        let (Kind::Items, Some(workspace)) = (self.kind(), self.view_workspace()) else {
            return self.flash(Level::Warn, "n creates things in a workspace's items");
        };
        let parent = self.frame.query.scope.folder.clone();
        match self.frame.query.item_type.clone() {
            Some(t) if CREATABLE.contains(&t.as_str()) => self.ask_name(workspace, parent, t),
            _ => {
                let (title, at) = (format!("new … in {}", place(&parent)), place(&parent));
                self.ask(title, PICK_HINT, "", Purpose::NewType { workspace, parent });
                self.mark_place(at);
            }
        }
    }

    fn ask_name(&mut self, workspace: String, parent: Option<Ref>, item_type: String) {
        let at = place(&parent);
        let title = format!("new {item_type} in {at}");
        self.ask(
            title,
            "its name · enter creates · esc cancels",
            "",
            Purpose::NewName {
                workspace,
                parent,
                item_type,
            },
        );
        self.mark_place(at);
    }

    fn mark_place(&mut self, at: String) {
        if let Some(p) = &mut self.prompt {
            p.place = Some(at);
        }
    }

    /// `ctrl-d`: delete. Items and folders need their name typed.
    pub(super) fn delete(&mut self) {
        let scope = self.frame.query.scope.clone();
        match self.kind() {
            Kind::Items => {
                if let Some((workspace, folder)) = self.selected_folder() {
                    let name = folder.name.rsplit('/').next().unwrap_or("").to_owned();
                    let action = Action::DeleteFolder { workspace, folder };
                    return self.ask_delete(action, name);
                }
                if let Some((workspace, item)) = self.selected_item() {
                    let name = item.name.clone();
                    return self.ask_delete(Action::DeleteItem { workspace, item }, name);
                }
            }
            Kind::Roles => {
                if let (Some(workspace), Some(row)) = (self.view_workspace(), self.selected_row()) {
                    let assignment = Ref {
                        id: row.id.clone(),
                        name: row.name.clone(),
                    };
                    return self.confirm(Action::RemoveRole {
                        workspace,
                        assignment,
                    });
                }
            }
            Kind::Shortcuts => {
                if let (Some(ws), Some(item), Some(row)) =
                    (&scope.workspace, &scope.item, self.selected_row())
                {
                    let action = Action::DeleteShortcut {
                        workspace: ws.id.clone(),
                        item: item.clone(),
                        path: row.field("path").unwrap_or("").to_owned(),
                        name: row.name.clone(),
                    };
                    return self.confirm(action);
                }
            }
            _ => {}
        }
        self.flash(
            Level::Warn,
            "ctrl-d deletes an item, folder, role assignment or shortcut",
        );
    }

    fn ask_delete(&mut self, action: Action, name: String) {
        let title = action.describe();
        let hint = format!("type {name} and press enter to delete it · esc cancels");
        self.ask(title, &hint, "", Purpose::Delete { action, name });
    }

    /// `m`: pick up the selected item or folder (again to put it down).
    pub(super) fn mark_move(&mut self) {
        let picked = match (self.selected_folder(), self.selected_item()) {
            (Some((ws, f)), _) => (ws, Movable::Folder(f)),
            (None, Some((ws, i))) => (ws, Movable::Item(i)),
            _ => return self.flash(Level::Warn, "m picks up an item or folder to move"),
        };
        if self
            .moving
            .as_ref()
            .is_some_and(|m| m.workspace == picked.0 && m.what == picked.1)
        {
            self.moving = None;
            return self.flash(Level::Info, "put it back");
        }
        if !self.writable("move things") {
            return;
        }
        self.flash(
            Level::Info,
            format!(
                "moving {}: open the destination folder and press p",
                picked.1.describe()
            ),
        );
        self.moving = Some(Moving {
            workspace: picked.0,
            what: picked.1,
        });
    }

    /// `p`: move what `m` picked up into the folder on screen.
    pub(super) fn paste(&mut self) {
        let Some(moving) = self.moving.clone() else {
            return self.flash(Level::Warn, "pick something up with m first");
        };
        let here = self.view_workspace();
        if self.kind() != Kind::Items || here.as_deref() != Some(moving.workspace.as_str()) {
            return self.flash(
                Level::Warn,
                "p moves into a folder of the same workspace's items",
            );
        }
        let to = self.frame.query.scope.folder.clone();
        if let (Movable::Folder(f), Some(t)) = (&moving.what, &to)
            && (t.id == f.id || t.name.starts_with(&format!("{}/", f.name)))
        {
            return self.flash(Level::Warn, "a folder can't move into itself");
        }
        self.confirm(Action::Move {
            workspace: moving.workspace,
            what: moving.what,
            to,
        });
    }

    /// `a` in roles: give someone a role.
    pub(super) fn add_role(&mut self) {
        let (Kind::Roles, Some(workspace)) = (self.kind(), self.view_workspace()) else {
            return self.flash(Level::Warn, "a adds a role assignment in a roles view");
        };
        self.ask(
            "add someone to the workspace".into(),
            "an email, an object id, or an exact group or app name",
            "",
            Purpose::Principal { workspace },
        );
    }

    fn change_role(&mut self) {
        let (Some(workspace), Some(row)) = (self.view_workspace(), self.selected_row()) else {
            return;
        };
        let assignment = Ref {
            id: row.id.clone(),
            name: row.name.clone(),
        };
        let current = ROLES
            .iter()
            .position(|r| Some(*r) == row.field("role"))
            .unwrap_or(0);
        let title = format!("new role for {}", assignment.name);
        self.ask(
            title,
            PICK_HINT,
            "",
            Purpose::Role {
                workspace,
                principal: None,
                assignment: Some(assignment),
            },
        );
        if let Some(p) = &mut self.prompt {
            p.choice = current;
        }
    }

    pub(super) fn key_prompt(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                let back = self.prompt_returns_to();
                self.prompt = None;
                self.input.clear();
                self.mode = back;
                self.flash(Level::Info, "cancelled");
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Up => return self.move_choice(-1),
            KeyCode::Down | KeyCode::Tab => return self.move_choice(1),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear()
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => return,
        }
        // The filter changed: start again from the best match.
        if let Some(p) = &mut self.prompt {
            p.choice = 0;
        }
    }

    fn move_choice(&mut self, by: isize) {
        let Some(p) = &mut self.prompt else {
            return;
        };
        let n = p.choices(&self.input).map_or(0, |c| c.len());
        if n > 0 {
            p.choice = (p.choice as isize + by).rem_euclid(n as isize) as usize;
        }
    }

    /// The highlighted choice of a picking prompt.
    fn picked(&self) -> Option<&'static str> {
        let p = self.prompt.as_ref()?;
        p.choices(&self.input)?.get(p.choice).copied()
    }

    fn submit(&mut self) {
        let Some(prompt) = self.prompt.clone() else {
            self.mode = Mode::Table;
            return;
        };
        let text = self.input.trim().to_owned();
        let need = |what: &str| format!("{what} can't be empty");
        let action = match prompt.purpose {
            Purpose::RenameItem { workspace, item } => {
                if text.is_empty() {
                    return self.flash(Level::Warn, need("the name"));
                }
                if text == item.name {
                    return self.close_prompt("unchanged");
                }
                Action::RenameItem {
                    workspace,
                    item,
                    name: text,
                }
            }
            Purpose::DescribeItem { workspace, item } => Action::DescribeItem {
                workspace,
                item,
                description: text,
            },
            Purpose::NewType { workspace, parent } => {
                let Some(item_type) = self.picked() else {
                    return self.flash(Level::Warn, "nothing matches");
                };
                self.prompt = None;
                return self.ask_name(workspace, parent, item_type.to_owned());
            }
            Purpose::NewName {
                workspace,
                parent,
                item_type,
            } => {
                if text.is_empty() {
                    return self.flash(Level::Warn, need("the name"));
                }
                if item_type == "Folder" {
                    Action::NewFolder {
                        workspace,
                        parent,
                        name: text,
                    }
                } else {
                    Action::NewItem {
                        workspace,
                        folder: parent,
                        item_type,
                        name: text,
                    }
                }
            }
            Purpose::RenameFolder { workspace, folder } => {
                if text.is_empty() {
                    return self.flash(Level::Warn, need("the name"));
                }
                Action::RenameFolder {
                    workspace,
                    folder,
                    name: text,
                }
            }
            Purpose::Principal { workspace } => {
                if text.is_empty() {
                    return self.flash(Level::Warn, need("who"));
                }
                self.input.clear();
                self.prompt = Some(Prompt {
                    title: format!("role for {text}"),
                    hint: PICK_HINT.into(),
                    choice: 0,
                    place: None,
                    purpose: Purpose::Role {
                        workspace,
                        principal: Some(text),
                        assignment: None,
                    },
                });
                return;
            }
            Purpose::Role {
                workspace,
                principal,
                assignment,
            } => {
                let Some(role) = self.picked() else {
                    return self.flash(Level::Warn, format!("pick one of {}", ROLES.join(", ")));
                };
                let role = role.to_owned();
                match (principal, assignment) {
                    (_, Some(assignment)) => Action::SetRole {
                        workspace,
                        assignment,
                        role,
                    },
                    (Some(principal), None) => Action::AddRole {
                        workspace,
                        principal,
                        role,
                    },
                    (None, None) => return self.close_prompt("nothing to do"),
                }
            }
            Purpose::RenameProfile { account } => {
                if text.is_empty() {
                    return self.flash(Level::Warn, need("the name"));
                }
                // Stay in the prompt to try another name.
                if self
                    .profiles
                    .iter()
                    .any(|p| p.name == text && p.account != account)
                {
                    return self.flash(Level::Warn, format!("another profile is named {text}"));
                }
                self.close_prompt("");
                return self.apply_profile_name(&account, text);
            }
            Purpose::Delete { action, name } => {
                if self.input != name {
                    return self.flash(Level::Warn, format!("type {name} exactly to delete it"));
                }
                action
            }
        };
        self.close_prompt("");
        self.act(action);
    }

    /// Profile renames go back to the picker; everything else to the table.
    fn prompt_returns_to(&self) -> Mode {
        match self.prompt.as_ref().map(|p| &p.purpose) {
            Some(Purpose::RenameProfile { .. }) => Mode::Profiles,
            _ => Mode::Table,
        }
    }

    fn close_prompt(&mut self, note: &str) {
        let back = self.prompt_returns_to();
        self.prompt = None;
        self.input.clear();
        self.mode = back;
        if !note.is_empty() {
            self.flash(Level::Info, note);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{fake_app, key, settle, type_str};
    use crate::app::{App, Effect, Mode, ProfileInfo};
    use crate::resources::Kind;
    use crate::source::{Action, Movable};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(app: &mut App, c: KeyCode) {
        app.on_key(key(c));
        settle(app);
    }

    fn ctrl(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    /// The writes queued so far.
    fn acts(app: &mut App) -> Vec<Action> {
        app.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Act { action, .. } => Some(action),
                _ => None,
            })
            .collect()
    }

    /// Urban1 Bronze root: ingest/, bronze_lh, Finance Model, transform_silver.
    fn bronze() -> App {
        let mut app = fake_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Items);
        app
    }

    #[test]
    fn rename_item_prefills_and_submits() {
        let mut app = bronze();
        press(&mut app, KeyCode::Char('G')); // transform_silver
        app.on_key(key(KeyCode::Char('e')));
        assert_eq!(app.mode, Mode::Prompt);
        assert_eq!(app.input, "transform_silver");
        ctrl(&mut app, 'u');
        type_str(&mut app, "silver_nb");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Table);
        let a = acts(&mut app);
        assert!(
            matches!(&a[..], [Action::RenameItem { item, name, .. }]
                if item.name == "transform_silver" && name == "silver_nb"),
            "{a:?}"
        );
    }

    #[test]
    fn delete_needs_the_exact_name() {
        let mut app = bronze();
        press(&mut app, KeyCode::Char('G'));
        ctrl(&mut app, 'd');
        assert_eq!(app.mode, Mode::Prompt);
        type_str(&mut app, "transform");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Prompt, "a partial name must not delete");
        assert!(acts(&mut app).is_empty());
        type_str(&mut app, "_silver");
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        assert!(matches!(&a[..], [Action::DeleteItem { item, .. }] if item.id == "it-5"));
    }

    #[test]
    fn esc_cancels_a_prompt() {
        let mut app = bronze();
        ctrl(&mut app, 'd'); // the ingest folder
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Table);
        assert!(app.prompt.is_none());
        assert!(acts(&mut app).is_empty());
        assert_eq!(app.kind(), Kind::Items, "esc in a prompt doesn't go back");
    }

    #[test]
    fn folders_delete_and_create_in_place() {
        let mut app = bronze();
        ctrl(&mut app, 'd');
        type_str(&mut app, "ingest");
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        assert!(matches!(&a[..], [Action::DeleteFolder { folder, .. }] if folder.id == "f-1"));

        press(&mut app, KeyCode::Enter); // into ingest
        app.on_key(key(KeyCode::Char('n')));
        type_str(&mut app, "fol");
        app.on_key(key(KeyCode::Enter));
        type_str(&mut app, "staging");
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        let [Action::NewFolder { parent, name, .. }] = &a[..] else {
            panic!("{a:?}");
        };
        assert_eq!(parent.as_ref().map(|p| p.id.as_str()), Some("f-1"));
        assert_eq!(name, "staging");
    }

    #[test]
    fn new_item_picks_a_type_then_a_name() {
        let mut app = bronze();
        press(&mut app, KeyCode::Enter); // into ingest
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.prompt.as_ref().unwrap().title, "new … in ingest");
        type_str(&mut app, "E");
        app.on_key(key(KeyCode::Backspace));
        let all = app.prompt.as_ref().unwrap().choices(&app.input).unwrap();
        assert_eq!(all.len(), crate::resources::CREATABLE.len());
        type_str(&mut app, "lh");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.prompt.as_ref().unwrap().title,
            "new Lakehouse in ingest"
        );
        type_str(&mut app, "silver_lh");
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        let [
            Action::NewItem {
                folder,
                item_type,
                name,
                ..
            },
        ] = &a[..]
        else {
            panic!("{a:?}");
        };
        assert_eq!(folder.as_ref().map(|f| f.id.as_str()), Some("f-1"));
        assert_eq!(
            (item_type.as_str(), name.as_str()),
            ("Lakehouse", "silver_lh")
        );
    }

    #[test]
    fn arrows_pick_among_filtered_types_and_type_views_skip_it() {
        let mut app = bronze();
        app.on_key(key(KeyCode::Char('n')));
        type_str(&mut app, "e");
        let p = app.prompt.as_ref().unwrap();
        assert_eq!(
            p.choices(&app.input).unwrap(),
            ["Environment", "Eventhouse", "Eventstream"]
        );
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down)); // wraps to the top
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.prompt.as_ref().unwrap().title,
            "new Eventhouse in the workspace root"
        );
        app.on_key(key(KeyCode::Esc));

        type_str(&mut app, ":nb");
        press(&mut app, KeyCode::Enter);
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(
            app.prompt.as_ref().unwrap().title,
            "new Notebook in the workspace root"
        );
    }

    #[test]
    fn pick_up_and_drop_into_a_folder() {
        let mut app = bronze();
        press(&mut app, KeyCode::Char('G')); // transform_silver
        app.on_key(key(KeyCode::Char('m')));
        assert!(app.moving.is_some());
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Enter); // into ingest
        app.on_key(key(KeyCode::Char('p')));
        assert_eq!(app.mode, Mode::Confirm);
        app.on_key(key(KeyCode::Char('y')));
        let a = acts(&mut app);
        let [Action::Move { what, to, .. }] = &a[..] else {
            panic!("{a:?}");
        };
        assert!(matches!(what, Movable::Item(i) if i.id == "it-5"));
        assert_eq!(to.as_ref().map(|t| t.name.as_str()), Some("ingest"));
        app.effects = vec![Effect::Act {
            profile: app.profile.account.clone(),
            action: a[0].clone(),
        }];
        settle(&mut app);
        assert!(app.moving.is_none(), "a done move puts nothing back down");
    }

    #[test]
    fn a_folder_cannot_move_into_itself() {
        let mut app = bronze();
        app.on_key(key(KeyCode::Char('m'))); // ingest
        press(&mut app, KeyCode::Enter); // ingest
        press(&mut app, KeyCode::Enter); // ingest/shop
        app.on_key(key(KeyCode::Char('p')));
        assert_eq!(app.mode, Mode::Table);
        assert!(app.status.as_ref().unwrap().text.contains("into itself"));
    }

    #[test]
    fn roles_add_change_remove() {
        let mut app = bronze();
        type_str(&mut app, ":roles");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Roles);

        app.on_key(key(KeyCode::Char('a')));
        type_str(&mut app, "jane@x.org");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Prompt, "then asks for the role");
        type_str(&mut app, "c");
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        assert!(
            matches!(&a[..], [Action::AddRole { principal, role, .. }]
                if principal == "jane@x.org" && role == "Contributor"),
            "{a:?}"
        );

        app.on_key(key(KeyCode::Char('e')));
        assert_eq!(app.prompt.as_ref().unwrap().choice, 0, "starts on Admin");
        app.on_key(key(KeyCode::Up)); // wraps to Viewer
        app.on_key(key(KeyCode::Enter));
        let a = acts(&mut app);
        assert!(matches!(&a[..], [Action::SetRole { role, .. }] if role == "Viewer"));

        ctrl(&mut app, 'd');
        assert_eq!(app.mode, Mode::Confirm);
        app.on_key(key(KeyCode::Char('y')));
        let a = acts(&mut app);
        assert!(
            matches!(&a[..], [Action::RemoveRole { assignment, .. }] if assignment.id == "r-1")
        );
    }

    #[test]
    fn delete_a_shortcut() {
        let mut app = bronze();
        press(&mut app, KeyCode::Char('j')); // bronze_lh
        type_str(&mut app, ":sc");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.kind(), Kind::Shortcuts);
        ctrl(&mut app, 'd');
        app.on_key(key(KeyCode::Char('y')));
        let a = acts(&mut app);
        assert!(
            matches!(&a[..], [Action::DeleteShortcut { path, name, .. }]
                if path == "Tables" && name == "claims"),
            "{a:?}"
        );
    }

    #[test]
    fn readonly_blocks_prompts_and_moves() {
        let mut app = App::new(
            vec![ProfileInfo {
                name: "prod".into(),
                account: "a@x".into(),
                readonly: true,
                color: None,
                ..Default::default()
            }],
            0,
        );
        settle(&mut app);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        for c in ['e', 'n', 'm'] {
            app.on_key(key(KeyCode::Char(c)));
            assert_eq!(app.mode, Mode::Table, "{c}");
        }
        ctrl(&mut app, 'd');
        assert_eq!(app.mode, Mode::Table);
        assert!(app.moving.is_none());
        assert!(app.status.as_ref().unwrap().text.contains("read-only"));
    }
}
