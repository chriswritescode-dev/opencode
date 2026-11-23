use opencode_ai::protocols::utils::{
    cache, gemini_generate_content, lifecycle, open_responses_options, partial_json, tool_stream,
};
use opencode_ai::schema::{
    AiErrorReason, CacheHint, CacheHintType, FinishReason, FinishReasonDetails, LlmEvent, LlmRequest, MediaAsset,
    MediaSource, ToolChoice, ToolChoiceType,
};
use serde_json::{Value, json};

#[test]
fn partial_json_parses_incomplete_documents() {
    assert_eq!(partial_json::parse("{\"city"), Some(json!({})));
    assert_eq!(partial_json::parse("{\"city\": \"Paris"), Some(json!({ "city": "Paris" })));
    assert_eq!(partial_json::parse("{\"city\":\"Paris\"}"), Some(json!({ "city": "Paris" })));
    assert_eq!(partial_json::parse(""), None);
    let concatenated = format!("{}{}", "{\"city", "\":\"Paris\"}");
    assert_eq!(partial_json::parse(&concatenated), Some(json!({ "city": "Paris" })));
}

#[test]
fn tool_stream_accumulates_partial_input() {
    let first = tool_stream::append_or_start(
        "test",
        tool_stream::empty(),
        0,
        tool_stream::AppendDelta {
            id: Some("call_1"),
            name: Some("weather"),
            text: "{\"city",
            provider_metadata: None,
        },
        "missing tool",
    )
    .expect("append starts a tool");
    assert_eq!(first.events.len(), 2);
    assert!(matches!(
        &first.events[0],
        LlmEvent::ToolInputStart { id, name, .. } if id == "call_1" && name == "weather"
    ));
    assert!(matches!(
        &first.events[1],
        LlmEvent::ToolInputDelta { text, input: Some(input), .. } if text == "{\"city" && input == &json!({})
    ));

    let second = tool_stream::append_or_start(
        "test",
        first.tools,
        0,
        tool_stream::AppendDelta { id: None, name: None, text: "\":\"Paris\"", provider_metadata: None },
        "missing tool",
    )
    .expect("append extends a tool");
    assert_eq!(second.tool.input, "{\"city\":\"Paris\"");
    assert_eq!(second.events.len(), 1);
    assert!(matches!(
        &second.events[0],
        LlmEvent::ToolInputDelta { text, input: Some(input), .. }
            if text == "\":\"Paris\"" && input == &json!({ "city": "Paris" })
    ));
}

#[test]
fn tool_stream_finish_all_orders_by_key() {
    let tools = start_tool(tool_stream::empty(), 1, "second", "{\"b\":2}");
    let tools = start_tool(tools, 0, "first", "{\"a\":1}");
    let finished = tool_stream::finish_all("test", tools).expect("valid tool input");
    assert!(finished.tools.is_empty());
    assert_eq!(finished.events.len(), 4);
    assert!(matches!(&finished.events[0], LlmEvent::ToolInputEnd { name, .. } if name == "first"));
    assert!(matches!(
        &finished.events[1],
        LlmEvent::ToolCall { name, input, .. } if name == "first" && input == &json!({ "a": 1 })
    ));
    assert!(matches!(&finished.events[2], LlmEvent::ToolInputEnd { name, .. } if name == "second"));
    assert!(matches!(
        &finished.events[3],
        LlmEvent::ToolCall { name, input, .. } if name == "second" && input == &json!({ "b": 2 })
    ));
}

#[test]
fn lifecycle_delta_and_finish_emit_in_order() {
    let mut events = Vec::new();
    let state = lifecycle::text_delta(lifecycle::initial(), &mut events, "text_1", "hello", None);
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], LlmEvent::StepStart { index: 0 }));
    assert!(matches!(&events[1], LlmEvent::TextStart { id, .. } if id == "text_1"));
    assert!(matches!(&events[2], LlmEvent::TextDelta { id, text, .. } if id == "text_1" && text == "hello"));

    lifecycle::finish(state, &mut events, finish_reason(), None, None);
    assert_eq!(events.len(), 6);
    assert!(matches!(&events[3], LlmEvent::TextEnd { id, text: None, .. } if id == "text_1"));
    assert!(matches!(&events[4], LlmEvent::StepFinish { index: 0, .. }));
    assert!(matches!(&events[5], LlmEvent::Finish { .. }));
}

#[test]
fn cache_control_spends_four_breakpoints() {
    let hint = CacheHint { r#type: CacheHintType::Ephemeral, ttl: None };
    let mut control = cache::CacheControl::new();
    for _ in 0..4 {
        assert_eq!(control.take(Some(&hint)), Some(json!({ "type": "ephemeral" })));
    }
    assert_eq!(control.breakpoints, cache::Breakpoints { remaining: 0, dropped: 0 });
    assert_eq!(control.take(Some(&hint)), None);
    assert_eq!(control.breakpoints, cache::Breakpoints { remaining: 0, dropped: 1 });
    assert_eq!(control.take(None), None);
}

#[test]
fn cache_ttl_maps_to_openai_fields() {
    assert!(cache::openai_request_ttl(None).is_empty());
    assert_eq!(Value::Object(cache::openai_request_ttl(Some("24h"))), json!({ "prompt_cache_retention": "24h" }));
    assert_eq!(
        Value::Object(cache::openai_request_ttl(Some("1h"))),
        json!({ "prompt_cache_options": { "ttl": "1h" } })
    );
}

#[test]
fn open_responses_options_drop_malformed_fields() {
    let request = request_with_options(json!({
        "metadata": { "team": 1 },
        "store": true,
        "reasoningEffort": "high",
        "parallelToolCalls": true
    }));
    let options = open_responses_options::resolve(&request);
    assert_eq!(options.metadata, None);
    assert_eq!(options.store, Some(true));
    assert_eq!(options.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(options.parallel_tool_calls, Some(true));

    let request = request_with_options(json!({ "metadata": { "team": "core" } }));
    assert_eq!(open_responses_options::resolve(&request).metadata, Some(object(json!({ "team": "core" }))));
}

#[test]
fn open_responses_parallel_tool_calls_prefers_option() {
    let mut request = request_with_options(json!({}));
    request.tool_choice =
        Some(ToolChoice { r#type: ToolChoiceType::Auto, name: None, disable_parallel_tool_use: Some(true) });
    let options = open_responses_options::resolve(&request);
    assert_eq!(open_responses_options::resolve_parallel_tool_calls(&request, &options), Some(false));

    request.provider_options = Some(object(json!({ "parallelToolCalls": true })));
    let options = open_responses_options::resolve(&request);
    assert_eq!(open_responses_options::resolve_parallel_tool_calls(&request, &options), Some(true));
}

#[test]
fn gemini_content_filtered_matches_policy_reasons() {
    assert!(gemini_generate_content::content_filtered(Some("SAFETY")));
    assert!(!gemini_generate_content::content_filtered(Some("STOP")));
}

#[test]
fn gemini_media_part_inlines_base64_media() {
    let asset = media_asset(MediaSource::Base64 { data: "aGVsbG8=".to_string(), media_type: "IMAGE/PNG".to_string() });
    assert_eq!(
        gemini_generate_content::media_part("test", &asset).expect("inline media"),
        json!({ "inlineData": { "mimeType": "image/png", "data": "aGVsbG8=" } })
    );
}

#[test]
fn gemini_media_part_sends_google_refs_as_file_data() {
    let asset = media_asset(MediaSource::Ref {
        provider: "google".to_string(),
        id: "files/abc".to_string(),
        media_type: Some("video/mp4".to_string()),
    });
    assert_eq!(
        gemini_generate_content::media_part("test", &asset).expect("file data"),
        json!({ "fileData": { "mimeType": "video/mp4", "fileUri": "files/abc" } })
    );
}

#[test]
fn gemini_media_part_sends_video_urls_as_file_data() {
    let asset = media_asset(MediaSource::Url {
        url: "HTTPS://example.com/video.mp4".to_string(),
        media_type: Some("video/mp4".to_string()),
        expires_at: None,
    });
    assert_eq!(
        gemini_generate_content::media_part("test", &asset).expect("file data"),
        json!({ "fileData": { "mimeType": "video/mp4", "fileUri": "HTTPS://example.com/video.mp4" } })
    );
}

#[test]
fn gemini_media_part_rejects_other_provider_refs() {
    let asset = media_asset(MediaSource::Ref {
        provider: "openai".to_string(),
        id: "file-abc".to_string(),
        media_type: Some("image/png".to_string()),
    });
    let error = gemini_generate_content::media_part("test", &asset).expect_err("foreign ref requires inline media");
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }));
}

fn media_asset(source: MediaSource) -> MediaAsset {
    MediaAsset { source, info: None, provider_metadata: None }
}

fn start_tool(tools: tool_stream::State, key: i64, name: &str, text: &str) -> tool_stream::State {
    tool_stream::append_or_start(
        "test",
        tools,
        key,
        tool_stream::AppendDelta { id: Some(name), name: Some(name), text, provider_metadata: None },
        "missing tool",
    )
    .expect("tool starts")
    .tools
}

fn finish_reason() -> FinishReasonDetails {
    FinishReasonDetails { normalized: FinishReason::Stop, raw: None, category: None, explanation: None }
}

fn request_with_options(provider_options: Value) -> LlmRequest {
    LlmRequest {
        id: None,
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: None,
        generation: Default::default(),
        cache: None,
        prompt_cache_key: None,
        include_raw: None,
        provider_options: Some(object(provider_options)),
        headers: None,
        timeout: None,
        http: None,
    }
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        value => panic!("expected object, got {value}"),
    }
}
