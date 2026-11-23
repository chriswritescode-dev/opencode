use std::sync::LazyLock;

use regex_lite::Regex;
use serde_json::Value;

use crate::route::model::Model;

pub const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[./])claude-([a-z]+)-([0-9]+)(?:[.-]([0-9]{1,2}))?(?:$|[-:@])")
        .expect("claude version pattern compiles")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub family: String,
    pub major: u32,
    pub minor: u32,
}

pub fn claude_version(id: &str) -> Option<Version> {
    let id = id.to_lowercase();
    let captures = VERSION.captures(&id)?;
    let family = captures.get(1)?.as_str().to_string();
    let major = captures.get(2)?.as_str().parse::<u32>().ok()?;
    let minor = captures.get(3).and_then(|minor| minor.as_str().parse::<u32>().ok()).unwrap_or(0);
    Some(Version { family, major, minor })
}

pub fn supports_thinking_block_binding(model: &Model) -> bool {
    if let Some(value) = model
        .compatibility
        .as_ref()
        .and_then(|compatibility| compatibility.get("supportsThinkingBlockBinding"))
        .and_then(Value::as_bool)
    {
        return value;
    }
    claude_version(&model.id).is_some_and(|version| version.major > 5 || (version.major == 5 && version.minor >= 1))
}
