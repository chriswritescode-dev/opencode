use serde_json::json;

use opencode_ai::MapEnv;
use opencode_ai::route::auth::{
    ApiKey, Auth, AuthDescriptor, AuthInput, Credential, ProviderAuthOptions, api_key, bearer,
};
use opencode_ai::schema::{AiError, AiErrorReason};

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
            let value =
                step.model.get("auth").unwrap_or_else(|| panic!("{} step {index}: missing auth", cassette.name));
            let descriptor: AuthDescriptor = serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("{} step {index}: auth descriptor: {error}", cassette.name));
            let auth: Auth = descriptor.clone().into();
            let result = apply(&auth, Vec::new(), &env);
            match descriptor {
                AuthDescriptor::Bearer { .. } => {
                    let headers = result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name));
                    assert_eq!(header(&headers, "authorization"), Some("Bearer fixture"), "{}", cassette.name);
                }
                AuthDescriptor::Header { name, .. } => {
                    let headers = result.unwrap_or_else(|error| panic!("{}: {error}", cassette.name));
                    assert_eq!(header(&headers, &name.to_lowercase()), Some("fixture"), "{}", cassette.name);
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
