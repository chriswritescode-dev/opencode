use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::StorageError;
use crate::column;
use crate::id::now_ms;

const COLUMNS: &str = "project_id, directory, strategy, time_created";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRow {
    pub project_id: String,
    pub directory: String,
    pub strategy: Option<String>,
    pub time_created: i64,
}

pub fn list(db: &Connection, project_id: &str) -> Result<Vec<WorktreeRow>, StorageError> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM worktree WHERE project_id = ?1 ORDER BY time_created DESC, directory ASC"
    ))?;
    let mut rows = statement.query(params![project_id])?;
    let mut worktrees = Vec::new();
    while let Some(row) = rows.next()? {
        worktrees.push(read_worktree(row)?);
    }
    Ok(worktrees)
}

pub fn find(db: &Connection, project_id: &str, directory: &str) -> Result<Option<WorktreeRow>, StorageError> {
    let stored = column::to_driver_absolute(directory)?;
    let mut statement =
        db.prepare_cached(&format!("SELECT {COLUMNS} FROM worktree WHERE project_id = ?1 AND directory = ?2"))?;
    let mut rows = statement.query(params![project_id, stored])?;
    rows.next()?.map(read_worktree).transpose()
}

pub fn upsert(
    db: &Connection,
    project_id: &str,
    directory: &str,
    strategy: Option<&str>,
    replace: bool,
) -> Result<bool, StorageError> {
    let stored = column::to_driver_absolute(directory)?;
    let guard = match (replace, strategy.is_some()) {
        (true, _) => "",
        (false, true) => " WHERE worktree.strategy IS NULL",
        (false, false) => " WHERE false",
    };
    let sql = format!(
        "INSERT INTO worktree (project_id, directory, strategy, time_created)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (project_id, directory) DO UPDATE SET strategy = excluded.strategy{guard}
         RETURNING directory"
    );
    let changed: Option<String> = db
        .prepare_cached(&sql)?
        .query_row(params![project_id, stored, strategy, now_ms()], |row| row.get(0))
        .optional()?;
    Ok(changed.is_some())
}

pub fn delete(db: &Connection, project_id: &str, directory: &str) -> Result<bool, StorageError> {
    let stored = column::to_driver_absolute(directory)?;
    let removed: Option<String> = db
        .prepare_cached("DELETE FROM worktree WHERE project_id = ?1 AND directory = ?2 RETURNING directory")?
        .query_row(params![project_id, stored], |row| row.get(0))
        .optional()?;
    Ok(removed.is_some())
}

pub fn insert_ignore(
    db: &Connection,
    project_id: &str,
    directory: &str,
    strategy: Option<&str>,
) -> Result<(), StorageError> {
    let stored = column::to_driver_absolute(directory)?;
    db.prepare_cached(
        "INSERT INTO worktree (project_id, directory, strategy, time_created)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![project_id, stored, strategy, now_ms()])?;
    Ok(())
}

fn read_worktree(row: &Row<'_>) -> Result<WorktreeRow, StorageError> {
    Ok(WorktreeRow {
        project_id: row.get(0)?,
        directory: column::from_driver_absolute(&row.get::<_, String>(1)?)?,
        strategy: row.get(2)?,
        time_created: row.get(3)?,
    })
}
