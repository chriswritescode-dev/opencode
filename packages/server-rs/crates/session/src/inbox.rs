use opencode_bus::{BusError, Event};
use opencode_contract::codec::{Encode, encode_json};
use opencode_contract::generated::{
    SessionInboxCancelledData, SessionInboxDeliveredData, SessionInboxDelivery, SessionInboxDeliveryChangedData,
    SessionInboxEnqueuedData, SessionInboxItem,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::blob;
use crate::projector::decode;

pub(crate) enum Delivered {
    User { id: String, payload: Value },
    Synthetic { id: String, payload: Value },
    Removed,
}

pub(crate) fn admitted(connection: &Connection, media_blob: bool, event: &Event) -> Result<(), BusError> {
    let data: SessionInboxEnqueuedData = decode(event)?;
    let seq =
        event.durable().map_or_else(|| Err(invalid(event, "missing durable envelope")), |durable| Ok(durable.seq))?;
    let id = &data.inbox_id.0;
    let exists =
        connection.query_row("SELECT id FROM session_message WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some();
    if exists {
        return Err(conflict(event));
    }
    let (kind, delivery, payload) = match &data.item {
        SessionInboxItem::User(item) => (
            "user",
            item.delivery,
            blob::externalize_payload(connection, media_blob, &as_value(event, &item.payload)?)?,
        ),
        SessionInboxItem::Synthetic(item) => ("synthetic", item.delivery, as_value(event, &item.payload)?),
        SessionInboxItem::Compaction(item) => ("compaction", item.delivery, as_value(event, &item.payload)?),
        SessionInboxItem::Move(item) => ("move", item.delivery, as_value(event, &item.payload)?),
    };
    let inserted = connection.execute(
        "INSERT INTO session_inbox (id, session_id, type, payload, delivery, enqueued_seq, time_created) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
        params![id, data.session_id.0, kind, encode_json(&payload), delivery_name(delivery), seq, event.created()],
    )?;
    if inserted == 0 {
        return Err(conflict(event));
    }
    let owner = blob::Owner { session_id: data.session_id.0, message_id: id.clone(), seq };
    blob::record_refs(connection, &owner, &[payload])
}

pub(crate) fn delivered(connection: &Connection, event: &Event) -> Result<Delivered, BusError> {
    let data: SessionInboxDeliveredData = decode(event)?;
    let row = connection
        .query_row(
            "DELETE FROM session_inbox WHERE id = ?1 AND session_id = ?2 RETURNING type, payload",
            params![data.inbox_id.0, data.session_id.0],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((kind, payload)) = row else {
        return Err(conflict(event));
    };
    if kind == "user" {
        return Ok(Delivered::User { id: data.inbox_id.0, payload: parse(event, &payload)? });
    }
    if kind == "synthetic" {
        return Ok(Delivered::Synthetic { id: data.inbox_id.0, payload: parse(event, &payload)? });
    }
    Ok(Delivered::Removed)
}

pub(crate) fn cancelled(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionInboxCancelledData = decode(event)?;
    let deleted = connection
        .query_row(
            "DELETE FROM session_inbox WHERE id = ?1 AND session_id = ?2 AND delivery IN ('queue', 'steer') \
             RETURNING id",
            params![data.inbox_id.0, data.session_id.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !deleted {
        return Err(conflict(event));
    }
    blob::release_message(connection, &data.session_id.0, &data.inbox_id.0)
}

pub(crate) fn delivery_changed(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionInboxDeliveryChangedData = decode(event)?;
    let from = match data.delivery {
        SessionInboxDelivery::Steer => SessionInboxDelivery::Queue,
        SessionInboxDelivery::Queue => SessionInboxDelivery::Steer,
    };
    let updated = connection.execute(
        "UPDATE session_inbox SET delivery = ?1 WHERE id = ?2 AND session_id = ?3 AND delivery = ?4",
        params![delivery_name(data.delivery), data.inbox_id.0, data.session_id.0, delivery_name(from)],
    )?;
    if updated == 0 {
        return Err(conflict(event));
    }
    Ok(())
}

fn delivery_name(delivery: SessionInboxDelivery) -> &'static str {
    match delivery {
        SessionInboxDelivery::Steer => "steer",
        SessionInboxDelivery::Queue => "queue",
    }
}

fn as_value<T: Encode>(event: &Event, payload: &T) -> Result<Value, BusError> {
    parse(event, &encode_json(payload))
}

fn parse(event: &Event, payload: &str) -> Result<Value, BusError> {
    serde_json::from_str(payload).map_err(|error| invalid(event, error.to_string()))
}

fn conflict(event: &Event) -> BusError {
    invalid(event, "SessionInbox.LifecycleConflict")
}

fn invalid(event: &Event, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: event.r#type().to_string(), message: message.into() }
}
