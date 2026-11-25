use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{Decode, decode_json};
use opencode_contract::generated::{
    AgentInfo, AgentInfoMode, CommandInfo, ConfigEntryDocumentInfo, ModelRef, ProviderID, SessionID,
    SessionInboxDelivery,
};
use opencode_registry::agent::default_info;
use opencode_registry::builtin;
use opencode_registry::command::{
    BoxFuture, CommandDefinition, CommandError, CommandExecute, CommandRuntime, CommandService, Invocation, Prompt,
    PromptRequest, SubagentRequest,
};
use opencode_registry::config::command;
use opencode_registry::events::RegistryEvents;
use serde_json::Value;

use crate::TempDir;

type SwitchCall = (SessionID, Option<String>, Option<ModelRef>);

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<&'static str>>,
}

impl RegistryEvents for Recorder {
    fn updated(&self, r#type: &'static str) {
        self.events.lock().expect("the event lock is not poisoned").push(r#type);
    }
}

impl Recorder {
    fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.events.lock().expect("the event lock is not poisoned"))
    }
}

struct Noop;

impl CommandExecute for Noop {
    fn execute<'a>(
        &'a self,
        _invocation: Invocation,
        _runtime: &'a dyn CommandRuntime,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

struct Failing;

impl CommandExecute for Failing {
    fn execute<'a>(
        &'a self,
        _invocation: Invocation,
        _runtime: &'a dyn CommandRuntime,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("kaboom".to_string()) })
    }
}

fn definition(name: &str, description: Option<&str>) -> CommandDefinition {
    CommandDefinition { name: name.to_string(), description: description.map(str::to_string), execute: Arc::new(Noop) }
}

fn invocation(text: &str) -> Invocation {
    Invocation {
        session_id: SessionID("ses_test".to_string()),
        prompt: Prompt { text: text.to_string(), ..Prompt::default() },
        delivery: SessionInboxDelivery::Steer,
    }
}

fn session() -> SessionID {
    SessionID("ses_test".to_string())
}

fn document(json: &str) -> ConfigEntryDocumentInfo {
    decode_json::<ConfigEntryDocumentInfo>(json).expect("the document decodes")
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("the file has a parent")).expect("create the file directory");
    fs::write(path, content).expect("write the file");
}

fn expand(template: &str, input: &str) -> (String, Vec<String>) {
    let parts = command::expand_template(template, input);
    (parts.text, parts.shells)
}

#[derive(Default)]
struct FakeRuntime {
    prompts: Mutex<Vec<PromptRequest>>,
    switches: Mutex<Vec<SwitchCall>>,
    subagents: Mutex<Vec<SubagentRequest>>,
    shells: Mutex<Vec<(String, PathBuf)>>,
    outputs: Mutex<Vec<String>>,
    agents: Mutex<HashMap<String, AgentInfo>>,
    fail: Mutex<Option<String>>,
    shell_fail: Mutex<Option<String>>,
}

impl FakeRuntime {
    fn with_agent(self, id: &str, mode: AgentInfoMode, model: Option<ModelRef>) -> Self {
        let mut info = default_info(id);
        info.mode = mode;
        info.model = model;
        self.agents.lock().expect("the agent lock is not poisoned").insert(id.to_string(), info);
        self
    }

    fn failing(self, message: &str) -> Self {
        *self.fail.lock().expect("the failure lock is not poisoned") = Some(message.to_string());
        self
    }

    fn failing_shell(self, message: &str) -> Self {
        *self.shell_fail.lock().expect("the shell failure lock is not poisoned") = Some(message.to_string());
        self
    }
}

impl CommandRuntime for FakeRuntime {
    fn prompt<'a>(&'a self, request: PromptRequest) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(message) = self.fail.lock().expect("the failure lock is not poisoned").clone() {
                return Err(message);
            }
            self.prompts.lock().expect("the prompt lock is not poisoned").push(request);
            Ok(())
        })
    }

    fn switch_agent_model<'a>(
        &'a self,
        session_id: SessionID,
        agent: Option<String>,
        model: Option<ModelRef>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.switches.lock().expect("the switch lock is not poisoned").push((session_id, agent, model));
            Ok(())
        })
    }

    fn start_subagent<'a>(&'a self, request: SubagentRequest) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.subagents.lock().expect("the subagent lock is not poisoned").push(request);
            Ok(())
        })
    }

    fn run_shell<'a>(&'a self, command: String, cwd: PathBuf) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if let Some(message) = self.shell_fail.lock().expect("the shell failure lock is not poisoned").clone() {
                return Err(message);
            }
            self.shells.lock().expect("the shell lock is not poisoned").push((command, cwd));
            let mut outputs = self.outputs.lock().expect("the output lock is not poisoned");
            if outputs.is_empty() {
                return Ok(String::new());
            }
            Ok(outputs.remove(0))
        })
    }

    fn resolve_agent<'a>(&'a self, id: String) -> BoxFuture<'a, Result<AgentInfo, String>> {
        Box::pin(async move {
            match self.agents.lock().expect("the agent lock is not poisoned").get(&id) {
                Some(info) => Ok(info.clone()),
                None => Err(format!("Agent not found: {id}")),
            }
        })
    }
}

#[tokio::test]
async fn add_replaces_value_and_keeps_first_position() {
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.add(definition("a", Some("first")));
            editor.add(definition("b", Some("bee")));
            editor.add(definition("a", Some("last")));
        })
        .await;

    let list = service.list().await.expect("the state is readable");
    assert_eq!(
        list,
        [
            CommandInfo { name: "a".to_string(), description: Some("last".to_string()) },
            CommandInfo { name: "b".to_string(), description: Some("bee".to_string()) },
        ]
    );

    let a = service.get("a").await.expect("the state is readable").expect("a is listed");
    assert_eq!(a.description.as_deref(), Some("last"));
}

#[tokio::test]
async fn reports_updates_only_when_the_list_changes() {
    let recorder = Arc::new(Recorder::default());
    let service = CommandService::new(recorder.clone());

    let _first = service.transform(|editor| editor.add(definition("a", Some("one")))).await;
    assert_eq!(recorder.take(), ["command.updated"]);

    let _same = service.transform(|editor| editor.add(definition("a", Some("one")))).await;
    assert!(recorder.take().is_empty());

    let _changed = service.transform(|editor| editor.add(definition("a", Some("two")))).await;
    assert_eq!(recorder.take(), ["command.updated"]);

    let _added = service.transform(|editor| editor.add(definition("b", Some("two")))).await;
    assert_eq!(recorder.take(), ["command.updated"]);
}

#[tokio::test]
async fn unknown_command_is_not_found() {
    let service = CommandService::new(Arc::new(Recorder::default()));
    let runtime = FakeRuntime::default();

    let error = service.execute("missing", invocation("hello"), &runtime).await.expect_err("the command is unknown");
    assert_eq!(
        error,
        CommandError::NotFound { command: "missing".to_string(), message: "Command not found: missing".to_string() }
    );
}

#[tokio::test]
async fn maps_execution_failures() {
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.add(CommandDefinition { name: "boom".to_string(), description: None, execute: Arc::new(Failing) });
        })
        .await;
    let runtime = FakeRuntime::default();

    let error = service.execute("boom", invocation("hello"), &runtime).await.expect_err("the command fails");
    assert_eq!(error, CommandError::Execution { command: "boom".to_string(), message: "kaboom".to_string() });
}

#[tokio::test]
async fn surfaces_runtime_failures_as_execution_errors() {
    let temp = TempDir::new();
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"noop":{"template":"Hello"}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;
    let runtime = FakeRuntime::default().failing("no runtime");

    let error = service.execute("noop", invocation("hello"), &runtime).await.expect_err("the runtime fails");
    assert_eq!(error, CommandError::Execution { command: "noop".to_string(), message: "no runtime".to_string() });
}

#[tokio::test]
async fn builtins_set_up_agents_md_and_review() {
    let temp = TempDir::new();
    let directory = temp.path().to_string_lossy().into_owned();
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _builtins = service
        .transform({
            let path = temp.path().to_path_buf();
            move |editor| builtin::command::command(editor, &path)
        })
        .await;

    let list = service.list().await.expect("the state is readable");
    assert_eq!(
        list,
        [
            CommandInfo { name: "init".to_string(), description: Some("guided AGENTS.md setup".to_string()) },
            CommandInfo {
                name: "review".to_string(),
                description: Some("review changes [commit|branch|pr], defaults to uncommitted".to_string()),
            },
        ]
    );

    let runtime = FakeRuntime::default();
    service.execute("init", invocation("focus on rust"), &runtime).await.expect("init executes");
    {
        let prompts = runtime.prompts.lock().expect("the prompt lock is not poisoned");
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].delivery, SessionInboxDelivery::Steer);
        assert!(prompts[0].prompt.text.contains(&directory));
        assert!(prompts[0].prompt.text.contains("focus on rust"));
        assert!(!prompts[0].prompt.text.contains("$ARGUMENTS"));
    }

    service.execute("review", invocation("latest"), &runtime).await.expect("review executes");
    let prompts = runtime.prompts.lock().expect("the prompt lock is not poisoned");
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].prompt.text.contains("latest"));
    assert!(!prompts[1].prompt.text.contains("$ARGUMENTS"));
}

#[tokio::test]
async fn loads_markdown_commands_from_directories() {
    let temp = TempDir::new();
    let root = temp.path().to_path_buf();
    write(&root.join("command/legacy.md"), "Legacy body");
    write(
        &root.join("commands/review.md"),
        "---\ndescription: File review\nagent: reviewer\nmodel: example/chat#high\nsubtask: true\n---\nReview files",
    );
    write(&root.join("commands/nested/docs.md"), "Write docs");
    write(&root.join("commands/empty.md"), "");
    write(&root.join("commands/.hidden.md"), "Hidden body");
    write(&root.join("commands/broken.md"), "---\ndescription: 5\n---\nBroken");
    write(&root.join("commands/ignore.txt"), "ignored");

    let documents = command::load(&[Entry::Directory { path: root.clone() }]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = root.clone();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let list = service.list().await.expect("the state is readable");
    assert_eq!(
        list,
        [
            CommandInfo { name: "legacy".to_string(), description: None },
            CommandInfo { name: ".hidden".to_string(), description: None },
            CommandInfo { name: "empty".to_string(), description: None },
            CommandInfo { name: "nested/docs".to_string(), description: None },
            CommandInfo { name: "review".to_string(), description: Some("File review".to_string()) },
        ]
    );
}

#[tokio::test]
async fn switches_agent_and_model_before_prompting() {
    let temp = TempDir::new();
    let model = ModelRef {
        id: "chat".to_string(),
        provider_id: ProviderID("example".to_string()),
        variant: Some("high".to_string()),
    };
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"probe":{"template":"Hello $ARGUMENTS","agent":"planner"}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let runtime = FakeRuntime::default().with_agent("planner", AgentInfoMode::Primary, Some(model.clone()));
    service.execute("probe", invocation("world"), &runtime).await.expect("the command executes");

    assert_eq!(
        *runtime.switches.lock().expect("the switch lock is not poisoned"),
        [(session(), Some("planner".to_string()), Some(model.clone()))]
    );
    let prompts = runtime.prompts.lock().expect("the prompt lock is not poisoned");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].prompt.text, "Hello world");
    assert_eq!(prompts[0].delivery, SessionInboxDelivery::Steer);
    assert!(runtime.subagents.lock().expect("the subagent lock is not poisoned").is_empty());
}

#[tokio::test]
async fn command_model_overrides_the_agent_model() {
    let temp = TempDir::new();
    let model = ModelRef {
        id: "chat".to_string(),
        provider_id: ProviderID("example".to_string()),
        variant: Some("high".to_string()),
    };
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"modeled":{"template":"Model","model":"example/chat#high"}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let runtime = FakeRuntime::default();
    service.execute("modeled", invocation(""), &runtime).await.expect("the command executes");

    assert_eq!(*runtime.switches.lock().expect("the switch lock is not poisoned"), [(session(), None, Some(model))]);
    let prompts = runtime.prompts.lock().expect("the prompt lock is not poisoned");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].prompt.text, "Model");
}

#[tokio::test]
async fn starts_a_subagent_without_prompting_the_parent() {
    let temp = TempDir::new();
    let model = ModelRef {
        id: "chat".to_string(),
        provider_id: ProviderID("example".to_string()),
        variant: Some("high".to_string()),
    };
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(
            r#"{"commands":{"spawn":{"template":"Do $ARGUMENTS","description":"Spawn","agent":"general","subagent":true}}}"#,
        ),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let runtime = FakeRuntime::default().with_agent("general", AgentInfoMode::Subagent, Some(model.clone()));
    service.execute("spawn", invocation("work"), &runtime).await.expect("the command executes");

    let subagents = runtime.subagents.lock().expect("the subagent lock is not poisoned");
    assert_eq!(subagents.len(), 1);
    let request = &subagents[0];
    assert_eq!(request.title, "Spawn");
    assert_eq!(request.description, "Spawn");
    assert_eq!(request.agent.as_deref(), Some("general"));
    assert_eq!(request.model, Some(model));
    assert_eq!(request.parent_session_id, session());
    assert_eq!(request.prompt.text, "You are a subagent spawned by another session.\nDo work");
    assert!(runtime.prompts.lock().expect("the prompt lock is not poisoned").is_empty());
    assert!(runtime.switches.lock().expect("the switch lock is not poisoned").is_empty());
}

#[tokio::test]
async fn treats_the_legacy_subtask_alias_as_a_subagent() {
    let temp = TempDir::new();
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"legacy":{"template":"Legacy","subtask":true}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let runtime = FakeRuntime::default();
    service.execute("legacy", invocation(""), &runtime).await.expect("the command executes");

    let subagents = runtime.subagents.lock().expect("the subagent lock is not poisoned");
    assert_eq!(subagents.len(), 1);
    assert_eq!(subagents[0].title, "legacy");
    assert_eq!(subagents[0].description, "legacy");
}

#[tokio::test]
async fn interpolates_shell_spans_through_the_runtime() {
    let temp = TempDir::new();
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"shell":{"template":"Run !`printf hi` now. !`printf bye`"}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;

    let runtime = FakeRuntime::default();
    runtime.outputs.lock().expect("the output lock is not poisoned").extend(["HI".to_string(), "BYE".to_string()]);
    service.execute("shell", invocation(""), &runtime).await.expect("the command executes");

    assert_eq!(
        *runtime.shells.lock().expect("the shell lock is not poisoned"),
        [("printf hi".to_string(), temp.path().to_path_buf()), ("printf bye".to_string(), temp.path().to_path_buf()),]
    );
    let prompts = runtime.prompts.lock().expect("the prompt lock is not poisoned");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].prompt.text, "Run HI now. BYE");
}

#[tokio::test]
async fn reports_shell_failures_with_the_interpolation_prefix() {
    let temp = TempDir::new();
    let documents = command::load(&[Entry::Document(Document {
        path: None,
        info: document(r#"{"commands":{"shell":{"template":"Run !`ls` now."}}}"#),
        origin: Origin::File,
    })]);
    let service = CommandService::new(Arc::new(Recorder::default()));
    let _config = service
        .transform({
            let root = temp.path().to_path_buf();
            move |editor| command::apply(&documents, &root, editor)
        })
        .await;
    let runtime = FakeRuntime::default().failing_shell("boom");

    let error = service.execute("shell", invocation(""), &runtime).await.expect_err("the shell fails");
    assert_eq!(
        error,
        CommandError::Execution {
            command: "shell".to_string(),
            message: r#"Shell interpolation failed for "ls": boom"#.to_string(),
        }
    );
}

#[test]
fn expands_arguments_and_appends_without_placeholders() {
    assert_eq!(expand("Explain $ARGUMENTS.", "$& $$").0, "Explain $& $$.");
    assert_eq!(expand("First $1. Rest $2.", "\"alpha beta\" gamma delta").0, "First alpha beta. Rest gamma delta.");
    assert_eq!(expand("Run awk '{print $0}' on $1.", "app.log").0, "Run awk '{print $0}' on app.log.");
    assert_eq!(expand("Explain.", "abc").0, "Explain.\n\nabc");

    let parts = command::expand_template("Run !`echo hi` now.", "");
    assert_eq!(parts.text, "Run !`echo hi` now.");
    assert_eq!(parts.shells, vec!["echo hi"]);
}

#[tokio::test]
async fn commands_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/commands.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        if let Some(templates) = case.get("templates").and_then(Value::as_array) {
            for item in templates {
                let template = item["template"].as_str().expect("template is a string");
                let input = item["input"].as_str().expect("input is a string");
                let expected = item["expected"].as_str().expect("expected is a string");
                let parts = command::expand_template(template, input);
                assert_eq!(parts.text, expected, "case {name}: template");
                assert!(parts.shells.is_empty(), "case {name}: template shells");
            }
            continue;
        }

        let temp = TempDir::new();
        let root = temp.path().join("config");
        fs::create_dir_all(&root).expect("create the config root");
        if let Some(files) = case.get("files").and_then(Value::as_object) {
            for (relative, content) in files {
                let target = root.join(relative);
                fs::create_dir_all(target.parent().expect("the file has a parent"))
                    .expect("create the fixture file directory");
                fs::write(&target, content.as_str().expect("fixture content is a string"))
                    .expect("write the fixture file");
            }
        }

        let mut entries = Vec::new();
        if let Some(documents) = case.get("documents").and_then(Value::as_array) {
            for value in documents {
                let info = ConfigEntryDocumentInfo::decode(value, &mut opencode_contract::codec::Path::new())
                    .expect("config document decodes");
                entries.push(Entry::Document(Document { path: None, info, origin: Origin::File }));
            }
        }
        if case.get("files").is_some() {
            entries.push(Entry::Directory { path: root.clone() });
        }

        let service = CommandService::new(Arc::new(Recorder::default()));
        let _builtins = service
            .transform({
                let root = root.clone();
                move |editor| builtin::command::command(editor, &root)
            })
            .await;
        let documents = command::load(&entries);
        let _config = service
            .transform({
                let root = root.clone();
                move |editor| command::apply(&documents, &root, editor)
            })
            .await;

        let expected = Vec::<CommandInfo>::decode(&case["list"], &mut opencode_contract::codec::Path::new())
            .expect("commands decode");
        assert_eq!(service.list().await.expect("the state is readable"), expected, "case {name}: list");
    }
}
