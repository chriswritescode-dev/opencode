use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use opencode_location::state::{
    BoxFuture, ExternalBlock, NotifyFn, Options, Registration, State, TransformError, batch, changes, inherit, shutdown,
};

#[derive(Clone, Debug, PartialEq)]
struct Data {
    values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Counter {
    value: i64,
}

#[derive(Clone, Debug)]
struct Sequenced {
    values: Vec<i64>,
    sequence: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct Tagged {
    values: Vec<String>,
    tags: Vec<(String, i64)>,
}

fn counting<S: Send + Sync + 'static>(count: Arc<AtomicUsize>) -> NotifyFn<S> {
    Box::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    })
}

fn push_notify<S: Send + Sync + 'static>(observed: Arc<Mutex<Vec<String>>>, label: &'static str) -> NotifyFn<S> {
    Box::new(move |_| {
        observed.lock().expect("the observed lock is not poisoned").push(label.to_string());
        Box::pin(async {})
    })
}

fn keep(registrations: &Mutex<Vec<Registration>>, registration: Registration) {
    registrations.lock().expect("the registrations lock is not poisoned").push(registration);
}

async fn values(state: &State<Data>) -> Vec<String> {
    let data = state.get().await.expect("reads the state");
    data.values.clone()
}

struct RecordingBlock {
    seen: Arc<Mutex<Vec<i64>>>,
}

impl ExternalBlock<Counter> for RecordingBlock {
    fn apply<'a>(&'a self, state: Counter) -> BoxFuture<'a, Result<Counter, TransformError>> {
        let seen = self.seen.clone();
        Box::pin(async move {
            seen.lock().expect("the seen lock is not poisoned").push(state.value);
            Ok(Counter { value: state.value * 10 })
        })
    }
}

async fn cancels_remaining_observers(phase: &'static str) {
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(true));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
    let signal = sender.clone();

    let first = State::new(Options {
        notify: Some(Box::new({
            let observed = observed.clone();
            let block = block.clone();
            move |_| {
                let observed = observed.clone();
                let block = block.clone();
                let sender = sender.clone();
                Box::pin(async move {
                    observed.lock().expect("the observed lock is not poisoned").push("first".to_string());
                    if phase != "observer" || !block.load(Ordering::Relaxed) {
                        return;
                    }
                    let _ = sender.send(());
                    std::future::pending::<()>().await;
                })
            }
        })),
        ..Options::new(|| Counter { value: 0 })
    });
    let second = State::new(Options {
        notify: Some(push_notify(observed.clone(), "second")),
        ..Options::new(|| Counter { value: 0 })
    });

    let task = {
        let first = first.clone();
        let second = second.clone();
        let registrations = registrations.clone();
        tokio::spawn(async move {
            batch(async {
                keep(
                    &registrations,
                    first
                        .transform(|data| {
                            data.value += 1;
                            Ok(())
                        })
                        .await,
                );
                keep(
                    &registrations,
                    second
                        .transform(|data| {
                            data.value += 1;
                            Ok(())
                        })
                        .await,
                );
                if phase != "body" {
                    return;
                }
                let _ = signal.send(());
                std::future::pending::<()>().await;
            })
            .await
        })
    };

    receiver.recv().await.expect("the batch signals once it is interruptible");
    task.abort();
    let _ = task.await;
    block.store(false, Ordering::Relaxed);

    let first_value = first.get().await.expect("reads the first state").value;
    let second_value = second.get().await.expect("reads the second state").value;
    assert_eq!([first_value, second_value], [1, 1]);
    let observed = observed.lock().expect("the observed lock is not poisoned").clone();
    assert_eq!(observed, if phase == "body" { Vec::new() } else { vec!["first".to_string()] });
}

#[tokio::test]
async fn commits_a_transform_when_its_notification_is_dropped() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let notify: Option<NotifyFn<Data>> = Some(Box::new({
        let release = release.clone();
        move |_| {
            let release = release.clone();
            let sender = sender.clone();
            Box::pin(async move {
                let _ = sender.send(());
                let _permit = release.acquire().await;
            })
        }
    }));
    let state = State::new(Options { notify, ..Options::new(|| Data { values: Vec::new() }) });

    let task = {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .transform(|data| {
                    data.values.push("registered".to_string());
                    Ok(())
                })
                .await
        })
    };
    receiver.recv().await.expect("the notification signals");
    assert_eq!(values(&state).await, vec!["registered".to_string()]);
    task.abort();
    let _ = task.await;
    release.add_permits(10);

    assert_eq!(values(&state).await, Vec::<String>::new());
}

#[tokio::test]
async fn makes_current_state_visible_before_notifying() {
    let observed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let slot: Arc<Mutex<Option<State<Data>>>> = Arc::new(Mutex::new(None));
    let state = State::new(Options {
        notify: Some(Box::new({
            let observed = observed.clone();
            let slot = slot.clone();
            move |_| {
                let state =
                    slot.lock().expect("the state slot is not poisoned").clone().expect("the state is installed");
                let observed = observed.clone();
                Box::pin(async move {
                    let value = state.get().await.expect("reads its own state");
                    observed.lock().expect("the observed lock is not poisoned").push(value.values.clone());
                })
            }
        })),
        ..Options::new(|| Data { values: Vec::new() })
    });
    *slot.lock().expect("the state slot is not poisoned") = Some(state.clone());

    let _registration = state
        .transform(|data| {
            data.values.push("value".to_string());
            Ok(())
        })
        .await;

    assert_eq!(*observed.lock().expect("the observed lock is not poisoned"), vec![vec!["value".to_string()]]);
}

#[tokio::test]
async fn runs_transforms_during_every_reload() {
    let value = Arc::new(Mutex::new("first".to_string()));
    let state = State::new(Options::new(|| Data { values: Vec::new() }));
    let _registration = state
        .transform({
            let value = value.clone();
            move |data| {
                data.values.push(value.lock().expect("the value lock is not poisoned").clone());
                Ok(())
            }
        })
        .await;
    assert_eq!(values(&state).await, vec!["first".to_string()]);

    *value.lock().expect("the value lock is not poisoned") = "second".to_string();
    state.reload().await;
    assert_eq!(values(&state).await, vec!["second".to_string()]);
}

#[tokio::test]
async fn disposes_a_transform_once_and_rebuilds_remaining_state() {
    let state = State::new(Options::new(|| Data { values: Vec::new() }));
    let _first = state
        .transform(|data| {
            data.values.push("first".to_string());
            Ok(())
        })
        .await;
    let registration = state
        .transform(|data| {
            data.values.push("second".to_string());
            Ok(())
        })
        .await;
    assert_eq!(values(&state).await, vec!["first".to_string(), "second".to_string()]);

    registration.dispose().await;
    assert_eq!(values(&state).await, vec!["first".to_string()]);

    registration.dispose().await;
    assert_eq!(values(&state).await, vec!["first".to_string()]);
}

#[tokio::test]
async fn batches_notifications_across_domains() {
    let finalized = Arc::new(AtomicUsize::new(0));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let first = State::new(Options {
        notify: Some(counting(finalized.clone())),
        ..Options::new(|| Data { values: Vec::new() })
    });
    let second = State::new(Options {
        notify: Some(counting(finalized.clone())),
        ..Options::new(|| Data { values: Vec::new() })
    });

    batch(async {
        keep(
            &registrations,
            first
                .transform(|data| {
                    data.values.push("first".to_string());
                    Ok(())
                })
                .await,
        );
        keep(
            &registrations,
            first
                .transform(|data| {
                    data.values.push("second".to_string());
                    Ok(())
                })
                .await,
        );
        keep(
            &registrations,
            second
                .transform(|data| {
                    data.values.push("third".to_string());
                    Ok(())
                })
                .await,
        );
        assert_eq!(finalized.load(Ordering::Relaxed), 0);
    })
    .await;

    assert_eq!(values(&first).await, vec!["first".to_string(), "second".to_string()]);
    assert_eq!(values(&second).await, vec!["third".to_string()]);
    assert_eq!(finalized.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn discards_reloads_and_disposals_after_shutdown_while_still_running_cleanup() {
    let finalized = Arc::new(AtomicUsize::new(0));
    let disposed = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options {
        notify: Some(counting(finalized.clone())),
        ..Options::new(|| Data { values: Vec::new() })
    });
    let registration = state
        .transform(|data| {
            data.values.push("value".to_string());
            Ok(())
        })
        .await;
    assert_eq!(finalized.load(Ordering::Relaxed), 1);

    shutdown(async {
        disposed.fetch_add(1, Ordering::Relaxed);
        registration.dispose().await;
    })
    .await;

    assert_eq!(disposed.load(Ordering::Relaxed), 1);
    assert_eq!(finalized.load(Ordering::Relaxed), 1);

    registration.dispose().await;
    state.reload().await;
    assert_eq!(finalized.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn keeps_teardown_suppression_separate_from_an_enclosing_live_batch() {
    let finalized: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let closing = State::new(Options {
        notify: Some(push_notify(finalized.clone(), "closing")),
        ..Options::new(|| Counter { value: 0 })
    });
    let live = State::new(Options {
        notify: Some(push_notify(finalized.clone(), "live")),
        ..Options::new(|| Counter { value: 0 })
    });
    let registration = closing.transform(|_| Ok(())).await;
    finalized.lock().expect("the finalized lock is not poisoned").clear();

    batch(async {
        keep(&registrations, live.transform(|_| Ok(())).await);
        shutdown(async {
            registration.dispose().await;
        })
        .await;
    })
    .await;

    assert_eq!(*finalized.lock().expect("the finalized lock is not poisoned"), vec!["live".to_string()]);
}

#[tokio::test]
async fn notifies_once_per_reload_without_waiting() {
    let finalized = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options {
        notify: Some(counting(finalized.clone())),
        ..Options::new(|| Data { values: Vec::new() })
    });
    let _registration = state
        .transform(|data| {
            data.values.push("value".to_string());
            Ok(())
        })
        .await;
    finalized.store(0, Ordering::Relaxed);

    state.reload().await;
    assert_eq!(finalized.load(Ordering::Relaxed), 1);
    state.reload().await;
    assert_eq!(finalized.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn publishes_the_initial_projection_and_skips_unchanged_reloads() {
    let notifications = Arc::new(AtomicUsize::new(0));
    let label = Arc::new(Mutex::new("first".to_string()));
    let state = State::new(Options {
        notify: Some(changes(|data: &Data| data.values.join(","), {
            let notifications = notifications.clone();
            move |_: Arc<Data>| {
                notifications.fetch_add(1, Ordering::Relaxed);
                Box::pin(async {})
            }
        })),
        ..Options::new(|| Data { values: Vec::new() })
    });

    let _empty = state.transform(|_| Ok(())).await;
    assert_eq!(notifications.load(Ordering::Relaxed), 1);
    state.reload().await;
    assert_eq!(notifications.load(Ordering::Relaxed), 1);

    let _label_transform = state
        .transform({
            let label = label.clone();
            move |data| {
                data.values.push(label.lock().expect("the label lock is not poisoned").clone());
                Ok(())
            }
        })
        .await;
    assert_eq!(notifications.load(Ordering::Relaxed), 2);
    state.reload().await;
    assert_eq!(notifications.load(Ordering::Relaxed), 2);

    *label.lock().expect("the label lock is not poisoned") = "second".to_string();
    state.reload().await;
    assert_eq!(notifications.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn leaves_a_retained_value_untouched_when_later_registrations_rebuild() {
    let state = State::new(Options::new(|| Tagged { values: Vec::new(), tags: Vec::new() }));

    batch(async {
        let _first = state
            .transform(|data| {
                data.values.push("first".to_string());
                data.tags.push(("first".to_string(), 1));
                Ok(())
            })
            .await;
        let retained = state.get().await.expect("reads the state");
        let same = state.get().await.expect("reads the state");
        assert!(Arc::ptr_eq(&retained, &same));

        let _second = state
            .transform(|data| {
                data.values.push("second".to_string());
                data.tags.push(("second".to_string(), 2));
                Ok(())
            })
            .await;
        let current = state.get().await.expect("reads the state");
        assert!(!Arc::ptr_eq(&current, &retained));
        assert_eq!(current.values, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(current.tags, vec![("first".to_string(), 1), ("second".to_string(), 2)]);
        assert_eq!(retained.values, vec!["first".to_string()]);
        assert_eq!(retained.tags, vec![("first".to_string(), 1)]);
    })
    .await;
}

#[tokio::test]
async fn rebuilds_with_a_fresh_initial_value_every_time() {
    let inits = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options::new({
        let inits = inits.clone();
        move || {
            inits.fetch_add(1, Ordering::Relaxed);
            Sequenced { values: Vec::new(), sequence: 0 }
        }
    }));

    batch(async {
        let _first = state
            .transform(|data| {
                data.sequence += 1;
                data.values.push(data.sequence);
                Ok(())
            })
            .await;
        assert_eq!(state.get().await.expect("reads the state").values, vec![1]);
        assert_eq!(inits.load(Ordering::Relaxed), 2);

        let _second = state
            .transform(|data| {
                data.sequence += 1;
                data.values.push(data.sequence);
                Ok(())
            })
            .await;
        assert_eq!(state.get().await.expect("reads the state").values, vec![1, 2]);
        assert_eq!(inits.load(Ordering::Relaxed), 3);

        state.reload().await;
        assert_eq!(state.get().await.expect("reads the state").values, vec![1, 2]);
        assert_eq!(inits.load(Ordering::Relaxed), 4);
    })
    .await;
}

#[tokio::test]
async fn rebuilds_lazily_on_read_and_notifies_even_without_a_final_read() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let notifications: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let state = State::new(Options {
        notify: Some(Box::new({
            let notifications = notifications.clone();
            move |value: Arc<Data>| {
                notifications.lock().expect("the notifications lock is not poisoned").push(value.values.clone());
                Box::pin(async {})
            }
        })),
        ..Options::new(|| Data { values: Vec::new() })
    });

    assert_eq!(values(&state).await, Vec::<String>::new());
    batch(std::future::ready(())).await;
    assert!(notifications.lock().expect("the notifications lock is not poisoned").is_empty());

    batch(async {
        keep(
            &registrations,
            state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push("first".to_string());
                        data.values.push("first".to_string());
                        Ok(())
                    }
                })
                .await,
        );
        keep(
            &registrations,
            state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push("second".to_string());
                        data.values.push("second".to_string());
                        Ok(())
                    }
                })
                .await,
        );
        assert!(calls.lock().expect("the calls lock is not poisoned").is_empty());

        let view = state.get().await.expect("reads the state");
        assert_eq!(view.values, vec!["first".to_string(), "second".to_string()]);
        let same = state.get().await.expect("reads the state");
        assert!(Arc::ptr_eq(&view, &same));
        assert_eq!(
            *calls.lock().expect("the calls lock is not poisoned"),
            vec!["first".to_string(), "second".to_string()]
        );

        keep(
            &registrations,
            state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push("third".to_string());
                        data.values.push("third".to_string());
                        Ok(())
                    }
                })
                .await,
        );
        assert_eq!(
            *calls.lock().expect("the calls lock is not poisoned"),
            vec!["first".to_string(), "second".to_string()]
        );

        let next = state.get().await.expect("reads the state");
        assert!(!Arc::ptr_eq(&next, &view));
        assert_eq!(next.values, vec!["first".to_string(), "second".to_string(), "third".to_string()]);
        assert_eq!(view.values, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(
            *calls.lock().expect("the calls lock is not poisoned"),
            vec![
                "first".to_string(),
                "second".to_string(),
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );

        keep(
            &registrations,
            state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push("fourth".to_string());
                        data.values.push("fourth".to_string());
                        Ok(())
                    }
                })
                .await,
        );
        assert!(notifications.lock().expect("the notifications lock is not poisoned").is_empty());
    })
    .await;

    {
        let calls = calls.lock().expect("the calls lock is not poisoned");
        assert_eq!(calls[5..], ["first".to_string(), "second".to_string(), "third".to_string(), "fourth".to_string()]);
    }
    assert_eq!(
        *notifications.lock().expect("the notifications lock is not poisoned"),
        vec![vec!["first".to_string(), "second".to_string(), "third".to_string(), "fourth".to_string()]]
    );
}

#[tokio::test]
async fn replays_every_callback_after_each_change_outside_a_batch() {
    let calls: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let state = State::new(Options::new(|| Counter { value: 2 }));
    let _first = state
        .transform({
            let calls = calls.clone();
            move |data| {
                calls.lock().expect("the calls lock is not poisoned").push(1);
                data.value += 3;
                Ok(())
            }
        })
        .await;
    let _second = state
        .transform({
            let calls = calls.clone();
            move |data| {
                calls.lock().expect("the calls lock is not poisoned").push(2);
                data.value *= 4;
                Ok(())
            }
        })
        .await;
    assert_eq!(*calls.lock().expect("the calls lock is not poisoned"), vec![1, 1, 2]);
    assert_eq!(state.get().await.expect("reads the state").value, 20);
    assert_eq!(*calls.lock().expect("the calls lock is not poisoned"), vec![1, 1, 2]);

    let _third = state
        .transform({
            let calls = calls.clone();
            move |data| {
                calls.lock().expect("the calls lock is not poisoned").push(3);
                data.value -= 1;
                Ok(())
            }
        })
        .await;
    assert_eq!(state.get().await.expect("reads the state").value, 19);
    assert_eq!(*calls.lock().expect("the calls lock is not poisoned"), vec![1, 1, 2, 1, 2, 3]);
}

#[tokio::test]
async fn rebuilds_noncommutative_edits_after_removing_each_position() {
    let removed_values = [11, 2, 18];
    for removed in 0..3 {
        let calls: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let state = State::new(Options::new(|| Counter { value: 5 }));
        let registrations: Vec<Registration> = batch(async {
            let first = state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push(0);
                        data.value += 1;
                        Ok(())
                    }
                })
                .await;
            let second = state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push(1);
                        data.value *= 3;
                        Ok(())
                    }
                })
                .await;
            let third = state
                .transform({
                    let calls = calls.clone();
                    move |data| {
                        calls.lock().expect("the calls lock is not poisoned").push(2);
                        data.value -= 4;
                        Ok(())
                    }
                })
                .await;
            vec![first, second, third]
        })
        .await;
        assert_eq!(state.get().await.expect("reads the state").value, 14);

        calls.lock().expect("the calls lock is not poisoned").clear();
        registrations[removed].dispose().await;
        assert_eq!(state.get().await.expect("reads the state").value, removed_values[removed]);
        let expected: Vec<usize> = [0, 1, 2].into_iter().filter(|index| *index != removed).collect();
        assert_eq!(*calls.lock().expect("the calls lock is not poisoned"), expected);

        calls.lock().expect("the calls lock is not poisoned").clear();
        registrations[removed].dispose().await;
        assert!(calls.lock().expect("the calls lock is not poisoned").is_empty());
    }
}

#[tokio::test]
async fn keeps_equal_callback_registrations_independently_disposable() {
    let increments = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options::new(|| Counter { value: 0 }));
    let first = state
        .transform({
            let increments = increments.clone();
            move |data| {
                increments.fetch_add(1, Ordering::Relaxed);
                data.value += 1;
                Ok(())
            }
        })
        .await;
    let second = state
        .transform({
            let increments = increments.clone();
            move |data| {
                increments.fetch_add(1, Ordering::Relaxed);
                data.value += 1;
                Ok(())
            }
        })
        .await;
    assert_eq!(state.get().await.expect("reads the state").value, 2);

    first.dispose().await;
    assert_eq!(state.get().await.expect("reads the state").value, 1);
    first.dispose().await;
    assert_eq!(state.get().await.expect("reads the state").value, 1);
    second.dispose().await;
    assert_eq!(state.get().await.expect("reads the state").value, 0);
}

#[tokio::test]
async fn does_not_evaluate_a_pending_callback_removed_before_the_first_read() {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options::new(|| Counter { value: 0 }));

    batch(async {
        let registration = state
            .transform({
                let calls = calls.clone();
                move |_| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
            })
            .await;
        registration.dispose().await;
        assert_eq!(state.get().await.expect("reads the state").value, 0);
    })
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn invalidates_on_reload_and_reads_new_inputs() {
    let source = Arc::new(AtomicI64::new(1));
    let calls = Arc::new(AtomicUsize::new(0));
    let notifications = Arc::new(AtomicUsize::new(0));
    let state =
        State::new(Options { notify: Some(counting(notifications.clone())), ..Options::new(|| Counter { value: 0 }) });
    let _source_edit = state
        .transform({
            let source = source.clone();
            let calls = calls.clone();
            move |data| {
                calls.fetch_add(1, Ordering::Relaxed);
                data.value += source.load(Ordering::Relaxed);
                Ok(())
            }
        })
        .await;
    notifications.store(0, Ordering::Relaxed);

    source.store(2, Ordering::Relaxed);
    state.reload().await;
    assert_eq!(state.get().await.expect("reads the state").value, 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(notifications.load(Ordering::Relaxed), 1);

    batch(async {
        source.store(3, Ordering::Relaxed);
        state.reload().await;
        assert_eq!(state.get().await.expect("reads the state").value, 3);
        let _multiply = state
            .transform(|data| {
                data.value *= 10;
                Ok(())
            })
            .await;
        assert_eq!(state.get().await.expect("reads the state").value, 30);
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert_eq!(notifications.load(Ordering::Relaxed), 1);
    })
    .await;

    assert_eq!(notifications.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn resamples_a_changing_initial_value_even_when_there_are_no_transforms() {
    let source = Arc::new(AtomicI64::new(1));
    let state = State::new(Options::new({
        let source = source.clone();
        move || Counter { value: source.load(Ordering::Relaxed) }
    }));
    assert_eq!(state.get().await.expect("reads the state").value, 1);

    source.store(2, Ordering::Relaxed);
    assert_eq!(state.get().await.expect("reads the state").value, 1);

    batch(async {
        state.reload().await;
        assert_eq!(state.get().await.expect("reads the state").value, 2);
    })
    .await;
}

#[tokio::test]
async fn keeps_the_previous_value_when_a_rebuild_throws_and_retries_on_the_next_read() {
    let fail = Arc::new(AtomicBool::new(true));
    let inits = Arc::new(AtomicUsize::new(0));
    let state = State::new(Options::new({
        let inits = inits.clone();
        move || {
            inits.fetch_add(1, Ordering::Relaxed);
            Data { values: Vec::new() }
        }
    }));
    let _first = state
        .transform(|data| {
            data.values.push("first".to_string());
            Ok(())
        })
        .await;
    let before = state.get().await.expect("reads the state");
    assert_eq!(inits.load(Ordering::Relaxed), 2);

    batch(async {
        let _failing = state
            .transform({
                let fail = fail.clone();
                move |data| {
                    data.values.push("second".to_string());
                    if fail.load(Ordering::Relaxed) {
                        return Err("failed edit".into());
                    }
                    Ok(())
                }
            })
            .await;

        let error = state.get().await.expect_err("the failing rebuild reports an error");
        assert!(error.to_string().contains("failed edit"));
        assert_eq!(inits.load(Ordering::Relaxed), 3);

        let error = state.get().await.expect_err("the failing rebuild reports an error");
        assert!(error.to_string().contains("failed edit"));
        assert_eq!(inits.load(Ordering::Relaxed), 4);
        assert_eq!(before.values, vec!["first".to_string()]);

        fail.store(false, Ordering::Relaxed);
        assert_eq!(values(&state).await, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(inits.load(Ordering::Relaxed), 5);

        let _third = state
            .transform(|data| {
                data.values.push("third".to_string());
                Ok(())
            })
            .await;
        assert_eq!(values(&state).await, vec!["first".to_string(), "second".to_string(), "third".to_string()]);
        assert_eq!(inits.load(Ordering::Relaxed), 6);
    })
    .await;
}

#[tokio::test]
async fn recovers_by_disposing_a_failing_callback_without_keeping_its_partial_edits() {
    let state = State::new(Options::new(|| Counter { value: 2 }));
    let _multiply = state
        .transform(|data| {
            data.value *= 3;
            Ok(())
        })
        .await;

    batch(async {
        let failing = state
            .transform(|data| {
                data.value += 100;
                Err("bad callback".into())
            })
            .await;
        let error = state.get().await.expect_err("the failing rebuild reports an error");
        assert!(error.to_string().contains("bad callback"));

        failing.dispose().await;
        assert_eq!(state.get().await.expect("reads the state").value, 6);
        let _add = state
            .transform(|data| {
                data.value += 1;
                Ok(())
            })
            .await;
        assert_eq!(state.get().await.expect("reads the state").value, 7);
    })
    .await;
}

#[tokio::test]
async fn cancels_remaining_observers_when_dropped_during_the_body() {
    cancels_remaining_observers("body").await;
}

#[tokio::test]
async fn cancels_remaining_observers_when_dropped_during_the_first_observer() {
    cancels_remaining_observers("observer").await;
}

#[tokio::test]
async fn shares_nested_live_batches_and_does_not_retain_an_escaped_batch() {
    let notifications = Arc::new(AtomicUsize::new(0));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let state =
        State::new(Options { notify: Some(counting(notifications.clone())), ..Options::new(|| Counter { value: 0 }) });

    let inherit = batch(async {
        keep(
            &registrations,
            state
                .transform(|data| {
                    data.value += 1;
                    Ok(())
                })
                .await,
        );
        keep(
            &registrations,
            batch(state.transform(|data| {
                data.value += 1;
                Ok(())
            }))
            .await,
        );
        assert_eq!(state.get().await.expect("reads the state").value, 2);
        assert_eq!(notifications.load(Ordering::Relaxed), 0);
        inherit().await
    })
    .await;
    assert_eq!(notifications.load(Ordering::Relaxed), 1);

    keep(
        &registrations,
        inherit
            .scope(state.transform(|data| {
                data.value += 1;
                Ok(())
            }))
            .await,
    );
    assert_eq!(state.get().await.expect("reads the state").value, 3);
    assert_eq!(notifications.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn lets_observers_read_other_pending_domains_and_register_more_edits() {
    let observed: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let added = Arc::new(AtomicBool::new(false));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let other = State::new(Options::new(|| Counter { value: 0 }));
    let slot: Arc<Mutex<Option<State<Counter>>>> = Arc::new(Mutex::new(None));
    let state = State::new(Options {
        notify: Some(Box::new({
            let observed = observed.clone();
            let added = added.clone();
            let registrations = registrations.clone();
            let other = other.clone();
            let slot = slot.clone();
            move |_| {
                let state =
                    slot.lock().expect("the state slot is not poisoned").clone().expect("the state is installed");
                let observed = observed.clone();
                let added = added.clone();
                let registrations = registrations.clone();
                let other = other.clone();
                Box::pin(async move {
                    let value = other.get().await.expect("reads the other state").value;
                    observed.lock().expect("the observed lock is not poisoned").push(value);
                    if added.swap(true, Ordering::Relaxed) {
                        return;
                    }
                    keep(&registrations, state.transform(|_| Ok(())).await);
                })
            }
        })),
        ..Options::new(|| Counter { value: 0 })
    });
    *slot.lock().expect("the state slot is not poisoned") = Some(state.clone());

    batch(async {
        keep(&registrations, state.transform(|_| Ok(())).await);
        keep(
            &registrations,
            other
                .transform(|data| {
                    data.value = 42;
                    Ok(())
                })
                .await,
        );
    })
    .await;

    assert_eq!(*observed.lock().expect("the observed lock is not poisoned"), vec![42, 42]);
}

#[tokio::test]
async fn allows_an_observer_to_await_a_nested_reload() {
    let observed: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(AtomicI64::new(1));
    let reload_again = Arc::new(AtomicBool::new(false));
    let slot: Arc<Mutex<Option<State<Counter>>>> = Arc::new(Mutex::new(None));
    let state = State::new(Options {
        notify: Some(Box::new({
            let observed = observed.clone();
            let source = source.clone();
            let reload_again = reload_again.clone();
            let slot = slot.clone();
            move |_| {
                let state =
                    slot.lock().expect("the state slot is not poisoned").clone().expect("the state is installed");
                let observed = observed.clone();
                let source = source.clone();
                let reload_again = reload_again.clone();
                Box::pin(async move {
                    let value = state.get().await.expect("reads its own state").value;
                    observed.lock().expect("the observed lock is not poisoned").push(value);
                    if !reload_again.swap(false, Ordering::Relaxed) {
                        return;
                    }
                    source.store(3, Ordering::Relaxed);
                    state.reload().await;
                })
            }
        })),
        ..Options::new(|| Counter { value: 0 })
    });
    *slot.lock().expect("the state slot is not poisoned") = Some(state.clone());

    let _registration = state
        .transform({
            let source = source.clone();
            move |data| {
                data.value = source.load(Ordering::Relaxed);
                Ok(())
            }
        })
        .await;

    source.store(2, Ordering::Relaxed);
    reload_again.store(true, Ordering::Relaxed);
    state.reload().await;

    assert_eq!(*observed.lock().expect("the observed lock is not poisoned"), vec![1, 2, 3]);
}

#[tokio::test]
async fn attempts_every_domain_notification_and_preserves_a_batch_failure() {
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let registrations: Arc<Mutex<Vec<Registration>>> = Arc::new(Mutex::new(Vec::new()));
    let first = State::new(Options {
        notify: Some(push_notify(observed.clone(), "first")),
        ..Options::new(|| Data { values: Vec::new() })
    });
    let second = State::new(Options {
        notify: Some(push_notify(observed.clone(), "second")),
        ..Options::new(|| Data { values: Vec::new() })
    });

    let output = batch(async {
        keep(
            &registrations,
            first
                .transform(|data| {
                    data.values.push("first".to_string());
                    Ok(())
                })
                .await,
        );
        keep(
            &registrations,
            second
                .transform(|data| {
                    data.values.push("second".to_string());
                    Ok(())
                })
                .await,
        );
        Err::<(), &'static str>("body failed")
    })
    .await;

    assert_eq!(output, Err("body failed"));
    assert_eq!(
        *observed.lock().expect("the observed lock is not poisoned"),
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(values(&first).await, vec!["first".to_string()]);
    assert_eq!(values(&second).await, vec!["second".to_string()]);
}

#[tokio::test]
async fn a_block_between_native_transforms_sees_both_neighbours() {
    let seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let observed: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let state = State::new(Options::new(|| Counter { value: 0 }));

    batch(async {
        let _set = state
            .transform(|data| {
                data.value = 1;
                Ok(())
            })
            .await;
        let _block = state.block(Arc::new(RecordingBlock { seen: seen.clone() })).await;
        let _observe = state
            .transform({
                let observed = observed.clone();
                move |data| {
                    observed.lock().expect("the observed lock is not poisoned").push(data.value);
                    Ok(())
                }
            })
            .await;
        assert_eq!(state.get().await.expect("reads the state").value, 10);
    })
    .await;

    assert_eq!(*seen.lock().expect("the seen lock is not poisoned"), vec![1]);
    assert_eq!(*observed.lock().expect("the observed lock is not poisoned"), vec![10]);
}
