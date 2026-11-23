use serde_json::{Map, Value};

use crate::prepare::gemini_json_schema::{SCHEMA_MAPS, VALUES, map_values, normalize};
use crate::route::model::Model;
use crate::schema::{SanitizerKind, ToolEntry};

pub fn tools(entries: Vec<ToolEntry>, model: &Model) -> Vec<ToolEntry> {
    entries
        .into_iter()
        .map(|entry| match entry {
            ToolEntry::Tool(mut definition) => {
                definition.input_schema = model_compatibility(&definition.input_schema, model);
                ToolEntry::Tool(definition)
            }
            ToolEntry::Namespace(mut namespace) => {
                namespace.tools = tools(namespace.tools, model);
                ToolEntry::Namespace(namespace)
            }
        })
        .collect()
}

pub fn gemini(schema: &Value) -> Value {
    normalize(schema)
}

pub fn moonshot(schema: &Value) -> Value {
    let projected = moonshot_node(schema);
    if projected.is_object() {
        return projected;
    }
    Value::Object(Map::new())
}

pub fn model_compatibility(schema: &Value, model: &Model) -> Value {
    let root = object_root(schema);
    let sanitizer = model
        .compatibility_sanitizer()
        .or_else(|| model.route.protocol.sanitizer())
        .or_else(|| model_name_sanitizer(&model.id));
    match sanitizer {
        Some(SanitizerKind::Gemini) => gemini(&root),
        Some(SanitizerKind::Moonshot) => moonshot(&root),
        Some(SanitizerKind::None) | None => root,
    }
}

fn model_name_sanitizer(id: &str) -> Option<SanitizerKind> {
    let id = id.to_ascii_lowercase();
    if id.contains("gemini") {
        return Some(SanitizerKind::Gemini);
    }
    if id.contains("kimi") {
        return Some(SanitizerKind::Moonshot);
    }
    None
}

fn object_root(schema: &Value) -> Value {
    let Some(entries) = schema.as_object() else {
        let mut output = Map::new();
        output.insert("type".to_string(), Value::String("object".to_string()));
        return Value::Object(output);
    };
    let is_null_only = |not: &Value| {
        not.as_object().is_some_and(|not| not.len() == 1 && not.get("type").and_then(Value::as_str) == Some("null"))
    };
    let object_type = match entries.get("type") {
        None => true,
        Some(Value::String(kind)) => kind == "object",
        Some(_) => false,
    };
    if object_type && entries.get("not").is_some_and(is_null_only) {
        let mut output = Map::new();
        output.insert("type".to_string(), Value::String("object".to_string()));
        for (key, value) in entries {
            if key != "not" {
                output.insert(key.clone(), value.clone());
            }
        }
        return Value::Object(output);
    }
    if entries.contains_key("type") {
        return schema.clone();
    }
    let has_object_branch = entries.get("anyOf").and_then(Value::as_array).is_some_and(|branches| {
        branches.iter().any(|branch| {
            branch
                .as_object()
                .is_some_and(|branch| branch.len() == 1 && branch.get("type").and_then(Value::as_str) == Some("object"))
        })
    });
    let mut output = Map::new();
    output.insert("type".to_string(), Value::String("object".to_string()));
    for (key, value) in entries {
        if has_object_branch && key == "anyOf" {
            continue;
        }
        output.insert(key.clone(), value.clone());
    }
    Value::Object(output)
}

fn tuple_items_schema(items: &[Value]) -> Value {
    let mut projected = items.iter().map(moonshot_node);
    let Some(first) = projected.next() else {
        return Value::Object(Map::new());
    };
    let rest = projected.collect::<Vec<_>>();
    if rest.is_empty() {
        return first;
    }
    let mut values = vec![first];
    values.extend(rest);
    let mut output = Map::new();
    output.insert("anyOf".to_string(), Value::Array(values));
    Value::Object(output)
}

fn enum_type(values: &[Value]) -> Option<Value> {
    let mut types: Vec<&str> = Vec::new();
    for value in values {
        let name = match value {
            Value::Null => "null",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
        };
        if !types.contains(&name) {
            types.push(name);
        }
    }
    match types.as_slice() {
        [only] => Some(Value::String((*only).to_string())),
        [first, second] if *first == "null" || *second == "null" => {
            let other = if *first == "null" { *second } else { *first };
            Some(Value::Array(vec![Value::String(other.to_string()), Value::String("null".to_string())]))
        }
        _ => None,
    }
}

fn moonshot_node(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(moonshot_node).collect()),
        Value::Object(entries) => {
            if let Some(reference) = entries.get("$ref").and_then(Value::as_str) {
                let mut output = Map::new();
                output.insert("$ref".to_string(), Value::String(reference.to_string()));
                return Value::Object(output);
            }
            let derived = if entries.contains_key("type") {
                None
            } else {
                entries.get("enum").and_then(Value::as_array).and_then(|values| enum_type(values))
            };
            let mut output = Map::new();
            if let Some(derived) = derived {
                output.insert("type".to_string(), derived);
            }
            for (key, value) in entries {
                if VALUES.contains(&key.as_str()) {
                    output.insert(key.clone(), value.clone());
                    continue;
                }
                if SCHEMA_MAPS.contains(&key.as_str()) && value.is_object() {
                    output.insert(
                        key.clone(),
                        map_values(value.as_object().expect("object"), &|value, _| moonshot_node(value)),
                    );
                    continue;
                }
                if key == "items" {
                    if entries.get("prefixItems").is_some_and(Value::is_array) {
                        continue;
                    }
                    if value.is_array() {
                        output.insert(key.clone(), tuple_items_schema(value.as_array().expect("array")));
                        continue;
                    }
                }
                if key == "prefixItems" {
                    let Some(value) = value.as_array() else {
                        if !entries.contains_key("items") {
                            output.insert("items".to_string(), Value::Object(Map::new()));
                        }
                        continue;
                    };
                    let mut items = value.clone();
                    if let Some(rest) = entries.get("items") {
                        if rest.is_object() {
                            items.push(rest.clone());
                        } else if let Some(rest) = rest.as_array() {
                            items.extend(rest.iter().cloned());
                        }
                    }
                    output.insert("items".to_string(), tuple_items_schema(&items));
                    continue;
                }
                if key == "unevaluatedItems" {
                    continue;
                }
                output.insert(key.clone(), moonshot_node(value));
            }
            Value::Object(output)
        }
        schema => schema.clone(),
    }
}
