use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    PromptAgentAttachment, SessionID, SessionInboxDelivery, SessionInboxEnqueuedData, SessionInboxInfo,
    SessionInboxItem, SessionInboxItemUser, SessionInboxItemUserType, SessionInboxUser, SessionInboxUserPayload,
    SessionInboxUserPayload1, SessionInboxUserTime, SessionInboxUserType, SessionMessageID, SessionMessageInfo,
};
use serde_json::Value;

use crate::session::SessionError;
use crate::store::SessionStore;

pub struct PromptInput {
    pub id: Option<String>,
    pub text: String,
    pub agents: Option<Vec<PromptAgentAttachment>>,
    pub metadata: Option<JsonMap<Value>>,
    pub delivery: SessionInboxDelivery,
    pub resume: bool,
}

pub(crate) fn existing(
    store: &SessionStore,
    session_id: &str,
    message_id: &str,
    delivery: SessionInboxDelivery,
) -> Result<Option<SessionInboxUser>, SessionError> {
    if let Some(info) = store.inbox_item(message_id).map_err(internal)? {
        return match info {
            SessionInboxInfo::User(item) if item.session_id.0 == session_id => Ok(Some(item)),
            _ => Err(conflict(message_id)),
        };
    }
    let Some((stored_session, message)) = store.message(message_id).map_err(internal)? else {
        return Ok(None);
    };
    let SessionMessageInfo::User(message) = message else {
        return Err(conflict(message_id));
    };
    if stored_session != session_id {
        return Err(conflict(message_id));
    }
    Ok(Some(SessionInboxUser {
        id: SessionMessageID(message.id.0),
        session_id: SessionID(session_id.to_owned()),
        time: SessionInboxUserTime { created: message.time.created },
        r#type: SessionInboxUserType::User,
        payload: SessionInboxUserPayload1 {
            text: message.text,
            files: message.files,
            agents: message.agents,
            skills: message.skills,
            metadata: message.metadata,
        },
        delivery,
    }))
}

pub(crate) fn enqueued(message_id: &str, session_id: &str, input: &PromptInput) -> SessionInboxEnqueuedData {
    SessionInboxEnqueuedData {
        session_id: SessionID(session_id.to_owned()),
        inbox_id: SessionMessageID(message_id.to_owned()),
        item: SessionInboxItem::User(SessionInboxItemUser {
            r#type: SessionInboxItemUserType::User,
            payload: SessionInboxUserPayload {
                text: input.text.clone(),
                files: None,
                agents: input.agents.clone(),
                skills: None,
                metadata: input.metadata.clone(),
            },
            delivery: input.delivery,
        }),
    }
}

pub(crate) fn admitted(message_id: &str, session_id: &str, input: &PromptInput, created: f64) -> SessionInboxUser {
    SessionInboxUser {
        id: SessionMessageID(message_id.to_owned()),
        session_id: SessionID(session_id.to_owned()),
        time: SessionInboxUserTime { created },
        r#type: SessionInboxUserType::User,
        payload: SessionInboxUserPayload1 {
            text: input.text.clone(),
            files: None,
            agents: input.agents.clone(),
            skills: None,
            metadata: input.metadata.clone(),
        },
        delivery: input.delivery,
    }
}

fn conflict(message_id: &str) -> SessionError {
    SessionError::Conflict { message_id: message_id.to_owned() }
}

fn internal(error: impl std::fmt::Display) -> SessionError {
    SessionError::Internal(error.to_string())
}
