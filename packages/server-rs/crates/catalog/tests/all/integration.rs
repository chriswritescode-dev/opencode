use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Bus, BusOptions, Event, ListenerHandle};
use opencode_catalog::credential::{CreateInput, CredentialService};
use opencode_catalog::integration::{
    AuthorizationError, ConfigEnv, Implementation, IntegrationService, OAuthAuthorization, OAuthFuture,
    OAuthImplementation, RefreshCallback, RevokeCallback, StatusInput,
};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AbsolutePath, ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType,
    ConnectionEnvInfo, ConnectionEnvInfoType, ConnectionInfo, ConnectionStatus, ConnectionStatusStatus, CredentialID,
    CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer,
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationInfo, IntegrationMethod,
    IntegrationMethodID, IntegrationOAuthMethod, IntegrationOAuthMethodType, LocationRef,
};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::TempDir;

type RefreshFn = Arc<dyn Fn(CredentialOAuth) -> OAuthFuture<CredentialOAuth> + Send + Sync>;
type RevokeFn = Arc<dyn Fn(CredentialOAuth) -> OAuthFuture<()> + Send + Sync>;
type Harness = (TempDir, Arc<Database>, Arc<Bus>, Arc<CredentialService>, Arc<IntegrationService>);
type Locations = (Harness, Arc<IntegrationService>);

fn location(name: &str) -> LocationRef {
    LocationRef { directory: AbsolutePath(format!("/{name}")), workspace_id: None }
}

fn environment(values: HashMap<String, String>) -> ConfigEnv {
    Arc::new(move |name| values.get(name).cloned().map(OsString::from))
}

fn empty_environment() -> ConfigEnv {
    Arc::new(|_| None)
}

pub(crate) fn setup_with_env(values: HashMap<String, String>) -> Harness {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let credentials = CredentialService::new(database.clone(), bus.clone(), dir.path().join("locks"));
    let command_env =
        Arc::new(values.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))).collect());
    let service =
        IntegrationService::new(credentials.clone(), bus.clone(), location("main"), environment(values), command_env);
    (dir, database, bus, credentials, service)
}

pub(crate) fn setup() -> Harness {
    setup_with_env(HashMap::new())
}

fn setup_locations() -> Locations {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let credentials = CredentialService::new(database.clone(), bus.clone(), dir.path().join("locks"));
    let empty = Arc::new(Vec::new());
    let a =
        IntegrationService::new(credentials.clone(), bus.clone(), location("a"), empty_environment(), empty.clone());
    let b = IntegrationService::new(credentials.clone(), bus.clone(), location("b"), empty_environment(), empty);
    ((dir, database, bus, credentials, a), b)
}

fn capture(bus: &Bus) -> (Arc<Mutex<Vec<Event>>>, ListenerHandle) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let handle = bus.listen(Box::new(move |event: &Event| {
        sink.lock().expect("lock the events").push(event.clone());
        Ok(())
    }));
    (events, handle)
}

fn captured(events: &Arc<Mutex<Vec<Event>>>) -> Vec<(String, Value)> {
    events
        .lock()
        .expect("lock the events")
        .iter()
        .map(|event| (event.r#type().to_owned(), event.data().clone()))
        .collect()
}

pub(crate) fn integration_id(name: &str) -> IntegrationID {
    IntegrationID(name.to_owned())
}

fn oauth_method(id: &str, label: &str) -> IntegrationOAuthMethod {
    IntegrationOAuthMethod {
        id: IntegrationMethodID(id.to_owned()),
        r#type: IntegrationOAuthMethodType::Oauth,
        label: label.to_owned(),
        form: None,
    }
}

fn oauth_value(method: &str, access: &str, refresh: &str, expires: f64) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(method.to_owned()),
        refresh: refresh.to_owned(),
        access: access.to_owned(),
        expires,
        metadata: None,
    })
}

fn create_credential(
    credentials: &CredentialService,
    integration: &IntegrationID,
    value: CredentialValue,
    label: Option<&str>,
) -> opencode_catalog::credential::Info {
    credentials
        .create(CreateInput {
            id: None,
            integration_id: integration.clone(),
            value,
            label: label.map(str::to_owned),
            activate: None,
        })
        .expect("create the credential")
}

fn connection(info: &opencode_catalog::credential::Info) -> ConnectionInfo {
    ConnectionInfo::Credential(ConnectionCredentialInfo {
        r#type: ConnectionCredentialInfoType::Credential,
        id: info.id.clone(),
        label: info.label.clone(),
        method: match &info.value {
            CredentialValue::Oauth(_) => ConnectionCredentialInfoMethod::Oauth,
            CredentialValue::Key(_) => ConnectionCredentialInfoMethod::Key,
            CredentialValue::External(_) => ConnectionCredentialInfoMethod::External,
        },
        status: None,
    })
}

fn connection_status(connection: &ConnectionInfo) -> Option<ConnectionStatus> {
    match connection {
        ConnectionInfo::Credential(connection) => connection.status.clone(),
        ConnectionInfo::Env(connection) => connection.status.clone(),
    }
}

#[derive(Clone)]
struct FakeOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    refresh: Option<RefreshFn>,
    revoke: Option<RevokeFn>,
}

impl FakeOAuth {
    fn new(
        integration_id: IntegrationID,
        method: IntegrationOAuthMethod,
        refresh: Option<RefreshFn>,
        revoke: Option<RevokeFn>,
    ) -> Self {
        Self { integration_id, method, refresh, revoke }
    }
}

impl OAuthImplementation for FakeOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(std::future::pending())
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let refresh = self.refresh.clone()?;
        Some(Box::new(move |credential| refresh(credential)))
    }

    fn revoke(&self) -> Option<RevokeCallback> {
        let revoke = self.revoke.clone()?;
        Some(Box::new(move |credential| revoke(credential)))
    }
}

fn oauth_registration(fake: FakeOAuth) -> Implementation {
    Implementation::OAuth(Arc::new(fake))
}

fn env_registration(integration: &IntegrationID, names: &[&str]) -> Implementation {
    Implementation::Method {
        integration_id: integration.clone(),
        method: IntegrationMethod::Env(IntegrationEnvMethod {
            r#type: IntegrationEnvMethodType::Env,
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }),
    }
}

fn integration_updated_event() -> (String, Value) {
    ("integration.updated".to_owned(), json!({}))
}

fn switched_event(credential: Option<&CredentialID>, integration: &IntegrationID) -> (String, Value) {
    (
        "credential.switched".to_owned(),
        json!({ "credentialID": credential.map(|credential| credential.0.as_str()), "integrationID": integration.0 }),
    )
}

fn updated_credential_event(credential: &CredentialID, integration: &IntegrationID) -> (String, Value) {
    ("credential.updated".to_owned(), json!({ "integrationID": integration.0, "credentialID": credential.0 }))
}

#[tokio::test]
async fn registers_integrations_through_the_editor() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    let integration = integration_id("openai");
    let metadata = JsonMap(vec![("source".to_owned(), json!("plugin")), ("featured".to_owned(), json!(true))]);
    let registration = service
        .transform({
            let integration = integration.clone();
            let metadata = metadata.clone();
            move |editor| {
                editor.update(&integration, |info| {
                    info.name = "OpenAI".to_owned();
                    info.metadata = Some(metadata.clone());
                });
                Ok(())
            }
        })
        .await;

    let expected = IntegrationInfo {
        id: integration.clone(),
        name: "OpenAI".to_owned(),
        metadata: Some(metadata),
        methods: Vec::new(),
        connections: Vec::new(),
    };
    assert_eq!(service.get(&integration).await.expect("get the integration"), Some(expected));

    drop(registration);
    assert_eq!(service.get(&integration).await.expect("get the integration"), None);
}

#[tokio::test]
async fn reveals_the_previous_registration_when_an_override_closes() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    let integration = integration_id("openai");
    let first = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.update(&integration, |info| info.name = "OpenAI".to_owned());
                Ok(())
            }
        })
        .await;
    let second = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.update(&integration, |info| info.name = "OpenAI Override".to_owned());
                Ok(())
            }
        })
        .await;

    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.name, "OpenAI Override");

    drop(second);
    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.name, "OpenAI");
    let ids: Vec<IntegrationID> =
        service.list().await.expect("list the integrations").into_iter().map(|info| info.id).collect();
    assert_eq!(ids, vec![integration]);

    drop(first);
}

#[tokio::test]
async fn registers_and_overrides_methods_independently() {
    let (_dir, _database, _bus, _credentials, service) = setup();
    let integration = integration_id("openai");
    let first = service
        .transform({
            let integration = integration.clone();
            let fake = FakeOAuth::new(integration.clone(), oauth_method("chatgpt", "ChatGPT"), None, None);
            move |editor| {
                editor.method_update(oauth_registration(fake.clone()));
                Ok(())
            }
        })
        .await;
    let second = service
        .transform({
            let integration = integration.clone();
            let fake = FakeOAuth::new(integration.clone(), oauth_method("chatgpt", "ChatGPT Override"), None, None);
            move |editor| {
                let current = editor.get(&integration).expect("the integration is registered");
                assert_eq!(current.name, "openai");
                assert_eq!(editor.list().len(), 1);
                let methods = editor.method_list(&integration);
                assert_eq!(methods.len(), 1);
                match &methods[0] {
                    IntegrationMethod::Oauth(method) => assert_eq!(method.label, "ChatGPT"),
                    _ => panic!("expected an oauth method"),
                }
                editor.method_update(oauth_registration(fake.clone()));
                Ok(())
            }
        })
        .await;

    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.name, "openai");
    match &info.methods[0] {
        IntegrationMethod::Oauth(method) => assert_eq!(method.label, "ChatGPT Override"),
        _ => panic!("expected an oauth method"),
    }

    drop(second);
    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.methods.len(), 1);
    match &info.methods[0] {
        IntegrationMethod::Oauth(method) => {
            assert_eq!(method.label, "ChatGPT");
            assert_eq!(method.id, IntegrationMethodID("chatgpt".to_owned()));
        }
        _ => panic!("expected an oauth method"),
    }

    drop(first);
}

#[tokio::test]
async fn revokes_an_oauth_credential_before_removing_it_even_when_revocation_fails() {
    let (_dir, _database, _bus, credentials, service) = setup();
    let integration = integration_id("openai");
    let revoked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let revoke: RevokeFn = {
        let revoked = revoked.clone();
        Arc::new(move |credential| {
            revoked.lock().expect("lock the revoked").push(credential.refresh.clone());
            let broken = credential.refresh == "broken";
            Box::pin(async move {
                if broken {
                    return Err(AuthorizationError::new(std::io::Error::other("offline")));
                }
                Ok(())
            })
        })
    };
    let fake = FakeOAuth::new(integration.clone(), oauth_method("chatgpt", "ChatGPT"), None, Some(revoke));
    let _registration = service
        .transform(move |editor| {
            editor.method_update(oauth_registration(fake.clone()));
            Ok(())
        })
        .await;

    let working = create_credential(&credentials, &integration, oauth_value("chatgpt", "access", "working", 1.0), None);
    let broken = create_credential(&credentials, &integration, oauth_value("chatgpt", "access", "broken", 1.0), None);
    let key = create_credential(
        &credentials,
        &integration,
        CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: "sk-test".to_owned(),
            metadata: None,
        }),
        None,
    );

    service.connection().remove(&working.id).await.expect("remove the working credential");
    service.connection().remove(&broken.id).await.expect("remove the broken credential");
    service.connection().remove(&key.id).await.expect("remove the key credential");

    assert_eq!(*revoked.lock().expect("lock the revoked"), vec!["working".to_owned(), "broken".to_owned()]);
    assert!(credentials.list(&integration).expect("list the credentials").is_empty());
}

#[tokio::test]
async fn projects_credential_and_env_connections() {
    let values = HashMap::from([("INTEGRATION_TEST_ACME_KEY".to_owned(), "secret".to_owned())]);
    let (_dir, _database, bus, credentials, service) = setup_with_env(values);
    let integration = integration_id("acme");
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(env_registration(
                    &integration,
                    &["INTEGRATION_TEST_ACME_KEY", "INTEGRATION_TEST_ACME_MISSING"],
                ));
                Ok(())
            }
        })
        .await;

    let archived = create_credential(
        &credentials,
        &integration,
        CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: "c".to_owned(), metadata: None }),
        Some("Archived"),
    );
    let work = create_credential(
        &credentials,
        &integration,
        CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: "a".to_owned(), metadata: None }),
        Some("Work"),
    );
    let personal = create_credential(
        &credentials,
        &integration,
        CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: "b".to_owned(), metadata: None }),
        Some("Personal"),
    );
    let env = ConnectionInfo::Env(ConnectionEnvInfo {
        r#type: ConnectionEnvInfoType::Env,
        name: "INTEGRATION_TEST_ACME_KEY".to_owned(),
        status: None,
    });

    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.connections, vec![connection(&personal), connection(&work), connection(&archived), env.clone()]);
    assert_eq!(
        service.connection().active(&integration).await.expect("resolve the active connection"),
        Some(connection(&personal))
    );

    let (events, _handle) = capture(&bus);
    service.connection().activate(&work.id).await.expect("activate the work credential");

    assert_eq!(
        service.connection().active(&integration).await.expect("resolve the active connection"),
        Some(connection(&work))
    );
    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    let types: Vec<&str> = info
        .connections
        .iter()
        .map(|connection| match connection {
            ConnectionInfo::Credential(_) => "credential",
            ConnectionInfo::Env(_) => "env",
        })
        .collect();
    assert_eq!(types, vec!["credential", "credential", "credential", "env"]);
    assert_eq!(captured(&events), vec![switched_event(Some(&work.id), &integration)]);

    service.connection().remove(&archived.id).await.expect("remove the archived credential");
    assert_eq!(
        captured(&events),
        vec![switched_event(Some(&work.id), &integration), updated_credential_event(&archived.id, &integration),]
    );

    service.connection().remove(&work.id).await.expect("remove the work credential");
    assert_eq!(
        service.connection().active(&integration).await.expect("resolve the active connection"),
        Some(connection(&personal))
    );

    service.connection().remove(&personal.id).await.expect("remove the personal credential");
    assert_eq!(service.connection().active(&integration).await.expect("resolve the active connection"), Some(env));
    assert_eq!(
        captured(&events),
        vec![
            switched_event(Some(&work.id), &integration),
            updated_credential_event(&archived.id, &integration),
            updated_credential_event(&work.id, &integration),
            switched_event(Some(&personal.id), &integration),
            updated_credential_event(&personal.id, &integration),
            switched_event(None, &integration),
        ]
    );
}

#[tokio::test]
async fn projects_and_clears_connection_status() {
    let values = HashMap::from([("INTEGRATION_TEST_ACME_KEY".to_owned(), "secret".to_owned())]);
    let (_dir, _database, bus, _credentials, service) = setup_with_env(values);
    let integration = integration_id("acme");
    let _registration = service
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.method_update(env_registration(&integration, &["INTEGRATION_TEST_ACME_KEY"]));
                Ok(())
            }
        })
        .await;
    let env = ConnectionInfo::Env(ConnectionEnvInfo {
        r#type: ConnectionEnvInfoType::Env,
        name: "INTEGRATION_TEST_ACME_KEY".to_owned(),
        status: None,
    });
    assert_eq!(
        service.connection().active(&integration).await.expect("resolve the active connection"),
        Some(env.clone())
    );

    let (events, _handle) = capture(&bus);
    let status =
        ConnectionStatus { status: ConnectionStatusStatus::NeedsAuth, message: "Sign in".to_owned(), url: None };
    service
        .connection()
        .status(StatusInput {
            integration_id: integration.clone(),
            connection: env.clone(),
            status: Some(status.clone()),
        })
        .expect("set the connection status");
    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(info.connections.len(), 1);
    assert_eq!(connection_status(&info.connections[0]), Some(status.clone()));
    assert_eq!(captured(&events), vec![integration_updated_event()]);

    service
        .connection()
        .status(StatusInput {
            integration_id: integration.clone(),
            connection: env.clone(),
            status: Some(status.clone()),
        })
        .expect("set the connection status again");
    assert_eq!(captured(&events), vec![integration_updated_event()]);

    service
        .connection()
        .status(StatusInput { integration_id: integration.clone(), connection: env, status: None })
        .expect("clear the connection status");
    let info = service.get(&integration).await.expect("get the integration").expect("the integration is registered");
    assert_eq!(connection_status(&info.connections[0]), None);
    assert_eq!(captured(&events), vec![integration_updated_event(), integration_updated_event()]);
}

#[derive(Debug)]
struct EmptyError;

impl std::fmt::Display for EmptyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("")
    }
}

impl std::error::Error for EmptyError {}

#[test]
fn authorization_error_reports_the_underlying_cause_message() {
    let error = AuthorizationError::new(std::io::Error::other("Request failed: 401"));
    assert_eq!(error.message(), "Request failed: 401");
    assert_eq!(AuthorizationError::new(EmptyError).message(), "Authorization failed");
}

async fn shares_refresh(cancel: bool) {
    let ((_dir, _database, _bus, credentials, a), b) = setup_locations();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let refresh: RefreshFn = {
        let calls = calls.clone();
        let entered = entered.clone();
        let release = release.clone();
        Arc::new(move |value: CredentialOAuth| {
            let calls = calls.clone();
            let entered = entered.clone();
            let release = release.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(value.refresh, "old-refresh");
                entered.add_permits(1);
                release.acquire().await.expect("release the refresh").forget();
                Ok(CredentialOAuth {
                    access: "new-access".to_owned(),
                    refresh: "new-refresh".to_owned(),
                    expires: 9_007_199_254_740_991.0,
                    ..value
                })
            })
        })
    };
    let integration = integration_id("openai");
    let fake = FakeOAuth::new(integration.clone(), oauth_method("oauth", "OAuth"), Some(refresh), None);
    let _registration_a = a
        .transform({
            let fake = fake.clone();
            move |editor| {
                editor.method_update(oauth_registration(fake.clone()));
                Ok(())
            }
        })
        .await;
    let _registration_b = b
        .transform({
            let fake = fake.clone();
            move |editor| {
                editor.method_update(oauth_registration(fake.clone()));
                Ok(())
            }
        })
        .await;
    let stored =
        create_credential(&credentials, &integration, oauth_value("oauth", "old-access", "old-refresh", 1.0), None);
    let connection = connection(&stored);

    let first = tokio::spawn({
        let a = a.clone();
        let connection = connection.clone();
        async move { a.connection().resolve(&connection).await }
    });
    entered.acquire().await.expect("enter the refresh").forget();
    if cancel {
        first.abort();
    }
    let second = tokio::spawn({
        let b = b.clone();
        let connection = connection.clone();
        async move { b.connection().resolve(&connection).await }
    });
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    release.add_permits(1);
    let resolved = second.await.expect("join the second resolve").expect("resolve the credential");
    if !cancel {
        let first_resolved = first.await.expect("join the first resolve").expect("resolve the credential");
        assert_eq!(first_resolved, resolved);
    }
    let stored = credentials.get(&stored.id).expect("get the stored credential").expect("the credential exists");
    assert_eq!(Some(stored.value), resolved);

    let again = b.connection().resolve(&connection).await.expect("resolve the credential again");
    assert_eq!(again, resolved);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shares_refresh_between_locations() {
    shares_refresh(false).await;
}

#[tokio::test]
async fn shares_refresh_after_the_first_caller_is_interrupted() {
    shares_refresh(true).await;
}

#[tokio::test(start_paused = true)]
async fn bounds_a_stuck_refresh_and_allows_a_later_attempt() {
    let ((_dir, _database, _bus, credentials, a), _b) = setup_locations();
    let integration = integration_id("openai");
    let entered = Arc::new(Semaphore::new(0));
    let stuck: RefreshFn = {
        let entered = entered.clone();
        Arc::new(move |_: CredentialOAuth| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.add_permits(1);
                std::future::pending().await
            })
        })
    };
    let fake = FakeOAuth::new(integration.clone(), oauth_method("oauth", "OAuth"), Some(stuck), None);
    let _stuck = a
        .transform(move |editor| {
            editor.method_update(oauth_registration(fake.clone()));
            Ok(())
        })
        .await;
    let stored = create_credential(&credentials, &integration, oauth_value("oauth", "old", "refresh", 1.0), None);
    let connection = connection(&stored);

    let first = tokio::spawn({
        let a = a.clone();
        let connection = connection.clone();
        async move { a.connection().resolve(&connection).await }
    });
    entered.acquire().await.expect("enter the refresh").forget();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(first.await.expect("join the first resolve").is_err());

    let working: RefreshFn = Arc::new(|value: CredentialOAuth| {
        Box::pin(
            async move { Ok(CredentialOAuth { access: "new".to_owned(), expires: 9_007_199_254_740_991.0, ..value }) },
        )
    });
    let fake = FakeOAuth::new(integration.clone(), oauth_method("oauth", "OAuth"), Some(working), None);
    let _working = a
        .transform(move |editor| {
            editor.method_update(oauth_registration(fake.clone()));
            Ok(())
        })
        .await;

    let resolved =
        a.connection().resolve(&connection).await.expect("resolve the credential").expect("the credential exists");
    match resolved {
        CredentialValue::Oauth(value) => assert_eq!(value.access, "new"),
        _ => panic!("expected an oauth credential"),
    }
}
