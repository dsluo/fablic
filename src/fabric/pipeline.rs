//! Parameters a pipeline run was started with. No public API returns them,
//! so this asks the way the Fabric web app does: the tenant's Power BI
//! cluster hands out an MWC token for the Data Integration workload, and the
//! capacity's workload host answers for the run. These endpoints are
//! undocumented, so nothing here runs until `P` turns parameters on.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::{RequestBuilder, StatusCode};
use serde_json::{Value, json};

use super::Client;
use crate::auth::{POWERBI, TokenProvider};

/// Where the tenant's Power BI cluster is looked up.
pub const DISCOVERY: &str = "https://api.powerbi.com";
/// MWC tokens say nothing about when they expire; the web app renews hourly.
const MWC_TTL: Duration = Duration::from_secs(30 * 60);
/// Parallel run lookups.
const FAN_OUT: usize = 4;

#[derive(Clone)]
struct Mwc {
    token: String,
    /// `https://<capacity>.pbidedicated.windows.net`, or a test server.
    base: String,
    capacity: String,
    at: Instant,
}

pub struct PipelineRuns {
    http: reqwest::Client,
    discovery: String,
    tokens: Arc<dyn TokenProvider>,
    fabric: Client,
    cluster: tokio::sync::Mutex<Option<String>>,
    /// Workload token per workspace. Held while one is fetched, so parallel
    /// lookups share it.
    mwc: tokio::sync::Mutex<HashMap<String, Mwc>>,
    /// Limits parallel run lookups; the app asks for every run at once.
    slots: tokio::sync::Semaphore,
}

impl PipelineRuns {
    pub fn new(tokens: Arc<dyn TokenProvider>, fabric: Client) -> PipelineRuns {
        PipelineRuns::with_discovery(tokens, fabric, DISCOVERY)
    }

    pub fn with_discovery(
        tokens: Arc<dyn TokenProvider>,
        fabric: Client,
        discovery: impl Into<String>,
    ) -> PipelineRuns {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fablic/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        PipelineRuns {
            http,
            discovery: discovery.into().trim_end_matches('/').to_owned(),
            tokens,
            fabric,
            cluster: tokio::sync::Mutex::new(None),
            mwc: tokio::sync::Mutex::default(),
            slots: tokio::sync::Semaphore::new(FAN_OUT),
        }
    }

    /// The parameters `run` was started with, as a JSON object.
    pub async fn parameters(&self, ws: &str, run: &str) -> Result<Value> {
        let _slot = self.slots.acquire().await?;
        let found = self.run(ws, run).await?;
        Ok(found
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({})))
    }

    /// The workload's record of a pipeline run. A rejected token is renewed once.
    async fn run(&self, ws: &str, run: &str) -> Result<Value> {
        for attempt in 0..2 {
            let m = self.mwc(ws, attempt > 0).await?;
            let url = format!(
                "{}/webapi/capacities/{}/workloads/DI/DiService/direct/workspaces/{ws}/pipelineruns/{run}",
                m.base, m.capacity
            );
            let resp = self
                .http
                .get(&url)
                .header("authorization", format!("MwcToken {}", m.token))
                .send()
                .await?;
            match resp.status() {
                StatusCode::UNAUTHORIZED if attempt == 0 => continue,
                s if s.is_success() => return Ok(resp.json().await?),
                s => bail!("{} pipeline run {run}", s.as_u16()),
            }
        }
        unreachable!("the second attempt returns")
    }

    async fn mwc(&self, ws: &str, renew: bool) -> Result<Mwc> {
        let mut cached = self.mwc.lock().await;
        if let Some(m) = cached.get(ws)
            && m.at.elapsed() < MWC_TTL
            && !(renew && m.at.elapsed() > Duration::from_secs(5))
        {
            return Ok(m.clone());
        }
        let info = self.fabric.get(&format!("workspaces/{ws}")).await?;
        let capacity = info["capacityId"]
            .as_str()
            .context("the workspace is not on a capacity")?
            .to_uppercase();
        let cluster = self.cluster().await?;
        let body = json!({
            "type": "[Start] GetTokens",
            "workloadType": "DI",
            "workspaceObjectId": ws,
            "capacityObjectId": capacity,
        });
        let got = self
            .send(
                self.http
                    .post(format!("{cluster}/metadata/v201606/generatemwctoken"))
                    .json(&body),
            )
            .await
            .context("getting a pipeline workload token")?;
        let token = got["Token"]
            .as_str()
            .context("no workload token")?
            .to_owned();
        let host = got["TargetUriHost"].as_str().context("no workload host")?;
        let base = if host.starts_with("http") {
            host.trim_end_matches('/').to_owned()
        } else {
            format!("https://{host}")
        };
        let m = Mwc {
            token,
            base,
            capacity,
            at: Instant::now(),
        };
        cached.insert(ws.to_owned(), m.clone());
        Ok(m)
    }

    /// The tenant's Power BI cluster, e.g. `https://wabi-….analysis.windows.net`.
    async fn cluster(&self) -> Result<String> {
        let mut cached = self.cluster.lock().await;
        if let Some(c) = cached.as_ref() {
            return Ok(c.clone());
        }
        let url = format!(
            "{}/spglobalservice/GetOrInsertClusterUrisByTenantLocation",
            self.discovery
        );
        let got = self
            .send(self.http.put(url).json(&json!({})))
            .await
            .context("finding the Power BI cluster")?;
        let c = got["FixedClusterUri"]
            .as_str()
            .context("no cluster in the reply")?
            .trim_end_matches('/')
            .to_owned();
        *cached = Some(c.clone());
        Ok(c)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Value> {
        let token = self.tokens.token(POWERBI).await?;
        let resp = req.bearer_auth(token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("{} {}", status.as_u16(), resp.url().path());
        }
        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::client::tests::StaticToken;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const WS: &str = "00000000-0000-0000-0000-00000000000a";
    const CAP: &str = "00000000-0000-0000-0000-0000000000ca";

    async fn server() -> MockServer {
        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("GET"))
            .and(path(format!("/workspaces/{WS}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id": WS, "capacityId": CAP})),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(
                "/spglobalservice/GetOrInsertClusterUrisByTenantLocation",
            ))
            .and(header("authorization", "Bearer t0k"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"FixedClusterUri": format!("{uri}/")})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/metadata/v201606/generatemwctoken"))
            .and(body_partial_json(
                json!({"workloadType": "DI", "workspaceObjectId": WS, "capacityObjectId": CAP.to_uppercase()}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Token": "mwc", "TargetUriHost": uri})),
            )
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    fn runs_path(run: &str) -> String {
        format!(
            "/webapi/capacities/{}/workloads/DI/DiService/direct/workspaces/{WS}/pipelineruns/{run}",
            CAP.to_uppercase()
        )
    }

    fn source(server: &MockServer) -> PipelineRuns {
        let fabric = Client::with_base(Arc::new(StaticToken), server.uri());
        PipelineRuns::with_discovery(Arc::new(StaticToken), fabric, server.uri())
    }

    #[tokio::test]
    async fn looks_up_runs_with_one_shared_token() {
        let server = server().await;
        Mock::given(path(runs_path("r1")))
            .and(header("authorization", "MwcToken mwc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "runId": "x", "status": "Succeeded",
                "parameters": {"source_name": "orders", "batch": "10"}
            })))
            .mount(&server)
            .await;
        Mock::given(path(runs_path("r2")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "Failed"})))
            .mount(&server)
            .await;
        Mock::given(path(runs_path("r3")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let p = source(&server);
        // In parallel, as the app asks; the token is fetched once (`expect(1)`).
        let (a, b, c) = futures::join!(
            p.parameters(WS, "r1"),
            p.parameters(WS, "r2"),
            p.parameters(WS, "r3"),
        );
        assert_eq!(a.unwrap(), json!({"source_name": "orders", "batch": "10"}));
        assert_eq!(b.unwrap(), json!({}), "a run without parameters");
        assert!(c.unwrap_err().to_string().starts_with("404"));
    }

    #[tokio::test]
    async fn a_workspace_off_capacity_says_so() {
        let server = MockServer::start().await;
        Mock::given(path(format!("/workspaces/{WS}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": WS})))
            .mount(&server)
            .await;
        let e = source(&server).parameters(WS, "r1").await.unwrap_err();
        assert!(format!("{e:#}").contains("not on a capacity"), "{e:#}");
    }
}
