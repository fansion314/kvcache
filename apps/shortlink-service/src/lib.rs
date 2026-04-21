use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kvcache::TtlLruCache;
use rand::Rng;
use rand::distributions::Alphanumeric;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use url::Url;

const GENERATED_ALIAS_LEN: usize = 7;
const GENERATED_ALIAS_ATTEMPTS: usize = 8;
const DEFAULT_CAPACITY: usize = 10_000;
const DEFAULT_TTL_SECS: u64 = 86_400;
const DEFAULT_PUBLIC_BASE_URL: &str = "http://127.0.0.1:3000";

pub fn app_from_config(config: ServiceConfig) -> Router {
    app(Arc::new(AppState::new(config)))
}

pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/healthz", get(healthz))
        .route("/api/links", post(create_link))
        .route("/api/links/{alias}", get(get_link).delete(delete_link))
        .route("/{alias}", get(redirect_link))
        .with_state(state)
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub capacity: usize,
    pub default_ttl: Duration,
    pub public_base_url: String,
}

impl ServiceConfig {
    pub fn new(
        capacity: usize,
        default_ttl: Duration,
        public_base_url: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        if capacity == 0 {
            return Err(ConfigError::new("CACHE_CAPACITY must be greater than 0"));
        }

        if default_ttl.is_zero() {
            return Err(ConfigError::new(
                "DEFAULT_TTL_SECS must be greater than 0 seconds",
            ));
        }

        let public_base_url = normalize_public_base_url(public_base_url.into())?;

        Ok(Self {
            capacity,
            default_ttl,
            public_base_url,
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let capacity = read_env_or_default("CACHE_CAPACITY", DEFAULT_CAPACITY)?;
        let default_ttl_secs = read_env_or_default("DEFAULT_TTL_SECS", DEFAULT_TTL_SECS)?;
        let public_base_url =
            std::env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| DEFAULT_PUBLIC_BASE_URL.to_owned());

        Self::new(
            capacity,
            Duration::from_secs(default_ttl_secs),
            public_base_url,
        )
    }

    fn short_url(&self, alias: &str) -> String {
        format!("{}/{}", self.public_base_url, alias)
    }
}

#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

pub struct AppState {
    cache: Mutex<TtlLruCache<String, Arc<LinkRecord>>>,
    config: ServiceConfig,
}

impl AppState {
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            cache: Mutex::new(TtlLruCache::new(config.capacity, config.default_ttl)),
            config,
        }
    }
}

#[derive(Debug)]
struct LinkRecord {
    alias: String,
    target_url: String,
    created_at: SystemTime,
    expires_at: Option<SystemTime>,
    hit_count: AtomicU64,
}

impl LinkRecord {
    fn new(
        alias: String,
        target_url: String,
        created_at: SystemTime,
        expires_at: Option<SystemTime>,
    ) -> Self {
        Self {
            alias,
            target_url,
            created_at,
            expires_at,
            hit_count: AtomicU64::new(0),
        }
    }

    fn hit_count(&self) -> u64 {
        self.hit_count.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Deserialize)]
struct CreateLinkRequest {
    url: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    ttl_seconds: Option<i64>,
}

#[derive(Debug, Serialize)]
struct LinkResponse {
    alias: String,
    short_url: String,
    url: String,
    created_at: u64,
    expires_at: Option<u64>,
    hit_count: u64,
}

impl LinkResponse {
    fn from_record(record: &LinkRecord, config: &ServiceConfig) -> Self {
        Self {
            alias: record.alias.clone(),
            short_url: config.short_url(&record.alias),
            url: record.target_url.clone(),
            created_at: unix_timestamp(record.created_at),
            expires_at: record.expires_at.map(unix_timestamp),
            hit_count: record.hit_count(),
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn not_found(alias: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("alias '{alias}' was not found"),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

async fn home(State(state): State<Arc<AppState>>) -> Html<String> {
    let html = format!(
        "<!doctype html>\
         <html lang=\"en\">\
         <head>\
           <meta charset=\"utf-8\">\
           <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
           <title>kvcache shortlink service</title>\
           <style>\
             body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif; max-width: 720px; margin: 48px auto; padding: 0 20px; line-height: 1.6; }}\
             code {{ background: #f4f4f5; padding: 0.1rem 0.35rem; border-radius: 4px; }}\
           </style>\
         </head>\
         <body>\
           <h1>kvcache shortlink service</h1>\
           <p>This service keeps every short link in memory only. Restarting the process removes all links.</p>\
           <p>Base URL: <code>{}</code></p>\
           <ul>\
             <li><code>GET /healthz</code></li>\
             <li><code>POST /api/links</code></li>\
             <li><code>GET /api/links/:alias</code></li>\
             <li><code>DELETE /api/links/:alias</code></li>\
             <li><code>GET /:alias</code></li>\
           </ul>\
         </body>\
         </html>",
        state.config.public_base_url
    );

    Html(html)
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_link(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateLinkRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let target_url = payload.url.trim().to_owned();
    validate_target_url(&target_url)?;

    let ttl = resolve_ttl(payload.ttl_seconds, state.config.default_ttl)?;
    let created_at = SystemTime::now();
    let expires_at = created_at
        .checked_add(ttl)
        .ok_or_else(|| ApiError::bad_request("ttl_seconds is too large"))?;

    let alias = match payload.alias {
        Some(alias) => {
            let alias = alias.trim().to_owned();
            validate_alias(&alias)?;

            let mut cache = state.cache.lock().await;
            if cache.get(&alias).is_some() {
                return Err(ApiError::conflict(format!(
                    "alias '{alias}' already exists"
                )));
            }

            let record = Arc::new(LinkRecord::new(
                alias.clone(),
                target_url.clone(),
                created_at,
                Some(expires_at),
            ));
            let _ = cache.put_with_ttl(alias.clone(), record, ttl);
            alias
        }
        None => {
            let mut cache = state.cache.lock().await;
            let mut generated_alias = None;

            for _ in 0..GENERATED_ALIAS_ATTEMPTS {
                let candidate = generate_alias();
                if cache.get(&candidate).is_none() {
                    let record = Arc::new(LinkRecord::new(
                        candidate.clone(),
                        target_url.clone(),
                        created_at,
                        Some(expires_at),
                    ));
                    let _ = cache.put_with_ttl(candidate.clone(), record, ttl);
                    generated_alias = Some(candidate);
                    break;
                }
            }

            generated_alias.ok_or_else(|| {
                ApiError::internal("failed to allocate a unique alias after 8 attempts")
            })?
        }
    };

    let record = {
        let mut cache = state.cache.lock().await;
        cache
            .get(&alias)
            .cloned()
            .ok_or_else(|| ApiError::internal("newly inserted alias was not found"))?
    };

    Ok((
        StatusCode::CREATED,
        Json(LinkResponse::from_record(&record, &state.config)),
    ))
}

async fn get_link(
    State(state): State<Arc<AppState>>,
    Path(alias): Path<String>,
) -> Result<Json<LinkResponse>, ApiError> {
    let record = {
        let mut cache = state.cache.lock().await;
        cache.get(&alias).cloned()
    }
    .ok_or_else(|| ApiError::not_found(&alias))?;

    Ok(Json(LinkResponse::from_record(&record, &state.config)))
}

async fn delete_link(
    State(state): State<Arc<AppState>>,
    Path(alias): Path<String>,
) -> Result<StatusCode, ApiError> {
    let removed = {
        let mut cache = state.cache.lock().await;
        cache.invalidate(&alias)
    };

    match removed {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(ApiError::not_found(&alias)),
    }
}

async fn redirect_link(
    State(state): State<Arc<AppState>>,
    Path(alias): Path<String>,
) -> Result<Response, ApiError> {
    let record = {
        let mut cache = state.cache.lock().await;
        cache.get(&alias).cloned()
    }
    .ok_or_else(|| ApiError::not_found(&alias))?;

    record.hit_count.fetch_add(1, Ordering::Relaxed);

    let location = HeaderValue::from_str(&record.target_url)
        .map_err(|_| ApiError::internal("stored URL could not be converted to a header"))?;

    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

fn resolve_ttl(
    request_ttl_seconds: Option<i64>,
    default_ttl: Duration,
) -> Result<Duration, ApiError> {
    match request_ttl_seconds {
        Some(seconds) if seconds <= 0 => {
            Err(ApiError::bad_request("ttl_seconds must be greater than 0"))
        }
        Some(seconds) => Ok(Duration::from_secs(seconds as u64)),
        None => Ok(default_ttl),
    }
}

fn validate_target_url(target_url: &str) -> Result<(), ApiError> {
    let parsed = Url::parse(target_url)
        .map_err(|_| ApiError::bad_request("url must be an absolute http or https URL"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(ApiError::bad_request(
                "url must use the http or https scheme",
            ));
        }
    }

    if parsed.host_str().is_none() {
        return Err(ApiError::bad_request("url must include a host"));
    }

    Ok(())
}

fn validate_alias(alias: &str) -> Result<(), ApiError> {
    if !(4..=32).contains(&alias.len()) {
        return Err(ApiError::bad_request(
            "alias length must be between 4 and 32 characters",
        ));
    }

    if !alias
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(ApiError::bad_request(
            "alias may only contain ASCII letters, digits, '_' or '-'",
        ));
    }

    Ok(())
}

fn generate_alias() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(GENERATED_ALIAS_LEN)
        .map(char::from)
        .collect()
}

fn normalize_public_base_url(raw: String) -> Result<String, ConfigError> {
    let trimmed = raw.trim().trim_end_matches('/').to_owned();
    if trimmed.is_empty() {
        return Err(ConfigError::new("PUBLIC_BASE_URL must not be empty"));
    }

    let parsed = Url::parse(&trimmed)
        .map_err(|_| ConfigError::new("PUBLIC_BASE_URL must be an absolute URL"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(ConfigError::new(
                "PUBLIC_BASE_URL must use the http or https scheme",
            ));
        }
    }

    if parsed.host_str().is_none() {
        return Err(ConfigError::new("PUBLIC_BASE_URL must include a host"));
    }

    Ok(trimmed)
}

fn read_env_or_default<T>(name: &str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr + Copy,
    T::Err: Display,
{
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|err| ConfigError::new(format!("{name} is invalid: {err}"))),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigError::new(format!("{name} is not valid unicode")))
        }
    }
}

fn unix_timestamp(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
