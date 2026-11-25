use std::sync::Arc;

use crate::instructions::{BoxFuture, Key, Observed, Source};
use crate::reference::ReferenceService;

pub const KEY: &str = "core/reference-guidance";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub name: String,
    pub path: String,
    pub description: String,
}

pub fn summaries(references: &[opencode_contract::generated::ReferenceInfo]) -> Vec<Summary> {
    let mut summaries: Vec<Summary> = references
        .iter()
        .filter_map(|reference| {
            Some(Summary {
                name: reference.name.clone(),
                path: reference.path.0.clone(),
                description: reference.description.clone()?,
            })
        })
        .collect();
    summaries.sort_by(|a, b| a.name.cmp(&b.name));
    summaries
}

fn entries(summaries: &[Summary]) -> Vec<String> {
    summaries
        .iter()
        .flat_map(|summary| {
            [
                "  <reference>".to_string(),
                format!("    <name>{}</name>", summary.name),
                format!("    <path>{}</path>", summary.path),
                format!("    <description>{}</description>", summary.description),
                "  </reference>".to_string(),
            ]
        })
        .collect()
}

pub fn render(summaries: &[Summary]) -> String {
    let mut lines = vec![
        "Project references provide additional directories that can be accessed when relevant.".to_string(),
        "<available_references>".to_string(),
    ];
    lines.extend(entries(summaries));
    lines.push("</available_references>".to_string());
    lines.join("\n")
}

pub fn source(references: ReferenceService) -> Arc<dyn Source> {
    Arc::new(GuidanceSource {
        key: Key::new(KEY).expect("the reference guidance instruction key is valid"),
        references,
    })
}

struct GuidanceSource {
    key: Key,
    references: ReferenceService,
}

impl Source for GuidanceSource {
    fn key(&self) -> &Key {
        &self.key
    }

    fn read<'a>(&'a self) -> BoxFuture<'a, Observed> {
        let references = self.references.clone();
        Box::pin(async move {
            let Ok(listed) = references.list().await else {
                return Observed::Unavailable;
            };
            let summaries = summaries(&listed);
            if summaries.is_empty() {
                return Observed::Removed;
            }
            Observed::Value(serde_json::Value::Array(
                summaries
                    .iter()
                    .map(|summary| {
                        serde_json::json!({
                            "name": summary.name,
                            "path": summary.path,
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
                    name: item.get("name")?.as_str()?.to_string(),
                    path: item.get("path")?.as_str()?.to_string(),
                    description: item.get("description")?.as_str()?.to_string(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(render(&summaries))
    }
}
