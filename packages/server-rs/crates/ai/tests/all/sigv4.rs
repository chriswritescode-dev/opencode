use opencode_ai::MapEnv;
use opencode_ai::protocols::utils::bedrock_auth::{
    AuthMode, Credentials, ResolveAuthInput, ResolveAuthOptions, resolve_auth, resolve_region,
};
use opencode_ai::route::auth::{Auth, AuthDescriptor, AuthInput, SigV4Source};
use opencode_ai::schema::AiErrorReason;

use crate::corpus::Corpus;

fn apply(auth: &Auth, input: AuthInput, env: &MapEnv) -> Vec<(String, String)> {
    match auth.apply(input, env) {
        Ok(headers) => headers,
        Err(error) => panic!("auth failed: {error}"),
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(existing, _)| existing == name).map(|(_, value)| value.as_str())
}

fn signed_headers(authorization: &str) -> &str {
    let (_, rest) = authorization.split_once("SignedHeaders=").expect("signed headers in authorization");
    let (signed, _) = rest.split_once(',').expect("signature in authorization");
    signed
}

#[test]
fn corpus_sigv4_signatures_match() {
    let corpus = Corpus::load();
    let env = MapEnv::default();
    let mut signed = 0usize;
    for cassette in &corpus.cassettes {
        for step in &cassette.steps {
            let Some(value) = step.model.get("auth") else {
                continue;
            };
            let descriptor = match AuthDescriptor::from_value(value) {
                Ok(descriptor) => descriptor,
                Err(error) => panic!("{}: auth descriptor: {error}", cassette.name),
            };
            if !matches!(descriptor, AuthDescriptor::Sigv4 { .. }) {
                continue;
            }
            let auth: Auth = descriptor.into();
            for exchange in step.exchanges.iter().flatten() {
                let prepared = &exchange.prepared;
                let headers = prepared
                    .headers
                    .iter()
                    .filter(|(name, _)| name.as_str() != "authorization")
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                let input = AuthInput {
                    method: prepared.method.clone(),
                    url: prepared.url.clone(),
                    body: prepared.body.clone(),
                    headers,
                    now_ms: cassette.clock_epoch_ms,
                };
                let signed_headers = apply(&auth, input, &env);
                let authorization = header(&signed_headers, "authorization")
                    .unwrap_or_else(|| panic!("{}: missing authorization", cassette.name));
                let expected = prepared.headers.get("authorization").expect("recorded authorization");
                assert_eq!(authorization, expected, "{}: {authorization}", cassette.name);
                assert_eq!(
                    header(&signed_headers, "x-amz-date"),
                    prepared.headers.get("x-amz-date").map(String::as_str),
                    "{}: x-amz-date",
                    cassette.name
                );
                signed += 1;
            }
        }
    }
    assert!(signed >= 5, "signed only {signed} exchanges");
}

#[test]
fn session_token_is_signed() {
    let auth = Auth::SigV4(SigV4Source {
        region: "us-east-1".to_string(),
        access_key_id: "fixture".to_string(),
        secret_access_key: "fixture".to_string(),
        session_token: Some("session".to_string()),
        service: "bedrock".to_string(),
    });
    let headers = apply(
        &auth,
        AuthInput {
            method: "POST".to_string(),
            url: "https://bedrock-runtime.us-east-1.amazonaws.com/model/m/converse".to_string(),
            body: "{}".to_string(),
            headers: Vec::new(),
            now_ms: 1_767_323_045_000,
        },
        &MapEnv::default(),
    );
    assert_eq!(header(&headers, "x-amz-security-token"), Some("session"));
    let authorization = header(&headers, "authorization").expect("authorization header");
    assert!(signed_headers(authorization).split(';').any(|name| name == "x-amz-security-token"), "{authorization}");
}

#[test]
fn repeated_slashes_collapse_in_canonical_path() {
    let auth = Auth::SigV4(SigV4Source {
        region: "us-east-1".to_string(),
        access_key_id: "fixture".to_string(),
        secret_access_key: "fixture".to_string(),
        session_token: None,
        service: "bedrock".to_string(),
    });
    let sign = |url: &str| {
        let headers = apply(
            &auth,
            AuthInput {
                method: "POST".to_string(),
                url: url.to_string(),
                body: "{}".to_string(),
                headers: Vec::new(),
                now_ms: 1_767_323_045_000,
            },
            &MapEnv::default(),
        );
        header(&headers, "authorization").expect("authorization header").to_string()
    };
    let single = sign("https://bedrock-runtime.us-east-1.amazonaws.com/model/us.amazon.nova-micro-v1%3A0/converse");
    let repeated = sign("https://bedrock-runtime.us-east-1.amazonaws.com//model/us.amazon.nova-micro-v1%3A0/converse");
    assert_eq!(single, repeated);
}

#[test]
fn bedrock_auth_precedence() {
    let credentials = Credentials {
        region: "us-west-2".to_string(),
        access_key_id: "AKIA".to_string(),
        secret_access_key: "secret".to_string(),
        session_token: None,
    };
    let env = MapEnv::new(vec![
        ("AWS_BEARER_TOKEN_BEDROCK".to_string(), "ambient".to_string()),
        ("AWS_REGION".to_string(), "eu-central-1".to_string()),
    ]);
    assert_eq!(resolve_region(None, None, &env), "eu-central-1");
    assert_eq!(resolve_region(None, Some(&credentials), &env), "us-west-2");
    assert_eq!(resolve_region(Some("ap-south-1"), Some(&credentials), &env), "ap-south-1");
    let default_env = MapEnv::new(vec![("AWS_DEFAULT_REGION".to_string(), "sa-east-1".to_string())]);
    assert_eq!(resolve_region(None, None, &default_env), "sa-east-1");
    assert_eq!(resolve_region(None, None, &MapEnv::default()), "us-east-1");

    let input = |api_key: Option<&str>, credentials: Option<Credentials>| ResolveAuthInput {
        api_key: api_key.map(str::to_string),
        credentials,
        profile: None,
    };
    let options = |mode: Option<AuthMode>| ResolveAuthOptions { service: None, mode };

    let auth = resolve_auth(input(Some("explicit"), Some(credentials.clone())), "us-east-1", options(None), &env)
        .expect("bearer auth");
    let headers = apply(&auth, auth_input(), &env);
    assert_eq!(header(&headers, "authorization"), Some("Bearer explicit"));

    let auth = resolve_auth(input(None, Some(credentials.clone())), "ap-south-1", options(Some(AuthMode::Sigv4)), &env)
        .expect("sigv4 auth");
    let headers = apply(&auth, auth_input(), &env);
    let authorization = header(&headers, "authorization").expect("authorization header");
    assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIA/"), "{authorization}");
    assert!(authorization.contains("/ap-south-1/bedrock/aws4_request"), "{authorization}");

    let auth = resolve_auth(input(None, Some(credentials)), "eu-west-1", options(None), &MapEnv::default())
        .expect("static credentials");
    let headers = apply(&auth, auth_input(), &MapEnv::default());
    let authorization = header(&headers, "authorization").expect("authorization header");
    assert!(authorization.contains("/eu-west-1/bedrock/aws4_request"), "{authorization}");

    let error = resolve_auth(input(None, None), "us-east-1", options(None), &MapEnv::default())
        .expect_err("missing credentials");
    assert_eq!(error.message(), "AWS default credential chain failed: the default credential chain is not available");
    match error.reason {
        AiErrorReason::Authentication { .. } => {}
        other => panic!("expected authentication error, got {other:?}"),
    }
}

fn auth_input() -> AuthInput {
    AuthInput {
        method: "POST".to_string(),
        url: "https://bedrock-runtime.us-east-1.amazonaws.com/model/m/converse".to_string(),
        body: "{}".to_string(),
        headers: Vec::new(),
        now_ms: 1_767_323_045_000,
    }
}
