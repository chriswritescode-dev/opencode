use std::path::Path;
use std::sync::{Arc, RwLock};

use opencode_config::entry::Entry;
use opencode_config::normalize::v1;
use opencode_config::observer::{self, ObserverHandle};
use opencode_config::service::ConfigService;
use opencode_contract::codec::{self, Decode, Encode, JsonMap};
use opencode_contract::generated::{
    AgentColor, AgentInfoMode, ConfigEntryDocumentInfo, ConfigEntryDocumentInfoAgentsValue,
    ConfigEntryDocumentInfoAgentsValueMode, ConfigEntryDocumentInfoAgentsValueModel, ModelRef, PermissionRule,
    ProviderID,
};
use opencode_location::location::{normalize_posix, normalize_windows};
use opencode_location::state::Registration;
use serde_json::Value;

use crate::agent::{AgentEditor, AgentService};
use crate::markdown;
use crate::scan::{self, ScanOptions};

pub const SOURCE_DIRECTORIES: [&str; 4] = ["agent", "agents", "mode", "modes"];

const LEGACY_SOURCES: [(&str, bool); 2] = [("{agent,agents}/**/*.md", false), ("{mode,modes}/*.md", true)];
const AGENT_KEYS: [&str; 11] = [
    "variant",
    "model",
    "request",
    "system",
    "description",
    "mode",
    "hidden",
    "color",
    "steps",
    "disabled",
    "permissions",
];
const NATIVE_KEYS: [&str; 4] = ["permissions", "request", "system", "disabled"];
const PATH_ACTIONS: [&str; 3] = ["external_directory", "read", "edit"];
const NAME_PREFIXES: [&str; 4] = ["agent/", "agents/", "mode/", "modes/"];

pub struct ConfigAgentHandle {
    _registration: Registration,
    _observer: ObserverHandle,
}

pub fn load(entries: &[Entry]) -> Vec<ConfigEntryDocumentInfo> {
    let mut documents = Vec::new();
    for entry in entries {
        match entry {
            Entry::Document(document) => documents.push(document.info.clone()),
            Entry::Directory { path } => documents.extend(load_directory(path)),
        }
    }
    documents
}

pub fn apply(documents: &[ConfigEntryDocumentInfo], home: &Path, editor: &mut AgentEditor<'_>) {
    let home = home.to_string_lossy();
    let permissions: Vec<PermissionRule> = documents
        .iter()
        .flat_map(|document| document.permissions.iter().flatten())
        .map(|rule| expand_rule(rule.clone(), &home))
        .collect();

    if let Some(id) = documents.iter().rev().find_map(|document| document.default_agent.as_deref()) {
        editor.default(Some(id));
    }

    let existing: Vec<String> = editor.list().into_iter().map(|info| info.id.0.clone()).collect();
    for id in existing {
        editor.update(&id, |agent| agent.permissions.extend(permissions.iter().cloned()));
    }

    for document in documents {
        let Some(agents) = &document.agents else {
            continue;
        };
        for (id, item) in &agents.0 {
            if item.disabled == Some(true) {
                editor.remove(id);
                continue;
            }
            let exists = editor.get(id).is_some();
            editor.update(id, |agent| {
                if !exists {
                    agent.permissions.extend(permissions.iter().cloned());
                }
                if let Some(model) = &item.model {
                    agent.model = Some(model_ref(model));
                }
                if let Some(request) = &item.request {
                    if let Some(headers) = &request.headers {
                        merge_map(&mut agent.request.headers, headers);
                    }
                    if let Some(body) = &request.body {
                        merge_map(&mut agent.request.body, body);
                    }
                }
                if let Some(system) = &item.system {
                    agent.system = Some(system.clone());
                }
                if let Some(description) = &item.description {
                    agent.description = Some(description.clone());
                }
                if let Some(mode) = item.mode {
                    agent.mode = agent_mode(mode);
                }
                if let Some(hidden) = item.hidden {
                    agent.hidden = hidden;
                }
                if let Some(color) = &item.color {
                    agent.color = Some(AgentColor(color.clone()));
                }
                if let Some(steps) = item.steps {
                    agent.steps = Some(steps);
                }
                if let Some(rules) = &item.permissions {
                    agent.permissions.extend(rules.iter().map(|rule| expand_rule(rule.clone(), &home)));
                }
            });
        }
    }
}

pub async fn register(config: &ConfigService, agents: &AgentService, home: &Path) -> ConfigAgentHandle {
    let documents = Arc::new(RwLock::new(Vec::<ConfigEntryDocumentInfo>::new()));
    let observer = observer::observe_sources(config, &SOURCE_DIRECTORIES, load, {
        let documents = documents.clone();
        let agents = agents.clone();
        move |loaded| {
            *documents.write().expect("the config agent lock is never poisoned") = loaded;
            let agents = agents.clone();
            tokio::spawn(async move { agents.reload().await });
        }
    });
    let registration = agents
        .transform({
            let documents = documents.clone();
            let home = home.to_path_buf();
            move |editor| {
                let documents = documents.read().expect("the config agent lock is never poisoned");
                apply(&documents, &home, editor);
            }
        })
        .await;
    ConfigAgentHandle { _registration: registration, _observer: observer }
}

fn load_directory(directory: &Path) -> Vec<ConfigEntryDocumentInfo> {
    let ignore = scan::ignore_globs();
    let ignore: Vec<&str> = ignore.iter().map(String::as_str).collect();
    let mut documents = Vec::new();
    for (pattern, primary) in LEGACY_SOURCES {
        let files = scan::scan(
            directory,
            pattern,
            ScanOptions { dot: true, symlink: true, include_files_only: true, ignore: &ignore },
        );
        documents.extend(files.iter().filter_map(|file| load_file(directory, file, primary)));
    }
    documents
}

fn load_file(directory: &Path, file: &Path, primary: bool) -> Option<ConfigEntryDocumentInfo> {
    let content = std::fs::read_to_string(file).ok()?;
    if content.is_empty() {
        return None;
    }
    let Some(parsed) = markdown::parse(&content) else {
        log::warn!("agent file skipped: {} (markdown)", file.display());
        return None;
    };
    let name = agent_name(directory, file)?;
    let body = parsed.content.trim().to_string();
    let keys: Vec<String> = parsed.data.keys().cloned().collect();
    let unsupported: Vec<&str> =
        keys.iter().filter(|key| !AGENT_KEYS.contains(&key.as_str())).map(String::as_str).collect();
    let legacy = !unsupported.is_empty() && !keys.iter().any(|key| NATIVE_KEYS.contains(&key.as_str()));
    if !legacy && !unsupported.is_empty() {
        log::warn!("ignored unsupported agent fields in {}: {}", file.display(), unsupported.join(", "));
    }

    let agent = if legacy {
        let mut data = parsed.data.clone();
        data.insert("name".to_string(), Value::String(name.clone()));
        data.insert("prompt".to_string(), Value::String(body));
        let Some(decoded) = v1::decode_agent(&Value::Object(data)) else {
            log::warn!("agent file skipped: {} (legacy frontmatter)", file.display());
            return None;
        };
        decode_agent(&v1::migrate_agent(&decoded), file)?
    } else {
        let mut data = markdown::join_legacy_variant(parsed.data);
        data.insert("system".to_string(), Value::String(body));
        if data.get("color").is_some_and(|color| !color.as_str().is_some_and(is_agent_color)) {
            data.remove("color");
        }
        decode_agent(&Value::Object(data), file)?
    };

    let agent = if primary {
        ConfigEntryDocumentInfoAgentsValue { mode: Some(ConfigEntryDocumentInfoAgentsValueMode::Primary), ..agent }
    } else {
        agent
    };

    let mut encoded = String::new();
    agent.encode(&mut encoded);
    let mut agents = serde_json::Map::new();
    agents.insert(name, serde_json::from_str(&encoded).ok()?);
    let mut value = serde_json::Map::new();
    value.insert("agents".to_string(), Value::Object(agents));
    ConfigEntryDocumentInfo::decode(&Value::Object(value), &mut codec::Path::new()).ok()
}

fn decode_agent(value: &Value, file: &Path) -> Option<ConfigEntryDocumentInfoAgentsValue> {
    match ConfigEntryDocumentInfoAgentsValue::decode(value, &mut codec::Path::new()) {
        Ok(agent) => Some(agent),
        Err(error) => {
            log::warn!("agent file skipped: {} ({error})", file.display());
            None
        }
    }
}

fn agent_name(directory: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(directory).ok()?.to_string_lossy().replace('\\', "/");
    let stripped = NAME_PREFIXES.iter().find_map(|prefix| relative.strip_prefix(prefix)).unwrap_or(&relative);
    Some(stripped.strip_suffix(".md").unwrap_or(stripped).to_string())
}

fn is_agent_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    hex.len() == 6 && hex.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn model_ref(model: &ConfigEntryDocumentInfoAgentsValueModel) -> ModelRef {
    match model {
        ConfigEntryDocumentInfoAgentsValueModel::String(value) => {
            let (provider, rest) = value.0.split_once('/').expect("the model pattern requires a provider");
            let (id, variant) = match rest.split_once('#') {
                Some((id, variant)) => (id, Some(variant.to_string())),
                None => (rest, None),
            };
            ModelRef { id: id.to_string(), provider_id: ProviderID(provider.to_string()), variant }
        }
        ConfigEntryDocumentInfoAgentsValueModel::Object(value) => ModelRef {
            id: value.model.clone(),
            provider_id: ProviderID(value.provider_id.clone()),
            variant: value.variant.clone(),
        },
    }
}

fn merge_map<T: Clone>(target: &mut JsonMap<T>, source: &JsonMap<T>) {
    for (key, value) in &source.0 {
        match target.0.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, existing)) => *existing = value.clone(),
            None => target.0.push((key.clone(), value.clone())),
        }
    }
}

fn expand_rule(mut rule: PermissionRule, home: &str) -> PermissionRule {
    if PATH_ACTIONS.contains(&rule.action.as_str()) {
        rule.resource = expand_home(&rule.resource, home);
    }
    rule
}

fn expand_home(resource: &str, home: &str) -> String {
    if resource == "~" || resource == "$HOME" {
        return home.to_string();
    }
    let relative = resource
        .strip_prefix("~/")
        .or_else(|| resource.strip_prefix("$HOME/"))
        .or_else(|| resource.strip_prefix("$HOME\\"));
    let Some(relative) = relative else {
        return resource.to_string();
    };
    if home.starts_with('/') {
        if relative.is_empty() {
            return normalize_posix(home);
        }
        return normalize_posix(&format!("{home}/{relative}"));
    }
    if relative.is_empty() {
        return normalize_windows(home);
    }
    normalize_windows(&format!("{home}\\{relative}"))
}

fn agent_mode(mode: ConfigEntryDocumentInfoAgentsValueMode) -> AgentInfoMode {
    match mode {
        ConfigEntryDocumentInfoAgentsValueMode::Subagent => AgentInfoMode::Subagent,
        ConfigEntryDocumentInfoAgentsValueMode::Primary => AgentInfoMode::Primary,
        ConfigEntryDocumentInfoAgentsValueMode::All => AgentInfoMode::All,
    }
}
