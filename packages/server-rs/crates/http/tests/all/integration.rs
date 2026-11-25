use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_catalog::integration::{
    AuthorizationError, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationCommandMethod,
    IntegrationCommandMethodType, IntegrationExternalMethod, IntegrationExternalMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType,
};
use opencode_http::app::App;
use opencode_http::services::GlobalServices;
use opencode_location::location::LocationRef;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{authorized, basic, ready_app_with_services, services_at, temp_directory, text};

const DENY_ALL: &str = r#"{"policies":[{"action":"integration.connect","resource":"*","effect":"deny"}]}"#;

struct FakeOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    fail: bool,
}

impl OAuthImplementation for FakeOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        if self.fail {
            return Box::pin(async {
                Err(AuthorizationError::new(std::io::Error::other("provider rejected the login")))
            });
        }
        let method_id = self.method.id.clone();
        Box::pin(async move {
            Ok(OAuthAuthorization {
                url: "https://provider.test/authorize".to_owned(),
                instructions: "Paste the authorization code".to_owned(),
                expires_at: None,
                mode: OAuthMode::Code {
                    callback: Box::new(move |code| {
                        let method_id = method_id.clone();
                        Box::pin(async move {
                            Ok(CredentialOAuth {
                                r#type: CredentialOAuthType::Oauth,
                                method_id,
                                refresh: format!("refresh-{code}"),
                                access: format!("access-{code}"),
                                expires: 0.0,
                                metadata: None,
                            })
                        })
                    }),
                },
            })
        })
    }
}

fn json_request(method: &str, uri: &str, body: &str) -> Request<Bytes> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", basic("opencode", "pw"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_owned()))
        .expect("a request")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

fn segment(id: &str) -> String {
    opencode_api_types::js::encode_component(id)
}

async fn boot(name: &str) -> (Arc<GlobalServices>, Arc<App>, LocationRef) {
    let start = temp_directory(name);
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(start.to_string_lossy().into_owned(), None);
    (services, app, reference)
}

fn write_deny_all(services: &GlobalServices) {
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    std::fs::write(services.paths.config.join("opencode.json"), DENY_ALL).expect("write the global config");
}

fn register_integration(editor: &mut opencode_catalog::integration::Editor<'_>, id: &str, name: &str, mcp: bool) {
    let integration_id = IntegrationID(id.to_owned());
    editor.update(&integration_id, |info| {
        info.name = name.to_owned();
        if mcp {
            info.metadata = Some(JsonMap(vec![("source".to_owned(), serde_json::json!("mcp"))]));
        }
    });
}

#[tokio::test]
async fn list_and_get_answer_integrations() {
    let (services, app, reference) = boot("integration-list").await;
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            register_integration(editor, "alpha_test", "Alpha Test", false);
            register_integration(editor, "mcp_test", "Mcp Test", true);
            Ok(())
        })
        .await;

    let response = app.handle(authorized("GET", "/api/integration")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body["location"]["directory"].as_str().is_some_and(|directory| !directory.is_empty()));
    let data = body["data"].as_array().expect("an integration list");
    let ids = data.iter().filter_map(|entry| entry["id"].as_str()).collect::<Vec<_>>();
    assert!(ids.contains(&"alpha_test"));
    assert!(ids.contains(&"mcp_test"));

    let response = app.handle(authorized("GET", "/api/integration/alpha_test")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["id"], "alpha_test");
    assert_eq!(body["data"]["name"], "Alpha Test");

    let response = app.handle(authorized("GET", "/api/integration/missing")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationNotFoundError");
    assert_eq!(body["integrationID"], "missing");
    assert_eq!(body["message"], "Integration not found: missing");
}

#[tokio::test]
async fn policy_filters_the_list_and_denies_connecting() {
    let start = temp_directory("integration-policy");
    let services = services_at(&start);
    write_deny_all(&services);
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(start.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            register_integration(editor, "alpha_test", "Alpha Test", false);
            register_integration(editor, "mcp_test", "Mcp Test", true);
            Ok(())
        })
        .await;

    let response = app.handle(authorized("GET", "/api/integration")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let data = body["data"].as_array().expect("an integration list");
    let ids = data.iter().filter_map(|entry| entry["id"].as_str()).collect::<Vec<_>>();
    assert!(ids.contains(&"mcp_test"));
    assert!(!ids.contains(&"alpha_test"));

    let response =
        app.handle(json_request("POST", "/api/integration/alpha_test/connect/key", r#"{"key":"sk-denied"}"#)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "integration_policy");
    assert_eq!(body["message"], "Blocked by configuration policy: connecting alpha_test is not allowed");

    let response = app.handle(authorized("GET", "/api/integration/mcp_test")).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn connect_key_and_external_create_credentials() {
    let (services, app, reference) = boot("integration-connect").await;
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            let integration_id = IntegrationID("connect_test".to_owned());
            editor.update(&integration_id, |info| info.name = "Connect Test".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: integration_id.clone(),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: Some("API key".to_owned()),
                    form: None,
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id,
                method: IntegrationMethod::External(IntegrationExternalMethod {
                    id: IntegrationMethodID("external_one".to_owned()),
                    r#type: IntegrationExternalMethodType::External,
                    label: "External".to_owned(),
                    form: None,
                }),
            });
            Ok(())
        })
        .await;

    let response =
        app.handle(json_request("POST", "/api/integration/connect_test/connect/key", r#"{"key":"sk-live"}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let credentials =
        services.credentials.list(&IntegrationID("connect_test".to_owned())).expect("list the credentials");
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].label, "Connect Test");
    match &credentials[0].value {
        CredentialValue::Key(key) => assert_eq!(key.key, "sk-live"),
        other => panic!("expected a key credential, found {other:?}"),
    }

    let response = app
        .handle(json_request(
            "POST",
            "/api/integration/connect_test/connect/key",
            r#"{"key":"sk-answer","answer":{"region":"eu"}}"#,
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "integration_authorization");
    assert_eq!(body["message"], "Key method does not accept a form answer");

    let response = app
        .handle(json_request(
            "POST",
            "/api/integration/connect_test/connect/external",
            r#"{"methodID":"external_one"}"#,
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let credentials =
        services.credentials.list(&IntegrationID("connect_test".to_owned())).expect("list the credentials");
    assert_eq!(credentials.len(), 2);
    let external = credentials
        .iter()
        .find_map(|credential| match &credential.value {
            CredentialValue::External(external) => Some(external),
            _ => None,
        })
        .expect("an external credential");
    assert_eq!(external.method_id.0, "external_one");

    let response = app
        .handle(json_request("POST", "/api/integration/connect_test/connect/external", r#"{"methodID":"missing"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "integration_authorization");
    assert_eq!(body["message"], "External method not found: missing");

    let response = app.handle(json_request("POST", "/api/integration/missing/connect/key", r#"{"key":"sk"}"#)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationNotFoundError");
}

#[tokio::test]
async fn oauth_connect_status_complete_and_cancel() {
    let (services, app, reference) = boot("integration-oauth").await;
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            let method = IntegrationOAuthMethod {
                id: IntegrationMethodID("oauth_method".to_owned()),
                r#type: IntegrationOAuthMethodType::Oauth,
                label: "OAuth".to_owned(),
                form: None,
            };
            let probe = Arc::new(FakeOAuth {
                integration_id: IntegrationID("oauth_test".to_owned()),
                method: method.clone(),
                fail: false,
            });
            editor.method_update(Implementation::OAuth(probe));
            let failing = Arc::new(FakeOAuth {
                integration_id: IntegrationID("failing_oauth".to_owned()),
                method: method.clone(),
                fail: true,
            });
            editor.method_update(Implementation::OAuth(failing));
            Ok(())
        })
        .await;

    let response = app
        .handle(json_request("POST", "/api/integration/oauth_test/connect/oauth", r#"{"methodID":"oauth_method"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body["location"]["directory"].as_str().is_some_and(|directory| !directory.is_empty()));
    assert_eq!(body["data"]["mode"], "code");
    assert_eq!(body["data"]["url"], "https://provider.test/authorize");
    assert_eq!(body["data"]["instructions"], "Paste the authorization code");
    assert!(body["data"].get("paste").is_none());
    assert!(body["data"]["time"]["created"].as_i64().is_some_and(|created| created > 0));
    assert!(body["data"]["time"]["expires"].as_i64().is_some());
    let attempt = body["data"]["attemptID"].as_str().expect("an attempt id").to_owned();
    assert!(attempt.starts_with("con_"));

    let response = app.handle(authorized("GET", &format!("/api/integration/oauth_test/connect/oauth/{attempt}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["status"], "pending");

    let response = app
        .handle(json_request("POST", &format!("/api/integration/oauth_test/connect/oauth/{attempt}/complete"), "{}"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "integration_code_required");
    assert_eq!(body["message"], "Authorization code is required");

    let response = app
        .handle(json_request(
            "POST",
            &format!("/api/integration/oauth_test/connect/oauth/{attempt}/complete"),
            r#"{"code":"the-code"}"#,
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let response = app.handle(authorized("GET", &format!("/api/integration/oauth_test/connect/oauth/{attempt}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["status"], "complete");
    let credentials = services.credentials.list(&IntegrationID("oauth_test".to_owned())).expect("list the credentials");
    assert_eq!(credentials.len(), 1);
    match &credentials[0].value {
        CredentialValue::Oauth(oauth) => assert_eq!(oauth.access, "access-the-code"),
        other => panic!("expected an oauth credential, found {other:?}"),
    }

    let response =
        app.handle(authorized("DELETE", &format!("/api/integration/oauth_test/connect/oauth/{attempt}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", "/api/integration/oauth_test/connect/oauth/con_missing")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationAttemptNotFoundError");
    assert_eq!(body["attemptID"], "con_missing");
    assert_eq!(body["message"], "OAuth attempt not found: con_missing");

    let response = app
        .handle(json_request("POST", "/api/integration/failing_oauth/connect/oauth", r#"{"methodID":"oauth_method"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "integration_authorization");
    assert_eq!(body["message"], "provider rejected the login");

    let response =
        app.handle(json_request("POST", "/api/integration/oauth_test/connect/oauth", r#"{"methodID":"nope"}"#)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationMethodNotFoundError");
    assert_eq!(body["message"], "Integration method not found: nope");

    let response = app
        .handle(json_request("POST", "/api/integration/oauth_test/connect/oauth", r#"{"methodID":"oauth_method"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let cancelled = body["data"]["attemptID"].as_str().expect("an attempt id").to_owned();
    let response =
        app.handle(authorized("DELETE", &format!("/api/integration/oauth_test/connect/oauth/{cancelled}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response =
        app.handle(authorized("GET", &format!("/api/integration/oauth_test/connect/oauth/{cancelled}"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn serve_manifest(manifest: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the manifest listener");
    let address = listener.local_addr().expect("the listener address");
    let body = manifest.to_owned();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

async fn wait_for_complete(app: &App, uri: &str) -> Value {
    for _ in 0..100 {
        let response = app.handle(authorized("GET", uri)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body["data"]["status"] == "complete" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the attempt did not complete: {uri}");
}

#[tokio::test]
async fn command_connect_runs_a_wellknown_login() {
    let (services, app, reference) = boot("integration-command").await;
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            editor.method_update(Implementation::Method {
                integration_id: IntegrationID("slow_command".to_owned()),
                method: IntegrationMethod::Command(IntegrationCommandMethod {
                    id: IntegrationMethodID("login".to_owned()),
                    r#type: IntegrationCommandMethodType::Command,
                    label: "Log in".to_owned(),
                    command: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()],
                }),
            });
            Ok(())
        })
        .await;

    let origin =
        serve_manifest(r#"{"auth":{"command":["/bin/sh","-c","printf sk_wellknown"],"env":"TEST_TOKEN"}}"#).await;
    let response = app
        .handle(json_request("POST", "/api/experimental/integration/wellknown", &format!(r#"{{"url":"{origin}"}}"#)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let path = segment(&origin);
    let response = app.handle(authorized("GET", &format!("/api/integration/{path}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["id"], origin);
    assert_eq!(body["data"]["name"], "127.0.0.1");
    assert_eq!(body["data"]["methods"][0]["type"], "command");
    assert_eq!(body["data"]["methods"][0]["id"], "login");

    let response = app
        .handle(json_request("POST", &format!("/api/integration/{path}/connect/command"), r#"{"methodID":"login"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let attempt = body["data"]["attemptID"].as_str().expect("an attempt id").to_owned();
    assert!(attempt.starts_with("con_"));
    assert!(body["data"]["time"]["created"].as_i64().is_some_and(|created| created > 0));

    let completed = wait_for_complete(&app, &format!("/api/integration/{path}/connect/command/{attempt}")).await;
    assert!(completed["data"].get("message").is_none());
    let credentials = services.credentials.list(&IntegrationID(origin.clone())).expect("list the credentials");
    assert_eq!(credentials.len(), 1);
    match &credentials[0].value {
        CredentialValue::Key(key) => assert_eq!(key.key, "sk_wellknown"),
        other => panic!("expected a key credential, found {other:?}"),
    }

    let response = app
        .handle(json_request("POST", &format!("/api/integration/{path}/connect/command"), r#"{"methodID":"missing"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationMethodNotFoundError");
    assert_eq!(body["message"], "Integration method not found: missing");

    let response = app.handle(authorized("GET", &format!("/api/integration/{path}/connect/command/con_missing"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "IntegrationAttemptNotFoundError");
    assert_eq!(body["message"], "Command attempt not found: con_missing");

    let response = app
        .handle(json_request("POST", "/api/experimental/integration/wellknown", r#"{"url":"http://127.0.0.1:1"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "well_known_discovery");

    let response = app
        .handle(json_request("POST", "/api/integration/slow_command/connect/command", r#"{"methodID":"login"}"#))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let slow = body["data"]["attemptID"].as_str().expect("an attempt id").to_owned();
    let response =
        app.handle(authorized("GET", &format!("/api/integration/slow_command/connect/command/{slow}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["status"], "pending");
    assert!(body["data"].get("message").is_none());

    let response =
        app.handle(authorized("DELETE", &format!("/api/integration/slow_command/connect/command/{slow}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response =
        app.handle(authorized("GET", &format!("/api/integration/slow_command/connect/command/{slow}"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn production_boot_registers_the_native_provider_plugins() {
    let (services, app, reference) = boot("integration-native-plugins").await;
    let _lease = services.locations.get(&reference).await.expect("boot the location");

    let response = app.handle(authorized("GET", "/api/integration/github-copilot")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["id"], "github-copilot");
    let methods = body["data"]["methods"].as_array().expect("an integration method list");
    assert!(methods.iter().any(|method| method["type"] == "oauth" && method["id"] == "device"));
    assert!(methods.iter().any(|method| {
        method["type"] == "env"
            && method["names"].as_array().is_some_and(|names| names.iter().any(|name| name == "COPILOT_GITHUB_TOKEN"))
    }));
}

#[tokio::test]
async fn invalid_bodies_are_400() {
    let (_services, app, _reference) = boot("integration-invalid").await;
    for (uri, expected) in [
        ("/api/experimental/integration/wellknown", "Payload"),
        ("/api/integration/x/connect/key", "Payload"),
        ("/api/integration/x/connect/external", "Payload"),
        ("/api/integration/x/connect/oauth", "Payload"),
        ("/api/integration/x/connect/oauth/con_x/complete", "Payload"),
        ("/api/integration/x/connect/command", "Payload"),
    ] {
        let response = app.handle(json_request("POST", uri, "{")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert_eq!(body["_tag"], "InvalidRequestError", "{uri}");
        assert_eq!(body["kind"], expected, "{uri}");
    }
}
