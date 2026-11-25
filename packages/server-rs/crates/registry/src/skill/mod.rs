pub mod discovery;
pub mod file;
pub mod guidance;
pub mod scan;

use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;
use opencode_contract::codec::{Encode, write_string};
use opencode_contract::generated::{PermissionEffect, PermissionRule, SkillID, SkillInfo};
use opencode_location::state::{Options, Registration, State, StateError, changes};

use crate::events::RegistryEvents;

pub const EVENT: &str = "skill.updated";

#[derive(Default)]
pub struct SkillData {
    pub skills: IndexMap<String, SkillInfo>,
    pub files: IndexMap<String, IndexMap<String, String>>,
}

pub struct SkillEditor<'a> {
    data: &'a mut SkillData,
}

impl<'a> SkillEditor<'a> {
    pub fn new(data: &'a mut SkillData) -> Self {
        SkillEditor { data }
    }

    pub fn list(&self) -> Vec<&SkillInfo> {
        self.data.skills.values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&SkillInfo> {
        self.data.skills.get(id)
    }

    pub fn add(&mut self, skill: SkillInfo, files: Option<&IndexMap<String, String>>) {
        let id = skill.id.0.clone();
        let mut entries: Vec<(String, String)> = files
            .into_iter()
            .flat_map(|files| files.iter())
            .filter(|(file, _)| file.as_str() != "SKILL.md")
            .map(|(file, content)| (file.clone(), content.clone()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.is_empty() {
            self.data.files.shift_remove(&id);
        } else {
            self.data.files.insert(id.clone(), entries.into_iter().collect());
        }
        self.data.skills.insert(id, skill);
    }

    pub fn update<F: FnOnce(&mut SkillInfo)>(&mut self, id: &str, f: F) {
        let Some(skill) = self.data.skills.get_mut(id) else { return };
        f(skill);
        skill.id = SkillID(id.to_string());
    }

    pub fn remove(&mut self, id: &str) {
        self.data.skills.shift_remove(id);
        self.data.files.shift_remove(id);
    }
}

#[derive(Clone)]
pub struct SkillService {
    state: State<SkillData>,
}

impl SkillService {
    pub fn new(events: Arc<dyn RegistryEvents>) -> Self {
        SkillService {
            state: State::new(Options {
                name: Some("skill".to_string()),
                notify: Some(changes(
                    |data: &SkillData| projection(data),
                    move |_| {
                        events.updated(EVENT);
                        Box::pin(async {})
                    },
                )),
                ..Options::new(SkillData::default)
            }),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<SkillInfo>, StateError> {
        Ok(self.state.get().await?.skills.get(id).cloned())
    }

    pub async fn files(&self, id: &str) -> Result<Option<IndexMap<String, String>>, StateError> {
        let data = self.state.get().await?;
        if !data.skills.contains_key(id) {
            return Ok(None);
        }
        Ok(data.files.get(id).cloned())
    }

    pub async fn list(&self) -> Result<Vec<SkillInfo>, StateError> {
        Ok(self.state.get().await?.skills.values().cloned().collect())
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a, 'b> Fn(&'a mut SkillEditor<'b>) + Send + Sync + 'static,
    {
        self.state
            .transform(move |data| {
                let mut editor = SkillEditor::new(data);
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }
}

pub fn available(skills: &[SkillInfo], rules: &[PermissionRule]) -> Vec<SkillInfo> {
    skills
        .iter()
        .filter(|skill| {
            opencode_permission::rule::evaluate("skill", &skill.id.0, &[rules]).effect != PermissionEffect::Deny
        })
        .cloned()
        .collect()
}

pub fn uri(id: &str, file: Option<&str>) -> String {
    match file {
        Some(file) => format!("skill://{id}/{file}"),
        None => format!("skill://{id}"),
    }
}

pub fn is_uri(value: &str) -> bool {
    value.starts_with("skill://")
}

pub fn to_model_output(skill: &SkillInfo, files: &[String]) -> String {
    let directory = if is_uri(&skill.path.0) { uri(&skill.id.0, None) } else { dirname(&skill.path.0) };
    let mut lines = vec![
        format!("<skill_content name=\"{}\">", skill.name.0),
        format!("# Skill: {}", skill.name.0),
        String::new(),
        skill.content.trim().to_string(),
        String::new(),
        format!("Base directory for this skill: {directory}"),
        "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.".to_string(),
        "Note: file list is sampled.".to_string(),
        String::new(),
        "<skill_files>".to_string(),
    ];
    lines.extend(files.iter().map(|file| format!("<file>{file}</file>")));
    lines.push("</skill_files>".to_string());
    lines.push("</skill_content>".to_string());
    lines.join("\n")
}

pub struct Prepared {
    pub directory: String,
    pub output: String,
}

pub fn prepare(skill: &SkillInfo, virtual_files: Option<&IndexMap<String, String>>) -> Prepared {
    if is_uri(&skill.path.0) {
        let mut names: Vec<&String> = virtual_files
            .into_iter()
            .flat_map(|files| files.keys())
            .filter(|file| file.as_str() != "SKILL.md")
            .collect();
        names.sort();
        let files: Vec<String> = names.into_iter().take(10).map(|file| uri(&skill.id.0, Some(file))).collect();
        let directory = uri(&skill.id.0, None);
        return Prepared { directory, output: to_model_output(skill, &files) };
    }
    let directory = dirname(&skill.path.0);
    let files = if Path::new(&skill.path.0).file_name().is_some_and(|name| name == "SKILL.md") {
        let globs = crate::scan::ignore_globs();
        let ignored: Vec<&str> = globs.iter().map(String::as_str).collect();
        let mut files = crate::scan::scan(
            Path::new(&directory),
            "**/*",
            crate::scan::ScanOptions { dot: true, symlink: false, include_files_only: true, ignore: &ignored },
        )
        .into_iter()
        .filter(|file| file.file_name().is_none_or(|name| name != "SKILL.md"))
        .map(|file| file.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
        files.sort();
        files.truncate(10);
        files
    } else {
        Vec::new()
    };
    Prepared { directory, output: to_model_output(skill, &files) }
}

fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

fn projection(data: &SkillData) -> String {
    let mut out = String::new();
    for skill in data.skills.values() {
        skill.encode(&mut out);
    }
    for (id, files) in &data.files {
        write_string(&mut out, id);
        for (file, content) in files {
            write_string(&mut out, file);
            write_string(&mut out, content);
        }
    }
    out
}
