use opencode_fs::diff::{self, FileDiffStatus};
use serde_json::Value;

#[test]
fn diff_corpus_matches_expected_output() {
    for case in crate::corpus::cases("diff") {
        let name = crate::corpus::field(&case, "name");
        let file = crate::corpus::field(&case, "file");
        let before = crate::corpus::field(&case, "before");
        let after = crate::corpus::field(&case, "after");
        let status = match crate::corpus::field(&case, "status") {
            "added" => FileDiffStatus::Added,
            "deleted" => FileDiffStatus::Deleted,
            "modified" => FileDiffStatus::Modified,
            value => panic!("case {name}: unknown status {value}"),
        };
        assert_eq!(serde_json::to_value(status).unwrap(), case["status"], "case {name}: status");

        let diff = diff::file_diff(file, before, after, status);
        assert_eq!(Value::String(diff.patch.clone()), case["patch"], "case {name}: patch");
        assert_eq!(diff.additions, case["additions"].as_u64().unwrap() as usize, "case {name}: additions");
        assert_eq!(diff.deletions, case["deletions"].as_u64().unwrap() as usize, "case {name}: deletions");
        assert_eq!(Value::String(diff::trim_diff(&diff.patch)), case["trimmed"], "case {name}: trimmed");
        assert_eq!(
            Value::String(diff::create_two_files_patch("old.txt", "new.txt", before, after)),
            case["twoFilesPatch"],
            "case {name}: two files patch"
        );

        let changes = diff::diff_lines(before, after)
            .iter()
            .map(|change| serde_json::to_value(change).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(Value::Array(changes), case["changes"], "case {name}: changes");
    }
}
