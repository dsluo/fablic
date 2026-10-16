//! The fake tenant's starting state. Everything here is made up; the shapes
//! follow the Fabric API so it goes through the same row builders as live
//! responses.
//!
//! `Urban1 Bronze` (`ws-1`) is the small fixture the tests walk through, so
//! keep it as it is. The other workspaces show off the rest: deep folders,
//! long names, every run state, schema-enabled lakehouses, each shortcut
//! target and role type.

use std::borrow::Cow;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::fabric::rows::WsData;
use crate::source::Part;

pub fn workspaces() -> Vec<Value> {
    let ws = |id: &str, name: &str, cap: Option<&str>| {
        let mut v = json!({"id": id, "displayName": name, "type": "Workspace",
                           "description": ""});
        if let Some(c) = cap {
            v["capacityId"] = c.into();
        }
        v
    };
    vec![
        ws("ws-1", "Urban1 Bronze", Some("cap-f64")),
        ws("ws-2", "Urban2 Silver", Some("cap-f64")),
        ws("ws-3", "Sandbox", Some("cap-trial")),
        ws("ws-5", "Urban3 Gold", Some("cap-f8")),
        ws("ws-6", "Urban4 Archive", None),
    ]
}

/// `git/connection` of each workspace: Urban2 Silver is on GitHub with
/// changes on both sides, Urban3 Gold on Azure DevOps and synced, and the
/// rest aren't connected.
pub fn git_connection(ws: &str) -> Value {
    match ws {
        "ws-2" => json!({
            "gitProviderDetails": {
                "gitProviderType": "GitHub", "ownerName": "contoso",
                "repositoryName": "fabric-silver", "branchName": "main",
                "directoryName": "/workspace"
            },
            "gitSyncDetails": {"head": "1111111111111111111111111111111111111111",
                               "lastSyncTime": "2026-10-08T16:20:00Z"},
            "gitConnectionState": "ConnectedAndInitialized",
            "gitConnectionType": "Full"
        }),
        "ws-5" => json!({
            "gitProviderDetails": {
                "gitProviderType": "AzureDevOps", "organizationName": "contoso",
                "projectName": "Analytics", "repositoryName": "gold",
                "branchName": "release/2026.10", "directoryName": ""
            },
            "gitSyncDetails": {"head": "2222222222222222222222222222222222222222",
                               "lastSyncTime": "2026-10-01T09:00:00Z"},
            "gitConnectionState": "ConnectedAndInitialized",
            "gitConnectionType": "Full"
        }),
        _ => json!({"gitProviderDetails": null, "gitSyncDetails": null,
                    "gitConnectionState": "NotConnected"}),
    }
}

/// `git/status` of a connected workspace; items not listed are synced.
pub fn git_status(ws: &str) -> Value {
    let change = |id: &str, name: &str, t: &str, ws_change: Option<&str>, remote: Option<&str>| {
        let mut c = json!({
            "itemMetadata": {"itemIdentifier": {"objectId": id, "logicalId": id},
                             "itemType": t, "displayName": name},
            "conflictType": if ws_change.is_some() && remote.is_some() { "Conflict" } else { "None" }
        });
        if let Some(w) = ws_change {
            c["workspaceChange"] = w.into();
        }
        if let Some(r) = remote {
            c["remoteChange"] = r.into();
        }
        c
    };
    match ws {
        "ws-2" => json!({
            "workspaceHead": "1111111111111111111111111111111111111111",
            "remoteCommitHash": "3333333333333333333333333333333333333333",
            "changes": [
                change("it-21", "merge_customers", "Notebook", Some("Modified"), None),
                change("it-22", "merge_orders", "Notebook", None, Some("Modified")),
                change("it-23", "nightly_silver", "DataPipeline", Some("Modified"), Some("Modified")),
                change("it-32", "settings", "VariableLibrary", Some("Added"), None),
            ]
        }),
        _ => json!({
            "workspaceHead": "2222222222222222222222222222222222222222",
            "remoteCommitHash": "2222222222222222222222222222222222222222",
            "changes": []
        }),
    }
}

/// `cap-trial` is left out on purpose: a capacity the caller can't see
/// shows as its id.
pub fn capacities() -> Vec<Value> {
    vec![
        json!({"id": "cap-f64", "displayName": "Prod F64", "sku": "F64",
                "region": "East US 2", "state": "Active"}),
        json!({"id": "cap-f8", "displayName": "Dev F8", "sku": "F8",
                "region": "West Europe", "state": "Paused"}),
        json!({"id": "cap-f2", "displayName": "Spare F2", "sku": "F2",
                "region": "East US 2", "state": "Provisioning"}),
    ]
}

pub fn connections() -> Vec<Value> {
    let conn = |id: &str, name: &str, kind: &str, path: &str, via: &str, privacy: &str| {
        json!({"id": id, "displayName": name, "connectivityType": via,
               "privacyLevel": privacy,
               "connectionDetails": {"type": kind, "path": path},
               "credentialDetails": {"credentialType": "OAuth2",
                                     "singleSignOnType": "None"}})
    };
    vec![
        conn(
            "conn-1",
            "sales-sql",
            "SQL",
            "sales.database.windows.net;sales",
            "ShareableCloud",
            "Organizational",
        ),
        conn(
            "conn-2",
            "contoso-lake",
            "AzureDataLakeStorage",
            "https://contosolake.dfs.core.windows.net/raw",
            "ShareableCloud",
            "Private",
        ),
        conn(
            "conn-3",
            "erp-onprem",
            "SQL",
            "erp01;erp",
            "OnPremisesGateway",
            "Organizational",
        ),
        conn(
            "conn-4",
            "vnet-postgres",
            "PostgreSql",
            "pg.internal.contoso.example;orders",
            "VirtualNetworkGateway",
            "Organizational",
        ),
        // Personal cloud connections have no name.
        conn(
            "conn-5",
            "",
            "Web",
            "https://api.example.com",
            "PersonalCloud",
            "None",
        ),
    ]
}

pub fn gateways() -> Vec<Value> {
    vec![
        json!({"id": "gw-1", "displayName": "contoso-onprem-cluster", "type": "OnPremises",
               "numberOfMemberGateways": 2, "version": "3000.290.12",
               "loadBalancingSetting": "Failover"}),
        json!({"id": "gw-2", "displayName": "vnet-eastus2", "type": "VirtualNetwork",
               "capacityId": "cap-f64", "inactivityMinutesBeforeSleep": 30,
               "numberOfMemberGateways": 1}),
        json!({"id": "gw-3", "type": "OnPremisesPersonal", "version": "3000.286.4"}),
    ]
}

pub fn domains() -> Vec<Value> {
    vec![
        json!({"id": "dom-1", "displayName": "Sales",
               "description": "Customers, orders and revenue"}),
        json!({"id": "dom-2", "displayName": "Loyalty", "parentDomainId": "dom-1",
               "description": "Loyalty programme members and points"}),
        json!({"id": "dom-3", "displayName": "Finance", "description": ""}),
        json!({"id": "dom-4", "displayName": "Operations",
               "description": "Stores, stock and logistics"}),
    ]
}

fn folder(ws: &str, id: &str, name: &str, parent: Option<&str>) -> Value {
    let mut v = json!({"id": id, "displayName": name, "workspaceId": ws});
    if let Some(p) = parent {
        v["parentFolderId"] = p.into();
    }
    v
}

fn item(ws: &str, id: &str, name: &str, t: &str, folder: Option<&str>, desc: &str) -> Value {
    let mut v = json!({"id": id, "displayName": name, "type": t, "description": desc,
                       "workspaceId": ws});
    if let Some(f) = folder {
        v["folderId"] = f.into();
    }
    v
}

/// Every workspace's folders and items, by workspace id.
pub fn spaces() -> Vec<(String, WsData)> {
    let bronze = WsData {
        folders: vec![
            folder("ws-1", "f-1", "ingest", None),
            folder("ws-1", "f-2", "shop", Some("f-1")),
        ],
        items: vec![
            item(
                "ws-1",
                "it-1",
                "ingest_shop",
                "Notebook",
                Some("f-2"),
                "Nightly shop extract",
            ),
            item("ws-1", "it-2", "bronze_lh", "Lakehouse", None, ""),
            item(
                "ws-1",
                "it-3",
                "load_all",
                "DataPipeline",
                Some("f-1"),
                "Orchestrates ingest",
            ),
            item("ws-1", "it-4", "Finance Model", "SemanticModel", None, ""),
            item("ws-1", "it-5", "transform_silver", "Notebook", None, ""),
        ],
    };
    let w = "ws-2";
    let silver = WsData {
        folders: vec![
            folder(w, "f-20", "silver", None),
            folder(w, "f-21", "conformed", Some("f-20")),
            folder(
                w,
                "f-22",
                "customer 360 and loyalty programme reconciliation",
                Some("f-21"),
            ),
            folder(w, "f-23", "orchestration", None),
            folder(w, "f-24", "reporting", None),
            folder(w, "f-25", "archive (2025)", None),
        ],
        items: vec![
            item(
                w,
                "it-20",
                "silver_lh",
                "Lakehouse",
                None,
                "Conformed silver tables, refreshed nightly from bronze",
            ),
            item(
                w,
                "it-21",
                "merge_customers",
                "Notebook",
                Some("f-21"),
                "Merges customer records from every bronze source into one conformed \
                 table, deduplicating on email address and loyalty number",
            ),
            item(
                w,
                "it-22",
                "merge_orders",
                "Notebook",
                Some("f-21"),
                "Orders and order lines",
            ),
            item(
                w,
                "it-23",
                "nightly_silver",
                "DataPipeline",
                Some("f-23"),
                "Runs every silver notebook in order",
            ),
            item(
                w,
                "it-24",
                "spark_compaction",
                "SparkJobDefinition",
                Some("f-23"),
                "OPTIMIZE and VACUUM on the large tables",
            ),
            item(
                w,
                "it-25",
                "silver_env",
                "Environment",
                None,
                "Spark 3.5 with pinned libraries",
            ),
            item(
                w,
                "it-26",
                "customer_dedupe_rules",
                "Notebook",
                Some("f-22"),
                "Matching rules, never run on a schedule",
            ),
            item(w, "it-27", "Sales Model", "SemanticModel", Some("f-24"), ""),
            item(
                w,
                "it-28",
                "Sales Overview",
                "Report",
                Some("f-24"),
                "Daily sales by region and channel",
            ),
            item(
                w,
                "it-29",
                "silver_wh",
                "Warehouse",
                None,
                "SQL serving layer",
            ),
            item(w, "it-30", "orders_stream", "Eventstream", None, ""),
            item(w, "it-31", "telemetry", "Eventhouse", None, ""),
            item(w, "it-32", "settings", "VariableLibrary", None, ""),
        ],
    };
    let w = "ws-3";
    let sandbox = WsData {
        folders: vec![folder(w, "f-30", "experiments", None)],
        items: vec![
            item(
                w,
                "it-40",
                "scratch",
                "Notebook",
                None,
                "Try things here; writes are safe in this workspace",
            ),
            item(w, "it-41", "playground_lh", "Lakehouse", None, ""),
            item(
                w,
                "it-42",
                "try_window_functions",
                "Notebook",
                Some("f-30"),
                "",
            ),
        ],
    };
    let w = "ws-5";
    let gold = WsData {
        folders: Vec::new(),
        items: vec![
            item(
                w,
                "it-50",
                "gold_lh",
                "Lakehouse",
                None,
                "KPIs for the dashboards",
            ),
            item(w, "it-51", "Gold Model", "SemanticModel", None, ""),
            item(
                w,
                "it-52",
                "Executive Dashboard",
                "Report",
                None,
                "Weekly KPIs for leadership",
            ),
        ],
    };
    vec![
        ("ws-1".into(), bronze),
        ("ws-2".into(), silver),
        ("ws-3".into(), sandbox),
        ("ws-5".into(), gold),
        ("ws-6".into(), WsData::default()),
    ]
}

/// Role assignments, by workspace id.
pub fn roles() -> Vec<(String, Vec<Value>)> {
    let group = |id: &str, name: &str, role: &str| {
        json!({"id": id, "role": role,
               "principal": {"id": format!("p-{id}"), "displayName": name, "type": "Group",
                             "groupDetails": {"groupType": "SecurityGroup"}}})
    };
    let user = |id: &str, name: &str, upn: &str, role: &str| {
        json!({"id": id, "role": role,
               "principal": {"id": format!("p-{id}"), "displayName": name, "type": "User",
                             "userDetails": {"userPrincipalName": upn}}})
    };
    let app = |id: &str, name: &str, role: &str| {
        json!({"id": id, "role": role,
               "principal": {"id": format!("p-{id}"), "displayName": name,
                             "type": "ServicePrincipal",
                             "servicePrincipalDetails": {
                                 "aadAppId": "00000000-1111-2222-3333-444444444444"}}})
    };
    vec![
        (
            "ws-1".into(),
            vec![group("r-1", "Data Engineering", "Admin")],
        ),
        (
            "ws-2".into(),
            vec![
                group("r-20", "Data Engineering", "Admin"),
                user("r-21", "Avery Example", "avery@contoso.example", "Member"),
                // Guests often come back without a display name.
                user("r-22", "", "sam@fabrikam.example", "Contributor"),
                app("r-23", "etl-automation", "Contributor"),
                group("r-24", "Analysts", "Viewer"),
            ],
        ),
        (
            "ws-3".into(),
            vec![user("r-30", "Dev Example", "dev@contoso.example", "Admin")],
        ),
        (
            "ws-5".into(),
            vec![
                group("r-50", "Data Engineering", "Admin"),
                group("r-51", "Leadership", "Viewer"),
            ],
        ),
        (
            "ws-6".into(),
            vec![group("r-60", "Data Engineering", "Admin")],
        ),
    ]
}

/// Shortcuts, by item id. silver_lh has one of each target type.
pub fn shortcuts() -> Vec<(String, Vec<Value>)> {
    vec![
        (
            "it-2".into(),
            vec![json!({"name": "claims", "path": "Tables",
                "target": {"type": "OneLake", "oneLake": {"workspaceId": "ws-2",
                           "itemId": "it-9", "path": "Tables/claims"}}})],
        ),
        (
            "it-20".into(),
            vec![
                json!({"name": "customers_bronze", "path": "Tables/dbo",
                    "target": {"type": "OneLake", "oneLake": {"workspaceId": "ws-1",
                               "itemId": "it-2", "path": "Tables/customers"}}}),
                json!({"name": "vendor_feed", "path": "Files",
                    "target": {"type": "AdlsGen2", "adlsGen2": {
                        "location": "https://contosolake.dfs.core.windows.net",
                        "subpath": "/vendors/feed", "connectionId": "conn-2"}}}),
                json!({"name": "clickstream", "path": "Files",
                    "target": {"type": "AmazonS3", "amazonS3": {
                        "location": "https://contoso-clicks.s3.us-east-1.amazonaws.com",
                        "subpath": "/2026", "connectionId": "conn-6"}}}),
                json!({"name": "partner_drops", "path": "Files",
                    "target": {"type": "GoogleCloudStorage", "googleCloudStorage": {
                        "location": "https://storage.googleapis.com/fabrikam-drops",
                        "subpath": "/contoso", "connectionId": "conn-7"}}}),
                json!({"name": "archive", "path": "Files",
                    "target": {"type": "S3Compatible", "s3Compatible": {
                        "location": "https://minio.contoso.example", "bucket": "archive",
                        "subpath": "/silver", "connectionId": "conn-8"}}}),
                json!({"name": "accounts", "path": "Tables/dbo",
                    "target": {"type": "Dataverse", "dataverse": {
                        "environmentDomain": "https://contoso.crm.dynamics.com",
                        "tableName": "account", "deltaLakeFolder": "account",
                        "connectionId": "conn-9"}}}),
            ],
        ),
    ]
}

/// What a directory of an item holds: (name, is directory, bytes). Shortcuts
/// show up as directories, as they do in OneLake.
pub fn listing(item: &str, dir: &str) -> Vec<(&'static str, bool, u64)> {
    const GB: u64 = 1024 * 1024 * 1024;
    match (item, dir) {
        // The test fixture.
        ("it-2", "") => vec![("Files", true, 0), ("Tables", true, 0)],
        ("it-2", "Files") => vec![
            ("readme.txt", false, 58),
            ("raw", true, 0),
            ("extract.parquet", false, 9_000),
        ],
        ("it-2", "Files/raw") => vec![("events.json", false, 120)],
        ("it-2", "Tables") => vec![("sales", true, 0), ("customers", true, 0)],
        ("it-2", "Tables/sales" | "Tables/customers") => vec![
            ("_delta_log", true, 0),
            ("part-00000.snappy.parquet", false, 52_000),
        ],
        ("it-2", "Tables/sales/_delta_log" | "Tables/customers/_delta_log") => {
            vec![("00000000000000000000.json", false, 1_900)]
        }

        // A schema-enabled lakehouse: tables sit one level down.
        ("it-20", "") => vec![("Files", true, 0), ("Tables", true, 0)],
        ("it-20", "Tables") => vec![("dbo", true, 0), ("sales", true, 0)],
        ("it-20", "Tables/dbo") => vec![
            ("customers", true, 0),
            ("orders", true, 0),
            ("loyalty_members", true, 0),
            ("customers_bronze", true, 0),
            ("accounts", true, 0),
        ],
        ("it-20", "Tables/sales") => vec![("daily_totals", true, 0)],
        ("it-20", "Files") => vec![
            ("README.md", false, 412),
            ("exports", true, 0),
            ("big_extract.csv", false, 3 * GB + 400 * 1024 * 1024),
            ("events.jsonl", false, 1_840),
            ("logo.png", false, 24_310),
            ("vendor_feed", true, 0),
            ("clickstream", true, 0),
            ("partner_drops", true, 0),
            ("archive", true, 0),
        ],
        ("it-20", "Files/exports") => vec![("2026-10-08", true, 0), ("2026-10-09", true, 0)],
        ("it-20", "Files/exports/2026-10-08") => vec![
            ("orders.parquet", false, 48_000_000),
            ("_SUCCESS", false, 0),
        ],
        ("it-20", "Files/vendor_feed") => vec![("feed_20261009.csv", false, 2_150)],
        ("it-20", "Files/clickstream") => vec![
            ("2026-10-08.json.gz", false, 912_000_000),
            ("2026-10-09.json.gz", false, 344_000_000),
        ],
        ("it-20", "Files/partner_drops") => vec![("stores.csv", false, 18_400)],
        ("it-20", "Files/archive") => vec![("2025", true, 0)],

        // A warehouse has tables only.
        ("it-29", "") => vec![("Tables", true, 0)],
        ("it-29", "Tables") => vec![("dbo", true, 0)],
        ("it-29", "Tables/dbo") => vec![("fact_sales", true, 0), ("dim_date", true, 0)],

        ("it-41", "") => vec![("Files", true, 0), ("Tables", true, 0)],
        ("it-50", "") => vec![("Files", true, 0), ("Tables", true, 0)],
        ("it-50", "Tables") => vec![("weekly_kpis", true, 0)],
        _ => Vec::new(),
    }
}

/// The start of a file, as a range read would return it.
pub fn file(path: &str) -> Cow<'static, [u8]> {
    let text: &'static str = match path {
        "Files/readme.txt" => "Bronze landing zone.\nraw/: one folder per source system.\n",
        "Files/raw/events.json" => "{\"id\": 1, \"kind\": \"admit\"}\n",
        "Files/README.md" => {
            "# silver_lh\n\nConformed silver tables.\n\n\
             | folder | what |\n|---|---|\n\
             | exports/ | daily extracts for partners |\n\
             | vendor_feed/ | shortcut to the vendor's lake |\n\n\
             Tables live under `dbo` and `sales`.\n"
        }
        "Files/events.jsonl" => {
            "{\"ts\": \"2026-10-09T01:00:00Z\", \"event\": \"merge_started\", \"rows\": 0}\n\
             {\"ts\": \"2026-10-09T01:04:12Z\", \"event\": \"merge_progress\", \"rows\": 1250000}\n\
             {\"ts\": \"2026-10-09T01:07:40Z\", \"event\": \"merge_failed\", \"error\": \"schema mismatch\"}\n"
        }
        "Files/vendor_feed/feed_20261009.csv" => {
            "sku,store,qty\tnote\nA-100,12,4\tok\nA-101,12,0\tout of stock\n"
        }
        "Files/big_extract.csv" => return big_csv().into(),
        "Files/exports/2026-10-08/_SUCCESS" => "",
        _ => return Cow::Borrowed(b"PAR1\0\x15\x04binary"),
    };
    Cow::Borrowed(text.as_bytes())
}

/// More than a preview's worth of CSV, so the preview says it was cut.
fn big_csv() -> Vec<u8> {
    let mut out = String::from("order_id,customer_id,store,amount,ordered_at\n");
    for i in 0..1_600 {
        out.push_str(&format!(
            "{},{},{},{}.{:02},2026-10-08T{:02}:{:02}:00Z\n",
            100_000 + i,
            5_000 + i * 7 % 997,
            1 + i % 40,
            10 + i * 13 % 490,
            i % 100,
            i % 24,
            i % 60
        ));
    }
    out.into_bytes()
}

/// Columns of a table, in the OneLake table API's shape.
pub fn columns(schema: &str, table: &str) -> Value {
    let cols = match (schema, table) {
        ("dbo", "customers") | ("dbo", "customers_bronze") => json!([
            {"name": "customer_id", "type_name": "LONG", "nullable": false},
            {"name": "email", "type_name": "STRING", "nullable": false},
            {"name": "loyalty_number", "type_name": "STRING"},
            {"name": "first_seen", "type_name": "DATE"},
            {"name": "sources", "type_name": "ARRAY", "type_text": "array<string>"}
        ]),
        ("dbo", "orders") | ("dbo", "fact_sales") => json!([
            {"name": "order_id", "type_name": "LONG", "nullable": false},
            {"name": "customer_id", "type_name": "LONG"},
            {"name": "store", "type_name": "INT"},
            {"name": "amount", "type_name": "DECIMAL", "type_precision": 18, "type_scale": 2},
            {"name": "ordered_at", "type_name": "TIMESTAMP"}
        ]),
        ("dbo", "loyalty_members") => json!([
            {"name": "loyalty_number", "type_name": "STRING", "nullable": false},
            {"name": "tier", "type_name": "STRING"},
            {"name": "points", "type_name": "LONG"},
            {"name": "joined", "type_name": "DATE"}
        ]),
        ("dbo", "accounts") => json!([
            {"name": "accountid", "type_name": "STRING", "nullable": false},
            {"name": "name", "type_name": "STRING"},
            {"name": "revenue", "type_name": "DECIMAL", "type_precision": 19, "type_scale": 4},
            {"name": "statecode", "type_name": "INT"},
            {"name": "modifiedon", "type_name": "TIMESTAMP"}
        ]),
        ("dbo", "dim_date") => json!([
            {"name": "date_key", "type_name": "INT", "nullable": false},
            {"name": "date", "type_name": "DATE"},
            {"name": "is_weekend", "type_name": "BOOLEAN"}
        ]),
        ("sales", "daily_totals") | ("dbo", "weekly_kpis") => json!([
            {"name": "day", "type_name": "DATE", "nullable": false},
            {"name": "region", "type_name": "STRING"},
            {"name": "revenue", "type_name": "DECIMAL", "type_precision": 18, "type_scale": 2},
            {"name": "orders", "type_name": "LONG"}
        ]),
        _ => json!([
            {"name": "id", "type_name": "LONG", "nullable": false},
            {"name": "amount", "type_name": "DECIMAL", "type_precision": 18, "type_scale": 2},
            {"name": "loaded_at", "type_name": "TIMESTAMP"}
        ]),
    };
    json!({"name": table, "schema_name": schema, "columns": cols})
}

fn run(
    item: &str,
    n: u32,
    job_type: &str,
    status: &str,
    invoke: &str,
    start: Option<&str>,
) -> Value {
    let mut v = json!({"id": format!("{item}-{n}"), "itemId": item, "status": status,
                       "jobType": job_type, "invokeType": invoke});
    if let Some(s) = start {
        v["startTimeUtc"] = s.into();
    }
    v
}

fn ended(mut run: Value, end: &str) -> Value {
    run["endTimeUtc"] = end.into();
    run
}

fn failed(mut run: Value, code: &str, message: &str) -> Value {
    run["failureReason"] = json!({"errorCode": code, "message": message});
    run
}

/// Job history of an item, newest first.
pub fn runs(item: &str, job_type: &str) -> Vec<Value> {
    let r = |n, status, invoke, start| run(item, n, job_type, status, invoke, start);
    match item {
        // Every state Fabric reports, and one cancel that only the Spark
        // session knows about (see `sessions`).
        "it-21" => vec![
            r(12, "InProgress", "Manual", Some("2026-10-09T12:58:00")),
            ended(
                r(11, "Completed", "Manual", Some("2026-10-09T09:15:00")),
                "2026-10-09T09:16:20",
            ),
            failed(
                ended(
                    r(10, "Failed", "Scheduled", Some("2026-10-09T01:00:00")),
                    "2026-10-09T01:07:41",
                ),
                "NotebookExecutionFailed",
                "AnalysisException: [DELTA_FAILED_TO_MERGE_FIELDS] Failed to merge fields \
                 'loyalty_number' and 'loyalty_number'\n\tat org.apache.spark.sql.delta...",
            ),
            ended(
                r(9, "Completed", "Scheduled", Some("2026-10-08T13:00:00")),
                "2026-10-08T13:11:02",
            ),
            ended(
                r(8, "Completed", "Scheduled", Some("2026-10-08T01:00:00")),
                "2026-10-08T01:09:47",
            ),
            ended(
                r(7, "Deduped", "Scheduled", Some("2026-10-07T13:00:00")),
                "2026-10-07T13:00:01",
            ),
            failed(
                ended(
                    r(6, "Failed", "Scheduled", Some("2026-10-07T01:00:00")),
                    "2026-10-07T01:20:00",
                ),
                "SparkSessionTimedOut",
                "",
            ),
            ended(
                r(5, "Completed", "Scheduled", Some("2026-10-06T13:00:00")),
                "2026-10-06T13:08:55",
            ),
            ended(
                r(4, "Completed", "Scheduled", Some("2026-10-06T01:00:00")),
                "2026-10-06T02:41:13",
            ),
        ],
        "it-22" => vec![
            ended(
                r(3, "Completed", "Scheduled", Some("2026-10-09T01:08:00")),
                "2026-10-09T01:14:30",
            ),
            ended(
                r(2, "Completed", "Scheduled", Some("2026-10-08T01:08:00")),
                "2026-10-08T01:13:58",
            ),
        ],
        "it-23" => vec![
            r(6, "NotStarted", "Scheduled", None),
            failed(
                ended(
                    r(5, "Failed", "Scheduled", Some("2026-10-09T01:00:00")),
                    "2026-10-09T01:08:02",
                ),
                "ActivityFailed",
                "Activity 'merge_customers' failed: Notebook execution failed",
            ),
            ended(
                r(4, "Cancelled", "Manual", Some("2026-10-08T15:30:00")),
                "2026-10-08T15:31:10",
            ),
            ended(
                r(3, "Completed", "Scheduled", Some("2026-10-08T01:00:00")),
                "2026-10-08T01:27:44",
            ),
        ],
        "it-24" => vec![
            r(2, "InProgress", "Scheduled", Some("2026-10-09T08:00:00")),
            ended(
                r(1, "Completed", "Scheduled", Some("2026-10-08T08:00:00")),
                "2026-10-08T12:47:09",
            ),
        ],
        "it-40" => vec![ended(
            r(1, "Completed", "Manual", Some("2026-10-09T10:30:00")),
            "2026-10-09T10:31:12",
        )],
        "it-26" | "it-42" => Vec::new(),
        // The test fixture: the same three runs for every item.
        _ => vec![
            r(3, "InProgress", "Scheduled", Some("2026-10-09T13:02:00")),
            failed(
                ended(
                    r(2, "Failed", "Scheduled", Some("2026-10-09T01:00:00")),
                    "2026-10-09T01:12:00",
                ),
                "NotebookExecutionFailed",
                "Py4JJavaError: table not found",
            ),
            ended(
                r(1, "Completed", "Scheduled", Some("2026-10-08T01:00:00")),
                "2026-10-08T01:09:00",
            ),
        ],
    }
}

/// Spark (Livy) sessions: the only record that a run was cancelled by hand.
pub fn sessions() -> Vec<Value> {
    vec![json!({
        "id": "livy-1", "jobInstanceId": "it-21-11", "state": "Cancelled",
        "cancellationReason": "User cancelled the Spark session from the monitoring hub",
        "itemId": "it-21"
    })]
}

pub fn schedules(item: &str) -> Vec<Value> {
    match item {
        "it-21" => vec![
            json!({"id": "sch-21", "enabled": true, "configuration": {"type": "Daily",
                   "startDateTime": "2026-01-01T00:00:00", "endDateTime": "2027-12-31T23:59:00",
                   "localTimeZoneId": "Eastern Standard Time", "times": ["01:00", "13:00"]}}),
        ],
        "it-23" => vec![
            json!({"id": "sch-23a", "enabled": true, "configuration": {"type": "Weekly",
                   "startDateTime": "2026-03-01T00:00:00", "endDateTime": "2027-03-01T00:00:00",
                   "localTimeZoneId": "UTC", "times": ["01:00"],
                   "weekdays": ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"]}}),
            json!({"id": "sch-23b", "enabled": false, "configuration": {"type": "Monthly",
                   "startDateTime": "2026-03-01T00:00:00", "endDateTime": "2027-03-01T00:00:00",
                   "localTimeZoneId": "UTC", "times": ["06:00"],
                   "recurrence": 1, "occurrence": {"occurrenceType": "DayOfMonth",
                                                   "dayOfMonth": 1}},
                   "enabledDetail": {"enabledControlType": "UserDisabled"}}),
            json!({"id": "sch-23c", "enabled": false, "configuration": {"type": "Cron",
                   "interval": 15, "startDateTime": "2026-09-01T00:00:00",
                   "endDateTime": "2026-12-01T00:00:00", "localTimeZoneId": "UTC"},
                   "enabledDetail": {"enabledControlType": "AutoDisabled"}}),
        ],
        "it-24" => vec![
            json!({"id": "sch-24", "enabled": true, "configuration": {"type": "Cron",
                   "interval": 1440, "startDateTime": "2026-05-01T08:00:00",
                   "endDateTime": "2027-05-01T00:00:00", "localTimeZoneId": "UTC"}}),
        ],
        // The test fixture's items all share these.
        "it-1" | "it-3" | "it-5" => vec![
            json!({"id": "sch-1", "enabled": true, "configuration": {"type": "Daily",
                   "startDateTime": "2026-06-09T00:00:00", "endDateTime": "2027-06-09T00:00:00",
                   "localTimeZoneId": "Eastern Standard Time", "times": ["01:00"]}}),
            json!({"id": "sch-2", "enabled": false, "configuration": {"type": "Cron", "interval": 30},
                   "enabledDetail": {"enabledControlType": "AutoDisabled"}}),
        ],
        _ => Vec::new(),
    }
}

fn platform(item: &Value) -> Part {
    let text = format!(
        "{{\n  \"metadata\": {{\"type\": \"{}\", \"displayName\": \"{}\"}},\n  \
         \"config\": {{\"version\": \"2.0\", \"logicalId\": \"00000000-0000-0000-0000-000000000000\"}}\n}}\n",
        item["type"].as_str().unwrap_or(""),
        item["displayName"].as_str().unwrap_or("")
    );
    Part {
        path: ".platform".into(),
        text,
    }
}

fn part(path: &str, text: &str) -> Part {
    Part {
        path: path.into(),
        text: text.into(),
    }
}

/// An item's definition, decoded, as `y` shows it.
pub fn definition(item: &Value) -> Result<Vec<Part>> {
    let id = item["id"].as_str().unwrap_or("");
    let name = item["displayName"].as_str().unwrap_or("");
    let mut parts = match item["type"].as_str().unwrap_or("") {
        "Notebook" if id == "it-21" => vec![part(
            "notebook-content.py",
            "# Fabric notebook source\n\n\
             # METADATA ********************\n\n\
             # META {\n#   \"kernel_info\": {\"name\": \"synapse_pyspark\"},\n\
             #   \"dependencies\": {\"lakehouse\": {\"default_lakehouse_name\": \"silver_lh\"}}\n# META }\n\n\
             # CELL ********************\n\n\
             from delta.tables import DeltaTable\n\n\
             sources = [\"shop\", \"app\", \"stores\"]\n\
             frames = [spark.table(f\"bronze_lh.{s}_customers\") for s in sources]\n\
             incoming = frames[0].unionByName(*frames[1:], allowMissingColumns=True)\n\n\
             # CELL ********************\n\n\
             target = DeltaTable.forName(spark, \"dbo.customers\")\n\
             (target.alias(\"t\")\n\
             \x20   .merge(incoming.alias(\"s\"), \"t.email = s.email\")\n\
             \x20   .whenMatchedUpdateAll()\n\
             \x20   .whenNotMatchedInsertAll()\n\
             \x20   .execute())\n",
        )],
        "Notebook" => {
            return Ok(vec![
                Part {
                    path: "notebook-content.py".into(),
                    text: format!("# Fabric notebook source\n\n# CELL\nprint(\"{id}\")\n"),
                },
                part(
                    ".platform",
                    "{\n  \"metadata\": {\"type\": \"Notebook\"}\n}\n",
                ),
            ]);
        }
        "DataPipeline" => vec![part(
            "pipeline-content.json",
            "{\n  \"properties\": {\n    \"activities\": [\n      {\n        \
             \"name\": \"merge_customers\",\n        \"type\": \"TridentNotebook\",\n        \
             \"typeProperties\": {\"notebookId\": \"it-21\", \"workspaceId\": \"ws-2\"}\n      },\n      {\n        \
             \"name\": \"merge_orders\",\n        \"type\": \"TridentNotebook\",\n        \
             \"dependsOn\": [{\"activity\": \"merge_customers\", \"dependencyConditions\": [\"Succeeded\"]}],\n        \
             \"typeProperties\": {\"notebookId\": \"it-22\", \"workspaceId\": \"ws-2\"}\n      }\n    ]\n  }\n}\n",
        )],
        "SparkJobDefinition" => vec![part(
            "SparkJobDefinitionV1.json",
            "{\n  \"executableFile\": \"abfss://ws-2@onelake.dfs.fabric.microsoft.com/it-20/Files/jobs/compact.py\",\n  \
             \"defaultLakehouseArtifactId\": \"it-20\",\n  \"commandLineArguments\": \"--tables orders,customers --vacuum-hours 168\",\n  \
             \"language\": \"Python\",\n  \"environmentArtifactId\": \"it-25\"\n}\n",
        )],
        "Environment" => vec![
            part(
                "Libraries/PublicLibraries/environment.yml",
                "dependencies:\n  - pip:\n    - great-expectations==1.2.4\n    - rapidfuzz==3.10.1\n",
            ),
            part(
                "Setting/Sparkcompute.yml",
                "enable_native_execution_engine: true\ndriver_cores: 8\ndriver_memory: 56g\n\
                 executor_cores: 8\nexecutor_memory: 56g\ndynamic_executor_allocation:\n  \
                 enabled: true\n  min_executors: 1\n  max_executors: 6\nruntime_version: 1.3\n",
            ),
        ],
        "Lakehouse" => vec![part(
            "lakehouse.metadata.json",
            "{\"defaultSchema\": \"dbo\"}\n",
        )],
        "SemanticModel" => vec![
            part(
                "definition.pbism",
                "{\n  \"version\": \"4.2\",\n  \"settings\": {}\n}\n",
            ),
            part(
                "definition/model.tmdl",
                "model Model\n\tculture: en-US\n\tdefaultPowerBIDataSourceVersion: powerBI_V3\n\n\
                 ref table Sales\nref table Date\n",
            ),
            part(
                "definition/tables/Sales.tmdl",
                "table Sales\n\n\tmeasure 'Total Revenue' = SUM(Sales[amount])\n\
                 \t\tformatString: \\$#,0.00\n\n\tcolumn amount\n\t\tdataType: decimal\n\n\
                 \tcolumn ordered_at\n\t\tdataType: dateTime\n\n\
                 \tpartition Sales = entity\n\t\tmode: directLake\n\t\tsource\n\
                 \t\t\tentityName: orders\n\t\t\tschemaName: dbo\n",
            ),
        ],
        "Report" => vec![
            part(
                "definition.pbir",
                "{\n  \"version\": \"4.0\",\n  \"datasetReference\": {\"byConnection\": \
                 {\"connectionString\": \"semanticmodelid=it-27\"}}\n}\n",
            ),
            part(
                "report.json",
                "{\n  \"config\": \"{}\",\n  \"sections\": [\n    {\"displayName\": \"Overview\", \"visualContainers\": []},\n    \
                 {\"displayName\": \"By region\", \"visualContainers\": []}\n  ]\n}\n",
            ),
        ],
        "VariableLibrary" => vec![part(
            "variables.json",
            "{\n  \"variables\": [\n    {\"name\": \"lakehouse\", \"type\": \"String\", \"value\": \"silver_lh\"},\n    \
             {\"name\": \"batch_size\", \"type\": \"Integer\", \"value\": 50000}\n  ]\n}\n",
        )],
        t => bail!(
            "400 /workspaces/{}/items/{id}/getDefinition: OperationNotSupportedForItem \
             {t} {name} has no definition to get",
            item["workspaceId"].as_str().unwrap_or("")
        ),
    };
    parts.push(platform(item));
    Ok(parts)
}
