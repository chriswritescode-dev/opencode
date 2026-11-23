use opencode_api_types::js::parse_number;
use serde_json::{Map, Value};

pub const SCHEMA_MAPS: [&str; 6] =
    ["properties", "patternProperties", "$defs", "definitions", "dependentSchemas", "dependencies"];
pub const VALUES: [&str; 5] = ["const", "default", "enum", "examples", "dependentRequired"];

pub fn map_values(record: &Map<String, Value>, map: &dyn Fn(&Value, &str) -> Value) -> Value {
    let mut output = Map::new();
    for (key, value) in record {
        output.insert(key.clone(), map(value, key));
    }
    Value::Object(output)
}

pub fn normalize(schema: &Value) -> Value {
    let normalized = normalize_node(schema);
    let Some(entries) = normalized.as_object() else {
        return Value::Object(Map::new());
    };
    let prepared = map_values(entries, &|value, key| {
        if (key == "$defs" || key == "definitions") && value.is_object() {
            let prefix = format!("#/{key}/");
            return map_values(value.as_object().expect("object"), &|definition, name| {
                cut_loops(definition, &format!("{prefix}{name}"), false)
            });
        }
        value.clone()
    });
    let cut = cut_loops(&prepared, "#", false);
    if cut.is_object() {
        return cut;
    }
    Value::Object(Map::new())
}

fn normalize_node(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(normalize_node).collect()),
        Value::Object(entries) => {
            let properties = entries.get("properties").and_then(Value::as_object);
            let mut output = Map::new();
            for (key, value) in entries {
                match key.as_str() {
                    "const" | "default" | "enum" | "examples" | "dependentRequired" => {
                        output.insert(key.clone(), value.clone());
                    }
                    "required" if properties.is_some() && value.is_array() => {
                        let properties = properties.expect("properties");
                        let declared = value
                            .as_array()
                            .expect("array")
                            .iter()
                            .filter(|name| name.as_str().is_some_and(|name| properties.contains_key(name)))
                            .cloned()
                            .collect();
                        output.insert(key.clone(), Value::Array(declared));
                    }
                    "exclusiveMinimum" if value.is_boolean() => {
                        if value.as_bool() == Some(true)
                            && let Some(minimum) = entries.get("minimum")
                            && minimum.is_number()
                        {
                            output.insert(key.clone(), minimum.clone());
                        }
                    }
                    "exclusiveMaximum" if value.is_boolean() => {
                        if value.as_bool() == Some(true)
                            && let Some(maximum) = entries.get("maximum")
                            && maximum.is_number()
                        {
                            output.insert(key.clone(), maximum.clone());
                        }
                    }
                    "minimum" if entries.get("exclusiveMinimum").and_then(Value::as_bool) == Some(true) => {}
                    "maximum" if entries.get("exclusiveMaximum").and_then(Value::as_bool) == Some(true) => {}
                    "items" if value.is_array() => {
                        output.insert(
                            "prefixItems".to_string(),
                            Value::Array(value.as_array().expect("array").iter().map(normalize_node).collect()),
                        );
                    }
                    "additionalItems" if entries.get("items").is_some_and(Value::is_array) => {
                        output.insert("items".to_string(), normalize_node(value));
                    }
                    _ if SCHEMA_MAPS.contains(&key.as_str()) && value.is_object() => {
                        output.insert(
                            key.clone(),
                            map_values(value.as_object().expect("object"), &|value, _| normalize_node(value)),
                        );
                    }
                    _ => {
                        output.insert(key.clone(), normalize_node(value));
                    }
                }
            }
            Value::Object(output)
        }
        schema => schema.clone(),
    }
}

fn cut_loops(schema: &Value, target: &str, safe: bool) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(|item| cut_loops(item, target, safe)).collect()),
        Value::Object(entries) => {
            if entries.get("$ref").and_then(Value::as_str) == Some(target) && !safe {
                return Value::Object(Map::new());
            }
            let required = entries.get("required").and_then(Value::as_array);
            map_values(entries, &|value, key| {
                if VALUES.contains(&key) {
                    return value.clone();
                }
                if key == "items" {
                    let has_items = entries.get("minItems").is_some_and(|min| js_number(min) > 0.0);
                    return cut_loops(value, target, safe || !has_items);
                }
                if key == "properties" && value.is_object() {
                    return map_values(value.as_object().expect("object"), &|child, name| {
                        let required =
                            required.is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)));
                        cut_loops(child, target, safe || !required)
                    });
                }
                if SCHEMA_MAPS.contains(&key) && value.is_object() {
                    return map_values(value.as_object().expect("object"), &|child, _| cut_loops(child, target, safe));
                }
                cut_loops(value, target, safe)
            })
        }
        schema => schema.clone(),
    }
}

fn js_number(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => parse_number(text).unwrap_or(f64::NAN),
        Value::Bool(true) => 1.0,
        Value::Bool(false) | Value::Null => 0.0,
        _ => f64::NAN,
    }
}
