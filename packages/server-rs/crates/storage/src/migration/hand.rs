use std::collections::HashSet;
use std::path::Path;

use rusqlite::{OptionalExtension, Transaction};

use crate::StorageError;
use crate::generated::migrations::{
    LOOSE_PSYLOCKE_FRESH, LOOSE_PSYLOCKE_PREVIOUS_V2, SESSION_METADATA_ADD, WORKSPACE_NAME_WITH_NAME,
    WORKSPACE_NAME_WITHOUT_NAME,
};
use crate::id;
use crate::migration::MigrationReport;

pub const HAND_PORTED: [&str; 4] = [
    "20260410174513_workspace-name",
    "20260511173437_session-metadata",
    "20260804233008_loose_psylocke",
    "20260805200742_import_legacy_credentials",
];

const PREVIOUS_V2_MARKER: &str = "20260730195856_optional_session_title";
const WELL_KNOWN_SOURCES_KEY: &str = "wellknown:sources";

pub(crate) fn apply(
    id: &str,
    tx: &Transaction<'_>,
    legacy_credentials: Option<&Path>,
    report: &mut MigrationReport,
) -> Result<(), StorageError> {
    match id {
        "20260410174513_workspace-name" => workspace_name(tx),
        "20260511173437_session-metadata" => session_metadata(tx),
        "20260804233008_loose_psylocke" => loose_psylocke(tx),
        "20260805200742_import_legacy_credentials" => match legacy_credentials {
            Some(auth_json) => import_legacy_credentials(tx, auth_json, report),
            None => Ok(()),
        },
        _ => Err(StorageError::Migration(format!("Unknown hand-ported migration: {id}"))),
    }
}

fn workspace_name(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let columns = table_columns(tx, "workspace")?;
    let statements = if columns.iter().any(|column| column == "name") {
        WORKSPACE_NAME_WITH_NAME
    } else {
        WORKSPACE_NAME_WITHOUT_NAME
    };
    for statement in statements {
        tx.execute(statement, [])?;
    }
    Ok(())
}

fn session_metadata(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let columns = table_columns(tx, "session")?;
    if columns.iter().any(|column| column == "metadata") {
        return Ok(());
    }
    for statement in SESSION_METADATA_ADD {
        tx.execute(statement, [])?;
    }
    Ok(())
}

fn loose_psylocke(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let previous_v2 = tx
        .query_row("SELECT id FROM migration WHERE id = ?", [PREVIOUS_V2_MARKER], |row| row.get::<_, String>(0))
        .optional()?;
    if previous_v2.is_none() {
        for statement in LOOSE_PSYLOCKE_FRESH {
            tx.execute(statement, [])?;
        }
        return Ok(());
    }
    let v1_only = tx
        .query_row(
            "SELECT 1 FROM message WHERE NOT EXISTS (SELECT 1 FROM session_message WHERE session_message.session_id = message.session_id) LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if v1_only.is_some() {
        return Err(StorageError::Migration("Previous V2 database contains V1-only session history".into()));
    }
    for statement in LOOSE_PSYLOCKE_PREVIOUS_V2 {
        tx.execute(statement, [])?;
    }
    Ok(())
}

pub(crate) fn import_legacy_credentials(
    tx: &Transaction<'_>,
    auth_json: &Path,
    report: &mut MigrationReport,
) -> Result<(), StorageError> {
    let Ok(bytes) = std::fs::read(auth_json) else {
        return Ok(());
    };
    let Some(serde_json::Value::Object(entries)) =
        serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&bytes)).ok()
    else {
        report
            .warnings
            .push(format!("Skipped legacy credential file that is not a JSON object: {}", auth_json.display()));
        return Ok(());
    };

    let mut origins: Vec<String> = Vec::new();
    let mut keys: Vec<(&String, Option<u32>)> = entries.keys().map(|key| (key, array_index(key))).collect();
    keys.sort_by_key(|(_, index)| index.map_or((1, 0), |index| (0, index)));
    for (key, _) in keys {
        let Some(credential) = decode_legacy(&entries[key.as_str()]) else {
            continue;
        };
        let integration_id = key.trim_end_matches('/');
        if integration_id.is_empty() {
            continue;
        }
        if matches!(credential, Legacy::WellKnown { .. }) {
            origins.push(integration_id.to_owned());
        }
        let existing = tx
            .query_row("SELECT id FROM credential WHERE integration_id = ?", [integration_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        if existing.is_some() {
            continue;
        }
        let label = if matches!(credential, Legacy::OAuth { .. }) { "OAuth" } else { "API key" };
        let value = encode_json(&credential_value(integration_id, credential));
        let now = id::now_ms();
        tx.execute(
            "INSERT INTO credential (id, integration_id, label, value, time_created, time_updated) VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![format!("cred_{}", id::ascending()), integration_id, label, value, now, now],
        )?;
    }

    if origins.is_empty() {
        return Ok(());
    }
    let stored = tx
        .query_row("SELECT value FROM kv WHERE key = ?", [WELL_KNOWN_SOURCES_KEY], |row| row.get::<_, String>(0))
        .optional()?;
    let mut merged: Vec<String> = stored.as_deref().and_then(parse_string_array).unwrap_or_default();
    merged.extend(origins);
    let mut seen = HashSet::new();
    merged.retain(|origin| seen.insert(origin.clone()));
    let value = serde_json::Value::Array(merged.into_iter().map(serde_json::Value::String).collect());
    let now = id::now_ms();
    tx.execute(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?, ?, ?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value, time_updated = excluded.time_updated",
        rusqlite::params![WELL_KNOWN_SOURCES_KEY, encode_json(&value), now, now],
    )?;
    Ok(())
}

fn table_columns(tx: &Transaction<'_>, table: &str) -> Result<Vec<String>, StorageError> {
    let mut statement = tx.prepare(&format!("PRAGMA table_info(`{table}`)"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn array_index(key: &str) -> Option<u32> {
    let value = key.parse::<u32>().ok()?;
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

enum Legacy {
    Api { key: String, metadata: Option<serde_json::Map<String, serde_json::Value>> },
    WellKnown { token: String },
    OAuth { refresh: String, access: String, expires: i64, account_id: Option<String>, enterprise_url: Option<String> },
}

fn decode_legacy(value: &serde_json::Value) -> Option<Legacy> {
    let serde_json::Value::Object(object) = value else {
        return None;
    };
    match object.get("type").and_then(serde_json::Value::as_str)? {
        "api" => {
            let key = object.get("key").and_then(serde_json::Value::as_str)?.to_owned();
            let metadata = match object.get("metadata") {
                None => None,
                Some(serde_json::Value::Object(metadata)) if metadata.values().all(serde_json::Value::is_string) => {
                    Some(metadata.clone())
                }
                Some(_) => return None,
            };
            Some(Legacy::Api { key, metadata })
        }
        "wellknown" => {
            object.get("key").and_then(serde_json::Value::as_str)?;
            let token = object.get("token").and_then(serde_json::Value::as_str)?.to_owned();
            Some(Legacy::WellKnown { token })
        }
        "oauth" => Some(Legacy::OAuth {
            refresh: object.get("refresh").and_then(serde_json::Value::as_str)?.to_owned(),
            access: object.get("access").and_then(serde_json::Value::as_str)?.to_owned(),
            expires: expires(object.get("expires")?)?,
            account_id: optional_string(object, "accountId")?,
            enterprise_url: optional_string(object, "enterpriseUrl")?,
        }),
        _ => None,
    }
}

fn optional_string(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn expires(value: &serde_json::Value) -> Option<i64> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&number)).then_some(number as i64)
}

fn credential_value(integration_id: &str, credential: Legacy) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    match credential {
        Legacy::Api { key, metadata } => {
            object.insert("type".to_owned(), serde_json::Value::String("key".to_owned()));
            object.insert("key".to_owned(), serde_json::Value::String(key));
            if let Some(metadata) = metadata {
                object.insert("metadata".to_owned(), serde_json::Value::Object(metadata));
            }
        }
        Legacy::WellKnown { token } => {
            object.insert("type".to_owned(), serde_json::Value::String("key".to_owned()));
            object.insert("key".to_owned(), serde_json::Value::String(token));
        }
        Legacy::OAuth { refresh, access, expires, account_id, enterprise_url } => {
            object.insert("type".to_owned(), serde_json::Value::String("oauth".to_owned()));
            object.insert("methodID".to_owned(), serde_json::Value::String(method_id(integration_id).to_owned()));
            object.insert("refresh".to_owned(), serde_json::Value::String(refresh));
            object.insert("access".to_owned(), serde_json::Value::String(access));
            object.insert("expires".to_owned(), serde_json::Value::Number(serde_json::Number::from(expires)));
            let mut metadata = serde_json::Map::new();
            if let Some(account_id) = account_id.filter(|value| !value.is_empty()) {
                metadata.insert("accountID".to_owned(), serde_json::Value::String(account_id));
            }
            if let Some(enterprise_url) = enterprise_url.filter(|value| !value.is_empty()) {
                metadata.insert("enterpriseUrl".to_owned(), serde_json::Value::String(enterprise_url));
            }
            if !metadata.is_empty() {
                object.insert("metadata".to_owned(), serde_json::Value::Object(metadata));
            }
        }
    }
    serde_json::Value::Object(object)
}

fn method_id(integration_id: &str) -> &'static str {
    match integration_id {
        "openai" => "chatgpt-browser",
        "github-copilot" | "opencode" | "xai" => "device",
        _ => "oauth",
    }
}

fn parse_string_array(text: &str) -> Option<Vec<String>> {
    let serde_json::Value::Array(items) = serde_json::from_str::<serde_json::Value>(text).ok()? else {
        return None;
    };
    Some(
        items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(item) => Some(item),
                _ => None,
            })
            .collect(),
    )
}

fn encode_json(value: &serde_json::Value) -> String {
    opencode_contract::codec::encode_json(value)
}
