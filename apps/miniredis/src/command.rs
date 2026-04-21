use crate::protocol::RespValue;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Ping,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    SetEx {
        key: String,
        ttl_secs: u64,
        value: String,
    },
    GetEx {
        key: String,
    },
    Del {
        key: String,
    },
    Quit,
}

impl Command {
    pub fn from_frame(frame: RespValue) -> Result<Self, String> {
        let RespValue::Array(values) = frame else {
            return Err("ERR command must be sent as a RESP array".into());
        };

        let tokens = values
            .into_iter()
            .map(token_from_frame)
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_tokens(tokens)
    }

    pub fn from_tokens(tokens: Vec<String>) -> Result<Self, String> {
        if tokens.is_empty() {
            return Err("ERR empty command".into());
        }

        let command_name = tokens[0].to_ascii_uppercase();
        match command_name.as_str() {
            "PING" => expect_no_args(&tokens, Command::Ping),
            "GET" => expect_one_arg(&tokens, |key| Command::Get { key }),
            "SET" => expect_two_args(&tokens, |key, value| Command::Set { key, value }),
            "SETEX" => {
                if tokens.len() != 4 {
                    return Err(wrong_arity("setex"));
                }

                let ttl_secs = tokens[2]
                    .parse::<u64>()
                    .map_err(|_| "ERR invalid TTL seconds".to_string())?;

                Ok(Command::SetEx {
                    key: tokens[1].clone(),
                    ttl_secs,
                    value: tokens[3].clone(),
                })
            }
            "GETEX" => expect_one_arg(&tokens, |key| Command::GetEx { key }),
            "DEL" => expect_one_arg(&tokens, |key| Command::Del { key }),
            "QUIT" => expect_no_args(&tokens, Command::Quit),
            _ => Err(format!("ERR unknown command '{}'", tokens[0])),
        }
    }

    pub fn to_frame(&self) -> RespValue {
        let values = match self {
            Self::Ping => vec![bulk("PING")],
            Self::Get { key } => vec![bulk("GET"), bulk(key)],
            Self::Set { key, value } => vec![bulk("SET"), bulk(key), bulk(value)],
            Self::SetEx {
                key,
                ttl_secs,
                value,
            } => vec![
                bulk("SETEX"),
                bulk(key),
                bulk(ttl_secs.to_string()),
                bulk(value),
            ],
            Self::GetEx { key } => vec![bulk("GETEX"), bulk(key)],
            Self::Del { key } => vec![bulk("DEL"), bulk(key)],
            Self::Quit => vec![bulk("QUIT")],
        };

        RespValue::Array(values)
    }
}

fn token_from_frame(value: RespValue) -> Result<String, String> {
    match value {
        RespValue::BulkString(value) | RespValue::SimpleString(value) => Ok(value),
        RespValue::Null => Err("ERR null values are not valid command arguments".into()),
        RespValue::Integer(_) => Err("ERR integer values are not valid command arguments".into()),
        RespValue::Error(_) => Err("ERR error frames are not valid command arguments".into()),
        RespValue::Array(_) => Err("ERR nested arrays are not valid command arguments".into()),
    }
}

fn bulk(value: impl Into<String>) -> RespValue {
    RespValue::BulkString(value.into())
}

fn expect_no_args(tokens: &[String], command: Command) -> Result<Command, String> {
    if tokens.len() != 1 {
        return Err(wrong_arity(&tokens[0].to_ascii_lowercase()));
    }
    Ok(command)
}

fn expect_one_arg<F>(tokens: &[String], build: F) -> Result<Command, String>
where
    F: FnOnce(String) -> Command,
{
    if tokens.len() != 2 {
        return Err(wrong_arity(&tokens[0].to_ascii_lowercase()));
    }
    Ok(build(tokens[1].clone()))
}

fn expect_two_args<F>(tokens: &[String], build: F) -> Result<Command, String>
where
    F: FnOnce(String, String) -> Command,
{
    if tokens.len() != 3 {
        return Err(wrong_arity(&tokens[0].to_ascii_lowercase()));
    }
    Ok(build(tokens[1].clone(), tokens[2].clone()))
}

fn wrong_arity(command: &str) -> String {
    format!("ERR wrong number of arguments for '{command}' command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_core_commands() {
        assert_eq!(Command::from_tokens(vec!["PING".into()]), Ok(Command::Ping));
        assert_eq!(
            Command::from_tokens(vec!["get".into(), "foo".into()]),
            Ok(Command::Get { key: "foo".into() })
        );
        assert_eq!(
            Command::from_tokens(vec!["SETEX".into(), "foo".into(), "5".into(), "bar".into()]),
            Ok(Command::SetEx {
                key: "foo".into(),
                ttl_secs: 5,
                value: "bar".into(),
            })
        );
    }

    #[test]
    fn rejects_invalid_ttl_and_wrong_arity() {
        assert_eq!(
            Command::from_tokens(vec![
                "SETEX".into(),
                "foo".into(),
                "abc".into(),
                "bar".into()
            ]),
            Err("ERR invalid TTL seconds".into())
        );
        assert_eq!(
            Command::from_tokens(vec!["GET".into()]),
            Err("ERR wrong number of arguments for 'get' command".into())
        );
    }
}
