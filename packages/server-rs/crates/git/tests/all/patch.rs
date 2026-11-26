use std::collections::BTreeMap;

use serde_json::{Map, Value};

use opencode_git::{
    MAX_PATCH_BYTES, MAX_TOTAL_PATCH_BYTES, PATCH_CONTEXT_LINES, Patch, add_patch, chunks_by_file, count_patch,
    delete_patch, empty_patch, file_from_patch_chunk, split_git_patch,
};

fn corpus() -> Map<String, Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/patch.json");
    let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let document: Value = serde_json::from_str(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    match document {
        Value::Object(corpus) => corpus,
        other => panic!("{}: expected an object, found {other}", path.display()),
    }
}

fn cases<'a>(corpus: &'a Map<String, Value>, group: &str) -> &'a [Value] {
    corpus
        .get(group)
        .and_then(Value::as_array)
        .filter(|cases| !cases.is_empty())
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("corpus group {group} is missing or empty"))
}

fn name_of(case: &Value) -> &str {
    case.get("name").and_then(Value::as_str).unwrap_or_else(|| panic!("corpus case is missing a name: {case}"))
}

fn require<'a>(case: &'a Value, name: &str, field: &str) -> &'a Value {
    case.get(field).unwrap_or_else(|| panic!("{name}: missing field {field}"))
}

fn text<'a>(case: &'a Value, name: &str, field: &str) -> &'a str {
    require(case, name, field).as_str().unwrap_or_else(|| panic!("{name}: field {field} is not a string"))
}

fn boolean(case: &Value, name: &str, field: &str) -> bool {
    require(case, name, field).as_bool().unwrap_or_else(|| panic!("{name}: field {field} is not a boolean"))
}

fn count_of(case: &Value, name: &str, field: &str) -> usize {
    let value = require(case, name, field).as_u64().unwrap_or_else(|| panic!("{name}: field {field} is not a number"));
    usize::try_from(value).unwrap_or_else(|_| panic!("{name}: field {field} does not fit in usize"))
}

fn strings(case: &Value, name: &str, field: &str) -> Vec<String> {
    require(case, name, field)
        .as_array()
        .unwrap_or_else(|| panic!("{name}: field {field} is not an array"))
        .iter()
        .map(|value| value.as_str().unwrap_or_else(|| panic!("{name}: field {field} holds a non-string")).to_string())
        .collect()
}

#[test]
fn patch_corpus_matches_the_pinned_typescript() {
    let corpus = corpus();

    for case in cases(&corpus, "empty") {
        let name = name_of(case);
        assert_eq!(empty_patch(text(case, name, "file")), text(case, name, "patch"), "{name}");
    }

    for case in cases(&corpus, "add") {
        let name = name_of(case);
        assert_eq!(
            add_patch(text(case, name, "file"), text(case, name, "content")),
            text(case, name, "patch"),
            "{name}"
        );
    }

    for case in cases(&corpus, "delete") {
        let name = name_of(case);
        assert_eq!(
            delete_patch(text(case, name, "file"), text(case, name, "content")),
            text(case, name, "patch"),
            "{name}"
        );
    }

    for case in cases(&corpus, "count") {
        let name = name_of(case);
        let count = count_patch(text(case, name, "patch"));
        assert_eq!(count.additions, count_of(case, name, "additions"), "{name}");
        assert_eq!(count.deletions, count_of(case, name, "deletions"), "{name}");
    }

    for case in cases(&corpus, "fileFromPatchChunk") {
        let name = name_of(case);
        let expected = case.get("file").and_then(Value::as_str).map(str::to_string);
        assert_eq!(file_from_patch_chunk(text(case, name, "chunk")), expected, "{name}");
    }

    for case in cases(&corpus, "splitGitPatch") {
        let name = name_of(case);
        let patch = Patch { text: text(case, name, "text").to_string(), truncated: boolean(case, name, "truncated") };
        let actual = split_git_patch(&patch);
        let expected = strings(case, name, "chunks");
        assert_eq!(actual, expected, "{name}");
    }

    for case in cases(&corpus, "chunksByFile") {
        let name = name_of(case);
        let patch = Patch { text: text(case, name, "text").to_string(), truncated: boolean(case, name, "truncated") };
        let fallback: Vec<Option<String>> = require(case, name, "fallback")
            .as_array()
            .unwrap_or_else(|| panic!("{name}: field fallback is not an array"))
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect();
        let expected: BTreeMap<String, String> = require(case, name, "files")
            .as_object()
            .unwrap_or_else(|| panic!("{name}: field files is not an object"))
            .iter()
            .map(|(file, chunk)| {
                let chunk =
                    chunk.as_str().unwrap_or_else(|| panic!("{name}: files[{file}] is not a string")).to_string();
                (file.clone(), chunk)
            })
            .collect();
        let actual = chunks_by_file(&patch, |index| fallback.get(index).cloned().flatten());
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn patch_limits_match_the_pinned_typescript() {
    assert_eq!(PATCH_CONTEXT_LINES, i32::MAX);
    assert_eq!(MAX_PATCH_BYTES, 10_000_000);
    assert_eq!(MAX_TOTAL_PATCH_BYTES, 10_000_000);
}
