use crate::command::Command;
use crate::protocol::RespValue;

pub fn parse_repl_line(line: &str) -> Result<Option<Command>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let tokens = shlex::split(trimmed).ok_or_else(|| "ERR invalid quoting in input".to_string())?;
    if tokens.is_empty() {
        return Ok(None);
    }

    Command::from_tokens(tokens).map(Some)
}

pub fn format_response(response: &RespValue) -> String {
    match response {
        RespValue::SimpleString(value) | RespValue::BulkString(value) => value.clone(),
        RespValue::Integer(value) => value.to_string(),
        RespValue::Null => "(nil)".into(),
        RespValue::Error(message) => format!("(error) {message}"),
        RespValue::Array(values) => format!("{values:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RespValue;

    #[test]
    fn parses_repl_input_with_quotes() {
        let command = parse_repl_line(r#"SET foo "bar baz""#)
            .expect("parse should succeed")
            .expect("command should exist");

        assert_eq!(
            command,
            Command::Set {
                key: "foo".into(),
                value: "bar baz".into(),
            }
        );

        assert_eq!(
            command.to_frame(),
            RespValue::Array(vec![
                RespValue::BulkString("SET".into()),
                RespValue::BulkString("foo".into()),
                RespValue::BulkString("bar baz".into()),
            ])
        );
    }

    #[test]
    fn rejects_invalid_repl_input() {
        assert_eq!(
            parse_repl_line(r#"SET foo "bar"#),
            Err("ERR invalid quoting in input".into())
        );
    }
}
