//! Easee's cloud API, the one the Easee app uses.
//!
//! Login with the app's user name (e-mail or phone number) and password gives
//! an access token and a refresh token. The access token is sent as a bearer
//! token. Before it runs out the client refreshes it, and when that fails, or
//! Easee answers 401, it logs in again once and repeats the request.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;

pub const BASE_URL: &str = "https://api.easee.com";
/// Refresh the access token this long before it runs out.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// Wrong user name or password.
    Auth,
    /// HTTP 429, with `Retry-After` when Easee sends it.
    RateLimited(Option<Duration>),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth => write!(f, "Easee afviste brugernavn eller adgangskode"),
            ApiError::RateLimited(_) => write!(f, "Easee beder om færre forespørgsler (HTTP 429)"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

#[derive(Clone)]
struct Token {
    access: String,
    refresh: String,
    expires: Instant,
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    username: String,
    password: String,
    token: Mutex<Option<Token>>,
}

impl Client {
    pub fn new(base: &str, username: &str, password: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default();
        Self { http, base: base.trim_end_matches('/').to_string(), username: username.into(), password: password.into(), token: Mutex::new(None) }
    }

    fn keep(v: &Value) -> Result<Token, ApiError> {
        let access = v["accessToken"].as_str().filter(|t| !t.is_empty()).ok_or(ApiError::Auth)?.to_string();
        let refresh = v["refreshToken"].as_str().unwrap_or_default().to_string();
        let secs = v["expiresIn"].as_u64().unwrap_or(3600);
        Ok(Token { access, refresh, expires: Instant::now() + Duration::from_secs(secs) })
    }

    /// Log in with user name and password and keep the tokens.
    pub async fn login(&self) -> Result<String, ApiError> {
        let body = json!({ "userName": self.username, "password": self.password });
        let res = self.http.post(format!("{}/api/accounts/login", self.base)).json(&body).send().await?;
        if res.status() == reqwest::StatusCode::BAD_REQUEST {
            return Err(ApiError::Auth);
        }
        let t = Self::keep(&read(res).await?)?;
        let access = t.access.clone();
        *self.token.lock().await = Some(t);
        Ok(access)
    }

    async fn refresh(&self, t: &Token) -> Result<String, ApiError> {
        let body = json!({ "accessToken": t.access, "refreshToken": t.refresh });
        let res = self.http.post(format!("{}/api/accounts/refresh_token", self.base)).json(&body).send().await?;
        let t = Self::keep(&read(res).await?)?;
        let access = t.access.clone();
        *self.token.lock().await = Some(t);
        Ok(access)
    }

    async fn access(&self) -> Result<String, ApiError> {
        let current = self.token.lock().await.clone();
        match current {
            Some(t) if t.expires > Instant::now() + REFRESH_MARGIN => Ok(t.access),
            Some(t) if !t.refresh.is_empty() => match self.refresh(&t).await {
                Ok(a) => Ok(a),
                Err(ApiError::RateLimited(d)) => Err(ApiError::RateLimited(d)),
                Err(_) => self.login().await,
            },
            _ => self.login().await,
        }
    }

    /// GET with `body = None`, else POST. Logs in again once on 401.
    async fn send(&self, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let token = self.access().await?;
            let url = format!("{}{path}", self.base);
            let req = match body {
                Some(b) => self.http.post(url).json(b),
                None => self.http.get(url),
            };
            let res = req.bearer_auth(token).header("Accept", "application/json").send().await?;
            if res.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.token.lock().await = None;
                continue;
            }
            return read(res).await;
        }
        Err(ApiError::Auth)
    }

    /// The chargers the account can see.
    pub async fn chargers(&self) -> Result<Value, ApiError> {
        self.send("/api/chargers", None).await
    }

    /// The live state of one charger.
    pub async fn state(&self, id: &str) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{id}/state"), None).await
    }

    /// The charger's configuration, among other things its maximum current.
    pub async fn config(&self, id: &str) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{id}/config"), None).await
    }

    /// `start_charging`, `stop_charging`, `pause_charging` or `resume_charging`.
    pub async fn command(&self, id: &str, command: &str) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{id}/commands/{command}"), Some(&json!({}))).await
    }

    /// Set the dynamic charger current in ampere.
    pub async fn set_dynamic_current(&self, id: &str, ampere: f64) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{id}/settings"), Some(&json!({ "dynamicChargerCurrent": ampere }))).await
    }
}

async fn read(res: reqwest::Response) -> Result<Value, ApiError> {
    let status = res.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs);
        return Err(ApiError::RateLimited(after));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ApiError::Auth);
    }
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(ApiError::Other(format!("HTTP {status}: {}", text.chars().take(200).collect::<String>())));
    }
    let text = res.text().await?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| ApiError::Other(format!("ugyldigt svar: {e}")))
}
