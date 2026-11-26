pub use opencode_ai::schema::messages::{Freeform, ToolDefinition};
use opencode_ai::schema::options::CacheHint;
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationError {
    pub name: String,
    pub message: String,
}

impl RegistrationError {
    fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        RegistrationError { name: name.into(), message: message.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOptions {
    pub namespace: Option<String>,
    pub permission: Option<String>,
    pub codemode: Value,
}

impl Default for ToolOptions {
    fn default() -> Self {
        ToolOptions { namespace: None, permission: None, codemode: Value::Bool(true) }
    }
}

impl ToolOptions {
    pub fn codemode_enabled(&self) -> bool {
        self.codemode != Value::Bool(false)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolSpec {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub freeform: Option<Freeform>,
    pub cache: Option<CacheHint>,
    pub native: Option<Map<String, Value>>,
}

pub fn normalized_name(name: &str) -> String {
    name.chars().map(|character| if valid_name_character(character) { character } else { '_' }).collect()
}

pub fn effective_name(namespace: Option<&str>, name: &str) -> String {
    let normalized = normalized_name(name);
    match namespace {
        Some(namespace) => format!("{}_{normalized}", namespace.replace('.', "_")),
        None => normalized,
    }
}

pub fn namespace_error(name: &str) -> Option<RegistrationError> {
    if name.split('.').all(valid_segment) {
        return None;
    }
    Some(RegistrationError::new(name, format!("Invalid tool namespace: {}", json_string(name))))
}

pub fn register(spec: &ToolSpec, options: &ToolOptions) -> Result<(String, ToolDefinition), RegistrationError> {
    if let Some(namespace) = &options.namespace
        && let Some(error) = namespace_error(namespace)
    {
        return Err(error);
    }
    let name = normalized_name(&spec.name);
    if !valid_segment(&name) {
        return Err(RegistrationError::new(&name, format!("Invalid tool name: {name}")));
    }
    let key = effective_name(options.namespace.as_deref(), &spec.name);
    if !options.codemode_enabled() && key == "execute" {
        return Err(RegistrationError::new(&key, "Tool name \"execute\" is reserved for CodeMode"));
    }
    let Some(description) = spec.description.as_ref() else {
        return Err(RegistrationError::new(
            &key,
            format!("Invalid tool definition {key}: description must be a string"),
        ));
    };
    Ok((
        key.clone(),
        ToolDefinition {
            name: key,
            description: description.clone(),
            input_schema: spec.input_schema.clone().unwrap_or_else(|| Value::Object(Map::new())),
            output_schema: spec.output_schema.clone(),
            freeform: spec.freeform.clone(),
            cache: spec.cache.clone(),
            native: spec.native.clone(),
        },
    ))
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(valid_name_character)
}

fn valid_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serialises as JSON")
}
