use std::io::Write;
use std::sync::Arc;

use base64::Engine;
use tokio::sync::oneshot;

use crate::cli::Serve;
use crate::env::ServerEnv;
use crate::shutdown::{Signal, Watch};

pub fn run_serve(args: Serve, env: ServerEnv) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    let result = runtime.block_on(serve(args, env));
    runtime.shutdown_background();
    match result {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::from(1)
        }
    }
}

async fn serve(args: Serve, mut env: ServerEnv) -> std::io::Result<std::process::ExitCode> {
    env.remove("OPENCODE_PTY_HANDOFF");
    if args.service {
        return crate::service_mode::run(args, env).await;
    }
    let environment_password = env.password();
    if args.stdio {
        env.remove("OPENCODE_PASSWORD");
        env.remove("OPENCODE_SERVER_PASSWORD");
    }
    let hostname = args.hostname.unwrap_or_else(|| "127.0.0.1".to_owned());
    let tmp = opencode_local::paths::Paths::from_vars(|name| env.get(name).map(std::ffi::OsStr::to_os_string))?.tmp;
    std::fs::create_dir_all(&tmp)?;
    let tmp = std::fs::canonicalize(&tmp)?;
    let listener = bind(&hostname, args.port).await?;
    let address = listener.local_addr()?;
    let generated = environment_password.is_none();
    let password = environment_password
        .unwrap_or_else(|| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()));
    let running = start(listener, tmp, password.clone(), hostname, args.cors).await?;
    let mut watch = Watch::new()?;
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
}

impl Running {
    pub(crate) fn ready(&self) {
        self.app.status().ready();
    }

    pub(crate) async fn shutdown(self) {
        self.app.status().begin_stopping();
        let _ = self.shutdown.send(());
        let _ = self.server.await;
    }
}

pub(crate) async fn start(
    listener: tokio::net::TcpListener,
    tmp: std::path::PathBuf,
    password: String,
    hostname: String,
    cors: Vec<String>,
) -> std::io::Result<Running> {
    let address = listener.local_addr()?;
    let urls = Arc::new(move || connection_urls(address));
    let app = opencode_http::app::App::new(opencode_http::app::Options {
        password,
        version: crate::VERSION.to_owned(),
        cors,
        tmp,
        hostname: Some(hostname),
        urls,
    });
    let (shutdown, shutdown_receiver) = oneshot::channel::<()>();
    let server = tokio::spawn(opencode_http::serve::serve(listener, app.clone(), async move {
        let _ = shutdown_receiver.await;
    }));
    Ok(Running { app, shutdown, server })
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
