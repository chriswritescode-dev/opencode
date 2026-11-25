use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::prepare::tool_updates::{flatten_tools, tools_update};
use crate::protocols::utils::tool_stream;
use crate::route::model::Model;
use crate::schema::{Freeform, LlmEvent, LlmRequest, ProviderMetadata, ToolDefinition, ToolEntry, freeform_field};

const STREAM_END: &str = "\"}";
const STRING_RULE: &str = r#"/"(?:[^"\\\x00-\x1f]|\\["\\\/bfnrt]|\\u[0-9a-fA-F]{4})*"/"#;

pub const PREFIX: &str = "// @options:";

#[derive(Debug, Clone, PartialEq)]
pub struct Custom {
    pub field: String,
    pub format: Freeform,
    pub options: Option<Options>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub fields: Vec<OptionField>,
    pub description: String,
    pub grammar: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionField {
    pub name: String,
    pub schema: Value,
    pub required: bool,
}

pub fn resolve(tool: &ToolDefinition, formats: Option<&[String]>) -> Option<Custom> {
    let field = freeform_field(&tool.input_schema, tool.freeform.as_ref())?;
    let format = tool.freeform.clone()?;
    if tool.native.is_some() {
        return None;
    }
    if let Some(formats) = formats
        && !formats.iter().any(|name| name == format_type(&format))
    {
        return None;
    }
    let lark = formats.is_none_or(|formats| formats.iter().any(|name| name == "lark"));
    let required = tool
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).collect::<Vec<&str>>())
        .unwrap_or_default();
    let fields = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .filter(|(name, _)| name.as_str() != field)
                .map(|(name, schema)| OptionField {
                    name: name.clone(),
                    schema: if schema.is_object() { schema.clone() } else { Value::Object(Map::new()) },
                    required: required.contains(&name.as_str()),
                })
                .collect::<Vec<OptionField>>()
        })
        .unwrap_or_default();
    if fields.is_empty() {
        return Some(Custom { field, format, options: None });
    }
    let rules = fields.iter().map(|item| value_rule(&item.schema)).collect::<Option<Vec<String>>>()?;
    if !lark && format_type(&format) != "text" {
        return None;
    }
    let description = describe(&field, &fields);
    let grammar = lark.then(|| line_grammar(&fields, &rules, &format));
    Some(Custom { field, format, options: Some(Options { fields, description, grammar }) })
}

fn line_grammar(fields: &[OptionField], rules: &[String], format: &Freeform) -> String {
    let text = matches!(format, Freeform::Text { .. });
    let body = if text { "ff_body" } else { "fb_start" };
    let plain = if text { "ff_plain" } else { body };
    let optional = if fields.iter().any(|item| item.required) { String::new() } else { format!(" | {plain}") };
    let mut lines = vec![
        format!("start: ff_line {body}{optional}"),
        format!(
            "ff_line: {} \" \"* \"{{\" \" \"* ff_pair (\" \"* \",\" \" \"* ff_pair)* \" \"* \"}}\" \"\\n\"",
            lark_string(PREFIX)
        ),
        format!(
            "ff_pair: {}",
            fields
                .iter()
                .zip(rules)
                .map(|(item, rule)| {
                    format!(
                        "{} \" \"* \":\" \" \"* {rule}",
                        lark_string(&crate::json::encode_body(&Value::String(item.name.clone())))
                    )
                })
                .collect::<Vec<String>>()
                .join(" | ")
        ),
    ];
    match format {
        Freeform::Text { .. } => {
            lines.push("ff_body: /[\\s\\S]+/".to_string());
            lines.push(format!("ff_plain: /{}/", not_starting_with(PREFIX)));
        }
        Freeform::Regex { pattern, .. } => lines.push(format!("fb_start: /{}/", escape_slashes(pattern))),
        Freeform::Lark { grammar, .. } => lines.push(rename_lark_start(grammar)),
    }
    lines.join("\n")
}

fn value_rule(schema: &Value) -> Option<String> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if values.is_empty() {
            return None;
        }
        let members = values.iter().map(|value| lark_string(&crate::json::encode_body(value))).collect::<Vec<String>>();
        return Some(format!("({})", members.join(" | ")));
    }
    match schema.get("type").and_then(Value::as_str)? {
        "boolean" => Some("(\"true\" | \"false\")".to_string()),
        "number" => Some("/-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/".to_string()),
        "string" => Some(STRING_RULE.to_string()),
        "integer" => {
            let lowest = minimum(schema);
            if lowest >= 1.0 {
                return Some("/[1-9][0-9]*/".to_string());
            }
            if lowest >= 0.0 { Some("/0|[1-9][0-9]*/".to_string()) } else { Some("/-?(?:0|[1-9][0-9]*)/".to_string()) }
        }
        _ => None,
    }
}

fn minimum(schema: &Value) -> f64 {
    let lowest = schema.get("minimum").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY);
    let exclusive =
        schema.get("exclusiveMinimum").and_then(Value::as_f64).map_or(f64::NEG_INFINITY, |value| value + 1.0);
    lowest.max(exclusive)
}

fn describe(field: &str, fields: &[OptionField]) -> String {
    let required = fields.iter().filter(|item| item.required).collect::<Vec<&OptionField>>();
    let example = if required.is_empty() {
        vec![
            fields
                .iter()
                .find(|item| item.schema.get("type").and_then(Value::as_str) == Some("boolean"))
                .unwrap_or(&fields[0]),
        ]
    } else {
        required.clone()
    };
    let mut lines = vec![if required.is_empty() {
        format!(
            "To set the parameters below, start with a `{PREFIX}` line holding a JSON object of them, then write `{field}` from the next line. Leave the line out to use the defaults."
        )
    } else {
        format!(
            "Start with a `{PREFIX}` line holding a JSON object of the parameters below, then write `{field}` from the next line."
        )
    }];
    lines.extend(fields.iter().map(|item| {
        let kind = match item.schema.get("enum").and_then(Value::as_array) {
            Some(values) => {
                format!("one of {}", values.iter().map(crate::json::encode_body).collect::<Vec<String>>().join(", "))
            }
            None => match item.schema.get("type") {
                Some(value) => opencode_api_types::js::js_string(value),
                None => "undefined".to_string(),
            },
        };
        let requirement = if item.required { ", required" } else { "" };
        let description = match item.schema.get("description").and_then(Value::as_str) {
            Some(description) => format!(": {description}"),
            None => String::new(),
        };
        format!("- `{}` ({kind}{requirement}){description}", item.name)
    }));
    lines.push("For example:".to_string());
    lines.push(format!(
        "{PREFIX} {{{}}}",
        example
            .iter()
            .map(|item| format!(
                "{}: {}",
                crate::json::encode_body(&Value::String(item.name.clone())),
                crate::json::encode_body(&sample(&item.schema))
            ))
            .collect::<Vec<String>>()
            .join(", ")
    ));
    lines.push(format!("<{field}>"));
    lines.join("\n")
}

fn sample(schema: &Value) -> Value {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.first().cloned().unwrap_or(Value::Null);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("boolean") => Value::Bool(true),
        Some("string") => Value::String("...".to_string()),
        _ => Value::from(minimum(schema).max(1.0)),
    }
}

fn lark_string(text: &str) -> String {
    crate::json::encode_body(&Value::String(text.to_string()))
}

fn not_starting_with(prefix: &str) -> String {
    let chars = prefix.chars().collect::<Vec<char>>();
    let head = |end: usize| chars[..end].iter().map(|character| escape_regex(*character)).collect::<String>();
    let differs = (0..chars.len())
        .map(|index| format!("{}[^{}]", head(index), escape_class(chars[index])))
        .collect::<Vec<String>>();
    let mut parts = vec![format!("(?:{})[\\s\\S]*", differs.join("|"))];
    parts.extend((1..chars.len()).map(head));
    parts.join("|")
}

fn escape_regex(character: char) -> String {
    if matches!(character, '\\' | '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/') {
        return format!("\\{character}");
    }
    character.to_string()
}

fn escape_class(character: char) -> String {
    if matches!(character, '\\' | ']' | '^' | '-' | '/') {
        return format!("\\{character}");
    }
    character.to_string()
}

fn escape_slashes(pattern: &str) -> String {
    let mut escaped = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' && chars.peek() == Some(&'/') {
            chars.next();
            escaped.push_str("\\/");
        } else if character == '/' {
            escaped.push_str("\\/");
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn rename_lark_start(grammar: &str) -> String {
    let mut renamed = false;
    grammar
        .split('\n')
        .map(|line| {
            if renamed {
                return line.to_string();
            }
            match rename_start_line(line) {
                Some(line) => {
                    renamed = true;
                    line
                }
                None => line.to_string(),
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn rename_start_line(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = line[indent..].strip_prefix("start")?;
    let spacing = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let tail = &rest[spacing..];
    tail.starts_with(':').then(|| format!("{}fb_start{}{tail}", &line[..indent], &rest[..spacing]))
}

pub fn tools(request: &LlmRequest, model: &Model) -> BTreeMap<String, Custom> {
    let formats = freeform_formats(model);
    let added = request
        .messages
        .iter()
        .filter_map(tools_update)
        .flat_map(|changes| changes.add.iter().cloned())
        .collect::<Vec<ToolEntry>>();
    let combined = request.tools.iter().chain(added.iter()).cloned().collect::<Vec<ToolEntry>>();
    flatten_tools(&combined)
        .into_iter()
        .filter_map(|tool| resolve(&tool, formats.as_deref()).map(|custom| (tool.name, custom)))
        .collect()
}

fn freeform_formats(model: &Model) -> Option<Vec<String>> {
    model
        .capabilities
        .as_ref()?
        .get("tools")?
        .get("freeform")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect()
}

pub fn grammar(custom: &Custom) -> Option<Value> {
    let (syntax, definition) = match custom.options.as_ref() {
        Some(options) => ("lark", options.grammar.as_deref()?),
        None => match &custom.format {
            Freeform::Text { .. } => return None,
            Freeform::Regex { pattern, .. } => ("regex", pattern.as_str()),
            Freeform::Lark { grammar, .. } => ("lark", grammar.as_str()),
        },
    };
    let mut object = Map::new();
    object.insert("syntax".to_string(), Value::String(syntax.to_string()));
    object.insert("definition".to_string(), Value::String(definition.to_string()));
    Some(Value::Object(object))
}

pub fn description(tool: &ToolDefinition, custom: &Custom) -> String {
    match custom.options.as_ref() {
        Some(options) => format!("{}\n\n{}", tool.description, options.description),
        None => tool.description.clone(),
    }
}

pub struct Parsed {
    pub input: String,
    pub header: Option<String>,
}

pub fn to_raw(custom: &Custom, input: &Value, header: Option<&Value>) -> String {
    let Some(object) = input.as_object() else {
        return encode_input(input);
    };
    let Some(Value::String(value)) = object.get(&custom.field) else {
        return encode_input(input);
    };
    let fields = custom.options.as_ref().map_or(&[][..], |options| options.fields.as_slice());
    let options = crate::json::encode_body(&Value::Object(pick(fields, object)));
    if options == "{}" {
        return value.clone();
    }
    let saved = header.and_then(Value::as_str).and_then(decode_line);
    let same =
        saved.as_ref().is_some_and(|saved| crate::json::encode_body(&Value::Object(pick(fields, saved))) == options);
    let line = match (same, header.and_then(Value::as_str)) {
        (true, Some(header)) => header.to_string(),
        _ => format!("{PREFIX} {options}"),
    };
    format!("{line}\n{value}")
}

fn pick(fields: &[OptionField], input: &Map<String, Value>) -> Map<String, Value> {
    let mut picked = Map::new();
    for item in fields {
        if let Some(value) = input.get(&item.name) {
            picked.insert(item.name.clone(), value.clone());
        }
    }
    picked
}

pub fn parse(custom: Option<&Custom>, raw: &str) -> Parsed {
    let plain = || Parsed { input: encode_field(field(custom), raw), header: None };
    let Some(definition) = custom.filter(|definition| definition.options.is_some()).filter(|_| raw.starts_with(PREFIX))
    else {
        return plain();
    };
    let Some(end) = raw.find('\n') else {
        return plain();
    };
    let header = &raw[..end];
    let Some(mut options) = decode_line(header) else {
        return Parsed { input: raw.to_string(), header: None };
    };
    options.insert(definition.field.clone(), Value::String(raw[end + 1..].to_string()));
    Parsed { input: crate::json::encode_body(&Value::Object(options)), header: Some(header.to_string()) }
}

fn encode_field(field: &str, raw: &str) -> String {
    let mut object = Map::new();
    object.insert(field.to_string(), Value::String(raw.to_string()));
    crate::json::encode_body(&Value::Object(object))
}

fn encode_input(input: &Value) -> String {
    if input.is_null() { "{}".to_string() } else { crate::json::encode_body(input) }
}

fn decode_line(header: &str) -> Option<Map<String, Value>> {
    decode_arguments(header.get(PREFIX.len()..)?)
}

pub fn stream_start(custom: Option<&Custom>, raw: &str) -> String {
    format!("{{{}:\"{}", crate::json::encode_body(&Value::String(field(custom).to_string())), stream_delta(raw))
}

pub fn stream_delta(raw: &str) -> String {
    let encoded = crate::json::encode_body(&Value::String(raw.to_string()));
    encoded.get(1..encoded.len().saturating_sub(1)).unwrap_or_default().to_string()
}

pub struct CloseOutcome {
    pub tools: tool_stream::State,
    pub events: Vec<LlmEvent>,
}

pub fn close(
    tools: tool_stream::State,
    keys: impl IntoIterator<Item = i64>,
    custom: &BTreeMap<String, Custom>,
    metadata_key: &str,
    final_raw: Option<&str>,
) -> CloseOutcome {
    let mut tools = tools;
    let mut events = Vec::new();
    for key in keys {
        let Some(pending) = tools.get(&key).cloned() else {
            continue;
        };
        let definition = custom.get(&pending.name);
        let streamed = decode_arguments(&format!("{}{STREAM_END}", pending.input))
            .and_then(|options| options.get(field(definition)).cloned());
        if let Some(final_raw) = final_raw
            && streamed.as_ref().and_then(Value::as_str) != Some(final_raw)
        {
            tools.insert(key, settle(&pending, definition, metadata_key, final_raw));
            continue;
        }
        let Some(appended) = tool_stream::append(&mut tools, key, STREAM_END) else {
            continue;
        };
        events.extend(appended.events);
        let Some(streamed) = streamed.as_ref().and_then(Value::as_str) else {
            continue;
        };
        let settled = tools.get(&key).map(|tool| settle(tool, definition, metadata_key, streamed));
        if let Some(settled) = settled {
            tools.insert(key, settled);
        }
    }
    CloseOutcome { tools, events }
}

fn settle(
    tool: &tool_stream::PendingTool,
    custom: Option<&Custom>,
    metadata_key: &str,
    raw: &str,
) -> tool_stream::PendingTool {
    let parsed = parse(custom, raw);
    let mut settled = tool.clone();
    settled.input = parsed.input;
    if let Some(header) = parsed.header {
        let mut metadata = settled.provider_metadata.take().unwrap_or_default();
        let mut inner = metadata.get(metadata_key).and_then(Value::as_object).cloned().unwrap_or_default();
        inner.insert("header".to_string(), Value::String(header));
        metadata.insert(metadata_key.to_string(), Value::Object(inner));
        settled.provider_metadata = Some(metadata);
    }
    settled
}

pub fn metadata(item_id: &str, custom: bool) -> ProviderMetadata {
    let mut metadata = ProviderMetadata::new();
    metadata.insert("itemId".to_string(), Value::String(item_id.to_string()));
    if custom {
        metadata.insert("custom".to_string(), Value::Bool(true));
    }
    metadata
}

pub fn keeps_item_id(provider_metadata: Option<&ProviderMetadata>, key: &str, custom: bool) -> bool {
    let arrived_custom = provider_metadata
        .and_then(|metadata| metadata.get(key))
        .and_then(|value| value.get("custom"))
        .is_some_and(|value| value == &Value::Bool(true));
    arrived_custom == custom
}

fn field(custom: Option<&Custom>) -> &str {
    custom.map_or("input", |custom| custom.field.as_str())
}

fn format_type(format: &Freeform) -> &'static str {
    match format {
        Freeform::Text { .. } => "text",
        Freeform::Regex { .. } => "regex",
        Freeform::Lark { .. } => "lark",
    }
}

fn decode_arguments(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}
