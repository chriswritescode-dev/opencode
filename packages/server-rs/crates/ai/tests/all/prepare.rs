use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::LlmClient;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::prepare::tool_schema::moonshot;
use opencode_ai::prepare::tool_schema::tools;
use opencode_ai::prepare::tool_updates::{ToolUpdateEncoding, ToolUpdateKind};
use opencode_ai::prepare::{
    apply_cache_policy, apply_effort_updates, gemini_json_schema, normalize_tool_history, plan_tool_updates, prepare,
    resolve_effort_updates, resolve_request_options, strip_effort_updates,
};
use opencode_ai::route::auth::Auth;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::endpoint::{Endpoint, PathPart};
use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::framing;
use opencode_ai::route::framing::Definition;
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry, ResolvedRequest, Route, RouteTransport};
use opencode_ai::route::protocol::{Frame, Protocol, ProtocolState};
use opencode_ai::schema::{
    AiError, AiErrorReason, CacheHint, CacheHintType, CacheMessages, CacheMode, CachePolicy, CachePolicyObject,
    CompactionPart, ContentPart, EffortPart, GenerationOptions, HttpOptions, HttpTimeout, LlmEvent, LlmRequest,
    Message, ProviderOptions, RequestOptions, RequestTimeout, SanitizerKind, StringMap, SystemMessage, SystemPart,
    TextPart, ToolCallPart, ToolChanges, ToolContent, ToolDefinition, ToolEntry, ToolMessage, ToolNamespace,
    ToolResultPart, ToolResultValue, TransportOperation, UserMessage, merge_http, merge_provider_options,
};

const ALL: [&str; 3] = ["add", "remove", "replace"];

struct TestProtocol {
    sanitizer: Option<SanitizerKind>,
    effort: bool,
    updates: Option<ToolUpdateEncoding>,
    body: Value,
}

struct NoopState;

impl ProtocolState for NoopState {
    fn step(&mut self, _frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        Ok(Vec::new())
    }

    fn terminal(&self, _frame: &Frame) -> bool {
        false
    }
}

impl Protocol for TestProtocol {
    fn id(&self) -> &str {
        "test"
    }

    fn build_body(
        &self,
        _request: &ResolvedRequest,
        _model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        Ok(self.body.clone())
    }

    fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
        Some(format!("/models/{}/stream", model.id))
    }

    fn framing(&self) -> Definition {
        framing::sse()
    }

    fn new_state(&self, _request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(NoopState)
    }

    fn sanitizer(&self) -> Option<SanitizerKind> {
        self.sanitizer
    }

    fn supports_effort_updates(&self, _model: &Model, _request: &LlmRequest) -> bool {
        self.effort
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        self.updates.clone()
    }
}

fn protocol() -> TestProtocol {
    TestProtocol { sanitizer: None, effort: false, updates: None, body: json!({}) }
}

fn update_protocol(kinds: Option<Vec<ToolUpdateKind>>) -> TestProtocol {
    let updates = kinds.map(|kinds| ToolUpdateEncoding { kinds, placeable: None, declarable: None });
    TestProtocol { updates, ..protocol() }
}

fn model_with_protocol(route_id: &str, id: &str, protocol: Arc<dyn Protocol>) -> Model {
    Model {
        id: id.to_string(),
        provider: "test".to_string(),
        route: Route {
            id: route_id.to_string(),
            protocol,
            provider_metadata_key: None,
            endpoint: Endpoint::new("https://api.test/v1", "/messages"),
            auth: Auth::None,
            headers: None,
            body: None,
            channel: None,
            transport: RouteTransport::Http,
            options: None,
        },
        options: None,
        compatibility: None,
        capabilities: None,
    }
}

fn model(route_id: &str, id: &str, protocol: TestProtocol) -> Model {
    model_with_protocol(route_id, id, Arc::new(protocol))
}

fn with_capabilities(mut model: Model, capabilities: Value) -> Model {
    model.capabilities = Some(capabilities);
    model
}

fn with_compatibility(mut model: Model, compatibility: Value) -> Model {
    model.compatibility = Some(compatibility);
    model
}

fn tool_with_description(name: &str, description: &str) -> ToolEntry {
    ToolEntry::Tool(ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({ "type": "object" }),
        output_schema: None,
        freeform: None,
        cache: None,
        native: None,
    })
}

fn tool(name: &str) -> ToolEntry {
    tool_with_description(name, &format!("{name} tool"))
}

fn namespace(name: &str, tools: Vec<ToolEntry>) -> ToolEntry {
    ToolEntry::Namespace(ToolNamespace { name: name.to_string(), description: None, tools })
}

fn definition_json(name: &str, description: &str) -> Value {
    json!({ "type": "tool", "name": name, "description": description, "inputSchema": { "type": "object" } })
}

fn entry_name(entry: &ToolEntry) -> String {
    match entry {
        ToolEntry::Tool(tool) => tool.name.clone(),
        ToolEntry::Namespace(namespace) => namespace.name.clone(),
    }
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextPart { text: text.to_string(), cache: None, provider_metadata: None })
}

fn text_part_cached(text: &str, cache: CacheHint) -> ContentPart {
    ContentPart::Text(TextPart { text: text.to_string(), cache: Some(cache), provider_metadata: None })
}

fn user(text: &str) -> Message {
    user_parts(vec![text_part(text)])
}

fn user_parts(content: Vec<ContentPart>) -> Message {
    Message::User(UserMessage { id: None, content, provider_metadata: None })
}

fn assistant_text(text: &str) -> Message {
    assistant(vec![text_part(text)])
}

fn assistant(content: Vec<ContentPart>) -> Message {
    Message::Assistant(opencode_ai::schema::AssistantMessage { id: None, content, provider_metadata: None })
}

fn system_text(text: &str) -> Message {
    Message::System(SystemMessage { id: None, content: vec![text_part(text)], tools: None })
}

fn system_parts(content: Vec<ContentPart>) -> Message {
    Message::System(SystemMessage { id: None, content, tools: None })
}

fn system_part(text: &str, cache: Option<CacheHint>) -> SystemPart {
    SystemPart::Text { text: text.to_string(), cache, metadata: None }
}

fn effort_marker(effort: Option<&str>, previous: Option<&str>) -> Message {
    Message::System(SystemMessage {
        id: None,
        content: vec![ContentPart::Effort(EffortPart {
            effort: effort.map(str::to_string),
            previous: previous.map(str::to_string),
        })],
        tools: None,
    })
}

fn tools_marker(add: Vec<ToolEntry>, remove: Vec<String>) -> Message {
    Message::System(SystemMessage { id: None, content: Vec::new(), tools: Some(ToolChanges { add, remove }) })
}

fn tools_marker_with_text(text: &str, add: Vec<ToolEntry>, remove: Vec<String>) -> Message {
    Message::System(SystemMessage {
        id: None,
        content: vec![text_part(text)],
        tools: Some(ToolChanges { add, remove }),
    })
}

fn tool_call(id: &str, name: &str) -> ContentPart {
    ContentPart::ToolCall(ToolCallPart {
        id: id.to_string(),
        name: name.to_string(),
        namespace: None,
        input: json!({}),
        provider_executed: None,
        cache: None,
        provider_metadata: None,
    })
}

fn hosted_call(id: &str, name: &str) -> ContentPart {
    ContentPart::ToolCall(ToolCallPart {
        id: id.to_string(),
        name: name.to_string(),
        namespace: None,
        input: json!({}),
        provider_executed: Some(true),
        cache: None,
        provider_metadata: None,
    })
}

fn tool_result_part(id: &str, name: &str, result: ToolResultValue) -> ContentPart {
    ContentPart::ToolResult(ToolResultPart {
        id: id.to_string(),
        name: name.to_string(),
        namespace: None,
        result,
        provider_executed: None,
        cache: None,
        provider_metadata: None,
    })
}

fn text_result(id: &str, name: &str, value: &str) -> ContentPart {
    tool_result_part(id, name, ToolResultValue::Text { value: json!(value) })
}

fn hosted_result(id: &str, name: &str) -> ContentPart {
    ContentPart::ToolResult(ToolResultPart {
        id: id.to_string(),
        name: name.to_string(),
        namespace: None,
        result: ToolResultValue::Text { value: json!("") },
        provider_executed: Some(true),
        cache: None,
        provider_metadata: None,
    })
}

fn tool_message(content: Vec<ContentPart>) -> Message {
    Message::Tool(ToolMessage { id: None, content })
}

fn contents(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn role(message: &Message) -> &'static str {
    match message {
        Message::System(_) => "system",
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::Tool(_) => "tool",
    }
}

fn value(messages: &[Message]) -> Value {
    serde_json::to_value(messages).expect("messages serialize")
}

fn ephemeral() -> CacheHint {
    CacheHint { r#type: CacheHintType::Ephemeral, ttl: None }
}

fn ephemeral_ttl(ttl: &str) -> CacheHint {
    CacheHint { r#type: CacheHintType::Ephemeral, ttl: Some(ttl.to_string()) }
}

fn tool_cache(entry: &ToolEntry) -> Option<&CacheHint> {
    match entry {
        ToolEntry::Tool(tool) => tool.cache.as_ref(),
        ToolEntry::Namespace(_) => None,
    }
}

fn system_cache(part: &SystemPart) -> Option<&CacheHint> {
    match part {
        SystemPart::Text { cache, .. } => cache.as_ref(),
    }
}

fn part_cache(part: &ContentPart) -> Option<&CacheHint> {
    match part {
        ContentPart::Text(part) => part.cache.as_ref(),
        ContentPart::Media(part) => part.cache.as_ref(),
        ContentPart::ToolCall(part) => part.cache.as_ref(),
        ContentPart::ToolResult(part) => part.cache.as_ref(),
        ContentPart::Reasoning(part) => part.cache.as_ref(),
        ContentPart::Compaction(_) | ContentPart::Effort(_) => None,
    }
}

fn number(value: f64) -> serde_json::Number {
    serde_json::Number::from_f64(value).expect("finite number")
}

fn string_map(pairs: &[(&str, &str)]) -> StringMap {
    StringMap::from_pairs(pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())))
}

fn provider_options(value: Value) -> ProviderOptions {
    value.as_object().expect("provider options object").clone()
}

fn json_object(value: Value) -> Map<String, Value> {
    value.as_object().expect("json object").clone()
}

fn entry_names(request: &LlmRequest) -> Vec<String> {
    request.tools.iter().map(entry_name).collect()
}

fn markers(request: &LlmRequest) -> Vec<(Vec<String>, Vec<String>)> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::System(system) => system
                .tools
                .as_ref()
                .map(|changes| (changes.add.iter().map(entry_name).collect(), changes.remove.clone())),
            _ => None,
        })
        .collect()
}

fn change(add: &[&str], remove: &[&str]) -> (Vec<String>, Vec<String>) {
    (add.iter().map(|name| (*name).to_string()).collect(), remove.iter().map(|name| (*name).to_string()).collect())
}

#[test]
fn tool_history_matches_typescript() {
    let input = vec![
        assistant(vec![tool_call("first", "first"), tool_call("second", "second")]),
        tool_message(vec![text_result("first", "wrong", "done")]),
        user("Continue."),
        assistant(vec![tool_call("trailing", "trailing")]),
    ];
    assert_eq!(
        value(&normalize_tool_history(input)),
        json!([
            {
                "role": "assistant",
                "content": [
                    { "type": "tool-call", "id": "first", "name": "first", "input": {} },
                    { "type": "tool-call", "id": "second", "name": "second", "input": {} }
                ]
            },
            {
                "role": "tool",
                "content": [
                    {
                        "type": "tool-result",
                        "id": "first",
                        "name": "first",
                        "result": { "type": "text", "value": "done" }
                    }
                ]
            },
            {
                "role": "tool",
                "content": [
                    {
                        "type": "tool-result",
                        "id": "second",
                        "name": "second",
                        "result": { "type": "error", "value": "Tool result missing" }
                    }
                ]
            },
            { "role": "user", "content": [{ "type": "text", "text": "Continue." }] },
            {
                "role": "assistant",
                "content": [{ "type": "tool-call", "id": "trailing", "name": "trailing", "input": {} }]
            },
            {
                "role": "tool",
                "content": [
                    {
                        "type": "tool-result",
                        "id": "trailing",
                        "name": "trailing",
                        "result": { "type": "error", "value": "Tool result missing" }
                    }
                ]
            }
        ])
    );

    let calls = assistant(vec![tool_call("answered", "answered"), tool_call("unanswered", "unanswered")]);
    let answered = tool_message(vec![text_result("answered", "answered", "done")]);
    assert_eq!(
        value(&normalize_tool_history(vec![calls.clone(), answered.clone()])),
        json!([
            serde_json::to_value(&calls).expect("calls json"),
            serde_json::to_value(&answered).expect("answered json"),
            {
                "role": "tool",
                "content": [
                    {
                        "type": "tool-result",
                        "id": "unanswered",
                        "name": "unanswered",
                        "result": { "type": "error", "value": "Tool result missing" }
                    }
                ]
            }
        ])
    );

    let hosted = assistant(vec![hosted_call("hosted", "web_search"), hosted_result("hosted", "web_search")]);
    assert_eq!(value(&normalize_tool_history(vec![hosted.clone()])), value(&[hosted]));

    let media =
        ToolContent::File { uri: "data:image/png;base64,AQID".to_string(), mime: "image/png".to_string(), name: None };
    let normalized = normalize_tool_history(vec![
        assistant(vec![
            tool_call("text", "text"),
            tool_call("content", "content"),
            tool_call("error", "error"),
            tool_call("mixed", "mixed"),
            tool_call("whitespace", "whitespace"),
        ]),
        tool_message(vec![text_result("text", "text", "")]),
        tool_message(vec![tool_result_part("content", "content", ToolResultValue::Content { value: Vec::new() })]),
        tool_message(vec![tool_result_part("error", "error", ToolResultValue::Error { value: json!("") })]),
        tool_message(vec![tool_result_part(
            "mixed",
            "mixed",
            ToolResultValue::Content { value: vec![ToolContent::Text { text: String::new() }, media.clone()] },
        )]),
        tool_message(vec![text_result("whitespace", "whitespace", "   ")]),
    ]);
    let results = normalized
        .iter()
        .skip(1)
        .map(|message| serde_json::to_value(&contents(message)[0]).expect("result part json"))
        .collect::<Vec<Value>>();
    assert_eq!(
        results,
        vec![
            json!({ "type": "tool-result", "id": "text", "name": "text", "result": { "type": "text", "value": "(no tool output)" } }),
            json!({ "type": "tool-result", "id": "content", "name": "content", "result": { "type": "text", "value": "(no tool output)" } }),
            json!({ "type": "tool-result", "id": "error", "name": "error", "result": { "type": "error", "value": "(no tool output)" } }),
            json!({ "type": "tool-result", "id": "mixed", "name": "mixed", "result": { "type": "content", "value": [media] } }),
            json!({ "type": "tool-result", "id": "whitespace", "name": "whitespace", "result": { "type": "text", "value": "   " } }),
        ]
    );

    let hosted = assistant(vec![hosted_call("hosted", "web_search"), hosted_result("hosted", "web_search")]);
    let orphan = tool_message(vec![text_result("orphan", "orphan", "ignored")]);
    let unchanged = vec![orphan, hosted];
    assert_eq!(value(&normalize_tool_history(unchanged.clone())), value(&unchanged));

    let normalized = normalize_tool_history(vec![
        assistant(vec![tool_call("call_1", "lookup")]),
        tool_message(vec![tool_result_part("call_1", "wrong", ToolResultValue::Json { value: json!("done") })]),
    ]);
    assert_eq!(
        serde_json::to_value(&contents(&normalized[1])[0]).expect("part json"),
        json!({
            "type": "tool-result",
            "id": "call_1",
            "name": "lookup",
            "result": { "type": "json", "value": "done" }
        })
    );

    let calls = assistant(vec![tool_call("first", "first"), tool_call("second", "second")]);
    let first = tool_message(vec![text_result("first", "first", "one")]);
    let second = tool_message(vec![text_result("second", "second", "two")]);
    let update = system_text("First update.");
    let later = system_text("Second update.");
    let prompt = user("Continue.");
    assert_eq!(
        value(&normalize_tool_history(vec![
            calls.clone(),
            first.clone(),
            update.clone(),
            later.clone(),
            second.clone(),
            prompt.clone()
        ])),
        value(&[calls.clone(), first.clone(), second.clone(), update.clone(), later.clone(), prompt.clone()])
    );
    let missing = tool_message(vec![tool_result_part(
        "first",
        "first",
        ToolResultValue::Error { value: json!("Tool result missing") },
    )]);
    assert_eq!(
        value(&normalize_tool_history(vec![
            assistant(vec![tool_call("first", "first")]),
            update.clone(),
            prompt.clone()
        ])),
        value(&[assistant(vec![tool_call("first", "first")]), missing.clone(), update.clone(), prompt.clone()])
    );
    assert_eq!(
        value(&normalize_tool_history(vec![assistant(vec![tool_call("first", "first")]), update.clone()])),
        value(&[assistant(vec![tool_call("first", "first")]), missing, update])
    );

    let call = assistant(vec![tool_call("first", "first")]);
    let result = tool_message(vec![text_result("first", "first", "one")]);
    let effort = effort_marker(Some("low"), Some("high"));
    assert_eq!(
        value(&normalize_tool_history(vec![call.clone(), effort.clone(), result.clone()])),
        value(&[call, result, effort])
    );

    let history =
        [assistant(vec![tool_call("first", "first")]), tool_message(vec![text_result("first", "first", "one")])];
    let update = system_text("Update.");
    let input = vec![user("Start."), update.clone(), history[0].clone(), history[1].clone(), update, user("Continue.")];
    assert_eq!(value(&normalize_tool_history(input.clone())), value(&input));
}

fn history_request() -> LlmRequest {
    LlmRequest {
        tools: vec![tool("read"), tool("edit")],
        messages: vec![
            user("One."),
            tools_marker(vec![tool("jira")], Vec::new()),
            user("Two."),
            tools_marker(Vec::new(), vec!["edit".to_string()]),
            user("Three."),
            tools_marker(vec![tool("write")], Vec::new()),
            user("Four."),
            tools_marker(vec![tool_with_description("read", "read v2")], Vec::new()),
        ],
        ..LlmRequest::default()
    }
}

fn plan_model(list: Option<&[&str]>) -> Model {
    let model = model(
        "anthropic-messages",
        "claude-opus-5",
        update_protocol(Some(vec![ToolUpdateKind::Add, ToolUpdateKind::Remove, ToolUpdateKind::Replace])),
    );
    match list {
        Some(list) => with_capabilities(model, json!({ "tools": { "updates": list } })),
        None => model,
    }
}

#[test]
fn tool_updates_plan_matches_typescript() {
    let no_in_place = with_capabilities(
        model("anthropic-messages", "gpt-4.1-mini", update_protocol(None)),
        json!({ "tools": { "updates": ALL } }),
    );
    let request = LlmRequest {
        tools: vec![tool("read"), tool("edit"), tool("lookup")],
        messages: vec![
            user("One."),
            tools_marker(vec![tool("write"), tool_with_description("edit", "edit v2")], Vec::new()),
            user("Two."),
            tools_marker(Vec::new(), vec!["lookup".to_string(), "read".to_string()]),
            tools_marker_with_text("Tools changed.", vec![tool_with_description("read", "read v2")], Vec::new()),
        ],
        ..LlmRequest::default()
    };
    let result = plan_tool_updates(&no_in_place, request);
    assert_eq!(
        serde_json::to_value(&result.tools).expect("tools json"),
        json!([
            definition_json("edit", "edit v2"),
            definition_json("write", "write tool"),
            definition_json("read", "read v2")
        ])
    );
    assert_eq!(
        value(&result.messages),
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "One." }] },
            { "role": "user", "content": [{ "type": "text", "text": "Two." }] },
            { "role": "system", "content": [{ "type": "text", "text": "Tools changed." }] }
        ])
    );

    let plain = LlmRequest { tools: vec![tool("read")], messages: vec![user("hi")], ..LlmRequest::default() };
    assert_eq!(plan_tool_updates(&plan_model(Some(&ALL)), plain.clone()), plain);

    struct PlanCase {
        list: Option<Vec<&'static str>>,
        names: Vec<&'static str>,
        kept: Vec<(&'static [&'static str], &'static [&'static str])>,
    }

    let cases = vec![
        PlanCase { list: None, names: vec!["read", "jira", "write"], kept: Vec::new() },
        PlanCase { list: Some(Vec::new()), names: vec!["read", "jira", "write"], kept: Vec::new() },
        PlanCase { list: Some(vec!["add"]), names: vec!["read", "jira", "write"], kept: Vec::new() },
        PlanCase { list: Some(vec!["add", "remove"]), names: vec!["read", "jira", "write"], kept: Vec::new() },
        PlanCase {
            list: Some(vec!["add", "replace"]),
            names: vec!["read", "jira"],
            kept: vec![(&["write"], &[]), (&["read"], &[])],
        },
        PlanCase {
            list: Some(vec!["remove", "replace"]),
            names: vec!["read", "jira", "write"],
            kept: vec![(&["read"], &[])],
        },
        PlanCase {
            list: Some(vec!["add", "remove", "replace"]),
            names: vec!["read", "edit"],
            kept: vec![(&["jira"], &[]), (&[], &["edit"]), (&["write"], &[]), (&["read"], &[])],
        },
    ];

    for case in cases {
        let list = case.list.as_deref();
        let result = plan_tool_updates(&plan_model(list), history_request());
        assert_eq!(entry_names(&result), case.names);
        let kept = case.kept.iter().map(|(add, remove)| change(add, remove)).collect::<Vec<_>>();
        assert_eq!(markers(&result), kept);
    }

    let mut partial = history_request();
    partial.messages.truncate(6);
    let result = plan_tool_updates(&plan_model(Some(&["add"])), partial);
    assert_eq!(entry_names(&result), vec!["read", "jira"]);
    assert_eq!(markers(&result), vec![change(&["write"], &[])]);

    let result = plan_tool_updates(
        &plan_model(Some(&["add", "remove"])),
        LlmRequest {
            tools: vec![tool("read"), tool("edit")],
            messages: vec![
                user("One."),
                tools_marker(Vec::new(), vec!["edit".to_string()]),
                user("Two."),
                tools_marker(vec![tool("edit")], Vec::new()),
            ],
            ..LlmRequest::default()
        },
    );
    assert_eq!(entry_names(&result), vec!["read", "edit"]);
    assert!(markers(&result).is_empty());

    let replacement = LlmRequest {
        tools: vec![tool("read"), tool("edit")],
        messages: vec![
            user("One."),
            tools_marker(vec![tool_with_description("edit", "edit v2")], vec!["edit".to_string()]),
        ],
        ..LlmRequest::default()
    };
    assert_eq!(
        markers(&plan_tool_updates(&plan_model(Some(&["replace"])), replacement.clone())),
        vec![change(&["edit"], &[])]
    );
    assert!(markers(&plan_tool_updates(&plan_model(Some(&["remove"])), replacement)).is_empty());

    let result = plan_tool_updates(
        &plan_model(Some(&ALL)),
        LlmRequest {
            tools: vec![tool("read"), tool("edit")],
            messages: vec![
                user("One."),
                tools_marker(Vec::new(), vec!["ghost".to_string(), "edit".to_string()]),
                tools_marker(Vec::new(), vec!["ghost".to_string()]),
                tools_marker(Vec::new(), Vec::new()),
            ],
            ..LlmRequest::default()
        },
    );
    assert_eq!(entry_names(&result), vec!["read", "edit"]);
    assert_eq!(result.messages.iter().map(role).collect::<Vec<_>>(), vec!["user", "system"]);
    assert_eq!(markers(&result), vec![change(&[], &["edit"])]);

    let result = plan_tool_updates(
        &plan_model(Some(&ALL)),
        LlmRequest {
            tools: vec![tool("read"), tool("edit")],
            messages: vec![tools_marker(vec![tool("jira")], vec!["read".to_string()]), user("One.")],
            ..LlmRequest::default()
        },
    );
    assert_eq!(entry_names(&result), vec!["edit", "jira"]);
    assert_eq!(value(&result.messages), value(&[user("One.")]));

    let plan_namespace = |list: &[&str]| {
        plan_tool_updates(
            &plan_model(Some(list)),
            LlmRequest {
                tools: vec![namespace("jira", vec![tool("read"), tool("edit")])],
                messages: vec![user("One."), tools_marker(vec![namespace("jira", vec![tool("read")])], Vec::new())],
                ..LlmRequest::default()
            },
        )
    };
    assert!(markers(&plan_namespace(&["add", "replace"])).is_empty());
    assert_eq!(markers(&plan_namespace(&ALL)).len(), 1);
}

#[test]
fn effort_updates_strip_and_resolve() {
    let conversation = vec![user("Before."), effort_marker(Some("low"), Some("high")), user("After.")];
    let supported = model("anthropic-messages", "claude-opus-5", TestProtocol { effort: true, ..protocol() });
    let request = LlmRequest { messages: conversation.clone(), ..LlmRequest::default() };
    assert_eq!(apply_effort_updates(&supported, request.clone()), request);

    let unsupported = model("anthropic-messages", "claude-opus-5", TestProtocol { effort: false, ..protocol() });
    assert_eq!(
        apply_effort_updates(&unsupported, request),
        LlmRequest { messages: vec![user("Before."), user("After.")], ..LlmRequest::default() }
    );

    let mixed = system_parts(vec![
        ContentPart::Effort(EffortPart { effort: Some("low".to_string()), previous: Some("high".to_string()) }),
        text_part("Kept."),
    ]);
    let strip_input = LlmRequest {
        messages: vec![user("Start."), effort_marker(Some("low"), Some("high")), mixed.clone(), system_text("Note.")],
        ..LlmRequest::default()
    };
    assert_eq!(strip_effort_updates(strip_input).messages, vec![user("Start."), mixed, system_text("Note.")]);

    let markers = LlmRequest {
        messages: vec![
            user("Before."),
            effort_marker(Some("low"), Some("medium")),
            effort_marker(Some("xhigh"), Some("low")),
            user("After."),
        ],
        ..LlmRequest::default()
    };
    let (resolved, current) = resolve_effort_updates(&markers, Some("xhigh"));
    assert_eq!(resolved, markers);
    assert_eq!(current, Some("medium".to_string()));

    let drifted = LlmRequest {
        messages: vec![user("Before."), effort_marker(Some("low"), Some("high")), user("After.")],
        ..LlmRequest::default()
    };
    let (resolved, current) = resolve_effort_updates(&drifted, Some("high"));
    assert_eq!(resolved, LlmRequest { messages: vec![user("Before."), user("After.")], ..LlmRequest::default() });
    assert_eq!(current, Some("high".to_string()));

    let plain = LlmRequest { messages: vec![user("Before."), user("After.")], ..LlmRequest::default() };
    let (resolved, current) = resolve_effort_updates(&plain, Some("low"));
    assert_eq!(resolved, plain);
    assert_eq!(current, Some("low".to_string()));
}

fn projected_schema(model: &Model, schema: Value) -> Value {
    let entries = tools(
        vec![ToolEntry::Tool(ToolDefinition {
            name: "lookup".to_string(),
            description: "Lookup data.".to_string(),
            input_schema: schema,
            output_schema: None,
            freeform: None,
            cache: None,
            native: None,
        })],
        model,
    );
    match entries.into_iter().next() {
        Some(ToolEntry::Tool(definition)) => definition.input_schema,
        _ => panic!("expected one projected tool"),
    }
}

#[test]
fn tool_schema_projection_matches_typescript() {
    assert_eq!(
        moonshot(&json!({
            "type": "object",
            "properties": {
                "linked": { "$ref": "#/$defs/Linked", "description": "drop me" },
                "tuple": { "type": "array", "items": [{ "type": "string" }, { "type": "number" }] },
                "prefixTuple": { "type": "array", "prefixItems": [{ "type": "boolean" }, { "type": "string" }] },
                "closedTuple": {
                    "type": "array",
                    "prefixItems": [{ "type": "string" }, { "type": "number" }],
                    "items": false
                },
                "restTuple": { "type": "array", "prefixItems": [{ "type": "string" }], "items": { "type": "number" } },
                "prefixItems": { "type": "string" },
                "unevaluatedItems": { "type": "boolean" },
                "items": { "type": "array", "items": { "type": "string" } },
                "cart": {
                    "type": "object",
                    "default": { "items": ["a", "b"], "$ref": "#/literal", "count": 1 },
                    "const": { "enum": ["literal"] },
                    "examples": [{ "items": ["x", "y"] }]
                }
            },
            "dependentRequired": { "items": ["cart"] },
            "dependencies": { "items": ["cart"], "prefixItems": { "required": ["items"] } }
        })),
        json!({
            "type": "object",
            "properties": {
                "linked": { "$ref": "#/$defs/Linked" },
                "tuple": { "type": "array", "items": { "anyOf": [{ "type": "string" }, { "type": "number" }] } },
                "prefixTuple": {
                    "type": "array",
                    "items": { "anyOf": [{ "type": "boolean" }, { "type": "string" }] }
                },
                "closedTuple": {
                    "type": "array",
                    "items": { "anyOf": [{ "type": "string" }, { "type": "number" }] }
                },
                "restTuple": {
                    "type": "array",
                    "items": { "anyOf": [{ "type": "string" }, { "type": "number" }] }
                },
                "prefixItems": { "type": "string" },
                "unevaluatedItems": { "type": "boolean" },
                "items": { "type": "array", "items": { "type": "string" } },
                "cart": {
                    "type": "object",
                    "default": { "items": ["a", "b"], "$ref": "#/literal", "count": 1 },
                    "const": { "enum": ["literal"] },
                    "examples": [{ "items": ["x", "y"] }]
                }
            },
            "dependentRequired": { "items": ["cart"] },
            "dependencies": { "items": ["cart"], "prefixItems": { "required": ["items"] } }
        })
    );

    assert_eq!(
        moonshot(&json!({
            "type": "object",
            "properties": {
                "kind": { "description": "The kind of flag", "enum": ["boolean", "string"] },
                "level": { "enum": [1, 2.5] },
                "optional": { "enum": [null, "a"] },
                "choice": { "anyOf": [{ "enum": [true, false] }, { "type": "null" }] },
                "list": { "type": "array", "items": { "enum": ["x"] } },
                "map": { "type": "object", "additionalProperties": { "enum": ["y"] } },
                "typed": { "type": "string", "enum": ["a", null] },
                "mixed": { "enum": ["a", 1] }
            },
            "$defs": { "Mode": { "enum": ["fast"] } }
        })),
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "description": "The kind of flag", "enum": ["boolean", "string"] },
                "level": { "type": "number", "enum": [1, 2.5] },
                "optional": { "type": ["string", "null"], "enum": [null, "a"] },
                "choice": { "anyOf": [{ "type": "boolean", "enum": [true, false] }, { "type": "null" }] },
                "list": { "type": "array", "items": { "type": "string", "enum": ["x"] } },
                "map": { "type": "object", "additionalProperties": { "type": "string", "enum": ["y"] } },
                "typed": { "type": "string", "enum": ["a", null] },
                "mixed": { "enum": ["a", 1] }
            },
            "$defs": { "Mode": { "type": "string", "enum": ["fast"] } }
        })
    );

    let base = model("openai-chat", "gpt-6-luna", protocol());
    assert_eq!(projected_schema(&base, json!({})), json!({ "type": "object" }));
    assert_eq!(
        projected_schema(&base, json!({ "description": "Query", "properties": { "q": { "type": "string" } } })),
        json!({ "type": "object", "description": "Query", "properties": { "q": { "type": "string" } } })
    );
    let union = json!({
        "anyOf": [
            { "type": "object", "properties": { "a": { "type": "string" } } },
            { "type": "object", "properties": { "b": { "type": "string" } } }
        ]
    });
    assert_eq!(projected_schema(&base, union.clone()), json!({ "type": "object", "anyOf": union["anyOf"].clone() }));
    let object = json!({ "type": "object", "properties": {} });
    assert_eq!(projected_schema(&base, object.clone()), object);
    assert_eq!(projected_schema(&base, json!({ "type": "string" })), json!({ "type": "string" }));

    let original = json!({
        "type": "object",
        "required": ["mode", "missing"],
        "properties": { "mode": { "enum": ["fast", "safe"] } }
    });
    let gemini_expected = json!({
        "type": "object",
        "required": ["mode"],
        "properties": { "mode": { "enum": ["fast", "safe"] } }
    });
    let moonshot_expected = json!({
        "type": "object",
        "required": ["mode", "missing"],
        "properties": { "mode": { "type": "string", "enum": ["fast", "safe"] } }
    });

    let gemini_by_name = model("openai-chat", "google/Gemini-3.8-Flash", protocol());
    assert_eq!(projected_schema(&gemini_by_name, original.clone()), gemini_expected);
    let gemini_by_compatibility =
        with_compatibility(model("openai-chat", "my-tuned-endpoint", protocol()), json!({ "sanitizer": "gemini" }));
    assert_eq!(projected_schema(&gemini_by_compatibility, original.clone()), gemini_expected);
    let moonshot_by_compatibility = with_compatibility(
        model("openai-chat", "google/gemini-3.8-flash", protocol()),
        json!({ "sanitizer": "moonshot" }),
    );
    assert_eq!(projected_schema(&moonshot_by_compatibility, original.clone()), moonshot_expected);
    let moonshot_by_name = model("openai-chat", "moonshotai/Kimi-K3", protocol());
    assert_eq!(projected_schema(&moonshot_by_name, original.clone()), moonshot_expected);
    let none_by_name =
        with_compatibility(model("openai-chat", "google/gemini-3.8-flash", protocol()), json!({ "sanitizer": "none" }));
    assert_eq!(projected_schema(&none_by_name, original.clone()), original);
    let none_on_kimi =
        with_compatibility(model("openai-chat", "moonshotai/Kimi-K3", protocol()), json!({ "sanitizer": "none" }));
    assert_eq!(projected_schema(&none_on_kimi, original.clone()), original);
    assert_eq!(projected_schema(&base, original.clone()), original);

    let route_gemini =
        model("openai-chat", "gpt-6-luna", TestProtocol { sanitizer: Some(SanitizerKind::Gemini), ..protocol() });
    assert_eq!(projected_schema(&route_gemini, original.clone()), gemini_expected);
    let route_moonshot =
        model("openai-chat", "gpt-6-luna", TestProtocol { sanitizer: Some(SanitizerKind::Moonshot), ..protocol() });
    assert_eq!(projected_schema(&route_moonshot, original.clone()), moonshot_expected);
    let compatibility_wins = with_compatibility(
        model(
            "openai-chat",
            "moonshotai/Kimi-K3",
            TestProtocol { sanitizer: Some(SanitizerKind::Moonshot), ..protocol() },
        ),
        json!({ "sanitizer": "gemini" }),
    );
    assert_eq!(projected_schema(&compatibility_wins, original.clone()), gemini_expected);
    let compatibility_disables_route = with_compatibility(route_gemini, json!({ "sanitizer": "none" }));
    assert_eq!(
        projected_schema(&compatibility_disables_route, original),
        json!({
            "type": "object",
            "required": ["mode", "missing"],
            "properties": { "mode": { "enum": ["fast", "safe"] } }
        })
    );
}

#[test]
fn gemini_json_schema_coerces_min_items_like_javascript() {
    let normalized_items = |min_items: Option<Value>| {
        let list = match min_items {
            Some(min_items) => json!({ "type": "array", "minItems": min_items, "items": { "$ref": "#" } }),
            None => json!({ "type": "array", "items": { "$ref": "#" } }),
        };
        gemini_json_schema::normalize(
            &json!({ "type": "object", "required": ["list"], "properties": { "list": list } }),
        )["properties"]["list"]["items"]
            .clone()
    };

    for min_items in [json!(1), json!(2.5), json!("1"), json!("  2 "), json!(true)] {
        assert_eq!(normalized_items(Some(min_items.clone())), json!({}), "minItems {min_items}");
    }
    for min_items in [json!(0), json!(""), json!(-1), json!(false), Value::Null, json!("abc"), json!([]), json!({})] {
        assert_eq!(normalized_items(Some(min_items.clone())), json!({ "$ref": "#" }), "minItems {min_items}");
    }
    assert_eq!(normalized_items(None), json!({ "$ref": "#" }));
}

fn cache_model() -> Model {
    model("anthropic-messages", "claude-sonnet-4-5", protocol())
}

fn auto_cache_request() -> LlmRequest {
    LlmRequest {
        tools: vec![tool("lookup")],
        system: vec![system_part("You are concise.", None)],
        messages: vec![user("hi")],
        cache: Some(CachePolicy::Mode(CacheMode::Auto)),
        ..LlmRequest::default()
    }
}

#[test]
fn cache_policy_direct_matches_typescript() {
    let request = LlmRequest {
        system: vec![
            system_part("Base agent", None),
            system_part("Manual context", Some(ephemeral_ttl("1h"))),
            system_part("Project instructions", None),
        ],
        tools: vec![tool("t1")],
        messages: vec![user("hi")],
        cache: Some(CachePolicy::Mode(CacheMode::Auto)),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    assert_eq!(tool_cache(&applied.tools[0]), Some(&ephemeral()));
    assert_eq!(
        applied.system.iter().map(|part| system_cache(part).is_some()).collect::<Vec<_>>(),
        vec![true, true, true]
    );
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()));
    assert_eq!(system_cache(&applied.system[1]), Some(&ephemeral_ttl("1h")));
    assert_eq!(system_cache(&applied.system[2]), Some(&ephemeral()));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), None);
    assert_eq!(apply_cache_policy(&cache_model(), applied.clone()), applied);

    let request = LlmRequest {
        tools: vec![namespace("crm", vec![tool("lookup"), tool("orders")])],
        cache: Some(CachePolicy::Object(CachePolicyObject { tools: Some(true), ..CachePolicyObject::default() })),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    match &applied.tools[0] {
        ToolEntry::Namespace(namespace) => {
            assert_eq!(tool_cache(&namespace.tools[0]), None);
            assert_eq!(tool_cache(&namespace.tools[1]), Some(&ephemeral()));
        }
        other => panic!("expected namespace, got {other:?}"),
    }

    let compaction = CompactionPart {
        provider: "anthropic".to_string(),
        id: None,
        encrypted: None,
        text: Some(Some("summary".to_string())),
    };
    let mixed = LlmRequest {
        messages: vec![assistant(vec![tool_call("tc_1", "lookup"), ContentPart::Compaction(compaction.clone())])],
        cache: Some(CachePolicy::Object(CachePolicyObject {
            messages: Some(CacheMessages::Named("latest-assistant".to_string())),
            ..CachePolicyObject::default()
        })),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), mixed);
    match &contents(&applied.messages[0])[0] {
        ContentPart::ToolCall(part) => {
            assert_eq!(part.id, "tc_1");
            assert_eq!(part.cache, Some(ephemeral()));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert_eq!(contents(&applied.messages[0])[1], ContentPart::Compaction(compaction.clone()));

    let compaction_only = LlmRequest {
        messages: vec![assistant(vec![ContentPart::Compaction(compaction)])],
        cache: Some(CachePolicy::Object(CachePolicyObject {
            messages: Some(CacheMessages::Named("latest-assistant".to_string())),
            ..CachePolicyObject::default()
        })),
        ..LlmRequest::default()
    };
    assert_eq!(apply_cache_policy(&cache_model(), compaction_only.clone()), compaction_only);

    let none = LlmRequest {
        messages: vec![user("hi")],
        cache: Some(CachePolicy::Mode(CacheMode::None)),
        ..LlmRequest::default()
    };
    assert_eq!(apply_cache_policy(&cache_model(), none.clone()), none);
}

#[test]
fn cache_policy_walks_past_trailing_tools_marker() {
    let marker = tools_marker(vec![tool("write")], vec!["edit".to_string()]);
    let request = LlmRequest {
        tools: vec![tool("read"), tool("edit")],
        messages: vec![user("first"), assistant_text("reply"), user("latest"), marker.clone()],
        cache: Some(CachePolicy::Object(CachePolicyObject {
            messages: Some(CacheMessages::Tail { tail: serde_json::Number::from(1) }),
            ..CachePolicyObject::default()
        })),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    assert_eq!(applied.messages[2], user_parts(vec![text_part_cached("latest", ephemeral())]));
    assert_eq!(applied.messages[3], marker);
}

#[test]
fn cache_policy_skips_routes_without_inline_hint_support() {
    for (route, id) in [("openai-chat", "gpt-4o-mini"), ("gemini", "gemini-2.5-flash")] {
        let request = auto_cache_request();
        assert_eq!(apply_cache_policy(&model(route, id, protocol()), request.clone()), request, "{route} {id}");
    }
}

#[test]
fn cache_policy_alibaba_chat_follows_qwen_heuristics() {
    let qwen = model("alibaba-chat", "qwen3.8-max", protocol());
    let applied = apply_cache_policy(&qwen, auto_cache_request());
    assert_eq!(tool_cache(&applied.tools[0]), None);
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()));

    for id in ["kimi-k3", "glm-5.2", "deepseek-v4-flash-0731", "MiniMax-M2.5"] {
        let request = auto_cache_request();
        assert_eq!(apply_cache_policy(&model("alibaba-chat", id, protocol()), request.clone()), request, "{id}");
    }

    let request = LlmRequest { cache: Some(CachePolicy::Mode(CacheMode::None)), ..auto_cache_request() };
    assert_eq!(apply_cache_policy(&qwen, request.clone()), request);
}

#[test]
fn cache_policy_gateway_routes_select_by_model_family() {
    for route in ["openrouter", "vercel-ai-gateway-messages"] {
        for id in ["anthropic/claude-sonnet-4-6", "~anthropic/claude-sonnet-4-6"] {
            let applied = apply_cache_policy(&model(route, id, protocol()), auto_cache_request());
            assert_eq!(tool_cache(&applied.tools[0]), Some(&ephemeral()), "{route} {id}");
            assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()), "{route} {id}");
            assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()), "{route} {id}");
        }

        let applied = apply_cache_policy(&model(route, "qwen/qwen3.8-max", protocol()), auto_cache_request());
        assert_eq!(tool_cache(&applied.tools[0]), None, "{route} qwen");
        assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()), "{route} qwen");
        assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()), "{route} qwen");

        let request = auto_cache_request();
        assert_eq!(
            apply_cache_policy(&model(route, "openai/gpt-4.1", protocol()), request.clone()),
            request,
            "{route} openai"
        );
    }
}

#[test]
fn cache_policy_compatibility_format_overrides_route_allowlist() {
    let anthropic_format =
        with_compatibility(model("openai-chat", "gpt-4o-mini", protocol()), json!({ "cacheFormat": "anthropic" }));
    let applied = apply_cache_policy(&anthropic_format, auto_cache_request());
    assert_eq!(tool_cache(&applied.tools[0]), Some(&ephemeral()));
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()));

    let alibaba_format =
        with_compatibility(model("openai-chat", "gpt-4o-mini", protocol()), json!({ "cacheFormat": "alibaba" }));
    let applied = apply_cache_policy(&alibaba_format, auto_cache_request());
    assert_eq!(tool_cache(&applied.tools[0]), None);
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()));

    let explicit = LlmRequest {
        cache: Some(CachePolicy::Object(CachePolicyObject {
            tools: Some(true),
            system: Some(true),
            messages: Some(CacheMessages::Tail { tail: serde_json::Number::from(1) }),
            ttl: None,
        })),
        ..auto_cache_request()
    };
    let applied = apply_cache_policy(&alibaba_format, explicit);
    assert_eq!(tool_cache(&applied.tools[0]), None);
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral()));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral()));
}

#[test]
fn cache_policy_stops_at_four_breakpoints_with_manual_hints() {
    let request = LlmRequest {
        tools: vec![tool("t1")],
        system: vec![
            system_part("Base agent", Some(ephemeral())),
            system_part("Manual context", Some(ephemeral_ttl("1h"))),
            system_part("Project instructions", Some(ephemeral())),
        ],
        messages: vec![user("hi")],
        cache: Some(CachePolicy::Mode(CacheMode::Auto)),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    assert_eq!(tool_cache(&applied.tools[0]), Some(&ephemeral()));
    assert_eq!(
        applied.system.iter().map(|part| system_cache(part).is_some()).collect::<Vec<_>>(),
        vec![true, true, true]
    );
    assert_eq!(system_cache(&applied.system[1]), Some(&ephemeral_ttl("1h")));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), None);
    assert_eq!(apply_cache_policy(&cache_model(), applied.clone()), applied);
}

#[test]
fn cache_policy_ttl_only_object_keeps_automatic_placement() {
    let request = LlmRequest {
        cache: Some(CachePolicy::Object(CachePolicyObject {
            ttl: Some("1h".to_string()),
            ..CachePolicyObject::default()
        })),
        ..auto_cache_request()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    assert_eq!(tool_cache(&applied.tools[0]), Some(&ephemeral_ttl("1h")));
    assert_eq!(system_cache(&applied.system[0]), Some(&ephemeral_ttl("1h")));
    assert_eq!(part_cache(&contents(&applied.messages[0])[0]), Some(&ephemeral_ttl("1h")));
}

#[test]
fn cache_policy_messages_tail_two_marks_last_two_boundaries() {
    let request = LlmRequest {
        messages: vec![user("u1"), assistant(vec![text_part("a1")]), user("u2"), assistant(vec![text_part("a2")])],
        cache: Some(CachePolicy::Object(CachePolicyObject {
            messages: Some(CacheMessages::Tail { tail: serde_json::Number::from(2) }),
            ..CachePolicyObject::default()
        })),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    let caches = applied.messages.iter().map(|message| part_cache(&contents(message)[0]).cloned()).collect::<Vec<_>>();
    assert_eq!(caches, vec![None, None, Some(ephemeral()), Some(ephemeral())]);
}

#[test]
fn cache_policy_message_breakpoint_skips_trailing_markers() {
    let latest_assistant = CachePolicy::Object(CachePolicyObject {
        messages: Some(CacheMessages::Named("latest-assistant".to_string())),
        ..CachePolicyObject::default()
    });

    let compaction = CompactionPart {
        provider: "anthropic".to_string(),
        id: None,
        encrypted: None,
        text: Some(Some("summary".to_string())),
    };
    let request = LlmRequest {
        messages: vec![assistant(vec![tool_call("tc_1", "lookup"), ContentPart::Compaction(compaction.clone())])],
        cache: Some(latest_assistant.clone()),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    match &contents(&applied.messages[0])[0] {
        ContentPart::ToolCall(part) => {
            assert_eq!(part.id, "tc_1");
            assert_eq!(part.cache, Some(ephemeral()));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert_eq!(contents(&applied.messages[0])[1], ContentPart::Compaction(compaction));

    let effort = EffortPart { effort: Some("low".to_string()), previous: Some("high".to_string()) };
    let request = LlmRequest {
        messages: vec![assistant(vec![tool_call("tc_2", "lookup"), ContentPart::Effort(effort.clone())])],
        cache: Some(latest_assistant),
        ..LlmRequest::default()
    };
    let applied = apply_cache_policy(&cache_model(), request);
    match &contents(&applied.messages[0])[0] {
        ContentPart::ToolCall(part) => {
            assert_eq!(part.id, "tc_2");
            assert_eq!(part.cache, Some(ephemeral()));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert_eq!(contents(&applied.messages[0])[1], ContentPart::Effort(effort));
}

#[test]
fn cache_policy_prepare_dedupes_tools_before_counting_hints() {
    let mut duplicate = tool("lookup");
    if let ToolEntry::Tool(definition) = &mut duplicate {
        definition.cache = Some(ephemeral());
    }
    let request = LlmRequest {
        tools: vec![duplicate.clone(), duplicate],
        system: vec![
            system_part("Base agent", Some(ephemeral())),
            system_part("Project instructions", Some(ephemeral())),
        ],
        messages: vec![user("Hello")],
        cache: Some(CachePolicy::Mode(CacheMode::Auto)),
        ..LlmRequest::default()
    };
    let prepared = prepare(&cache_model(), request);
    assert_eq!(prepared.tools.len(), 1);
    assert_eq!(tool_cache(&prepared.tools[0]), Some(&ephemeral()));
    assert_eq!(prepared.system.iter().map(|part| system_cache(part).is_some()).collect::<Vec<_>>(), vec![true, true]);
    assert_eq!(part_cache(&contents(&prepared.messages[0])[0]), Some(&ephemeral()));
}

#[test]
fn request_option_precedence_matches_typescript() {
    let route = provider_options(json!({
        "include": ["route"],
        "metadata": { "route": true, "shared": "route" },
        "nullable": "route",
        "primitive": "route"
    }));
    let model_options = provider_options(json!({
        "include": ["model"],
        "metadata": { "model": true, "shared": "model" },
        "nullable": null,
        "primitive": "model"
    }));
    let request_options = provider_options(json!({ "metadata": { "request": true }, "primitive": false }));
    assert_eq!(
        serde_json::to_value(
            merge_provider_options(&[Some(&route), Some(&model_options), Some(&request_options)])
                .expect("merged provider options")
        )
        .expect("provider options json"),
        json!({
            "include": ["model"],
            "metadata": { "route": true, "model": true, "request": true, "shared": "model" },
            "nullable": null,
            "primitive": false
        })
    );

    let model_options = RequestOptions {
        generation: GenerationOptions {
            max_tokens: Some(serde_json::Number::from(20)),
            temperature: Some(number(0.5)),
            frequency_penalty: Some(number(0.25)),
            stop: Some(vec!["model".to_string()]),
            ..GenerationOptions::default()
        },
        provider_options: Some(provider_options(json!({ "reasoningEffort": "medium" }))),
        headers: Some(string_map(&[("x-shared", "model"), ("x-model", "model")])),
        timeout: None,
        http: Some(HttpOptions {
            query: Some(string_map(&[("q", "model"), ("m", "model")])),
            body: Some(json_object(json!({ "nested": { "from": "model" } }))),
            encoding: None,
            tls: None,
        }),
    };
    let mut test_model = model("openai-chat", "gpt-4o-mini", protocol());
    test_model.options = Some(model_options);
    let request = LlmRequest {
        generation: GenerationOptions {
            max_tokens: Some(serde_json::Number::from(30)),
            top_p: Some(number(0.9)),
            stop: Some(vec!["request".to_string()]),
            ..GenerationOptions::default()
        },
        provider_options: Some(provider_options(json!({ "store": true }))),
        headers: Some(string_map(&[("x-shared", "request"), ("x-request", "request")])),
        http: Some(HttpOptions {
            query: Some(string_map(&[("q", "request")])),
            body: Some(json_object(json!({ "nested": { "to": "request" } }))),
            encoding: None,
            tls: None,
        }),
        ..LlmRequest::default()
    };
    assert_eq!(
        resolve_request_options(&test_model, request),
        LlmRequest {
            generation: GenerationOptions {
                max_tokens: Some(serde_json::Number::from(30)),
                temperature: Some(number(0.5)),
                top_p: Some(number(0.9)),
                frequency_penalty: Some(number(0.25)),
                stop: Some(vec!["request".to_string()]),
                ..GenerationOptions::default()
            },
            provider_options: Some(provider_options(json!({
                "reasoningEffort": "medium",
                "store": true
            }))),
            headers: Some(string_map(&[("x-shared", "request"), ("x-model", "model"), ("x-request", "request")])),
            http: Some(HttpOptions {
                query: Some(string_map(&[("q", "request"), ("m", "model")])),
                body: Some(json_object(json!({ "nested": { "from": "model", "to": "request" } }))),
                encoding: None,
                tls: None,
            }),
            ..LlmRequest::default()
        }
    );
}

#[test]
fn empty_object_merges_drop_keys_like_typescript() {
    assert_eq!(opencode_ai::json::deep_merge(&json!({}), &json!({})), None);
    assert_eq!(opencode_ai::json::deep_merge(&json!({ "metadata": {} }), &json!({ "metadata": {} })), Some(json!({})));
    assert_eq!(
        opencode_ai::json::deep_merge(&json!({ "keep": 1, "metadata": {} }), &json!({ "metadata": {} })),
        Some(json!({ "keep": 1 }))
    );
    assert_eq!(
        opencode_ai::json::deep_merge(&json!({ "p": { "a": {} } }), &json!({ "p": { "a": {} } })),
        Some(json!({ "p": {} }))
    );

    let first = provider_options(json!({ "openai": {} }));
    let second = provider_options(json!({ "openai": {} }));
    assert_eq!(merge_provider_options(&[Some(&first), Some(&second)]), Some(provider_options(json!({}))));
    assert_eq!(merge_provider_options(&[Some(&first)]), Some(first));
    let empty = provider_options(json!({}));
    assert_eq!(merge_provider_options(&[Some(&empty), Some(&empty)]), None);

    let http_body = json_object(json!({ "metadata": {} }));
    let http_a = HttpOptions { body: Some(http_body.clone()), ..HttpOptions::default() };
    let http_b = HttpOptions { body: Some(http_body.clone()), ..HttpOptions::default() };
    assert_eq!(merge_http(&[Some(&http_a), Some(&http_b)]).and_then(|http| http.body), Some(json_object(json!({}))));
    assert_eq!(merge_http(&[Some(&http_a)]).and_then(|http| http.body), Some(http_body));
}

struct StalledSender;

impl HttpSender for StalledSender {
    fn send(&self, _request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn client_stream_applies_the_request_timeout() {
    let test_model = model("test", "test-model", TestProtocol { body: json!({ "hello": "world" }), ..protocol() });
    let context =
        ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let executor = RequestExecutor::new(Arc::new(StalledSender), None, context.clock.clone());
    let client = LlmClient::new(executor, context);
    let request = LlmRequest {
        timeout: Some(RequestTimeout::Timeout(HttpTimeout::Millis(serde_json::Number::from(100)))),
        ..LlmRequest::default()
    };
    let mut events = client.stream(&test_model, request, StreamOptions::default());
    let error = match events.next().await {
        Some(Err(error)) => error,
        other => panic!("expected a request timeout, got {other:?}"),
    };
    match error.reason {
        AiErrorReason::Transport { message, operation, code, .. } => {
            assert_eq!(message, "Timed out waiting for response headers");
            assert_eq!(operation, TransportOperation::Request);
            assert_eq!(code.as_deref(), Some("Timeout"));
        }
        other => panic!("expected a transport timeout, got {other:?}"),
    }
}

#[test]
fn compile_builds_prepared_http_without_network() {
    let mut test_model = model("test", "test-model", TestProtocol { body: json!({ "hello": "world" }), ..protocol() });
    test_model.route.auth = Auth::headers(vec![("authorization".to_string(), "Bearer secret".to_string())]);
    test_model.route.endpoint = Endpoint {
        base_url: Some("https://api.test/v1".to_string()),
        path: PathPart::Default,
        query: StringMap::new(),
    };
    let context =
        ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let compiled =
        compile(&test_model, LlmRequest::default(), &StreamOptions::default(), &context).expect("compile succeeds");
    let prepared = compiled.prepared.request;
    assert_eq!(prepared.method, "POST");
    assert_eq!(prepared.url, "https://api.test/v1/models/test-model/stream");
    assert_eq!(String::from_utf8(prepared.body.to_vec()).expect("body is utf8"), "{\"hello\":\"world\"}");
    let header = |name: &str| prepared.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    assert_eq!(header("content-type"), Some("application/json"));
    assert_eq!(header("content-length"), Some(prepared.body.len().to_string().as_str()));
    assert_eq!(header("authorization"), Some("Bearer secret"));
}

#[test]
fn compile_rejects_a_later_local_file_reference() {
    let mut test_model = model("test", "test-model", TestProtocol { body: json!({ "hello": "world" }), ..protocol() });
    test_model.route.endpoint = Endpoint {
        base_url: Some("https://api.test/v1".to_string()),
        path: PathPart::Default,
        query: StringMap::new(),
    };
    let context =
        ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let request = LlmRequest {
        messages: vec![tool_message(vec![tool_result_part(
            "call_1",
            "read",
            ToolResultValue::Content {
                value: vec![
                    ToolContent::File {
                        uri: "https://example.test/a.png".to_string(),
                        mime: "image/png".to_string(),
                        name: None,
                    },
                    ToolContent::File {
                        uri: "file:///etc/passwd".to_string(),
                        mime: "text/plain".to_string(),
                        name: None,
                    },
                ],
            },
        )])],
        ..LlmRequest::default()
    };
    let error = match compile(&test_model, request, &StreamOptions::default(), &context) {
        Ok(_) => panic!("a later local file reference is rejected"),
        Err(error) => error,
    };
    assert_eq!(error.message(), "Local file references must be read with Media.file(path) before sending");
}

#[test]
fn every_corpus_descriptor_requires_protocol() {
    let corpus = crate::corpus::Corpus::load();
    assert_eq!(corpus.cassettes.len(), 255);
    let registry = ProtocolRegistry::new();
    let mut steps = 0usize;
    for cassette in &corpus.cassettes {
        for (index, step) in cassette.steps.iter().enumerate() {
            steps += 1;
            if step.operation.is_some() {
                continue;
            }
            let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone())
                .unwrap_or_else(|error| panic!("{} step {index}: {error}", cassette.name));
            match Model::from_descriptor(descriptor, &registry) {
                Err(error) => assert!(
                    error.message().contains("protocol not registered"),
                    "{} step {index}: {}",
                    cassette.name,
                    error.message()
                ),
                Ok(_) => panic!("{} step {index}: descriptor resolved without a protocol", cassette.name),
            }
        }
    }
    assert!(steps > 0, "corpus has no steps");
}

#[test]
fn surrogates_cannot_reach_rust_strings() {
    assert!(serde_json::from_str::<String>("\"\\ud800\"").is_err());
    assert_eq!(serde_json::from_str::<String>("\"\\ufffd\"").expect("replacement character decodes"), "\u{fffd}");
}

mod client {
    use std::sync::Arc;

    use bytes::Bytes;
    use futures_util::{StreamExt, stream};
    use serde_json::{Value, json};

    use opencode_ai::ExecutionContext;
    use opencode_ai::MapEnv;
    use opencode_ai::clock::FixedClock;
    use opencode_ai::id::SequenceIds;
    use opencode_ai::route::client::LlmClient;
    use opencode_ai::route::compile::StreamOptions;
    use opencode_ai::route::executor::{
        BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor,
    };
    use opencode_ai::route::framing;
    use opencode_ai::route::framing::Definition;
    use opencode_ai::route::model::{Model, ResolvedRequest};
    use opencode_ai::route::protocol::{Frame, Protocol, ProtocolState};
    use opencode_ai::schema::{
        AiError, AiErrorReason, FinishReason, FinishReasonDetails, InvalidProviderOutputClassification, LlmEvent,
        LlmRequest,
    };

    use super::model_with_protocol;

    struct StreamProtocol;

    struct StreamState;

    impl ProtocolState for StreamState {
        fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
            let value: Value = serde_json::from_str(&frame).expect("frame is json");
            let kind = value.get("kind").and_then(Value::as_str).expect("frame kind");
            match kind {
                "text" => Ok(vec![
                    LlmEvent::TextStart { id: "t1".to_string(), provider_metadata: None },
                    LlmEvent::TextDelta {
                        id: "t1".to_string(),
                        text: value.get("text").and_then(Value::as_str).expect("frame text").to_string(),
                        provider_metadata: None,
                    },
                ]),
                "done" => Ok(vec![LlmEvent::Finish {
                    reason: FinishReasonDetails {
                        normalized: FinishReason::Stop,
                        raw: None,
                        category: None,
                        explanation: None,
                    },
                    usage: None,
                    message: None,
                    provider_metadata: None,
                }]),
                other => panic!("unexpected frame kind {other}"),
            }
        }

        fn terminal(&self, frame: &Frame) -> bool {
            serde_json::from_str::<Value>(frame)
                .is_ok_and(|value| value.get("kind").and_then(Value::as_str) == Some("done"))
        }
    }

    impl Protocol for StreamProtocol {
        fn id(&self) -> &str {
            "test-stream"
        }

        fn build_body(
            &self,
            _request: &ResolvedRequest,
            _model: &Model,
            _context: &ExecutionContext,
        ) -> Result<Value, AiError> {
            Ok(json!({}))
        }

        fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
            Some(format!("/models/{}/stream", model.id))
        }

        fn framing(&self) -> Definition {
            framing::sse()
        }

        fn new_state(&self, _request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
            Box::new(StreamState)
        }
    }

    struct SseSender {
        body: &'static str,
    }

    impl HttpSender for SseSender {
        fn send(&self, _request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
            let body: ByteStream = Box::pin(stream::iter(vec![Ok(Bytes::from_static(self.body.as_bytes()))]));
            let response =
                HttpResponse::new(200, vec![("content-type".to_string(), "text/event-stream".to_string())], body);
            Box::pin(async move { Ok(response) })
        }
    }

    fn stream_model() -> Model {
        model_with_protocol("test-stream", "test-model", Arc::new(StreamProtocol))
    }

    fn context() -> ExecutionContext {
        ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()))
    }

    fn client(body: &'static str) -> LlmClient {
        let sender = Arc::new(SseSender { body });
        LlmClient::new(RequestExecutor::new(sender, None, Arc::new(FixedClock::new(0))), context())
    }

    const FULL: &str = "data: {\"kind\":\"text\",\"text\":\"hello\"}\n\ndata: {\"kind\":\"done\"}\n\n";
    const TEXT_ONLY: &str = "data: {\"kind\":\"text\",\"text\":\"hello\"}\n\n";

    fn start() -> LlmEvent {
        LlmEvent::TextStart { id: "t1".to_string(), provider_metadata: None }
    }

    fn delta() -> LlmEvent {
        LlmEvent::TextDelta { id: "t1".to_string(), text: "hello".to_string(), provider_metadata: None }
    }

    fn finish() -> LlmEvent {
        LlmEvent::Finish {
            reason: FinishReasonDetails {
                normalized: FinishReason::Stop,
                raw: None,
                category: None,
                explanation: None,
            },
            usage: None,
            message: None,
            provider_metadata: None,
        }
    }

    #[tokio::test]
    async fn client_stream_and_generate() {
        let full = client(FULL);
        let events = full
            .stream(&stream_model(), LlmRequest::default(), StreamOptions::default())
            .map(|item| item.expect("stream event"))
            .collect::<Vec<LlmEvent>>()
            .await;
        assert_eq!(events, vec![start(), delta(), finish()]);

        let response = full
            .generate(&stream_model(), LlmRequest::default(), StreamOptions::default())
            .await
            .expect("generate succeeds");
        assert_eq!(response.text(), "hello");
        assert_eq!(response.finish_reason.expect("finish reason").normalized, FinishReason::Stop);

        let incomplete = client(TEXT_ONLY)
            .stream(&stream_model(), LlmRequest::default(), StreamOptions::default())
            .collect::<Vec<Result<LlmEvent, AiError>>>()
            .await;
        let error = incomplete.into_iter().find_map(Result::err).expect("incomplete stream errors");
        assert!(error.is_retryable());
        match error.reason {
            AiErrorReason::InvalidProviderOutput { classification, .. } => {
                assert_eq!(classification, Some(InvalidProviderOutputClassification::IncompleteStream))
            }
            other => panic!("expected invalid provider output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn client_raw_events_precede_frames() {
        let request = LlmRequest { include_raw: Some(true), ..LlmRequest::default() };
        let events = client(FULL)
            .stream(&stream_model(), request, StreamOptions::default())
            .map(|item| item.expect("stream event"))
            .collect::<Vec<LlmEvent>>()
            .await;
        assert_eq!(
            events,
            vec![
                LlmEvent::Raw { data: json!({ "kind": "text", "text": "hello" }) },
                start(),
                delta(),
                LlmEvent::Raw { data: json!({ "kind": "done" }) },
                finish(),
            ]
        );
    }
}
