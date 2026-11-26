use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use opencode_config::options::truthy;
use opencode_location::state::{Options, Registration, State, StateError};

use crate::events::RegistryEvents;
use crate::instructions::{FixedSource, Key, List, Observed, Source};

pub const EVENT: &str = "instruction-discovery.updated";
pub const KEY: &str = "core/instructions";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstructionOptions {
    pub project: bool,
    pub global: bool,
    pub claude: bool,
}

impl Default for InstructionOptions {
    fn default() -> Self {
        InstructionOptions { project: true, global: true, claude: true }
    }
}

impl InstructionOptions {
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let claude = !truthy(var("OPENCODE_DISABLE_CLAUDE_CODE").as_deref().map(OsStr::new))
            && !truthy(var("OPENCODE_DISABLE_CLAUDE_CODE_PROMPT").as_deref().map(OsStr::new));
        InstructionOptions { project: true, global: true, claude }
    }
}

pub fn filenames(claude: bool) -> Vec<&'static str> {
    if claude { vec!["AGENTS.md", "CLAUDE.md"] } else { vec!["AGENTS.md"] }
}

pub fn prefer_agents(found: &[PathBuf]) -> Vec<PathBuf> {
    found
        .iter()
        .filter(|file| {
            let claude = file.file_name().is_some_and(|name| name == "CLAUDE.md");
            !claude || !found.contains(&file.with_file_name("AGENTS.md"))
        })
        .cloned()
        .collect()
}

pub fn render(files: &[File]) -> String {
    files
        .iter()
        .map(|file| format!("Instructions from: {}\n{}", file.path, file.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryData {
    pub files: IndexMap<String, File>,
    pub available: bool,
}

impl Default for DiscoveryData {
    fn default() -> Self {
        DiscoveryData { files: IndexMap::new(), available: true }
    }
}

pub struct DiscoveryEditor<'a> {
    data: &'a mut DiscoveryData,
}

impl DiscoveryEditor<'_> {
    pub fn list(&self) -> Vec<&File> {
        self.data.files.values().collect()
    }

    pub fn add(&mut self, file: File) {
        self.data.files.insert(file.path.clone(), file);
    }

    pub fn update<F: FnOnce(&mut File)>(&mut self, path: &str, f: F) {
        let Some(file) = self.data.files.get_mut(path) else {
            return;
        };
        f(file);
        file.path = path.to_string();
    }

    pub fn remove(&mut self, path: &str) {
        self.data.files.shift_remove(path);
    }

    pub fn unavailable(&mut self) {
        self.data.available = false;
    }
}

#[derive(Clone)]
pub struct InstructionDiscovery {
    state: State<DiscoveryData>,
    options: InstructionOptions,
}

impl InstructionDiscovery {
    pub fn new(events: Arc<dyn RegistryEvents>, options: InstructionOptions) -> Self {
        InstructionDiscovery {
            state: State::new(Options {
                name: Some("instruction-discovery".to_string()),
                notify: Some(Box::new(move |_| {
                    events.updated(EVENT);
                    Box::pin(async {})
                })),
                ..Options::new(DiscoveryData::default)
            }),
            options,
        }
    }

    pub fn options(&self) -> &InstructionOptions {
        &self.options
    }

    pub async fn list(&self) -> Result<Option<Vec<File>>, StateError> {
        let data = self.state.get().await?;
        if !data.available {
            return Ok(None);
        }
        Ok(Some(data.files.values().cloned().collect()))
    }

    pub async fn load(&self) -> Result<List, StateError> {
        let files = self.list().await?;
        let observed = match &files {
            None => Observed::Unavailable,
            Some(files) if files.is_empty() => Observed::Removed,
            Some(files) => Observed::Value(serde_json::Value::Array(
                files.iter().map(|file| serde_json::json!({ "path": &file.path, "content": &file.content })).collect(),
            )),
        };
        let source: Arc<dyn Source> = Arc::new(FixedSource::new(
            Key::new(KEY).expect("the instruction discovery key is valid"),
            observed,
            initial,
        ));
        Ok(vec![source])
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a, 'b> Fn(&'a mut DiscoveryEditor<'b>) + Send + Sync + 'static,
    {
        self.state
            .transform(move |data| {
                let mut editor = DiscoveryEditor { data };
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }
}

fn initial(value: &serde_json::Value) -> Option<String> {
    let files = value
        .as_array()?
        .iter()
        .map(|item| {
            Some(File {
                path: item.get("path")?.as_str()?.to_string(),
                content: item.get("content")?.as_str()?.to_string(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(render(&files))
}
