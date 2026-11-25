use opencode_contract::generated::operations::{OPERATION_ROUTES, REQUEST_MANIFEST, REQUEST_SLOTS};
use opencode_contract::generated::{DURABLE_EVENTS, ERROR_STATUS, PUBLIC_EVENT_TYPES, ROUTES, corpus_roots};
use opencode_contract::{BodyKind, SuccessKind};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const REQUESTS_JSON: &str = include_str!("../corpus/requests.json");

fn fixture() -> Value {
    serde_json::from_str(REQUESTS_JSON).unwrap()
}

#[test]
fn the_request_manifest_covers_every_route() {
    assert_eq!(REQUEST_MANIFEST.len(), ROUTES.len(), "expected one manifest entry per route");
    for (route, (operation, slots)) in ROUTES.iter().zip(REQUEST_MANIFEST) {
        assert_eq!(route.operation_id, operation, "request manifest order differs from the routes");
        let body = slots.contains(&"body");
        assert_eq!(
            body,
            route.body == BodyKind::Json,
            "{} has body kind {:?} and slots {slots:?}",
            route.operation_id,
            route.body
        );
        for slot in slots {
            assert!(
                matches!(*slot, "params" | "query" | "headers" | "body"),
                "{} has unknown request slot {slot}",
                route.operation_id
            );
        }
        let unique: BTreeSet<&str> = slots.iter().copied().collect();
        assert_eq!(unique.len(), slots.len(), "{} has a duplicate request slot", route.operation_id);
    }
}

#[test]
fn every_request_slot_has_a_root_and_corpus_samples() {
    let slots: BTreeMap<(&str, &str), _> =
        REQUEST_SLOTS.iter().map(|slot| ((slot.operation_id, slot.slot), slot.decode)).collect();
    assert_eq!(slots.len(), REQUEST_SLOTS.len(), "duplicate request roots");

    let fixture = fixture();
    let mut expected = 0;
    for (operation, names) in REQUEST_MANIFEST {
        for name in names {
            expected += 1;
            assert!(slots.contains_key(&(operation, name)), "no request root for {operation} {name}");
            let entry = &fixture[operation][name];
            let valid = entry["valid"].as_array().map(Vec::len).unwrap_or(0);
            assert!(valid > 0, "{operation} {name} has no valid corpus samples");
        }
    }
    assert_eq!(slots.len(), expected, "the request roots do not match the manifest");

    println!("coverage: {} operations, {} request slots, {} request roots", ROUTES.len(), expected, slots.len());
}

#[test]
fn every_operation_has_a_module_and_a_success_root() {
    let roots: BTreeSet<&str> = corpus_roots().iter().map(|(root, _)| *root).collect();
    assert_eq!(OPERATION_ROUTES.len(), ROUTES.len(), "expected one operation module per route");
    let mut typed = 0;
    for (route, module) in ROUTES.iter().zip(OPERATION_ROUTES) {
        assert_eq!(route.operation_id, module.operation_id, "operation module order differs from the routes");
        if !matches!(route.success, SuccessKind::Json | SuccessKind::Data | SuccessKind::Sse) {
            continue;
        }
        typed += 1;
        let root = format!("operation:{}", route.operation_id);
        assert!(roots.contains(root.as_str()), "{} has no response corpus root {root}", route.operation_id);
    }
    assert_eq!(typed, 124, "expected 124 typed successes, found {typed}");
}

#[test]
fn every_event_and_error_has_a_corpus_root() {
    let roots: BTreeSet<&str> = corpus_roots().iter().map(|(root, _)| *root).collect();
    assert_eq!(PUBLIC_EVENT_TYPES.len(), 102, "expected 102 public event types");
    assert_eq!(DURABLE_EVENTS.len(), 49, "expected 49 durable events");
    assert_eq!(ERROR_STATUS.len(), 46, "expected 46 error tags");
    for tag in PUBLIC_EVENT_TYPES {
        assert!(roots.contains(tag), "public event {tag} has no corpus root");
    }
    for (tag, version, _) in DURABLE_EVENTS {
        let key = format!("{tag}.{version}");
        assert!(roots.contains(key.as_str()), "durable event {key} has no corpus root");
    }
    for (tag, status) in ERROR_STATUS {
        assert!(roots.contains(tag), "error {tag} has no corpus root");
        assert!(status >= 400, "error {tag} has status {status}");
    }

    println!(
        "coverage: {} public events, {} durable events, {} errors, {} corpus roots",
        PUBLIC_EVENT_TYPES.len(),
        DURABLE_EVENTS.len(),
        ERROR_STATUS.len(),
        roots.len()
    );
}
