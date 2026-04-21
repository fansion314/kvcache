use std::io;
use std::sync::Arc;
use std::time::Duration;

use kvcache::TtlLruCache;
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::command::Command;
use crate::protocol::{RespValue, read_frame};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub addr: String,
    pub capacity: usize,
    pub default_ttl: Duration,
}

#[derive(Clone)]
pub struct AppState {
    cache: Arc<Mutex<TtlLruCache<String, String>>>,
}

impl AppState {
    pub fn new(capacity: usize, default_ttl: Duration) -> Self {
        Self {
            cache: Arc::new(Mutex::new(TtlLruCache::new(capacity, default_ttl))),
        }
    }
}

pub async fn run(config: ServerConfig) -> io::Result<()> {
    let listener = TcpListener::bind(&config.addr).await?;
    let state = AppState::new(config.capacity, config.default_ttl);
    serve(listener, state).await
}

pub async fn serve(listener: TcpListener, state: AppState) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state).await {
                eprintln!("connection error: {error}");
            }
        });
    }
}

async fn handle_connection(stream: TcpStream, state: AppState) -> io::Result<()> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);

    while let Some(frame) = read_frame(&mut reader).await? {
        let (response, should_close) = match Command::from_frame(frame) {
            Ok(command) => {
                let should_close = matches!(command, Command::Quit);
                (execute_command(&state, command).await, should_close)
            }
            Err(message) => (RespValue::Error(message), false),
        };

        response.write_to(&mut writer_half).await?;

        if should_close {
            break;
        }
    }

    Ok(())
}

pub async fn execute_command(state: &AppState, command: Command) -> RespValue {
    match command {
        Command::Ping => RespValue::SimpleString("PONG".into()),
        Command::Get { key } => {
            let value = {
                let mut cache = state.cache.lock().await;
                cache.get(&key).cloned()
            };
            value.map_or(RespValue::Null, RespValue::BulkString)
        }
        Command::Set { key, value } => {
            let mut cache = state.cache.lock().await;
            let _ = cache.put(key, value);
            RespValue::SimpleString("OK".into())
        }
        Command::SetEx {
            key,
            ttl_secs,
            value,
        } => {
            let mut cache = state.cache.lock().await;
            let _ = cache.put_with_ttl(key, value, Duration::from_secs(ttl_secs));
            RespValue::SimpleString("OK".into())
        }
        Command::GetEx { key } => {
            let value = {
                let mut cache = state.cache.lock().await;
                cache.get_and_refresh_expiry(&key).cloned()
            };
            value.map_or(RespValue::Null, RespValue::BulkString)
        }
        Command::Del { key } => {
            let deleted = {
                let mut cache = state.cache.lock().await;
                cache.invalidate(&key).is_some()
            };
            RespValue::Integer(if deleted { 1 } else { 0 })
        }
        Command::Quit => RespValue::SimpleString("OK".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn executes_mutating_commands_against_cache() {
        let state = AppState::new(8, Duration::from_secs(5));

        assert_eq!(
            execute_command(
                &state,
                Command::Set {
                    key: "foo".into(),
                    value: "bar".into(),
                },
            )
            .await,
            RespValue::SimpleString("OK".into())
        );
        assert_eq!(
            execute_command(&state, Command::Get { key: "foo".into() }).await,
            RespValue::BulkString("bar".into())
        );
        assert_eq!(
            execute_command(&state, Command::Del { key: "foo".into() }).await,
            RespValue::Integer(1)
        );
    }
}
