use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opencode_http::ticket::{TICKET_TTL_SECONDS, TicketScope, TicketStore};

fn scope() -> TicketScope {
    TicketScope {
        target: "pty:pty_1".to_owned(),
        directory: "/project".to_owned(),
        workspace_id: Some("ws_1".to_owned()),
    }
}

fn clock(millis: Arc<AtomicU64>) -> opencode_http::Clock {
    Arc::new(move || millis.load(Ordering::SeqCst))
}

#[test]
fn issue_returns_a_hyphenated_uuid_v4_ticket() {
    let issued = TicketStore::new().issue(scope());
    assert_eq!(issued.ticket.len(), 36);
    assert!(issued.ticket.chars().all(|character| character.is_ascii_hexdigit() || character == '-'));
    let groups: Vec<&str> = issued.ticket.split('-').collect();
    assert_eq!(groups.iter().map(|group| group.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
    assert!(groups[2].starts_with('4'));
    assert!(matches!(groups[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
    assert_eq!(issued.expires_in, 60.0);
}

#[test]
fn consume_is_single_use() {
    let store = TicketStore::new();
    let ticket = store.issue(scope()).ticket;
    assert!(store.consume(&scope(), &ticket));
    assert!(!store.consume(&scope(), &ticket));
    assert!(!store.consume(&scope(), "unknown"));
}

#[test]
fn a_scope_mismatch_leaves_the_ticket_usable() {
    let store = TicketStore::new();
    let ticket = store.issue(scope()).ticket;
    let other = TicketScope { target: "mcp:client".to_owned(), ..scope() };
    assert!(!store.consume(&other, &ticket));
    assert!(store.consume(&scope(), &ticket));
}

#[test]
fn every_scope_field_must_match() {
    let store = TicketStore::new();
    let target = store.issue(scope()).ticket;
    let directory = store.issue(scope()).ticket;
    let workspace = store.issue(scope()).ticket;
    assert!(!store.consume(&TicketScope { target: "pty:pty_2".to_owned(), ..scope() }, &target));
    assert!(!store.consume(&TicketScope { directory: "/other".to_owned(), ..scope() }, &directory));
    assert!(!store.consume(&TicketScope { workspace_id: None, ..scope() }, &workspace));
    assert!(store.consume(&scope(), &target));
    assert!(store.consume(&scope(), &directory));
    assert!(store.consume(&scope(), &workspace));
}

#[test]
fn a_ticket_expires_after_sixty_seconds() {
    let now = Arc::new(AtomicU64::new(5_000));
    let store = TicketStore::with_clock(clock(now.clone()));
    let ticket = store.issue(scope()).ticket;
    now.store(5_000 + TICKET_TTL_SECONDS * 1_000 + 1, Ordering::SeqCst);
    assert!(!store.consume(&scope(), &ticket));
}
