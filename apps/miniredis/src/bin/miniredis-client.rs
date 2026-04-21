use std::io::{self, Write};

use clap::Parser;
use miniredis::command::Command;
use miniredis::protocol::read_frame;
use miniredis::repl::{format_response, parse_repl_line};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:6379")]
    addr: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    run_repl(&args.addr).await?;
    Ok(())
}

async fn run_repl(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let stream = TcpStream::connect(addr).await?;
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("miniredis> ");
        io::stdout().flush()?;

        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }

        let Some(command) = (match parse_repl_line(&line) {
            Ok(command) => command,
            Err(message) => {
                println!("(error) {message}");
                continue;
            }
        }) else {
            continue;
        };

        let should_close = matches!(command, Command::Quit);
        writer_half.write_all(&command.to_frame().encode()).await?;

        let Some(response) = read_frame(&mut reader).await? else {
            return Err("server closed the connection".into());
        };

        println!("{}", format_response(&response));

        if should_close {
            break;
        }
    }

    Ok(())
}
