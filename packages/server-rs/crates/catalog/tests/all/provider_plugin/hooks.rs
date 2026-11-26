use futures_util::FutureExt;
use opencode_catalog::provider_plugin::hooks::{PromptEvent, ProviderHooks};
use opencode_contract::generated::SessionID;

#[tokio::test]
async fn prompt_hooks_run_in_registration_order_until_they_are_dropped() {
    let hooks = ProviderHooks::new();
    let first = hooks.on_prompt(|mut event| {
        event.session_id.0.push_str("-first");
        async move { event }.boxed()
    });
    let _second = hooks.on_prompt(|mut event| {
        event.session_id.0.push_str("-second");
        async move { event }.boxed()
    });

    let event = hooks.trigger_prompt(PromptEvent { session_id: SessionID("ses".to_owned()) }).await;
    assert_eq!(event.session_id.0, "ses-first-second");

    drop(first);
    let event = hooks.trigger_prompt(PromptEvent { session_id: SessionID("ses".to_owned()) }).await;
    assert_eq!(event.session_id.0, "ses-second");
}
