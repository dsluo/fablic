//! Turn Fabric API objects into table rows. Pure, so fake data and tests go
//! through exactly the same code as live responses.

use std::collections::HashMap;

#[cfg(not(test))]
use chrono::Local;
use chrono::{DateTime, NaiveDateTime, Utc};

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
        .map(item_row)
        .collect();
    items.sort_by_key(|r| r.name.to_lowercase());
    folders.extend(items);
    folders
}

fn item_row(i: &Value) -> Row {
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
}

/// Every folder and item at or below `folder` as one tree: in each folder,
/// subfolders first, then its items. Items are leaves.
pub fn items_tree(data: &WsData, folder: Option<&str>) -> Vec<Row> {
    let paths = data.folder_paths();
    let mut folders: Vec<(Row, String)> = data
        .folders
        .iter()
        .map(|f| {
            let path = paths.get(s(f, "id")).map_or("", String::as_str);
            (folder_row(f, path), s(f, "parentFolderId").to_owned())
        })
        .collect();
    folders.sort_by_key(|(r, _)| r.name.to_lowercase());
    let mut items: Vec<(Row, String)> = data
        .items
        .iter()
        .map(|i| (item_row(i), s(i, "folderId").to_owned()))
        .collect();
    items.sort_by_key(|(r, _)| r.name.to_lowercase());
    folders.extend(items);
    tree(folders, folder.unwrap_or(""))
}

/// Lay out `(row, parent id)` pairs as the tree below `root`, depth first,
/// keeping the given order among siblings. The first cell gets `├─` / `└─`
/// guides, and a container with something under it turns from `▸` to `▾`.
/// Rows that aren't below `root` are left out.
pub fn tree(nodes: Vec<(Row, String)>, root: &str) -> Vec<Row> {
    let mut children: HashMap<String, Vec<Row>> = HashMap::new();
    for (row, parent) in nodes {
        children.entry(parent).or_default().push(row);
    }
    let mut out = Vec::new();
    branch(&mut children, root, None, &mut out);
    out
}

/// The children of `parent`; `indent` is `None` for the top level, which
/// gets no guides.
fn branch(
    children: &mut HashMap<String, Vec<Row>>,
    parent: &str,
    indent: Option<&str>,
    out: &mut Vec<Row>,
) {
    // Taking each list out also ends a cycle in bad data.
    let Some(rows) = children.remove(parent) else {
        return;
    };
    let n = rows.len();
    for (i, mut row) in rows.into_iter().enumerate() {
        let last = i + 1 == n;
        let (guide, below) = match indent {
            None => (String::new(), String::new()),
            Some(pre) if last => (format!("{pre}└─ "), format!("{pre}   ")),
            Some(pre) => (format!("{pre}├─ "), format!("{pre}│  ")),
        };
        let open = children.contains_key(&row.id);
        if let Some(first) = row.cells.first_mut() {
            let name = match first.strip_prefix("▸ ") {
                Some(name) if open => format!("▾ {name}"),
                _ => first.clone(),
            };
            *first = format!("{guide}{name}");
        }
        let id = row.id.clone();
        out.push(row);
        branch(children, &id, Some(&below), out);
    }
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

/// Fabric timestamps: UTC, usually without an offset, with up to 7 fraction digits.
pub fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    let s = s.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|n| n.and_utc())
}

/// Start times in the viewer's timezone. Tests use UTC so snapshots don't
/// depend on where they run.
fn local_time(t: Option<DateTime<Utc>>) -> String {
    let fmt = "%Y-%m-%d %H:%M";
    match t {
        #[cfg(not(test))]
        Some(t) => t.with_timezone(&Local).format(fmt).to_string(),
        #[cfg(test)]
        Some(t) => t.format(fmt).to_string(),
        None => "-".into(),
    }
}

/// `45s`, `4m12s`, `1h02m`, `3d04h`.
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        3600..86400 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d{:02}h", s / 86400, s % 86400 / 3600),
    }
}

/// Job instances, newest first. `item` is the item the runs belong to when
/// runs of several items are mixed (adds an ITEM column and records its name
/// and type on each run); `now` dates runs that are still going.
pub fn jobs(list: &[Value], item: Option<&Value>, now: DateTime<Utc>) -> Vec<Row> {
    let runs: Vec<(Option<DateTime<Utc>>, Row)> = list
        .iter()
        .map(|j| {
            let start = parse_utc(s(j, "startTimeUtc"));
            let end = parse_utc(s(j, "endTimeUtc"));
            let status = s(j, "status");
            let took = match (start, end) {
                (Some(a), Some(b)) => duration((b - a).num_seconds()),
                (Some(a), None) if Health::from_status(status) == Health::Pending => {
                    format!("{}…", duration((now - a).num_seconds()))
                }
                _ => "-".into(),
            };
            let mut cells = vec![
                status.to_owned(),
                s(j, "jobType").into(),
                s(j, "invokeType").into(),
                local_time(start),
                took,
                run_reason(j),
            ];
            let raw = match item {
                Some(it) => {
                    cells.insert(0, s(it, "displayName").to_owned());
                    with(
                        j,
                        json!({"itemName": s(it, "displayName"), "itemType": s(it, "type")}),
                    )
                }
                None => j.clone(),
            };
            let row = Row::new(s(j, "id"), s(j, "id"), cells)
                .health(Health::from_status(status))
                .raw(raw);
            (start, row)
        })
        .collect();
    let mut rows: Vec<Row> = runs.into_iter().map(|(_, r)| r).collect();
    sort_runs(&mut rows);
    rows
}

/// Why a run failed or was cancelled, on one line: the failure message (or
/// its error code), else the reason `mark_cancelled` found.
fn run_reason(run: &Value) -> String {
    let failure = run.get("failureReason").unwrap_or(&Value::Null);
    let text = [
        at(failure, "/message"),
        at(failure, "/errorCode"),
        s(run, "cancellationReason"),
    ]
    .into_iter()
    .find(|t| !t.trim().is_empty())
    .unwrap_or("");
    text.lines().next().unwrap_or("").trim().to_owned()
}

/// Fabric reports a notebook or Spark job run that a user cancelled as
/// `Completed`; only its Spark (Livy) session records the cancellation. Mark
/// those runs `Cancelled`, keeping the session's reason on the run.
pub fn mark_cancelled(runs: &mut [Value], sessions: &[Value]) {
    for run in runs.iter_mut() {
        if !matches!(s(run, "status"), "Completed" | "Succeeded") {
            continue;
        }
        let reason = sessions
            .iter()
            .find(|l| s(l, "jobInstanceId") == s(run, "id"))
            .map(|l| s(l, "cancellationReason"))
            .filter(|r| r.starts_with("User cancelled"));
        if let Some(r) = reason {
            run["cancellationReason"] = r.into();
            run["status"] = "Cancelled".into();
        }
    }
}

/// Newest first; runs that haven't started have no start time and go on top.
/// Also used to merge per-item lists into one workspace-wide list.
pub fn sort_runs(rows: &mut [Row]) {
    rows.sort_by_cached_key(|r| {
        std::cmp::Reverse(
            parse_utc(r.field("startTimeUtc").unwrap_or(""))
                .map_or(i64::MAX, |t| t.timestamp_micros()),
        )
    });
}

/// Human summary of a schedule configuration: `every 15m`, `daily 01:47`,
/// `Mon,Fri 06:00`.
fn schedule_when(c: &Value) -> String {
    let list = |key: &str| -> Vec<String> {
        c.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let times = list("times").join(",");
    match s(c, "type") {
        "Cron" => {
            let mins = c.get("interval").and_then(Value::as_i64).unwrap_or(0);
            format!("every {}", duration(mins * 60).trim_end_matches("00s"))
        }
        "Daily" => format!("daily {times}"),
        "Weekly" => {
            let days: Vec<String> = list("weekdays")
                .iter()
                .map(|d| d.chars().take(3).collect())
                .collect();
            format!("{} {times}", days.join(","))
        }
        "Monthly" => {
            let day = c.get("occurrence").map(|o| {
                o.get("dayOfMonth")
                    .map(|d| format!("day {d}"))
                    .unwrap_or_else(|| format!("{} {}", s(o, "weekIndex"), s(o, "weekday")))
            });
            format!("monthly {} {times}", day.unwrap_or_default())
        }
        other => other.to_owned(),
    }
}

pub fn schedules(list: &[Value]) -> Vec<Row> {
    list.iter()
        .map(|sc| {
            let c = sc.get("configuration").cloned().unwrap_or(Value::Null);
            let enabled = sc.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let state = match (enabled, at(sc, "/enabledDetail/enabledControlType")) {
                (true, _) => "yes".to_owned(),
                (false, "") | (false, "UserDisabled") => "no".to_owned(),
                // e.g. AutoDisabled after repeated failures
                (false, why) => format!("no ({why})"),
            };
            let health = match state.as_str() {
                "yes" => Health::Ok,
                "no" => Health::Neutral,
                _ => Health::Bad,
            };
            Row::new(
                s(sc, "id"),
                s(sc, "id"),
                vec![
                    s(&c, "type").into(),
                    state,
                    schedule_when(&c),
                    s(&c, "localTimeZoneId").into(),
                    s(&c, "startDateTime").get(..10).unwrap_or("-").into(),
                    s(&c, "endDateTime").get(..10).unwrap_or("-").into(),
                    s(sc, "id").into(),
                ],
            )
            .health(health)
            .raw(sc.clone())
        })
        .collect()
}

/// `512 B`, `1.2 KB`, `3.4 MB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// One OneLake directory listing. DFS names are `{item}/{path}`; rows carry
/// the path relative to the item as their id. Directories first.
pub fn onelake(paths: &[Value], item: &str) -> Vec<Row> {
    let prefix = format!("{item}/");
    let mut rows: Vec<Row> = paths
        .iter()
        .map(|p| {
            let full = s(p, "name");
            let rel = full.strip_prefix(&prefix).unwrap_or(full);
            let name = rel.rsplit('/').next().unwrap_or(rel);
            // DFS sends booleans and numbers as strings.
            let dir = s(p, "isDirectory") == "true" || p.get("isDirectory") == Some(&json!(true));
            let len: u64 = s(p, "contentLength").parse().unwrap_or(0);
            let modified = DateTime::parse_from_rfc2822(s(p, "lastModified"))
                .ok()
                .map(|t| t.with_timezone(&Utc));
            let row = Row::new(
                rel,
                name,
                vec![
                    if dir {
                        format!("▸ {name}")
                    } else {
                        name.to_owned()
                    },
                    if dir { "dir" } else { "file" }.into(),
                    if dir { String::new() } else { size(len) },
                    local_time(modified),
                ],
            )
            .raw(with(p, json!({"path": rel, "size": len})));
            if dir { row.dir() } else { row }
        })
        .collect();
    rows.sort_by_key(|r| (!r.dir, r.name.to_lowercase()));
    rows
}

/// A recursive OneLake listing below `dir` as a tree, directories first at
/// every level.
pub fn onelake_tree(paths: &[Value], item: &str, dir: &str) -> Vec<Row> {
    let nodes = onelake(paths, item)
        .into_iter()
        .map(|r| {
            let parent = r.id.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
            (r, parent)
        })
        .collect();
    tree(nodes, dir)
}

/// Where a shortcut points, in one line.
fn shortcut_target(t: &Value) -> (String, String) {
    let kind = s(t, "type").to_owned();
    let detail = match kind.as_str() {
        "OneLake" => format!(
            "{}/{}/{}",
            at(t, "/oneLake/workspaceId"),
            at(t, "/oneLake/itemId"),
            at(t, "/oneLake/path")
        ),
        "AdlsGen2" => format!(
            "{}{}",
            at(t, "/adlsGen2/location"),
            at(t, "/adlsGen2/subpath")
        ),
        "AmazonS3" => format!(
            "{}{}",
            at(t, "/amazonS3/location"),
            at(t, "/amazonS3/subpath")
        ),
        "GoogleCloudStorage" => format!(
            "{}{}",
            at(t, "/googleCloudStorage/location"),
            at(t, "/googleCloudStorage/subpath")
        ),
        "S3Compatible" => format!(
            "{}/{}{}",
            at(t, "/s3Compatible/location"),
            at(t, "/s3Compatible/bucket"),
            at(t, "/s3Compatible/subpath")
        ),
        "Dataverse" => format!(
            "{} {}",
            at(t, "/dataverse/environmentDomain"),
            at(t, "/dataverse/tableName")
        ),
        _ => String::new(),
    };
    (kind, detail)
}

pub fn shortcuts(list: &[Value]) -> Vec<Row> {
    let mut rows: Vec<Row> = list
        .iter()
        .map(|sc| {
            let (kind, target) = shortcut_target(sc.get("target").unwrap_or(&Value::Null));
            let full = format!("{}/{}", s(sc, "path"), s(sc, "name"));
            Row::new(
                full,
                s(sc, "name"),
                vec![s(sc, "name").into(), s(sc, "path").into(), kind, target],
            )
            .raw(sc.clone())
        })
        .collect();
    rows.sort_by_key(|r| r.id.to_lowercase());
    rows
}

/// Columns of a table as aligned text: name, type, nullability. (The API's
/// `partition_index` is 0 for every column of unpartitioned tables, so it
/// can't tell partition columns apart and isn't shown.)
pub fn schema_text(table: &Value) -> String {
    let cols = table
        .get("columns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if cols.is_empty() {
        return "(no columns reported)".into();
    }
    let ty = |c: &Value| -> String {
        let name = s(c, "type_name");
        let (p, sc) = (
            c.get("type_precision").and_then(Value::as_i64).unwrap_or(0),
            c.get("type_scale").and_then(Value::as_i64).unwrap_or(0),
        );
        match s(c, "type_text") {
            "" if name.eq_ignore_ascii_case("decimal") && p > 0 => format!("decimal({p},{sc})"),
            "" => name.to_lowercase(),
            text => text.to_owned(),
        }
    };
    let width = cols
        .iter()
        .map(|c| s(c, "name").chars().count())
        .max()
        .unwrap_or(4)
        .max(6);
    let tw = cols.iter().map(|c| ty(c).len()).max().unwrap_or(4).max(4);
    let mut out = format!("{:<width$}  {:<tw$}  NULL\n", "COLUMN", "TYPE");
    for c in &cols {
        let nullable = c.get("nullable").and_then(Value::as_bool).unwrap_or(true);
        out.push_str(&format!(
            "{:<width$}  {:<tw$}  {}\n",
            s(c, "name"),
            ty(c),
            if nullable { "yes" } else { "no" },
        ));
    }
    out
}

/// Text to show for the start of a file, or a note when it isn't text.
pub fn preview_text(bytes: &[u8], total: Option<u64>, max: usize) -> String {
    let binary =
        bytes.contains(&0) || std::str::from_utf8(bytes).is_err_and(|e| e.error_len().is_some());
    if binary {
        return format!(
            "(binary file{}; press D to download it)",
            total.map(|t| format!(", {}", size(t))).unwrap_or_default()
        );
    }
    // A multi-byte character may be cut at the range boundary; drop the partial tail.
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t.to_owned(),
        Err(e) => String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
    };
    let mut out = text.replace('\t', "    ");
    if bytes.len() >= max && total.is_none_or(|t| t > max as u64) {
        out.push_str(&format!(
            "\n… (first {} only; press D to download)",
            size(max as u64)
        ));
    }
    out
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
    fn user_cancelled_spark_runs_show_as_cancelled() {
        let mut runs = vec![
            json!({"id": "a", "status": "Completed"}),
            json!({"id": "b", "status": "Completed"}),
            json!({"id": "c", "status": "Failed"}),
        ];
        let sessions = [
            json!({"jobInstanceId": "a", "state": "Succeeded",
                   "cancellationReason": "User cancelled the Spark session"}),
            json!({"jobInstanceId": "b", "state": "Succeeded",
                   "cancellationReason": "System stopped the Spark session upon completion of the notebook run"}),
            json!({"jobInstanceId": "c", "cancellationReason": "User cancelled the Spark session"}),
        ];
        mark_cancelled(&mut runs, &sessions);
        let status: Vec<&str> = runs.iter().map(|r| s(r, "status")).collect();
        assert_eq!(status, ["Cancelled", "Completed", "Failed"]);
        assert_eq!(
            s(&runs[0], "cancellationReason"),
            "User cancelled the Spark session"
        );
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
    fn tree_nests_folders_and_items() {
        let d = data();
        let cells =
            |rows: &[Row]| -> Vec<String> { rows.iter().map(|r| r.cells[0].clone()).collect() };
        assert_eq!(
            cells(&items_tree(&d, None)),
            [
                "▾ archive",
                "└─ old",
                "▾ ingest",
                "├─ ▾ shop",
                "│  └─ extract",
                "└─ load",
                "readme",
            ]
        );
        let below = items_tree(&d, Some("f1"));
        assert_eq!(cells(&below), ["▾ shop", "└─ extract", "load"]);
        assert_eq!(below[0].field("path"), Some("ingest/shop"));
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
    fn parses_fabric_timestamps() {
        let t = parse_utc("2026-10-09T13:02:05.1234567").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-09T13:02:05.123456700+00:00");
        assert!(parse_utc("2026-10-09T13:02:05Z").is_some());
        assert!(parse_utc("").is_none());
    }

    #[test]
    fn durations() {
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(252), "4m12s");
        assert_eq!(duration(3720), "1h02m");
        assert_eq!(duration(273600), "3d04h");
    }

    #[test]
    fn jobs_newest_first_with_running_durations() {
        let now = parse_utc("2026-10-09T13:10:00").unwrap();
        let list = [
            json!({"id": "old", "status": "Completed", "startTimeUtc": "2026-10-08T01:00:00",
                   "endTimeUtc": "2026-10-08T01:09:30"}),
            json!({"id": "run", "status": "InProgress", "startTimeUtc": "2026-10-09T13:02:00"}),
            json!({"id": "queued", "status": "NotStarted"}),
        ];
        let rows = jobs(&list, None, now);
        assert_eq!(names(&rows), ["queued", "run", "old"]);
        assert_eq!(rows[1].cells[4], "8m00s…");
        assert_eq!(rows[2].cells[4], "9m30s");
        assert_eq!(rows[1].health, Health::Pending);
        let item = json!({"displayName": "nb", "type": "Notebook"});
        let mixed = jobs(&list[..1], Some(&item), now);
        assert_eq!(mixed[0].cells[0], "nb");
        assert_eq!(mixed[0].field("itemType"), Some("Notebook"));
    }

    #[test]
    fn schedule_summaries() {
        let list = [
            json!({"id": "a", "enabled": false, "configuration": {"type": "Daily",
                   "startDateTime": "2026-06-09T00:00:00", "endDateTime": "2027-06-09T00:00:00",
                   "localTimeZoneId": "Eastern Standard Time", "times": ["01:47"]},
                   "enabledDetail": {"enabledControlType": "UserDisabled"}}),
            json!({"id": "b", "enabled": true, "configuration": {"type": "Cron", "interval": 15}}),
            json!({"id": "c", "enabled": true, "configuration": {"type": "Weekly",
                   "weekdays": ["Monday", "Friday"], "times": ["06:00"]}}),
            json!({"id": "d", "enabled": false, "configuration": {"type": "Cron", "interval": 90},
                   "enabledDetail": {"enabledControlType": "AutoDisabled"}}),
        ];
        let rows = schedules(&list);
        let when: Vec<&str> = rows.iter().map(|r| r.cells[2].as_str()).collect();
        assert_eq!(
            when,
            ["daily 01:47", "every 15m", "Mon,Fri 06:00", "every 1h30m"]
        );
        assert_eq!(rows[0].cells[1], "no");
        assert_eq!(rows[0].cells[4], "2026-06-09");
        assert_eq!(rows[3].cells[1], "no (AutoDisabled)");
        assert_eq!(rows[3].health, Health::Bad);
    }

    #[test]
    fn onelake_rows_strip_item_prefix_dirs_first() {
        let paths = [
            json!({"name": "i/Files/z.csv", "isDirectory": "false", "contentLength": "1536",
                   "lastModified": "Mon, 27 Oct 2025 13:53:32 GMT"}),
            json!({"name": "i/Files/raw", "isDirectory": "true", "contentLength": "0"}),
        ];
        let rows = onelake(&paths, "i");
        assert_eq!(rows[0].id, "Files/raw");
        assert!(rows[0].dir);
        assert_eq!(rows[1].cells[..3], ["z.csv", "file", "1.5 KB"]);
        assert_eq!(rows[1].cells[3], "2025-10-27 13:53");
    }

    #[test]
    fn onelake_tree_nests_below_the_directory() {
        let p = |name: &str, dir: bool| json!({"name": format!("i/{name}"), "isDirectory": dir.to_string()});
        let paths = [
            p("Files/z.csv", false),
            p("Files/raw", true),
            p("Files/raw/a.json", false),
            p("Files/empty", true),
        ];
        let rows = onelake_tree(&paths, "i", "Files");
        let names: Vec<&str> = rows.iter().map(|r| r.cells[0].as_str()).collect();
        assert_eq!(names, ["▸ empty", "▾ raw", "└─ a.json", "z.csv"]);
        assert_eq!(rows[2].id, "Files/raw/a.json");
    }

    #[test]
    fn shortcut_targets() {
        let rows = shortcuts(&[json!({"name": "sales", "path": "Tables",
            "target": {"type": "AdlsGen2", "adlsGen2": {"location": "https://acct.dfs.core.windows.net",
                       "subpath": "/c/sales"}}})]);
        assert_eq!(
            rows[0].cells,
            [
                "sales",
                "Tables",
                "AdlsGen2",
                "https://acct.dfs.core.windows.net/c/sales"
            ]
        );
        assert_eq!(rows[0].id, "Tables/sales");
    }

    #[test]
    fn schema_lists_columns() {
        let t = json!({"columns": [
            {"name": "id", "type_name": "LONG", "nullable": false, "partition_index": 0},
            {"name": "amount", "type_name": "DECIMAL", "type_precision": 18, "type_scale": 2, "nullable": true}
        ]});
        let text = schema_text(&t);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "COLUMN  TYPE           NULL");
        assert_eq!(lines[1].trim_end(), "id      long           no");
        assert_eq!(lines[2].trim_end(), "amount  decimal(18,2)  yes");
    }

    #[test]
    fn previews_text_and_flags_binary() {
        assert_eq!(preview_text(b"a\tb\n", Some(4), 64), "a    b\n");
        assert!(preview_text(b"PAR1\0\x15", Some(9000), 64).starts_with("(binary file, 8.8 KB"));
        let cut = preview_text("é".repeat(40).as_bytes().get(..63).unwrap(), Some(80), 63);
        assert!(cut.ends_with("press D to download)"), "{cut}");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
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
