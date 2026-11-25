use std::sync::Arc;

use opencode_contract::generated::{PermissionRule, SkillInfo};

use crate::instructions::{BoxFuture, Key, Observed, Source};
use crate::skill::SkillService;

pub const KEY: &str = "core/skill-guidance";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub description: String,
}

pub fn summaries(skills: &[SkillInfo], rules: &[PermissionRule]) -> Vec<Summary> {
    let mut summaries: Vec<Summary> = crate::skill::available(skills, rules)
        .into_iter()
        .filter_map(|skill| {
            let description = skill.description?;
            if skill.autoinvoke == Some(false) {
                return None;
            }
            Some(Summary { id: skill.id.0, name: skill.name.0, description })
        })
        .collect();
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

pub fn render(summaries: &[Summary]) -> String {
    let mut lines = vec![
        "Skills provide specialized instructions and workflows for specific tasks.".to_string(),
        "Use the skill tool to load a skill when a task matches its description.".to_string(),
        "The user may also invoke a skill directly. When that happens, its instructions appear in the conversation as a <skill_content> block, the same shape the skill tool returns. A skill that is already present this way does not need to be invoked again.".to_string(),
    ];
    if summaries.is_empty() {
        lines.push("No skills are currently available.".to_string());
    } else {
        lines.push("<available_skills>".to_string());
        for summary in summaries {
            let Summary { id, name, description } = summary;
            lines.push("  <skill>".to_string());
            lines.push(format!("    <id>{id}</id>"));
            lines.push(format!("    <name>{name}</name>"));
            lines.push(format!("    <description>{description}</description>"));
            lines.push("  </skill>".to_string());
        }
        lines.push("</available_skills>".to_string());
    }
    lines.join("\n")
}

pub fn text(skills: &[SkillInfo], rules: &[PermissionRule]) -> String {
    let summaries = summaries(skills, rules);
    if summaries.is_empty() {
        return String::new();
    }
    render(&summaries)
}

pub fn source(skills: SkillService, rules: Vec<PermissionRule>) -> Arc<dyn Source> {
    Arc::new(GuidanceSource { key: Key::new(KEY).expect("the skill guidance instruction key is valid"), skills, rules })
}

struct GuidanceSource {
    key: Key,
    skills: SkillService,
    rules: Vec<PermissionRule>,
}

impl Source for GuidanceSource {
    fn key(&self) -> &Key {
        &self.key
    }

    fn read<'a>(&'a self) -> BoxFuture<'a, Observed> {
        let skills = self.skills.clone();
        let rules = self.rules.clone();
        Box::pin(async move {
            let Ok(listed) = skills.list().await else {
                return Observed::Unavailable;
            };
            let summaries = summaries(&listed, &rules);
            if summaries.is_empty() {
                return Observed::Removed;
            }
            Observed::Value(serde_json::Value::Array(
                summaries
                    .iter()
                    .map(|summary| {
                        serde_json::json!({
                            "id": summary.id,
                            "name": summary.name,
                            "description": summary.description,
                        })
                    })
                    .collect(),
            ))
        })
    }

    fn initial(&self, value: &serde_json::Value) -> Option<String> {
        let summaries = value
            .as_array()?
            .iter()
            .map(|item| {
                Some(Summary {
                    id: item.get("id")?.as_str()?.to_string(),
                    name: item.get("name")?.as_str()?.to_string(),
                    description: item.get("description")?.as_str()?.to_string(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(render(&summaries))
    }
}
