use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use opencode_bus::{Bus, Definition, Event, PublishOptions};
use opencode_contract::codec::{Decode, Encode, Path, encode_json, write_optional, write_value};
use opencode_contract::generated::requests::{SessionsQuery, SessionsQueryOrder};
use opencode_contract::generated::{
    AbsolutePath, AgentID, ModelRef, PermissionRuleset, ProjectID, RelativePath, SessionAgentSelectedData,
    SessionArchivedData, SessionCreatedData, SessionDeletedData, SessionID, SessionInboxUser, SessionInfo,
    SessionMetadata, SessionMetadataUpdatedData, SessionModelSelectedData, SessionPermissionsData, SessionRenamedData,
    SessionUnarchivedData, SessionsCursor, SessionsResponse, SessionsResponseCursor, WorkspaceID,
};
use opencode_location::environment::Environment;
use opencode_location::location::LocationRef;
use opencode_location::project::{ProjectService, ResolveOptions};
use opencode_storage::id;
use opencode_util::path::Flavor;
use serde_json::{Map, Value};

use crate::execution::{NoopExecution, SessionExecution};
use crate::prompt::{self, PromptInput};
use crate::store::{Direction, LastAssistantInput, ListAnchor, ListInput, Order, SessionStore};

const DEFAULT_LIST_LIMIT: i64 = 50;

pub struct CreateInput {
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<ModelRef>,
    pub location: Option<LocationRef>,
    pub metadata: Option<SessionMetadata>,
    pub permissions: Option<PermissionRuleset>,
}

pub struct UpdateInput {
    pub title: Option<String>,
    pub metadata: Option<SessionMetadata>,
    pub permissions: Option<PermissionRuleset>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    NotFound(String),
    InvalidCursor,
    Conflict { message_id: String },
    Internal(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotFound(session_id) => write!(formatter, "Session not found: {session_id}"),
            SessionError::InvalidCursor => formatter.write_str("Invalid cursor"),
            SessionError::Conflict { message_id } => {
                write!(formatter, "Prompt message ID conflicts with an existing durable record: {message_id}")
            }
            SessionError::Internal(message) => formatter.write_str(message),
        }
    }
}

pub struct Sessions {
    store: SessionStore,
    bus: Arc<Bus>,
    projects: Arc<ProjectService>,
    environment: Arc<dyn Environment>,
    execution: Arc<dyn SessionExecution>,
    version: String,
}

impl Sessions {
    pub fn new(
        store: SessionStore,
        bus: Arc<Bus>,
        projects: Arc<ProjectService>,
        environment: Arc<dyn Environment>,
        version: String,
    ) -> Self {
        Self { store, bus, projects, environment, execution: Arc::new(NoopExecution), version }
    }

    pub fn with_execution(mut self, execution: Arc<dyn SessionExecution>) -> Self {
        self.execution = execution;
        self
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    pub fn execution(&self) -> &Arc<dyn SessionExecution> {
        &self.execution
    }

    pub async fn wait_idle(&self, session_id: &str) {
        self.execution.wait_idle(session_id).await;
    }

    pub fn active(&self) -> Vec<String> {
        self.execution.active()
    }

    pub async fn shutdown(&self) {
        self.execution.shutdown().await;
    }

    pub fn prompt(&self, session_id: &str, input: PromptInput) -> Result<SessionInboxUser, SessionError> {
        self.get(session_id)?;
        let message_id = input.id.clone().unwrap_or_else(|| format!("msg_{}", id::ascending()));
        if let Some(item) = prompt::existing(&self.store, session_id, &message_id, input.delivery)? {
            if input.resume {
                self.execution.wake(session_id);
            }
            return Ok(item);
        }
        let data = prompt::enqueued(&message_id, session_id, &input);
        let location = self
            .store
            .session_location(session_id)
            .ok()
            .flatten()
            .map(|location| crate::execution::contract_location(&location));
        let event =
            match self.publish("session.inbox.enqueued", &data, PublishOptions { location, ..Default::default() }) {
                Ok(event) => event,
                Err(error) => {
                    return match prompt::existing(&self.store, session_id, &message_id, input.delivery) {
                        Ok(Some(item)) => {
                            if input.resume {
                                self.execution.wake(session_id);
                            }
                            Ok(item)
                        }
                        Ok(None) => Err(error),
                        Err(conflict) => Err(conflict),
                    };
                }
            };
        if input.resume {
            self.execution.wake(session_id);
        }
        Ok(prompt::admitted(&message_id, session_id, &input, event.created() as f64))
    }

    pub async fn create(&self, input: CreateInput) -> Result<SessionInfo, SessionError> {
        let session_id = input.id.clone().unwrap_or_else(|| format!("ses_{}", id::descending()));
        if let Some(recorded) = self.store.get(&session_id).map_err(internal)? {
            return Ok(recorded);
        }
        let parent = match input.parent_id.as_deref() {
            Some(parent_id) => Some(
                self.store
                    .get(parent_id)
                    .map_err(internal)?
                    .ok_or_else(|| SessionError::NotFound(parent_id.to_owned()))?,
            ),
            None => None,
        };
        let location = match &parent {
            Some(parent) => self
                .store
                .session_location(&parent.id.0)
                .map_err(internal)?
                .ok_or_else(|| SessionError::Internal("Session.create parent has no location".to_owned()))?,
            None => input.location.as_ref().map(LocationRef::canonical).ok_or_else(|| {
                SessionError::Internal("Session.create requires either location or an existing parentID".to_owned())
            })?,
        };
        let project = self
            .projects
            .resolve(self.environment.as_ref(), &location.directory, ResolveOptions::persist())
            .await
            .map_err(internal)?;
        let subpath = Flavor::NATIVE.relative(&project.directory, &location.directory).replace('\\', "/");
        let contract_location = opencode_contract::generated::LocationRef {
            directory: AbsolutePath(location.directory.clone()),
            workspace_id: location.workspace_id.clone().map(WorkspaceID),
        };
        let data = SessionCreatedData {
            session_id: SessionID(session_id.clone()),
            project_id: ProjectID(project.id),
            location: contract_location.clone(),
            subpath: Some(RelativePath(subpath)),
            parent_id: input.parent_id.clone().map(SessionID),
            slug: crate::slug::create(),
            title: input.title.clone(),
            agent: input.agent.clone().map(AgentID),
            model: input.model.clone(),
            metadata: input.metadata.clone().or_else(|| parent.as_ref().and_then(|parent| parent.metadata.clone())),
            permissions: input
                .permissions
                .clone()
                .or_else(|| parent.as_ref().and_then(|parent| parent.permissions.clone())),
            version: self.version.clone(),
        };
        let options = PublishOptions { location: Some(contract_location), ..Default::default() };
        if let Err(error) = self.publish("session.created", &data, options) {
            if let Some(recorded) = self.store.get(&session_id).map_err(internal)? {
                return Ok(recorded);
            }
            return Err(error);
        }
        self.store.get(&session_id).map_err(internal)?.ok_or(SessionError::NotFound(session_id))
    }

    pub fn get(&self, session_id: &str) -> Result<SessionInfo, SessionError> {
        self.store.get(session_id).map_err(internal)?.ok_or_else(|| SessionError::NotFound(session_id.to_owned()))
    }

    pub fn list(&self, query: &SessionsQuery) -> Result<SessionsResponse, SessionError> {
        let filter = match query.cursor.as_ref().and_then(|cursor| cursor.as_ref()) {
            Some(cursor) => parse_cursor(&cursor.0)?,
            None => ListFilter::from_query(query),
        };
        let limit = query.limit.flatten().map(|limit| limit as i64).unwrap_or(DEFAULT_LIST_LIMIT);
        let data = self.store.list(&filter.to_store(limit)).map_err(internal)?;
        let previous =
            data.first().map(|session| Some(cursor(&filter, &session.id.0, session.time.updated, Direction::Previous)));
        let next =
            data.last().map(|session| Some(cursor(&filter, &session.id.0, session.time.updated, Direction::Next)));
        Ok(SessionsResponse { data, cursor: SessionsResponseCursor { previous, next } })
    }

    pub async fn update(&self, session_id: &str, input: UpdateInput) -> Result<(), SessionError> {
        let session = self.get(session_id)?;
        let id = SessionID(session.id.0.clone());
        if let Some(title) = input.title.as_deref().filter(|title| !title.is_empty()) {
            let data = SessionRenamedData { session_id: id.clone(), title: title.to_owned() };
            self.publish("session.renamed", &data, PublishOptions::default())?;
        }
        if let Some(metadata) = &input.metadata {
            let data = SessionMetadataUpdatedData { session_id: id.clone(), metadata: metadata.clone() };
            self.publish("session.metadata.updated", &data, PublishOptions::default())?;
        }
        if let Some(permissions) = &input.permissions {
            let data = SessionPermissionsData { session_id: id.clone(), permissions: permissions.clone() };
            self.publish("session.permissions", &data, PublishOptions::default())?;
        }
        if let Some(archived) = input.archived {
            let current = session.time.archived.is_some();
            if archived && !current {
                self.publish(
                    "session.archived",
                    &SessionArchivedData { session_id: id.clone() },
                    PublishOptions::default(),
                )?;
            } else if !archived && current {
                self.publish(
                    "session.unarchived",
                    &SessionUnarchivedData { session_id: id.clone() },
                    PublishOptions::default(),
                )?;
            }
        }
        Ok(())
    }

    pub fn switch_agent(&self, session_id: &str, agent: &str) -> Result<(), SessionError> {
        let session = self.get(session_id)?;
        let current = match &session.agent {
            Some(current) => Some(current.0.clone()),
            None => self
                .store
                .last_assistant_after(&LastAssistantInput { session_id: session_id.to_owned(), after_seq: None })
                .ok()
                .flatten()
                .map(|assistant| assistant.agent.0),
        };
        if current.as_deref() == Some(agent) {
            return Ok(());
        }
        let data = SessionAgentSelectedData {
            session_id: SessionID(session_id.to_owned()),
            agent: AgentID(agent.to_owned()),
            previous: session.agent.clone(),
        };
        self.publish("session.agent.selected", &data, PublishOptions::default())?;
        Ok(())
    }

    pub fn switch_model(&self, session_id: &str, model: &ModelRef) -> Result<(), SessionError> {
        let session = self.get(session_id)?;
        if let Some(current) = &session.model
            && current.provider_id.0 == model.provider_id.0
            && current.id == model.id
            && current.variant.as_deref().unwrap_or("default") == model.variant.as_deref().unwrap_or("default")
        {
            return Ok(());
        }
        let data = SessionModelSelectedData {
            session_id: SessionID(session_id.to_owned()),
            model: model.clone(),
            previous: session.model.clone(),
        };
        self.publish("session.model.selected", &data, PublishOptions::default())?;
        Ok(())
    }

    pub async fn remove(&self, session_id: &str) -> Result<(), SessionError> {
        self.get(session_id)?;
        let mut stack = vec![session_id.to_owned()];
        while let Some(current) = stack.pop() {
            self.execution.interrupt(&current).await;
            let children = self
                .store
                .list(&ListInput { parent_id: Some(Some(current.clone())), ..Default::default() })
                .map_err(internal)?;
            self.publish(
                "session.deleted",
                &SessionDeletedData { session_id: SessionID(current.clone()) },
                PublishOptions::default(),
            )?;
            self.bus.remove(&current).map_err(internal)?;
            stack.extend(children.into_iter().rev().map(|child| child.id.0));
        }
        Ok(())
    }

    fn publish<T: Encode>(&self, r#type: &str, data: &T, options: PublishOptions) -> Result<Event, SessionError> {
        let definition = Definition::latest_durable(r#type)
            .ok_or_else(|| SessionError::Internal(format!("No durable event definition for {}", r#type)))?;
        let value = serde_json::from_str::<Value>(&encode_json(data)).map_err(internal)?;
        self.bus.publish(&definition, value, options).map_err(internal)
    }
}

#[derive(Clone)]
struct ListFilter {
    order: Option<SessionsQueryOrder>,
    search: Option<String>,
    parent_id: Option<Option<String>>,
    archived: Option<bool>,
    directory: Option<String>,
    project: Option<String>,
    subpath: Option<String>,
    anchor: Option<CursorAnchor>,
}

#[derive(Clone)]
struct CursorAnchor {
    id: String,
    time: f64,
    direction: Direction,
}

impl ListFilter {
    fn from_query(query: &SessionsQuery) -> Self {
        Self {
            order: query.order.flatten(),
            search: query.search.clone().flatten(),
            parent_id: query.parent_id.clone().map(|parent_id| parent_id.map(|parent_id| parent_id.0)),
            archived: query.archived.flatten(),
            directory: query.directory.clone().flatten().map(|directory| directory.0),
            project: query.project.clone().flatten().map(|project| project.0),
            subpath: query.subpath.clone().flatten().map(|subpath| subpath.0),
            anchor: None,
        }
    }

    fn to_store(&self, limit: i64) -> ListInput {
        ListInput {
            directory: self.directory.clone(),
            workspace_id: None,
            project: self.project.clone(),
            subpath: self.subpath.clone(),
            search: self.search.clone(),
            limit: Some(limit),
            order: self.order.map(|order| match order {
                SessionsQueryOrder::Asc => Order::Asc,
                SessionsQueryOrder::Desc => Order::Desc,
            }),
            parent_id: self.parent_id.clone(),
            archived: self.archived,
            anchor: self.anchor.as_ref().map(|anchor| ListAnchor {
                id: anchor.id.clone(),
                time: anchor.time,
                direction: anchor.direction,
            }),
        }
    }
}

fn parse_cursor(raw: &str) -> Result<ListFilter, SessionError> {
    let bytes =
        URL_SAFE_NO_PAD.decode(raw).or_else(|_| URL_SAFE.decode(raw)).map_err(|_| SessionError::InvalidCursor)?;
    let text = String::from_utf8(bytes).map_err(|_| SessionError::InvalidCursor)?;
    let value = serde_json::from_str::<Value>(&text).map_err(|_| SessionError::InvalidCursor)?;
    let object = value.as_object().ok_or(SessionError::InvalidCursor)?;
    let anchor = object.get("anchor").and_then(Value::as_object).ok_or(SessionError::InvalidCursor)?;
    let id = decode_field::<SessionID>(anchor, "id")?.ok_or(SessionError::InvalidCursor)?;
    let time = anchor
        .get("time")
        .and_then(Value::as_f64)
        .filter(|time| time.is_finite())
        .ok_or(SessionError::InvalidCursor)?;
    let direction = match anchor.get("direction").and_then(Value::as_str) {
        Some("previous") => Direction::Previous,
        Some("next") => Direction::Next,
        _ => return Err(SessionError::InvalidCursor),
    };
    let archived = match object.get("archived") {
        None => None,
        Some(Value::String(value)) if value == "true" => Some(true),
        Some(Value::String(value)) if value == "false" => Some(false),
        Some(_) => return Err(SessionError::InvalidCursor),
    };
    let parent_id = match object.get("parentID") {
        None => None,
        Some(Value::String(value)) if value == "null" => Some(None),
        Some(Value::Null) => return Err(SessionError::InvalidCursor),
        Some(value) => {
            let id = SessionID::decode(value, &mut Path::new()).map_err(|_| SessionError::InvalidCursor)?;
            Some(Some(id.0))
        }
    };
    let directory = decode_field::<AbsolutePath>(object, "directory")?.map(|directory| directory.0);
    let project = match &directory {
        Some(_) => None,
        None => decode_field::<ProjectID>(object, "project")?.map(|project| project.0),
    };
    let subpath = match &project {
        Some(_) => decode_field::<RelativePath>(object, "subpath")?.map(|subpath| subpath.0),
        None => None,
    };
    Ok(ListFilter {
        order: decode_field(object, "order")?,
        search: decode_field(object, "search")?,
        parent_id,
        archived,
        directory,
        project,
        subpath,
        anchor: Some(CursorAnchor { id: id.0, time, direction }),
    })
}

fn decode_field<T: Decode>(object: &Map<String, Value>, key: &str) -> Result<Option<T>, SessionError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    T::decode(value, &mut Path::new()).map(Some).map_err(|_| SessionError::InvalidCursor)
}

fn cursor(filter: &ListFilter, id: &str, time: f64, direction: Direction) -> SessionsCursor {
    let mut out = String::from("{");
    let mut first = true;
    if let Some(order) = &filter.order {
        write_value(&mut out, &mut first, "order", order);
    }
    write_optional(&mut out, &mut first, "search", &filter.search);
    match &filter.parent_id {
        Some(None) => write_value(&mut out, &mut first, "parentID", &"null".to_owned()),
        Some(Some(parent_id)) => write_value(&mut out, &mut first, "parentID", parent_id),
        None => {}
    }
    if let Some(archived) = filter.archived {
        write_value(&mut out, &mut first, "archived", &archived.to_string());
    }
    match (&filter.directory, &filter.project) {
        (Some(directory), _) => write_value(&mut out, &mut first, "directory", directory),
        (None, Some(project)) => {
            write_value(&mut out, &mut first, "project", project);
            write_optional(&mut out, &mut first, "subpath", &filter.subpath);
        }
        (None, None) => {}
    }
    if !first {
        out.push(',');
    }
    out.push_str("\"anchor\":");
    out.push_str(&anchor_json(id, time, direction));
    out.push('}');
    SessionsCursor(URL_SAFE_NO_PAD.encode(out))
}

fn anchor_json(id: &str, time: f64, direction: Direction) -> String {
    let mut out = String::from("{");
    let mut first = true;
    write_value(&mut out, &mut first, "id", &id.to_owned());
    write_value(&mut out, &mut first, "time", &time);
    write_value(
        &mut out,
        &mut first,
        "direction",
        &match direction {
            Direction::Previous => "previous".to_owned(),
            Direction::Next => "next".to_owned(),
        },
    );
    out.push('}');
    out
}

fn internal(error: impl std::fmt::Display) -> SessionError {
    SessionError::Internal(error.to_string())
}
