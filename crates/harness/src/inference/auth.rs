//! Shared Codex-compatible ChatGPT credentials and API-key ownership.
//!
//! Orvek-owned secret strings, JSON strings, and buffers zeroize on drop. HTTP/TLS,
//! URL parsing, and serde may retain temporary non-zeroizing copies; this module
//! does not claim to erase memory owned by those dependencies. No provider body,
//! token, callback query, or credential file contents enters an error message.
//! JWT claims are decoded for routing/status only, never treated as verified identity.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_AUTH_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Owned credentials cannot be cloned, formatted for display, or serialized.
///
/// ```compile_fail
/// use orvek_harness::inference::auth::SecretString;
/// let key = SecretString::new(String::new());
/// let retained_copy = key.clone();
/// ```
/// ```compile_fail
/// use orvek_harness::inference::auth::SecretString;
/// println!("{}", SecretString::new(String::new()));
/// ```
/// ```compile_fail
/// use orvek_harness::inference::auth::SecretString;
/// serde_json::to_string(&SecretString::new(String::new())).unwrap();
/// ```
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);
impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMode {
    Auto,
    ApiKey,
    ChatGpt,
}
impl AuthMode {
    pub const fn api_base_url(self) -> &'static str {
        match self {
            Self::ChatGpt => "https://chatgpt.com/backend-api/codex",
            _ => "https://api.openai.com/v1",
        }
    }
    pub const fn websocket_url(self) -> &'static str {
        match self {
            Self::ChatGpt => "wss://chatgpt.com/backend-api/codex/responses",
            _ => "wss://api.openai.com/v1/responses",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    #[error("credentials are empty or malformed")]
    InvalidCredentials,
    #[error("credential file is unavailable")]
    StoreUnavailable,
    #[error("credential file changed during authentication; retry after checking login status")]
    StoreChanged,
    #[error("the shared ChatGPT account changed while this client was active")]
    AccountChanged,
    #[error("ChatGPT login is required")]
    LoginRequired,
    #[error(
        "credential refresh outcome is unknown; log in again or load externally refreshed credentials"
    )]
    RefreshUncertain,
    #[error("authentication transport failed")]
    Transport,
    #[error("authentication request was rejected")]
    Rejected,
    #[error("authentication timed out")]
    Timeout,
    #[error("authentication was cancelled")]
    Cancelled,
    #[error("OAuth callback did not match this login")]
    InvalidCallback,
    #[error("OAuth loopback callback ports are unavailable")]
    CallbackUnavailable,
    #[error("invalid authentication issuer")]
    InvalidIssuer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatGptAuthStatus {
    pub account_id: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub fedramp: bool,
}

/// One shared instance coordinates refresh for concurrent provider calls. File
/// locking coordinates Orvek instances; compare-before-persist detects changes by
/// other shared-file writers. A lost rotating-token response is never retried.
pub struct Auth {
    mode: AuthMode,
    source: Mutex<Source>,
    issuer: Url,
    client: Client,
}
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("mode", &self.mode)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}
impl Zeroize for Auth {
    fn zeroize(&mut self) {
        self.source.get_mut().zeroize();
    }
}
impl Drop for Auth {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for Auth {}

enum Source {
    ApiKey(SecretString),
    ChatGpt(Box<Managed>),
}
impl Zeroize for Source {
    fn zeroize(&mut self) {
        match self {
            Self::ApiKey(key) => key.zeroize(),
            Self::ChatGpt(managed) => managed.document.zeroize(),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for Source {}

struct Managed {
    path: PathBuf,
    document: SecretJson,
    status: ChatGptAuthStatus,
    generation: u64,
    refresh_uncertain: bool,
}

impl Zeroize for Managed {
    fn zeroize(&mut self) {
        self.document.zeroize();
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for Managed {}

impl Auth {
    pub fn api_key(key: SecretString) -> Result<Self, AuthError> {
        if key.expose_secret().trim().is_empty() {
            return Err(AuthError::InvalidCredentials);
        }
        Ok(Self {
            mode: AuthMode::ApiKey,
            source: Mutex::new(Source::ApiKey(key)),
            issuer: issuer(ISSUER)?,
            client: client()?,
        })
    }

    /// Auto prefers a present shared file even if malformed; it never silently
    /// falls back to API billing after a ChatGPT-file error.
    pub fn load(
        mode: AuthMode,
        path: &Path,
        read_api_key: impl FnOnce() -> Result<Option<SecretString>, AuthError>,
    ) -> Result<Self, AuthError> {
        if mode == AuthMode::ChatGpt
            || (mode == AuthMode::Auto
                && path.try_exists().map_err(|_| AuthError::StoreUnavailable)?)
        {
            return Self::chatgpt(path.to_owned());
        }
        Self::api_key(read_api_key()?.ok_or(AuthError::LoginRequired)?)
    }

    pub fn chatgpt(path: PathBuf) -> Result<Self, AuthError> {
        Self::chatgpt_with_issuer(path, ISSUER)
    }

    /// Explicit issuer injection supports a trusted enterprise endpoint or local
    /// fixture. TLS is mandatory except for loopback fake servers.
    pub fn chatgpt_with_issuer(path: PathBuf, auth_issuer: &str) -> Result<Self, AuthError> {
        let document = read_store(&path)?;
        let status = document.status()?;
        Ok(Self {
            mode: AuthMode::ChatGpt,
            source: Mutex::new(Source::ChatGpt(Box::new(Managed {
                path,
                document,
                status,
                generation: 0,
                refresh_uncertain: false,
            }))),
            issuer: issuer(auth_issuer)?,
            client: client()?,
        })
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    pub(crate) async fn headers(&self) -> Result<(HeaderMap, u64), AuthError> {
        let mut source = self.source.lock().await;
        match &mut *source {
            Source::ApiKey(key) => Ok((headers(key.expose_secret(), None)?, 0)),
            Source::ChatGpt(managed) => {
                managed.reload()?;
                if managed.refresh_uncertain {
                    return Err(AuthError::RefreshUncertain);
                }
                let claims = decode_jwt(managed.document.token("access_token")?)?;
                if claims
                    .0
                    .get("exp")
                    .and_then(Value::as_u64)
                    .is_some_and(|expiry| expiry <= unix_now().saturating_add(300))
                {
                    self.refresh(managed).await?;
                }
                Ok((
                    headers(
                        managed.document.token("access_token")?,
                        Some(&managed.status),
                    )?,
                    managed.generation,
                ))
            }
        }
    }

    pub(crate) async fn recover_unauthorized(
        &self,
        rejected_generation: u64,
    ) -> Result<(), AuthError> {
        let mut source = self.source.lock().await;
        let Source::ChatGpt(managed) = &mut *source else {
            return Err(AuthError::LoginRequired);
        };
        managed.reload()?;
        if managed.generation != rejected_generation {
            return Ok(());
        }
        self.refresh(managed).await
    }

    async fn refresh(&self, managed: &mut Managed) -> Result<(), AuthError> {
        if managed.refresh_uncertain {
            return Err(AuthError::RefreshUncertain);
        }
        let _lock = lock_store(&managed.path).await?;
        if managed.reload()? {
            return Ok(());
        }
        let mut request =
            SecretJson(json!({"client_id": CLIENT_ID, "grant_type": "refresh_token"}));
        request.0["refresh_token"] = managed.document.token("refresh_token")?.into();
        let body = request.bytes()?;
        // Set before dispatch so cancellation of this future leaves the source
        // poisoned. Reusing a possibly rotated refresh token is not safe.
        managed.refresh_uncertain = true;
        let refreshed = token_request(&self.client, &self.issuer, body, "application/json").await?;
        let mut next = SecretJson::parse(&managed.document.bytes()?)?;
        for field in ["access_token", "refresh_token", "id_token"] {
            if let Some(value) = refreshed.0.get(field) {
                let token = value
                    .as_str()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or(AuthError::InvalidCredentials)?;
                replace_secret(&mut next.0["tokens"][field], token.into());
            }
        }
        if refreshed
            .0
            .get("access_token")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(AuthError::InvalidCredentials);
        }
        let status = next.status()?;
        if status.account_id != managed.status.account_id {
            return Err(AuthError::AccountChanged);
        }
        next.0["last_refresh"] = chrono::DateTime::from_timestamp(
            unix_now()
                .try_into()
                .map_err(|_| AuthError::InvalidCredentials)?,
            0,
        )
        .ok_or(AuthError::InvalidCredentials)?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .into();
        if read_store(&managed.path)?.0 != managed.document.0 {
            return Err(AuthError::StoreChanged);
        }
        write_store(&managed.path, &next)?;
        managed.document = next;
        managed.status = status;
        managed.generation = managed
            .generation
            .checked_add(1)
            .ok_or(AuthError::StoreChanged)?;
        managed.refresh_uncertain = false;
        Ok(())
    }
}

impl Managed {
    fn reload(&mut self) -> Result<bool, AuthError> {
        let document = read_store(&self.path)?;
        if document.0 == self.document.0 {
            return Ok(false);
        }
        let status = document.status()?;
        if status.account_id != self.status.account_id {
            return Err(AuthError::AccountChanged);
        }
        self.document = document;
        self.status = status;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(AuthError::StoreChanged)?;
        self.refresh_uncertain = false;
        Ok(true)
    }
}

pub fn chatgpt_auth_status(path: impl AsRef<Path>) -> Result<ChatGptAuthStatus, AuthError> {
    read_store(path.as_ref())?.status()
}

/// Synchronous, idempotent removal retains the existing shared logout behavior.
/// Active Orvek clients re-read the path before further requests. A concurrent Orvek
/// refresh holds the shared lock, so logout fails visibly while that write is active.
pub fn logout_chatgpt(path: impl AsRef<Path>) -> Result<bool, AuthError> {
    let path = path.as_ref();
    if !path.try_exists().map_err(|_| AuthError::StoreUnavailable)? {
        return Ok(false);
    }
    let lock = open_store_lock(path)?;
    lock.try_lock_exclusive()
        .map_err(|_| AuthError::StoreUnavailable)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(AuthError::StoreUnavailable),
    }
}

pub struct ChatGptLogin {
    path: PathBuf,
    issuer: Url,
    authorization_url: SecretString,
    redirect_uri: String,
    state: SecretString,
    verifier: SecretString,
    listener: TcpListener,
    client: Client,
}
impl fmt::Debug for ChatGptLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChatGptLogin([REDACTED])")
    }
}
impl Zeroize for ChatGptLogin {
    fn zeroize(&mut self) {
        self.authorization_url.zeroize();
        self.state.zeroize();
        self.verifier.zeroize();
    }
}
impl Drop for ChatGptLogin {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for ChatGptLogin {}

impl ChatGptLogin {
    pub async fn start(path: impl Into<PathBuf>) -> Result<Self, AuthError> {
        Self::start_with_issuer(path.into(), ISSUER, &[1455, 1457]).await
    }

    pub async fn start_with_issuer(
        path: PathBuf,
        auth_issuer: &str,
        callback_ports: &[u16],
    ) -> Result<Self, AuthError> {
        let issuer = issuer(auth_issuer)?;
        let mut listener = None;
        for &port in callback_ports {
            if let Ok(bound) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
                listener = Some(bound);
                break;
            }
        }
        let listener = listener.ok_or(AuthError::CallbackUnavailable)?;
        let redirect_uri = format!(
            "http://localhost:{}/auth/callback",
            listener
                .local_addr()
                .map_err(|_| AuthError::CallbackUnavailable)?
                .port()
        );
        let state = SecretString::new(random_urlsafe());
        let verifier = SecretString::new(random_urlsafe());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose_secret().as_bytes()));
        let mut url = issuer
            .join("oauth/authorize")
            .map_err(|_| AuthError::InvalidIssuer)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair(
                "scope",
                "openid profile email offline_access api.connectors.read api.connectors.invoke",
            )
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", "tact")
            .append_pair("state", state.expose_secret());
        Ok(Self {
            path,
            issuer,
            authorization_url: SecretString::new(url.into()),
            redirect_uri,
            state,
            verifier,
            listener,
            client: client()?,
        })
    }

    /// Explicit display boundary for the browser authorization URL.
    pub fn authorization_url(&self) -> &str {
        self.authorization_url.expose_secret()
    }

    pub async fn complete(self) -> Result<ChatGptAuthStatus, AuthError> {
        self.complete_with_cancellation(&CancellationToken::new())
            .await
    }

    pub async fn complete_with_cancellation(
        self,
        cancel: &CancellationToken,
    ) -> Result<ChatGptAuthStatus, AuthError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AuthError::Cancelled),
            result = timeout(Duration::from_secs(300), self.receive()) => result.map_err(|_| AuthError::Timeout)?,
        }
    }

    async fn receive(&self) -> Result<ChatGptAuthStatus, AuthError> {
        // Unrelated browser requests do not consume the one login callback.
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| AuthError::Transport)?;
            let mut bytes = Zeroizing::new(Vec::new());
            let read = async {
                while bytes.len() < 16 * 1024 {
                    let mut byte = [0u8; 1];
                    if socket
                        .read(&mut byte)
                        .await
                        .map_err(|_| AuthError::Transport)?
                        == 0
                    {
                        return Err(AuthError::InvalidCallback);
                    }
                    bytes.push(byte[0]);
                    if bytes.ends_with(b"\r\n\r\n") {
                        return Ok(());
                    }
                }
                Err(AuthError::InvalidCallback)
            };
            if !matches!(timeout(Duration::from_secs(5), read).await, Ok(Ok(()))) {
                continue;
            }
            let code = self.callback_code(&bytes);
            let Ok(code) = code else {
                let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            };
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            serializer
                .append_pair("grant_type", "authorization_code")
                .append_pair("code", code.expose_secret())
                .append_pair("redirect_uri", &self.redirect_uri)
                .append_pair("client_id", CLIENT_ID)
                .append_pair("code_verifier", self.verifier.expose_secret());
            let body = Zeroizing::new(serializer.finish().into_bytes());
            let result = self.exchange(body).await;
            let reply: &[u8] = if result.is_ok() {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 34\r\nConnection: close\r\n\r\nSigned in. You may close this tab."
            } else {
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = timeout(Duration::from_secs(2), socket.write_all(reply)).await;
            return result;
        }
    }

    fn callback_code(&self, bytes: &[u8]) -> Result<SecretString, AuthError> {
        let first_line = std::str::from_utf8(bytes)
            .map_err(|_| AuthError::InvalidCallback)?
            .lines()
            .next()
            .ok_or(AuthError::InvalidCallback)?;
        let mut words = first_line.split_whitespace();
        if words.next() != Some("GET") {
            return Err(AuthError::InvalidCallback);
        }
        let target = words.next().ok_or(AuthError::InvalidCallback)?;
        if !target.starts_with("/auth/callback?") {
            return Err(AuthError::InvalidCallback);
        }
        let url = Url::parse(&format!("http://localhost{target}"))
            .map_err(|_| AuthError::InvalidCallback)?;
        let mut state = None;
        let mut code = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "state" if state.is_none() => state = Some(SecretString::new(value.into_owned())),
                "code" if code.is_none() => code = Some(SecretString::new(value.into_owned())),
                "state" | "code" | "error" => return Err(AuthError::InvalidCallback),
                _ => {}
            }
        }
        if state.as_ref().map(SecretString::expose_secret) != Some(self.state.expose_secret()) {
            return Err(AuthError::InvalidCallback);
        }
        code.filter(|s| !s.expose_secret().is_empty())
            .ok_or(AuthError::InvalidCallback)
    }

    async fn exchange(&self, body: Zeroizing<Vec<u8>>) -> Result<ChatGptAuthStatus, AuthError> {
        let tokens = token_request(
            &self.client,
            &self.issuer,
            body,
            "application/x-www-form-urlencoded",
        )
        .await?;
        for name in ["id_token", "access_token", "refresh_token"] {
            if !tokens
                .0
                .get(name)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
            {
                return Err(AuthError::InvalidCredentials);
            }
        }
        let _lock = lock_store(&self.path).await?;
        let mut document = if self
            .path
            .try_exists()
            .map_err(|_| AuthError::StoreUnavailable)?
        {
            read_store(&self.path)?
        } else {
            SecretJson(json!({}))
        };
        if !document.0.is_object() {
            return Err(AuthError::InvalidCredentials);
        }
        document.0["auth_mode"] = "chatgpt".into();
        if !document.0["tokens"].is_object() {
            replace_secret(&mut document.0["tokens"], json!({}));
        }
        for name in ["id_token", "access_token", "refresh_token"] {
            replace_secret(&mut document.0["tokens"][name], tokens.0[name].clone());
        }
        let claims = decode_jwt(
            tokens.0["id_token"]
                .as_str()
                .ok_or(AuthError::InvalidCredentials)?,
        )?;
        document.0["tokens"]["account_id"] = claims
            .0
            .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
            .and_then(Value::as_str)
            .ok_or(AuthError::InvalidCredentials)?
            .into();
        replace_secret(&mut document.0["OPENAI_API_KEY"], Value::Null);
        document.0["last_refresh"] = chrono::DateTime::from_timestamp(
            unix_now()
                .try_into()
                .map_err(|_| AuthError::InvalidCredentials)?,
            0,
        )
        .ok_or(AuthError::InvalidCredentials)?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .into();
        let status = document.status()?;
        write_store(&self.path, &document)?;
        Ok(status)
    }
}

struct SecretJson(Value);
impl SecretJson {
    fn parse(bytes: &[u8]) -> Result<Self, AuthError> {
        serde_json::from_slice(bytes)
            .map(Self)
            .map_err(|_| AuthError::InvalidCredentials)
    }
    fn bytes(&self) -> Result<Zeroizing<Vec<u8>>, AuthError> {
        let mut bytes = Zeroizing::new(Vec::new());
        serde_json::to_writer(&mut *bytes, &self.0).map_err(|_| AuthError::InvalidCredentials)?;
        Ok(bytes)
    }
    fn token(&self, name: &str) -> Result<&str, AuthError> {
        self.0
            .get("tokens")
            .and_then(|tokens| tokens.get(name))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or(AuthError::InvalidCredentials)
    }
    fn status(&self) -> Result<ChatGptAuthStatus, AuthError> {
        if self.0.get("auth_mode").is_some_and(|v| v != "chatgpt") {
            return Err(AuthError::LoginRequired);
        }
        self.token("access_token")?;
        self.token("refresh_token")?;
        let claims = decode_jwt(self.token("id_token")?)?;
        let auth = &claims.0["https://api.openai.com/auth"];
        let stored = self.0.pointer("/tokens/account_id").and_then(Value::as_str);
        let claimed = auth.get("chatgpt_account_id").and_then(Value::as_str);
        if let (Some(stored), Some(claimed)) = (stored, claimed)
            && stored != claimed
        {
            return Err(AuthError::AccountChanged);
        }
        let account_id = stored
            .or(claimed)
            .filter(|s| !s.trim().is_empty())
            .ok_or(AuthError::InvalidCredentials)?
            .to_owned();
        Ok(ChatGptAuthStatus {
            account_id,
            email: claims
                .0
                .get("email")
                .or_else(|| claims.0.pointer("/https:~1~1api.openai.com~1profile/email"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            plan: auth
                .get("chatgpt_plan_type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            fedramp: auth
                .get("chatgpt_account_is_fedramp")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}
impl Zeroize for SecretJson {
    fn zeroize(&mut self) {
        wipe_json(&mut self.0);
    }
}
impl Drop for SecretJson {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for SecretJson {}
fn wipe_json(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(wipe_json),
        Value::Object(values) => {
            for (mut key, mut value) in std::mem::take(values) {
                key.zeroize();
                wipe_json(&mut value);
            }
        }
        _ => {}
    }
    *value = Value::Null;
}
fn replace_secret(value: &mut Value, next: Value) {
    wipe_json(value);
    *value = next;
}

fn headers(bearer: &str, status: Option<&ChatGptAuthStatus>) -> Result<HeaderMap, AuthError> {
    if bearer.trim().is_empty() {
        return Err(AuthError::InvalidCredentials);
    }
    let authorization = Zeroizing::new(format!("Bearer {bearer}"));
    let mut header =
        HeaderValue::from_str(&authorization).map_err(|_| AuthError::InvalidCredentials)?;
    header.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, header);
    if let Some(status) = status {
        let mut account =
            HeaderValue::from_str(&status.account_id).map_err(|_| AuthError::InvalidCredentials)?;
        account.set_sensitive(true);
        headers.insert("chatgpt-account-id", account);
        if status.fedramp {
            headers.insert("x-openai-fedramp", HeaderValue::from_static("true"));
        }
    }
    Ok(headers)
}

fn decode_jwt(token: &str) -> Result<SecretJson, AuthError> {
    let payload = token
        .split('.')
        .nth(1)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::InvalidCredentials)?;
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| AuthError::InvalidCredentials)?,
    );
    SecretJson::parse(&bytes)
}
fn read_store(path: &Path) -> Result<SecretJson, AuthError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| AuthError::StoreUnavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_AUTH_BYTES as u64 {
        return Err(AuthError::InvalidCredentials);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    File::open(path)
        .map_err(|_| AuthError::StoreUnavailable)?
        .take(MAX_AUTH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::StoreUnavailable)?;
    if bytes.len() > MAX_AUTH_BYTES {
        return Err(AuthError::InvalidCredentials);
    }
    SecretJson::parse(&bytes)
}
fn write_store(path: &Path, document: &SecretJson) -> Result<(), AuthError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| AuthError::StoreUnavailable)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|_| AuthError::StoreUnavailable)?;
    let bytes = document.bytes()?;
    if bytes.len() > MAX_AUTH_BYTES {
        return Err(AuthError::InvalidCredentials);
    }
    temporary
        .write_all(&bytes)
        .map_err(|_| AuthError::StoreUnavailable)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| AuthError::StoreUnavailable)?;
    temporary
        .persist(path)
        .map_err(|_| AuthError::StoreUnavailable)?;
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| AuthError::StoreUnavailable)?;
    Ok(())
}

fn open_store_lock(path: &Path) -> Result<File, AuthError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| AuthError::StoreUnavailable)?;
    let mut name = path.as_os_str().to_owned();
    name.push(".orvek-lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(PathBuf::from(name))
        .map_err(|_| AuthError::StoreUnavailable)?;
    Ok(lock)
}

async fn lock_store(path: &Path) -> Result<File, AuthError> {
    let lock = open_store_lock(path)?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < REQUEST_TIMEOUT =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await
            }
            Err(_) => return Err(AuthError::StoreUnavailable),
        }
    }
}

async fn token_request(
    client: &Client,
    issuer: &Url,
    body: Zeroizing<Vec<u8>>,
    content_type: &str,
) -> Result<SecretJson, AuthError> {
    let operation = async {
        let mut response = client
            .post(
                issuer
                    .join("oauth/token")
                    .map_err(|_| AuthError::InvalidIssuer)?,
            )
            .header("content-type", content_type)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| AuthError::Transport)?;
        if !response.status().is_success() {
            return Err(AuthError::Rejected);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_AUTH_BYTES {
                return Err(AuthError::InvalidCredentials);
            }
            bytes.extend_from_slice(&chunk);
        }
        SecretJson::parse(&bytes)
    };
    timeout(REQUEST_TIMEOUT, operation)
        .await
        .map_err(|_| AuthError::Timeout)?
}
fn issuer(value: &str) -> Result<Url, AuthError> {
    let url = Url::parse(value).map_err(|_| AuthError::InvalidIssuer)?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(AuthError::InvalidIssuer);
    }
    Ok(url)
}
fn client() -> Result<Client, AuthError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| AuthError::Transport)
}
fn random_urlsafe() -> String {
    let mut bytes = Zeroizing::new(Vec::with_capacity(32));
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(&*bytes)
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
