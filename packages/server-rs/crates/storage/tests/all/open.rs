use std::collections::HashMap;
use std::ffi::OsString;

use opencode_storage::{Database, DatabaseLocation, database_path};

use crate::TempDir;

fn resolve(dir: &TempDir, channel: &str, pairs: &[(&str, &str)]) -> DatabaseLocation {
    let vars: HashMap<String, OsString> =
        pairs.iter().map(|(name, value)| ((*name).to_string(), OsString::from(value))).collect();
    database_path(dir.path(), channel, |name| vars.get(name).cloned())
}

fn in_dir(dir: &TempDir, name: &str) -> DatabaseLocation {
    DatabaseLocation::File(dir.path().join(name))
}

#[test]
fn shared_channels_share_opencode_db() {
    let dir = TempDir::new();
    for channel in ["latest", "dev", "beta", "next", "prod"] {
        assert_eq!(resolve(&dir, channel, &[]), in_dir(&dir, "opencode.db"), "channel {channel}");
    }
}

#[test]
fn disabling_the_channel_db_uses_opencode_db() {
    let dir = TempDir::new();
    for value in ["1", "true"] {
        assert_eq!(
            resolve(&dir, "local", &[("OPENCODE_DISABLE_CHANNEL_DB", value)]),
            in_dir(&dir, "opencode.db"),
            "OPENCODE_DISABLE_CHANNEL_DB={value}"
        );
    }
}

#[test]
fn only_the_exact_disable_values_use_opencode_db() {
    let dir = TempDir::new();
    for value in ["0", "TRUE", "yes"] {
        assert_eq!(
            resolve(&dir, "local", &[("OPENCODE_DISABLE_CHANNEL_DB", value)]),
            in_dir(&dir, "opencode-local.db"),
            "OPENCODE_DISABLE_CHANNEL_DB={value}"
        );
    }
}

#[test]
fn other_channels_get_their_own_file() {
    let dir = TempDir::new();
    assert_eq!(resolve(&dir, "local", &[]), in_dir(&dir, "opencode-local.db"));
    assert_eq!(resolve(&dir, "a/b", &[]), in_dir(&dir, "opencode-a-b.db"));
    assert_eq!(resolve(&dir, "🚀", &[]), in_dir(&dir, "opencode---.db"));
}

#[test]
fn opencode_db_overrides_the_filename() {
    let dir = TempDir::new();
    let absolute = dir.path().join("absolute.db").to_string_lossy().into_owned();
    assert_eq!(
        resolve(&dir, "local", &[("OPENCODE_DB", &absolute)]),
        DatabaseLocation::File(dir.path().join("absolute.db"))
    );
    assert_eq!(resolve(&dir, "local", &[("OPENCODE_DB", "custom.db")]), in_dir(&dir, "custom.db"));
    assert_eq!(resolve(&dir, "local", &[("OPENCODE_DB", "")]), DatabaseLocation::File(dir.path().to_path_buf()));
    assert_eq!(resolve(&dir, "latest", &[("OPENCODE_DB", ":memory:")]), DatabaseLocation::Memory);
}

#[test]
fn opencode_db_is_resolved_and_normalized() {
    let dir = TempDir::new();
    assert_eq!(resolve(&dir, "local", &[("OPENCODE_DB", "sub/../x.db")]), in_dir(&dir, "x.db"));
}

#[test]
fn file_database_is_owner_only_and_configured() {
    let dir = TempDir::new();
    let path = dir.path().join("opencode.db");
    let (database, report) =
        Database::open(DatabaseLocation::File(path.clone()), dir.path()).expect("open the database");

    assert_eq!(database.location(), &DatabaseLocation::File(path.clone()));
    assert!(report.bootstrapped, "the empty file is bootstrapped");
    assert!(report.applied.is_empty(), "bootstrap records migrations without applying them");
    assert!(report.warnings.is_empty(), "no migration warned");

    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600, "the database file is owner-only");

    database
        .read(|connection| {
            connection.execute_batch("CREATE TABLE t (id INTEGER)")?;
            Ok(())
        })
        .expect("create a table");

    #[cfg(unix)]
    {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push("-wal");
        assert_eq!(mode(std::path::Path::new(&sidecar)), 0o600, "the WAL sidecar is owner-only");
    }

    assert_eq!(text(&database, "journal_mode"), "wal");
    assert_eq!(integer(&database, "synchronous"), 1);
    assert_eq!(integer(&database, "busy_timeout"), 5000);
    assert_eq!(integer(&database, "cache_size"), -64000);
    assert_eq!(integer(&database, "foreign_keys"), 1);
}

#[cfg(unix)]
#[test]
fn existing_sidecars_are_restricted_to_owner() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new();
    let path = dir.path().join("opencode.db");
    let _ = Database::open(DatabaseLocation::File(path.clone()), dir.path()).expect("bootstrap the database");
    let connection = rusqlite::Connection::open(&path).expect("open a raw connection");
    connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).expect("enable WAL");
    connection.execute_batch("CREATE TABLE t (id INTEGER)").expect("create a table");
    for candidate in [path.clone(), sidecar(&path, "-wal"), sidecar(&path, "-shm")] {
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o644))
            .expect("loosen the file permissions");
        assert_eq!(mode(&candidate), 0o644, "{} starts world readable", candidate.display());
    }

    let (database, _) = Database::open(DatabaseLocation::File(path.clone()), dir.path()).expect("open the database");
    for candidate in [path.clone(), sidecar(&path, "-wal"), sidecar(&path, "-shm")] {
        assert_eq!(mode(&candidate), 0o600, "{} is restricted to the owner", candidate.display());
    }
    drop(database);
    drop(connection);
}

#[test]
fn memory_database_is_configured() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    assert_eq!(database.location(), &DatabaseLocation::Memory);
    assert_eq!(text(&database, "journal_mode"), "memory");
    assert_eq!(integer(&database, "synchronous"), 1);
    assert_eq!(integer(&database, "busy_timeout"), 5000);
    assert_eq!(integer(&database, "cache_size"), -64000);
    assert_eq!(integer(&database, "foreign_keys"), 1);
}

fn text(database: &Database, pragma: &str) -> String {
    database
        .read(|connection| Ok(connection.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))?))
        .expect("read a text pragma")
}

fn integer(database: &Database, pragma: &str) -> i64 {
    database
        .read(|connection| Ok(connection.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))?))
        .expect("read an integer pragma")
}

#[cfg(unix)]
fn sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    std::path::PathBuf::from(sidecar)
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("read the file metadata").permissions().mode() & 0o777
}
