use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AgentID, PermissionEffect, PermissionID, PermissionReply, PermissionRequest, PermissionRule, PermissionSource,
    PermissionSourceType, SessionID,
};
use opencode_permission::saved::PermissionSaved;
use opencode_permission::service::{
    AskResult, AssertInput, PermissionAgents, PermissionDeps, PermissionError, PermissionEvents, PermissionService,
    PermissionSessions, ReplyInput, SessionRules,
};
use opencode_storage::{Database, DatabaseLocation, project};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct FakeSessions {
    rules: Mutex<HashMap<String, SessionRules>>,
}

impl PermissionSessions for FakeSessions {
    fn get(&self, session_id: &str) -> Result<Option<SessionRules>, String> {
        Ok(self.rules.lock().unwrap().get(session_id).cloned())
    }
}

struct FakeAgents {
    rules: Mutex<HashMap<String, Vec<PermissionRule>>>,
    default: Mutex<Option<String>>,
}

impl PermissionAgents for FakeAgents {
    fn resolve<'a>(&'a self, id: Option<&'a str>) -> BoxFuture<'a, Option<Vec<PermissionRule>>> {
        let selected = match id {
            Some(id) => Some(id.to_string()),
            None => {
                let rules = self.rules.lock().unwrap();
                self.default
                    .lock()
                    .unwrap()
                    .clone()
                    .or_else(|| if rules.contains_key("build") { Some("build".to_string()) } else { None })
            }
        };
        let resolved = selected.and_then(|id| self.rules.lock().unwrap().get(&id).cloned());
        Box::pin(async move { resolved })
    }
}

struct Recorder {
    asked: Mutex<Vec<PermissionRequest>>,
    replied: Mutex<Vec<(PermissionID, PermissionReply)>>,
    tx: mpsc::UnboundedSender<PermissionRequest>,
}

impl PermissionEvents for Recorder {
    fn asked(&self, request: &PermissionRequest) {
        self.asked.lock().unwrap().push(request.clone());
        let _ = self.tx.send(request.clone());
    }

    fn replied(&self, _session_id: &SessionID, request_id: &PermissionID, reply: PermissionReply) {
        self.replied.lock().unwrap().push((request_id.clone(), reply));
    }
}

struct Harness {
    service: Arc<PermissionService>,
    sessions: Arc<FakeSessions>,
    agents: Arc<FakeAgents>,
    recorder: Arc<Recorder>,
    saved: Arc<PermissionSaved>,
    _database: Arc<Database>,
    rx: mpsc::UnboundedReceiver<PermissionRequest>,
}

fn setup(rules: Vec<PermissionRule>) -> Harness {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("opens the database");
    let database = Arc::new(database);
    database.read(|connection| project::upsert(connection, "global", "/project", None)).expect("inserts the project");

    let sessions = Arc::new(FakeSessions { rules: Mutex::new(HashMap::new()) });
    sessions
        .rules
        .lock()
        .unwrap()
        .insert("ses_test".to_string(), SessionRules { agent: Some("test".to_string()), permissions: vec![] });

    let agents = Arc::new(FakeAgents { rules: Mutex::new(HashMap::new()), default: Mutex::new(None) });
    {
        let mut map = agents.rules.lock().unwrap();
        map.insert("test".to_string(), rules);
        map.insert("build".to_string(), vec![]);
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let recorder = Arc::new(Recorder { asked: Mutex::new(vec![]), replied: Mutex::new(vec![]), tx });
    let saved = Arc::new(PermissionSaved::new(Arc::clone(&database)));
    let service = Arc::new(PermissionService::new(PermissionDeps {
        sessions: Arc::clone(&sessions) as Arc<dyn PermissionSessions>,
        agents: Arc::clone(&agents) as Arc<dyn PermissionAgents>,
        saved: Arc::clone(&saved),
        events: Arc::clone(&recorder) as Arc<dyn PermissionEvents>,
        project_id: "global".to_string(),
    }));

    Harness { service, sessions, agents, recorder, saved, _database: database, rx }
}

impl Harness {
    fn set_rules(&self, rules: Vec<PermissionRule>) {
        self.set_agent_rules("test", rules);
    }

    fn set_agent_rules(&self, id: &str, rules: Vec<PermissionRule>) {
        self.agents.rules.lock().unwrap().insert(id.to_string(), rules);
    }

    fn remove_agent(&self, id: &str) {
        self.agents.rules.lock().unwrap().remove(id);
    }

    fn add_session(&self, id: &str, rules: SessionRules) {
        self.sessions.rules.lock().unwrap().insert(id.to_string(), rules);
    }

    fn set_agent_on_session(&self, id: &str, agent: Option<&str>) {
        let mut sessions = self.sessions.rules.lock().unwrap();
        sessions.get_mut(id).expect("the session exists").agent = agent.map(str::to_string);
    }

    fn set_session_permissions(&self, id: &str, permissions: Vec<PermissionRule>) {
        let mut sessions = self.sessions.rules.lock().unwrap();
        sessions.get_mut(id).expect("the session exists").permissions = permissions;
    }

    fn remove_session(&self, id: &str) {
        self.sessions.rules.lock().unwrap().remove(id);
    }

    fn asked(&self) -> Vec<PermissionRequest> {
        self.recorder.asked.lock().unwrap().clone()
    }

    fn replied(&self) -> Vec<(PermissionID, PermissionReply)> {
        self.recorder.replied.lock().unwrap().clone()
    }

    async fn next_request(&mut self) -> PermissionRequest {
        self.rx.recv().await.expect("receives the asked request")
    }

    async fn wait_for_request(
        &mut self,
        input: AssertInput,
    ) -> (JoinHandle<Result<(), PermissionError>>, PermissionRequest) {
        let service = Arc::clone(&self.service);
        let handle = tokio::spawn(async move { service.assert(input).await });
        let request = self.next_request().await;
        (handle, request)
    }

    async fn reply(&self, request_id: &PermissionID, reply: PermissionReply, message: Option<&str>) {
        self.service
            .reply(ReplyInput { request_id: request_id.clone(), reply, message: message.map(str::to_string) })
            .await
            .expect("replies to the request");
    }
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}

fn assertion(id: &str) -> AssertInput {
    AssertInput {
        id: Some(PermissionID(id.to_string())),
        session_id: SessionID("ses_test".to_string()),
        agent: None,
        action: "read".to_string(),
        resources: vec!["src/index.ts".to_string()],
        save: None,
        metadata: None,
        source: None,
        unattended: None,
    }
}

fn source(message_id: &str, id: &str) -> PermissionSource {
    PermissionSource { r#type: PermissionSourceType::Tool, message_id: message_id.to_string(), id: id.to_string() }
}

#[tokio::test]
async fn ask_reports_the_effect_and_only_queues_prompts() {
    let harness = setup(vec![rule("read", "*", PermissionEffect::Allow)]);
    assert_eq!(
        harness.service.ask(assertion("per_test")).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Allow }
    );
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![rule("read", "*", PermissionEffect::Deny)]);
    assert_eq!(
        harness.service.ask(assertion("per_test")).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Deny }
    );
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![]);
    assert_eq!(
        harness.service.ask(assertion("per_test")).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Ask }
    );
    assert!(harness.service.get("per_test").is_some());
}

#[tokio::test]
async fn evaluates_against_an_explicit_agent() {
    let harness = setup(vec![rule("read", "*", PermissionEffect::Allow)]);
    harness.set_agent_rules("reviewer", vec![rule("read", "*", PermissionEffect::Deny)]);

    let mut explicit = assertion("per_test");
    explicit.agent = Some(AgentID("reviewer".to_string()));
    assert_eq!(
        harness.service.ask(explicit).await.expect("asks").effect,
        PermissionEffect::Deny,
        "the explicit agent denies the read"
    );

    harness.set_agent_rules("reviewer", vec![]);
    let mut explicit = assertion("per_test");
    explicit.agent = Some(AgentID("reviewer".to_string()));
    assert_eq!(
        harness.service.ask(explicit).await.expect("asks").effect,
        PermissionEffect::Ask,
        "without rules the read is asked"
    );
}

#[tokio::test]
async fn allows_and_denies_from_explicit_rules_without_asking() {
    let harness = setup(vec![rule("read", "*", PermissionEffect::Allow)]);
    harness.service.assert(assertion("per_test")).await.expect("allows the read");
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![rule("read", "*", PermissionEffect::Deny)]);
    let error = harness.service.assert(assertion("per_test")).await.expect_err("blocks the read");
    assert!(matches!(error, PermissionError::Blocked { .. }));
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn checks_for_a_deny_without_asking() {
    let harness = setup(vec![]);
    harness.service.assert_not_denied(assertion("per_test")).await.expect("does not deny the read");
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![rule("read", "*", PermissionEffect::Deny)]);
    let error = harness.service.assert_not_denied(assertion("per_test")).await.expect_err("reports the deny");
    assert!(matches!(error, PermissionError::Blocked { .. }));
}

#[tokio::test]
async fn reports_whether_a_request_is_already_allowed_without_asking() {
    let harness = setup(vec![]);
    assert!(!harness.service.allows(assertion("per_test")).await.expect("evaluates"));
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![rule("read", "*", PermissionEffect::Allow)]);
    assert!(harness.service.allows(assertion("per_test")).await.expect("evaluates"));

    harness.set_rules(vec![rule("read", "*", PermissionEffect::Deny)]);
    assert!(!harness.service.allows(assertion("per_test")).await.expect("evaluates"));
}

#[tokio::test]
async fn applies_saved_broad_approvals_with_configured_deny_precedence() {
    let harness = setup(vec![rule("read", "gpu:*", PermissionEffect::Ask)]);
    harness.saved.add("global", "read", &["*".to_string()]).expect("adds the saved rule");

    let mut broad = assertion("per_test");
    broad.resources = vec!["gpu:/work/a".to_string()];
    assert_eq!(
        harness.service.ask(broad).await.expect("asks").effect,
        PermissionEffect::Allow,
        "the saved approval allows the machine path"
    );

    harness.set_rules(vec![rule("read", "gpu:*", PermissionEffect::Deny)]);
    let mut broad = assertion("per_test");
    broad.resources = vec!["gpu:/work/a".to_string()];
    assert_eq!(
        harness.service.ask(broad).await.expect("asks").effect,
        PermissionEffect::Deny,
        "the configured deny wins over the saved approval"
    );
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn allows_managed_output_reads_without_external_directory_access() {
    let harness = setup(vec![rule("*", "*", PermissionEffect::Deny), rule("read", "*", PermissionEffect::Allow)]);

    let mut read = assertion("per_test");
    read.resources = vec!["tool_123".to_string()];
    assert_eq!(
        harness.service.ask(read).await.expect("asks").effect,
        PermissionEffect::Allow,
        "the managed output read is allowed"
    );

    let mut external = assertion("per_test");
    external.action = "external_directory".to_string();
    external.resources = vec!["/tmp/tool-output/*".to_string()];
    assert_eq!(
        harness.service.ask(external).await.expect("asks").effect,
        PermissionEffect::Deny,
        "the external directory access is denied"
    );
}

#[tokio::test]
async fn uses_build_permissions_when_the_session_agent_is_omitted() {
    let harness = setup(vec![]);
    harness.set_agent_on_session("ses_test", None);
    harness.set_agent_rules("build", vec![rule("custom", "*", PermissionEffect::Allow)]);

    let mut custom = assertion("per_test");
    custom.action = "custom".to_string();
    custom.resources = vec!["*".to_string()];
    assert_eq!(
        harness.service.ask(custom).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Allow }
    );
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn denies_omitted_agent_permissions_when_no_primary_default_agent_exists() {
    let harness = setup(vec![]);
    harness.set_agent_on_session("ses_test", None);
    harness.remove_agent("test");
    harness.remove_agent("build");

    assert_eq!(
        harness.service.ask(assertion("per_test")).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Deny }
    );
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn evaluates_bash_with_normal_configured_rule_semantics() {
    let harness = setup(vec![rule("*", "*", PermissionEffect::Allow)]);

    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Allow }
    );

    harness.set_rules(vec![]);
    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Ask }
    );
    assert!(harness.service.get("per_test").is_some());
}

#[tokio::test]
async fn merges_session_rules_after_agent_rules_and_before_saved_approvals() {
    let harness = setup(vec![rule("*", "*", PermissionEffect::Allow)]);
    harness.set_session_permissions("ses_test", vec![rule("edit", "/original/**", PermissionEffect::Deny)]);

    let mut edit = assertion("per_test");
    edit.action = "edit".to_string();
    edit.resources = vec!["/original/src/index.ts".to_string()];
    assert_eq!(
        harness.service.ask(edit).await.expect("asks").effect,
        PermissionEffect::Deny,
        "the session deny overrides the agent allow"
    );

    harness.set_rules(vec![]);
    harness.saved.add("global", "bash", &["pwd".to_string()]).expect("adds the saved rule");
    harness.set_session_permissions("ses_test", vec![rule("bash", "*", PermissionEffect::Deny)]);

    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks").effect,
        PermissionEffect::Deny,
        "the session deny overrides the saved approval"
    );

    harness.set_session_permissions("ses_test", vec![rule("bash", "*", PermissionEffect::Ask)]);
    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks").effect,
        PermissionEffect::Allow,
        "the saved approval resolves the session ask"
    );
}

#[tokio::test]
async fn uses_saved_bash_approvals_while_preserving_configured_deny_precedence() {
    let harness = setup(vec![]);
    harness.saved.add("global", "bash", &["pwd".to_string()]).expect("adds the saved rule");

    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Allow }
    );
    assert!(harness.service.list().is_empty());

    harness.set_rules(vec![rule("bash", "*", PermissionEffect::Deny)]);
    let mut bash = assertion("per_test");
    bash.action = "bash".to_string();
    bash.resources = vec!["pwd".to_string()];
    assert_eq!(
        harness.service.ask(bash).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Deny }
    );
}

#[tokio::test]
async fn resolves_an_asked_permission_once() {
    let mut harness = setup(vec![]);
    let (handle, request) = harness.wait_for_request(assertion("per_test")).await;

    assert_eq!(harness.service.list(), vec![request.clone()]);
    assert_eq!(harness.service.for_session("ses_test"), vec![request.clone()]);
    assert!(harness.service.for_session("ses_other").is_empty());
    assert_eq!(harness.service.get(request.id.0.as_str()), Some(request.clone()));

    harness.reply(&request.id, PermissionReply::Once, None).await;
    handle.await.expect("joins the assert").expect("allows the request");

    assert!(harness.service.list().is_empty());
    assert!(harness.service.get(request.id.0.as_str()).is_none());
}

#[tokio::test]
async fn shows_a_nested_calls_request_on_its_parents_row_until_the_scope_closes() {
    let harness = setup(vec![]);
    let child = source("msg_test", "call_child");

    let mut nested = assertion("per_nested");
    nested.source = Some(child.clone());
    let guard = harness.service.nested("call_child", "call_parent");
    harness.service.ask(nested).await.expect("asks");
    drop(guard);

    let mut after = assertion("per_after");
    after.source = Some(child.clone());
    harness.service.ask(after).await.expect("asks");

    let mut parent = child.clone();
    parent.id = "call_parent".to_string();
    assert_eq!(harness.service.get("per_nested").expect("the nested request").source, Some(parent));
    assert_eq!(harness.service.get("per_after").expect("the later request").source, Some(child));
}

#[tokio::test]
async fn drops_null_metadata_values() {
    let mut harness = setup(vec![]);
    let mut input = assertion("per_test");
    input.metadata = Some(JsonMap(vec![
        ("root".to_string(), json!("/project")),
        ("path".to_string(), Value::Null),
        ("hidden".to_string(), Value::Null),
        ("limit".to_string(), Value::Null),
    ]));

    let (handle, request) = harness.wait_for_request(input).await;
    assert_eq!(request.metadata, Some(JsonMap(vec![("root".to_string(), json!("/project"))])));
    assert_eq!(harness.service.for_session("ses_test"), vec![request.clone()]);
    assert_eq!(harness.asked(), vec![request]);
    handle.abort();
}

#[tokio::test]
async fn reports_a_decline_as_its_own_error() {
    let mut harness = setup(vec![]);
    let (handle, request) = harness.wait_for_request(assertion("per_test")).await;

    harness.reply(&request.id, PermissionReply::Reject, None).await;
    let error = handle.await.expect("joins the assert").expect_err("declines the request");
    assert!(matches!(error, PermissionError::Declined));
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn reports_rejection_feedback_as_a_correction() {
    let mut harness = setup(vec![]);
    let (handle, request) = harness.wait_for_request(assertion("per_test")).await;

    harness.reply(&request.id, PermissionReply::Reject, Some("use the docs path")).await;
    let error = handle.await.expect("joins the assert").expect_err("rejects the request");
    assert!(matches!(error, PermissionError::Corrected { feedback } if feedback == "use the docs path"));
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn rejects_sibling_requests_in_the_same_step_and_leaves_another_steps_request_pending() {
    let harness = setup(vec![]);
    let ask = |call: &str, message_id: &str| {
        let mut input = assertion(&format!("per_{call}"));
        input.source = Some(source(message_id, &format!("call_{call}")));
        input
    };

    let first = harness.service.ask(ask("first", "msg_one")).await.expect("asks");
    harness.service.ask(ask("sibling", "msg_one")).await.expect("asks");
    let other = harness.service.ask(ask("other", "msg_two")).await.expect("asks");

    harness.reply(&first.id, PermissionReply::Reject, None).await;

    let remaining = harness.service.list().into_iter().map(|request| request.id).collect::<Vec<_>>();
    assert_eq!(remaining, vec![other.id]);
}

#[tokio::test]
async fn stores_saved_resources_for_a_project_when_replied_always() {
    let mut harness = setup(vec![]);
    let mut input = assertion("per_test");
    input.save = Some(vec!["src/*".to_string()]);

    let (handle, request) = harness.wait_for_request(input).await;
    harness.reply(&request.id, PermissionReply::Always, None).await;
    handle.await.expect("joins the assert").expect("allows the request");

    let rows = harness.saved.list(Some("global")).expect("lists the saved rules");
    assert_eq!(rows.len(), 1, "the reply stores one saved rule");
    assert_eq!(rows[0].action, "read");
    assert_eq!(rows[0].resource, "src/*");

    let mut next = assertion("per_next");
    next.resources = vec!["src/next.ts".to_string()];
    harness.service.assert(next).await.expect("allows the saved resource");

    harness.saved.remove(&rows[0].id.0).expect("removes the saved rule");
    assert!(harness.saved.list(Some("global")).expect("lists the saved rules").is_empty());
}

#[tokio::test]
async fn saved_approvals_resolve_other_pending_requests() {
    let mut harness = setup(vec![]);
    harness.add_session("ses_other", SessionRules { agent: Some("test".to_string()), permissions: vec![] });

    let mut first_input = assertion("per_first");
    first_input.resources = vec!["src/one.ts".to_string()];
    first_input.save = Some(vec!["src/*".to_string()]);
    let (first_handle, first_request) = harness.wait_for_request(first_input).await;

    let mut second_input = assertion("per_other");
    second_input.session_id = SessionID("ses_other".to_string());
    second_input.resources = vec!["src/two.ts".to_string()];
    second_input.source = Some(source("msg_two", "call_other"));
    let (second_handle, second_request) = harness.wait_for_request(second_input).await;

    harness.reply(&first_request.id, PermissionReply::Always, None).await;
    first_handle.await.expect("joins the first assert").expect("allows the first request");
    second_handle.await.expect("joins the second assert").expect("allows the second request");

    assert!(harness.service.list().is_empty());
    assert!(
        harness.replied().iter().any(|(id, reply)| id == &second_request.id && *reply == PermissionReply::Always),
        "the saved approval replied Always for the other pending request"
    );
}

async fn assert_pending_guard(harness: &Harness, other_handle: JoinHandle<Result<(), PermissionError>>) {
    let _ = other_handle.await;
    assert!(harness.service.list().is_empty(), "the cancelled request leaves the queue");
}

#[tokio::test]
async fn skips_pending_auto_approval_after_always_for_a_configured_deny() {
    let mut harness = setup(vec![]);
    harness.add_session("ses_other", SessionRules { agent: Some("test".to_string()), permissions: vec![] });
    harness.set_agent_rules("reviewer", vec![]);

    let mut selected = assertion("per_first");
    selected.resources = vec!["src/one.ts".to_string()];
    selected.save = Some(vec!["src/*".to_string()]);
    let (selected_handle, selected_request) = harness.wait_for_request(selected).await;

    let mut other = assertion("per_other");
    other.session_id = SessionID("ses_other".to_string());
    other.agent = Some(AgentID("reviewer".to_string()));
    let (other_handle, other_request) = harness.wait_for_request(other).await;

    harness.set_agent_rules("reviewer", vec![rule("read", "*", PermissionEffect::Deny)]);
    harness.reply(&selected_request.id, PermissionReply::Always, None).await;
    selected_handle.await.expect("joins the selected assert").expect("allows the selected request");

    assert_eq!(harness.service.list(), vec![other_request]);
    assert!(!other_handle.is_finished(), "the other request stays pending");
    other_handle.abort();
    assert_pending_guard(&harness, other_handle).await;
}

#[tokio::test]
async fn settles_an_ask_with_unattended_rules_when_they_allow() {
    let harness = setup(vec![]);
    let mut input = assertion("per_test");
    input.unattended = Some(vec![rule("read", "*", PermissionEffect::Allow)]);
    assert_eq!(
        harness.service.ask(input).await.expect("asks"),
        AskResult { id: PermissionID("per_test".to_string()), effect: PermissionEffect::Allow }
    );
    assert!(harness.service.list().is_empty());

    let denied = setup(vec![rule("read", "*", PermissionEffect::Deny)]);
    let mut input = assertion("per_again");
    input.unattended = Some(vec![rule("read", "*", PermissionEffect::Allow)]);
    assert_eq!(
        denied.service.ask(input).await.expect("asks"),
        AskResult { id: PermissionID("per_again".to_string()), effect: PermissionEffect::Deny }
    );
    assert!(denied.service.list().is_empty());
}

#[tokio::test]
async fn denies_an_unattended_ask_that_is_not_settled() {
    let harness = setup(vec![]);
    let mut input = assertion("per_test");
    input.unattended = Some(vec![rule("read", "other/*", PermissionEffect::Allow)]);

    let error = harness.service.assert(input).await.expect_err("denies the unsettled ask");
    let PermissionError::Blocked { reason, .. } = error else {
        panic!("the unsettled ask is blocked");
    };
    assert_eq!(reason, Some("Permission read needs approval, and nobody is present to give it".to_string()));
    assert!(harness.service.list().is_empty());
}

#[tokio::test]
async fn skips_pending_auto_approval_after_always_for_a_missing_session() {
    let mut harness = setup(vec![]);
    harness.add_session("ses_other", SessionRules { agent: Some("test".to_string()), permissions: vec![] });
    harness.set_agent_rules("reviewer", vec![]);

    let mut selected = assertion("per_first");
    selected.resources = vec!["src/one.ts".to_string()];
    selected.save = Some(vec!["src/*".to_string()]);
    let (selected_handle, selected_request) = harness.wait_for_request(selected).await;

    let mut other = assertion("per_other");
    other.session_id = SessionID("ses_other".to_string());
    other.agent = Some(AgentID("reviewer".to_string()));
    let (other_handle, other_request) = harness.wait_for_request(other).await;

    harness.remove_session("ses_other");
    harness.reply(&selected_request.id, PermissionReply::Always, None).await;
    selected_handle.await.expect("joins the selected assert").expect("allows the selected request");

    assert_eq!(harness.service.list(), vec![other_request]);
    assert!(!other_handle.is_finished(), "the other request stays pending");
    other_handle.abort();
    assert_pending_guard(&harness, other_handle).await;
}
