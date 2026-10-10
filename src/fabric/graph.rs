//! Just enough Microsoft Graph to turn what someone types for a workspace
//! role (an email, an object id, a group or app name) into the principal id
//! and type Fabric wants.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::StatusCode;
use serde_json::Value;

use crate::auth::{GRAPH, TokenProvider};

pub const API: &str = "https://graph.microsoft.com/v1.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub id: String,
    /// Fabric's principal type: `User`, `Group` or `ServicePrincipal`.
    pub kind: String,
    pub name: String,
}

#[derive(Clone)]
pub struct Graph {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenProvider>,
}

fn is_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn principal(v: &Value, kind: &str) -> Principal {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let name = match s("displayName") {
        n if n.is_empty() => s("userPrincipalName"),
        n => n,
    };
    Principal {
        id: s("id"),
        kind: kind.into(),
        name,
    }
}

/// OData string literal: single quotes doubled.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl Graph {
    pub fn new(tokens: Arc<dyn TokenProvider>) -> Graph {
        Graph::with_base(tokens, API)
    }

    pub fn with_base(tokens: Arc<dyn TokenProvider>, base: impl Into<String>) -> Graph {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fablic/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        Graph {
            http,
            base: base.into().trim_end_matches('/').to_owned(),
            tokens,
        }
    }

    /// GET `path`; `None` on 404.
    async fn get(&self, path: &str) -> Result<Option<Value>> {
        let token = self.tokens.token(GRAPH).await?;
        let resp = self
            .http
            .get(format!("{}/{path}", self.base))
            .bearer_auth(token)
            .send()
            .await?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => Ok(Some(resp.json().await?)),
            s => {
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                let msg = body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                bail!(
                    "Microsoft Graph {} as {}: {msg}",
                    s.as_u16(),
                    self.tokens.account()
                )
            }
        }
    }

    async fn search(&self, collection: &str, filter: &str) -> Result<Vec<Value>> {
        let path = format!(
            "{collection}?$filter={}&$select=id,displayName,userPrincipalName",
            urlencoding::encode(filter)
        );
        Ok(match self.get(&path).await? {
            Some(Value::Object(mut o)) => match o.remove("value") {
                Some(Value::Array(v)) => v,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        })
    }

    /// Resolve what was typed. Anything that matches nothing, or more than
    /// one principal, is an error rather than a guess.
    pub async fn resolve(&self, typed: &str) -> Result<Principal> {
        let typed = typed.trim();
        if is_guid(typed) {
            let obj = self
                .get(&format!("directoryObjects/{typed}"))
                .await?
                .ok_or_else(|| anyhow!("no user, group or app has object id {typed}"))?;
            let kind = match obj.get("@odata.type").and_then(Value::as_str) {
                Some("#microsoft.graph.user") => "User",
                Some("#microsoft.graph.group") => "Group",
                Some("#microsoft.graph.servicePrincipal") => "ServicePrincipal",
                other => bail!(
                    "{typed} is a {}, not a user, group or app",
                    other.unwrap_or("?")
                ),
            };
            return Ok(principal(&obj, kind));
        }
        if typed.contains('@') {
            let user = self
                .get(&format!(
                    "users/{}",
                    urlencoding::encode(typed).replace("%40", "@")
                ))
                .await?;
            if let Some(u) = user {
                return Ok(principal(&u, "User"));
            }
            // Guests' UPNs differ from their email.
            let found = self
                .search("users", &format!("mail eq {}", quote(typed)))
                .await?;
            return match &found[..] {
                [u] => Ok(principal(u, "User")),
                [] => bail!("no user {typed}"),
                _ => bail!("{} users have the email {typed}", found.len()),
            };
        }
        let filter = format!("displayName eq {}", quote(typed));
        let (groups, apps) = futures::try_join!(
            self.search("groups", &filter),
            self.search("servicePrincipals", &filter)
        )?;
        let mut found: Vec<Principal> = groups.iter().map(|g| principal(g, "Group")).collect();
        found.extend(apps.iter().map(|a| principal(a, "ServicePrincipal")));
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => bail!("no group or app is named \"{typed}\" (users go by email)"),
            n => bail!("{n} groups/apps are named \"{typed}\"; use the object id"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::client::tests::StaticToken;
    use serde_json::json;
    use wiremock::matchers::{path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn graph(server: &MockServer) -> Graph {
        Graph::with_base(Arc::new(StaticToken), server.uri())
    }

    #[tokio::test]
    async fn resolves_users_ids_and_names() {
        let server = MockServer::start().await;
        Mock::given(path("/users/jane@x.org"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "u1", "displayName": "Jane"})),
            )
            .mount(&server)
            .await;
        let gid = "0b6f0a43-7d6c-4c7e-9a11-3f7d5e9c2b10";
        Mock::given(path(format!("/directoryObjects/{gid}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"@odata.type": "#microsoft.graph.group", "id": gid, "displayName": "DE"}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/groups"))
            .and(query_param("$filter", "displayName eq 'O''Neil team'"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value": [{"id": "g2", "displayName": "O'Neil team"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/servicePrincipals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        let g = graph(&server).await;
        let u = g.resolve("jane@x.org").await.unwrap();
        assert_eq!((u.id.as_str(), u.kind.as_str()), ("u1", "User"));
        let grp = g.resolve(gid).await.unwrap();
        assert_eq!((grp.kind.as_str(), grp.name.as_str()), ("Group", "DE"));
        let named = g.resolve("O'Neil team").await.unwrap();
        assert_eq!(named.id, "g2");
    }

    #[tokio::test]
    async fn refuses_ambiguous_and_missing_names() {
        let server = MockServer::start().await;
        Mock::given(path("/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value": [{"id": "g1", "displayName": "ops"}, {"id": "g2", "displayName": "ops"}]}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/servicePrincipals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        Mock::given(path("/users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        let g = graph(&server).await;
        let e = g.resolve("ops").await.unwrap_err().to_string();
        assert!(e.starts_with("2 groups/apps"), "{e}");
        let e = g.resolve("ghost@x.org").await.unwrap_err().to_string();
        assert_eq!(e, "no user ghost@x.org");
    }
}
