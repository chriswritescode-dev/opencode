use std::path::Path;

use opencode_contract::generated::{AbsolutePath, SkillID, SkillInfo, SkillName};
use serde_json::{Map, Value};

pub enum ParseResult {
    Parsed(SkillInfo),
    SkippedMarkdown,
    SkippedFrontmatter,
}

pub fn parse(directory: &Path, filepath: &Path, content: &str) -> ParseResult {
    let Some(markdown) = crate::markdown::parse(content) else { return ParseResult::SkippedMarkdown };
    let Some(frontmatter) = frontmatter(&markdown.data) else { return ParseResult::SkippedFrontmatter };
    let id = skill_id(directory, filepath);
    let autoinvoke = metadata_boolean(frontmatter.metadata.as_ref(), "opencode/autoinvoke")
        .or_else(|| (boolean_value(frontmatter.disable_model_invocation.as_ref()) == Some(true)).then_some(false));
    ParseResult::Parsed(SkillInfo {
        id: SkillID(id.clone()),
        name: SkillName(frontmatter.name.unwrap_or(id)),
        description: frontmatter.description,
        autoinvoke,
        path: AbsolutePath(filepath.to_string_lossy().into_owned()),
        content: markdown.content,
    })
}

struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    metadata: Option<Value>,
    disable_model_invocation: Option<Value>,
}

fn frontmatter(data: &Map<String, Value>) -> Option<Frontmatter> {
    Some(Frontmatter {
        name: string_field(data.get("name"))?,
        description: string_field(data.get("description"))?,
        metadata: data.get("metadata").cloned(),
        disable_model_invocation: data.get("disable-model-invocation").cloned(),
    })
}

fn string_field(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        None => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn skill_id(directory: &Path, filepath: &Path) -> String {
    let name = filepath.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if filepath.parent() == Some(directory) && name != "SKILL.md" {
        return name.strip_suffix(".md").map(str::to_string).unwrap_or(name);
    }
    filepath.parent().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn metadata_boolean(metadata: Option<&Value>, key: &str) -> Option<bool> {
    let Some(Value::Object(metadata)) = metadata else { return None };
    boolean_value(metadata.get(key))
}

fn boolean_value(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => {
            let value = value.as_f64()?;
            if value == 1.0 {
                Some(true)
            } else if value == 0.0 {
                Some(false)
            } else {
                None
            }
        }
        Value::String(value) => match value.trim().to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
