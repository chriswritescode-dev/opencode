use std::fmt::Write as _;

use serde_json::Value;

use crate::context::ToolError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: Vec<Segment>,
    pub message: String,
}

pub fn decode(tool: &str, schema: &Value, value: &Value) -> Result<Value, ToolError> {
    let mut issues = Vec::new();
    validate(tool, schema, value, &mut Vec::new(), &mut issues);
    if issues.is_empty() {
        return Ok(normalize(schema, value));
    }
    Err(ToolError::new(format_message(tool, &issues, value)))
}

fn normalize(schema: &Value, value: &Value) -> Value {
    let Some(schema) = schema.as_object() else { return value.clone() };
    let Some(kind) = schema.get("type").and_then(Value::as_str) else {
        return value.clone();
    };
    match kind {
        "object" => {
            let Some(input) = value.as_object() else { return value.clone() };
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return value.clone();
            };
            Value::Object(
                properties
                    .iter()
                    .filter_map(|(key, property)| input.get(key).map(|item| (key.clone(), normalize(property, item))))
                    .collect(),
            )
        }
        "array" => {
            let Some(input) = value.as_array() else { return value.clone() };
            let Some(items) = schema.get("items") else { return value.clone() };
            Value::Array(input.iter().map(|item| normalize(items, item)).collect())
        }
        _ => value.clone(),
    }
}

fn validate(tool: &str, schema: &Value, value: &Value, path: &mut Vec<Segment>, issues: &mut Vec<Issue>) {
    let Some(schema) = schema.as_object() else { return };
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        for branch in branches {
            let mut branch_issues = Vec::new();
            validate(tool, branch, value, path, &mut branch_issues);
            if branch_issues.is_empty() {
                return;
            }
        }
        issues.push(Issue { path: path.clone(), message: "Expected a value that matches a schema".to_owned() });
        return;
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => validate_object(tool, schema, value, path, issues),
        Some("array") => validate_array(tool, schema, value, path, issues),
        Some("string") => validate_string(tool, schema, value, path, issues),
        Some("integer") => validate_integer(tool, schema, value, path, issues),
        Some("number") => validate_number(tool, schema, value, path, issues),
        Some("boolean") => {
            if !value.is_boolean() {
                push_type_issue(tool, path, issues, "boolean");
            }
        }
        Some("null") => {
            if !value.is_null() {
                push_type_issue(tool, path, issues, "null");
            }
        }
        None if schema.contains_key("properties") => validate_object(tool, schema, value, path, issues),
        _ => {}
    }
}

fn validate_object(
    tool: &str,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &mut Vec<Segment>,
    issues: &mut Vec<Issue>,
) {
    let Some(object) = value.as_object() else {
        push_type_issue(tool, path, issues, "object");
        return;
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    let required = schema.get("required").and_then(Value::as_array);
    let is_required = |key: &str| required.is_some_and(|keys| keys.iter().any(|item| item.as_str() == Some(key)));
    if let Some(properties) = properties {
        for (key, property) in properties {
            path.push(Segment::Key(key.clone()));
            match object.get(key) {
                Some(item) => validate(tool, property, item, path, issues),
                None if is_required(key) => {
                    issues.push(Issue { path: path.clone(), message: "Missing key".to_owned() })
                }
                None => {}
            }
            path.pop();
        }
        if let Some(required) = required {
            for key in required.iter().filter_map(Value::as_str) {
                if !properties.contains_key(key) && !object.contains_key(key) {
                    path.push(Segment::Key(key.to_owned()));
                    issues.push(Issue { path: path.clone(), message: "Missing key".to_owned() });
                    path.pop();
                }
            }
        }
    }
}

fn validate_array(
    tool: &str,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &mut Vec<Segment>,
    issues: &mut Vec<Issue>,
) {
    let Some(items) = value.as_array() else {
        push_type_issue(tool, path, issues, "array");
        return;
    };
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            path.push(Segment::Index(index));
            validate(tool, item_schema, item, path, issues);
            path.pop();
        }
    }
}

fn validate_string(
    tool: &str,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &[Segment],
    issues: &mut Vec<Issue>,
) {
    let Some(text) = value.as_str() else {
        push_type_issue(tool, path, issues, "string");
        return;
    };
    if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
        && text.chars().count() < minimum as usize
    {
        issues.push(Issue { path: path.to_vec(), message: min_length_message(tool, path, minimum) });
    }
}

fn validate_integer(
    tool: &str,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &[Segment],
    issues: &mut Vec<Issue>,
) {
    let Some(number) = value.as_f64() else {
        push_type_issue(tool, path, issues, "number");
        return;
    };
    if !number.is_finite() || number.fract() != 0.0 || number.abs() > 9_007_199_254_740_991.0 {
        issues.push(Issue { path: path.to_vec(), message: "Expected an integer".to_owned() });
        return;
    }
    validate_range(schema, number, path, issues);
}

fn validate_number(
    tool: &str,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &[Segment],
    issues: &mut Vec<Issue>,
) {
    let Some(number) = value.as_f64() else {
        push_type_issue(tool, path, issues, "number");
        return;
    };
    validate_range(schema, number, path, issues);
}

fn validate_range(schema: &serde_json::Map<String, Value>, number: f64, path: &[Segment], issues: &mut Vec<Issue>) {
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && number < minimum
    {
        issues.push(Issue {
            path: path.to_vec(),
            message: format!("Expected a value greater than or equal to {}", number_text(minimum)),
        });
        return;
    }
    if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
        && number <= minimum
    {
        issues.push(Issue {
            path: path.to_vec(),
            message: format!("Expected a value greater than {}", number_text(minimum)),
        });
        return;
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
        && number > maximum
    {
        issues.push(Issue {
            path: path.to_vec(),
            message: format!("Expected a value less than or equal to {}", number_text(maximum)),
        });
        return;
    }
    if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
        && number >= maximum
    {
        issues.push(Issue {
            path: path.to_vec(),
            message: format!("Expected a value less than {}", number_text(maximum)),
        });
    }
}

fn push_type_issue(tool: &str, path: &[Segment], issues: &mut Vec<Issue>, expected: &str) {
    issues.push(Issue { path: path.to_vec(), message: format!("Expected {}", type_name(tool, path, expected)) });
}

fn type_name<'a>(tool: &str, path: &[Segment], expected: &'a str) -> &'a str {
    match (tool, path) {
        ("glob" | "grep", [Segment::Key(key)]) if key == "path" => "RelativePath",
        ("skill", [Segment::Key(key)]) if key == "id" => "Skill.ID",
        _ => expected,
    }
}

fn min_length_message(tool: &str, path: &[Segment], minimum: u64) -> String {
    match (tool, path) {
        ("grep", [Segment::Key(key)]) if key == "pattern" && minimum == 1 => "Pattern must not be empty".to_owned(),
        _ => format!("Expected a value with a length of at least {minimum}"),
    }
}

fn number_text(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e21 { format!("{number:.0}") } else { number.to_string() }
}

fn format_message(tool: &str, issues: &[Issue], value: &Value) -> String {
    let mut message = format!("Invalid arguments for tool \"{tool}\":");
    for issue in issues.iter().take(5) {
        let _ = write!(message, "\n- {}: {}", format_path(&issue.path), issue.message);
    }
    if issues.len() > 5 {
        let remaining = issues.len() - 5;
        let _ = write!(message, "\n- ...and {remaining} more {}", if remaining == 1 { "issue" } else { "issues" });
    }
    let arguments = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let _ = write!(message, "\n\nArguments provided:\n{arguments}\n\nUpdate the arguments and call the tool again.");
    message
}

fn format_path(path: &[Segment]) -> String {
    let mut rendered = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(key);
            }
            Segment::Index(index) => {
                let _ = write!(rendered, "[{index}]");
            }
        }
    }
    if rendered.is_empty() { "root".to_owned() } else { rendered }
}
