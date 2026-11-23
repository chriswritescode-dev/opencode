use std::path::Path;

use opencode_config::entry::{Entry, Origin};
use opencode_contract::codec::{Encode, encode_json};
use serde_json::{Map, Value, json};

pub struct Case {
    pub name: String,
    pub value: Value,
}

pub fn area(name: &str) -> Vec<Case> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("corpus").join(format!("{name}.json"));
    let input =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let value: Value =
        serde_json::from_str(&input).unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    value
        .as_object()
        .unwrap_or_else(|| panic!("{} is not a JSON object", path.display()))
        .iter()
        .map(|(name, value)| Case { name: name.clone(), value: value.clone() })
        .collect()
}

pub fn entries_view(root: &Path, entries: &[Entry]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            let mut object = wire_object(&entry.to_wire());
            if let Some(path) = object.get("path").and_then(Value::as_str) {
                object.insert("path".to_owned(), Value::String(relative(root, Path::new(path))));
            }
            if let Entry::Document(document) = entry {
                object.insert("origin".to_owned(), origin_view(&document.origin));
            }
            Value::Object(object)
        })
        .collect()
}

pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is outside {}", path.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn origin_view(origin: &Origin) -> Value {
    match origin {
        Origin::File => json!({ "kind": "file" }),
        Origin::Managed => json!({ "kind": "managed" }),
        Origin::WellKnown { integration_id } => {
            json!({ "kind": "wellknown", "integrationID": integration_id })
        }
        Origin::Integration { integration_id, connection, organization } => {
            let mut value = json!({
                "kind": "integration",
                "integrationID": integration_id,
                "connection": connection,
            });
            if let Some(organization) = organization {
                value["organization"] = json!(organization);
            }
            value
        }
    }
}

fn wire_object<T: Encode>(value: &T) -> Map<String, Value> {
    serde_json::from_str::<Value>(&encode_json(value))
        .expect("the wire encoding is JSON")
        .as_object()
        .cloned()
        .expect("the wire encoding is a JSON object")
}
