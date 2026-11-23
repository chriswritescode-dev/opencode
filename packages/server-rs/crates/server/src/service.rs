mod ensure;
mod stop;

use std::ffi::OsStr;
use std::process::ExitCode;

use opencode_local::paths::Paths;
use opencode_local::registration::{self, Files};
use opencode_local::service_config::{Edit, ServiceConfig};

use crate::cli::ServiceCommand;
use crate::env::ServerEnv;
use crate::probe;

pub(crate) fn run(command: ServiceCommand, mut env: ServerEnv) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let result = runtime.block_on(execute(command, &mut env));
    runtime.shutdown_background();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

async fn execute(command: ServiceCommand, env: &mut ServerEnv) -> Result<(), String> {
    let paths = Paths::from_vars(|name| env.get(name).map(OsStr::to_os_string)).map_err(|error| error.to_string())?;
    let files = Files::new(&paths, crate::CHANNEL);
    files.migrate(crate::CHANNEL, crate::VERSION);
    let service_config = ServiceConfig::new(&paths, crate::CHANNEL);
    match command {
        ServiceCommand::Start | ServiceCommand::Restart => {
            let config = service_config.read().map_err(|error| error.to_string())?;
            if let Some(configured) = &config.env {
                env.merge(configured);
            }
            if matches!(command, ServiceCommand::Restart) {
                stop::stop(&files.file).await;
            }
            println!("{}", ensure::ensure(&files, env).await?);
        }
        ServiceCommand::Status => {
            let found = match registration::read(&files.file) {
                Some(registration) => match probe::probe(&registration, probe::TIMEOUT).await {
                    probe::Probe::Answered(probe::State::Ready) => Some(registration.url),
                    _ => None,
                },
                None => None,
            };
            println!("{}", found.as_deref().unwrap_or("stopped"));
        }
        ServiceCommand::Stop => stop::stop(&files.file).await,
        ServiceCommand::Get { key, name } => {
            let value = service_config.get(key.as_deref(), name.as_deref()).map_err(|error| error.to_string())?;
            println!("{value}");
        }
        ServiceCommand::Set { key, value, nested } => {
            let edit = Edit::set(&key, &value, nested.as_deref()).map_err(|error| error.to_string())?;
            if edit.stops_service() {
                stop::stop(&files.file).await;
            }
            service_config.apply(&edit).map_err(|error| error.to_string())?;
        }
        ServiceCommand::Unset { key, name } => {
            let edit = Edit::unset(&key, name.as_deref()).map_err(|error| error.to_string())?;
            if edit.stops_service() {
                stop::stop(&files.file).await;
            }
            service_config.apply(&edit).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
