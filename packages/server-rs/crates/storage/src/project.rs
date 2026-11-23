use rusqlite::{Connection, Row, params};
use serde_json::Value;

use crate::StorageError;
use crate::column;
use crate::id::now_ms;

macro_rules! select_project {
    ($rest:literal) => {
        concat!(
            "SELECT id, worktree, vcs, name, icon_url, icon_url_override, icon_color, time_created, time_updated, time_initialized, time_active, time_archived, sandboxes, commands FROM project",
            $rest
        )
    };
}

const GET: &str = select_project!(" WHERE id = ?1");
const LIST_ACTIVE: &str = select_project!(" WHERE time_archived IS NULL ORDER BY time_active DESC, id ASC");
const LIST_ALL: &str = select_project!(" ORDER BY time_active DESC, id ASC");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    pub id: String,
    pub worktree: String,
    pub vcs: Option<String>,
    pub name: Option<String>,
    pub icon_url: Option<String>,
    pub icon_url_override: Option<String>,
    pub icon_color: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_initialized: Option<i64>,
    pub time_active: i64,
    pub time_archived: Option<i64>,
    pub sandboxes: Vec<String>,
    pub commands: Option<Value>,
}

pub fn get(db: &Connection, id: &str) -> Result<Option<ProjectRow>, StorageError> {
    let mut statement = db.prepare_cached(GET)?;
    let mut rows = statement.query(params![id])?;
    Ok(collect(&mut rows)?.into_iter().next())
}

pub fn list(db: &Connection, include_archived: bool) -> Result<Vec<ProjectRow>, StorageError> {
    let mut statement = db.prepare_cached(if include_archived { LIST_ALL } else { LIST_ACTIVE })?;
    let mut rows = statement.query([])?;
    collect(&mut rows)
}

pub fn upsert(db: &Connection, id: &str, canonical: &str, vcs: Option<&str>) -> Result<(), StorageError> {
    let worktree = column::to_driver_absolute(canonical)?;
    let now = now_ms();
    match vcs {
        Some(vcs) => db
            .prepare_cached(
                "INSERT INTO project (id, worktree, vcs, sandboxes, time_created, time_updated, time_active)
             VALUES (?1, ?2, ?3, '[]', ?4, ?4, ?4)
             ON CONFLICT (id) DO UPDATE SET vcs = ?3, time_updated = ?4
             WHERE project.vcs IS NULL OR project.vcs <> ?3",
            )?
            .execute(params![id, worktree, vcs, now])?,
        None => db
            .prepare_cached(
                "INSERT INTO project (id, worktree, vcs, sandboxes, time_created, time_updated, time_active)
             VALUES (?1, ?2, NULL, '[]', ?3, ?3, ?3)
             ON CONFLICT (id) DO UPDATE SET vcs = NULL, time_updated = ?3
             WHERE project.vcs IS NOT NULL",
            )?
            .execute(params![id, worktree, now])?,
    };
    Ok(())
}

fn collect(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<ProjectRow>, StorageError> {
    let mut projects = Vec::new();
    while let Some(row) = rows.next()? {
        projects.push(read_project(row)?);
    }
    Ok(projects)
}

fn read_project(row: &Row<'_>) -> Result<ProjectRow, StorageError> {
    let worktree = column::from_driver_absolute(&row.get::<_, String>(1)?)?;
    let sandboxes = column::decode_absolute_array(&row.get::<_, String>(12)?)?;
    let commands: Option<Value> =
        row.get::<_, Option<String>>(13)?.map(|text| serde_json::from_str(&text)).transpose()?;
    Ok(ProjectRow {
        id: row.get(0)?,
        worktree,
        vcs: row.get(2)?,
        name: row.get(3)?,
        icon_url: row.get(4)?,
        icon_url_override: row.get(5)?,
        icon_color: row.get(6)?,
        time_created: row.get(7)?,
        time_updated: row.get(8)?,
        time_initialized: row.get(9)?,
        time_active: row.get(10)?,
        time_archived: row.get(11)?,
        sandboxes,
        commands,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDirectoryRow {
    pub project_id: String,
    pub directory: String,
    pub kind: Option<String>,
    pub strategy: Option<String>,
    pub time_created: i64,
}

impl ProjectDirectoryRow {
    pub fn insert(
        db: &Connection,
        project_id: &str,
        directory: &str,
        kind: Option<&str>,
        strategy: Option<&str>,
    ) -> Result<ProjectDirectoryRow, StorageError> {
        let stored = column::to_driver_absolute(directory)?;
        let time_created = now_ms();
        db.prepare_cached(
            "INSERT INTO project_directory (project_id, directory, type, strategy, time_created)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![project_id, stored, kind, strategy, time_created])?;
        Ok(ProjectDirectoryRow {
            project_id: project_id.to_string(),
            directory: directory.to_string(),
            kind: kind.map(str::to_string),
            strategy: strategy.map(str::to_string),
            time_created,
        })
    }

    pub fn list(db: &Connection, project_id: &str) -> Result<Vec<ProjectDirectoryRow>, StorageError> {
        let mut statement = db.prepare_cached(
            "SELECT project_id, directory, type, strategy, time_created FROM project_directory
             WHERE project_id = ?1 ORDER BY directory",
        )?;
        let mut rows = statement.query(params![project_id])?;
        let mut directories = Vec::new();
        while let Some(row) = rows.next()? {
            directories.push(ProjectDirectoryRow {
                project_id: row.get(0)?,
                directory: column::from_driver_absolute(&row.get::<_, String>(1)?)?,
                kind: row.get(2)?,
                strategy: row.get(3)?,
                time_created: row.get(4)?,
            });
        }
        Ok(directories)
    }
}
