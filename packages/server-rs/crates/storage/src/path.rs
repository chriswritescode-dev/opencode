use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use opencode_util::path::Flavor;

const SHARED: [&str; 5] = ["latest", "dev", "beta", "next", "prod"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseLocation {
    Memory,
    File(PathBuf),
}

pub fn database_path(data: &Path, channel: &str, var: impl Fn(&str) -> Option<OsString>) -> DatabaseLocation {
    let filename = match var("OPENCODE_DB") {
        Some(value) => value.to_string_lossy().into_owned(),
        None => {
            let disabled = var("OPENCODE_DISABLE_CHANNEL_DB");
            let disabled = disabled.as_deref().and_then(OsStr::to_str);
            if SHARED.contains(&channel) || matches!(disabled, Some("1" | "true")) {
                "opencode.db".to_string()
            } else {
                format!("opencode-{}.db", safe_channel(channel))
            }
        }
    };
    if filename == ":memory:" {
        return DatabaseLocation::Memory;
    }
    DatabaseLocation::File(PathBuf::from(Flavor::NATIVE.resolve(&[&data.to_string_lossy(), &filename])))
}

fn safe_channel(channel: &str) -> String {
    channel
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_string()
            } else {
                "-".repeat(c.len_utf16())
            }
        })
        .collect()
}
