use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RespValue {
    SimpleString(String),
    BulkString(String),
    Integer(i64),
    Null,
    Error(String),
    Array(Vec<RespValue>),
}

impl RespValue {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::SimpleString(value) => format!("+{value}\r\n").into_bytes(),
            Self::BulkString(value) => {
                let mut encoded = format!("${}\r\n", value.len()).into_bytes();
                encoded.extend_from_slice(value.as_bytes());
                encoded.extend_from_slice(b"\r\n");
                encoded
            }
            Self::Integer(value) => format!(":{value}\r\n").into_bytes(),
            Self::Null => b"$-1\r\n".to_vec(),
            Self::Error(message) => format!("-{message}\r\n").into_bytes(),
            Self::Array(values) => {
                let mut encoded = format!("*{}\r\n", values.len()).into_bytes();
                for value in values {
                    encoded.extend_from_slice(&value.encode());
                }
                encoded
            }
        }
    }

    pub async fn write_to<W>(&self, writer: &mut W) -> io::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        writer.write_all(&self.encode()).await
    }
}

pub async fn read_frame<R>(reader: &mut R) -> io::Result<Option<RespValue>>
where
    R: AsyncBufRead + Unpin,
{
    let Some(prefix) = read_prefix(reader).await? else {
        return Ok(None);
    };

    let value = read_frame_with_prefix(reader, prefix).await?;
    Ok(Some(value))
}

async fn read_frame_with_prefix<R>(reader: &mut R, prefix: u8) -> io::Result<RespValue>
where
    R: AsyncBufRead + Unpin,
{
    if prefix == b'*' {
        return read_array(reader).await;
    }

    read_scalar_frame_with_prefix(reader, prefix).await
}

async fn read_scalar_frame_with_prefix<R>(reader: &mut R, prefix: u8) -> io::Result<RespValue>
where
    R: AsyncBufRead + Unpin,
{
    match prefix {
        b'+' => Ok(RespValue::SimpleString(read_line(reader).await?)),
        b'-' => Ok(RespValue::Error(read_line(reader).await?)),
        b':' => {
            let value = read_line(reader).await?;
            let parsed = value
                .parse::<i64>()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid RESP integer"))?;
            Ok(RespValue::Integer(parsed))
        }
        b'$' => read_bulk_string(reader).await,
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported RESP prefix: {}", prefix as char),
        )),
    }
}

async fn read_bulk_string<R>(reader: &mut R) -> io::Result<RespValue>
where
    R: AsyncBufRead + Unpin,
{
    let len = parse_len(&read_line(reader).await?)?;
    if len == -1 {
        return Ok(RespValue::Null);
    }

    let len = usize::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "bulk string length must be non-negative",
        )
    })?;

    let mut data = vec![0; len];
    reader.read_exact(&mut data).await?;

    let mut crlf = [0; 2];
    reader.read_exact(&mut crlf).await?;
    if crlf != *b"\r\n" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bulk string missing CRLF terminator",
        ));
    }

    let text = String::from_utf8(data).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "bulk string must be valid UTF-8",
        )
    })?;
    Ok(RespValue::BulkString(text))
}

async fn read_array<R>(reader: &mut R) -> io::Result<RespValue>
where
    R: AsyncBufRead + Unpin,
{
    let len = parse_len(&read_line(reader).await?)?;
    if len < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "null arrays are not supported",
        ));
    }

    let len = usize::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid array length"))?;

    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        let prefix = read_prefix(reader).await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF inside RESP array",
            )
        })?;
        if prefix == b'*' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "nested RESP arrays are not supported",
            ));
        }
        values.push(read_scalar_frame_with_prefix(reader, prefix).await?);
    }

    Ok(RespValue::Array(values))
}

async fn read_prefix<R>(reader: &mut R) -> io::Result<Option<u8>>
where
    R: AsyncBufRead + Unpin,
{
    let mut prefix = [0; 1];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => Ok(Some(prefix[0])),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

async fn read_line<R>(reader: &mut R) -> io::Result<String>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    let bytes_read = reader.read_until(b'\n', &mut buf).await?;
    if bytes_read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected EOF while reading RESP line",
        ));
    }

    if buf.len() < 2 || buf[buf.len() - 2] != b'\r' {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RESP line missing CRLF terminator",
        ));
    }

    buf.truncate(buf.len() - 2);
    String::from_utf8(buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "RESP text must be valid UTF-8"))
}

fn parse_len(value: &str) -> io::Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid RESP length"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    async fn decode(bytes: &[u8]) -> RespValue {
        let mut reader = BufReader::new(Cursor::new(bytes.to_vec()));
        read_frame(&mut reader)
            .await
            .expect("decode should succeed")
            .expect("frame should exist")
    }

    #[tokio::test]
    async fn decodes_scalar_frames() {
        assert_eq!(
            decode(b"+PONG\r\n").await,
            RespValue::SimpleString("PONG".into())
        );
        assert_eq!(
            decode(b"$3\r\nfoo\r\n").await,
            RespValue::BulkString("foo".into())
        );
        assert_eq!(decode(b":7\r\n").await, RespValue::Integer(7));
        assert_eq!(decode(b"$-1\r\n").await, RespValue::Null);
        assert_eq!(
            decode(b"-ERR boom\r\n").await,
            RespValue::Error("ERR boom".into())
        );
    }

    #[tokio::test]
    async fn round_trips_array_of_bulk_strings() {
        let frame = RespValue::Array(vec![
            RespValue::BulkString("SET".into()),
            RespValue::BulkString("foo".into()),
            RespValue::BulkString("bar baz".into()),
        ]);

        let mut reader = BufReader::new(Cursor::new(frame.encode()));
        let decoded = read_frame(&mut reader)
            .await
            .expect("decode should succeed")
            .expect("frame should exist");

        assert_eq!(decoded, frame);
    }
}
