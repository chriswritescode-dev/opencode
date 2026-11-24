use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::sync::Arc;

use base64::Engine;
use opencode_http::state::ServerState;
use opencode_storage::{MigrationReport, StorageError, database_path};
use tokio::sync::oneshot;

use crate::cli::Serve;
use crate::env::ServerEnv;
use crate::shutdown::{Signal, Watch};

pub fn run_serve(args: Serve, env: ServerEnv) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "error: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    let result = runtime.block_on(serve(args, env));
    runtime.shutdown_background();
    match result {
        Ok(exit) => exit,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "error: {error}");
            std::process::ExitCode::from(1)
        }
    }
}

async fn serve(args: Serve, mut env: ServerEnv) -> std::io::Result<std::process::ExitCode> {
    let mut watch = Watch::new()?;
    env.remove("OPENCODE_PTY_HANDOFF");
    if args.service {
        return crate::service_mode::run(args, env, watch).await;
    }
    let environment_password = env.password();
    if args.stdio {
        env.remove("OPENCODE_PASSWORD");
        env.remove("OPENCODE_SERVER_PASSWORD");
    }
    let bind_hostname = args.hostname.unwrap_or_else(|| "127.0.0.1".to_owned());
    let paths = opencode_local::paths::Paths::from_vars(|name| env.get(name).map(OsStr::to_os_string))?;
    std::fs::create_dir_all(&paths.data)?;
    std::fs::create_dir_all(&paths.tmp)?;
    let tmp = std::fs::canonicalize(&paths.tmp)?;
    let listener = bind(&bind_hostname, args.port).await?;
    let address = listener.local_addr()?;
    let generated = environment_password.is_none();
    let password = environment_password.unwrap_or_else(generate_password);
    let (state, report) = open_state(&env, &paths.data).map_err(std::io::Error::other)?;
    report_warnings(&report);
    let running = start(listener, tmp, password.clone(), machine_hostname(&env), args.cors, Some(state), &env).await?;
    running.ready();
    if args.stdio {
        write_line(&serde_json::json!({ "url": format!("http://{address}") }).to_string())?;
        let reader = tokio::task::spawn_blocking(|| {
            let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        });
        let stdin_or_signal = tokio::select! {
            _ = reader => None,
            signal = watch.next() => Some(signal),
        };
        match stdin_or_signal {
            Some(signal) => return Ok(teardown_server(&mut watch, signal, running).await),
            None => {
                running.shutdown().await;
                return Ok(std::process::ExitCode::SUCCESS);
            }
        }
    }
    write_line(&format!("server listening on http://{address}"))?;
    if generated {
        write_line(&format!("server password {password}"))?;
    }
    let signal = watch.next().await;
    Ok(teardown_server(&mut watch, signal, running).await)
}

pub(crate) struct Running {
    app: Arc<opencode_http::app::App>,
    shutdown: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    sidecar: opencode_sidecar::supervisor::Sidecar,
}

impl Running {
    pub(crate) fn ready(&self) {
        self.app.status().ready();
    }

    #[cfg(unix)]
    #[allow(dead_code)]
    pub(crate) fn sidecar(&self) -> &opencode_sidecar::supervisor::Sidecar {
        &self.sidecar
    }

    pub(crate) fn fail(&self) {
        self.app.status().fail();
    }

    pub(crate) async fn shutdown(self) {
        self.app.status().begin_stopping();
        let _ = self.shutdown.send(());
        let _ = self.server.await;
        self.app.shutdown().await;
        #[cfg(unix)]
        self.sidecar.shutdown().await;
    }
}

pub(crate) async fn start(
    listener: tokio::net::TcpListener,
    tmp: std::path::PathBuf,
    password: String,
    hostname: String,
    cors: Vec<String>,
    state: Option<Arc<ServerState>>,
    env: &ServerEnv,
) -> std::io::Result<Running> {
    let address = listener.local_addr()?;
    let urls = Arc::new(move || connection_urls(address));
    #[cfg(unix)]
    let sidecar = opencode_sidecar::supervisor::Sidecar::new(
        opencode_sidecar::config::Config::new(
            env.vars(),
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")),
            tmp.clone(),
        ),
        opencode_sidecar::connection::Handlers::default(),
    );
    let services = match &state {
        Some(state) => {
            let paths = opencode_config::paths::GlobalPaths::from_vars(|name| env.get(name).map(OsStr::to_os_string))?
                .acquire()?;
            let start_directory = opencode_local::paths::current_dir()?.to_string_lossy().into_owned();
            let vars: HashMap<OsString, OsString> =
                env.vars().map(|(name, value)| (name.to_os_string(), value.to_os_string())).collect();
            let config_env: opencode_http::services::ConfigEnv =
                Arc::new(move |name: &str| vars.get(OsStr::new(name)).cloned());
            Some(Arc::new(opencode_http::services::GlobalServices::new(
                state.db.clone(),
                paths,
                start_directory,
                config_env,
            )))
        }
        None => None,
    };
    let app = opencode_http::app::App::new(opencode_http::app::Options {
        password,
        version: crate::VERSION.to_owned(),
        cors,
        tmp,
        hostname: Some(hostname),
        urls,
        state,
        services,
    });
    let (shutdown, shutdown_receiver) = oneshot::channel::<()>();
    let server = tokio::spawn(opencode_http::serve::serve(listener, app.clone(), async move {
        let _ = shutdown_receiver.await;
    }));
    Ok(Running {
        app,
        shutdown,
        server,
        #[cfg(unix)]
        sidecar,
    })
}

pub(crate) fn open_state(
    env: &ServerEnv,
    data: &std::path::Path,
) -> Result<(Arc<ServerState>, MigrationReport), StorageError> {
    let location = database_path(data, crate::CHANNEL, |name| env.get(name).map(OsStr::to_os_string));
    let (state, report) = ServerState::open(location, data)?;
    Ok((Arc::new(state), report))
}

pub(crate) fn report_warnings(report: &MigrationReport) {
    for warning in &report.warnings {
        let _ = writeln!(std::io::stderr(), "{warning}");
    }
}

pub(crate) fn generate_password() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

#[cfg(unix)]
pub(crate) fn machine_hostname(_: &ServerEnv) -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

#[cfg(not(unix))]
pub(crate) fn machine_hostname(env: &ServerEnv) -> String {
    env.get("COMPUTERNAME").map(|value| value.to_string_lossy().into_owned()).unwrap_or_default()
}

async fn teardown_server(watch: &mut Watch, signal: Signal, running: Running) -> std::process::ExitCode {
    crate::shutdown::teardown(watch, signal, running.shutdown()).await
}

async fn bind(hostname: &str, port: Option<u16>) -> std::io::Result<tokio::net::TcpListener> {
    if let Some(port) = port {
        return tokio::net::TcpListener::bind((hostname, port)).await;
    }
    let mut port = 4096;
    loop {
        match tokio::net::TcpListener::bind((hostname, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && port < 65_535 => port += 1,
            Err(error) => return Err(error),
        }
    }
}

fn connection_urls(address: std::net::SocketAddr) -> Vec<String> {
    let bound = match address.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
            std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, address.port()))
        }
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
            std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, address.port()))
        }
        _ => address,
    };
    vec![format!("http://{bound}")]
}

pub(crate) fn write_line(line: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}
