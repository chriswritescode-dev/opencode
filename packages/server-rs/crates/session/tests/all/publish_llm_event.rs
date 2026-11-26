use std::sync::Arc;

use opencode_ai::LlmEvent;
use opencode_bus::{Bus, BusOptions};
use opencode_contract::generated::{ModelRef, ProviderID, SessionStructuredError};
use opencode_session::publish_llm_event::Publisher;
use opencode_storage::{Database, DatabaseLocation};
use opencode_tool::ToolOutput;
use serde_json::json;

use crate::TempDir;

#[tokio::test]
async fn an_empty_tool_execution_leaves_the_tool_unsettled_for_a_failure_event() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let bus = Bus::new(Arc::new(database), BusOptions::default());
    let mut publisher = Publisher::new(
        bus,
        "ses_1",
        "msg_1",
        "build",
        &ModelRef { id: "gpt-5".to_owned(), provider_id: ProviderID("openai".to_owned()), variant: None },
        None,
        1000,
        "test",
    );
    publisher
        .publish(&LlmEvent::ToolCall {
            id: "call_1".to_owned(),
            name: "read".to_owned(),
            namespace: None,
            input: json!({}),
            provider_executed: None,
            provider_metadata: None,
        })
        .expect("publishes the tool call");

    publisher
        .tool_execution("call_1", "read", ToolOutput { output: json!({}), content: Vec::new(), metadata: None })
        .expect_err("the empty execution fails");

    let failed = publisher
        .fail_unsettled_tools(&SessionStructuredError {
            r#type: "tool.execution".to_owned(),
            message: "the tool failed".to_owned(),
            status: None,
            response: None,
            retry_after_ms: None,
        })
        .expect("publishes the failure");
    assert!(failed, "the rejected execution leaves the tool unsettled so it still fails");
}
