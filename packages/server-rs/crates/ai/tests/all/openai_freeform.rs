use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use opencode_ai::protocols::openai_freeform::{self, Custom};
use opencode_ai::protocols::utils::tool_stream;
use opencode_ai::schema::{Freeform, LlmEvent, ProviderMetadata, ToolDefinition};

fn one_field_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "code": { "type": "string" } },
        "required": ["code"],
        "additionalProperties": false
    })
}

fn tool() -> ToolDefinition {
    ToolDefinition {
        name: "run".to_string(),
        description: "Run code".to_string(),
        input_schema: one_field_schema(),
        output_schema: None,
        freeform: Some(Freeform::Text { field: None }),
        cache: None,
        native: None,
    }
}

fn custom() -> Custom {
    Custom { field: "code".to_string(), format: Freeform::Text { field: None }, options: None }
}

fn customs() -> BTreeMap<String, Custom> {
    BTreeMap::from([("run".to_string(), custom())])
}

fn formats(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn provider_metadata(key: &str, value: Value) -> ProviderMetadata {
    let mut metadata = ProviderMetadata::new();
    metadata.insert(key.to_string(), value);
    metadata
}

fn parsed_json(custom: Option<&Custom>, raw: &str) -> (Value, Option<String>) {
    let parsed = openai_freeform::parse(custom, raw);
    (serde_json::from_str(&parsed.input).expect("valid JSON input"), parsed.header)
}

fn header_of(raw: &str) -> Option<String> {
    raw.split_once('\n').map(|(head, _)| head.to_string())
}

fn started_tool(key: i64, input: String) -> tool_stream::State {
    let mut tools = tool_stream::empty();
    tool_stream::start(
        &mut tools,
        key,
        tool_stream::PendingTool {
            id: "call_1".to_string(),
            name: "run".to_string(),
            namespace: None,
            input,
            provider_executed: None,
            provider_metadata: None,
        },
    );
    tools
}

fn delta_texts(events: &[LlmEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            LlmEvent::ToolInputDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn schema(properties: Value, required: Value) -> Value {
    let mut fields = Map::new();
    fields.insert("code".to_string(), json!({ "type": "string" }));
    for (name, value) in properties.as_object().expect("properties") {
        fields.insert(name.clone(), value.clone());
    }
    json!({ "type": "object", "properties": Value::Object(fields), "required": required })
}

fn execute_schema() -> Value {
    schema(
        json!({
            "timeout": { "type": "integer", "exclusiveMinimum": 0, "description": "How long to wait." },
            "background": { "type": "boolean" }
        }),
        json!(["code"]),
    )
}

fn execute_format() -> Freeform {
    Freeform::Text { field: Some("code".to_string()) }
}

fn execute_options() -> Custom {
    openai_freeform::resolve(
        &ToolDefinition { input_schema: execute_schema(), freeform: Some(execute_format()), ..tool() },
        None,
    )
    .expect("custom tool")
}

#[test]
fn resolves_a_freeform_tool_to_its_string_field() {
    assert_eq!(openai_freeform::resolve(&tool(), None), Some(custom()));
    assert_eq!(openai_freeform::resolve(&ToolDefinition { freeform: None, ..tool() }, None), None);
    let native = provider_metadata("openai", json!({}));
    assert_eq!(openai_freeform::resolve(&ToolDefinition { native: Some(native), ..tool() }, None), None);
    let numeric = json!({
        "type": "object",
        "properties": { "code": { "type": "number" } },
        "required": ["code"],
        "additionalProperties": false
    });
    assert_eq!(openai_freeform::resolve(&ToolDefinition { input_schema: numeric, ..tool() }, None), None);
    let optional = json!({
        "type": "object",
        "properties": { "code": { "type": "string" } },
        "required": [],
        "additionalProperties": false
    });
    assert_eq!(openai_freeform::resolve(&ToolDefinition { input_schema: optional, ..tool() }, None), None);
    // Naming the only field changes nothing.
    let named = Freeform::Text { field: Some("code".to_string()) };
    assert_eq!(
        openai_freeform::resolve(&ToolDefinition { freeform: Some(named.clone()), ..tool() }, None),
        Some(Custom { field: "code".to_string(), format: named, options: None })
    );
}

#[test]
fn only_sends_a_format_the_model_accepts_and_every_format_when_the_model_does_not_say() {
    assert_eq!(openai_freeform::resolve(&tool(), Some(&formats(&["text", "lark"]))), Some(custom()));
    assert_eq!(openai_freeform::resolve(&tool(), None), Some(custom()));
    assert_eq!(openai_freeform::resolve(&tool(), Some(&formats(&[]))), None);
    assert_eq!(openai_freeform::resolve(&tool(), Some(&formats(&["regex"]))), None);
}

#[test]
fn converts_between_raw_text_and_the_json_input() {
    let raw = ["say \"hi\"\n\tdone\\", "naïve 😀 \u{2028}", "\u{0} }{ ]"];
    for raw in raw {
        let (input, header) = parsed_json(Some(&custom()), raw);
        assert_eq!(input, json!({ "code": raw }));
        assert_eq!(header, None);
        assert_eq!(openai_freeform::to_raw(&custom(), &json!({ "code": raw }), None), raw);
    }
    assert_eq!(openai_freeform::parse(None, "x").input, r#"{"input":"x"}"#);
    assert_eq!(openai_freeform::parse(None, "x").header, None);
    assert_eq!(openai_freeform::to_raw(&custom(), &json!({ "code": 1 }), None), r#"{"code":1}"#);
}

#[test]
fn streams_raw_chunks_as_json_fragments_that_add_up_to_the_input() {
    for raw in ["say \"hi\"\n\tdone\\", "naïve 😀 \u{2028}", "\u{0} }{ ]"] {
        // Split at every code point, including escape-worthy characters.
        let fragments = std::iter::once(openai_freeform::stream_start(Some(&custom()), ""))
            .chain(raw.chars().map(|character| openai_freeform::stream_delta(&character.to_string())))
            .collect::<Vec<String>>()
            .join("");
        let tools = started_tool(0, fragments);
        let closed = openai_freeform::close(tools, [0], &customs(), "openai", Some(raw));
        assert_eq!(delta_texts(&closed.events), vec!["\"}"]);
        assert_eq!(serde_json::from_str::<Value>(&closed.tools[&0].input).expect("valid JSON"), json!({ "code": raw }));
    }
}

#[test]
fn takes_the_final_input_when_it_disagrees_with_the_streamed_one() {
    let tools = started_tool(0, openai_freeform::stream_start(Some(&custom()), "partial"));
    let closed = openai_freeform::close(tools.clone(), [0, 1], &customs(), "openai", Some("different"));
    assert_eq!(closed.events, Vec::new());
    assert_eq!(
        serde_json::from_str::<Value>(&closed.tools[&0].input).expect("valid JSON"),
        json!({ "code": "different" })
    );
    let appended = openai_freeform::close(tools, [0], &customs(), "openai", None);
    assert_eq!(delta_texts(&appended.events), vec!["\"}"]);
}

#[test]
fn keeps_an_item_id_only_while_the_call_type_is_unchanged() {
    let metadata = |item_id: &str, custom: bool| {
        provider_metadata("openai", Value::Object(openai_freeform::metadata(item_id, custom)))
    };
    let custom_call = metadata("ctc_1", true);
    let function_call = metadata("fc_1", false);
    assert!(openai_freeform::keeps_item_id(Some(&custom_call), "openai", true));
    assert!(!openai_freeform::keeps_item_id(Some(&custom_call), "openai", false));
    assert!(openai_freeform::keeps_item_id(Some(&function_call), "openai", false));
    assert!(!openai_freeform::keeps_item_id(Some(&function_call), "openai", true));
    assert!(openai_freeform::keeps_item_id(None, "openai", false));
}

#[test]
fn sends_an_order_free_grammar_and_a_description_built_from_the_schema() {
    let options = execute_options();
    let expected = [
        "start: ff_line ff_body | ff_plain",
        r#"ff_line: "// @options:" " "* "{" " "* ff_pair (" "* "," " "* ff_pair)* " "* "}" "\n""#,
        r#"ff_pair: "\"timeout\"" " "* ":" " "* /[1-9][0-9]*/ | "\"background\"" " "* ":" " "* ("true" | "false")"#,
        r"ff_body: /[\s\S]+/",
        r"ff_plain: /(?:[^\/]|\/[^\/]|\/\/[^ ]|\/\/ [^@]|\/\/ @[^o]|\/\/ @o[^p]|\/\/ @op[^t]|\/\/ @opt[^i]|\/\/ @opti[^o]|\/\/ @optio[^n]|\/\/ @option[^s]|\/\/ @options[^:])[\s\S]*|\/|\/\/|\/\/ |\/\/ @|\/\/ @o|\/\/ @op|\/\/ @opt|\/\/ @opti|\/\/ @optio|\/\/ @option|\/\/ @options/",
    ];
    assert_eq!(
        options.options.as_ref().and_then(|options| options.grammar.as_deref()),
        Some(expected.join("\n").as_str())
    );
    let expected_description = [
        "Run code",
        "",
        "To set the parameters below, start with a `// @options:` line holding a JSON object of them, then write `code` from the next line. Leave the line out to use the defaults.",
        "- `timeout` (integer): How long to wait.",
        "- `background` (boolean)",
        "For example:",
        r#"// @options: {"background": true}"#,
        "<code>",
    ]
    .join("\n");
    assert_eq!(openai_freeform::description(&tool(), &options), expected_description);
}

#[test]
fn reads_keys_in_any_order_with_spaces_and_replays_the_line_as_written() {
    let options = execute_options();
    let cases = [
        ("// @options: {\"timeout\":5000}\nawait build()", json!({ "code": "await build()", "timeout": 5000 })),
        (
            "// @options: { \"timeout\": 1, \"background\": true }\nx",
            json!({ "code": "x", "timeout": 1, "background": true }),
        ),
        (
            "// @options:{\"background\" :false,\"timeout\":2}\r\nx",
            json!({ "code": "x", "timeout": 2, "background": false }),
        ),
        ("await build()", json!({ "code": "await build()" })),
    ];
    for (raw, expected) in cases {
        let (input, header) = parsed_json(Some(&options), raw);
        assert_eq!(input, expected);
        let expected_header = if raw.starts_with("//") { header_of(raw) } else { None };
        assert_eq!(header, expected_header);
        let header_value = header.as_ref().map(|header| json!(header));
        assert_eq!(openai_freeform::to_raw(&options, &expected, header_value.as_ref()), raw);
    }
}

#[test]
fn writes_a_fresh_line_when_the_input_no_longer_matches_the_saved_one_or_there_is_none() {
    let options = execute_options();
    let header = json!("// @options: { \"background\": true }");
    assert_eq!(
        openai_freeform::to_raw(&options, &json!({ "code": "x", "background": true, "timeout": 9 }), Some(&header)),
        "// @options: {\"timeout\":9,\"background\":true}\nx"
    );
    assert_eq!(
        openai_freeform::to_raw(&options, &json!({ "code": "x", "background": true }), None),
        "// @options: {\"background\":true}\nx"
    );
    assert_eq!(openai_freeform::to_raw(&options, &json!({ "code": "x" }), Some(&header)), "x");
}

#[test]
fn leaves_a_line_that_is_not_a_json_object_as_raw_text_so_it_fails_as_malformed_json() {
    let options = execute_options();
    let parsed = openai_freeform::parse(Some(&options), "// @options: {timeout: 1}\nx");
    assert_eq!(parsed.input, "// @options: {timeout: 1}\nx");
    assert_eq!(parsed.header, None);
    // The tool's own schema checks keys and values, as for a JSON call.
    let (input, header) = parsed_json(Some(&options), "// @options: {\"timeout\":\"5\"}\nx");
    assert_eq!(input, json!({ "code": "x", "timeout": "5" }));
    assert_eq!(header.as_deref(), Some("// @options: {\"timeout\":\"5\"}"));
    // A line with nothing after it is plain text, not a call with empty code.
    let (input, header) = parsed_json(Some(&options), "// @options: {\"background\":true}");
    assert_eq!(input, json!({ "code": "// @options: {\"background\":true}" }));
    assert_eq!(header, None);
}

#[test]
fn takes_number_string_and_enum_values() {
    let definition = openai_freeform::resolve(
        &ToolDefinition {
            input_schema: schema(
                json!({
                    "ratio": { "type": "number" },
                    "cwd": { "type": "string" },
                    "mode": { "type": "string", "enum": ["fast", "safe"] }
                }),
                json!(["code"]),
            ),
            freeform: Some(execute_format()),
            ..tool()
        },
        None,
    )
    .expect("custom tool");
    let grammar = definition.options.as_ref().and_then(|options| options.grammar.as_deref()).expect("grammar");
    assert!(grammar.contains(r#""\"ratio\"" " "* ":" " "* /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/"#));
    assert!(grammar.contains(r#""\"cwd\"" " "* ":" " "* /"(?:[^"\\\x00-\x1f]|\\["\\\/bfnrt]|\\u[0-9a-fA-F]{4})*"/"#));
    assert!(grammar.contains(r#""\"mode\"" " "* ":" " "* ("\"fast\"" | "\"safe\"")"#));
    let raw = "// @options: {\"mode\": \"safe\", \"ratio\": 1.5e2, \"cwd\": \"C:\\\\src\"}\nls";
    let parsed = openai_freeform::parse(Some(&definition), raw);
    let input = serde_json::from_str::<Value>(&parsed.input).expect("valid JSON");
    assert_eq!(input["code"], json!("ls"));
    assert_eq!(input["mode"], json!("safe"));
    assert_eq!(input["ratio"].as_f64(), Some(150.0));
    assert_eq!(input["cwd"], json!("C:\\src"));
}

#[test]
fn requires_the_line_when_a_parameter_is_required() {
    let definition = openai_freeform::resolve(
        &ToolDefinition {
            input_schema: schema(json!({ "path": { "type": "string" } }), json!(["code", "path"])),
            freeform: Some(execute_format()),
            ..tool()
        },
        None,
    )
    .expect("custom tool");
    let grammar = definition.options.as_ref().and_then(|options| options.grammar.as_deref()).expect("grammar");
    assert_eq!(grammar.split('\n').next(), Some("start: ff_line ff_body"));
    let parsed = openai_freeform::parse(Some(&definition), "// @options: {\"path\": \"a.ts\"}\nx");
    assert_eq!(
        serde_json::from_str::<Value>(&parsed.input).expect("valid JSON"),
        json!({ "code": "x", "path": "a.ts" })
    );
}

#[test]
fn composes_a_tools_own_grammar_as_the_body() {
    let lark_grammar = ["start: patch LF", "patch: \"*** \" /(.+)/", "%import common.LF"].join("\n");
    let definition = openai_freeform::resolve(
        &ToolDefinition {
            input_schema: execute_schema(),
            freeform: Some(Freeform::Lark { grammar: lark_grammar, field: Some("code".to_string()) }),
            ..tool()
        },
        None,
    )
    .expect("custom tool");
    let grammar = definition.options.as_ref().and_then(|options| options.grammar.as_deref()).expect("grammar");
    let lines = grammar.split('\n').collect::<Vec<&str>>();
    assert_eq!(lines.first().copied(), Some("start: ff_line fb_start | fb_start"));
    assert_eq!(&lines[lines.len() - 3..], &["fb_start: patch LF", "patch: \"*** \" /(.+)/", "%import common.LF"]);
}

#[test]
fn parses_the_line_without_a_grammar_for_a_model_that_only_takes_text() {
    let options = execute_options();
    let definition = openai_freeform::resolve(
        &ToolDefinition { input_schema: execute_schema(), freeform: Some(execute_format()), ..tool() },
        Some(&formats(&["text"])),
    )
    .expect("custom tool");
    assert_eq!(openai_freeform::grammar(&definition), None);
    assert_eq!(
        definition.options.as_ref().map(|options| options.description.clone()),
        options.options.as_ref().map(|options| options.description.clone())
    );
    let parsed = openai_freeform::parse(Some(&definition), "// @options: {\"background\": true}\nx");
    assert_eq!(
        serde_json::from_str::<Value>(&parsed.input).expect("valid JSON"),
        json!({ "code": "x", "background": true })
    );
    // Without Lark, only a raw text body can share the wire with the line.
    assert_eq!(
        openai_freeform::resolve(
            &ToolDefinition {
                input_schema: execute_schema(),
                freeform: Some(Freeform::Regex { pattern: "x".to_string(), field: Some("code".to_string()) }),
                ..tool()
            },
            Some(&formats(&["text", "regex"])),
        ),
        None
    );
}

#[test]
fn stays_a_json_tool_when_a_parameter_cannot_go_on_the_line() {
    for property in [json!({ "type": "object" }), json!({ "type": ["string", "null"] }), json!({ "enum": [] })] {
        assert_eq!(
            openai_freeform::resolve(
                &ToolDefinition {
                    input_schema: schema(json!({ "env": property }), json!(["code"])),
                    freeform: Some(execute_format()),
                    ..tool()
                },
                None,
            ),
            None
        );
    }
    assert_eq!(
        openai_freeform::resolve(
            &ToolDefinition {
                input_schema: execute_schema(),
                freeform: Some(Freeform::Text { field: Some("script".to_string()) }),
                ..tool()
            },
            None,
        ),
        None
    );
}

#[test]
fn closes_a_streamed_call_with_the_parsed_input_and_its_line_in_the_metadata() {
    let options = execute_options();
    let raw = "// @options: {\"background\": true}\nx";
    let mut tools = tool_stream::empty();
    tool_stream::start(
        &mut tools,
        0,
        tool_stream::PendingTool {
            id: "call_1".to_string(),
            name: "run".to_string(),
            namespace: None,
            input: openai_freeform::stream_start(Some(&options), raw),
            provider_executed: None,
            provider_metadata: Some(provider_metadata("openai", json!({ "itemId": "ctc_1", "custom": true }))),
        },
    );
    let closed =
        openai_freeform::close(tools, [0], &BTreeMap::from([("run".to_string(), options.clone())]), "openai", None);
    assert_eq!(delta_texts(&closed.events), vec!["\"}"]);
    assert_eq!(
        serde_json::from_str::<Value>(&closed.tools[&0].input).expect("valid JSON"),
        json!({ "code": "x", "background": true })
    );
    assert_eq!(
        closed.tools[&0].provider_metadata,
        Some(provider_metadata(
            "openai",
            json!({ "itemId": "ctc_1", "custom": true, "header": "// @options: {\"background\": true}" }),
        ))
    );
}
