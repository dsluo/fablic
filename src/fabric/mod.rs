//! Fabric-backed `DataSource`.

pub mod client;
pub mod fake;
pub mod rows;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures::future::BoxFuture;
use futures::{StreamExt, TryStreamExt};
use serde_json::Value;

use crate::auth::{FABRIC, TokenProvider};
use crate::resources::{Kind, Row};
use crate::source::{DataSource, Part, Query};
pub use client::Client;
use rows::WsData;

/// How long a workspace's folder/item listing is reused while navigating its tree.
const WS_TTL: Duration = Duration::from_secs(60);
/// Parallel requests when listing items across every workspace.
const FAN_OUT: usize = 6;
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
    tokens: Arc<dyn TokenProvider>,
    spaces: Mutex<HashMap<String, (Instant, Arc<WsData>)>>,
    capacities: Cached<Vec<Value>>,
}

impl FabricSource {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> FabricSource {
        FabricSource::with_client(Client::new(tokens.clone()), tokens)
    }

    pub fn with_client(client: Client, tokens: Arc<dyn TokenProvider>) -> FabricSource {
        FabricSource {
            inner: Arc::new(Inner {
                client,
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
            k @ (Kind::Jobs | Kind::Schedules | Kind::OneLake | Kind::Shortcuts) => {
                bail!("{} is not implemented yet", k.name())
            }
        })
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
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn source(server: &MockServer) -> FabricSource {
        let tokens: Arc<dyn TokenProvider> = Arc::new(StaticToken);
        FabricSource::with_client(Client::with_base(tokens.clone(), server.uri()), tokens)
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

    #[test]
    fn decodes_definition_parts() {
        let res = json!({"definition": {"parts": [
            {"path": "notebook-content.py", "payload": "cHJpbnQoMSk=", "payloadType": "InlineBase64"}
        ]}});
        let parts = decode_parts(&res).unwrap();
        assert_eq!(parts[0].text, "print(1)");
    }
}
