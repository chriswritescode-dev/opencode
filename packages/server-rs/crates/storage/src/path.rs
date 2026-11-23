//! The database filename and its absolute path, as `packages/cli/src/database-path.ts` computes them.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use opencode_util::path::Flavor;

/// The channel names that share `opencode.db`.
const SHARED: [&str; 5] = ["latest", "dev", "beta", "next", "prod"];

/// Where the database lives: SQLite's in-memory database, or a file resolved from the data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseLocation {
    Memory,
    File(PathBuf),
}

/// `databasePath`: `OPENCODE_DB` when set, else `opencode.db` for the shared channels or when
/// `OPENCODE_DISABLE_CHANNEL_DB` is `1` or `true`, else `opencode-<channel>.db` with each UTF-16 unit
/// outside `[a-zA-Z0-9._-]` replaced by `-`. `:memory:` stays in memory; every other name resolves
/// against `data` like `path.resolve`.
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
