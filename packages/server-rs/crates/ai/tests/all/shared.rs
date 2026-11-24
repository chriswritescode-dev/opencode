use serde_json::{Number, json};

use opencode_ai::protocols::shared;
use opencode_ai::protocols::shared::{
    flatten_tool_request, inline_media, reported_billing, tool_file_media, tool_result_text, wrapped_system_update,
};
use opencode_ai::schema::{
    AssistantMessage, CacheHint, CacheHintType, ContentPart, LlmRequest, MediaAsset, MediaPart, MediaSource, Message,
    SystemMessage, TextPart, ToolCallPart, ToolContent, ToolDefinition, ToolEntry, ToolNamespace, ToolResultPart,
    ToolResultValue,
};

#[test]
fn flatten_tool_request_flattens_namespaces_and_sanitizes_undeclared_names() {
    let request = LlmRequest {
        tools: vec![ToolEntry::Namespace(ToolNamespace {
            name: "files".to_string(),
            description: None,
            tools: vec![ToolEntry::Tool(tool_definition("read"))],
        })],
        messages: vec![Message::Assistant(AssistantMessage {
            content: vec![
                ContentPart::ToolCall(tool_call("call_1", "read", Some("files"))),
                ContentPart::ToolCall(tool_call("call_2", "a.b.c", None)),
            ],
            ..Default::default()
        })],
        ..Default::default()
    };

    let (tools, flattened) = flatten_tool_request(&request);
    assert_eq!(tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<&str>>(), vec!["files_read"]);
    let Message::Assistant(message) = &flattened.messages[0] else {
        panic!("expected assistant message");
    };
    let ContentPart::ToolCall(namespaced) = &message.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(namespaced.name, "files_read");
    assert_eq!(namespaced.namespace, None);
    let ContentPart::ToolCall(dotted) = &message.content[1] else {
        panic!("expected tool call");
    };
    assert_eq!(dotted.name, "a_b_c");
    assert_eq!(dotted.namespace, None);
}

#[test]
fn tool_result_text_encodes_json_and_stringifies_text_and_errors() {
    assert_eq!(tool_result_text(&tool_result_part(ToolResultValue::Text { value: json!("hello") })), "hello");
    assert_eq!(tool_result_text(&tool_result_part(ToolResultValue::Text { value: json!(7) })), "7");
    let structured = ToolResultValue::Error { value: json!({ "code": "denied" }) };
    assert_eq!(tool_result_text(&tool_result_part(structured)), "{\"code\":\"denied\"}");
    let plain = ToolResultValue::Error { value: json!("boom") };
    assert_eq!(tool_result_text(&tool_result_part(plain)), "boom");
    let value = ToolResultValue::Json { value: json!({ "ok": true }) };
    assert_eq!(tool_result_text(&tool_result_part(value)), "{\"ok\":true}");
}

#[test]
fn wrapped_system_update_escapes_text_and_rejects_media() {
    let message = Message::System(SystemMessage {
        content: vec![
            ContentPart::Text(TextPart { text: "a & b".to_string(), cache: None, provider_metadata: None }),
            ContentPart::Text(TextPart {
                text: "<tag>".to_string(),
                cache: Some(CacheHint { r#type: CacheHintType::Ephemeral, ttl: None }),
                provider_metadata: None,
            }),
        ],
        ..Default::default()
    });

    let part = wrapped_system_update("openai-chat", &message).expect("text system message");
    assert_eq!(part.text, "<system-update>\na &amp; b\n&lt;tag&gt;\n</system-update>");
    assert_eq!(part.cache, Some(CacheHint { r#type: CacheHintType::Ephemeral, ttl: None }));
    assert_eq!(part.provider_metadata, None);

    let message = Message::System(SystemMessage {
        content: vec![ContentPart::Media(media_part(media_asset(MediaSource::Base64 {
            data: "aGVsbG8=".to_string(),
            media_type: "image/png".to_string(),
        })))],
        ..Default::default()
    });
    let error = wrapped_system_update("openai-chat", &message).expect_err("media is rejected");
    assert_eq!(error.message(), "openai-chat system messages only support text content for now");
}

#[test]
fn reported_billing_keeps_integer_cost_and_ignores_negative_cost() {
    let reported = reported_billing(&json!({ "cost": 3 }), &[]).expect("integer cost");
    assert!(reported.is_i64() || reported.is_u64());
    assert_eq!(reported.to_string(), "3");

    assert_eq!(reported_billing(&json!({ "cost": -1 }), &[]), None);
}

#[test]
fn inline_media_builds_data_url_from_bytes_asset() {
    let asset = media_asset(MediaSource::Bytes { data: "aGVsbG8=".to_string(), media_type: "IMAGE/PNG".to_string() });

    let inline = inline_media(&asset).expect("inline media");
    assert_eq!(inline.mime, "image/png");
    assert_eq!(inline.base64, "aGVsbG8=");
    assert_eq!(inline.data_url, "data:image/png;base64,aGVsbG8=");
}

#[test]
fn tool_file_media_reads_data_urls_and_plain_base64() {
    let data_url = ToolContent::File {
        uri: "data:IMAGE/PNG;base64,aGVsbG8=".to_string(),
        mime: "application/pdf".to_string(),
        name: Some("report.pdf".to_string()),
    };
    let part = tool_file_media(&data_url).expect("data url file");
    assert_eq!(part.filename.as_deref(), Some("report.pdf"));
    let MediaSource::Base64 { data, media_type } = &part.media.source else {
        panic!("expected base64 source");
    };
    assert_eq!(data, "aGVsbG8=");
    assert_eq!(media_type, "application/pdf");

    let plain = ToolContent::File { uri: "aGVsbG8=".to_string(), mime: "image/png".to_string(), name: None };
    let part = tool_file_media(&plain).expect("base64 file");
    let MediaSource::Base64 { data, media_type } = &part.media.source else {
        panic!("expected base64 source");
    };
    assert_eq!(data, "aGVsbG8=");
    assert_eq!(media_type, "image/png");

    assert_eq!(tool_file_media(&ToolContent::Text { text: "not a file".to_string() }), None);
}

#[test]
fn fit_thinking_budget_matches_typescript() {
    let cases = [
        (2048.0, Some(4096.0), 1, "2048"),
        (2049.0, Some(4096.0), 1, "2048"),
        (3000.0, Some(4096.0), 1024, "2048"),
        (100.0, Some(4096.0), 1024, "100"),
        (100.0, Some(100.0), 1024, "1024"),
        (0.0, None, 1, "0"),
        (-1.0, Some(4096.0), 1, "-1"),
        (2048.5, Some(4096.0), 1, "2048"),
        (2000.5, Some(4096.0), 1, "2000.5"),
        (2000.5, None, 1, "2000.5"),
        (1024.0, Some(2049.0), 1024, "1024"),
        (1025.0, Some(2049.0), 1024, "1024"),
        (500.0, Some(1000.5), 1, "500"),
        (501.0, Some(1000.5), 1, "500"),
        (10.5, Some(20.0), 1024, "1024"),
        (4096.0, Some(4096.0), 512, "2048"),
        (2048.0, Some(4097.0), 1024, "2048"),
        (1023.9, Some(2048.0), 1024, "1023.9"),
    ];
    for (budget, max_tokens, minimum, expected) in cases {
        let budget = Number::from_f64(budget).expect("finite budget");
        let max_tokens = max_tokens.map(|value| Number::from_f64(value).expect("finite max tokens"));
        assert_eq!(
            shared::fit_thinking_budget(&budget, max_tokens.as_ref(), minimum).to_string(),
            expected,
            "budget {budget} max_tokens {max_tokens:?} minimum {minimum}"
        );
    }
}

#[test]
fn tool_file_media_matches_typescript_data_url_syntax() {
    let accepted = [
        ("data:image/png;base64,aGVsbG8=", "aGVsbG8="),
        ("DATA:IMAGE/PNG;BASE64,aGVsbG8=", "aGVsbG8="),
        ("data:image/png;charset=utf-8;base64,aGVsbG8=", "aGVsbG8="),
        ("data:image/png;base64,a,b", "a,b"),
        ("data:image/png;base64,line1\nline2", "line1\nline2"),
        ("data:image/png;base64,", ""),
        ("data:image/png;base64,abc ", "abc "),
        ("data:image/png;;base64,abc", "abc"),
    ];
    for (uri, expected) in accepted {
        assert_eq!(tool_file_data(uri), expected, "accepted {uri:?}");
    }

    let rejected = [
        "data:image/png;notbase64,abc",
        "data:image/png;base64",
        "data:;base64,abc",
        "data:image/png; base64,abc",
        "data:image/png;base64 ,abc",
        " data:image/png;base64,abc",
        "data:image/png;foo=bar,baz;base64,xx",
    ];
    for uri in rejected {
        assert_eq!(tool_file_data(uri), uri, "rejected {uri:?} keeps the raw value");
    }
}

fn tool_file_data(uri: &str) -> String {
    let part = tool_file_media(&ToolContent::File { uri: uri.to_string(), mime: "image/png".to_string(), name: None })
        .expect("file content");
    let MediaSource::Base64 { data, .. } = part.media.source else {
        panic!("expected base64 source for {uri:?}");
    };
    data
}

fn tool_definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: String::new(),
        input_schema: json!({ "type": "object" }),
        output_schema: None,
        freeform: None,
        cache: None,
        native: None,
    }
}

fn tool_call(id: &str, name: &str, namespace: Option<&str>) -> ToolCallPart {
    ToolCallPart {
        id: id.to_string(),
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        input: json!({}),
        provider_executed: None,
        cache: None,
        provider_metadata: None,
    }
}

fn tool_result_part(result: ToolResultValue) -> ToolResultPart {
    ToolResultPart {
        id: "call_1".to_string(),
        name: "tool".to_string(),
        namespace: None,
        result,
        provider_executed: None,
        cache: None,
        provider_metadata: None,
    }
}

fn media_asset(source: MediaSource) -> MediaAsset {
    MediaAsset { source, info: None, provider_metadata: None, headers: None }
}

fn media_part(media: MediaAsset) -> MediaPart {
    MediaPart { media, filename: None, cache: None, provider_metadata: None }
}
