use serde_json::Value;

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
