use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, stream};
use serde_json::json;

use opencode_ai::MapEnv;
use opencode_ai::route::auth::{
    ApiKey, Auth, AuthDescriptor, AuthInput, Credential, CredentialSource, ProviderAuthOptions, api_key, bearer,
};
use opencode_ai::route::client::LlmClient;
use opencode_ai::route::compile::StreamOptions;
use opencode_ai::route::endpoint::{Endpoint, PathPart};
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::model::{Model, ProtocolRegistry, Route, RouteTransport};
use opencode_ai::schema::{AiError, AiErrorReason, StringMap};
use opencode_ai::{Env, ExecutionContext};

use crate::corpus::Corpus;

fn auth_input(headers: Vec<(String, String)>) -> AuthInput {
    AuthInput {
        method: "POST".to_string(),
        url: "https://api.example.test/v1/messages".to_string(),
        body: "{}".to_string(),
        headers,
        now_ms: 1_767_323_045_000,
    }
}

fn apply(auth: &Auth, headers: Vec<(String, String)>, env: &MapEnv) -> Result<Vec<(String, String)>, AiError> {
    auth.apply(auth_input(headers), env)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(existing, _)| existing == name).map(|(_, value)| value.as_str())
}

#[test]
fn descriptors_render_their_headers() {
    let env = MapEnv::default();
    let bearer: Auth = serde_json::from_value::<AuthDescriptor>(json!({"type": "bearer", "value": "fixture"}))
        .expect("bearer descriptor")
        .into();
    let headers = apply(&bearer, Vec::new(), &env).expect("bearer applies");
    assert_eq!(header(&headers, "authorization"), Some("Bearer fixture"));

    let header_auth: Auth =
        serde_json::from_value::<AuthDescriptor>(json!({"type": "header", "name": "x-api-key", "value": "fixture"}))
            .expect("header descriptor")
            .into();
    let headers = apply(&header_auth, Vec::new(), &env).expect("header applies");
    assert_eq!(header(&headers, "x-api-key"), Some("fixture"));

    let none: Auth = serde_json::from_value::<AuthDescriptor>(json!({"type": "none"})).expect("none descriptor").into();
    assert!(apply(&none, Vec::new(), &env).expect("none applies").is_empty());

    let sigv4: Auth = serde_json::from_value::<AuthDescriptor>(json!({
        "type": "sigv4",
        "region": "us-east-1",
        "accessKeyId": "fixture",
        "secretAccessKey": "fixture"
    }))
    .expect("sigv4 descriptor")
    .into();
    let headers = apply(&sigv4, Vec::new(), &env).expect("sigv4 applies");
    let authorization = header(&headers, "authorization").expect("authorization header");
    assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=fixture/"), "{authorization}");
    assert_eq!(header(&headers, "x-amz-date"), Some("20260102T030405Z"));
}

#[test]
fn credentials_render_every_scheme() {
    let env = MapEnv::default();
    let headers = apply(&Credential::value("secret").bearer_header("x-api-key"), Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "x-api-key"), Some("Bearer secret"));

    let headers = apply(&Credential::value("secret").scheme("Token"), Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Token secret"));

    let headers = apply(&Credential::value("secret").header("X-Custom"), Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "x-custom"), Some("secret"));
}

#[test]
fn missing_and_empty_credentials_are_authentication_errors() {
    let env = MapEnv::default();
    let missing = apply(&Credential::env("MISSING_KEY").bearer(), Vec::new(), &env).unwrap_err();
    assert_eq!(missing.message(), "MISSING_KEY is not set");
    match missing.reason {
        AiErrorReason::Authentication { .. } => {}
        other => panic!("expected authentication error, got {other:?}"),
    }

    let empty = apply(&Credential::value("").bearer(), Vec::new(), &env).unwrap_err();
    assert_eq!(empty.message(), "Missing auth credential: value");

    let empty = apply(&Credential::optional(Some(String::new())).bearer(), Vec::new(), &env).unwrap_err();
    assert_eq!(empty.message(), "Missing auth credential: optional value");
}

#[test]
fn credential_and_auth_fallbacks_compose() {
    let env = MapEnv::new([("SECOND_KEY".to_string(), "from-env".to_string())]);
    let credential = Credential::optional(None).or_else(Credential::env("SECOND_KEY"));
    let headers = apply(&credential.bearer(), Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Bearer from-env"));

    let auth = Credential::optional(None).bearer().or_else(Credential::value("fallback").bearer());
    let headers = apply(&auth, Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Bearer fallback"));

    let auth = Credential::value("one").header("x-one").and_then(Credential::value("two").header("x-two"));
    let headers = apply(&auth, Vec::new(), &env).unwrap();
    assert_eq!(header(&headers, "x-one"), Some("one"));
    assert_eq!(header(&headers, "x-two"), Some("two"));

    let headers = vec![("authorization".to_string(), "old".to_string())];
    let headers = apply(&Auth::remove("authorization"), headers, &env).unwrap();
    assert!(headers.is_empty());

    let headers = vec![("authorization".to_string(), "old".to_string())];
    let headers = apply(&Auth::headers([("authorization".to_string(), "new".to_string())]), headers, &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("new"));
}

#[test]
fn api_key_options_follow_the_typescript_precedence() {
    let env = MapEnv::new([("PROVIDER_KEY".to_string(), "from-env".to_string())]);

    let disabled = ProviderAuthOptions { auth: None, api_key: Some(ApiKey::Disabled) };
    assert!(
        api_key(disabled, &["PROVIDER_KEY"], |credential| credential.bearer())
            .apply(auth_input(Vec::new()), &env)
            .unwrap()
            .is_empty()
    );

    let explicit = ProviderAuthOptions { auth: None, api_key: Some(ApiKey::Value("from-option".to_string())) };
    let headers = bearer(explicit, &["PROVIDER_KEY"]).apply(auth_input(Vec::new()), &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Bearer from-option"));

    let from_env = ProviderAuthOptions { auth: None, api_key: None };
    let headers = bearer(from_env, &["PROVIDER_KEY"]).apply(auth_input(Vec::new()), &env).unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Bearer from-env"));

    let override_auth = ProviderAuthOptions {
        auth: Some(Auth::headers([("x-override".to_string(), "1".to_string())])),
        api_key: Some(ApiKey::Value("ignored".to_string())),
    };
    let headers = bearer(override_auth, &["PROVIDER_KEY"]).apply(auth_input(Vec::new()), &env).unwrap();
    assert_eq!(header(&headers, "x-override"), Some("1"));
}

#[test]
fn corpus_auth_descriptors_resolve() {
    let corpus = Corpus::load();
    let env = MapEnv::default();
    let mut seen = 0usize;
    for cassette in &corpus.cassettes {
        for (index, step) in cassette.steps.iter().enumerate() {
            if step.operation.is_some() {
                continue;
            }
            let value =
                step.model.get("auth").unwrap_or_else(|| panic!("{} step {index}: missing auth", cassette.name));
            let descriptor: AuthDescriptor = serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("{} step {index}: auth descriptor: {error}", cassette.name));
            let auth: Auth = descriptor.clone().into();
            let result = apply(&auth, Vec::new(), &env);
            match descriptor {
                AuthDescriptor::Bearer { value } => {
                    let headers = result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name));
                    let expected = format!("Bearer {value}");
                    assert_eq!(header(&headers, "authorization"), Some(expected.as_str()), "{}", cassette.name);
                }
                AuthDescriptor::Header { name, value } => {
                    let headers = result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name));
                    assert_eq!(header(&headers, &name.to_lowercase()), Some(value.as_str()), "{}", cassette.name);
                }
                AuthDescriptor::None => {
                    assert!(
                        result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name)).is_empty(),
                        "{}",
                        cassette.name
                    );
                }
                AuthDescriptor::Sigv4 { .. } => {
                    let headers = result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name));
                    let authorization = header(&headers, "authorization").expect("authorization header");
                    assert!(
                        authorization.starts_with("AWS4-HMAC-SHA256 Credential=fixture/"),
                        "{}: {authorization}",
                        cassette.name
                    );
                }
            }
            seen += 1;
        }
    }
    assert!(seen > 0, "corpus has no auth descriptors");
}

#[derive(Debug)]
struct RecordingSource {
    label: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
    secret: Option<String>,
}

impl CredentialSource for RecordingSource {
    fn load(&self, _env: &dyn Env) -> Result<String, AiError> {
        match &self.secret {
            Some(secret) => Ok(secret.clone()),
            None => panic!("stub source load must not run"),
        }
    }

    fn prepare(&self, _context: &ExecutionContext, _sender: &Arc<dyn HttpSender>) -> BoxFuture<Result<(), AiError>> {
        let label = self.label.to_string();
        let calls = self.calls.clone();
        let fails = self.secret.is_none();
        Box::pin(async move {
            calls.lock().expect("prepare calls").push(label);
            if fails { Err(authentication_error("stub prepare failed")) } else { Ok(()) }
        })
    }
}

fn authentication_error(message: &str) -> AiError {
    AiError { reason: AiErrorReason::Authentication { message: message.to_string(), body: None, http: None } }
}

struct PanickingSender;

impl HttpSender for PanickingSender {
    fn send(&self, _request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        panic!("the sender must not run")
    }
}

struct RecordingSender {
    requests: Mutex<Vec<PreparedHttp>>,
}

impl HttpSender for RecordingSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        self.requests.lock().expect("recorded requests").push(request);
        Box::pin(async {
            let body: ByteStream = Box::pin(stream::empty());
            Ok(HttpResponse::new(200, Vec::new(), body))
        })
    }
}

fn source(label: &'static str, calls: &Arc<Mutex<Vec<String>>>, secret: Option<&str>) -> Arc<RecordingSource> {
    Arc::new(RecordingSource { label, calls: calls.clone(), secret: secret.map(str::to_string) })
}

fn recorded_calls(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    calls.lock().expect("prepare calls").clone()
}

fn stub_model(auth: Auth) -> Model {
    Model {
        id: "test-model".to_string(),
        provider: "test".to_string(),
        options: None,
        route: Route {
            id: "test".to_string(),
            protocol: ProtocolRegistry::builtin().get("openai-chat").expect("openai-chat protocol"),
            provider_metadata_key: None,
            endpoint: Endpoint {
                base_url: Some("https://api.test/v1".to_string()),
                path: PathPart::Static("/chat/completions".to_string()),
                query: StringMap::new(),
            },
            auth,
            headers: None,
            body: None,
            channel: None,
            transport: RouteTransport::Http,
            options: None,
        },
        compatibility: None,
        capabilities: None,
    }
}

#[tokio::test]
async fn prepare_prepares_the_first_credential_branch_that_succeeds() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sender: Arc<dyn HttpSender> = Arc::new(PanickingSender);
    let context = crate::compile::context();

    let credential = Credential::Source(source("first", &calls, Some("one"))).or_else(Credential::Source(source(
        "second",
        &calls,
        Some("two"),
    )));
    credential.bearer().prepare(&context, &sender).await.expect("first branch prepares");
    assert_eq!(recorded_calls(&calls), vec!["first".to_string()]);

    calls.lock().expect("prepare calls").clear();

    let credential = Credential::Source(source("first", &calls, None)).or_else(Credential::Source(source(
        "second",
        &calls,
        Some("two"),
    )));
    credential.bearer().prepare(&context, &sender).await.expect("second branch prepares after an error");
    assert_eq!(recorded_calls(&calls), vec!["first".to_string(), "second".to_string()]);

    calls.lock().expect("prepare calls").clear();

    let credential = Credential::env("UNSET").or_else(Credential::Source(source("second", &calls, Some("two"))));
    credential.bearer().prepare(&context, &sender).await.expect("an unset env credential falls through");
    assert_eq!(recorded_calls(&calls), vec!["second".to_string()]);

    calls.lock().expect("prepare calls").clear();

    let credential =
        Credential::Source(source("first", &calls, None)).or_else(Credential::Source(source("second", &calls, None)));
    let error = credential.bearer().prepare(&context, &sender).await.expect_err("both branches fail");
    assert_eq!(error.message(), "stub prepare failed");
    assert_eq!(recorded_calls(&calls), vec!["first".to_string(), "second".to_string()]);
}

#[tokio::test]
async fn prepare_sequences_and_then_auth_in_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sender: Arc<dyn HttpSender> = Arc::new(PanickingSender);
    let context = crate::compile::context();

    let auth = Credential::Source(source("first", &calls, Some("one")))
        .bearer()
        .and_then(Credential::Source(source("second", &calls, Some("two"))).bearer());
    auth.prepare(&context, &sender).await.expect("both credentials prepare");
    assert_eq!(recorded_calls(&calls), vec!["first".to_string(), "second".to_string()]);

    calls.lock().expect("prepare calls").clear();

    let auth = Credential::Source(source("first", &calls, None))
        .bearer()
        .and_then(Credential::Source(source("second", &calls, Some("two"))).bearer());
    let error = auth.prepare(&context, &sender).await.expect_err("the first credential fails");
    assert_eq!(error.message(), "stub prepare failed");
    assert_eq!(recorded_calls(&calls), vec!["first".to_string()]);
}

#[tokio::test]
async fn stream_reports_prepare_failures_without_sending() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let context = crate::compile::context();
    let model = stub_model(Credential::Source(source("source", &calls, None)).bearer());
    let sender: Arc<dyn HttpSender> = Arc::new(PanickingSender);
    let client = LlmClient::new(RequestExecutor::new(sender, None, context.clock.clone()), context);

    let mut items = client
        .stream(&model, crate::compile::request("Hi"), StreamOptions::default())
        .collect::<Vec<Result<opencode_ai::schema::LlmEvent, AiError>>>()
        .await;
    assert_eq!(items.len(), 1, "a prepare failure produces one item");
    let error = match items.pop().expect("one item") {
        Err(error) => error,
        Ok(_) => panic!("expected the prepare failure"),
    };
    match error.reason {
        AiErrorReason::Authentication { .. } => {}
        other => panic!("expected authentication error, got {other:?}"),
    }
    assert_eq!(error.message(), "stub prepare failed");
    assert_eq!(recorded_calls(&calls), vec!["source".to_string()]);
}

#[tokio::test]
async fn stream_prepares_the_source_before_rendering_auth() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let context = crate::compile::context();
    let model = stub_model(Credential::Source(source("source", &calls, Some("fixture"))).bearer());
    let sender = Arc::new(RecordingSender { requests: Mutex::new(Vec::new()) });
    let client = LlmClient::new(RequestExecutor::new(sender.clone(), None, context.clock.clone()), context);

    let _items =
        client.stream(&model, crate::compile::request("Hi"), StreamOptions::default()).collect::<Vec<_>>().await;
    assert_eq!(recorded_calls(&calls), vec!["source".to_string()]);

    let requests = sender.requests.lock().expect("recorded requests");
    assert_eq!(requests.len(), 1, "the prepared request is sent once");
    let headers: BTreeMap<String, String> =
        requests[0].headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect();
    assert_eq!(headers.get("authorization").map(String::as_str), Some("Bearer fixture"));
}
