use opencode_bus::BusError;
use opencode_contract::codec::{Decode, DecodeError, Path, as_object, decode_field, decode_json, decode_optional};
use opencode_contract::generated::{
    AbsolutePath, AgentID, FileDiffInfo, FileDiffInfoStatus, LocationPublicRef, ModelRef, MoneyUSD, PermissionRuleset,
    ProjectID, RelativePath, SessionForkBoundary, SessionID, SessionInfo, SessionInfoFork, SessionInfoOutcome,
    SessionInfoTime, SessionMessageID, SessionMetadata, SessionRevert1, SnapshotID, TokenUsageInfo,
    TokenUsageInfoCache,
};
use rusqlite::Row;
use serde_json::Value;

use crate::projector::read_stored;

pub(crate) const COLUMNS: &str = "id, project_id, title, parent_id, fork_session_id, fork_boundary, agent, model, \
     cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, directory, path, \
     metadata, permission, revert, idle_outcome, time_created, time_updated, time_busy, time_idle, time_viewed, \
     time_archived";

pub(crate) struct SessionRow {
    pub id: String,
    pub project_id: String,
    pub title: Option<String>,
    pub parent_id: Option<String>,
    pub fork_session_id: Option<String>,
    pub fork_boundary: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub cost: f64,
    pub tokens_input: f64,
    pub tokens_output: f64,
    pub tokens_reasoning: f64,
    pub tokens_cache_read: f64,
    pub tokens_cache_write: f64,
    pub directory: String,
    pub path: Option<String>,
    pub metadata: Option<String>,
    pub permission: Option<String>,
    pub revert: Option<String>,
    pub idle_outcome: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_busy: Option<i64>,
    pub time_idle: Option<i64>,
    pub time_viewed: Option<i64>,
    pub time_archived: Option<i64>,
}

pub(crate) fn read(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        parent_id: row.get(3)?,
        fork_session_id: row.get(4)?,
        fork_boundary: row.get(5)?,
        agent: row.get(6)?,
        model: row.get(7)?,
        cost: row.get(8)?,
        tokens_input: row.get(9)?,
        tokens_output: row.get(10)?,
        tokens_reasoning: row.get(11)?,
        tokens_cache_read: row.get(12)?,
        tokens_cache_write: row.get(13)?,
        directory: row.get(14)?,
        path: row.get(15)?,
        metadata: row.get(16)?,
        permission: row.get(17)?,
        revert: row.get(18)?,
        idle_outcome: row.get(19)?,
        time_created: row.get(20)?,
        time_updated: row.get(21)?,
        time_busy: row.get(22)?,
        time_idle: row.get(23)?,
        time_viewed: row.get(24)?,
        time_archived: row.get(25)?,
    })
}

pub(crate) fn from_row(row: &SessionRow) -> Result<SessionInfo, BusError> {
    let fork = match (
        row.fork_session_id.as_deref().filter(|value| !value.is_empty()),
        row.fork_boundary.as_deref().filter(|value| !value.is_empty()),
    ) {
        (Some(session_id), Some(boundary)) => Some(SessionInfoFork {
            session_id: SessionID(session_id.to_string()),
            boundary: decode_json::<SessionForkBoundary>(boundary).map_err(|error| invalid(error.to_string()))?,
        }),
        _ => None,
    };
    let mut model = stored::<ModelRef>(row.model.as_deref())?;
    if let Some(model) = &mut model
        && model.variant.is_none()
    {
        model.variant = Some("default".to_string());
    }
    Ok(SessionInfo {
        id: SessionID(row.id.clone()),
        parent_id: row.parent_id.clone().map(SessionID),
        fork,
        project_id: ProjectID(row.project_id.clone()),
        agent: row.agent.clone().map(AgentID),
        model,
        cost: MoneyUSD(row.cost),
        tokens: TokenUsageInfo {
            input: row.tokens_input,
            output: row.tokens_output,
            reasoning: row.tokens_reasoning,
            cache: TokenUsageInfoCache { read: row.tokens_cache_read, write: row.tokens_cache_write },
        },
        outcome: row
            .idle_outcome
            .as_deref()
            .map(|value| {
                SessionInfoOutcome::decode(&Value::String(value.to_string()), &mut Path::new())
                    .map_err(|error| invalid(error.to_string()))
            })
            .transpose()?,
        time: SessionInfoTime {
            created: row.time_created as f64,
            updated: row.time_updated as f64,
            busy: row.time_busy.map(|value| value as f64),
            idle: row.time_idle.map(|value| value as f64),
            viewed: row.time_viewed.map(|value| value as f64),
            archived: row.time_archived.map(|value| value as f64),
        },
        title: row.title.clone(),
        subpath: row.path.as_deref().filter(|path| !path.is_empty()).map(|path| RelativePath(path.to_string())),
        metadata: read_stored(stored::<SessionMetadata>(row.metadata.as_deref())?.as_ref()).metadata,
        permissions: stored::<PermissionRuleset>(row.permission.as_deref())?,
        revert: revert(row.revert.as_deref())?,
        location: LocationPublicRef { directory: AbsolutePath(row.directory.clone()) },
    })
}

fn revert(raw: Option<&str>) -> Result<Option<SessionRevert1>, BusError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(raw).map_err(|error| invalid(error.to_string()))?;
    if value.is_null() {
        return Ok(None);
    }
    if let Ok(current) = SessionRevert1::decode(&value, &mut Path::new()) {
        return Ok(Some(current));
    }
    legacy_revert(&value).map(Some).map_err(|error| invalid(error.to_string()))
}

fn legacy_revert(value: &Value) -> Result<SessionRevert1, DecodeError> {
    let mut path = Path::new();
    let object = as_object(value, &path)?;
    let message_id = decode_field::<SessionMessageID>(object, "messageID", &mut path)?;
    let part_id = decode_optional::<String>(object, "partID", &mut path)?;
    let snapshot = decode_optional::<String>(object, "snapshot", &mut path)?
        .filter(|snapshot| !snapshot.is_empty())
        .map(SnapshotID);
    let files = decode_optional::<Vec<LegacyFileDiff>>(object, "files", &mut path)?
        .map(|files| files.into_iter().map(LegacyFileDiff::into_current).collect());
    Ok(SessionRevert1 { message_id, part_id, snapshot, files })
}

struct LegacyFileDiff {
    path: String,
    status: FileDiffInfoStatus,
    additions: f64,
    deletions: f64,
    patch: String,
}

impl LegacyFileDiff {
    fn into_current(self) -> FileDiffInfo {
        FileDiffInfo {
            file: self.path,
            patch: self.patch,
            additions: self.additions,
            deletions: self.deletions,
            status: self.status,
            more: None,
        }
    }
}

impl Decode for LegacyFileDiff {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        let object = as_object(value, path)?;
        Ok(Self {
            path: decode_field(object, "path", path)?,
            status: decode_field(object, "status", path)?,
            additions: decode_field(object, "additions", path)?,
            deletions: decode_field(object, "deletions", path)?,
            patch: decode_field(object, "patch", path)?,
        })
    }
}

fn stored<T: Decode>(raw: Option<&str>) -> Result<Option<T>, BusError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(raw).map_err(|error| invalid(error.to_string()))?;
    if value.is_null() {
        return Ok(None);
    }
    T::decode(&value, &mut Path::new()).map(Some).map_err(|error| invalid(error.to_string()))
}

fn invalid(message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: "session_v2".to_string(), message: message.into() }
}
