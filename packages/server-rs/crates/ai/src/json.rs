pub fn encode_body(value: &serde_json::Value) -> String {
    opencode_contract::codec::encode_json(value)
}

pub fn canonical(value: &serde_json::Value) -> String {
    encode_body(&sorted(value))
}

pub fn deep_merge(base: &serde_json::Value, overlay: &serde_json::Value) -> serde_json::Value {
    let Some(overlay) = overlay.as_object() else {
        return overlay.clone();
    };
    let Some(base) = base.as_object() else {
        return serde_json::Value::Object(overlay.clone());
    };
    let mut merged = base.clone();
    for (key, value) in overlay {
        let next = merged.get(key).map_or_else(|| value.clone(), |current| deep_merge(current, value));
        merged.insert(key.clone(), next);
    }
    serde_json::Value::Object(merged)
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
