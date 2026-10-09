//! Thin Fabric REST client: bearer auth, continuation paging, 429/503 retry,
//! and long-running operations.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, LOCATION, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;

use crate::auth::{FABRIC, TokenProvider};

pub const API: &str = "https://api.fabric.microsoft.com/v1";

const MAX_RETRIES: u32 = 4;
const MAX_WAIT: Duration = Duration::from_secs(30);
const LRO_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenProvider>,
}

impl Client {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> Client {
        Client::with_base(tokens, API)
    }

    pub fn with_base(tokens: Arc<dyn TokenProvider>, base: impl Into<String>) -> Client {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fablic/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Client {
            http,
            base: base.into().trim_end_matches('/').to_owned(),
            tokens,
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_owned()
        } else {
            format!("{}/{}", self.base, path.trim_start_matches('/'))
        }
    }

    /// Send with auth, retrying throttled and unavailable responses.
    async fn send(&self, build: impl Fn() -> RequestBuilder) -> Result<Response> {
        let mut attempt = 0;
        loop {
            let token = self.tokens.token(FABRIC).await?;
            let resp = build().bearer_auth(token).send().await?;
            let status = resp.status();
            let retryable = status == StatusCode::TOO_MANY_REQUESTS
                || status == StatusCode::SERVICE_UNAVAILABLE;
            if retryable && attempt < MAX_RETRIES {
                attempt += 1;
                let wait = retry_after(resp.headers()).unwrap_or(Duration::from_secs(1 << attempt));
                tracing::warn!(%status, ?wait, "throttled, retrying");
                tokio::time::sleep(wait.min(MAX_WAIT)).await;
                continue;
            }
            if status.is_success() {
                return Ok(resp);
            }
            return Err(self.error(resp).await);
        }
    }

    async fn error(&self, resp: Response) -> anyhow::Error {
        let status = resp.status();
        let url = resp.url().path().to_owned();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let code = body.get("errorCode").and_then(Value::as_str).unwrap_or("");
        let msg = body.get("message").and_then(Value::as_str).unwrap_or("");
        match status {
            StatusCode::UNAUTHORIZED => anyhow!(
                "Fabric rejected the token for {} (401 {code}). Sign in again with `az login`",
                self.tokens.account()
            ),
            StatusCode::FORBIDDEN => anyhow!(
                "{} is not allowed to {url} (403 {code}) {msg}",
                self.tokens.account()
            ),
            _ => anyhow!("{} {url}: {code} {msg}", status.as_u16()),
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let url = self.url(path);
        Ok(self.send(|| self.http.get(&url)).await?.json().await?)
    }

    /// GET every page of a list endpoint and return the elements of `key`
    /// (`value` for almost everything).
    pub async fn list(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut url = self.url(path);
        loop {
            let page = self.get(&url).await?;
            if let Some(Value::Array(items)) = page.get(key) {
                out.extend(items.iter().cloned());
            }
            match page.get("continuationUri").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => url = next.to_owned(),
                _ => return Ok(out),
            }
        }
    }

    /// POST that answers 202 Accepted and points at what it started (job
    /// runs, cancellations). Returns the `Location` header, if any.
    pub async fn post_accepted(&self, path: &str) -> Result<Option<String>> {
        let url = self.url(path);
        let resp = self
            .send(|| self.http.post(&url).header("content-length", "0"))
            .await?;
        Ok(resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned))
    }

    /// POST that may run as a long-running operation. Returns the final result body.
    pub async fn post_lro(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let url = self.url(path);
        let resp = self
            .send(|| {
                let req = self.http.request(Method::POST, &url);
                match &body {
                    Some(b) => req.json(b),
                    None => req.header("content-length", "0"),
                }
            })
            .await?;
        if resp.status() != StatusCode::ACCEPTED {
            let text = resp.text().await?;
            return Ok(if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text)?
            });
        }
        let mut wait = retry_after(resp.headers()).unwrap_or(Duration::from_secs(1));
        let op = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("202 without a Location header"))?
            .to_owned();
        let started = std::time::Instant::now();
        loop {
            tokio::time::sleep(wait.min(Duration::from_secs(5))).await;
            let resp = self.send(|| self.http.get(&op)).await?;
            wait = retry_after(resp.headers()).unwrap_or(Duration::from_secs(2));
            let state: Value = resp.json().await?;
            match state.get("status").and_then(Value::as_str) {
                Some("Succeeded") => return self.get(&format!("{op}/result")).await,
                Some("Failed") => {
                    let err = state.pointer("/error/message").and_then(Value::as_str);
                    bail!("operation failed: {}", err.unwrap_or("no details"));
                }
                _ if started.elapsed() > LRO_TIMEOUT => {
                    bail!("operation still running after 2 minutes")
                }
                _ => {}
            }
        }
    }
}

fn retry_after(h: &HeaderMap) -> Option<Duration> {
    let secs: u64 = h.get(RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub struct StaticToken;

    impl TokenProvider for StaticToken {
        fn token(&self, _: &'static str) -> BoxFuture<'_, Result<String>> {
            Box::pin(async { Ok("t0k".to_owned()) })
        }
        fn account(&self) -> &str {
            "me@example.com"
        }
    }

    pub async fn client(server: &MockServer) -> Client {
        Client::with_base(Arc::new(StaticToken), server.uri())
    }

    #[tokio::test]
    async fn follows_continuation_uri() {
        let server = MockServer::start().await;
        let next = format!("{}/workspaces?continuationToken=abc", server.uri());
        Mock::given(method("GET"))
            .and(path("/workspaces"))
            .and(query_param("continuationToken", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"id": "2"}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/workspaces"))
            .and(header("authorization", "Bearer t0k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value": [{"id": "1"}], "continuationUri": next, "continuationToken": "abc"}),
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let got = client(&server)
            .await
            .list("workspaces", "value")
            .await
            .unwrap();
        assert_eq!(got, vec![json!({"id": "1"}), json!({"id": "2"})]);
    }

    #[tokio::test]
    async fn retries_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(path("/capacities"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(path("/capacities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        assert!(
            client(&server)
                .await
                .list("capacities", "value")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn explains_401_and_api_errors() {
        let server = MockServer::start().await;
        Mock::given(path("/a"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"errorCode": "TokenExpired"})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/b"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                json!({"errorCode": "ItemNotFound", "message": "The item could not be found"}),
            ))
            .mount(&server)
            .await;
        let c = client(&server).await;
        let e = c.get("a").await.unwrap_err().to_string();
        assert!(
            e.contains("me@example.com") && e.contains("az login"),
            "{e}"
        );
        let e = c.get("b").await.unwrap_err().to_string();
        assert_eq!(e, "404 /b: ItemNotFound The item could not be found");
    }

    #[tokio::test]
    async fn polls_long_running_operations() {
        let server = MockServer::start().await;
        let op = format!("{}/operations/op1", server.uri());
        Mock::given(method("POST"))
            .and(path("/items/x/getDefinition"))
            .respond_with(
                ResponseTemplate::new(202)
                    .insert_header("location", op.as_str())
                    .insert_header("retry-after", "0"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/operations/op1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"status": "Running"}))
                    .insert_header("retry-after", "0"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(path("/operations/op1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "Succeeded"})))
            .mount(&server)
            .await;
        Mock::given(path("/operations/op1/result"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"definition": {"parts": []}})),
            )
            .mount(&server)
            .await;
        let got = client(&server)
            .await
            .post_lro("items/x/getDefinition", None)
            .await
            .unwrap();
        assert_eq!(got, json!({"definition": {"parts": []}}));
    }
}
