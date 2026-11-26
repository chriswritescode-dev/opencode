use std::time::Duration;

use crate::service::Harness;

#[tokio::test]
async fn eviction_makes_progress_past_the_retention_limit() {
    let harness = Harness::new().await;
    let first = harness.finish("printf first").await;
    for _ in 0..opencode_shell::retention::EXITED_LIMIT {
        harness.finish("printf next").await;
    }
    for _ in 0..200 {
        if harness.shell.get(&first.id.0).is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(matches!(harness.shell.get(&first.id.0), Err(opencode_shell::service::ShellError::NotFound { .. })));

    let survivor = harness.finish("printf survivor").await;
    let result = harness.shell.result(survivor, None).await;
    let capture = result.capture.expect("the output file is known");
    assert_eq!(capture.output, "survivor");
    assert!(!capture.truncated);
}
