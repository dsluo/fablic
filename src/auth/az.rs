//! Tokens from the az CLI's own login cache, pinned to one account.
//!
//! az picks the identity from the *active* subscription context, so two
//! accounts in one tenant can't be told apart with `--tenant`. Passing
//! `--subscription <id>` selects that subscription's account for a single
//! call without changing the active context, so other tools that rely on it
//! (e.g. an Azure DevOps login) are left alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use futures::future::BoxFuture;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::TokenProvider;

/// Refresh this long before a token actually expires.
const MARGIN: Duration = Duration::from_secs(5 * 60);

pub struct AzCli {
    account: String,
    az: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    subscription: Option<String>,
    /// resource -> (token, expiry as unix seconds)
    tokens: HashMap<&'static str, (String, u64)>,
}

#[derive(Deserialize)]
struct Account {
    id: String,
    user: User,
}

#[derive(Deserialize)]
struct User {
    name: String,
}

#[derive(Deserialize)]
struct Token {
    #[serde(rename = "accessToken")]
    access_token: String,
    expires_on: u64,
}

impl AzCli {
    pub fn new(account: impl Into<String>) -> AzCli {
        AzCli::with_binary(account, "az")
    }

    pub fn with_binary(account: impl Into<String>, az: impl Into<PathBuf>) -> AzCli {
        AzCli {
            account: account.into(),
            az: az.into(),
            state: Mutex::new(State::default()),
        }
    }

    /// Every account az has cached, deduplicated, in az's order.
    pub async fn accounts() -> Result<Vec<String>> {
        let list = list_accounts(&PathBuf::from("az")).await?;
        let mut out: Vec<String> = Vec::new();
        for a in list {
            if !out.contains(&a.user.name) {
                out.push(a.user.name);
            }
        }
        Ok(out)
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        run_az(&self.az, args).await
    }

    /// The subscription id whose account is ours, refusing ids that another
    /// account shares (tenant-level entries use the tenant id as their id).
    async fn resolve_subscription(&self) -> Result<String> {
        let list = list_accounts(&self.az).await?;
        let mine = list
            .iter()
            .find(|a| a.user.name.eq_ignore_ascii_case(&self.account))
            .ok_or_else(|| {
                anyhow!(
                    "az has no login for {}. Run `az login --allow-no-subscriptions` and pick that account",
                    self.account
                )
            })?;
        let shared = list
            .iter()
            .any(|a| a.id == mine.id && !a.user.name.eq_ignore_ascii_case(&self.account));
        if shared {
            bail!(
                "az can't select {} on its own: its context id {} is shared with another account",
                self.account,
                mine.id
            );
        }
        Ok(mine.id.clone())
    }

    async fn fetch(&self, state: &mut State, resource: &str) -> Result<(String, u64)> {
        if state.subscription.is_none() {
            state.subscription = Some(self.resolve_subscription().await?);
        }
        let sub = state.subscription.as_deref().unwrap_or_default();
        let out = self
            .run(&[
                "account",
                "get-access-token",
                "--resource",
                resource,
                "--subscription",
                sub,
                "--output",
                "json",
            ])
            .await
            .with_context(|| format!("getting a token for {} as {}", resource, self.account))?;
        let t: Token = serde_json::from_slice(&out).context("parsing az token output")?;
        Ok((t.access_token, t.expires_on))
    }
}

impl TokenProvider for AzCli {
    fn token(&self, resource: &'static str) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            // Holding the lock across the az call means concurrent requests
            // share one az process instead of each spawning their own.
            let mut state = self.state.lock().await;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            if let Some((tok, exp)) = state.tokens.get(resource)
                && *exp > now + MARGIN.as_secs()
            {
                return Ok(tok.clone());
            }
            let (tok, exp) = self.fetch(&mut state, resource).await?;
            state.tokens.insert(resource, (tok.clone(), exp));
            Ok(tok)
        })
    }

    fn account(&self) -> &str {
        &self.account
    }
}

async fn list_accounts(az: &PathBuf) -> Result<Vec<Account>> {
    let out = run_az(az, &["account", "list", "--all", "--output", "json"]).await?;
    serde_json::from_slice(&out).context("parsing `az account list`")
}

async fn run_az(az: &PathBuf, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(az)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("running {} (is the Azure CLI installed?)", az.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("az {} failed: {}", args[..2].join(" "), first.trim());
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `az` that logs its arguments and answers `account list` / `get-access-token`.
    fn stub_az(dir: &std::path::Path, accounts: &str) -> PathBuf {
        let path = dir.join("az");
        let log = dir.join("calls.log");
        let script = format!(
            r#"#!/bin/sh
echo "$@" >> '{log}'
case "$2" in
  list) echo '{accounts}' ;;
  get-access-token) echo '{{"accessToken":"tok-'$4'","expires_on":4102444800}}' ;;
esac
"#,
            log = log.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const ACCOUNTS: &str = r#"[
      {"id":"sub-a","user":{"name":"admin@fabric.example"}},
      {"id":"tenant-1","user":{"name":"me@devops.example"}}
    ]"#;

    #[tokio::test]
    async fn pins_the_account_by_subscription_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let az = AzCli::with_binary("ADMIN@fabric.example", stub_az(dir.path(), ACCOUNTS));
        assert_eq!(
            az.token(super::super::FABRIC).await.unwrap(),
            "tok-https://api.fabric.microsoft.com"
        );
        az.token(super::super::FABRIC).await.unwrap();
        let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        let calls: Vec<&str> = log.lines().collect();
        assert_eq!(
            calls.len(),
            2,
            "one list, one token, then cached: {calls:?}"
        );
        assert!(calls[1].contains("--subscription sub-a"));
        assert!(
            !log.contains("account set"),
            "must never change the active context"
        );
    }

    #[tokio::test]
    async fn refuses_shared_context_ids_and_unknown_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let shared = r#"[
          {"id":"tenant-1","user":{"name":"a@x"}},
          {"id":"tenant-1","user":{"name":"b@x"}}
        ]"#;
        let az = AzCli::with_binary("a@x", stub_az(dir.path(), shared));
        let err = az
            .token(super::super::FABRIC)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("shared with another account"), "{err}");

        let az = AzCli::with_binary("nobody@x", stub_az(dir.path(), shared));
        let err = az
            .token(super::super::FABRIC)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no login for nobody@x"), "{err}");
    }
}
