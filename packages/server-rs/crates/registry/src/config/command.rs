use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use indexmap::IndexMap;
use opencode_config::entry::Entry;
use opencode_config::observer::{self, ObserverHandle};
use opencode_config::service::ConfigService;
use opencode_contract::codec::{self, Decode, JsonMap};
use opencode_contract::generated::{
    AgentInfoMode, ConfigEntryDocumentInfoCommandsValue, ConfigEntryDocumentInfoCommandsValueModel, ModelRef,
    ProviderID,
};
use opencode_location::state::Registration;
use regex_lite::Regex;
use serde_json::Value;

use crate::command::{
    BoxFuture, CommandDefinition, CommandEditor, CommandExecute, CommandRuntime, CommandService, Invocation, Prompt,
    PromptRequest, SubagentRequest,
};
use crate::markdown;
use crate::scan::{self, ScanOptions};

pub const SOURCE_DIRECTORIES: [&str; 2] = ["command", "commands"];

static ARGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:\[Image\s+\d+\]|"[^"]*"|'[^']*'|[^\s"']+)"#).expect("the arguments pattern compiles")
});
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$([1-9]\d*)").expect("the placeholder pattern compiles"));
static ARGUMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$(?:ARGUMENTS|([1-9]\d*))").expect("the argument pattern compiles"));
static SHELL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!`([^`]+)`").expect("the shell pattern compiles"));

pub struct ConfigCommandHandle {
    _registration: Registration,
    _observer: ObserverHandle,
}

pub fn load(entries: &[Entry]) -> Vec<JsonMap<ConfigEntryDocumentInfoCommandsValue>> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Document(document) => document.info.commands.clone().unwrap_or_else(|| JsonMap(Vec::new())),
            Entry::Directory { path } => load_directory(path),
        })
        .collect()
}

pub fn apply(
    commands: &[JsonMap<ConfigEntryDocumentInfoCommandsValue>],
    directory: &Path,
    editor: &mut CommandEditor<'_>,
) {
    for document in commands {
        for (name, command) in &document.0 {
            editor.add(CommandDefinition {
                name: name.clone(),
                description: command.description.clone(),
                execute: Arc::new(ConfigExecute {
                    name: name.clone(),
                    command: command.clone(),
                    directory: directory.to_path_buf(),
                }),
            });
        }
    }
}

pub async fn register(config: &ConfigService, commands: &CommandService, directory: &Path) -> ConfigCommandHandle {
    let documents = Arc::new(RwLock::new(Vec::<JsonMap<ConfigEntryDocumentInfoCommandsValue>>::new()));
    let observer = observer::observe_sources(config, &SOURCE_DIRECTORIES, load, {
        let documents = documents.clone();
        let commands = commands.clone();
        move |loaded| {
            *documents.write().expect("the config command lock is never poisoned") = loaded;
            let commands = commands.clone();
            tokio::spawn(async move { commands.reload().await });
        }
    });
    let registration = commands
        .transform({
            let documents = documents.clone();
            let directory = directory.to_path_buf();
            move |editor| {
                let documents = documents.read().expect("the config command lock is never poisoned");
                apply(&documents, &directory, editor);
            }
        })
        .await;
    ConfigCommandHandle { _registration: registration, _observer: observer }
}

pub struct TemplateParts {
    pub text: String,
    pub shells: Vec<String>,
}

pub fn expand_template(template: &str, input: &str) -> TemplateParts {
    let args = parse_arguments(input);
    let placeholders: Vec<&str> = PLACEHOLDER.find_iter(template).map(|item| item.as_str()).collect();
    let last = placeholders.iter().filter_map(|item| item[1..].parse::<usize>().ok()).max().unwrap_or(0);
    let mut with_arguments = String::new();
    let mut cursor = 0;
    for captures in ARGUMENT.captures_iter(template) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        with_arguments.push_str(&template[cursor..whole.start()]);
        let replacement = if whole.as_str() == "$ARGUMENTS" {
            input.to_string()
        } else {
            let position = captures.get(1).and_then(|index| index.as_str().parse::<usize>().ok()).unwrap_or(0);
            let arg_index = position.saturating_sub(1);
            match args.get(arg_index) {
                None => String::new(),
                Some(_) if position == last => args[arg_index..].join(" "),
                Some(arg) => arg.clone(),
            }
        };
        with_arguments.push_str(&replacement);
        cursor = whole.end();
    }
    with_arguments.push_str(&template[cursor..]);

    let text = if placeholders.is_empty() && !template.contains("$ARGUMENTS") && !input.trim().is_empty() {
        format!("{with_arguments}\n\n{input}").trim().to_string()
    } else {
        with_arguments.trim().to_string()
    };
    let shells = SHELL
        .captures_iter(&text)
        .filter_map(|captures| captures.get(1).map(|source| source.as_str().to_string()))
        .collect();
    TemplateParts { text, shells }
}

fn parse_arguments(input: &str) -> Vec<String> {
    ARGS.find_iter(input).map(|item| trim_quotes(item.as_str()).to_string()).collect()
}

fn trim_quotes(value: &str) -> &str {
    let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
    value.strip_suffix(['"', '\'']).unwrap_or(value)
}

fn fill_shells(text: &str, outputs: &[String]) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    let mut outputs = outputs.iter();
    for captures in SHELL.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        result.push_str(&text[cursor..whole.start()]);
        result.push_str(outputs.next().map(String::as_str).unwrap_or(""));
        cursor = whole.end();
    }
    result.push_str(&text[cursor..]);
    result
}

struct ConfigExecute {
    name: String,
    command: ConfigEntryDocumentInfoCommandsValue,
    directory: PathBuf,
}

impl CommandExecute for ConfigExecute {
    fn execute<'a>(
        &'a self,
        invocation: Invocation,
        runtime: &'a dyn CommandRuntime,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Invocation { session_id, prompt, delivery } = invocation;
            let agent_id = self.command.agent.clone();
            let agent_info = match &agent_id {
                Some(id) => Some(runtime.resolve_agent(id.clone()).await?),
                None => None,
            };
            let model = match &self.command.model {
                Some(model) => Some(model_ref(model)),
                None => agent_info.as_ref().and_then(|info| info.model.clone()),
            };
            let parts = expand_template(&self.command.template, &prompt.text);
            let text = if parts.shells.is_empty() {
                parts.text
            } else {
                let mut outputs = Vec::new();
                for source in &parts.shells {
                    outputs.push(runtime.run_shell(source.clone(), self.directory.clone()).await?);
                }
                fill_shells(&parts.text, &outputs)
            };
            let is_subagent = self
                .command
                .subagent
                .or(self.command.subtask)
                .unwrap_or_else(|| agent_info.as_ref().is_some_and(|info| info.mode == AgentInfoMode::Subagent));
            if is_subagent {
                let title = self.command.description.clone().unwrap_or_else(|| self.name.clone());
                let request = SubagentRequest {
                    parent_session_id: session_id,
                    title: title.clone(),
                    description: title,
                    agent: agent_id,
                    model,
                    prompt: Prompt {
                        text: format!("You are a subagent spawned by another session.\n{text}"),
                        ..prompt
                    },
                };
                return runtime.start_subagent(request).await;
            }
            if agent_id.is_some() || model.is_some() {
                runtime.switch_agent_model(session_id.clone(), agent_id, model).await?;
            }
            let request = PromptRequest { session_id, prompt: Prompt { text, ..prompt }, delivery, resume: None };
            runtime.prompt(request).await
        })
    }
}

fn model_ref(model: &ConfigEntryDocumentInfoCommandsValueModel) -> ModelRef {
    match model {
        ConfigEntryDocumentInfoCommandsValueModel::String(value) => {
            let (provider, rest) = value.0.split_once('/').expect("the model pattern requires a provider");
            let (id, variant) = match rest.split_once('#') {
                Some((id, variant)) => (id, Some(variant.to_string())),
                None => (rest, None),
            };
            ModelRef { id: id.to_string(), provider_id: ProviderID(provider.to_string()), variant }
        }
        ConfigEntryDocumentInfoCommandsValueModel::Object(value) => ModelRef {
            id: value.model.clone(),
            provider_id: ProviderID(value.provider_id.clone()),
            variant: value.variant.clone(),
        },
    }
}

fn load_directory(directory: &Path) -> JsonMap<ConfigEntryDocumentInfoCommandsValue> {
    let mut commands = IndexMap::new();
    let files = scan::scan(
        directory,
        "{command,commands}/**/*.md",
        ScanOptions { dot: true, symlink: true, include_files_only: true, ignore: &[] },
    );
    for file in &files {
        let Some((name, command)) = load_file(directory, file) else {
            continue;
        };
        commands.insert(name, command);
    }
    JsonMap(commands.into_iter().collect())
}

fn load_file(directory: &Path, file: &Path) -> Option<(String, ConfigEntryDocumentInfoCommandsValue)> {
    let content = std::fs::read_to_string(file).ok()?;
    let Some(parsed) = markdown::parse(&content) else {
        log::warn!("command file skipped: {} (markdown)", file.display());
        return None;
    };
    let mut data = markdown::join_legacy_variant(parsed.data);
    data.insert("template".to_string(), Value::String(parsed.content.trim().to_string()));
    let command = match ConfigEntryDocumentInfoCommandsValue::decode(&Value::Object(data), &mut codec::Path::new()) {
        Ok(command) => command,
        Err(error) => {
            log::warn!("command file skipped: {} ({error})", file.display());
            return None;
        }
    };
    Some((command_name(directory, file), command))
}

fn command_name(directory: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(directory).unwrap_or(file).to_string_lossy().replace('\\', "/");
    let stripped =
        relative.strip_prefix("command/").or_else(|| relative.strip_prefix("commands/")).unwrap_or(&relative);
    stripped.strip_suffix(".md").unwrap_or(stripped).to_string()
}
