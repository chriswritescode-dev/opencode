use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::protocols::open_responses_channel;
use opencode_ai::route::auth::Auth;
use opencode_ai::route::compile::{Compiled, StreamOptions, compile};
use opencode_ai::route::endpoint::{Endpoint, PathPart};
use opencode_ai::route::executor::{BoxFuture, PreparedHttp};
use opencode_ai::route::model::{Model, ProtocolRegistry, ResolvedRequest, Route, RouteTransport};
use opencode_ai::route::transport::channel::{ChannelExchange, ChannelExecution, ChannelExecutor};
use opencode_ai::schema::{AiError, AiErrorReason, LlmRequest, StringMap};

pub(crate) fn request(prompt: &str) -> LlmRequest {
    serde_json::from_value(json!({
        "system": [],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": prompt }] }],
        "tools": []
    }))
    .expect("request")
}

pub(crate) fn context() -> ExecutionContext {
    ExecutionContext::new(
        Arc::new(FixedClock::new(1_767_323_045_000)),
        Arc::new(SequenceIds::new()),
        Arc::new(MapEnv::default()),
    )
}

pub(crate) fn compile_model(model: &Model) -> Compiled {
    compile(model, request("Hi"), &StreamOptions::default(), &context()).expect("compile")
}

pub(crate) fn compile_model_with_channel(model: &Model) -> Compiled {
    let options = StreamOptions { web_socket: Some(Arc::new(NoopChannelExecutor)), ..Default::default() };
    compile(model, request("Hi"), &options, &context()).expect("compile")
}

pub(crate) fn headers(prepared: &PreparedHttp) -> BTreeMap<String, String> {
    prepared.headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect()
}

fn model(route: Route) -> Model {
    Model {
        id: "test-model".to_string(),
        provider: "test".to_string(),
        options: route.options.clone(),
        route,
        compatibility: None,
        capabilities: None,
    }
}

fn model_with_capabilities(route: Route, capabilities: Value) -> Model {
    Model { capabilities: Some(capabilities), ..model(route) }
}

fn test_endpoint() -> Endpoint {
    Endpoint { base_url: Some("https://api.test/v1".to_string()), path: PathPart::Default, query: StringMap::new() }
}

fn namespaced_request(prompt: &str, choice: &str) -> LlmRequest {
    serde_json::from_value(json!({
        "system": [],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": prompt }] }],
        "tools": [{
            "type": "namespace",
            "name": "crm",
            "description": "CRM tools",
            "tools": [{ "type": "tool", "name": "lookup", "description": "Look up", "inputSchema": {} }]
        }],
        "toolChoice": { "type": "tool", "name": choice }
    }))
    .expect("request")
}

fn route(protocol_id: &str, id: &str, endpoint: Endpoint) -> Route {
    Route {
        id: id.to_string(),
        protocol: ProtocolRegistry::builtin().get(protocol_id).expect("protocol"),
        provider_metadata_key: None,
        endpoint,
        auth: Auth::None,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: None,
    }
}

#[test]
fn flat_protocols_send_a_namespaced_forced_choice_under_its_flattened_name() {
    for protocol in [
        "anthropic-messages",
        "bedrock-converse",
        "cohere-chat",
        "gemini",
        "google-interactions",
        "mistral-chat",
        "openai-chat",
        "open-responses",
    ] {
        let compiled = compile(
            &model(route(protocol, "test", test_endpoint())),
            namespaced_request("Look up a customer", "crm.lookup"),
            &StreamOptions::default(),
            &context(),
        )
        .expect("compile");
        let body = serde_json::to_string(&compiled.body).expect("body");
        assert!(body.contains("crm_lookup"), "{protocol}: {body}");
        assert!(!body.contains("crm.lookup"), "{protocol}: {body}");
    }
}

#[test]
fn openai_chat_lowers_a_flattened_forced_choice() {
    let model = model(route("openai-chat", "test", test_endpoint()));
    let choose = |name: &str| {
        serde_json::from_value::<LlmRequest>(json!({
            "system": [],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hi" }] }],
            "tools": [
                { "type": "tool", "name": "foo_bar", "description": "Top level", "inputSchema": {} },
                {
                    "type": "namespace",
                    "name": "crm",
                    "description": "CRM tools",
                    "tools": [{ "type": "tool", "name": "customer.lookup", "description": "Look up", "inputSchema": {} }]
                }
            ],
            "toolChoice": { "type": "tool", "name": name }
        }))
        .expect("request")
    };
    let tool_choice = |name: &str| {
        compile(&model, choose(name), &StreamOptions::default(), &context()).expect("compile").body["tool_choice"]
            .clone()
    };

    assert_eq!(
        tool_choice("crm.customer.lookup"),
        json!({ "type": "function", "function": { "name": "crm_customer.lookup" } })
    );
    assert_eq!(tool_choice("foo.bar"), json!({ "type": "function", "function": { "name": "foo.bar" } }));
}

#[test]
fn anthropic_lowers_a_forced_choice_for_a_namespace_a_tool_update_adds() {
    let model = model_with_capabilities(
        route("anthropic-messages", "test", test_endpoint()),
        json!({ "tools": { "updates": ["add", "remove", "replace"] } }),
    );
    let request = serde_json::from_value(json!({
        "system": [],
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "Hi" }] },
            {
                "role": "system",
                "content": [],
                "tools": {
                    "add": [{
                        "type": "namespace",
                        "name": "crm",
                        "description": "CRM tools",
                        "tools": [{ "type": "tool", "name": "lookup", "description": "Look up", "inputSchema": {} }]
                    }],
                    "remove": []
                }
            },
            { "role": "user", "content": [{ "type": "text", "text": "Look up a customer" }] }
        ],
        "tools": [],
        "toolChoice": { "type": "tool", "name": "crm.lookup" }
    }))
    .expect("request");

    let body = compile(&model, request, &StreamOptions::default(), &context()).expect("compile").body;
    assert_eq!(body["tool_choice"], json!({ "type": "tool", "name": "crm_lookup" }));
}

#[test]
fn openai_chat_flattens_namespaced_tool_calls_and_choices_in_the_compiled_body() {
    let request = serde_json::from_value(json!({
        "system": [],
        "tools": [{
            "type": "namespace",
            "name": "crm",
            "description": "CRM tools",
            "tools": [{ "type": "tool", "name": "lookup", "description": "Look up", "inputSchema": {} }]
        }],
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "Look up a customer" }] },
            {
                "role": "assistant",
                "content": [{ "type": "tool-call", "id": "call_1", "name": "lookup", "namespace": "crm", "input": {} }]
            }
        ],
        "toolChoice": { "type": "tool", "name": "crm.lookup" }
    }))
    .expect("request");

    let body =
        compile(&model(route("openai-chat", "test", test_endpoint())), request, &StreamOptions::default(), &context())
            .expect("compile")
            .body;
    assert_eq!(body["tools"][0]["function"]["name"], json!("crm_lookup"));
    assert_eq!(body["messages"][1]["tool_calls"][0]["function"]["name"], json!("crm_lookup"));
    assert!(!serde_json::to_string(&body).expect("body").contains("crm.lookup"));
}

struct NoopChannelExecutor;

impl ChannelExecutor for NoopChannelExecutor {
    fn execute(&self, _exchange: ChannelExchange) -> BoxFuture<Result<ChannelExecution, AiError>> {
        Box::pin(async {
            Err(AiError {
                reason: AiErrorReason::InvalidRequest {
                    message: "channel executor must not run".to_string(),
                    body: None,
                    http: None,
                    parameter: None,
                    classification: None,
                },
            })
        })
    }
}

#[test]
fn path_function_renders_from_the_built_body() {
    let endpoint = Endpoint {
        base_url: Some("https://api.test/v1".to_string()),
        path: PathPart::Function(Arc::new(|body: &Value, _request: &LlmRequest| {
            format!("/models/{}", body.get("model").and_then(Value::as_str).unwrap_or_default())
        })),
        query: StringMap::new(),
    };
    let compiled = compile_model(&model(route("openai-chat", "test", endpoint)));
    assert_eq!(compiled.prepared.request.method, "POST");
    assert_eq!(compiled.prepared.request.url, "https://api.test/v1/models/test-model");
}

#[test]
fn body_transform_rewrites_the_prepared_body() {
    let mut route = route(
        "openai-chat",
        "test",
        Endpoint {
            base_url: Some("https://api.test/v1".to_string()),
            path: PathPart::Static("/chat/completions".to_string()),
            query: StringMap::new(),
        },
    );
    route.body = Some(Arc::new(|mut body: Value, _resolved: &ResolvedRequest, _model: &Model| {
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
        }
        Ok(body)
    }));
    let compiled = compile_model(&model(route));
    let body: Value = serde_json::from_slice(&compiled.prepared.request.body).expect("prepared body");
    assert!(body.get("stream").is_none(), "{body}");
    assert_eq!(body.get("model"), Some(&json!("test-model")));
    assert!(body.get("messages").is_some(), "{body}");
    assert!(compiled.body.get("stream").is_none(), "{}", compiled.body);
}

#[test]
fn header_factory_reaches_the_prepared_request() {
    let mut route = route(
        "openai-chat",
        "test",
        Endpoint {
            base_url: Some("https://api.test/v1".to_string()),
            path: PathPart::Static("/chat/completions".to_string()),
            query: StringMap::new(),
        },
    );
    route.headers = Some(Arc::new(|_request: &LlmRequest| vec![("x-route".to_string(), "one".to_string())]));
    let compiled = compile_model(&model(route));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.method, "POST");
    assert_eq!(prepared.url, "https://api.test/v1/chat/completions");
    let mut expected = BTreeMap::new();
    expected.insert("content-length".to_string(), prepared.body.len().to_string());
    expected.insert("content-type".to_string(), "application/json".to_string());
    expected.insert("x-route".to_string(), "one".to_string());
    assert_eq!(headers(prepared), expected);
}

#[test]
fn route_channel_options_gate_preparation() {
    let endpoint = || Endpoint {
        base_url: Some("https://api.test/v1".to_string()),
        path: PathPart::Static("/responses".to_string()),
        query: StringMap::new(),
    };
    let channel = |enabled: bool| {
        Arc::new(open_responses_channel::Options {
            id: "test".to_string(),
            name: "Test".to_string(),
            rotate_after_ms: Some(1234),
            enabled: Some(Arc::new(move |url: &str| enabled && url.contains("/responses"))),
            url: Some(Arc::new(|_url: &str| "https://api.test/v1/responses".to_string())),
            headers: None,
            continuation: None,
        })
    };
    let options = || StreamOptions { web_socket: Some(Arc::new(NoopChannelExecutor)), ..Default::default() };
    let mut enabled = route("openai-responses", "test", endpoint());
    enabled.channel = Some(channel(true));
    let compiled = compile(&model(enabled), request("Hi"), &options(), &context()).expect("compile");
    let prepared = compiled.channel.expect("channel preparation");
    assert_eq!(prepared.url, "wss://api.test/v1/responses");
    assert_eq!(prepared.rotate_after_ms, Some(1234));
    assert_eq!(compiled.prepared.request.url, "https://api.test/v1/responses");

    let mut disabled = route("openai-responses", "test", endpoint());
    disabled.channel = Some(channel(false));
    let compiled = compile(&model(disabled), request("Hi"), &options(), &context()).expect("compile");
    assert!(compiled.channel.is_none());
    assert_eq!(compiled.prepared.request.url, "https://api.test/v1/responses");
}
