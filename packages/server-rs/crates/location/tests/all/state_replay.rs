use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use opencode_location::state::{NotifyFn, Options, Registration, State, batch};

#[derive(Clone, Copy, Debug)]
struct Operation {
    multiply: i64,
    add: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct Value {
    value: i64,
    order: Vec<usize>,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn range(&mut self, low: i64, high: i64) -> i64 {
        low + (self.next() % (high - low + 1) as u64) as i64
    }
}

fn apply(value: i64, operation: Operation) -> i64 {
    (value * operation.multiply + operation.add) % 10_007
}

fn generate_operation(rng: &mut Rng) -> Operation {
    let multiply = [-3, -2, 2, 3][rng.below(4)];
    Operation { multiply, add: rng.range(-9, 9) }
}

fn generate_operations(rng: &mut Rng, high: i64) -> Vec<Operation> {
    (0..rng.range(1, high)).map(|_| generate_operation(rng)).collect()
}

fn counting<S: Send + Sync + 'static>(count: Arc<AtomicUsize>) -> NotifyFn<S> {
    Box::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    })
}

fn read_source(sources: &Mutex<[i64; 2]>, target: usize) -> i64 {
    sources.lock().expect("the sources lock is not poisoned")[target]
}

#[derive(Clone, Copy, Debug)]
enum Command {
    Append { target: usize, callback: usize },
    Read { target: usize },
    Dispose { target: usize, registration: usize },
    Reload { target: usize, source: i64 },
}

fn generate_command(rng: &mut Rng, callbacks: usize) -> Command {
    let target = rng.below(2);
    match rng.below(4) {
        0 => Command::Append { target, callback: rng.below(callbacks) },
        1 => Command::Read { target },
        2 => Command::Dispose { target, registration: rng.next() as usize },
        _ => Command::Reload { target, source: rng.range(-100, 100) },
    }
}

fn generate_commands(rng: &mut Rng, callbacks: usize) -> Vec<Command> {
    (0..rng.below(49)).map(|_| generate_command(rng, callbacks)).collect()
}

struct Entry {
    registration: Registration,
    callback: usize,
    active: bool,
}

fn expected(entries: &[Entry], operations: &[Operation], source: i64) -> Value {
    let order: Vec<usize> = entries.iter().filter(|entry| entry.active).map(|entry| entry.callback).collect();
    let mut value = source;
    for callback in &order {
        value = apply(value, operations[*callback]);
    }
    Value { value, order }
}

#[tokio::test]
async fn matches_a_full_fold_across_reads_registrations_removals_and_batched_reloads() {
    for seed in 1..=24 {
        let mut rng = Rng::new(seed);
        let initial = [rng.range(-100, 100), rng.range(-100, 100)];
        let operations = generate_operations(&mut rng, 6);
        let batch_count = rng.range(1, 8) as usize;
        let batches: Vec<Vec<Command>> =
            (0..batch_count).map(|_| generate_commands(&mut rng, operations.len())).collect();

        let sources = Arc::new(Mutex::new(initial));
        let notifications: [Arc<AtomicUsize>; 2] = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
        let calls = Arc::new(AtomicUsize::new(0));
        let states: [State<Value>; 2] = std::array::from_fn(|target| {
            let sources = Arc::clone(&sources);
            let notifications = Arc::clone(&notifications[target]);
            let mut options = Options::new(move || Value { value: read_source(&sources, target), order: Vec::new() });
            options.notify = Some(counting(notifications));
            State::new(options)
        });
        let mut entries: [Vec<Entry>; 2] = [Vec::new(), Vec::new()];

        for commands in &batches {
            let mut dirty = [false; 2];
            let snapshot: [usize; 2] = std::array::from_fn(|target| notifications[target].load(Ordering::Relaxed));
            batch(async {
                for command in commands {
                    match *command {
                        Command::Append { target, callback } => {
                            let operation = operations[callback];
                            let counter = Arc::clone(&calls);
                            let registration = states[target]
                                .transform(move |editor: &mut Value| {
                                    counter.fetch_add(1, Ordering::Relaxed);
                                    editor.value = apply(editor.value, operation);
                                    editor.order.push(callback);
                                    Ok(())
                                })
                                .await;
                            entries[target].push(Entry { registration, callback, active: true });
                            dirty[target] = true;
                        }
                        Command::Read { target } => {
                            let current = read_source(&sources, target);
                            let actual = states[target].get().await.expect("reads the state");
                            assert_eq!(*actual, expected(&entries[target], &operations, current));
                        }
                        Command::Dispose { target, registration } => {
                            if entries[target].is_empty() {
                                continue;
                            }
                            let index = registration % entries[target].len();
                            let entry = &mut entries[target][index];
                            entry.registration.dispose().await;
                            if entry.active {
                                entry.active = false;
                                dirty[target] = true;
                            }
                        }
                        Command::Reload { target, source } => {
                            sources.lock().expect("the sources lock is not poisoned")[target] = source;
                            states[target].reload().await;
                            dirty[target] = true;
                        }
                    }
                }
                for (count, before) in notifications.iter().zip(snapshot) {
                    assert_eq!(count.load(Ordering::Relaxed), before);
                }
            })
            .await;

            let counts: Vec<usize> = notifications.iter().map(|count| count.load(Ordering::Relaxed)).collect();
            let expected_counts: Vec<usize> =
                snapshot.iter().zip(dirty).map(|(before, dirty)| before + usize::from(dirty)).collect();
            assert_eq!(counts, expected_counts);

            let flushed = calls.load(Ordering::Relaxed);
            for (target, (state, entries)) in states.iter().zip(entries.iter()).enumerate() {
                let current = read_source(&sources, target);
                let actual = state.get().await.expect("reads the state");
                assert_eq!(*actual, expected(entries, &operations, current));
            }
            assert_eq!(calls.load(Ordering::Relaxed), flushed);
        }
    }
}

struct Chunk {
    append: Vec<Operation>,
    reads: usize,
}

fn generate_chunk(rng: &mut Rng) -> Chunk {
    let append = generate_operations(rng, 8);
    let reads = rng.range(1, 5) as usize;
    Chunk { append, reads }
}

async fn remember(state: &State<Value>, retained: &mut Vec<(Arc<Value>, Value)>) {
    let value = state.get().await.expect("reads the state");
    retained.push((Arc::clone(&value), (*value).clone()));
}

#[tokio::test]
async fn rebuilds_all_active_callbacks_once_per_change_and_never_touches_retained_values() {
    for seed in 1..=16 {
        let mut rng = Rng::new(seed);
        let chunk_count = rng.range(1, 8) as usize;
        let chunks: Vec<Chunk> = (0..chunk_count).map(|_| generate_chunk(&mut rng)).collect();
        let removal = rng.next() as usize;

        let calls = Arc::new(AtomicUsize::new(0));
        let state = State::new(Options::new(|| Value { value: 1, order: Vec::new() }));
        let mut registrations: Vec<Registration> = Vec::new();
        let mut retained: Vec<(Arc<Value>, Value)> = Vec::new();
        let mut settled = 0usize;
        batch(async {
            for chunk in &chunks {
                let before = calls.load(Ordering::Relaxed);
                for operation in &chunk.append {
                    let operation = *operation;
                    let counter = Arc::clone(&calls);
                    let registration = state
                        .transform(move |editor: &mut Value| {
                            counter.fetch_add(1, Ordering::Relaxed);
                            editor.value = apply(editor.value, operation);
                            editor.order.push(editor.order.len());
                            Ok(())
                        })
                        .await;
                    registrations.push(registration);
                }
                assert_eq!(calls.load(Ordering::Relaxed), before);
                state.get().await.expect("rebuilds the state");
                let rebuilt = before + registrations.len();
                assert_eq!(calls.load(Ordering::Relaxed), rebuilt);
                for _ in 0..chunk.reads {
                    let first = state.get().await.expect("reads the state");
                    let second = state.get().await.expect("reads the state");
                    assert!(Arc::ptr_eq(&first, &second));
                    assert_eq!(calls.load(Ordering::Relaxed), rebuilt);
                }
                remember(&state, &mut retained).await;
            }

            let removed = removal % registrations.len();
            registrations[removed].dispose().await;
            let before = calls.load(Ordering::Relaxed);
            state.get().await.expect("rebuilds after a removal");
            assert_eq!(calls.load(Ordering::Relaxed), before + registrations.len() - 1);
            remember(&state, &mut retained).await;

            let replayed = calls.load(Ordering::Relaxed);
            registrations[removed].dispose().await;
            state.get().await.expect("reads the state");
            assert_eq!(calls.load(Ordering::Relaxed), replayed);
            state.reload().await;
            state.reload().await;
            assert_eq!(calls.load(Ordering::Relaxed), replayed);
            settled = replayed;
        })
        .await;

        assert_eq!(calls.load(Ordering::Relaxed), settled + registrations.len() - 1);
        state.get().await.expect("reads the rebuilt state");
        assert_eq!(calls.load(Ordering::Relaxed), settled + registrations.len() - 1);
        for (value, snapshot) in &retained {
            assert_eq!(**value, *snapshot);
        }
        let pointers: HashSet<usize> = retained.iter().map(|(value, _)| Arc::as_ptr(value) as usize).collect();
        assert_eq!(pointers.len(), retained.len());
    }
}
