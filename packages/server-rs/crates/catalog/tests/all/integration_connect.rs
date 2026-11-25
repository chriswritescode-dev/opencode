use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::collections::HashMap;

use opencode_catalog::integration::{
    AttemptError, AttemptMode, AttemptStatus, AuthorizationError, AutoCallback, CodeCallback, CommandAttemptStatus,
    CommandConnectInput, CommandError, ExternalInput, Implementation, KeyInput, LabelCallback, OAuthAuthorization,
    OAuthConnectInput, OAuthFuture, OAuthImplementation, OAuthMode, SubmitCallback,
};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType,
    CredentialValue, FormAnswer, FormField1, FormFields1, FormStringField1, FormStringField1Type, FormValue,
    IntegrationAttemptID, IntegrationExternalMethod, IntegrationExternalMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType,
};
#[cfg(unix)]
use opencode_contract::generated::{IntegrationCommandMethod, IntegrationCommandMethodType};
use opencode_util::time::now_ms;
use serde_json::json;
use tokio::sync::Semaphore;

#[cfg(unix)]
use crate::integration::setup_with_env;
use crate::integration::{integration_id, setup};

type AuthorizeFn = Arc<dyn Fn(FormAnswer) -> OAuthFuture<OAuthAuthorization> + Send + Sync>;
type LabelFn = Arc<dyn Fn(&CredentialOAuth) -> Option<String> + Send + Sync>;

struct ScriptedOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    authorize: AuthorizeFn,
    label: Option<LabelFn>,
}

impl OAuthImplementation for ScriptedOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        (self.authorize)(answer)
    }

    fn label(&self) -> Option<LabelCallback> {
        let label = self.label.clone()?;
        Some(Box::new(move |value| label(value)))
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn scripted(
    integration: &IntegrationID,
    method: IntegrationOAuthMethod,
    authorize: impl Fn(FormAnswer) -> OAuthFuture<OAuthAuthorization> + Send + Sync + 'static,
    label: Option<LabelFn>,
) -> Implementation {
    Implementation::OAuth(Arc::new(ScriptedOAuth {
        integration_id: integration.clone(),
        method,
        authorize: Arc::new(authorize),
        label,
    }))
}

fn oauth_method(id: &str, label: &str) -> IntegrationOAuthMethod {
    IntegrationOAuthMethod {
        id: IntegrationMethodID(id.to_owned()),
        r#type: IntegrationOAuthMethodType::Oauth,
        label: label.to_owned(),
        form: None,
    }
}

fn key_method(form: Option<FormFields1>) -> IntegrationMethod {
    IntegrationMethod::Key(IntegrationKeyMethod {
        r#type: IntegrationKeyMethodType::Key,
        label: Some("API key".to_owned()),
        form,
    })
}

fn external_method(id: &str, form: Option<FormFields1>) -> IntegrationMethod {
    IntegrationMethod::External(IntegrationExternalMethod {
        id: IntegrationMethodID(id.to_owned()),
        r#type: IntegrationExternalMethodType::External,
        label: "Account".to_owned(),
        form,
    })
}

#[cfg(unix)]
fn command_method(id: &str, command: &[&str]) -> IntegrationMethod {
    IntegrationMethod::Command(IntegrationCommandMethod {
        id: IntegrationMethodID(id.to_owned()),
        r#type: IntegrationCommandMethodType::Command,
        label: "Log in".to_owned(),
        command: command.iter().map(|part| (*part).to_owned()).collect(),
    })
}

fn required_string_form(key: &str) -> FormFields1 {
    FormFields1(vec![FormField1::String(FormStringField1 {
        key: key.to_owned(),
        title: None,
        description: None,
        required: Some(true),
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: None,
        max_length: None,
        pattern: None,
        placeholder: None,
        default: None,
        options: None,
        custom: None,
    })])
}

fn answer(values: &[(&str, FormValue)]) -> FormAnswer {
    JsonMap(values.iter().map(|(key, value)| ((*key).to_owned(), value.clone())).collect())
}

fn oauth_value(method: &IntegrationMethodID, access: &str) -> CredentialOAuth {
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method.clone(),
        access: access.to_owned(),
        refresh: "refresh".to_owned(),
        expires: 1.0,
        metadata: None,
    }
}

fn code_oauth(method: &IntegrationMethodID, code: &str) -> CredentialOAuth {
    CredentialOAuth { metadata: Some(JsonMap(vec![("code".to_owned(), json!(code))])), ..oauth_value(method, "access") }
}

fn pending_callback() -> AutoCallback {
    Box::new(|| Box::pin(std::future::pending::<Result<CredentialOAuth, AuthorizationError>>()))
}

fn never_code_callback() -> CodeCallback {
    Box::new(|_| Box::pin(std::future::pending::<Result<CredentialOAuth, AuthorizationError>>()))
}

async fn eventually<T, F, Fut>(mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..2000 {
        if let Some(value) = check().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the condition was not reached");
}

async fn wait_flag(flag: &AtomicBool) {
    for _ in 0..2000 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the flag was not set");
}

async fn settle_runtime() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn connects_with_a_key_and_stores_the_credential() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: key_method(Some(required_string_form("accountId"))),
                });
                Ok(())
            }
        })
        .await;

    let error = service
        .connection()
        .key(KeyInput { integration_id: integration.clone(), key: "secret".to_owned(), answer: None, label: None })
        .await
        .expect_err("the key is rejected without the form answer");
    assert_eq!(error.message(), "Missing required form field: accountId");

    service
        .connection()
        .key(KeyInput {
            integration_id: integration.clone(),
            key: "secret".to_owned(),
            answer: Some(answer(&[("accountId", FormValue::String("account".to_owned()))])),
            label: Some("Work".to_owned()),
        })
        .await
        .expect("connect with the key");

    let stored = credentials.list(&integration).expect("list the credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].label, "Work");
    assert_eq!(
        stored[0].value,
        CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: "secret".to_owned(),
            metadata: Some(JsonMap(vec![("accountId".to_owned(), json!("account"))])),
        })
    );

    let _no_form = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: key_method(None),
                });
                Ok(())
            }
        })
        .await;

    let error = service
        .connection()
        .key(KeyInput {
            integration_id: integration.clone(),
            key: "secret".to_owned(),
            answer: Some(answer(&[("accountId", FormValue::String("account".to_owned()))])),
            label: None,
        })
        .await
        .expect_err("the key method rejects a form answer");
    assert_eq!(error.message(), "Key method does not accept a form answer");
}

#[tokio::test]
async fn names_unlabeled_credentials_after_the_integration() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.update(&integration, |info| info.name = "OpenAI".to_owned());
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: key_method(None),
                });
                Ok(())
            }
        })
        .await;

    for (key, label) in [("first", None), ("second", None), ("work", Some("Work")), ("third", None)] {
        service
            .connection()
            .key(KeyInput {
                integration_id: integration.clone(),
                key: key.to_owned(),
                answer: None,
                label: label.map(str::to_owned),
            })
            .await
            .expect("connect with the key");
    }

    let labels: Vec<String> =
        credentials.list(&integration).expect("list the credentials").into_iter().map(|info| info.label).collect();
    assert_eq!(labels, vec!["OpenAI", "OpenAI 2", "Work", "OpenAI 3"]);
}

#[tokio::test]
async fn connects_an_external_method() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("acme");
    let method_id = IntegrationMethodID("account".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: external_method("account", Some(required_string_form("accountId"))),
                });
                Ok(())
            }
        })
        .await;

    let error = service
        .connection()
        .external(ExternalInput {
            integration_id: integration.clone(),
            method_id: method_id.clone(),
            answer: None,
            label: None,
        })
        .await
        .expect_err("the external method is rejected without the form answer");
    assert_eq!(error.message(), "Missing required form field: accountId");

    service
        .connection()
        .external(ExternalInput {
            integration_id: integration.clone(),
            method_id: method_id.clone(),
            answer: Some(answer(&[("accountId", FormValue::String("account".to_owned()))])),
            label: Some("Work".to_owned()),
        })
        .await
        .expect("connect the external method");

    let stored = credentials.list(&integration).expect("list the credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].label, "Work");
    assert_eq!(
        stored[0].value,
        CredentialValue::External(CredentialExternal {
            r#type: CredentialExternalType::External,
            method_id,
            metadata: Some(JsonMap(vec![("accountId".to_owned(), json!("account"))])),
        })
    );
}

#[tokio::test]
async fn completes_code_oauth_once_and_stores_the_credential() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("chatgpt".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            move |editor| {
                let method_id = method_id.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("chatgpt", "ChatGPT"),
                    move |_| {
                        let method_id = method_id.clone();
                        Box::pin(async move {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Paste the code".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Code {
                                    callback: Box::new(move |code| {
                                        let method_id = method_id.clone();
                                        Box::pin(async move { Ok(code_oauth(&method_id, &code)) })
                                    }),
                                },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput {
            integration_id: integration.clone(),
            method_id: method_id.clone(),
            answer: None,
            label: Some("Personal".to_owned()),
        })
        .await
        .expect("connect the oauth method");
    assert_eq!(attempt.mode, AttemptMode::Code);

    service.oauth().complete(&integration, &attempt.attempt_id, Some("1234")).await.expect("complete the attempt");

    let stored = credentials.list(&integration).expect("list the credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].label, "Personal");
    assert_eq!(stored[0].value, CredentialValue::Oauth(code_oauth(&method_id, "1234")));
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Complete { time: attempt.time }
    );
}

#[tokio::test]
async fn keeps_code_attempts_open_when_the_code_is_missing_and_closes_them_on_cancel() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let other = integration_id("other");
    let closed = Arc::new(AtomicBool::new(false));
    let _registration = service
        .transform({
            let integration = integration.clone();
            let closed = closed.clone();
            move |editor| {
                let closed = closed.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("chatgpt", "ChatGPT"),
                    move |_| {
                        let closed = closed.clone();
                        Box::pin(async move {
                            let guard = DropFlag(closed);
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Paste the code".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Code {
                                    callback: Box::new(move |_| {
                                        let _held = &guard;
                                        Box::pin(std::future::pending::<Result<CredentialOAuth, AuthorizationError>>())
                                    }),
                                },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput {
            integration_id: integration.clone(),
            method_id: IntegrationMethodID("chatgpt".to_owned()),
            answer: None,
            label: None,
        })
        .await
        .expect("connect the oauth method");

    let error =
        service.oauth().complete(&integration, &attempt.attempt_id, None).await.expect_err("the code is required");
    assert!(matches!(error, AttemptError::CodeRequired { .. }));
    assert!(!closed.load(Ordering::SeqCst));

    service.oauth().cancel(&other, &attempt.attempt_id).await;
    assert!(!closed.load(Ordering::SeqCst));
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Pending { time: attempt.time }
    );

    service.oauth().cancel(&integration, &attempt.attempt_id).await;
    assert!(closed.load(Ordering::SeqCst));
    assert!(matches!(
        service.oauth().status(&integration, &attempt.attempt_id).await,
        Err(AttemptError::AttemptNotFound { .. })
    ));
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[tokio::test]
async fn completes_auto_oauth_in_the_background() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("browser".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            move |editor| {
                let method_id = method_id.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("browser", "Browser"),
                    move |_| {
                        let method_id = method_id.clone();
                        Box::pin(async move {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Sign in".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Auto {
                                    callback: Box::new(move || {
                                        let method_id = method_id.clone();
                                        Box::pin(async move { Ok(oauth_value(&method_id, "access")) })
                                    }),
                                    submit: None,
                                },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");
    assert_eq!(attempt.mode, AttemptMode::Auto);

    let status = eventually(|| {
        let service = service.clone();
        let integration = integration.clone();
        let attempt_id = attempt.attempt_id.clone();
        async move {
            let status = service.oauth().status(&integration, &attempt_id).await.ok()?;
            matches!(&status, AttemptStatus::Complete { .. }).then_some(status)
        }
    })
    .await;
    assert_eq!(status, AttemptStatus::Complete { time: attempt.time });
    assert_eq!(credentials.list(&integration).expect("list the credentials").len(), 1);
}

#[tokio::test]
async fn completes_auto_oauth_through_submit() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("browser".to_owned());
    let release = Arc::new(Semaphore::new(0));
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            let release = release.clone();
            let seen = seen.clone();
            move |editor| {
                let method_id = method_id.clone();
                let release = release.clone();
                let seen = seen.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("browser", "Browser"),
                    move |_| {
                        let method_id = method_id.clone();
                        let release = release.clone();
                        let seen = seen.clone();
                        let callback: AutoCallback = Box::new({
                            let method_id = method_id.clone();
                            let release = release.clone();
                            move || {
                                let method_id = method_id.clone();
                                let release = release.clone();
                                Box::pin(async move {
                                    release.acquire().await.expect("release the auto callback").forget();
                                    Ok(oauth_value(&method_id, "access"))
                                })
                            }
                        });
                        let submit: SubmitCallback = Box::new(move |code| {
                            *seen.lock().expect("lock the submitted code") = Some(code);
                            release.add_permits(1);
                            Box::pin(async { Ok(()) })
                        });
                        Box::pin(async move {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Sign in".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Auto { callback, submit: Some(submit) },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");
    assert_eq!(attempt.mode, AttemptMode::Auto);
    assert!(attempt.paste);

    service.oauth().complete(&integration, &attempt.attempt_id, Some("1234")).await.expect("submit the code");

    let status = eventually(|| {
        let service = service.clone();
        let integration = integration.clone();
        let attempt_id = attempt.attempt_id.clone();
        async move {
            let status = service.oauth().status(&integration, &attempt_id).await.ok()?;
            matches!(&status, AttemptStatus::Complete { .. }).then_some(status)
        }
    })
    .await;
    assert_eq!(status, AttemptStatus::Complete { time: attempt.time });
    assert_eq!(credentials.list(&integration).expect("list the credentials").len(), 1);
    assert_eq!(seen.lock().expect("lock the submitted code").as_deref(), Some("1234"));
}

#[tokio::test(start_paused = true)]
async fn expires_abandoned_oauth_attempts() {
    let (_dir, _database, _bus, credentials, service) = setup();
    settle_runtime().await;
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("browser".to_owned());
    let closed = Arc::new(AtomicBool::new(false));
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            let closed = closed.clone();
            move |editor| {
                let method_id = method_id.clone();
                let closed = closed.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("browser", "Browser"),
                    move |_| {
                        let method_id = method_id.clone();
                        let closed = closed.clone();
                        Box::pin(async move {
                            let callback: AutoCallback = Box::new(move || {
                                let method_id = method_id.clone();
                                let flag = closed.clone();
                                Box::pin(async move {
                                    let _guard = DropFlag(flag);
                                    std::future::pending::<()>().await;
                                    Ok(oauth_value(&method_id, "access"))
                                })
                            });
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Sign in".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Auto { callback, submit: None },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");
    assert_eq!(attempt.time.expires - attempt.time.created, 600_000);
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Pending { time: attempt.time }
    );

    tokio::time::advance(Duration::from_secs(600)).await;
    let status = eventually(|| {
        let service = service.clone();
        let integration = integration.clone();
        let attempt_id = attempt.attempt_id.clone();
        async move {
            let status = service.oauth().status(&integration, &attempt_id).await.ok()?;
            matches!(&status, AttemptStatus::Expired { .. }).then_some(status)
        }
    })
    .await;
    assert_eq!(status, AttemptStatus::Expired { time: attempt.time });
    wait_flag(&closed).await;
    assert!(closed.load(Ordering::SeqCst));
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[tokio::test]
async fn uses_provider_defined_oauth_attempt_expirations() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    let integration = integration_id("openai");
    let created = now_ms();
    let expirations = [created + 5 * 60 * 1000, created + 20 * 60 * 1000];
    for (index, expires_at) in expirations.into_iter().enumerate() {
        let method_id = IntegrationMethodID(format!("browser-{index}"));
        let _registration = service
            .transform({
                let integration = integration.clone();
                let method_id = method_id.clone();
                move |editor| {
                    let method_id = method_id.clone();
                    editor.method_update(scripted(
                        &integration,
                        oauth_method(&method_id.0, "Browser"),
                        move |_| {
                            Box::pin(async move {
                                Ok(OAuthAuthorization {
                                    url: "https://example.com/authorize".to_owned(),
                                    instructions: "Sign in".to_owned(),
                                    expires_at: Some(expires_at),
                                    mode: OAuthMode::Auto { callback: pending_callback(), submit: None },
                                })
                            })
                        },
                        None,
                    ));
                    Ok(())
                }
            })
            .await;

        let attempt = service
            .oauth()
            .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
            .await
            .expect("connect the oauth method");
        assert_eq!(attempt.time.expires, expires_at);
    }
}

#[tokio::test(start_paused = true)]
async fn retains_finished_attempts_for_one_minute() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    settle_runtime().await;
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("chatgpt".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            move |editor| {
                let method_id = method_id.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("chatgpt", "ChatGPT"),
                    move |_| {
                        let method_id = method_id.clone();
                        Box::pin(async move {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Paste the code".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Code {
                                    callback: Box::new(move |code| {
                                        let method_id = method_id.clone();
                                        Box::pin(async move { Ok(code_oauth(&method_id, &code)) })
                                    }),
                                },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");
    service.oauth().complete(&integration, &attempt.attempt_id, Some("1234")).await.expect("complete the attempt");
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Complete { time: attempt.time }
    );

    let mut removed = false;
    for tick in 0..4 {
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        let status = service.oauth().status(&integration, &attempt.attempt_id).await;
        if tick == 0 {
            assert_eq!(
                status.as_ref().expect("the attempt is retained"),
                &AttemptStatus::Complete { time: attempt.time }
            );
        }
        if matches!(&status, Err(AttemptError::AttemptNotFound { .. })) {
            removed = true;
            break;
        }
    }
    assert!(removed, "the attempt was not removed after the retention window");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_cancel_while_persisting() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let method_id = IntegrationMethodID("chatgpt".to_owned());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let label: LabelFn = {
        let entered = entered.clone();
        let release = release.clone();
        Arc::new(move |_| {
            entered.wait();
            release.wait();
            Some("Scripted".to_owned())
        })
    };
    let _registration = service
        .transform({
            let integration = integration.clone();
            let method_id = method_id.clone();
            move |editor| {
                let method_id = method_id.clone();
                let label = label.clone();
                editor.method_update(scripted(
                    &integration,
                    oauth_method("chatgpt", "ChatGPT"),
                    move |_| {
                        let method_id = method_id.clone();
                        Box::pin(async move {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Paste the code".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Code {
                                    callback: Box::new(move |_| {
                                        let method_id = method_id.clone();
                                        Box::pin(async move { Ok(oauth_value(&method_id, "access")) })
                                    }),
                                },
                            })
                        })
                    },
                    Some(label),
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");

    let completion = tokio::spawn({
        let service = service.clone();
        let integration = integration.clone();
        let attempt_id = attempt.attempt_id.clone();
        async move { service.oauth().complete(&integration, &attempt_id, Some("1234")).await }
    });
    tokio::task::spawn_blocking({
        let entered = entered.clone();
        move || {
            entered.wait();
        }
    })
    .await
    .expect("wait for the label to start");

    service.oauth().cancel(&integration, &attempt.attempt_id).await;
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Pending { time: attempt.time }
    );

    tokio::task::spawn_blocking({
        let release = release.clone();
        move || {
            release.wait();
        }
    })
    .await
    .expect("release the label");

    completion.await.expect("join the completion").expect("complete the attempt");
    assert_eq!(
        service.oauth().status(&integration, &attempt.attempt_id).await.expect("read the attempt"),
        AttemptStatus::Complete { time: attempt.time }
    );
    let stored = credentials.list(&integration).expect("list the credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].label, "Scripted");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn runs_command_authentication_and_stores_the_final_output() {
    let (dir, _database, _bus, credentials, service) =
        setup_with_env(HashMap::from([("COMMAND_TEST_SECRET".to_owned(), "secret".to_owned())]));
    let integration = integration_id("company");
    let method_id = IntegrationMethodID("login".to_owned());
    let release = dir.path().join("release");
    let release = release.to_str().expect("the release path is utf-8").to_owned();
    let command = [
        "/bin/sh",
        "-c",
        "printf 'https://example.com/login\\n' >&2; while [ ! -f \"$1\" ]; do :; done; printf '%s' \"$COMMAND_TEST_SECRET\"",
        "sh",
        release.as_str(),
    ];
    let method = command_method("login", &command);
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: method.clone(),
                });
                Ok(())
            }
        })
        .await;

    let attempt = service
        .command()
        .connect(CommandConnectInput { integration_id: integration.clone(), method_id, label: Some("Work".to_owned()) })
        .await
        .expect("connect the command attempt");

    let pending = tokio::time::timeout(
        Duration::from_secs(10),
        eventually(|| {
            let service = service.clone();
            let integration = integration.clone();
            let attempt_id = attempt.attempt_id.clone();
            async move {
                let status = service.command().status(&integration, &attempt_id).await.ok()?;
                match &status {
                    CommandAttemptStatus::Pending { message: Some(message), .. }
                        if message == "https://example.com/login\n" =>
                    {
                        Some(status)
                    }
                    _ => None,
                }
            }
        }),
    )
    .await
    .expect("the command reports its login url");
    assert_eq!(
        pending,
        CommandAttemptStatus::Pending { message: Some("https://example.com/login\n".to_owned()), time: attempt.time }
    );

    std::fs::write(&release, "").expect("create the release file");

    let complete = tokio::time::timeout(
        Duration::from_secs(10),
        eventually(|| {
            let service = service.clone();
            let integration = integration.clone();
            let attempt_id = attempt.attempt_id.clone();
            async move {
                let status = service.command().status(&integration, &attempt_id).await.ok()?;
                matches!(&status, CommandAttemptStatus::Complete { .. }).then_some(status)
            }
        }),
    )
    .await
    .expect("the command completes");
    assert_eq!(complete, CommandAttemptStatus::Complete { time: attempt.time });

    let stored = credentials.list(&integration).expect("list the credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].label, "Work");
    assert_eq!(
        stored[0].value,
        CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: "secret".to_owned(),
            metadata: None,
        })
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn fails_command_authentication_with_the_trimmed_stderr() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("company");
    let method_id = IntegrationMethodID("login".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: command_method("login", &["/bin/sh", "-c", "printf 'bad credentials\\n' >&2; exit 3"]),
                });
                Ok(())
            }
        })
        .await;

    let attempt = service
        .command()
        .connect(CommandConnectInput { integration_id: integration.clone(), method_id, label: None })
        .await
        .expect("connect the command attempt");

    let failed = tokio::time::timeout(
        Duration::from_secs(10),
        eventually(|| {
            let service = service.clone();
            let integration = integration.clone();
            let attempt_id = attempt.attempt_id.clone();
            async move {
                let status = service.command().status(&integration, &attempt_id).await.ok()?;
                matches!(&status, CommandAttemptStatus::Failed { .. }).then_some(status)
            }
        }),
    )
    .await
    .expect("the command fails");
    match failed {
        CommandAttemptStatus::Failed { message, time } => {
            assert_eq!(message, "bad credentials");
            assert_eq!(time, attempt.time);
        }
        other => panic!("expected a failed attempt, got {other:?}"),
    }
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn fails_when_the_command_returns_no_credential() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("company");
    let method_id = IntegrationMethodID("login".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: command_method("login", &["/bin/sh", "-c", "exit 0"]),
                });
                Ok(())
            }
        })
        .await;

    let attempt = service
        .command()
        .connect(CommandConnectInput { integration_id: integration.clone(), method_id, label: None })
        .await
        .expect("connect the command attempt");

    let failed = tokio::time::timeout(
        Duration::from_secs(10),
        eventually(|| {
            let service = service.clone();
            let integration = integration.clone();
            let attempt_id = attempt.attempt_id.clone();
            async move {
                let status = service.command().status(&integration, &attempt_id).await.ok()?;
                matches!(&status, CommandAttemptStatus::Failed { .. }).then_some(status)
            }
        }),
    )
    .await
    .expect("the command fails");
    match failed {
        CommandAttemptStatus::Failed { message, time } => {
            assert_eq!(message, "Authentication command returned no credential");
            assert_eq!(time, attempt.time);
        }
        other => panic!("expected a failed attempt, got {other:?}"),
    }
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn cancels_command_attempts() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("company");
    let method_id = IntegrationMethodID("login".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: integration.clone(),
                    method: command_method("login", &["/bin/sh", "-c", "while :; do :; done"]),
                });
                Ok(())
            }
        })
        .await;

    let attempt = service
        .command()
        .connect(CommandConnectInput { integration_id: integration.clone(), method_id, label: None })
        .await
        .expect("connect the command attempt");

    tokio::time::timeout(
        Duration::from_secs(10),
        eventually(|| {
            let service = service.clone();
            let integration = integration.clone();
            let attempt_id = attempt.attempt_id.clone();
            async move {
                let status = service.command().status(&integration, &attempt_id).await.ok()?;
                matches!(&status, CommandAttemptStatus::Pending { .. }).then_some(status)
            }
        }),
    )
    .await
    .expect("the command is pending");

    service.command().cancel(&integration, &attempt.attempt_id).await;
    assert!(matches!(
        service.command().status(&integration, &attempt.attempt_id).await,
        Err(CommandError::AttemptNotFound { .. })
    ));
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[tokio::test]
async fn reports_unknown_attempts() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    let integration = integration_id("openai");
    let other = integration_id("other");
    let missing = IntegrationAttemptID("con_missing".to_owned());

    assert!(matches!(service.oauth().status(&integration, &missing).await, Err(AttemptError::AttemptNotFound { .. })));
    assert!(matches!(
        service.command().status(&integration, &missing).await,
        Err(CommandError::AttemptNotFound { .. })
    ));

    let method_id = IntegrationMethodID("chatgpt".to_owned());
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(scripted(
                    &integration,
                    oauth_method("chatgpt", "ChatGPT"),
                    |_| {
                        Box::pin(async {
                            Ok(OAuthAuthorization {
                                url: "https://example.com/authorize".to_owned(),
                                instructions: "Paste the code".to_owned(),
                                expires_at: None,
                                mode: OAuthMode::Code { callback: never_code_callback() },
                            })
                        })
                    },
                    None,
                ));
                Ok(())
            }
        })
        .await;

    let attempt = service
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration.clone(), method_id, answer: None, label: None })
        .await
        .expect("connect the oauth method");
    assert!(matches!(
        service.oauth().status(&other, &attempt.attempt_id).await,
        Err(AttemptError::AttemptNotFound { .. })
    ));
}
