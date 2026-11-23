use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use opencode_sidecar::bun::Runtime;
use opencode_sidecar::config::{Config, Limits};

pub fn config() -> Config {
    let vars = ["PATH", "HOME"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect::<Vec<_>>();
    let directory = std::env::current_dir().expect("a directory");
    let mut config = Config::new(vars, directory, std::env::temp_dir());
    config.test_methods = true;
    config.limits = Limits { max_frame: 1 << 20, handshake: Duration::from_secs(10), ..Limits::default() };
    config
}

pub async fn runtime(config: &Config) -> Option<Runtime> {
    let runtime = config.runtime().await;
    if runtime.is_none() {
        eprintln!("skipping: neither Bun nor an opencode runtime was found");
    }
    runtime
}

pub fn lookup(pairs: impl IntoIterator<Item = (&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
    let vars = pairs.into_iter().map(|(name, value)| (name.to_owned(), value)).collect::<Vec<_>>();
    move |name| vars.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
}

pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("an executable");
    path
}
