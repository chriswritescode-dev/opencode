use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    FormAnswer, FormField, FormID, FormInfo, FormStringField, FormStringFieldType, FormValue,
};
use opencode_permission::form::{CreateInput, FormClock, FormError, FormEvents, FormService, FormState};
use tokio::sync::mpsc;

struct TestClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl TestClock {
    fn new() -> Self {
        Self { base: Instant::now(), offset: Mutex::new(Duration::ZERO) }
    }

    fn advance(&self, duration: Duration) {
        *self.offset.lock().unwrap() += duration;
    }
}

impl FormClock for TestClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap()
    }
}

struct Recorder {
    created: Mutex<Vec<FormInfo>>,
    replied: Mutex<Vec<(FormID, String, FormAnswer)>>,
    cancelled: Mutex<Vec<(FormID, String)>>,
    tx: mpsc::UnboundedSender<FormInfo>,
}

impl FormEvents for Recorder {
    fn created(&self, form: &FormInfo) {
        self.created.lock().unwrap().push(form.clone());
        let _ = self.tx.send(form.clone());
    }

    fn replied(&self, id: &FormID, session_id: &str, answer: &FormAnswer) {
        self.replied.lock().unwrap().push((id.clone(), session_id.to_string(), answer.clone()));
    }

    fn cancelled(&self, id: &FormID, session_id: &str) {
        self.cancelled.lock().unwrap().push((id.clone(), session_id.to_string()));
    }
}

struct Harness {
    service: Arc<FormService>,
    clock: Arc<TestClock>,
    recorder: Arc<Recorder>,
    rx: mpsc::UnboundedReceiver<FormInfo>,
}

fn setup() -> Harness {
    let clock = Arc::new(TestClock::new());
    let (tx, rx) = mpsc::unbounded_channel();
    let recorder = Arc::new(Recorder {
        created: Mutex::new(vec![]),
        replied: Mutex::new(vec![]),
        cancelled: Mutex::new(vec![]),
        tx,
    });
    let service = Arc::new(FormService::new(
        Arc::clone(&recorder) as Arc<dyn FormEvents>,
        Arc::clone(&clock) as Arc<dyn FormClock>,
    ));
    Harness { service, clock, recorder, rx }
}

impl Harness {
    async fn next_created(&mut self) -> FormInfo {
        self.rx.recv().await.expect("receives the created event")
    }

    fn created(&self) -> Vec<FormInfo> {
        self.recorder.created.lock().unwrap().clone()
    }
}

fn string_field(key: &str) -> FormStringField {
    FormStringField {
        key: key.to_string(),
        title: None,
        description: None,
        required: None,
        hidden: None,
        when: None,
        r#type: FormStringFieldType::String,
        format: None,
        min_length: None,
        max_length: None,
        pattern: None,
        placeholder: None,
        default: None,
        options: None,
        custom: None,
    }
}

fn text(key: &str) -> FormField {
    FormField::String(string_field(key))
}

fn required_text(key: &str) -> FormField {
    FormField::String(FormStringField { required: Some(true), ..string_field(key) })
}

fn input(id: &str, session_id: &str, fields: Vec<FormField>) -> CreateInput {
    CreateInput {
        id: Some(FormID(id.to_string())),
        session_id: session_id.to_string(),
        title: "Title".to_string(),
        metadata: None,
        fields,
    }
}

fn answer(pairs: Vec<(&str, FormValue)>) -> FormAnswer {
    JsonMap(pairs.into_iter().map(|(key, value)| (key.to_string(), value)).collect())
}

#[test]
fn create_stores_a_pending_form() {
    let harness = setup();
    let form = harness.service.create(input("frm_one", "ses_a", vec![text("name")])).expect("creates the form");

    assert_eq!(form.id, FormID("frm_one".to_string()));
    assert_eq!(form.session_id, "ses_a");
    assert_eq!(harness.service.state(&form.id), Ok(FormState::Pending));
    assert_eq!(harness.service.get(&form.id), Ok(form.clone()));
    assert_eq!(harness.service.list(None), vec![form.clone()]);
    assert_eq!(harness.created(), vec![form]);
}

#[test]
fn duplicate_id_is_already_exists() {
    let harness = setup();
    harness.service.create(input("frm_dup", "ses_a", vec![text("name")])).expect("creates the first form");

    let error = harness.service.create(input("frm_dup", "ses_a", vec![text("name")])).unwrap_err();
    assert_eq!(error, FormError::AlreadyExists("frm_dup".to_string()));
}

#[test]
fn invalid_fields_report_the_upstream_message() {
    let harness = setup();

    let empty = harness.service.create(input("frm_empty", "ses_a", vec![])).unwrap_err();
    assert_eq!(empty, FormError::InvalidForm { message: "Form must have at least one field".to_string() });

    let duplicate =
        harness.service.create(input("frm_duplicate", "ses_a", vec![text("name"), text("name")])).unwrap_err();
    assert_eq!(duplicate, FormError::InvalidForm { message: "Duplicate form field key: name".to_string() });

    assert!(harness.service.list(None).is_empty());
}

#[test]
fn reply_settles_the_form() {
    let harness = setup();
    let form = harness.service.create(input("frm_reply", "ses_a", vec![text("name")])).expect("creates");
    let reply = answer(vec![("name", FormValue::String("Ada".to_string()))]);

    harness.service.reply(&form.id, reply.clone()).expect("replies");

    assert_eq!(harness.service.state(&form.id), Ok(FormState::Answered(reply)));
    assert!(harness.service.list(None).is_empty());
    assert_eq!(
        harness.service.reply(&form.id, answer(vec![])),
        Err(FormError::AlreadySettled("frm_reply".to_string()))
    );
}

#[test]
fn invalid_answer_leaves_the_form_pending() {
    let harness = setup();
    let form = harness.service.create(input("frm_invalid", "ses_a", vec![required_text("name")])).expect("creates");

    let error = harness.service.reply(&form.id, answer(vec![])).unwrap_err();
    assert_eq!(
        error,
        FormError::InvalidAnswer {
            id: "frm_invalid".to_string(),
            message: "Missing required form field: name".to_string(),
        }
    );
    assert_eq!(harness.service.state(&form.id), Ok(FormState::Pending));
    assert_eq!(harness.service.list(None), vec![form]);
}

#[test]
fn cancel_settles_the_form() {
    let harness = setup();
    let first = harness.service.create(input("frm_cancel", "ses_a", vec![text("name")])).expect("creates");
    let second = harness.service.create(input("frm_cancel_empty", "ses_a", vec![text("name")])).expect("creates");

    harness.service.cancel(&first.id, Some("stop".to_string())).expect("cancels");
    harness.service.cancel(&second.id, None).expect("cancels");

    assert_eq!(harness.service.state(&first.id), Ok(FormState::Cancelled(Some("stop".to_string()))));
    assert_eq!(harness.service.state(&second.id), Ok(FormState::Cancelled(None)));
    assert_eq!(harness.service.cancel(&first.id, None), Err(FormError::AlreadySettled("frm_cancel".to_string())));
    assert_eq!(
        harness.service.cancel(&FormID("frm_missing".to_string()), None),
        Err(FormError::NotFound("frm_missing".to_string()))
    );
    assert_eq!(
        harness.service.get(&FormID("frm_missing".to_string())),
        Err(FormError::NotFound("frm_missing".to_string()))
    );
}

#[tokio::test]
async fn ask_resolves_from_reply() {
    let mut harness = setup();
    let service = Arc::clone(&harness.service);
    let handle = tokio::spawn(async move { service.ask(input("frm_ask_reply", "ses_a", vec![text("name")])).await });
    let form = harness.next_created().await;
    let reply = answer(vec![("name", FormValue::String("Ada".to_string()))]);

    harness.service.reply(&form.id, reply.clone()).expect("replies");

    assert_eq!(handle.await.expect("joins").expect("asks"), FormState::Answered(reply));
}

#[tokio::test]
async fn ask_resolves_from_cancel() {
    let mut harness = setup();
    let service = Arc::clone(&harness.service);
    let handle = tokio::spawn(async move { service.ask(input("frm_ask_cancel", "ses_a", vec![text("name")])).await });
    let form = harness.next_created().await;

    harness.service.cancel(&form.id, Some("stop".to_string())).expect("cancels");

    assert_eq!(handle.await.expect("joins").expect("asks"), FormState::Cancelled(Some("stop".to_string())));
}

#[tokio::test]
async fn aborting_ask_cancels_the_form() {
    let mut harness = setup();
    let service = Arc::clone(&harness.service);
    let handle = tokio::spawn(async move { service.ask(input("frm_abort", "ses_a", vec![text("name")])).await });
    let form = harness.next_created().await;

    handle.abort();
    assert!(handle.await.is_err());

    assert_eq!(harness.service.state(&form.id), Ok(FormState::Cancelled(None)));
    assert!(harness.service.list(None).is_empty());
}

#[test]
fn settled_forms_expire_after_ten_minutes() {
    let harness = setup();
    let form = harness.service.create(input("frm_retained", "ses_a", vec![text("name")])).expect("creates");
    harness.service.reply(&form.id, answer(vec![])).expect("replies");

    harness.clock.advance(Duration::from_secs(540));
    assert_eq!(harness.service.get(&form.id), Ok(form.clone()));

    harness.clock.advance(Duration::from_secs(120));
    assert_eq!(harness.service.get(&form.id), Err(FormError::NotFound("frm_retained".to_string())));
    assert_eq!(harness.service.state(&form.id), Err(FormError::NotFound("frm_retained".to_string())));

    let pending = harness.service.create(input("frm_pending_retained", "ses_a", vec![text("name")])).expect("creates");
    harness.clock.advance(Duration::from_secs(3600));
    assert_eq!(harness.service.get(&pending.id), Ok(pending));
}

#[test]
fn list_filters_by_session_and_keeps_insertion_order() {
    let harness = setup();
    let first = harness.service.create(input("frm_first", "ses_a", vec![text("name")])).expect("creates");
    let second = harness.service.create(input("frm_second", "global", vec![text("name")])).expect("creates");
    let third = harness.service.create(input("frm_third", "ses_b", vec![text("name")])).expect("creates");

    assert_eq!(harness.service.list(None), vec![first.clone(), second.clone(), third.clone()]);
    assert_eq!(harness.service.list(Some("ses_a")), vec![first.clone()]);
    assert_eq!(harness.service.list(Some("global")), vec![second.clone()]);
    assert!(harness.service.list(Some("ses_b")).contains(&third));
}

#[tokio::test]
async fn close_cancels_pending_forms_and_resolves_waiters() {
    let mut harness = setup();
    let pending = harness.service.create(input("frm_pending", "ses_a", vec![text("name")])).expect("creates");
    assert_eq!(harness.next_created().await.id, pending.id);

    let service = Arc::clone(&harness.service);
    let handle = tokio::spawn(async move { service.ask(input("frm_waiter", "ses_a", vec![text("name")])).await });
    harness.next_created().await;

    harness.service.close();
    harness.service.close();

    assert_eq!(harness.service.state(&pending.id), Ok(FormState::Cancelled(None)));
    assert_eq!(handle.await.expect("joins").expect("asks"), FormState::Cancelled(None));
    assert!(harness.service.list(None).is_empty());

    let after = harness.service.create(input("frm_after_close", "ses_a", vec![text("name")])).expect("creates");
    assert_eq!(harness.service.state(&after.id), Ok(FormState::Cancelled(None)));
    assert_eq!(harness.created().last().map(|form| form.id.clone()), Some(after.id));
}

#[test]
fn publishes_created_replied_and_cancelled_payloads() {
    let harness = setup();
    let form = harness.service.create(input("frm_events", "ses_a", vec![text("name")])).expect("creates");
    assert_eq!(harness.created()[0].id, form.id);

    let reply = answer(vec![("name", FormValue::String("Ada".to_string()))]);
    harness.service.reply(&form.id, reply.clone()).expect("replies");
    assert_eq!(harness.recorder.replied.lock().unwrap().clone(), vec![(form.id.clone(), "ses_a".to_string(), reply)]);

    let other = harness.service.create(input("frm_events_cancel", "ses_b", vec![text("name")])).expect("creates");
    harness.service.cancel(&other.id, Some("stop".to_string())).expect("cancels");
    assert_eq!(harness.recorder.cancelled.lock().unwrap().clone(), vec![(other.id.clone(), "ses_b".to_string())]);
}
