use std::sync::Arc;

use serde_json::{Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::protocols::openai_freeform;
use opencode_ai::route::compile::{Compiled, StreamOptions, compile};
use opencode_ai::route::model::{Model, ProtocolRegistry};
use opencode_ai::schema::{
    AssistantMessage, ContentPart, Freeform, LlmEvent, LlmRequest, LlmResponse, Message, ProviderMetadata,
    ResponseFold, TextPart, ToolCallPart, ToolDefinition, ToolEntry, UserMessage,
};

const LINE: &str = "// @options: { \"background\": true, \"timeout\": 5 }";

fn context() -> ExecutionContext {
    ExecutionContext::new(
        Arc::new(FixedClock::new(1_767_323_045_000)),
        Arc::new(SequenceIds::new()),
        Arc::new(MapEnv::default()),
    )
}

fn options_tool() -> ToolDefinition {
    ToolDefinition {
        name: "run".to_string(),
        description: "Run code".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "timeout": { "type": "integer" },
                "background": { "type": "boolean" }
            },
            "required": ["code"]
        }),
        output_schema: None,
        freeform: Some(Freeform::Text { field: Some("code".to_string()) }),
        cache: None,
        native: None,
    }
}

fn options_entry() -> ToolEntry {
    ToolEntry::Tool(options_tool())
}

fn raw() -> String {
    format!("{LINE}\nawait build()")
}

fn input() -> Value {
    json!({ "code": "await build()", "background": true, "timeout": 5 })
}

fn model(descriptor: Value) -> Model {
    let descriptor = serde_json::from_value(descriptor).expect("model descriptor");
    Model::from_descriptor(descriptor, &ProtocolRegistry::builtin()).expect("model")
}

fn responses_model() -> Model {
    model(json!({
        "id": "gpt-5.1",
        "provider": "openai",
        "route": { "id": "openai-responses", "protocol": "openai-responses", "providerMetadataKey": "openai" },
        "endpoint": { "baseURL": "https://api.openai.com/v1", "path": "/responses" },
        "transport": "http",
        "auth": { "type": "bearer", "value": "test" }
    }))
}

fn chat_model() -> Model {
    model(json!({
        "id": "gpt-5.1",
        "provider": "openai",
        "route": { "id": "openai-chat", "protocol": "openai-chat", "providerMetadataKey": "openai" },
        "endpoint": { "baseURL": "https://api.openai.com/v1", "path": "/chat/completions" },
        "transport": "http",
        "auth": { "type": "bearer", "value": "test" }
    }))
}

fn anthropic_model() -> Model {
    model(json!({
        "id": "claude-sonnet-4-5",
        "provider": "anthropic",
        "route": {
            "id": "anthropic-messages",
            "protocol": "anthropic-messages",
            "providerMetadataKey": "anthropic"
        },
        "endpoint": { "baseURL": "https://api.anthropic.com/v1", "path": "/messages" },
        "transport": "http",
        "auth": { "type": "header", "name": "x-api-key", "value": "test" }
    }))
}

fn request(tools: Vec<ToolEntry>, messages: Vec<Message>) -> LlmRequest {
    LlmRequest {
        id: None,
        system: Vec::new(),
        messages,
        tools,
        tool_choice: None,
        generation: Default::default(),
        cache: None,
        prompt_cache_key: None,
        include_raw: None,
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    }
}

fn run_request(tools: Vec<ToolEntry>) -> LlmRequest {
    let content =
        vec![ContentPart::Text(TextPart { text: "Run it.".to_string(), cache: None, provider_metadata: None })];
    request(tools, vec![Message::User(UserMessage { id: None, content, provider_metadata: None })])
}

fn compile_request(model: &Model, request: LlmRequest) -> Compiled {
    compile(model, request, &StreamOptions::default(), &context()).expect("compile")
}

fn folded(model: &Model, frames: &[Value]) -> LlmResponse {
    let compiled = compile_request(model, run_request(vec![options_entry()]));
    let context = context();
    let mut state = compiled.resolved.model.route.protocol.new_state(&compiled.resolved, &context);
    let mut fold = ResponseFold::new();
    for frame in frames {
        for event in state.step(frame.to_string()).expect("stream frame") {
            fold.push(event);
        }
    }
    for event in state.halt().expect("stream end") {
        fold.push(event);
    }
    fold.finish()
}

fn tool_calls(response: &LlmResponse) -> Vec<&ToolCallPart> {
    response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolCall(part) => Some(part),
            _ => None,
        })
        .collect()
}

#[test]
fn sends_a_tool_with_other_parameters_with_a_grammar_and_a_description_of_the_line() {
    let responses = responses_model();
    let sent = compile_request(&responses, run_request(vec![options_entry()])).body["tools"][0].clone();
    assert_eq!(sent["type"], json!("custom"));
    assert_eq!(sent["name"], json!("run"));
    let description = sent["description"].as_str().expect("description").to_string();
    assert!(
        description.starts_with("Run code\n\nTo set the parameters below, start with a `// @options:`"),
        "{description}"
    );
    assert_eq!(sent["format"]["type"], json!("grammar"));
    assert_eq!(sent["format"]["syntax"], json!("lark"));
    let grammar = sent["format"]["definition"].as_str().expect("grammar definition");
    assert!(grammar.contains(r#"| "\"background\"" " "* ":" " "* ("true" | "false")"#), "{grammar}");

    let chat = compile_request(&chat_model(), run_request(vec![options_entry()])).body;
    assert_eq!(chat["tools"][0]["type"], json!("custom"));
    assert_eq!(chat["tools"][0]["custom"]["description"], json!(description));
    assert_eq!(
        chat["tools"][0]["custom"]["format"],
        json!({ "type": "grammar", "grammar": { "syntax": "lark", "definition": grammar } })
    );

    let mut text = responses.clone();
    text.capabilities = Some(json!({ "tools": { "freeform": ["text"] } }));
    let mut expected = sent.clone();
    expected["format"] = json!({ "type": "text" });
    assert_eq!(compile_request(&text, run_request(vec![options_entry()])).body["tools"], json!([expected]));

    let anthropic = compile_request(&anthropic_model(), run_request(vec![options_entry()])).body;
    let tool = &anthropic["tools"][0];
    assert_eq!(tool["name"], json!("run"));
    assert_eq!(tool["description"], json!("Run code"));
    let properties = &tool["input_schema"]["properties"];
    assert!(properties.get("background").is_some(), "{tool}");
    assert!(properties.get("timeout").is_some(), "{tool}");
    assert_ne!(tool["type"], json!("custom"));
}

#[test]
fn reads_the_line_on_responses_and_replays_it_exactly_as_written() {
    let model = responses_model();
    let response = folded(
        &model,
        &[
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "run", "input": "" }
            }),
            json!({
                "type": "response.custom_tool_call_input.delta",
                "output_index": 0,
                "item_id": "ctc_1",
                "delta": format!("{LINE}\n")
            }),
            json!({
                "type": "response.custom_tool_call_input.delta",
                "output_index": 0,
                "item_id": "ctc_1",
                "delta": "await build()"
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "run", "input": raw() }
            }),
            json!({ "type": "response.completed", "response": { "id": "resp_1" } }),
        ],
    );
    let calls = tool_calls(&response);
    assert_eq!(calls.len(), 1, "{:?}", response.message.content);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].input, input());
    let expected: ProviderMetadata =
        serde_json::from_value(json!({ "openai": { "itemId": "ctc_1", "custom": true, "header": LINE } }))
            .expect("provider metadata");
    assert_eq!(calls[0].provider_metadata, Some(expected));

    let replay =
        compile_request(&model, request(vec![options_entry()], vec![Message::Assistant(response.message.clone())]));
    assert_eq!(replay.body["input"][0]["type"], json!("custom_tool_call"));
    assert_eq!(replay.body["input"][0]["call_id"], json!("call_1"));
    assert_eq!(replay.body["input"][0]["input"], json!(raw()));

    let mut edited = calls[0].clone();
    edited.input = json!({ "code": "await build()", "background": true, "timeout": 6 });
    let replay = compile_request(
        &model,
        request(
            vec![options_entry()],
            vec![Message::Assistant(AssistantMessage {
                content: vec![ContentPart::ToolCall(edited)],
                ..Default::default()
            })],
        ),
    );
    assert_eq!(
        replay.body["input"][0]["input"],
        json!("// @options: {\"timeout\":6,\"background\":true}\nawait build()")
    );
}

#[test]
fn reads_the_line_on_chat_and_replays_it_exactly_as_written() {
    let model = chat_model();
    let response = folded(
        &model,
        &[
            json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "type": "custom",
                            "custom": { "name": "run", "input": format!("{LINE}\n") }
                        }]
                    },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": { "tool_calls": [{ "index": 0, "custom": { "input": "await build()" } }] },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_1",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
            }),
        ],
    );
    let calls = tool_calls(&response);
    assert_eq!(calls.len(), 1, "{:?}", response.message.content);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].input, input());

    let replay =
        compile_request(&model, request(vec![options_entry()], vec![Message::Assistant(response.message.clone())]));
    assert_eq!(replay.body["messages"][0]["tool_calls"][0]["type"], json!("custom"));
    assert_eq!(replay.body["messages"][0]["tool_calls"][0]["custom"]["input"], json!(raw()));
}

#[test]
fn finishes_a_custom_call_that_only_the_completed_response_ends() {
    let response = folded(
        &responses_model(),
        &[
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "run", "input": "" }
            }),
            json!({
                "type": "response.custom_tool_call_input.delta",
                "output_index": 0,
                "item_id": "ctc_1",
                "delta": raw()
            }),
            json!({ "type": "response.completed", "response": { "id": "resp_1" } }),
        ],
    );
    let calls = tool_calls(&response);
    assert_eq!(calls.len(), 1, "{:?}", response.message.content);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].input, input());
}

#[test]
fn fails_a_call_whose_line_is_not_a_json_object_as_malformed_input() {
    let malformed = "// @options: {background: true}\nawait build()";
    let response = folded(
        &responses_model(),
        &[
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "run", "input": malformed }
            }),
            json!({ "type": "response.completed", "response": { "id": "resp_1" } }),
        ],
    );
    assert!(tool_calls(&response).is_empty(), "{:?}", response.message.content);
    let errors = response
        .events
        .iter()
        .filter_map(|event| match event {
            LlmEvent::ToolInputError { id, raw, .. } => Some((id.as_str(), raw.as_str())),
            _ => None,
        })
        .collect::<Vec<(&str, &str)>>();
    assert_eq!(errors, vec![("call_1", malformed)]);
}

#[test]
fn rejects_a_named_field_that_is_not_a_required_string() {
    let definition = ToolDefinition {
        input_schema: json!({
            "type": "object",
            "properties": { "code": { "type": "string" }, "lang": { "type": "string" } },
            "required": ["code", "lang"]
        }),
        freeform: Some(Freeform::Text { field: Some("script".to_string()) }),
        ..options_tool()
    };
    assert!(openai_freeform::resolve(&definition, None).is_none());
}
