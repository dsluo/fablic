//! OneLake: the ADLS Gen2-compatible file system (`onelake.dfs`) and the
//! Unity Catalog-compatible table metadata API (`onelake.table`). Both take
//! storage-audience tokens, not Fabric ones.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use reqwest::StatusCode;
use reqwest::header::RANGE;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::auth::{STORAGE, TokenProvider};

pub const DFS: &str = "https://onelake.dfs.fabric.microsoft.com";
pub const TABLES: &str = "https://onelake.table.fabric.microsoft.com/delta";
const VERSION: &str = "2023-11-03";

#[derive(Clone)]
pub struct OneLake {
    http: reqwest::Client,
    tokens: Arc<dyn TokenProvider>,
    dfs: String,
    tables: String,
}

/// Path inside an item, URL-encoded segment by segment (names can have spaces).
fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|s| urlencoding::encode(s).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl OneLake {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> OneLake {
        OneLake::with_bases(tokens, DFS, TABLES)
    }

    pub fn with_bases(tokens: Arc<dyn TokenProvider>, dfs: &str, tables: &str) -> OneLake {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fablic/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        OneLake {
            http,
            tokens,
            dfs: dfs.trim_end_matches('/').to_owned(),
            tables: tables.trim_end_matches('/').to_owned(),
        }
    }

    async fn get(&self, url: &str, range: Option<String>) -> Result<reqwest::Response> {
        let token = self.tokens.token(STORAGE).await?;
        let mut req = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("x-ms-version", VERSION);
        if let Some(r) = range {
            req = req.header(RANGE, r);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let detail: String = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or(body)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned();
        Err(match status {
            StatusCode::UNAUTHORIZED => anyhow!(
                "OneLake rejected the token for {} (401). Sign in again with `az login`",
                self.tokens.account()
            ),
            StatusCode::FORBIDDEN => {
                anyhow!(
                    "{} can't read this in OneLake (403) {detail}",
                    self.tokens.account()
                )
            }
            StatusCode::NOT_FOUND => anyhow!("not found in OneLake (404) {detail}"),
            s => anyhow!("OneLake {}: {detail}", s.as_u16()),
        })
    }

    /// One directory level under an item. `dir` is relative to the item
    /// (`""` for its root). Entry names come back as `{item}/{path}`.
    pub async fn list(&self, ws: &str, item: &str, dir: &str) -> Result<Vec<Value>> {
        let directory = if dir.is_empty() {
            item.to_owned()
        } else {
            format!("{item}/{dir}")
        };
        let base = format!(
            "{}/{ws}?resource=filesystem&recursive=false&directory={}",
            self.dfs,
            urlencoding::encode(&directory)
        );
        let mut out = Vec::new();
        let mut url = base.clone();
        loop {
            let resp = self.get(&url, None).await?;
            // OneLake returns the continuation in a header, the body, or both.
            let header = resp
                .headers()
                .get("x-ms-continuation")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let page: Value = resp.json().await?;
            if let Some(Value::Array(paths)) = page.get("paths") {
                out.extend(paths.iter().cloned());
            }
            let body = page
                .get("ContinuationToken")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match header.or(body).filter(|t| !t.is_empty()) {
                Some(t) => url = format!("{base}&continuation={}", urlencoding::encode(&t)),
                None => return Ok(out),
            }
        }
    }

    fn file_url(&self, ws: &str, item: &str, path: &str) -> String {
        format!("{}/{ws}/{item}/{}", self.dfs, encode_path(path))
    }

    /// The first `max` bytes of a file.
    pub async fn head(&self, ws: &str, item: &str, path: &str, max: usize) -> Result<Vec<u8>> {
        let url = self.file_url(ws, item, path);
        let resp = self
            .get(&url, Some(format!("bytes=0-{}", max.saturating_sub(1))))
            .await?;
        let bytes = resp.bytes().await?;
        Ok(bytes[..bytes.len().min(max)].to_vec())
    }

    /// Stream a whole file to `dest`. Returns the bytes written.
    pub async fn download(&self, ws: &str, item: &str, path: &str, dest: &Path) -> Result<u64> {
        let url = self.file_url(ws, item, path);
        let resp = self.get(&url, None).await?;
        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("creating {}", dest.display()))?;
        let mut stream = resp.bytes_stream();
        let mut written = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
        }
        file.flush().await?;
        Ok(written)
    }

    /// Table metadata, including columns, from the Delta (Unity Catalog) API.
    pub async fn table(&self, ws: &str, item: &str, schema: &str, table: &str) -> Result<Value> {
        let url = format!(
            "{}/{ws}/{item}/api/2.1/unity-catalog/tables/{}",
            self.tables,
            urlencoding::encode(&format!("{item}.{schema}.{table}"))
        );
        Ok(self.get(&url, None).await?.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::client::tests::StaticToken;
    use serde_json::json;
    use wiremock::matchers::{header, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn lake(server: &MockServer) -> OneLake {
        OneLake::with_bases(
            Arc::new(StaticToken),
            &server.uri(),
            &format!("{}/delta", server.uri()),
        )
    }

    #[tokio::test]
    async fn lists_with_body_continuation() {
        let server = MockServer::start().await;
        Mock::given(path("/w"))
            .and(query_param("directory", "i/Files"))
            .and(query_param("continuation", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"paths": [{"name": "i/Files/b.csv"}], "ContinuationToken": null}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/w"))
            .and(query_param("directory", "i/Files"))
            .and(header("x-ms-version", VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"paths": [{"name": "i/Files/a.csv"}], "ContinuationToken": "next"}),
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let got = lake(&server).await.list("w", "i", "Files").await.unwrap();
        let names: Vec<&str> = got.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["i/Files/a.csv", "i/Files/b.csv"]);
    }

    #[tokio::test]
    async fn head_sends_range_and_encodes_paths() {
        let server = MockServer::start().await;
        Mock::given(path("/w/i/Files/my%20data/a.csv"))
            .and(header("range", "bytes=0-3"))
            .respond_with(ResponseTemplate::new(206).set_body_string("id,n"))
            .mount(&server)
            .await;
        let got = lake(&server)
            .await
            .head("w", "i", "Files/my data/a.csv", 4)
            .await
            .unwrap();
        assert_eq!(got, b"id,n");
    }

    #[tokio::test]
    async fn downloads_to_a_file() {
        let server = MockServer::start().await;
        Mock::given(path("/w/i/Files/a.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string("id,n\n1,2\n"))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.csv");
        let n = lake(&server)
            .await
            .download("w", "i", "Files/a.csv", &dest)
            .await
            .unwrap();
        assert_eq!(n, 9);
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "id,n\n1,2\n");
    }

    #[tokio::test]
    async fn table_by_item_id_and_explains_404() {
        let server = MockServer::start().await;
        Mock::given(path("/delta/w/i/api/2.1/unity-catalog/tables/i.dbo.sales"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"columns": []})))
            .mount(&server)
            .await;
        let l = lake(&server).await;
        assert_eq!(
            l.table("w", "i", "dbo", "sales").await.unwrap(),
            json!({"columns": []})
        );
        let e = l
            .table("w", "i", "dbo", "nope")
            .await
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("not found in OneLake (404)"), "{e}");
    }
}
