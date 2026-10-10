//! fablic's own sign-in with Microsoft Entra, for identities that aren't (or
//! shouldn't be) in the az CLI.
//!
//! `fablic login` signs in through the browser the way `az login` does: an
//! authorization code with PKCE, redirected to a one-shot listener on
//! localhost. `fablic login --device-code` shows a code to enter at
//! microsoft.com/devicelogin instead, for machines without a browser (some
//! tenants' Conditional Access blocks that flow). Either way the refresh
//! token goes in the OS keychain, is traded for Fabric, OneLake or Graph
//! tokens as needed, and each new one Entra hands back replaces it.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use super::{FABRIC, TokenProvider};

/// The Azure CLI's public client. Microsoft's own Azure Identity libraries
/// use it as their default for device-code sign-in, and it is pre-consented
/// for Fabric, Storage and Graph in most tenants. A profile can name its own
/// app registration instead (it needs "Allow public client flows").
pub const DEFAULT_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";
/// Any work or school account, when a profile names no tenant.
pub const DEFAULT_TENANT: &str = "organizations";
pub const AUTHORITY: &str = "https://login.microsoftonline.com";

/// How long the browser sign-in may take.
const BROWSER_WAIT: Duration = Duration::from_secs(5 * 60);

/// Refresh this long before a token actually expires.
const MARGIN: Duration = Duration::from_secs(5 * 60);
/// Keychain service name; the profile name is the account.
const SERVICE: &str = "fablic";

/// Where refresh tokens live between runs.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// The OS keychain (macOS Keychain, Windows Credential Manager, Linux kernel keyring).
pub struct Keychain;

impl SecretStore for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match keyring::Entry::new(SERVICE, key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("reading the keychain"),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, key)?
            .set_password(value)
            .context("writing the keychain")
    }

    fn delete(&self, key: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context("deleting from the keychain"),
        }
    }
}

/// What is kept per profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Saved {
    account: String,
    refresh_token: String,
}

/// Where to sign in: tenant and client, plus the authority (tests point it
/// at a mock server).
#[derive(Debug, Clone)]
pub struct Settings {
    pub tenant: String,
    pub client_id: String,
    pub authority: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            tenant: DEFAULT_TENANT.into(),
            client_id: DEFAULT_CLIENT_ID.into(),
            authority: AUTHORITY.into(),
        }
    }
}

impl Settings {
    fn endpoint(&self, what: &str) -> String {
        format!(
            "{}/{}/oauth2/v2.0/{what}",
            self.authority.trim_end_matches('/'),
            self.tenant
        )
    }
}

/// `{resource}/.default` plus a refresh token and the sign-in name.
fn scopes(resource: &str) -> String {
    format!("{resource}/.default offline_access openid profile")
}

/// What the device-code endpoint says to show the user.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    /// Entra's own instructions, e.g. "To sign in, use a web browser to open…".
    pub message: String,
    device_code: String,
    #[serde(default = "five")]
    interval: u64,
    expires_in: u64,
}

fn five() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

/// The sign-in name from an id token's claims (not verified: it only labels
/// the profile, the access tokens are what Fabric checks).
fn account_of(id_token: &str) -> Option<String> {
    let claims = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    ["preferred_username", "upn", "email"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_owned)
}

/// `n` random bytes, base64url: PKCE verifiers and `state` values.
fn random_b64(n: usize) -> Result<String> {
    let mut buf = vec![0u8; n];
    aws_lc_rs::rand::fill(&mut buf).map_err(|_| anyhow!("no system randomness"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// The PKCE S256 challenge for a verifier.
fn challenge(verifier: &str) -> String {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// A browser sign-in in progress: open `url`, then `finish_browser`.
pub struct BrowserLogin {
    pub url: String,
    listeners: Vec<TcpListener>,
    redirect: String,
    verifier: String,
    state: String,
}

/// The query of the request line `GET /?code=…&state=… HTTP/1.1`.
fn query_of(request: &str) -> HashMap<String, String> {
    let target = request.split_whitespace().nth(1).unwrap_or("");
    let query = target.split_once('?').map_or("", |(_, q)| q);
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            let v = urlencoding::decode(&v.replace('+', " "))
                .map(|d| d.into_owned())
                .unwrap_or_default();
            (k.to_owned(), v)
        })
        .collect()
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>fablic</title>\
<body style=\"font:16px system-ui;margin:4em\"><h2>fablic is signed in</h2>\
<p>You can close this tab and go back to the terminal.</p>";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct EntraLogin {
    profile: String,
    settings: Settings,
    store: Arc<dyn SecretStore>,
    http: reqwest::Client,
    /// Shown in the header; the account the last login signed in as.
    account: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    refresh: Option<String>,
    /// resource -> (token, expiry as unix seconds)
    tokens: HashMap<&'static str, (String, u64)>,
}

impl EntraLogin {
    /// A provider for `profile`, picking up a refresh token from an earlier
    /// `fablic login`. `account` is shown until the first login names one.
    pub fn new(
        profile: &str,
        account: Option<&str>,
        settings: Settings,
        store: Arc<dyn SecretStore>,
    ) -> EntraLogin {
        let saved = store
            .get(profile)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Saved>(&s).ok());
        let account = saved
            .as_ref()
            .map(|s| s.account.clone())
            .or_else(|| account.map(str::to_owned))
            .unwrap_or_else(|| "(not signed in)".into());
        EntraLogin {
            profile: profile.to_owned(),
            settings,
            store,
            http: reqwest::Client::builder()
                .user_agent(concat!("fablic/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            account,
            state: Mutex::new(State {
                refresh: saved.map(|s| s.refresh_token),
                tokens: HashMap::new(),
            }),
        }
    }

    async fn post(&self, what: &str, form: &[(&str, &str)]) -> Result<Value> {
        let resp = self
            .http
            .post(self.settings.endpoint(what))
            .form(form)
            .send()
            .await
            .with_context(|| format!("reaching {}", self.settings.authority))?;
        Ok(resp.json().await?)
    }

    /// Start a browser sign-in: listen on a free localhost port and build the
    /// authorize URL that redirects back to it. `hint` pre-fills the account.
    pub async fn start_browser(&self, hint: Option<&str>) -> Result<BrowserLogin> {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("listening on localhost for the sign-in redirect")?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        // Browsers may resolve `localhost` to ::1; answer there too if we can.
        if let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
            listeners.push(v6);
        }
        let redirect = format!("http://localhost:{port}");
        let verifier = random_b64(32)?;
        let state = random_b64(16)?;
        let scope = scopes(FABRIC);
        let mut params = vec![
            ("client_id", self.settings.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("response_mode", "query"),
            ("scope", &scope),
            ("state", &state),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ];
        let challenge = challenge(&verifier);
        params.push(("code_challenge", &challenge));
        if let Some(h) = hint {
            params.push(("login_hint", h));
        }
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect();
        let url = format!(
            "{}?{}",
            self.settings.endpoint("authorize"),
            query.join("&")
        );
        Ok(BrowserLogin {
            url,
            listeners,
            redirect,
            verifier,
            state,
        })
    }

    /// Wait for the browser to come back with a code, trade it for tokens and
    /// keep the refresh token. Returns the account signed in as.
    pub async fn finish_browser(&self, b: BrowserLogin) -> Result<String> {
        let code = tokio::time::timeout(BROWSER_WAIT, wait_for_code(&b.listeners, &b.state))
            .await
            .map_err(|_| anyhow!("no sign-in after 5 minutes; run login again"))??;
        let scope = scopes(FABRIC);
        let v = self
            .post(
                "token",
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &self.settings.client_id),
                    ("code", &code),
                    ("redirect_uri", &b.redirect),
                    ("code_verifier", &b.verifier),
                    ("scope", &scope),
                ],
            )
            .await?;
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            let desc = v
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or(err);
            bail!("{}", desc.lines().next().unwrap_or(desc));
        }
        self.keep(serde_json::from_value(v)?).await
    }

    /// Keep a fresh sign-in: the refresh token (keychain) and the Fabric
    /// token that came with it.
    async fn keep(&self, t: TokenResponse) -> Result<String> {
        let account = t
            .id_token
            .as_deref()
            .and_then(account_of)
            .unwrap_or_else(|| self.profile.clone());
        let refresh = t
            .refresh_token
            .ok_or_else(|| anyhow!("Entra returned no refresh token"))?;
        self.save(&account, &refresh)?;
        let mut state = self.state.lock().await;
        state.refresh = Some(refresh);
        state
            .tokens
            .insert(FABRIC, (t.access_token, now() + t.expires_in));
        Ok(account)
    }

    /// Start a device-code sign-in. Show the result, then `finish_device_code`.
    pub async fn start_device_code(&self) -> Result<DeviceCode> {
        let scope = scopes(FABRIC);
        let v = self
            .post(
                "devicecode",
                &[("client_id", &self.settings.client_id), ("scope", &scope)],
            )
            .await?;
        if let Some(e) = v.get("error_description").and_then(Value::as_str) {
            bail!("{}", e.lines().next().unwrap_or(e));
        }
        Ok(serde_json::from_value(v)?)
    }

    /// Wait for the user to finish signing in, then keep the refresh token.
    /// Returns the account signed in as.
    pub async fn finish_device_code(&self, code: &DeviceCode) -> Result<String> {
        let deadline = now() + code.expires_in;
        let mut interval = code.interval;
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let v = self
                .post(
                    "token",
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("client_id", &self.settings.client_id),
                        ("device_code", &code.device_code),
                    ],
                )
                .await?;
            match v.get("error").and_then(Value::as_str) {
                None => return self.keep(serde_json::from_value(v)?).await,
                Some("authorization_pending") if now() < deadline => {}
                Some("slow_down") => interval += 5,
                Some("authorization_pending" | "expired_token") => {
                    bail!("the code expired before sign-in finished; run login again")
                }
                Some("authorization_declined") => bail!("sign-in was declined"),
                Some(other) => {
                    let desc = v
                        .get("error_description")
                        .and_then(Value::as_str)
                        .unwrap_or(other);
                    bail!("{}", desc.lines().next().unwrap_or(desc))
                }
            }
        }
    }

    fn save(&self, account: &str, refresh: &str) -> Result<()> {
        let saved = Saved {
            account: account.to_owned(),
            refresh_token: refresh.to_owned(),
        };
        self.store
            .set(&self.profile, &serde_json::to_string(&saved)?)
    }

    /// Forget the stored sign-in.
    pub fn logout(&self) -> Result<()> {
        self.store.delete(&self.profile)
    }

    /// Trade the refresh token for an access token to `resource`.
    async fn refresh(&self, state: &mut State, resource: &str) -> Result<(String, u64)> {
        let Some(refresh) = state.refresh.clone() else {
            bail!(
                "profile {} isn't signed in: run `fablic login {}`",
                self.profile,
                self.profile
            );
        };
        let scope = scopes(resource);
        let v = self
            .post(
                "token",
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &self.settings.client_id),
                    ("refresh_token", &refresh),
                    ("scope", &scope),
                ],
            )
            .await?;
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            let desc = v
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or(err);
            let first = desc.lines().next().unwrap_or(desc);
            if err == "invalid_grant" {
                bail!(
                    "the sign-in for {} has expired or was revoked ({first}): run `fablic login {}`",
                    self.profile,
                    self.profile
                );
            }
            bail!("Entra refused a token for {resource}: {first}");
        }
        let t: TokenResponse = serde_json::from_value(v)?;
        // Entra rotates refresh tokens; keep the newest.
        if let Some(new) = t.refresh_token.filter(|r| *r != refresh) {
            if let Err(e) = self.save(&self.account, &new) {
                tracing::warn!(profile = %self.profile, "keeping the new refresh token: {e:#}");
            }
            state.refresh = Some(new);
        }
        Ok((t.access_token, now() + t.expires_in))
    }
}

/// Answer requests on the redirect listeners until one carries our `state`
/// with a code (or an error from Entra). Stray requests (favicons, browser
/// pre-connects) get a 404 and are ignored.
async fn wait_for_code(listeners: &[TcpListener], state: &str) -> Result<String> {
    loop {
        let accepts = listeners.iter().map(|l| Box::pin(l.accept()));
        let (Ok((mut conn, _)), _, _) = futures::future::select_all(accepts).await else {
            continue;
        };
        let mut buf = vec![0u8; 16 * 1024];
        let n = match tokio::time::timeout(Duration::from_secs(10), conn.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let request = String::from_utf8_lossy(&buf[..n]);
        let q = query_of(request.lines().next().unwrap_or(""));
        if q.get("state").map(String::as_str) != Some(state) {
            let _ = conn
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let page = match q.get("error") {
            None => DONE_PAGE.to_owned(),
            Some(_) => {
                "<!doctype html><title>fablic</title><p>Sign-in failed; see the terminal.".into()
            }
        };
        let _ = conn
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                )
                .as_bytes(),
            )
            .await;
        if let Some(err) = q.get("error") {
            let desc = q.get("error_description").unwrap_or(err);
            bail!("sign-in failed: {}", desc.lines().next().unwrap_or(desc));
        }
        return q
            .get("code")
            .cloned()
            .ok_or_else(|| anyhow!("the sign-in redirect had no code"));
    }
}

impl TokenProvider for EntraLogin {
    fn token(&self, resource: &'static str) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            // One refresh at a time, so a rotated refresh token isn't raced.
            let mut state = self.state.lock().await;
            if let Some((tok, exp)) = state.tokens.get(resource)
                && *exp > now() + MARGIN.as_secs()
            {
                return Ok(tok.clone());
            }
            let (tok, exp) = self.refresh(&mut state, resource).await?;
            state.tokens.insert(resource, (tok.clone(), exp));
            Ok(tok)
        })
    }

    fn account(&self) -> &str {
        &self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::STORAGE;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct Memory(StdMutex<HashMap<String, String>>);

    impl SecretStore for Memory {
        fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn settings(server: &MockServer) -> Settings {
        Settings {
            tenant: "contoso".into(),
            authority: server.uri(),
            ..Settings::default()
        }
    }

    fn id_token(upn: &str) -> String {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"preferred_username": upn}).to_string());
        format!("x.{claims}.y")
    }

    fn saved(store: &Memory, profile: &str) -> Saved {
        serde_json::from_str(&store.get(profile).unwrap().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn device_code_login_waits_then_keeps_the_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/contoso/oauth2/v2.0/devicecode"))
            .and(body_string_contains("offline_access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_code": "ABCD-1234", "device_code": "dev", "interval": 0,
                "expires_in": 900, "verification_uri": "https://microsoft.com/devicelogin",
                "message": "To sign in, use a web browser…"
            })))
            .mount(&server)
            .await;
        // Pending twice, then signed in.
        Mock::given(path("/contoso/oauth2/v2.0/token"))
            .and(body_string_contains("device_code"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "authorization_pending"})),
            )
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(path("/contoso/oauth2/v2.0/token"))
            .and(body_string_contains("device_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fabric-1", "expires_in": 3600,
                "refresh_token": "rt-1", "id_token": id_token("jane@contoso.com")
            })))
            .mount(&server)
            .await;
        let store = Arc::new(Memory::default());
        let login = EntraLogin::new("dev", None, settings(&server), store.clone());
        assert_eq!(login.account(), "(not signed in)");
        let code = login.start_device_code().await.unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        let account = login.finish_device_code(&code).await.unwrap();
        assert_eq!(account, "jane@contoso.com");
        assert_eq!(
            saved(&store, "dev"),
            Saved {
                account: "jane@contoso.com".into(),
                refresh_token: "rt-1".into()
            }
        );
        // The first Fabric token came with the login.
        assert_eq!(login.token(FABRIC).await.unwrap(), "fabric-1");
    }

    #[tokio::test]
    async fn refresh_tokens_mint_each_resource_and_rotate() {
        let server = MockServer::start().await;
        Mock::given(path("/contoso/oauth2/v2.0/token"))
            .and(body_string_contains("refresh_token=rt-1"))
            .and(body_string_contains("storage.azure.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "storage-1", "expires_in": 3600, "refresh_token": "rt-2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(Memory::default());
        store
            .set(
                "dev",
                &json!({"account": "jane@contoso.com", "refresh_token": "rt-1"}).to_string(),
            )
            .unwrap();
        let login = EntraLogin::new("dev", None, settings(&server), store.clone());
        assert_eq!(login.account(), "jane@contoso.com");
        assert_eq!(login.token(STORAGE).await.unwrap(), "storage-1");
        // Cached: no second request (expect(1) above).
        assert_eq!(login.token(STORAGE).await.unwrap(), "storage-1");
        assert_eq!(saved(&store, "dev").refresh_token, "rt-2");
    }

    #[tokio::test]
    async fn explains_missing_and_expired_sign_ins() {
        let server = MockServer::start().await;
        Mock::given(path("/contoso/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "AADSTS70043: The refresh token has expired.\r\nTrace ID: x"
            })))
            .mount(&server)
            .await;
        let store = Arc::new(Memory::default());
        let fresh = EntraLogin::new("dev", None, settings(&server), store.clone());
        let e = fresh.token(FABRIC).await.unwrap_err().to_string();
        assert_eq!(e, "profile dev isn't signed in: run `fablic login dev`");

        store
            .set(
                "dev",
                &json!({"account": "a", "refresh_token": "old"}).to_string(),
            )
            .unwrap();
        let stale = EntraLogin::new("dev", None, settings(&server), store);
        let e = stale.token(FABRIC).await.unwrap_err().to_string();
        assert_eq!(
            e,
            "the sign-in for dev has expired or was revoked \
             (AADSTS70043: The refresh token has expired.): run `fablic login dev`"
        );
    }

    #[tokio::test]
    async fn browser_login_trades_the_redirected_code_with_pkce() {
        let server = MockServer::start().await;
        let store = Arc::new(Memory::default());
        let login = EntraLogin::new("dev", None, settings(&server), store.clone());
        let b = login.start_browser(Some("jane@contoso.com")).await.unwrap();
        let url = reqwest::Url::parse(&b.url).unwrap();
        assert_eq!(url.path(), "/contoso/oauth2/v2.0/authorize");
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["login_hint"], "jane@contoso.com");
        assert_eq!(q["code_challenge"], challenge(&b.verifier));
        let (redirect, state) = (q["redirect_uri"].clone(), q["state"].clone());
        assert!(redirect.starts_with("http://localhost:"));

        Mock::given(path("/contoso/oauth2/v2.0/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .and(body_string_contains(format!(
                "code_verifier={}",
                b.verifier
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fabric-1", "expires_in": 3600,
                "refresh_token": "rt-1", "id_token": id_token("jane@contoso.com")
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The "browser": a stray request, a forged state, then the real redirect.
        let browser = tokio::spawn(async move {
            let http = reqwest::Client::new();
            let local = redirect.replace("localhost", "127.0.0.1");
            let stray = http
                .get(format!("{local}/favicon.ico"))
                .send()
                .await
                .unwrap();
            assert_eq!(stray.status(), 404);
            let forged = http
                .get(format!("{local}/?code=evil&state=nope"))
                .send()
                .await
                .unwrap();
            assert_eq!(forged.status(), 404);
            let page = http
                .get(format!("{local}/?code=the-code&state={state}"))
                .send()
                .await
                .unwrap();
            page.text().await.unwrap()
        });
        let account = login.finish_browser(b).await.unwrap();
        assert_eq!(account, "jane@contoso.com");
        assert!(browser.await.unwrap().contains("fablic is signed in"));
        assert_eq!(saved(&store, "dev").refresh_token, "rt-1");
        assert_eq!(login.token(FABRIC).await.unwrap(), "fabric-1");
    }

    #[test]
    fn parses_the_redirect_query() {
        let q = query_of("GET /?code=a%2Fb&state=s&error_description=no+way HTTP/1.1");
        assert_eq!(q["code"], "a/b");
        assert_eq!(q["error_description"], "no way");
        assert!(query_of("GET /favicon.ico HTTP/1.1").is_empty());
    }

    #[test]
    fn reads_the_account_from_an_id_token() {
        assert_eq!(account_of(&id_token("a@b.c")).as_deref(), Some("a@b.c"));
        assert_eq!(account_of("not-a-jwt"), None);
    }
}
