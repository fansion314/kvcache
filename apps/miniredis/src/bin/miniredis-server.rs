use std::time::Duration;

use clap::Parser;
use miniredis::server::{ServerConfig, run};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:6379")]
    addr: String,
    #[arg(long, default_value_t = 1024)]
    capacity: usize,
    #[arg(long = "default-ttl-secs", default_value_t = 60)]
    default_ttl_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    run(ServerConfig {
        addr: args.addr,
        capacity: args.capacity,
        default_ttl: Duration::from_secs(args.default_ttl_secs),
    })
    .await?;
    Ok(())
}
