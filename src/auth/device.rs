//! fablic's own sign-in: the OAuth device-code flow against Microsoft Entra,
//! for identities that aren't (or shouldn't be) in the az CLI.
//!
//! `fablic login <profile>` shows a code to enter at microsoft.com/devicelogin
//! and keeps the resulting refresh token in the OS keychain. After that, the
//! refresh token is traded for Fabric, OneLake or Graph tokens as needed, and
//! each new refresh token Entra hands back replaces the stored one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct DeviceLogin {
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

impl DeviceLogin {
    /// A provider for `profile`, picking up a refresh token from an earlier
    /// `fablic login`. `account` is shown until the first login names one.
    pub fn new(
        profile: &str,
        account: Option<&str>,
        settings: Settings,
        store: Arc<dyn SecretStore>,
    ) -> DeviceLogin {
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
        DeviceLogin {
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

    /// Start a device-code sign-in. Show the result, then `finish` it.
    pub async fn start(&self) -> Result<DeviceCode> {
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
    pub async fn finish(&self, code: &DeviceCode) -> Result<String> {
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
                None => {
                    let t: TokenResponse = serde_json::from_value(v)?;
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
                    return Ok(account);
                }
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

impl TokenProvider for DeviceLogin {
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
        let login = DeviceLogin::new("dev", None, settings(&server), store.clone());
        assert_eq!(login.account(), "(not signed in)");
        let code = login.start().await.unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        let account = login.finish(&code).await.unwrap();
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
        let login = DeviceLogin::new("dev", None, settings(&server), store.clone());
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
        let fresh = DeviceLogin::new("dev", None, settings(&server), store.clone());
        let e = fresh.token(FABRIC).await.unwrap_err().to_string();
        assert_eq!(e, "profile dev isn't signed in: run `fablic login dev`");

        store
            .set(
                "dev",
                &json!({"account": "a", "refresh_token": "old"}).to_string(),
            )
            .unwrap();
        let stale = DeviceLogin::new("dev", None, settings(&server), store);
        let e = stale.token(FABRIC).await.unwrap_err().to_string();
        assert_eq!(
            e,
            "the sign-in for dev has expired or was revoked \
             (AADSTS70043: The refresh token has expired.): run `fablic login dev`"
        );
    }

    #[test]
    fn reads_the_account_from_an_id_token() {
        assert_eq!(account_of(&id_token("a@b.c")).as_deref(), Some("a@b.c"));
        assert_eq!(account_of("not-a-jwt"), None);
    }
}
