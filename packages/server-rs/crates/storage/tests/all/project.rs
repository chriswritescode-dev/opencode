use opencode_storage::column;
use opencode_storage::project::{self, ProjectDirectoryRow};
use opencode_storage::{Database, DatabaseLocation, StorageError};

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn read<T>(database: &Database, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StorageError>) -> T {
    database.read(f).expect("read the database")
}

fn count(database: &Database, sql: &str) -> i64 {
    read(database, |connection| Ok(connection.query_row(sql, [], |row| row.get(0))?))
}

#[test]
fn upsert_inserts_a_project_with_an_empty_sandboxes_array() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));

    let stored = read(&database, |connection| {
        Ok(connection.query_row(
            "SELECT sandboxes, time_created, time_updated, time_active FROM project WHERE id = 'p1'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
        )?)
    });
    assert_eq!(stored.0, "[]", "the sandboxes column stores an empty JSON array");
    assert_eq!(stored.1, stored.2, "the insert stamps the same time_created and time_updated");
    assert_eq!(stored.2, stored.3, "the insert stamps the same time_updated and time_active");
    assert_ne!(stored.1, 0, "the insert stamps a real timestamp");
}

#[test]
fn upserting_the_same_vcs_keeps_time_updated() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));
    read(&database, |connection| {
        connection.execute("UPDATE project SET time_updated = 1 WHERE id = 'p1'", [])?;
        Ok(())
    });

    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));

    let row = read(&database, |connection| project::get(connection, "p1")).expect("the project is present");
    assert_eq!(row.vcs.as_deref(), Some("git"));
    assert_eq!(row.time_updated, 1, "an unchanged vcs leaves time_updated untouched");
}

#[test]
fn upserting_a_different_vcs_updates_it_and_bumps_time_updated() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));
    read(&database, |connection| {
        connection.execute("UPDATE project SET time_updated = 1 WHERE id = 'p1'", [])?;
        Ok(())
    });

    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("hg")));

    let row = read(&database, |connection| project::get(connection, "p1")).expect("the project is present");
    assert_eq!(row.vcs.as_deref(), Some("hg"), "the changed vcs is stored");
    assert!(row.time_updated > 1, "the changed vcs bumps time_updated");
}

#[test]
fn upserting_without_a_vcs_clears_it() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));

    read(&database, |connection| project::upsert(connection, "p1", "/work/project", None));

    let row = read(&database, |connection| project::get(connection, "p1")).expect("the project is present");
    assert_eq!(row.vcs, None, "a missing vcs clears the stored one");
    assert_eq!(count(&database, "SELECT COUNT(*) FROM project WHERE vcs IS NULL"), 1);
}

#[test]
fn upsert_rejects_a_relative_canonical() {
    let (_dir, database) = database();
    let error = database
        .read(|connection| project::upsert(connection, "p1", "relative/path", None))
        .expect_err("a relative canonical is not stored");
    assert_eq!(error.to_string(), "Path is not absolute: relative/path");
    assert_eq!(count(&database, "SELECT COUNT(*) FROM project"), 0, "nothing is inserted");
}

#[test]
fn get_rejects_a_stored_relative_worktree() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));
    read(&database, |connection| {
        connection.execute("UPDATE project SET worktree = 'relative/path' WHERE id = 'p1'", [])?;
        Ok(())
    });

    let error = database.read(|connection| project::get(connection, "p1")).expect_err("the row cannot be decoded");
    assert_eq!(error.to_string(), "Path is not absolute: relative/path");
}

#[test]
fn list_skips_archived_projects_and_orders_by_activity() {
    let (_dir, database) = database();
    for (id, directory) in [("p1", "/work/one"), ("p2", "/work/two"), ("p3", "/work/three")] {
        read(&database, |connection| project::upsert(connection, id, directory, Some("git")));
    }
    read(&database, |connection| {
        connection.execute("UPDATE project SET time_active = 10 WHERE id = 'p1'", [])?;
        connection.execute("UPDATE project SET time_active = 30, time_archived = 7 WHERE id = 'p2'", [])?;
        connection.execute("UPDATE project SET time_active = 30 WHERE id = 'p3'", [])?;
        Ok(())
    });

    let active = read(&database, |connection| project::list(connection, false));
    assert_eq!(active.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["p3", "p1"]);

    let all = read(&database, |connection| project::list(connection, true));
    assert_eq!(all.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["p2", "p3", "p1"]);
    assert_eq!(all[0].time_archived, Some(7), "the archived project is included with its archive time");
}

#[test]
fn project_directories_round_trip_and_cascade() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));
    let worktree = read(&database, |connection| {
        ProjectDirectoryRow::insert(connection, "p1", "/work/project/worktree", Some("git_worktree"), Some("git"))
    });
    let root = read(&database, |connection| ProjectDirectoryRow::insert(connection, "p1", "/work/project", None, None));

    assert_eq!(worktree.project_id, "p1");
    assert_eq!(worktree.directory, "/work/project/worktree");
    assert_eq!(worktree.kind.as_deref(), Some("git_worktree"));
    assert_eq!(worktree.strategy.as_deref(), Some("git"));
    assert_ne!(worktree.time_created, 0);
    assert_eq!(root.kind, None);
    assert_eq!(root.strategy, None);

    let directories = read(&database, |connection| ProjectDirectoryRow::list(connection, "p1"));
    assert_eq!(directories, [root.clone(), worktree.clone()], "directories are ordered by directory");

    read(&database, |connection| {
        connection.execute("DELETE FROM project WHERE id = 'p1'", [])?;
        Ok(())
    });
    assert_eq!(
        count(&database, "SELECT COUNT(*) FROM project_directory"),
        0,
        "deleting the project cascades its directories"
    );
}

#[test]
fn absolute_columns_accept_posix_drive_and_unc_paths() {
    for path in ["/work/project", "C:/work/project", "//server/share"] {
        assert_eq!(column::stored_absolute_path(path).as_deref(), Some(path), "{path} is absolute");
    }
    assert_eq!(column::stored_absolute_path("relative/path"), None, "a relative path is not absolute");
    assert_eq!(
        column::to_driver_absolute("relative/path").expect_err("a relative path is rejected").to_string(),
        "Path is not absolute: relative/path"
    );

    let paths = ["/work/one".to_string(), "/work/two".to_string()];
    let stored = column::encode_absolute_array(&paths).expect("the paths are absolute");
    assert_eq!(stored, r#"["/work/one","/work/two"]"#, "the array is stored as JSON text");
    assert_eq!(column::decode_absolute_array(&stored).expect("the array decodes"), paths, "the array round-trips");
    assert_eq!(
        column::encode_absolute_array(&["//server/share".to_string()]).expect("the UNC path is absolute"),
        r#"["//server/share"]"#,
        "a UNC path is stored with forward slashes"
    );
    assert_eq!(
        column::decode_absolute_array(r#"["/work/one","relative"]"#)
            .expect_err("a stored relative path is rejected")
            .to_string(),
        "Path is not absolute: relative"
    );
}

#[test]
fn inserting_a_relative_directory_is_rejected() {
    let (_dir, database) = database();
    read(&database, |connection| project::upsert(connection, "p1", "/work/project", Some("git")));

    let error = database
        .read(|connection| ProjectDirectoryRow::insert(connection, "p1", "relative/path", None, None))
        .expect_err("a relative directory is not stored");
    assert_eq!(error.to_string(), "Path is not absolute: relative/path");
    assert_eq!(count(&database, "SELECT COUNT(*) FROM project_directory"), 0, "nothing is inserted");
}
