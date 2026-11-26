use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

use crate::definition::ToolDefinition;

#[derive(Clone, Debug)]
pub struct BuiltinDefinition {
    pub definition: ToolDefinition,
    pub permission: Option<String>,
    pub codemode: Value,
}

#[derive(Deserialize)]
struct GeneratedEntry {
    #[serde(flatten)]
    definition: ToolDefinition,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default = "default_codemode")]
    codemode: Value,
}

#[derive(Deserialize)]
struct Generated {
    off: Vec<GeneratedEntry>,
    on: Vec<GeneratedEntry>,
}

struct Store {
    off: Vec<BuiltinDefinition>,
    on: Vec<BuiltinDefinition>,
}

static STORE: OnceLock<Store> = OnceLock::new();

pub fn definitions(full_codemode: bool) -> &'static [BuiltinDefinition] {
    let store = STORE.get_or_init(|| {
        let generated: Generated = serde_json::from_str(include_str!("definitions.json"))
            .expect("the generated tool definitions are valid JSON");
        Store {
            off: generated.off.into_iter().map(BuiltinDefinition::from).collect(),
            on: generated.on.into_iter().map(BuiltinDefinition::from).collect(),
        }
    });
    if full_codemode { &store.on } else { &store.off }
}

impl From<GeneratedEntry> for BuiltinDefinition {
    fn from(entry: GeneratedEntry) -> Self {
        BuiltinDefinition { definition: entry.definition, permission: entry.permission, codemode: entry.codemode }
    }
}

fn default_codemode() -> Value {
    Value::Bool(true)
}
