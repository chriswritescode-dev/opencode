use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use opencode_location::state::{BoxFuture, Group, NotifyFn, Options, Registration, State, batch};

#[derive(Clone, Debug, PartialEq)]
struct Values {
    values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Counter {
    value: i64,
}

fn push_values_notify(observed: Arc<Mutex<Vec<String>>>, label: &'static str) -> NotifyFn<Values> {
    Box::new(move |_| {
        observed.lock().expect("the observed lock is not poisoned").push(label.to_string());
        Box::pin(async {})
    })
}

fn counting_notify(count: Arc<AtomicUsize>) -> NotifyFn<Counter> {
    Box::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    })
}

fn keep(registrations: &Mutex<Vec<Registration>>, registration: Registration) {
    registrations.lock().expect("the registrations lock is not poisoned").push(registration);
}

async fn values(state: &State<Values>) -> Vec<String> {
    state.get().await.expect("reads the state").values.clone()
}

#[tokio::test]
async fn detaches_every_registration_of_a_failed_group_and_refreshes_every_affected_domain() {
    let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let failures: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let refresh: Arc<Mutex<Option<BoxFuture<'static, ()>>>> = Arc::new(Mutex::new(None));
    let fail = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let group = Group::new({
        let failures = failures.clone();
        let refresh = refresh.clone();
        move |failure, changed| {
            failures
                .lock()
                .expect("the failures lock is not poisoned")
                .push((failure.state, failure.cause.to_string()));
            *refresh.lock().expect("the refresh lock is not poisoned") = Some(changed);
        }
    });
    let first = State::new(Options {
        name: Some("first".to_string()),
        notify: Some(push_values_notify(notices.clone(), "first")),
        ..Options::new(|| Values { values: Vec::new() })
    });
    let second = State::new(Options {
        name: Some("second".to_string()),
        notify: Some(push_values_notify(notices.clone(), "second")),
        ..Options::new(|| Values { values: Vec::new() })
    });

    let _healthy = first
        .transform(|data| {
            data.values.push("healthy".to_string());
            Ok(())
        })
        .await;
    let grouped = group
        .scope(first.transform(|data| {
            data.values.push("grouped".to_string());
            Ok(())
        }))
        .await;
    let _also_grouped = group
        .scope(first.transform(|data| {
            data.values.push("also grouped".to_string());
            Ok(())
        }))
        .await;
    let _partial = group
        .scope(second.transform({
            let fail = fail.clone();
            let calls = calls.clone();
            move |data| {
                calls.fetch_add(1, Ordering::Relaxed);
                data.values.push("partial".to_string());
                if fail.load(Ordering::Relaxed) {
                    return Err("broken".into());
                }
                Ok(())
            }
        }))
        .await;

    let before = first.get().await.expect("reads the first state");
    notices.lock().expect("the notices lock is not poisoned").clear();
    fail.store(true, Ordering::Relaxed);
    second.reload().await;

    assert_eq!(values(&first).await, vec!["healthy".to_string()]);
    assert_eq!(values(&second).await, Vec::<String>::new());
    assert_eq!(before.values, vec!["healthy".to_string(), "grouped".to_string(), "also grouped".to_string()]);
    {
        let failures = failures.lock().expect("the failures lock is not poisoned");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "second");
        assert_eq!(failures[0].1, "broken");
    }
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    notices.lock().expect("the notices lock is not poisoned").clear();
    let pending = refresh.lock().expect("the refresh lock is not poisoned").take().expect("the group stored a refresh");
    pending.await;
    let mut observed = notices.lock().expect("the notices lock is not poisoned").clone();
    observed.sort();
    assert_eq!(observed, vec!["first".to_string(), "second".to_string()]);
    grouped.dispose().await;
    assert_eq!(notices.lock().expect("the notices lock is not poisoned").len(), 2);
    let _resurrected = group
        .scope(first.transform(|data| {
            data.values.push("resurrected".to_string());
            Ok(())
        }))
        .await;
    assert_eq!(values(&first).await, vec!["healthy".to_string()]);
    assert_eq!(failures.lock().expect("the failures lock is not poisoned").len(), 1);
}

#[tokio::test]
async fn restarts_an_outer_candidate_when_a_nested_read_disables_one_of_its_contributors() {
    let fail = Arc::new(AtomicBool::new(false));
    let group = Group::new(|_, _| {});
    let inner = State::new(Options::new(|| Counter { value: 0 }));
    let outer = State::new(Options::new(|| Counter { value: 0 }));

    let _add_ten = group
        .scope(outer.transform(|data| {
            data.value += 10;
            Ok(())
        }))
        .await;
    let _broken = group
        .scope(inner.transform({
            let fail = fail.clone();
            move |data| {
                data.value = 5;
                if fail.load(Ordering::Relaxed) {
                    return Err("inner failed".into());
                }
                Ok(())
            }
        }))
        .await;

    let inner_state = inner.clone();
    let _reader = outer
        .transform_async(move |data| {
            let inner = inner_state.clone();
            Box::pin(async move {
                let value = inner.get().await.expect("reads the inner state").value;
                data.value += value + 1;
                Ok(())
            })
        })
        .await;

    assert_eq!(outer.get().await.expect("reads the outer state").value, 16);

    fail.store(true, Ordering::Relaxed);
    batch(async {
        inner.reload().await;
        outer.reload().await;
        assert_eq!(outer.get().await.expect("reads the outer state").value, 1);
        assert_eq!(inner.get().await.expect("reads the inner state").value, 0);
    })
    .await;
}

#[tokio::test]
async fn disables_multiple_failing_groups_once_each_before_publishing_a_complete_fold() {
    let reported: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let first = Group::new({
        let reported = reported.clone();
        move |_, _| reported.lock().expect("the reported lock is not poisoned").push("first".to_string())
    });
    let second = Group::new({
        let reported = reported.clone();
        move |_, _| reported.lock().expect("the reported lock is not poisoned").push("second".to_string())
    });
    let state = State::new(Options::new(|| Values { values: Vec::new() }));
    let registrations = Mutex::new(Vec::new());

    batch(async {
        keep(
            &registrations,
            first
                .scope(state.transform(|data| {
                    data.values.push("first".to_string());
                    Err("first failed".into())
                }))
                .await,
        );
        keep(
            &registrations,
            second
                .scope(state.transform(|data| {
                    data.values.push("second".to_string());
                    Err("second failed".into())
                }))
                .await,
        );
        keep(
            &registrations,
            state
                .transform(|data| {
                    data.values.push("healthy".to_string());
                    Ok(())
                })
                .await,
        );
    })
    .await;

    assert_eq!(values(&state).await, vec!["healthy".to_string()]);
    assert_eq!(
        *reported.lock().expect("the reported lock is not poisoned"),
        vec!["first".to_string(), "second".to_string()]
    );
    state.reload().await;
    assert_eq!(
        *reported.lock().expect("the reported lock is not poisoned"),
        vec!["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn does_not_disable_a_group_when_a_notification_runs_after_a_successful_replay() {
    let reported = Arc::new(AtomicUsize::new(0));
    let notifications = Arc::new(AtomicUsize::new(0));
    let group = Group::new({
        let reported = reported.clone();
        move |_, _| {
            reported.fetch_add(1, Ordering::Relaxed);
        }
    });
    let state = State::new(Options {
        notify: Some(counting_notify(notifications.clone())),
        ..Options::new(|| Counter { value: 0 })
    });

    let _increment = group
        .scope(state.transform(|data| {
            data.value += 1;
            Ok(())
        }))
        .await;

    assert_eq!(reported.load(Ordering::Relaxed), 0);
    assert_eq!(state.get().await.expect("reads the state").value, 1);
    state.reload().await;
    assert_eq!(state.get().await.expect("reads the state").value, 1);
    assert_eq!(reported.load(Ordering::Relaxed), 0);
}
