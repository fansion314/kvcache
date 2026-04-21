use shortlink_service::{ConfigError, ServiceConfig, app_from_config};
use std::fmt::{Display, Formatter};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 3000;

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    init_tracing();

    let host = std::env::var("HOST").unwrap_or_else(|_| DEFAULT_HOST.to_owned());
    let port = read_port()?;
    let config = ServiceConfig::from_env()?;
    let bind_addr = format!("{host}:{port}");

    let listener = TcpListener::bind(&bind_addr).await?;
    let local_addr = listener.local_addr()?;

    info!(
        %host,
        port,
        capacity = config.capacity,
        default_ttl_secs = config.default_ttl.as_secs(),
        public_base_url = %config.public_base_url,
        %local_addr,
        "shortlink service listening"
    );

    axum::serve(listener, app_from_config(config)).await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("shortlink_service=info,tower_http=info")),
        )
        .with_target(false)
        .compact()
        .init();
}

fn read_port() -> Result<u16, StartupError> {
    match std::env::var("PORT") {
        Ok(raw) => raw
            .parse::<u16>()
            .map_err(|err| StartupError::new(format!("PORT is invalid: {err}"))),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_PORT),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(StartupError::new("PORT is not valid unicode"))
        }
    }
}

#[derive(Debug)]
pub struct StartupError {
    message: String,
}

impl StartupError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<ConfigError> for StartupError {
    fn from(err: ConfigError) -> Self {
        Self::new(err.to_string())
    }
}

impl From<std::io::Error> for StartupError {
    fn from(err: std::io::Error) -> Self {
        Self::new(err.to_string())
    }
}

impl From<axum::Error> for StartupError {
    fn from(err: axum::Error) -> Self {
        Self::new(err.to_string())
    }
}

impl Display for StartupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}
