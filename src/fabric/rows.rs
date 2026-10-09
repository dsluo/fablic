//! Turn Fabric API objects into table rows. Pure, so fake data and tests go
//! through exactly the same code as live responses.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::resources::{Health, Row};

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn at<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or("")
}

fn or_dash(x: &str) -> String {
    if x.is_empty() {
        "-".into()
    } else {
        x.to_owned()
    }
}

/// Copy of `v` with extra fields merged in, so describe shows what we derived.
fn with(v: &Value, extra: Value) -> Value {
    let mut out = v.clone();
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
    out
}

/// Everything listed in one workspace: its folders and all of its items.
#[derive(Debug, Clone, Default)]
pub struct WsData {
    pub folders: Vec<Value>,
    pub items: Vec<Value>,
}

impl WsData {
    /// Full slash-separated path of every folder id.
    pub fn folder_paths(&self) -> HashMap<String, String> {
        let by_id: HashMap<&str, &Value> = self.folders.iter().map(|f| (s(f, "id"), f)).collect();
        let mut out = HashMap::new();
        for f in &self.folders {
            let mut parts = vec![s(f, "displayName")];
            let mut cur = f;
            // Fabric caps nesting at 10 levels; the bound also stops a bad cycle.
            for _ in 0..16 {
                match by_id.get(s(cur, "parentFolderId")) {
                    Some(p) => {
                        parts.push(s(p, "displayName"));
                        cur = p;
                    }
                    None => break,
                }
            }
            parts.reverse();
            out.insert(s(f, "id").to_owned(), parts.join("/"));
        }
        out
    }
}

pub fn workspaces(list: &[Value], capacities: &[Value]) -> Vec<Row> {
    let caps: HashMap<&str, &str> = capacities
        .iter()
        .map(|c| (s(c, "id"), s(c, "displayName")))
        .collect();
    let mut rows: Vec<Row> = list
        .iter()
        .map(|w| {
            let cap_id = s(w, "capacityId");
            let cap = caps.get(cap_id).copied().unwrap_or(cap_id);
            Row::new(
                s(w, "id"),
                s(w, "displayName"),
                vec![
                    s(w, "displayName").into(),
                    s(w, "type").into(),
                    or_dash(cap),
                    s(w, "id").into(),
                ],
            )
            .raw(w.clone())
        })
        .collect();
    rows.sort_by_key(|r| r.name.to_lowercase());
    rows
}

fn folder_row(f: &Value, path: &str) -> Row {
    let name = s(f, "displayName");
    Row::new(
        s(f, "id"),
        name,
        vec![
            format!("▸ {name}"),
            "Folder".into(),
            String::new(),
            s(f, "id").into(),
        ],
    )
    .dir()
    .raw(with(f, json!({"type": "Folder", "path": path})))
}

/// One level of the folder tree: subfolders first, then the items directly in it.
pub fn items_level(data: &WsData, folder: Option<&str>) -> Vec<Row> {
    let paths = data.folder_paths();
    let here = |parent: &str| match folder {
        Some(id) => parent == id,
        None => parent.is_empty(),
    };
    let mut folders: Vec<Row> = data
        .folders
        .iter()
        .filter(|f| here(s(f, "parentFolderId")))
        .map(|f| folder_row(f, paths.get(s(f, "id")).map_or("", String::as_str)))
        .collect();
    folders.sort_by_key(|r| r.name.to_lowercase());
    let mut items: Vec<Row> = data
        .items
        .iter()
        .filter(|i| here(s(i, "folderId")))
        .map(|i| {
            Row::new(
                s(i, "id"),
                s(i, "displayName"),
                vec![
                    s(i, "displayName").into(),
                    s(i, "type").into(),
                    s(i, "description").into(),
                    s(i, "id").into(),
                ],
            )
            .raw(i.clone())
        })
        .collect();
    items.sort_by_key(|r| r.name.to_lowercase());
    folders.extend(items);
    folders
}

/// Every item at or below `folder`, with its folder path, optionally of one type.
pub fn items_flat(data: &WsData, folder: Option<&str>, item_type: Option<&str>) -> Vec<Row> {
    let paths = data.folder_paths();
    let prefix = folder.and_then(|id| paths.get(id)).cloned();
    let mut rows: Vec<Row> = data
        .items
        .iter()
        .filter(|i| item_type.is_none_or(|t| s(i, "type").eq_ignore_ascii_case(t)))
        .filter_map(|i| {
            let path = paths.get(s(i, "folderId")).cloned().unwrap_or_default();
            if let Some(p) = &prefix
                && !(path == *p || path.starts_with(&format!("{p}/")))
            {
                return None;
            }
            Some(
                Row::new(
                    s(i, "id"),
                    s(i, "displayName"),
                    vec![
                        s(i, "displayName").into(),
                        s(i, "type").into(),
                        format!("/{path}"),
                        s(i, "description").into(),
                        s(i, "id").into(),
                    ],
                )
                .raw(with(i, json!({"folderPath": path}))),
            )
        })
        .collect();
    rows.sort_by(|a, b| {
        (&a.cells[2], a.name.to_lowercase()).cmp(&(&b.cells[2], b.name.to_lowercase()))
    });
    rows
}

/// Items across several workspaces, for `:items` / `:nb` with no workspace in scope.
pub fn items_all(spaces: &[(Value, WsData)], item_type: Option<&str>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (ws, data) in spaces {
        let ws_name = s(ws, "displayName");
        for r in items_flat(data, None, item_type) {
            let folder = r.cells[2].clone();
            rows.push(Row {
                cells: vec![
                    r.cells[0].clone(),
                    r.cells[1].clone(),
                    ws_name.into(),
                    folder,
                    r.id.clone(),
                ],
                raw: with(&r.raw, json!({"workspaceName": ws_name})),
                ..r
            });
        }
    }
    rows.sort_by(|a, b| {
        (&a.cells[2], a.name.to_lowercase()).cmp(&(&b.cells[2], b.name.to_lowercase()))
    });
    rows
}

pub fn roles(list: &[Value]) -> Vec<Row> {
    list.iter()
        .map(|r| {
            let upn = at(r, "/principal/userDetails/userPrincipalName");
            let name = match at(r, "/principal/displayName") {
                "" => upn,
                n => n,
            };
            Row::new(
                s(r, "id"),
                name,
                vec![
                    name.into(),
                    at(r, "/principal/type").into(),
                    s(r, "role").into(),
                    s(r, "id").into(),
                ],
            )
            .raw(r.clone())
        })
        .collect()
}

pub fn capacities(list: &[Value]) -> Vec<Row> {
    list.iter()
        .map(|c| {
            Row::new(
                s(c, "id"),
                s(c, "displayName"),
                vec![
                    s(c, "displayName").into(),
                    s(c, "sku").into(),
                    s(c, "region").into(),
                    s(c, "state").into(),
                    s(c, "id").into(),
                ],
            )
            .health(Health::from_status(s(c, "state")))
            .raw(c.clone())
        })
        .collect()
}

pub fn connections(list: &[Value]) -> Vec<Row> {
    list.iter()
        .map(|c| {
            Row::new(
                s(c, "id"),
                s(c, "displayName"),
                vec![
                    or_dash(s(c, "displayName")),
                    at(c, "/connectionDetails/type").into(),
                    s(c, "connectivityType").into(),
                    s(c, "privacyLevel").into(),
                    s(c, "id").into(),
                ],
            )
            .raw(c.clone())
        })
        .collect()
}

pub fn gateways(list: &[Value]) -> Vec<Row> {
    list.iter()
        .map(|g| {
            let members = g
                .get("numberOfMemberGateways")
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into());
            Row::new(
                s(g, "id"),
                s(g, "displayName"),
                vec![
                    or_dash(s(g, "displayName")),
                    s(g, "type").into(),
                    members,
                    or_dash(s(g, "version")),
                    s(g, "id").into(),
                ],
            )
            .raw(g.clone())
        })
        .collect()
}

pub fn domains(list: &[Value]) -> Vec<Row> {
    let names: HashMap<&str, &str> = list
        .iter()
        .map(|d| (s(d, "id"), s(d, "displayName")))
        .collect();
    list.iter()
        .map(|d| {
            let parent = s(d, "parentDomainId");
            Row::new(
                s(d, "id"),
                s(d, "displayName"),
                vec![
                    s(d, "displayName").into(),
                    s(d, "description").into(),
                    or_dash(names.get(parent).copied().unwrap_or(parent)),
                    s(d, "id").into(),
                ],
            )
            .raw(d.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> WsData {
        WsData {
            folders: vec![
                json!({"id": "f1", "displayName": "ingest"}),
                json!({"id": "f2", "displayName": "shop", "parentFolderId": "f1"}),
                json!({"id": "f3", "displayName": "archive"}),
            ],
            items: vec![
                json!({"id": "i1", "displayName": "readme", "type": "Report"}),
                json!({"id": "i2", "displayName": "load", "type": "Notebook", "folderId": "f1"}),
                json!({"id": "i3", "displayName": "extract", "type": "Notebook", "folderId": "f2"}),
                json!({"id": "i4", "displayName": "old", "type": "Notebook", "folderId": "f3"}),
            ],
        }
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn folder_paths_nest() {
        let p = data().folder_paths();
        assert_eq!(p["f2"], "ingest/shop");
        assert_eq!(p["f3"], "archive");
    }

    #[test]
    fn levels_list_folders_first() {
        let d = data();
        let root = items_level(&d, None);
        assert_eq!(names(&root), ["archive", "ingest", "readme"]);
        assert!(root[0].dir && !root[2].dir);
        assert_eq!(root[1].field("path"), Some("ingest"));
        assert_eq!(names(&items_level(&d, Some("f1"))), ["shop", "load"]);
        assert_eq!(items_level(&d, Some("f2"))[0].field("path"), None);
    }

    #[test]
    fn flat_covers_subtree_and_type() {
        let d = data();
        assert_eq!(
            names(&items_flat(&d, None, None)),
            ["readme", "old", "load", "extract"]
        );
        assert_eq!(
            names(&items_flat(&d, Some("f1"), None)),
            ["load", "extract"]
        );
        let nb = items_flat(&d, None, Some("notebook"));
        assert_eq!(names(&nb), ["old", "load", "extract"]);
        assert_eq!(nb[2].cells[2], "/ingest/shop");
    }

    #[test]
    fn cross_workspace_rows_carry_workspace() {
        let spaces = vec![(json!({"id": "w1", "displayName": "Bronze"}), data())];
        let rows = items_all(&spaces, Some("Report"));
        assert_eq!(rows[0].cells, ["readme", "Report", "Bronze", "/", "i1"]);
        assert_eq!(rows[0].field("workspaceName"), Some("Bronze"));
    }

    #[test]
    fn workspaces_show_capacity_names() {
        let rows = workspaces(
            &[json!({"id": "w", "displayName": "B", "type": "Workspace", "capacityId": "c"})],
            &[json!({"id": "c", "displayName": "Prod F64"})],
        );
        assert_eq!(rows[0].cells[2], "Prod F64");
    }
}
