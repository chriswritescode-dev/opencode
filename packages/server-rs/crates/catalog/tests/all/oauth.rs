use opencode_catalog::oauth::page::{BootstrapOptions, CallbackPageOptions, bootstrap, success};
use opencode_catalog::oauth::redirect::{RedirectError, submit};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

#[test]
fn success_skips_the_auto_close_script_only_when_asked() {
    let auto_close = success(Some(CallbackPageOptions { provider: None, auto_close: None }));
    let stay_open = success(Some(CallbackPageOptions { provider: None, auto_close: Some(false) }));

    assert!(auto_close.contains("<script>setTimeout(function(){try{window.close()}catch(e){}},2500)</script>"));
    assert!(!stay_open.contains("<script>"));
    assert_eq!(
        success(Some(CallbackPageOptions { provider: Some(String::new()), auto_close: None })),
        success(None),
        "an empty provider falls back to the generic message"
    );
}

#[test]
fn escapes_bootstrap_options_embedded_in_the_inline_script() {
    let html = bootstrap(BootstrapOptions {
        token_path: r#"/token</script><script>alert("path")</script>"#.to_owned(),
        provider: Some(r#"xAI</script><script>alert("provider")</script>"#.to_owned()),
    });

    assert_eq!(html.matches("</script>").count(), 1);
    assert!(
        html.contains(r#"xAI\u003c/script>\u003cscript>alert(\"provider\")\u003c/script>"#),
        "the provider is escaped inside the inline script"
    );
    assert!(
        html.contains(r#"/token\u003c/script>\u003cscript>alert(\"path\")\u003c/script>"#),
        "the token path is escaped inside the inline script"
    );
}

#[tokio::test]
async fn submit_sends_the_authorization_code() {
    let (tx, rx) = oneshot::channel();
    let mut callback = Some(tx);
    let result = submit("state-1", "http://127.0.0.1:1455/callback?state=state-1&code=code-1", &mut callback);

    assert_eq!(result, Ok(()));
    assert!(callback.is_none());
    assert_eq!(submit("state-1", "http://127.0.0.1:1455/callback?state=state-1&code=code-1", &mut callback), Ok(()));
    assert_eq!(rx.await, Ok(Ok("code-1".to_owned())));
}

#[tokio::test]
async fn submit_sends_a_provider_error() {
    let (tx, rx) = oneshot::channel();
    let mut callback = Some(tx);
    let result = submit("state-1", "http://127.0.0.1:1455/callback?state=state-1&error=access_denied", &mut callback);

    assert_eq!(result, Ok(()));
    assert!(callback.is_none());
    assert_eq!(rx.await, Ok(Err("access_denied".to_owned())));
}

#[tokio::test]
async fn submit_rejects_a_url_for_another_sign_in() {
    let (tx, mut rx) = oneshot::channel();
    let mut callback = Some(tx);
    let result = submit("state-1", "http://127.0.0.1:1455/callback?state=other&code=code-1", &mut callback);

    assert_eq!(result, Err(RedirectError::WrongState));
    assert!(callback.is_some(), "the sign-in keeps its callback for another paste");
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty), "the sign-in is still waiting for another paste");

    let result = submit("state-1", "http://127.0.0.1:1455/callback?state=state-1&code=code-1", &mut callback);
    assert_eq!(result, Ok(()));
    assert!(callback.is_none());
    assert_eq!(rx.await, Ok(Ok("code-1".to_owned())), "a retry after a bad paste completes the sign-in");
}
