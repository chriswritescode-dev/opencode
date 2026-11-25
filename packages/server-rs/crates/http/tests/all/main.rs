mod auth;
mod compression;
mod config;
mod cors;
mod coverage;
mod debug;
mod event;
mod info;
mod location;
mod pairing;
mod plugin;
mod project;
mod request_site;
mod router;
mod serve;
mod session_log;
mod status;
mod ticket;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use http::Request;
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation};

const UNAUTHORIZED_BODY: &str = r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#;

fn services() -> Arc<GlobalServices> {
    services_with(Path::new("/tmp/opencode-http-test"), std::env::temp_dir().join("opencode-http-test-home"))
}

fn services_at(start_directory: &Path) -> Arc<GlobalServices> {
    services_with(start_directory, temp_directory("services-home"))
}

fn services_with(start_directory: &Path, home: PathBuf) -> Arc<GlobalServices> {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let database = Arc::new(database);
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.clone().into_os_string())).expect("global paths");
    let bus = opencode_bus::Bus::new(database.clone(), opencode_bus::BusOptions::default());
    Arc::new(GlobalServices::new(
        database,
        paths,
        start_directory.to_string_lossy().into_owned(),
        Arc::new(|_: &str| None),
        bus,
        global_plugins(),
    ))
}

fn global_plugins() -> Arc<opencode_plugin::GlobalPlugins> {
    Arc::new(opencode_plugin::GlobalPlugins {
        host: None,
        router: opencode_plugin::PluginRouter::default(),
        sdk: opencode_plugin::SdkPlugins::default(),
        updates: opencode_plugin::Updates::new(None, opencode_plugin::NpmPaths::default()),
        paths: opencode_plugin::NpmPaths::default(),
        app: opencode_plugin::AppInfo {
            name: "opencode".to_owned(),
            version: "test-version".to_owned(),
            channel: "local".to_owned(),
        },
    })
}

fn ready_app_with_services(services: Arc<GlobalServices>) -> Arc<App> {
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
        state: None,
        services: Some(services),
    });
    app.status().ready();
    app
}

fn app() -> Arc<App> {
    make_app(Vec::new(), None)
}

fn app_with_cors(cors: Vec<String>) -> Arc<App> {
    make_app(cors, None)
}

fn app_with_hostname(hostname: Option<&str>) -> Arc<App> {
    make_app(Vec::new(), hostname)
}

fn make_app(cors: Vec<String>, hostname: Option<&str>) -> Arc<App> {
    App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors,
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: hostname.map(str::to_owned),
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
        state: None,
        services: Some(services()),
    })
}

fn ready_app() -> Arc<App> {
    let app = app();
    app.status().ready();
    app
}

fn ready_app_with_cors(cors: Vec<String>) -> Arc<App> {
    let app = app_with_cors(cors);
    app.status().ready();
    app
}

fn request(method: &str, uri: &str) -> Request<Bytes> {
    Request::builder().method(method).uri(uri).body(Bytes::new()).expect("a request")
}

fn request_with(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Bytes> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Bytes::new()).expect("a request")
}

fn encoded(user: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
}

fn basic(user: &str, password: &str) -> String {
    let credential = encoded(user, password);
    format!("Basic {credential}")
}

fn authorized(method: &str, uri: &str) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    request_with(method, uri, &[("authorization", credential.as_str())])
}

fn authorized_with(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    let mut all = Vec::with_capacity(headers.len() + 1);
    all.push(("authorization", credential.as_str()));
    all.extend_from_slice(headers);
    request_with(method, uri, &all)
}

fn temp_directory(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "opencode-http-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("create the temp directory");
    path
}

async fn text(response: opencode_http::response::Response) -> String {
    use http_body_util::BodyExt;
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

async fn body(response: opencode_http::response::Response) -> Bytes {
    use http_body_util::BodyExt;
    response.into_body().collect().await.expect("a body").to_bytes()
}
