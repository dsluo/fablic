//! Fabric-backed `DataSource`.

pub mod client;
pub mod fake;
pub mod onelake;
pub mod rows;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use futures::future::BoxFuture;
use futures::{StreamExt, TryStreamExt};
use serde_json::Value;

use crate::auth::{FABRIC, TokenProvider};
use crate::resources::{Kind, Row, job_type_for};
use crate::source::{Action, DataSource, PREVIEW_BYTES, Part, Query, table_of};
pub use client::Client;
use onelake::OneLake;
use rows::WsData;

/// How long a workspace's folder/item listing is reused while navigating its tree.
const WS_TTL: Duration = Duration::from_secs(60);
/// Parallel requests when listing items across every workspace.
const FAN_OUT: usize = 6;
/// Parallel requests when listing runs of every runnable item in a workspace.
const JOB_FAN_OUT: usize = 4;
/// Capacity names change rarely, and listing capacities can take seconds.
const CAP_TTL: Duration = Duration::from_secs(600);
/// How long the workspace list waits for capacity names before showing ids.
const CAP_WAIT: Duration = Duration::from_millis(1500);

pub struct FabricSource {
    inner: Arc<Inner>,
}

type Cached<T> = Arc<Mutex<Option<(Instant, T)>>>;

struct Inner {
    client: Client,
    lake: OneLake,
    tokens: Arc<dyn TokenProvider>,
    spaces: Mutex<HashMap<String, (Instant, Arc<WsData>)>>,
    capacities: Cached<Vec<Value>>,
}

impl FabricSource {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> FabricSource {
        let lake = OneLake::new(tokens.clone());
        FabricSource::with_clients(Client::new(tokens.clone()), lake, tokens)
    }

    pub fn with_clients(
        client: Client,
        lake: OneLake,
        tokens: Arc<dyn TokenProvider>,
    ) -> FabricSource {
        FabricSource {
            inner: Arc::new(Inner {
                client,
                lake,
                tokens,
                spaces: Mutex::new(HashMap::new()),
                capacities: Arc::default(),
            }),
        }
    }
}

impl Inner {
    async fn workspace(&self, id: &str, force: bool) -> Result<Arc<WsData>> {
        if !force
            && let Some((at, data)) = self.spaces.lock().unwrap().get(id)
            && at.elapsed() < WS_TTL
        {
            return Ok(data.clone());
        }
        let folders_url = format!("workspaces/{id}/folders?recursive=true");
        let items_url = format!("workspaces/{id}/items?recursive=true");
        let (folders, items) = futures::try_join!(
            self.client.list(&folders_url, "value"),
            self.client.list(&items_url, "value"),
        )?;
        let data = Arc::new(WsData { folders, items });
        self.spaces
            .lock()
            .unwrap()
            .insert(id.to_owned(), (Instant::now(), data.clone()));
        Ok(data)
    }

    /// Capacities for naming workspace capacities. Never holds up the caller
    /// for long: a slow listing keeps running in the background and fills the
    /// cache for the next refresh, and until then the old (or no) names are used.
    async fn capacity_names(&self, force: bool) -> Vec<Value> {
        let cached = self.capacities.lock().unwrap().clone();
        if !force
            && let Some((at, caps)) = &cached
            && at.elapsed() < CAP_TTL
        {
            return caps.clone();
        }
        let (client, cache) = (self.client.clone(), self.capacities.clone());
        let task = tokio::spawn(async move {
            let caps = client.list("capacities", "value").await?;
            *cache.lock().unwrap() = Some((Instant::now(), caps.clone()));
            Ok::<_, anyhow::Error>(caps)
        });
        match tokio::time::timeout(CAP_WAIT, task).await {
            Ok(Ok(Ok(caps))) => caps,
            _ => cached.map(|(_, c)| c).unwrap_or_default(),
        }
    }

    async fn fetch(&self, q: Query, force: bool) -> Result<Vec<Row>> {
        let c = &self.client;
        let ws = q.scope.workspace.as_ref().map(|w| w.id.as_str());
        let folder = q.scope.folder.as_ref().map(|f| f.id.as_str());
        Ok(match q.kind {
            Kind::Workspaces => {
                let (list, caps) =
                    futures::join!(c.list("workspaces", "value"), self.capacity_names(force));
                rows::workspaces(&list?, &caps)
            }
            Kind::Items => match ws {
                Some(id) => {
                    let data = self.workspace(id, force).await?;
                    if q.is_flat() {
                        rows::items_flat(&data, folder, q.item_type.as_deref())
                    } else {
                        rows::items_level(&data, folder)
                    }
                }
                None => {
                    let list = c.list("workspaces", "value").await?;
                    let spaces: Vec<(Value, WsData)> = futures::stream::iter(list)
                        .map(|w| async move {
                            let id = w.get("id").and_then(Value::as_str).unwrap_or_default();
                            let data = self.workspace(id, force).await?;
                            Ok::<_, anyhow::Error>((w, (*data).clone()))
                        })
                        .buffer_unordered(FAN_OUT)
                        .try_collect()
                        .await?;
                    rows::items_all(&spaces, q.item_type.as_deref())
                }
            },
            Kind::Roles => {
                let id = ws.context("roles need a workspace")?;
                rows::roles(
                    &c.list(&format!("workspaces/{id}/roleAssignments"), "value")
                        .await?,
                )
            }
            Kind::Capacities => rows::capacities(&c.list("capacities", "value").await?),
            Kind::Connections => rows::connections(&c.list("connections", "value").await?),
            Kind::Gateways => rows::gateways(&c.list("gateways", "value").await?),
            Kind::Domains => rows::domains(&c.list("admin/domains", "domains").await?),
            Kind::Jobs => {
                let ws = ws.context("jobs need a workspace")?;
                match &q.scope.item {
                    Some(item) => {
                        let list = self.runs(ws, &item.id).await?;
                        rows::jobs(&list, None, chrono::Utc::now())
                    }
                    None => self.workspace_runs(ws, force).await?,
                }
            }
            Kind::Schedules => {
                let ws = ws.context("schedules need a workspace")?;
                let item = q.scope.item.as_ref().context("schedules need an item")?;
                let job_type = job_type_for(&item.item_type)
                    .with_context(|| format!("{} items have no schedules", item.item_type))?;
                let path = format!(
                    "workspaces/{ws}/items/{}/jobs/{job_type}/schedules",
                    item.id
                );
                rows::schedules(&c.list(&path, "value").await?)
            }
            Kind::OneLake => {
                let ws = ws.context("OneLake needs a workspace")?;
                let item = q.scope.item.as_ref().context("OneLake needs an item")?;
                let dir = q.scope.path.as_deref().unwrap_or("");
                rows::onelake(&self.lake.list(ws, &item.id, dir).await?, &item.id)
            }
            Kind::Shortcuts => {
                let ws = ws.context("shortcuts need a workspace")?;
                let item = q.scope.item.as_ref().context("shortcuts need an item")?;
                let path = format!("workspaces/{ws}/items/{}/shortcuts", item.id);
                rows::shortcuts(&c.list(&path, "value").await?)
            }
        })
    }

    async fn runs(&self, ws: &str, item: &str) -> Result<Vec<Value>> {
        self.client
            .list(
                &format!("workspaces/{ws}/items/{item}/jobs/instances"),
                "value",
            )
            .await
    }

    /// Runs of every runnable item in a workspace. There is no workspace-wide
    /// endpoint, so this asks each item. Items that fail (e.g. no permission)
    /// are skipped unless every one of them fails.
    async fn workspace_runs(&self, ws: &str, force: bool) -> Result<Vec<Row>> {
        let data = self.workspace(ws, force).await?;
        let runnable: Vec<Value> = data
            .items
            .iter()
            .filter(|i| job_type_for(i["type"].as_str().unwrap_or("")).is_some())
            .cloned()
            .collect();
        let results: Vec<(Value, Result<Vec<Value>>)> = futures::stream::iter(runnable)
            .map(|item| async move {
                let id = item["id"].as_str().unwrap_or_default().to_owned();
                let res = self.runs(ws, &id).await;
                (item, res)
            })
            .buffer_unordered(JOB_FAN_OUT)
            .collect()
            .await;
        let now = chrono::Utc::now();
        let (mut rows, mut errors) = (Vec::new(), Vec::new());
        let total = results.len();
        for (item, res) in results {
            match res {
                Ok(list) => rows.extend(rows::jobs(&list, Some(&item), now)),
                Err(e) => {
                    tracing::warn!(item = %item["displayName"], "listing runs: {e:#}");
                    errors.push(e);
                }
            }
        }
        if total > 0 && errors.len() == total {
            return Err(errors.remove(0));
        }
        rows::sort_runs(&mut rows);
        Ok(rows)
    }

    async fn act(&self, action: Action) -> Result<String> {
        match action {
            Action::RunJob {
                workspace,
                item,
                job_type,
            } => {
                let path = format!(
                    "workspaces/{workspace}/items/{}/jobs/instances?jobType={job_type}",
                    item.id
                );
                let location = self.client.post_accepted(&path).await?;
                let run = location
                    .as_deref()
                    .and_then(|l| l.rsplit('/').next())
                    .unwrap_or("?");
                Ok(format!("started {} run {run}", item.name))
            }
            Action::CancelJob {
                workspace,
                item,
                job,
            } => {
                let path = format!(
                    "workspaces/{workspace}/items/{}/jobs/instances/{job}/cancel",
                    item.id
                );
                self.client.post_accepted(&path).await?;
                Ok(format!("cancel requested for {} run {job}", item.name))
            }
        }
    }

    async fn definition(&self, ws: &str, item: &str) -> Result<Vec<Part>> {
        let res = self
            .client
            .post_lro(&format!("workspaces/{ws}/items/{item}/getDefinition"), None)
            .await?;
        decode_parts(&res)
    }
}

/// Decode the base64 parts of a `getDefinition` result.
pub fn decode_parts(res: &Value) -> Result<Vec<Part>> {
    let parts = res
        .pointer("/definition/parts")
        .and_then(Value::as_array)
        .context("definition has no parts")?;
    parts
        .iter()
        .map(|p| {
            let path = p
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned();
            let payload = p.get("payload").and_then(Value::as_str).unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .with_context(|| format!("decoding {path}"))?;
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => format!("<{} bytes of binary data>", e.as_bytes().len()),
            };
            Ok(Part { path, text })
        })
        .collect()
}

impl DataSource for FabricSource {
    fn fetch(&self, q: Query, force: bool) -> BoxFuture<'static, Result<Vec<Row>>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.fetch(q, force).await })
    }

    fn definition(&self, ws: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.definition(&ws, &item).await })
    }

    fn preview(
        &self,
        ws: String,
        item: String,
        path: String,
        total: Option<u64>,
    ) -> BoxFuture<'static, Result<Part>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let bytes = inner.lake.head(&ws, &item, &path, PREVIEW_BYTES).await?;
            let text = rows::preview_text(&bytes, total, PREVIEW_BYTES);
            Ok(Part { path, text })
        })
    }

    fn schema(&self, ws: String, item: String, path: String) -> BoxFuture<'static, Result<Part>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let (schema, table) = table_of(&path)?;
            let meta = inner.lake.table(&ws, &item, &schema, &table).await?;
            Ok(Part {
                path: format!("{schema}.{table}"),
                text: rows::schema_text(&meta),
            })
        })
    }

    fn download(
        &self,
        ws: String,
        item: String,
        path: String,
        dest: std::path::PathBuf,
    ) -> BoxFuture<'static, Result<u64>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.lake.download(&ws, &item, &path, &dest).await })
    }

    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.act(action).await })
    }

    fn warm(&self) -> BoxFuture<'static, Result<()>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.tokens.token(FABRIC).await.map(|_| ()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Ref, Scope};
    use client::tests::StaticToken;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn source(server: &MockServer) -> FabricSource {
        let tokens: Arc<dyn TokenProvider> = Arc::new(StaticToken);
        let lake = OneLake::with_bases(tokens.clone(), &server.uri(), &server.uri());
        FabricSource::with_clients(
            Client::with_base(tokens.clone(), server.uri()),
            lake,
            tokens,
        )
    }

    #[tokio::test]
    async fn folder_levels_reuse_one_listing() {
        let server = MockServer::start().await;
        Mock::given(path("/workspaces/w1/folders"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value": [{"id": "f1", "displayName": "ingest"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "i1", "displayName": "nb", "type": "Notebook", "folderId": "f1"}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let mut q = Query::new(
            Kind::Items,
            Scope {
                workspace: Some(Ref {
                    id: "w1".into(),
                    name: "W".into(),
                }),
                ..Scope::default()
            },
        );
        let root = src.fetch(q.clone(), false).await.unwrap();
        assert_eq!(root[0].name, "ingest");
        q.scope.folder = Some(Ref {
            id: "f1".into(),
            name: "ingest".into(),
        });
        let inside = src.fetch(q, false).await.unwrap();
        assert_eq!(inside[0].name, "nb");
        // `expect(1)` on each mock is verified when the server drops.
    }

    fn ws_scope() -> Scope {
        Scope {
            workspace: Some(Ref {
                id: "w1".into(),
                name: "W".into(),
            }),
            ..Scope::default()
        }
    }

    #[tokio::test]
    async fn workspace_jobs_fan_out_and_skip_failing_items() {
        let server = MockServer::start().await;
        Mock::given(path("/workspaces/w1/folders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "nb", "displayName": "load", "type": "Notebook"},
                {"id": "pl", "displayName": "orchestrate", "type": "DataPipeline"},
                {"id": "rpt", "displayName": "sales", "type": "Report"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items/nb/jobs/instances"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": "r1", "status": "Completed", "startTimeUtc": "2026-10-09T01:00:00",
                 "endTimeUtc": "2026-10-09T01:05:00"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(path("/workspaces/w1/items/pl/jobs/instances"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"errorCode": "Forbidden"})),
            )
            .mount(&server)
            .await;
        let src = source(&server).await;
        let rows = src
            .fetch(Query::new(Kind::Jobs, ws_scope()), false)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "load");
        assert_eq!(rows[0].cells[5], "5m00s");
    }

    #[tokio::test]
    async fn run_and_cancel_post_to_job_endpoints() {
        let server = MockServer::start().await;
        let started = format!(
            "{}/workspaces/w1/items/nb/jobs/instances/run-9",
            server.uri()
        );
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items/nb/jobs/instances"))
            .and(query_param("jobType", "RunNotebook"))
            .respond_with(ResponseTemplate::new(202).insert_header("location", started.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workspaces/w1/items/nb/jobs/instances/run-9/cancel"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        let src = source(&server).await;
        let item = crate::source::ItemRef {
            id: "nb".into(),
            name: "load".into(),
            item_type: "Notebook".into(),
        };
        let msg = src
            .act(Action::RunJob {
                workspace: "w1".into(),
                item: item.clone(),
                job_type: "RunNotebook".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "started load run run-9");
        let msg = src
            .act(Action::CancelJob {
                workspace: "w1".into(),
                item,
                job: "run-9".into(),
            })
            .await
            .unwrap();
        assert_eq!(msg, "cancel requested for load run run-9");
    }

    #[test]
    fn decodes_definition_parts() {
        let res = json!({"definition": {"parts": [
            {"path": "notebook-content.py", "payload": "cHJpbnQoMSk=", "payloadType": "InlineBase64"}
        ]}});
        let parts = decode_parts(&res).unwrap();
        assert_eq!(parts[0].text, "print(1)");
    }
}
