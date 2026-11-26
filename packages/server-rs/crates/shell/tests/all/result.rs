use opencode_shell::result::{
    Interrupted, Metadata, Notification, NotificationInput, Output, ShellResult, interrupted, metadata, notice,
    notification, output, user_notification,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    kind: String,
    #[serde(default)]
    result: Option<ShellResult>,
    #[serde(default)]
    output: Option<Output>,
    #[serde(default)]
    timeout: Option<f64>,
    #[serde(default)]
    input: Option<NotificationInput>,
    expected: serde_json::Value,
}

#[test]
fn replays_the_result_corpus() {
    let corpus = std::fs::read_to_string(format!("{}/tests/corpus/result.json", env!("CARGO_MANIFEST_DIR")))
        .expect("result corpus");
    let cases: Vec<Case> = serde_json::from_str(&corpus).expect("result corpus json");
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let actual = match case.kind.as_str() {
                "output" => serde_json::to_value(output(case.result.as_ref().expect("result"))),
                "notice" => serde_json::to_value(notice(case.output.as_ref().expect("output"), case.timeout)),
                "metadata" => serde_json::to_value(metadata(&output(case.result.as_ref().expect("result")))),
                "interrupted" => serde_json::to_value(interrupted(case.result.as_ref().expect("result"))),
                "notification" => serde_json::to_value(notification(case.input.clone().expect("input"))),
                "userNotification" => serde_json::to_value(user_notification(case.result.as_ref().expect("result"))),
                other => panic!("unknown result case kind {other}"),
            };
            (actual.as_ref().ok() != Some(&case.expected)).then(|| {
                format!(
                    "{}: {:?}\n  expected {}\n  actual   {}",
                    case.name,
                    case.kind,
                    serde_json::to_string(&case.expected).unwrap_or_default(),
                    serde_json::to_string(&actual.ok()).unwrap_or_default(),
                )
            })
        })
        .take(20)
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} of {} result cases mismatch:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}

#[test]
fn exposes_the_stable_output_shapes() {
    let _: Interrupted = interrupted(&ShellResult {
        info: opencode_shell::result::Info {
            id: "sh".to_owned(),
            status: opencode_shell::result::Status::Killed,
            command: "work".to_owned(),
            file: "/tmp/out".to_owned(),
            exit: None,
            signal: None,
        },
        capture: None,
    });
    let _: Metadata = metadata(&Output::default());
    let _: Notification = notification(NotificationInput {
        shell_id: "sh".to_owned(),
        job_id: None,
        command: "work".to_owned(),
        state: opencode_shell::result::State::Completed,
        text: String::new(),
        output: None,
    });
}
