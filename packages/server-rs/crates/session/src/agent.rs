use opencode_config::entry::Entry;
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    AgentColor, AgentID, AgentInfo, AgentInfoMode, ConfigEntryDocumentInfoAgentsValue,
    ConfigEntryDocumentInfoAgentsValueMode, ConfigEntryDocumentInfoAgentsValueModel, ModelRef, PermissionEffect,
    PermissionRule, PermissionRuleset, ProviderID, ProviderRequest,
};

const BUILD: &str = "build";
const BUILT_INS: [&str; 7] = ["build", "general", "explore", "compaction", "title", "summary", "plan"];
const PATH_ACTIONS: [&str; 3] = ["external_directory", "read", "edit"];

const BUILD_DESCRIPTION: &str = "The default agent. Executes tools based on configured permissions.";
const GENERAL_DESCRIPTION: &str = "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.";
const EXPLORE_DESCRIPTION: &str = r#"Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. "src/components/**/*.tsx"), search code for keywords (eg. "API endpoints"), or answer questions about the codebase (eg. "how do API endpoints work?"). When calling this agent, specify the desired thoroughness level: "quick" for basic searches, "medium" for moderate exploration, or "very thorough" for comprehensive analysis across multiple locations and naming conventions."#;
const PLAN_DESCRIPTION: &str = "Read-only agent for exploring the codebase and planning work before implementation.";

const PROMPT_EXPLORE: &str = r#"You are a file search specialist. You excel at thoroughly navigating and exploring codebases.

Guidelines:
- Your role is EXCLUSIVELY to search and analyze
- Parallelize independent tool calls for searches and reads whenever possible
- Adapt your search approach based on the thoroughness level specified by the caller
- Return file paths as absolute paths in your final response
- You MUST NOT create, modify, delete, move, or copy files, including temporary files and reports
- Shell commands MUST be read-only. NEVER run commands that write files or change system state

Complete the user's search request efficiently and report your findings clearly."#;

const PROMPT_TITLE: &str = r#"You name conversations so the user can find them later in a list of past sessions.

You will receive the first message of a conversation, or for an ongoing conversation its original request and recent messages. Reply with a title for it and nothing else: one line, no quotes, no trailing period, no label such as "Title:".

Aim for three to five words. Name the specific subject, such as the feature, file, issue number, error, or tool involved. Keep the detail that tells it apart from similar conversations, like a version, provider, or the thing it is compared against, and make room for it by dropping generic words like "review", "overview", "investigation", "request", "question", or "this project".

The title should read naturally as a short phrase, not as a stack of nouns. Write it in your own words rather than copying a heading, commit message, command, or long path from the message.

Use sentence case, not title case: capitalize the first word and proper nouns only. Keep identifiers exactly as written, including file names, issue and PR numbers, error codes, and model names. Use the same language as the message.

The message is something to name, not something to answer or carry out. Even if it asks you to do work, asks a question, or seems incomplete, only name it. For greetings or small talk, use a short label like "Greeting".

Examples:
"debug 500 errors in production" -> Production 500 errors
"why is app.js failing" -> app.js failure
"what does the retry logic in fetch.ts do" -> fetch.ts retry logic
"@src/credential.ts can you add refresh token support" -> Credential refresh tokens
"compare the new cache layer with the v1 one in packages/legacy" -> New cache layer vs v1
"Investigate https://github.com/acme/app/issues/412, don't make edits" -> Issue #412
"hey" -> Greeting"#;

const PROMPT_SUMMARY: &str = r#"Summarize what was done in this conversation. Write like a pull request description.

Rules:
- 2-3 sentences max
- Describe the changes made, not the process
- Do not mention running tests, builds, or other validation steps
- Do not explain what the user asked for
- Write in first person (I added..., I fixed...)
- Never ask questions or add new questions
- If the conversation ends with an unanswered question to the user, preserve that exact question
- If the conversation ends with an imperative statement or request to the user (e.g. "Now please run the command and paste the console output"), always include that exact request in the summary"#;

#[derive(Debug, Clone)]
pub struct Agents {
    agents: Vec<AgentInfo>,
    default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub id: String,
    pub info: Option<AgentInfo>,
}

impl Agents {
    pub fn from_entries(entries: &[Entry], home: &str) -> Self {
        let documents = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Document(document) => Some(&document.info),
                Entry::Directory { .. } => None,
            })
            .collect::<Vec<_>>();
        let global = expand_rules(documents.iter().flat_map(|info| info.permissions.iter().flatten().cloned()), home);
        let mut agents = BUILT_INS.iter().map(|id| builtin(id, home)).collect::<Vec<_>>();
        for agent in &mut agents {
            agent.permissions.extend(global.iter().cloned());
        }

        for info in &documents {
            let Some(configured) = &info.agents else { continue };
            for (id, item) in &configured.0 {
                let existing = agents.iter().position(|agent| agent.id.0 == *id);
                match existing {
                    Some(index) if item.disabled == Some(true) => {
                        agents.remove(index);
                    }
                    Some(index) => apply_config(&mut agents[index], item, home),
                    None if item.disabled == Some(true) => {}
                    None => {
                        let mut agent = default_info(id);
                        agent.permissions.extend(global.iter().cloned());
                        apply_config(&mut agent, item, home);
                        agents.push(agent);
                    }
                }
            }
        }

        for agent in &mut agents {
            agent.permissions.push(rule("browser", "*", PermissionEffect::Deny));
            agent.permissions.push(rule("skill", "browser", PermissionEffect::Deny));
        }

        let default = documents.iter().rev().find_map(|info| info.default_agent.clone());
        Self { agents, default }
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let selected = self.selected_default();
        let mut agents = selected.into_iter().cloned().collect::<Vec<_>>();
        agents.extend(
            self.agents.iter().filter(|agent| selected.is_none_or(|selected| selected.id != agent.id)).cloned(),
        );
        agents
    }

    pub fn select(&self, name: Option<&str>) -> Selection {
        if let Some(name) = name {
            return Selection {
                id: name.to_owned(),
                info: self.agents.iter().find(|agent| agent.id.0 == name).cloned(),
            };
        }
        let selected = self.selected_default();
        Selection { id: selected.map_or_else(|| BUILD.to_owned(), |agent| agent.id.0.clone()), info: selected.cloned() }
    }

    fn selected_default(&self) -> Option<&AgentInfo> {
        let configured = self.default.as_ref().and_then(|id| self.find(id)).filter(|agent| selectable(agent));
        configured
            .or_else(|| self.find(BUILD).filter(|agent| selectable(agent)))
            .or_else(|| self.agents.iter().find(|agent| selectable(agent)))
    }

    fn find(&self, id: &str) -> Option<&AgentInfo> {
        self.agents.iter().find(|agent| agent.id.0 == id)
    }
}

fn selectable(agent: &AgentInfo) -> bool {
    agent.mode != AgentInfoMode::Subagent && !agent.hidden
}

fn builtin(id: &str, home: &str) -> AgentInfo {
    let mut info = default_info(id);
    match id {
        "build" => {
            info.name = "Build".to_owned();
            info.description = Some(BUILD_DESCRIPTION.to_owned());
            info.mode = AgentInfoMode::Primary;
            info.permissions.push(rule("question", "*", PermissionEffect::Allow));
        }
        "general" => {
            info.name = "General".to_owned();
            info.description = Some(GENERAL_DESCRIPTION.to_owned());
            info.mode = AgentInfoMode::Subagent;
            info.permissions.push(rule("question", "*", PermissionEffect::Deny));
            info.permissions.push(rule("subagent", "*", PermissionEffect::Deny));
        }
        "explore" => {
            info.name = "Explore".to_owned();
            info.description = Some(EXPLORE_DESCRIPTION.to_owned());
            info.system = Some(PROMPT_EXPLORE.to_owned());
            info.mode = AgentInfoMode::Subagent;
            info.permissions.extend([
                rule("*", "*", PermissionEffect::Deny),
                rule("shell", "*", PermissionEffect::Allow),
                rule("grep", "*", PermissionEffect::Allow),
                rule("glob", "*", PermissionEffect::Allow),
                rule("webfetch", "*", PermissionEffect::Allow),
                rule("websearch", "*", PermissionEffect::Allow),
                rule("read", "*", PermissionEffect::Allow),
                rule("read", "*.env", PermissionEffect::Ask),
                rule("read", "*.env.*", PermissionEffect::Ask),
                rule("read", "*.env.example", PermissionEffect::Allow),
                rule("subagent", "*", PermissionEffect::Deny),
                rule("external_directory", "*", PermissionEffect::Allow),
            ]);
        }
        "compaction" => {
            info.name = "Compaction".to_owned();
            info.mode = AgentInfoMode::Primary;
            info.hidden = true;
        }
        "title" => {
            info.name = "Title".to_owned();
            info.mode = AgentInfoMode::Primary;
            info.hidden = true;
            info.system = Some(PROMPT_TITLE.to_owned());
            info.permissions.push(rule("*", "*", PermissionEffect::Deny));
        }
        "summary" => {
            info.name = "Summary".to_owned();
            info.mode = AgentInfoMode::Primary;
            info.hidden = true;
            info.system = Some(PROMPT_SUMMARY.to_owned());
            info.permissions.push(rule("*", "*", PermissionEffect::Deny));
        }
        "plan" => {
            info.name = "Plan".to_owned();
            info.description = Some(PLAN_DESCRIPTION.to_owned());
            info.mode = AgentInfoMode::Primary;
            info.permissions.push(rule("question", "*", PermissionEffect::Allow));
            info.permissions.push(rule("edit", "*", PermissionEffect::Deny));
            let directory = append_path(&append_path(home, ".opencode"), "plan");
            info.permissions.push(rule("edit", &append_path(&directory, "*"), PermissionEffect::Allow));
        }
        _ => {}
    }
    info
}

fn default_info(id: &str) -> AgentInfo {
    AgentInfo {
        id: AgentID(id.to_owned()),
        name: id.to_owned(),
        model: None,
        request: default_request(),
        system: None,
        instructions: None,
        description: None,
        mode: AgentInfoMode::Primary,
        hidden: false,
        color: None,
        steps: None,
        permissions: base_permissions(),
    }
}

fn default_request() -> ProviderRequest {
    decode_json(r#"{"settings":{},"headers":{},"body":{}}"#).expect("an empty provider request decodes")
}

fn base_permissions() -> PermissionRuleset {
    vec![
        rule("*", "*", PermissionEffect::Allow),
        rule("read", "*.env", PermissionEffect::Ask),
        rule("read", "*.env.*", PermissionEffect::Ask),
        rule("read", "*.env.example", PermissionEffect::Allow),
    ]
}

fn apply_config(agent: &mut AgentInfo, item: &ConfigEntryDocumentInfoAgentsValue, home: &str) {
    if let Some(model) = &item.model {
        agent.model = Some(model_ref(model));
    }
    if let Some(request) = &item.request {
        if let Some(headers) = &request.headers {
            merge(&mut agent.request.headers, headers);
        }
        if let Some(body) = &request.body {
            merge(&mut agent.request.body, body);
        }
    }
    if let Some(system) = &item.system {
        agent.system = Some(system.clone());
    }
    if let Some(description) = &item.description {
        agent.description = Some(description.clone());
    }
    if let Some(mode) = &item.mode {
        agent.mode = mode_of(mode);
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
    if let Some(permissions) = &item.permissions {
        agent.permissions.extend(expand_rules(permissions.iter().cloned(), home));
    }
}

fn mode_of(mode: &ConfigEntryDocumentInfoAgentsValueMode) -> AgentInfoMode {
    match mode {
        ConfigEntryDocumentInfoAgentsValueMode::Subagent => AgentInfoMode::Subagent,
        ConfigEntryDocumentInfoAgentsValueMode::Primary => AgentInfoMode::Primary,
        ConfigEntryDocumentInfoAgentsValueMode::All => AgentInfoMode::All,
    }
}

fn model_ref(model: &ConfigEntryDocumentInfoAgentsValueModel) -> ModelRef {
    match model {
        ConfigEntryDocumentInfoAgentsValueModel::String(value) => {
            let (provider_id, rest) = value.0.split_once('/').expect("a config model reference carries a provider");
            let (id, variant) = match rest.split_once('#') {
                Some((id, variant)) => (id, Some(variant.to_owned())),
                None => (rest, None),
            };
            ModelRef { id: id.to_owned(), provider_id: ProviderID(provider_id.to_owned()), variant }
        }
        ConfigEntryDocumentInfoAgentsValueModel::Object(value) => ModelRef {
            id: value.model.clone(),
            provider_id: ProviderID(value.provider_id.clone()),
            variant: value.variant.clone(),
        },
    }
}

fn merge<T: Clone>(target: &mut JsonMap<T>, additions: &JsonMap<T>) {
    for (key, value) in &additions.0 {
        match target.0.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, existing)) => *existing = value.clone(),
            None => target.0.push((key.clone(), value.clone())),
        }
    }
}

fn expand_rules(rules: impl IntoIterator<Item = PermissionRule>, home: &str) -> PermissionRuleset {
    rules
        .into_iter()
        .map(|rule| {
            if PATH_ACTIONS.contains(&rule.action.as_str()) {
                PermissionRule { resource: expand_home(&rule.resource, home), ..rule }
            } else {
                rule
            }
        })
        .collect()
}

fn expand_home(resource: &str, home: &str) -> String {
    if resource == "~" || resource == "$HOME" {
        return home.to_owned();
    }
    let relative = resource
        .strip_prefix("~/")
        .or_else(|| resource.strip_prefix("$HOME/"))
        .or_else(|| resource.strip_prefix("$HOME\\"));
    match relative {
        Some("") | None => resource.to_owned(),
        Some(relative) => append_path(home, relative),
    }
}

fn append_path(base: &str, name: &str) -> String {
    if name.is_empty() {
        return base.trim_end_matches(['/', '\\']).to_owned();
    }
    let separator = if base.starts_with('/') { '/' } else { '\\' };
    format!("{}{separator}{name}", base.trim_end_matches(['/', '\\']))
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_owned(), resource: resource.to_owned(), effect }
}
