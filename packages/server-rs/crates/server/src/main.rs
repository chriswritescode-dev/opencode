mod cli;
mod env;
mod serve;
mod shutdown;

pub(crate) const VERSION: &str = match option_env!("OPENCODE_VERSION") {
    Some(version) => version,
    None => "local",
};

fn main() -> std::process::ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            error.report();
            return std::process::ExitCode::from(1);
        }
    };
    match command {
        cli::Command::Version => {
            println!("opencode-server v{VERSION}");
            std::process::ExitCode::SUCCESS
        }
        cli::Command::Serve(args) => serve::run_serve(args, env::ServerEnv::from_process()),
        cli::Command::Service => {
            eprintln!("not implemented");
            std::process::ExitCode::from(1)
        }
    }
}
