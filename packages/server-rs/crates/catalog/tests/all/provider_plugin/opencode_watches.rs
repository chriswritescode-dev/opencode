use std::sync::Arc;
use std::time::Duration;

use opencode_catalog::builtin::Builtin;
use opencode_catalog::provider_plugin::PluginContext;
use opencode_catalog::provider_plugin::opencode;
use opencode_catalog::provider_plugin::opencode::watches::{ACK_LIMIT, HEADER, LEASE_CLOSE_CODE, OVERSIZED_CLOSE_CODE};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{CredentialKey, CredentialKeyType, CredentialValue};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::console::{FakeConsole, wait_until};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with};

const PAGE: usize = 200;

fn config_url(origin: &str) -> String {
    format!("{origin}/api/v2/config")
}

fn console_key(origin: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: "secret".to_owned(),
        metadata: Some(JsonMap(vec![("server".to_owned(), json!(origin))])),
    })
}

fn watch_event(session_id: &str, id: &str) -> Value {
    json!({
        "id": id,
        "watchID": "ew_1",
        "sessionID": session_id,
        "source": "github",
        "kind": "review",
        "actor": { "login": "octocat", "bot": false },
        "title": "acme/app#1",
        "body": "Looks good, ship it",
        "url": "https://github.com/acme/app/pull/1",
        "status": "approved",
    })
}

fn events(session_id: &str, ids: impl IntoIterator<Item = String>) -> Vec<Value> {
    ids.into_iter().map(|id| watch_event(session_id, &id)).collect()
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) -> PluginContext {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    opencode::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
    context
}

async fn start_console(advertise: bool, upgrade: bool) -> (Harness, PluginContext, Arc<Routes>, FakeConsole) {
    let console = FakeConsole::start(upgrade).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), advertise);
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    (harness, context, routes, console)
}

fn set_config(routes: &Routes, origin: &str, advertise: bool) {
    routes.remove("GET", &config_url(origin));
    let headers = if advertise { vec![(HEADER.to_owned(), "1".to_owned())] } else { Vec::new() };
    routes.route_json_with_headers("GET", &config_url(origin), headers, json!({}));
}

fn poll_cursors(routes: &Routes) -> Vec<Option<String>> {
    routes
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET" && request.url.contains("/api/event-watches/events?"))
        .map(|request| {
            url::Url::parse(&request.url).ok().and_then(|url| {
                url.query_pairs().find(|(name, _)| name == "cursor").map(|(_, value)| value.into_owned())
            })
        })
        .collect()
}

fn acknowledged(routes: &Routes) -> Vec<usize> {
    routes
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.url.ends_with("/api/event-watches/ack"))
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body)
                .ok()
                .and_then(|body| body.get("ids").and_then(Value::as_array).map(Vec::len))
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stays_dormant_while_the_console_config_doesnt_advertise_event_watches() {
    let (_harness, _context, routes, console) = start_console(false, true).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(console.opens(), 0);
    assert!(routes.requests().iter().all(|request| !request.url.contains("/api/event-watches")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stays_dormant_without_the_console_watches_flag_even_when_the_console_advertises_event_watches() {
    let console = FakeConsole::start(true).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), true);
    let mut harness = start(Input::new(vec![json!({ "experimental": { "console_watches": false } })])).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let _context = add_plugin(&mut harness, routes.clone()).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(console.opens(), 0);
    assert!(routes.requests().iter().all(|request| !request.url.contains("/api/event-watches")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shares_one_socket_between_locations_of_the_same_workspace() {
    let console = FakeConsole::start(true).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), true);
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let first = add_plugin(&mut harness, routes.clone()).await;
    let second = add_plugin(&mut harness, routes).await;

    wait_until(|| console.opens() == 1).await;
    console.push(vec![watch_event("ses_watched", "evt_shared")]);
    wait_until(|| console.acks().len() == 1).await;

    assert_eq!(console.opens(), 1);
    assert_eq!(console.acks(), vec![vec!["evt_shared".to_owned()]]);
    assert_eq!(first.console.watched_events().len() + second.console.watched_events().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_at_once_when_console_ends_the_sockets_lease_and_replays_what_it_missed() {
    let (_harness, context, _routes, console) = start_console(true, true).await;
    wait_until(|| console.opens() == 1).await;

    console.set_inbox(events("ses_watched", (0..PAGE + 1).map(|index| format!("evt_missed_{index}"))));
    console.close_sockets(LEASE_CLOSE_CODE);
    wait_until(|| console.acks().iter().map(Vec::len).sum::<usize>() == PAGE + 1).await;

    assert_eq!(console.opens(), 2);
    assert_eq!(console.acks().iter().map(Vec::len).collect::<Vec<_>>(), vec![PAGE, 1]);
    assert_eq!(context.console.watched_events().len(), PAGE + 1);
}

async fn reconnects_after_a_backoff(code: u16) {
    let (_harness, _context, _routes, console) = start_console(true, true).await;
    wait_until(|| console.opens() == 1).await;

    console.close_sockets(code);
    wait_until(|| console.socket_count() == 0).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(console.opens(), 1);

    wait_until(|| console.opens() == 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_after_a_backoff_when_console_closes_the_socket_with_1011() {
    reconnects_after_a_backoff(1011).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_after_a_backoff_when_console_closes_the_socket_with_1009() {
    reconnects_after_a_backoff(OVERSIZED_CLOSE_CODE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_polls_with_consoles_cursor_when_the_websocket_upgrade_fails() {
    let console = FakeConsole::start(false).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), true);
    routes.route_json(
        "GET",
        &format!("{}/api/event-watches/events?wait=25", console.origin()),
        json!({
            "cursor": "1.2",
            "events": [watch_event("ses_watched", "evt_polled"), watch_event("ses_elsewhere", "evt_other")],
        }),
    );
    routes.route_with("POST", &format!("{}/api/event-watches/ack", console.origin()), |_| Reply::status(204));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    wait_until(|| poll_cursors(&routes).len() >= 2).await;

    assert_eq!(console.opens(), 0);
    assert_eq!(poll_cursors(&routes)[0], None);
    assert!(poll_cursors(&routes).contains(&Some("1.2".to_owned())));
    assert_eq!(console.acks(), Vec::<Vec<String>>::new());
    assert_eq!(
        acknowledged(&routes),
        vec![2],
        "the polled events are acknowledged over HTTP rather than the absent socket"
    );
    assert_eq!(context.console.watched_events().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn polls_again_with_each_answers_cursor_while_answers_come_back_full() {
    let console = FakeConsole::start(false).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), true);
    let first = events("ses_watched", (0..PAGE).map(|index| format!("evt_{index}")));
    let second = events("ses_watched", [format!("evt_{PAGE}")]);
    routes.route_json(
        "GET",
        &format!("{}/api/event-watches/events?wait=25", console.origin()),
        json!({ "cursor": format!("1.{PAGE}"), "events": first }),
    );
    routes.route_json(
        "GET",
        &format!("{}/api/event-watches/events?wait=25&cursor=1.{PAGE}", console.origin()),
        json!({ "cursor": format!("1.{}", PAGE + 1), "events": second }),
    );
    routes.route_with("POST", &format!("{}/api/event-watches/ack", console.origin()), |_| Reply::status(204));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let _context = add_plugin(&mut harness, routes.clone()).await;

    wait_until(|| acknowledged(&routes).iter().sum::<usize>() == PAGE + 1).await;

    assert_eq!(acknowledged(&routes), vec![PAGE, 1]);
    assert!(poll_cursors(&routes).contains(&Some(format!("1.{PAGE}"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledges_at_most_the_ack_limit_of_events_per_frame() {
    let (_harness, _context, _routes, console) = start_console(true, true).await;
    wait_until(|| console.opens() == 1).await;

    console.push(events("ses_watched", (0..ACK_LIMIT + 1).map(|index| format!("evt_{index}"))));
    wait_until(|| console.acks().iter().map(Vec::len).sum::<usize>() == ACK_LIMIT + 1).await;

    assert_eq!(console.acks().iter().map(Vec::len).collect::<Vec<_>>(), vec![ACK_LIMIT, 1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_and_stops_with_the_header_on_the_consoles_periodic_config_check() {
    let console = FakeConsole::start(true).await;
    let routes = Routes::new();
    set_config(&routes, console.origin(), false);
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(console.origin())).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    assert!(!context.console.allowed().watches);

    set_config(&routes, console.origin(), true);
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    wait_until(|| console.opens() == 1).await;
    assert!(context.console.allowed().watches);

    set_config(&routes, console.origin(), false);
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    wait_until(|| console.socket_count() == 0).await;
    assert!(!context.console.allowed().watches);
}
