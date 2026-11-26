use opencode_tool::ToolContext;
use opencode_tool::context::permission_input;

#[tokio::test]
async fn default_context_fails_closed() {
    let context = ToolContext::default();
    let allows = context
        .authorizer
        .allows(permission_input(&context, "read", vec!["notes.txt".to_owned()], Vec::new()))
        .await
        .expect("allows resolves");
    assert!(!allows);
    let error = context
        .authorizer
        .assert(permission_input(&context, "read", vec!["notes.txt".to_owned()], Vec::new()))
        .await
        .expect_err("the default context denies access");
    assert!(error.to_string().contains("Permission denied"));
}
