use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use opencode_catalog::lock::{LockError, with_lock};
use opencode_local::lock::Options;
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(|| format!("missing <command>\n{}", usage()))?;
    let directory = PathBuf::from(args.next().ok_or_else(|| format!("missing <dir>\n{}", usage()))?);
    let key = args.next().ok_or_else(|| format!("missing <key>\n{}", usage()))?;
    match command.as_str() {
        "hold" => hold(directory, key, args).await,
        "try" => try_command(directory, key, args).await,
        other => Err(format!("unknown command `{other}`\n{}", usage())),
    }
}

async fn hold(directory: PathBuf, key: String, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    if args.next().is_some() {
        return Err(format!("hold accepts only <dir> <key>\n{}", usage()));
    }
    with_lock(&directory, &key, Options::default(), async {
        println!("held");
        let _ = tokio::io::stdin().read_to_end(&mut Vec::new()).await;
    })
    .await
    .map_err(|error| error.to_string())
}

async fn try_command(directory: PathBuf, key: String, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let timeout = args.next().ok_or_else(|| format!("missing <timeout_ms>\n{}", usage()))?;
    if args.next().is_some() {
        return Err(format!("try accepts only <dir> <key> <timeout_ms>\n{}", usage()));
    }
    let timeout_ms = timeout.parse().map_err(|_| format!("invalid <timeout_ms> `{timeout}`\n{}", usage()))?;
    let options = Options { timeout: Duration::from_millis(timeout_ms), ..Options::default() };
    match with_lock(&directory, &key, options, std::future::ready(())).await {
        Ok(()) => {
            println!("acquired");
            Ok(())
        }
        Err(LockError::Timeout(_)) => {
            println!("timeout");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn usage() -> String {
    "usage: lock hold <dir> <key> | lock try <dir> <key> <timeout_ms>".to_string()
}
