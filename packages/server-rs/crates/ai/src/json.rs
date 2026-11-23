use serde_json::{Map, Value};

pub fn encode_body(value: &serde_json::Value) -> String {
    opencode_contract::codec::encode_json(value)
}

pub fn canonical(value: &serde_json::Value) -> String {
    encode_body(&sorted(value))
}

pub fn deep_merge(base: &Value, overlay: &Value) -> Option<Value> {
    match (base.as_object(), overlay.as_object()) {
        (Some(base), Some(overlay)) => {
            (!base.is_empty() || !overlay.is_empty()).then(|| Value::Object(merge_objects(base, overlay)))
        }
        _ => Some(overlay.clone()),
    }
}

pub fn merge_objects(base: &Map<String, Value>, overlay: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        let next = merged.get(key).map_or_else(|| Some(value.clone()), |current| deep_merge(current, value));
        match next {
            Some(next) => {
                merged.insert(key.clone(), next);
            }
            None => {
                merged.remove(key);
            }
        }
    }
    merged
}

pub fn merge_records<'a>(items: impl IntoIterator<Item = &'a Map<String, Value>>) -> Option<Map<String, Value>> {
    let mut items = items.into_iter();
    let first = items.next()?;
    let mut merged = first.clone();
    let mut merged_any = false;
    let mut any_keys = !first.is_empty();
    for item in items {
        merged = merge_objects(&merged, item);
        merged_any = true;
        any_keys |= !item.is_empty();
    }
    (!merged_any || any_keys).then_some(merged)
}

fn sorted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(sorted).collect()),
        serde_json::Value::Object(entries) => {
            let mut keys: Vec<&String> = entries.keys().collect();
            keys.sort();
            let ordered = keys.into_iter().map(|key| (key.clone(), sorted(&entries[key])));
            serde_json::Value::Object(ordered.collect())
        }
        value => value.clone(),
    }
}
