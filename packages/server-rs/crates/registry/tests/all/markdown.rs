use opencode_registry::markdown;

use serde_json::{Map, Value, json};

const INVALID_YAML: &str =
    "---\ndescription: Use when the user needs to crawl pages. Keywords: crawl, scrape\n---\nbody";

fn map(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

#[test]
fn recovers_unquoted_colon_frontmatter_via_the_sanitize_fallback() {
    let parsed = markdown::parse(INVALID_YAML).expect("frontmatter parses");
    assert_eq!(
        parsed.data.get("description"),
        Some(&json!("Use when the user needs to crawl pages. Keywords: crawl, scrape"))
    );
    assert_eq!(parsed.content.trim(), "body");
}

#[test]
fn recovers_a_frontmatter_value_that_starts_with_a_bracketed_tag() {
    let parsed = markdown::parse("---\ndescription: [Team] Does a thing\ntools: [read, edit]\n---\nbody")
        .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("[Team] Does a thing")));
    assert_eq!(parsed.data.get("tools"), Some(&json!(["read", "edit"])));
}

#[test]
fn preserves_flow_mappings_and_flow_sequences_when_repairing_another_field() {
    let parsed = markdown::parse(
        r#"---
description: Review changes. Keywords: review, audit
model: { providerID: example, model: chat, variant: high }
tools: { edit: false }
metadata: { "opencode/autoinvoke": false }
permissions: [{ action: edit, resource: "*", effect: deny }]
---
body"#,
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("Review changes. Keywords: review, audit")));
    assert_eq!(parsed.data.get("model"), Some(&json!({ "providerID": "example", "model": "chat", "variant": "high" })));
    assert_eq!(parsed.data.get("tools"), Some(&json!({ "edit": false })));
    assert_eq!(parsed.data.get("metadata"), Some(&json!({ "opencode/autoinvoke": false })));
    assert_eq!(parsed.data.get("permissions"), Some(&json!([{ "action": "edit", "resource": "*", "effect": "deny" }])));
    assert_eq!(parsed.content.trim(), "body");
}

#[test]
fn preserves_nested_flow_collections_quoted_delimiters_and_inline_comments_during_repair() {
    let parsed = markdown::parse(
        r#"---
description: Review changes. Keywords: review, audit
nested: [[read], [edit]]
tools: [read, edit] # comment: with colon
quoted: ["a]b", 'c''}d', don't]
---
body"#,
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("Review changes. Keywords: review, audit")));
    assert_eq!(parsed.data.get("nested"), Some(&json!([["read"], ["edit"]])));
    assert_eq!(parsed.data.get("tools"), Some(&json!(["read", "edit"])));
    assert_eq!(parsed.data.get("quoted"), Some(&json!(["a]b", "c'}d", "don't"])));
}

#[test]
fn preserves_multi_line_flow_collections_while_repairing_fields_before_and_after() {
    let parsed = markdown::parse(
        r#"---
description: Review changes. Keywords: review, audit
model: { providerID: example, # note: don't close } here
  model: chat,
  variant: high
} # trailing: comment
permissions: [{ action: edit,
resource: "*",
effect: deny }]
note: Remember: check tests
---
body"#,
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("Review changes. Keywords: review, audit")));
    assert_eq!(parsed.data.get("model"), Some(&json!({ "providerID": "example", "model": "chat", "variant": "high" })));
    assert_eq!(parsed.data.get("permissions"), Some(&json!([{ "action": "edit", "resource": "*", "effect": "deny" }])));
    assert_eq!(parsed.data.get("note"), Some(&json!("Remember: check tests")));
}

#[test]
fn recovers_leading_bracketed_and_braced_tags_with_colons_or_apostrophes() {
    let parsed = markdown::parse(
        r#"---
description: [Team: Core] Does a thing
summary: {Team} Review changes
owner: [User's] Follow up
model: { providerID: example, model: chat }
---
body"#,
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("[Team: Core] Does a thing")));
    assert_eq!(parsed.data.get("summary"), Some(&json!("{Team} Review changes")));
    assert_eq!(parsed.data.get("owner"), Some(&json!("[User's] Follow up")));
    assert_eq!(parsed.data.get("model"), Some(&json!({ "providerID": "example", "model": "chat" })));
}

#[test]
fn preserves_flow_collections_with_crlf_line_endings_during_repair() {
    let parsed = markdown::parse(
        "---\r\ndescription: Review changes. Keywords: review, audit\r\nmodel: { providerID: example, model: chat }\r\n---\r\nbody",
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("Review changes. Keywords: review, audit")));
    assert_eq!(parsed.data.get("model"), Some(&json!({ "providerID": "example", "model": "chat" })));
}

#[test]
fn keeps_block_scalar_headers_with_commented_colons_during_repair() {
    let parsed = markdown::parse(
        r#"---
description: Review changes. Keywords: review, audit
prompt: |- # see: docs
  Review carefully.
---
body"#,
    )
    .expect("frontmatter parses");
    assert_eq!(parsed.data.get("description"), Some(&json!("Review changes. Keywords: review, audit")));
    assert_eq!(parsed.data.get("prompt"), Some(&json!("Review carefully.")));
}

#[test]
fn still_fails_on_unclosed_or_mismatched_flow_collections() {
    assert!(markdown::parse("---\nmodel: { providerID: example\n---\nbody").is_none());
    assert!(markdown::parse("---\ndescription: [Tag} text\n---\nbody").is_none());
}

#[test]
fn recovers_the_same_content_again_after_a_previous_failed_parse() {
    assert!(markdown::parse("---\ndescription: [unclosed\n---\nbody").is_none());
    let parsed = markdown::parse(INVALID_YAML).expect("frontmatter parses");
    assert_eq!(
        parsed.data.get("description"),
        Some(&json!("Use when the user needs to crawl pages. Keywords: crawl, scrape"))
    );
}

#[test]
fn keeps_failing_for_the_same_unparseable_content_on_every_call() {
    let input = "---\ndescription: [unclosed\n---\nbody";
    assert!(markdown::parse(input).is_none());
    assert!(markdown::parse(input).is_none());
}

#[test]
fn parses_plain_content_without_frontmatter() {
    let parsed = markdown::parse("just body").expect("content parses");
    assert_eq!(parsed.content, "just body");
    assert_eq!(Value::Object(parsed.data), json!({}));
}

#[test]
fn join_legacy_variant_joins_a_separate_model_and_variant() {
    let data = markdown::join_legacy_variant(map(json!({ "model": "example/chat", "variant": "high" })));
    assert_eq!(Value::Object(data), json!({ "model": "example/chat#high", "variant": "high" }));
}

#[test]
fn join_legacy_variant_leaves_a_model_that_already_has_a_variant() {
    let data = markdown::join_legacy_variant(map(json!({ "model": "example/chat#high", "variant": "low" })));
    assert_eq!(Value::Object(data), json!({ "model": "example/chat#high", "variant": "low" }));
}

#[test]
fn join_legacy_variant_leaves_a_non_string_model() {
    let data = markdown::join_legacy_variant(map(json!({ "model": 5, "variant": "high" })));
    assert_eq!(Value::Object(data), json!({ "model": 5, "variant": "high" }));
}

#[test]
fn join_legacy_variant_leaves_empty_or_hash_containing_variants() {
    let empty = markdown::join_legacy_variant(map(json!({ "model": "example/chat", "variant": "" })));
    assert_eq!(Value::Object(empty), json!({ "model": "example/chat", "variant": "" }));
    let hashed = markdown::join_legacy_variant(map(json!({ "model": "example/chat", "variant": "high#low" })));
    assert_eq!(Value::Object(hashed), json!({ "model": "example/chat", "variant": "high#low" }));
}

#[test]
fn join_legacy_variant_leaves_a_variant_without_a_model() {
    let data = markdown::join_legacy_variant(map(json!({ "variant": "high" })));
    assert_eq!(Value::Object(data), json!({ "variant": "high" }));
}

#[test]
fn pins_the_yaml_scalars_that_differ_from_js_yaml() {
    let parsed = markdown::parse("---\nc: NULL\nd: Null\nn: 012\nf: 0o17\nm: 1_000\ns: 1:30\n---\nbody")
        .expect("frontmatter parses");
    assert_eq!(parsed.data.get("c"), Some(&json!("NULL")));
    assert_eq!(parsed.data.get("d"), Some(&json!("Null")));
    assert_eq!(parsed.data.get("n"), Some(&json!(12)));
    assert_eq!(parsed.data.get("f"), Some(&json!(15)));
    assert_eq!(parsed.data.get("m"), Some(&json!("1_000")));
    assert_eq!(parsed.data.get("s"), Some(&json!("1:30")));
}

#[test]
fn pins_a_merge_key_as_a_literal_key() {
    let parsed =
        markdown::parse("---\nbase: &b\n  x: 1\nchild:\n  <<: *b\n  y: 2\n---\nbody").expect("frontmatter parses");
    assert_eq!(parsed.data.get("base"), Some(&json!({ "x": 1 })));
    assert_eq!(parsed.data.get("child"), Some(&json!({ "<<": { "x": 1 }, "y": 2 })));
}

#[test]
fn rejects_non_mapping_and_complex_flow_frontmatter() {
    assert!(markdown::parse("---\n- a\n- b\n---\nbody").is_none());
    assert!(markdown::parse("---\njust text\n---\nbody").is_none());
    assert!(markdown::parse("---\n? [a, b]\n: c\n---\nbody").is_none());
}

#[test]
fn rejects_a_json_language_header() {
    assert!(markdown::parse("---json\n{\"a\":1}\n---\nbody").is_none());
}
