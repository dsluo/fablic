//! Tokens from the az CLI's own login cache, pinned to one account.
//!
//! az picks the identity from the *active* subscription context, so two
//! accounts in one tenant can't be told apart with `--tenant`. Passing
//! `--subscription <id>` selects that subscription's account for a single
//! call without changing the active context, so other tools that rely on it
//! (e.g. an Azure DevOps login) are left alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// `AZURE_CONFIG_DIR` for this profile's az calls; az's default (`~/.azure`) if none.
    config_dir: Option<PathBuf>,
    /// Sign in from the environment when az has no (working) login.
    env_login: Option<EnvLogin>,
    state: Mutex<State>,
    /// Set by `forget`; the next `token` call starts from scratch.
    stale: std::sync::atomic::AtomicBool,
}

/// A service principal from the variables the Azure SDKs' EnvironmentCredential
/// reads: `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and `AZURE_CLIENT_SECRET` or
/// `AZURE_CLIENT_CERTIFICATE_PATH`.
#[derive(Clone)]
pub struct EnvLogin {
    pub client_id: String,
    pub tenant: String,
    secret: Option<String>,
    certificate: Option<PathBuf>,
}

impl std::fmt::Debug for EnvLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvLogin")
            .field("client_id", &self.client_id)
            .field("tenant", &self.tenant)
            .field("secret", &self.secret.as_ref().map(|_| "…"))
            .field("certificate", &self.certificate)
            .finish()
    }
}

impl EnvLogin {
    /// The service principal the environment names, if it names one fully.
    pub fn from_env() -> Option<EnvLogin> {
        EnvLogin::from_vars(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Option<EnvLogin> {
        let login = EnvLogin {
            client_id: var("AZURE_CLIENT_ID")?,
            tenant: var("AZURE_TENANT_ID")?,
            secret: var("AZURE_CLIENT_SECRET"),
            certificate: var("AZURE_CLIENT_CERTIFICATE_PATH").map(PathBuf::from),
        };
        (login.secret.is_some() || login.certificate.is_some()).then_some(login)
    }
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
    #[serde(rename = "tenantId", default)]
    tenant_id: String,
}

#[derive(Deserialize)]
struct User {
    name: String,
    /// `user` or `servicePrincipal`.
    #[serde(rename = "type", default)]
    kind: String,
}

/// One identity az has signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzAccount {
    /// A UPN, or a service principal's client id.
    pub name: String,
    pub tenant: String,
    pub service_principal: bool,
}

/// How to sign a service principal in: fablic can't, it has no secret.
/// One option per line, so it fits a terminal and pastes as one command.
pub fn service_principal_hint(client_id: &str, tenant: &str) -> String {
    let tenant = if tenant.is_empty() {
        "<tenant>"
    } else {
        tenant
    };
    format!(
        "{client_id} is a service principal; fablic doesn't have its secret.\n\
         Sign it in with:\n\n\
         az login --service-principal \\\n\
         \x20 --username {client_id} \\\n\
         \x20 --tenant {tenant} \\\n\
         \x20 --password <client secret>\n\n\
         (For a certificate, use --certificate <path to .pem> instead of --password.)"
    )
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
            config_dir: None,
            env_login: None,
            state: Mutex::new(State::default()),
            stale: Default::default(),
        }
    }

    /// A profile for the environment's service principal, with az logins of
    /// its own in `config_dir` so signing in elsewhere can't drop it.
    pub fn from_env_login(login: EnvLogin, config_dir: PathBuf) -> AzCli {
        AzCli {
            config_dir: Some(config_dir),
            ..AzCli::new(login.client_id.clone())
        }
        .with_env_login(login)
    }

    fn with_env_login(mut self, login: EnvLogin) -> AzCli {
        self.env_login = Some(login);
        self
    }

    /// Sign the environment's service principal in to this profile's az.
    async fn env_sign_in(&self) -> Result<()> {
        let Some(l) = &self.env_login else {
            return Ok(());
        };
        let mut args = vec![
            "login",
            "--service-principal",
            "--username",
            &l.client_id,
            "--tenant",
            &l.tenant,
            "--allow-no-subscriptions",
            "--output",
            "none",
        ];
        let cert;
        match (&l.secret, &l.certificate) {
            (Some(secret), _) => args.extend(["--password", secret]),
            (None, Some(path)) => {
                cert = path.display().to_string();
                args.extend(["--certificate", &cert]);
            }
            (None, None) => bail!("no AZURE_CLIENT_SECRET or AZURE_CLIENT_CERTIFICATE_PATH"),
        }
        if let Some(dir) = &self.config_dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        self.run(&args)
            .await
            .map(|_| ())
            .with_context(|| format!("signing {} in from the environment", l.client_id))
    }

    /// Every identity az has cached, deduplicated, in az's order.
    pub async fn accounts() -> Result<Vec<AzAccount>> {
        let list = list_accounts(&PathBuf::from("az")).await?;
        let mut out: Vec<AzAccount> = Vec::new();
        for a in list {
            if !out.iter().any(|o| o.name == a.user.name) {
                out.push(AzAccount {
                    service_principal: a.user.kind == "servicePrincipal",
                    name: a.user.name,
                    tenant: a.tenant_id,
                });
            }
        }
        Ok(out)
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        run_az_in(&self.az, self.config_dir.as_deref(), args).await
    }

    /// The subscription id whose account is ours, refusing ids that another
    /// account shares (tenant-level entries use the tenant id as their id).
    async fn resolve_subscription(&self) -> Result<String> {
        let mut list = self.list().await?;
        let known = |l: &[Account]| {
            l.iter()
                .any(|a| a.user.name.eq_ignore_ascii_case(&self.account))
        };
        if !known(&list) && self.env_login.is_some() {
            self.env_sign_in().await?;
            list = self.list().await?;
        }
        let mine = list
            .iter()
            .find(|a| a.user.name.eq_ignore_ascii_case(&self.account))
            .ok_or_else(|| {
                anyhow!(
                    "az has no login for {}: run `fablic login {}` and sign in as that account",
                    self.account,
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

    async fn list(&self) -> Result<Vec<Account>> {
        let out = self
            .run(&["account", "list", "--all", "--output", "json"])
            .await?;
        serde_json::from_slice(&out).context("parsing `az account list`")
    }

    /// A token for `resource`; an environment login signs in again once if
    /// az can't produce one (say, after the secret was rotated).
    async fn fetch(&self, state: &mut State, resource: &str) -> Result<(String, u64)> {
        match self.fetch_once(state, resource).await {
            Err(e) if self.env_login.is_some() => {
                tracing::info!(account = %self.account, "signing in from the environment again: {e:#}");
                self.env_sign_in().await?;
                state.subscription = None;
                self.fetch_once(state, resource).await
            }
            r => r,
        }
    }

    async fn fetch_once(&self, state: &mut State, resource: &str) -> Result<(String, u64)> {
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
            if self.stale.swap(false, std::sync::atomic::Ordering::SeqCst) {
                *state = State::default();
                // `l` on an environment profile: sign in afresh.
                if self.env_login.is_some() {
                    self.env_sign_in().await?;
                }
            }
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

    fn forget(&self) {
        self.stale.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `fablic login`: an interactive `az login`, for the tenant az last saw
/// `account` in, after which the previously active az account is made
/// active again (az login always switches to the new login, which would
/// break other tools relying on the active context).
pub async fn login(az: &Path, account: Option<&str>, tenant: Option<&str>) -> Result<()> {
    let az = az.to_path_buf();
    let active = |out: Vec<u8>| -> Option<(String, String)> {
        let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
        Some((
            v["id"].as_str()?.to_owned(),
            v["user"]["name"].as_str()?.to_owned(),
        ))
    };
    let before = run_az(&az, &["account", "show", "--output", "json"])
        .await
        .ok()
        .and_then(active);
    let known = match account {
        None => None,
        Some(acct) => list_accounts(&az).await.ok().and_then(|list| {
            list.into_iter()
                .find(|a| a.user.name.eq_ignore_ascii_case(acct))
        }),
    };
    // An interactive login can't sign a service principal in.
    if let Some(a) = known.as_ref().filter(|a| a.user.kind == "servicePrincipal") {
        bail!("{}", service_principal_hint(&a.user.name, &a.tenant_id));
    }
    let known_tenant = match tenant {
        Some(_) => None,
        None => known.map(|a| a.tenant_id).filter(|t| !t.is_empty()),
    };
    // `--output none`: the subscription list az prints after signing in is noise here.
    let mut args = vec!["login", "--allow-no-subscriptions", "--output", "none"];
    if let Some(t) = tenant.or(known_tenant.as_deref()) {
        args.extend(["--tenant", t]);
    }
    if let Some(a) = account {
        println!("Sign in as {a} in the browser window az opens.");
    }
    let status = Command::new(&az)
        .args(&args)
        // No "pick a subscription" prompt after signing in: fablic chooses
        // subscriptions per account itself.
        .env("AZURE_CORE_LOGIN_EXPERIENCE_V2", "off")
        .status()
        .await
        .with_context(|| format!("running {} (is the Azure CLI installed?)", az.display()))?;
    if !status.success() {
        bail!("az login failed");
    }
    let Some((id, user)) = before else {
        return Ok(());
    };
    let show = || run_az(&az, &["account", "show", "--output", "json"]);
    let is_back =
        |now: Option<(String, String)>| now.is_some_and(|(_, u)| u.eq_ignore_ascii_case(&user));
    let _ = run_az(&az, &["account", "set", "--subscription", &id]).await;
    if is_back(show().await.ok().and_then(active)) {
        return Ok(());
    }
    // Accounts that share a subscription (or tenant) id can't be told apart
    // by `account set`; mark the right entry active in az's profile itself.
    let profile = profile_path();
    let patched = profile
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| make_default(&text, &id, &user));
    match (patched, profile) {
        (Some(text), Some(path)) => {
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?
        }
        _ => {
            eprintln!(
                "note: az no longer has {user} (az login replaced it); sign it in again to use it"
            );
            return Ok(());
        }
    }
    if !is_back(show().await.ok().and_then(active)) {
        eprintln!("note: couldn't make {user} az's active account again; use `az account set`");
    }
    Ok(())
}

/// az's profile: `$AZURE_CONFIG_DIR/azureProfile.json`, or `~/.azure`.
fn profile_path() -> Option<PathBuf> {
    let dir = std::env::var_os("AZURE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().join(".azure")))?;
    Some(dir.join("azureProfile.json"))
}

/// az's profile with only the entry for (`id`, `user`) marked as the active
/// one, or `None` if it has no such entry. Keeps the byte order mark az
/// writes the file with.
fn make_default(text: &str, id: &str, user: &str) -> Option<String> {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
    let mut doc: serde_json::Value = serde_json::from_str(body).ok()?;
    let subs = doc.get_mut("subscriptions")?.as_array_mut()?;
    let is_it = |s: &serde_json::Value| {
        s["id"].as_str() == Some(id)
            && s["user"]["name"]
                .as_str()
                .is_some_and(|u| u.eq_ignore_ascii_case(user))
    };
    if !subs.iter().any(is_it) {
        return None;
    }
    for s in subs.iter_mut() {
        let on = is_it(s);
        s["isDefault"] = on.into();
    }
    Some(format!("{bom}{}", serde_json::to_string_pretty(&doc).ok()?))
}

async fn list_accounts(az: &Path) -> Result<Vec<Account>> {
    let out = run_az(az, &["account", "list", "--all", "--output", "json"]).await?;
    serde_json::from_slice(&out).context("parsing `az account list`")
}

async fn run_az(az: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_az_in(az, None, args).await
}

/// Run az with `AZURE_CONFIG_DIR` set to `config_dir`, if given. Errors name
/// only the first two arguments, so a secret further on never shows up.
async fn run_az_in(az: &Path, config_dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new(az);
    if let Some(dir) = config_dir {
        cmd.env("AZURE_CONFIG_DIR", dir);
    }
    let out = cmd
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

    /// A fake `az` for `login`: logs calls, and `account show` reports
    /// whatever `account set` last chose (starting on sub-a).
    fn stub_login(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("az");
        let log = dir.join("calls.log");
        let cur = dir.join("current");
        std::fs::write(&cur, "sub-a").unwrap();
        let script = format!(
            r#"#!/bin/sh
echo "$@" >> '{log}'
case "$1 $2" in
  "account show") id=$(cat '{cur}'); echo '{{"id":"'$id'","user":{{"name":"admin@fabric.example"}}}}' ;;
  "account list") echo '[{{"id":"sub-a","tenantId":"t-1","user":{{"name":"admin@fabric.example"}}}}]' ;;
  "account set") echo "$4" > '{cur}' ;;
  "login "*) echo other > '{cur}' ;;
esac
"#,
            log = log.display(),
            cur = cur.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn login_uses_the_known_tenant_and_restores_the_active_account() {
        let dir = tempfile::tempdir().unwrap();
        let az = stub_login(dir.path());
        login(&az, Some("ADMIN@fabric.example"), None)
            .await
            .unwrap();
        let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        let calls: Vec<&str> = log.lines().collect();
        assert!(
            calls.contains(&"login --allow-no-subscriptions --output none --tenant t-1"),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"account set --subscription sub-a"),
            "{calls:?}"
        );
        let cur = std::fs::read_to_string(dir.path().join("current")).unwrap();
        assert_eq!(cur.trim(), "sub-a");
    }

    #[test]
    fn env_logins_need_an_id_tenant_and_a_credential() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(
            EnvLogin::from_vars(vars(&[("AZURE_CLIENT_ID", "c"), ("AZURE_TENANT_ID", "t")]))
                .is_none()
        );
        let l = EnvLogin::from_vars(vars(&[
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_CLIENT_SECRET", "hunter2"),
        ]))
        .unwrap();
        assert!(
            !format!("{l:?}").contains("hunter2"),
            "Debug hides the secret"
        );
    }

    /// A fake az that keeps its accounts in `$AZURE_CONFIG_DIR/accounts`:
    /// `login --service-principal` adds one there.
    fn stub_env_az(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("az");
        let log = dir.join("calls.log");
        let script = format!(
            r#"#!/bin/sh
echo "AZURE_CONFIG_DIR=$AZURE_CONFIG_DIR $@" >> '{log}'
acc="$AZURE_CONFIG_DIR/accounts"
case "$1 $2" in
  "account list") cat "$acc" 2>/dev/null || echo '[]' ;;
  "login --service-principal") echo '[{{"id":"t","tenantId":"t","user":{{"name":"c","type":"servicePrincipal"}}}}]' > "$acc" ;;
  "account get-access-token") echo '{{"accessToken":"sp-tok","expires_on":4102444800}}' ;;
esac
"#,
            log = log.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn env_profiles_sign_in_to_their_own_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("own-az");
        let login = EnvLogin {
            client_id: "c".into(),
            tenant: "t".into(),
            secret: Some("hunter2".into()),
            certificate: None,
        };
        let az = AzCli {
            az: stub_env_az(dir.path()),
            ..AzCli::from_env_login(login, own.clone())
        };
        assert_eq!(az.token(super::super::FABRIC).await.unwrap(), "sp-tok");
        let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert!(
            log.lines()
                .all(|l| l.starts_with(&format!("AZURE_CONFIG_DIR={} ", own.display()))),
            "{log}"
        );
        assert!(
            log.contains("login --service-principal --username c --tenant t"),
            "{log}"
        );
        // Signed in once; the next token comes from the cache.
        az.token(super::super::FABRIC).await.unwrap();
        let again = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert_eq!(again.matches("login --service-principal").count(), 1);
        // `l` (forget) signs in afresh.
        az.forget();
        az.token(super::super::FABRIC).await.unwrap();
        let after = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert_eq!(after.matches("login --service-principal").count(), 2);
    }

    #[test]
    fn marks_the_right_account_active_when_ids_are_shared() {
        let text = "\u{feff}{\"subscriptions\": [\
            {\"id\": \"s1\", \"isDefault\": true, \"user\": {\"name\": \"admin@x\", \"type\": \"user\"}},\
            {\"id\": \"s1\", \"isDefault\": false, \"user\": {\"name\": \"48cd\", \"type\": \"servicePrincipal\"}}\
            ], \"installationId\": \"i\"}";
        let out = make_default(text, "s1", "48CD").unwrap();
        assert!(out.starts_with('\u{feff}'));
        let v: serde_json::Value = serde_json::from_str(&out[3..]).unwrap();
        let flags: Vec<bool> = v["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["isDefault"].as_bool().unwrap())
            .collect();
        assert_eq!(flags, [false, true]);
        assert_eq!(v["installationId"], "i");
        assert!(make_default(text, "s1", "gone@x").is_none());
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
