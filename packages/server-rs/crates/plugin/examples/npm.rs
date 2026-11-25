use std::path::PathBuf;
use std::process::ExitCode;

use opencode_plugin::{NpmPaths, NpmRequest, ScriptHost, SidecarHost};
use opencode_sidecar::config::Config;
use opencode_sidecar::connection::Handlers;
use opencode_sidecar::supervisor::Sidecar;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = parse(std::env::args().skip(1))?;
    let runtime =
        tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(call(options))
}

async fn call(options: Options) -> Result<(), String> {
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let config = Config::new(std::env::vars_os(), directory, std::env::temp_dir());
    let sidecar = Sidecar::new(config, Handlers::new());
    let host = SidecarHost::new(sidecar.clone());
    let result = host.npm(options.request()).await.map_err(|error| error.to_string());
    sidecar.shutdown().await;
    let value = result?;
    println!("{}", serde_json::to_string(&value).map_err(|error| error.to_string())?);
    Ok(())
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let command = args.next().ok_or_else(|| format!("missing <command>\n{}", usage()))?;
    let mut positionals = Vec::new();
    let mut cache = None;
    let mut state = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cache" => {
                let dir = args.next().ok_or_else(|| format!("missing --cache <dir>\n{}", usage()))?;
                if cache.replace(PathBuf::from(dir)).is_some() {
                    return Err(format!("duplicated --cache\n{}", usage()));
                }
            }
            "--state" => {
                let dir = args.next().ok_or_else(|| format!("missing --state <dir>\n{}", usage()))?;
                if state.replace(PathBuf::from(dir)).is_some() {
                    return Err(format!("duplicated --state\n{}", usage()));
                }
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag `{flag}`\n{}", usage())),
            _ => positionals.push(arg),
        }
    }
    let cache = cache.ok_or_else(|| format!("missing --cache <dir>\n{}", usage()))?;
    let state = state.ok_or_else(|| format!("missing --state <dir>\n{}", usage()))?;
    let mut positionals = positionals.into_iter();
    let pkg = positionals.next().ok_or_else(|| format!("missing <pkg>\n{}", usage()))?;
    let command = match (command.as_str(), positionals.next()) {
        ("add", None) => Command::Add,
        ("resolve", None) => Command::Resolve,
        ("update", None) => Command::Update,
        ("reject", Some(version)) => Command::Reject(version),
        ("reject", None) => return Err(format!("reject requires <version>\n{}", usage())),
        ("add" | "resolve" | "update", Some(_)) => {
            return Err(format!("{command} accepts no <version>\n{}", usage()));
        }
        (command, _) => return Err(format!("unknown command `{command}`\n{}", usage())),
    };
    Ok(Options { command, pkg, cache, state })
}

fn usage() -> String {
    "usage: npm <add|resolve|update|reject> <pkg> [version] --cache <dir> --state <dir>".to_string()
}

struct Options {
    command: Command,
    pkg: String,
    cache: PathBuf,
    state: PathBuf,
}

enum Command {
    Add,
    Resolve,
    Update,
    Reject(String),
}

impl Options {
    fn request(&self) -> NpmRequest {
        let paths = self.paths();
        let pkg = self.pkg.clone();
        match &self.command {
            Command::Add => NpmRequest::Add { pkg, paths },
            Command::Resolve => NpmRequest::Resolve { pkg, paths },
            Command::Update => NpmRequest::Update { pkg, paths },
            Command::Reject(version) => NpmRequest::Reject { pkg, version: version.clone(), paths },
        }
    }

    fn paths(&self) -> NpmPaths {
        let base = self.cache.parent().unwrap_or(&self.cache);
        NpmPaths {
            cache: self.cache.clone(),
            state: self.state.clone(),
            data: base.join("data"),
            config: base.join("config"),
            tmp: base.join("tmp"),
            bin: base.join("bin"),
            log: base.join("log"),
            repos: base.join("repos"),
            home: base.join("home"),
        }
    }
}
