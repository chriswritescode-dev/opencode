use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use opencode_local::paths::Paths;
use opencode_local::registration::{Files, Registration};
use opencode_local::service_config::ServiceConfig;
use opencode_local::{channel, id};

use crate::cli::Serve;
use crate::env::ServerEnv;
use crate::probe;
use crate::registration::{self, Ownership};
use crate::serve;
use crate::shutdown::{self, Signal, Watch};

const ELECTION_WINDOW: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
const REGISTRATION_CHECK_INTERVAL: Duration = Duration::from_secs(5);

enum Election {
    Bound(tokio::net::TcpListener),
    Incumbent,
    Signaled(Signal),
    PortInUse,
}

pub(crate) async fn run(args: Serve, mut env: ServerEnv, mut watch: Watch) -> std::io::Result<std::process::ExitCode> {
    let paths = Paths::from_vars(|name| env.get(name).map(OsStr::to_os_string))?;
    let files = Files::new(&paths, crate::CHANNEL);
    files.migrate(crate::CHANNEL, crate::VERSION);
    let file = files.file;
    let service_config = ServiceConfig::new(&paths, crate::CHANNEL);
    if let Err(error) = std::env::set_current_dir(&paths.home) {
        let _ = writeln!(std::io::stderr(), "failed to change service directory to home: {error}");
    }
    let config = service_config.read()?;
    let hostname = args.hostname.or(config.hostname.clone()).unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = args.port.or(config.port).unwrap_or_else(|| channel::default_port(crate::CHANNEL, cfg!(windows)));
    let cors = if args.cors.is_empty() { config.cors.clone().unwrap_or_default() } else { args.cors };
    if let Some(configured) = &config.env {
        env.merge(configured);
    }
    let url = service_url(&hostname, port);
    let incumbent = tokio::select! {
        found = probe::incumbent(&file, &url, probe::TIMEOUT) => found,
        signal = watch.next() => return Ok(shutdown::teardown(&mut watch, signal, async {}).await),
    };
    if incumbent.is_some() {
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let configured_password = config.password.clone().filter(|password| !password.is_empty());
    let generated = configured_password.is_none();
    let password = configured_password.unwrap_or_else(serve::generate_password);
    let listener = match elect(&file, &hostname, port, &url, &mut watch).await? {
        Election::Bound(listener) => listener,
        Election::Incumbent => return Ok(std::process::ExitCode::SUCCESS),
        Election::Signaled(signal) => return Ok(shutdown::teardown(&mut watch, signal, async {}).await),
        Election::PortInUse => {
            let _ = writeln!(
                std::io::stderr(),
                "Managed service port {port} on {hostname} is already in use by another process. Configure another port with `opencode service set port <port>` and start the service again."
            );
            return Ok(std::process::ExitCode::from(1));
        }
    };
    std::fs::create_dir_all(&paths.tmp)?;
    let tmp = std::fs::canonicalize(&paths.tmp)?;
    let address = listener.local_addr()?;
    let running = serve::start(listener, tmp, password.clone(), serve::machine_hostname(&env), cors, &env).await?;
    if generated {
        service_config.password(Some(&password))?;
    }
    let registration = Registration {
        id: Some(id::v4()),
        version: Some(crate::VERSION.to_owned()),
        url: format!("http://{address}"),
        pid: std::process::id(),
        password: Some(password),
    };
    registration::register(&file, &registration)?;
    if config.remote.is_some() {
        let _ = writeln!(std::io::stderr(), "remote access is not supported by opencode-server yet");
    }
    running.ready();
    if let Err(error) = serve::write_line(&format!("server listening on {}", registration.url)) {
        running.shutdown().await;
        registration::remove_if_owned(&file, &registration);
        return Err(error);
    }
    let mut check = tokio::time::interval(REGISTRATION_CHECK_INTERVAL);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let signal = loop {
        tokio::select! {
            signal = watch.next() => break signal,
            _ = check.tick() => {}
        }
        let message = match registration::ownership(&file, &registration) {
            Ownership::Owned => continue,
            Ownership::Replaced => "managed service registration replaced; shutting down",
            Ownership::Unreadable => "managed service registration check failed; shutting down",
        };
        let _ = writeln!(std::io::stderr(), "{message}");
        running.shutdown().await;
        registration::remove_if_owned(&file, &registration);
        return Ok(std::process::ExitCode::SUCCESS);
    };
    let cleanup = async {
        running.shutdown().await;
        registration::remove_if_owned(&file, &registration);
    };
    Ok(shutdown::teardown(&mut watch, signal, cleanup).await)
}

async fn elect(file: &Path, hostname: &str, port: u16, url: &str, watch: &mut Watch) -> std::io::Result<Election> {
    let deadline = Instant::now() + ELECTION_WINDOW;
    loop {
        match tokio::net::TcpListener::bind((hostname, port)).await {
            Ok(listener) => return Ok(Election::Bound(listener)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                let timeout = probe::TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
                let incumbent = tokio::select! {
                    found = probe::incumbent(file, url, timeout) => found,
                    signal = watch.next() => return Ok(Election::Signaled(signal)),
                };
                if incumbent.is_some() {
                    return Ok(Election::Incumbent);
                }
                if Instant::now() >= deadline {
                    return Ok(Election::PortInUse);
                }
                tokio::select! {
                    _ = tokio::time::sleep(RETRY_INTERVAL) => {}
                    signal = watch.next() => return Ok(Election::Signaled(signal)),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn service_url(hostname: &str, port: u16) -> String {
    if hostname.contains(':') {
        return format!("http://[{hostname}]:{port}");
    }
    format!("http://{hostname}:{port}")
}
