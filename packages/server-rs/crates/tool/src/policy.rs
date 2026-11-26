use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub model_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct ModelTools {
    pub hidden: BTreeSet<String>,
    pub direct: BTreeSet<String>,
}

pub fn model_tools(model: Option<&ModelInfo>, websearch: bool, full_codemode: bool) -> ModelTools {
    let mut hidden = BTreeSet::new();
    if !websearch {
        hidden.insert("websearch".to_string());
    }
    if full_codemode {
        for name in ["glob", "grep", "webfetch"] {
            hidden.insert(name.to_string());
        }
    }
    if let Some(model) = model {
        let names: &[&str] = if uses_patch(model) { &["edit", "write"] } else { &["patch"] };
        for name in names {
            hidden.insert((*name).to_string());
        }
    }
    ModelTools { hidden, direct: BTreeSet::new() }
}

fn uses_patch(model: &ModelInfo) -> bool {
    let id = model.id.to_lowercase();
    let model_id = model.model_id.to_lowercase();
    (id.contains("gpt-") || model_id.contains("gpt-"))
        && !id.contains("oss")
        && !model_id.contains("oss")
        && !id.contains("gpt-4")
        && !model_id.contains("gpt-4")
}
