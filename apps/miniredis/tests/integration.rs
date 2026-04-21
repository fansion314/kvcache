use std::time::Duration;

use miniredis::command::Command;
use miniredis::protocol::{RespValue, read_frame};
use miniredis::server::{AppState, serve};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, tcp::OwnedReadHalf, tcp::OwnedWriteHalf};
use tokio::task::JoinHandle;
use tokio::time::sleep;

struct TestClient {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl TestClient {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        let stream = TcpStream::connect(addr)
            .await
            .expect("connect should succeed");
        let (reader_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader_half),
            writer,
        }
    }

    async fn send(&mut self, command: Command) -> RespValue {
        self.writer
            .write_all(&command.to_frame().encode())
            .await
            .expect("write should succeed");
        read_frame(&mut self.reader)
            .await
            .expect("read should succeed")
            .expect("response should exist")
    }

    async fn send_raw(&mut self, frame: RespValue) -> RespValue {
        self.writer
            .write_all(&frame.encode())
            .await
            .expect("write should succeed");
        read_frame(&mut self.reader)
            .await
            .expect("read should succeed")
            .expect("response should exist")
    }
}

async fn spawn_server(default_ttl: Duration) -> (std::net::SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind should succeed");
    let addr = listener.local_addr().expect("local addr should exist");
    let state = AppState::new(32, default_ttl);
    let handle = tokio::spawn(async move {
        let _ = serve(listener, state).await;
    });
    (addr, handle)
}

#[tokio::test]
async fn supports_set_get_and_del() {
    let (addr, handle) = spawn_server(Duration::from_secs(5)).await;
    let mut client = TestClient::connect(addr).await;

    assert_eq!(
        client
            .send(Command::Set {
                key: "foo".into(),
                value: "bar".into(),
            })
            .await,
        RespValue::SimpleString("OK".into())
    );
    assert_eq!(
        client.send(Command::Get { key: "foo".into() }).await,
        RespValue::BulkString("bar".into())
    );
    assert_eq!(
        client.send(Command::Del { key: "foo".into() }).await,
        RespValue::Integer(1)
    );
    assert_eq!(
        client.send(Command::Get { key: "foo".into() }).await,
        RespValue::Null
    );

    handle.abort();
}

#[tokio::test]
async fn expires_short_ttl_entries() {
    let (addr, handle) = spawn_server(Duration::from_secs(5)).await;
    let mut client = TestClient::connect(addr).await;

    assert_eq!(
        client
            .send(Command::SetEx {
                key: "foo".into(),
                ttl_secs: 0,
                value: "bar".into(),
            })
            .await,
        RespValue::SimpleString("OK".into())
    );
    assert_eq!(
        client.send(Command::Get { key: "foo".into() }).await,
        RespValue::Null
    );

    handle.abort();
}

#[tokio::test]
async fn refreshes_expiry_via_getex() {
    let (addr, handle) = spawn_server(Duration::from_secs(1)).await;
    let mut client = TestClient::connect(addr).await;

    assert_eq!(
        client
            .send(Command::Set {
                key: "session".into(),
                value: "alive".into(),
            })
            .await,
        RespValue::SimpleString("OK".into())
    );

    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        client
            .send(Command::GetEx {
                key: "session".into(),
            })
            .await,
        RespValue::BulkString("alive".into())
    );

    sleep(Duration::from_millis(700)).await;
    assert_eq!(
        client
            .send(Command::Get {
                key: "session".into(),
            })
            .await,
        RespValue::BulkString("alive".into())
    );

    handle.abort();
}

#[tokio::test]
async fn returns_errors_for_invalid_commands() {
    let (addr, handle) = spawn_server(Duration::from_secs(5)).await;
    let mut client = TestClient::connect(addr).await;

    assert_eq!(
        client
            .send_raw(RespValue::Array(vec![RespValue::BulkString("GET".into())]))
            .await,
        RespValue::Error("ERR wrong number of arguments for 'get' command".into())
    );
    assert_eq!(
        client
            .send_raw(RespValue::Array(vec![RespValue::BulkString("BOOM".into())]))
            .await,
        RespValue::Error("ERR unknown command 'BOOM'".into())
    );

    handle.abort();
}
