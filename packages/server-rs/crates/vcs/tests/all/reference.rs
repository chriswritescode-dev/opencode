use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::{AbsolutePath, ConfigEntryDocumentInfo, LocationRef};
use opencode_location::state::{self, Registration};
use opencode_util::path::Flavor;
use opencode_vcs::reference::{GitSource, Info, LocalSource, Reference, Source, Vars, sources_from_entries};
use opencode_vcs::repository;
use opencode_vcs::repository_cache::{EnsureInput, EnsureResult, Error, Materializer, Refresh, Status};
use tokio::sync::{oneshot, watch};

use crate::support::Fixture;

struct MockCache {
    operations: Mutex<Vec<EnsureInput>>,
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

impl MockCache {
    fn new() -> Arc<Self> {
        Arc::new(MockCache { operations: Mutex::new(Vec::new()), started: Mutex::new(None), release: Mutex::new(None) })
    }

    fn blocking() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (started, started_receiver) = oneshot::channel();
        let (release, release_receiver) = oneshot::channel();
        let cache = Arc::new(MockCache {
            operations: Mutex::new(Vec::new()),
            started: Mutex::new(Some(started)),
            release: Mutex::new(Some(release_receiver)),
        });
        (cache, started_receiver, release)
    }

    fn operations(&self) -> Vec<EnsureInput> {
        self.operations.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }
}

impl Materializer for MockCache {
    fn ensure(&self, input: EnsureInput) -> BoxFuture<'_, Result<EnsureResult, Error>> {
        Box::pin(async move {
            self.operations.lock().unwrap_or_else(|error| error.into_inner()).push(input.clone());
            if let Some(started) = self.started.lock().unwrap_or_else(|error| error.into_inner()).take() {
                let _ = started.send(());
            }
            let release = self.release.lock().unwrap_or_else(|error| error.into_inner()).take();
            if let Some(release) = release {
                let _ = release.await;
            }
            Ok(EnsureResult {
                repository: input.reference.label.clone(),
                host: input.reference.host.clone(),
                remote: input.reference.remote.clone(),
                local_path: String::new(),
                status: Status::Cached,
                head: None,
                branch: input.branch,
            })
        })
    }
}

fn vars() -> Vars {
    Arc::new(|name: &str| std::env::var_os(name))
}

fn build(fixture: &Fixture, cache: Arc<dyn Materializer>) -> (Arc<Reference>, LocationRef) {
    let location = LocationRef { directory: AbsolutePath("/tmp/location".to_owned()), workspace_id: None };
    let reference = Reference::new(fixture.bus.clone(), location.clone(), cache, fixture.root.join("repos"), vars());
    (reference, location)
}

fn local(path: &str) -> Source {
    Source::Local(LocalSource { path: path.to_owned(), description: None, hidden: None })
}

fn git(repository: &str, branch: Option<&str>) -> Source {
    Source::Git(GitSource {
        repository: repository.to_owned(),
        branch: branch.map(str::to_owned),
        description: None,
        hidden: None,
    })
}

fn git_cache_path(repos: &Path, repository: &str, branch: Option<&str>) -> String {
    let parsed = repository::parse_remote(repository, &vars()).expect("parses the reference");
    repository::cache_path(&repos.to_string_lossy(), &repository::Reference::Remote(parsed), branch)
}

async fn names(reference: &Reference) -> Vec<String> {
    reference.list().await.into_iter().map(|info| info.name).collect()
}

async fn paths(reference: &Reference) -> Vec<String> {
    reference.list().await.into_iter().map(|info| info.path).collect()
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn observed_names(observed: &Arc<Mutex<Vec<Vec<String>>>>) -> Vec<Vec<String>> {
    observed.lock().unwrap_or_else(|error| error.into_inner()).clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_the_current_editor_source_by_name() {
    let fixture = Fixture::new();
    let (reference, _location) = build(&fixture, MockCache::new());

    let first = reference.transform(|editor| editor.add("docs", local("/docs"))).await;
    let second = reference
        .transform(|editor| {
            let listed = editor.list();
            assert_eq!(editor.get("docs"), Some(&listed[0].1));
            assert_eq!(editor.get("docs").cloned(), Some(local("/docs")));
            assert!(editor.get("missing").is_none());

            let replacement = git("owner/repo", None);
            editor.add("docs", replacement.clone());
            assert_eq!(editor.get("docs").cloned(), Some(replacement));

            editor.remove("docs");
            assert!(editor.get("docs").is_none());
        })
        .await;

    drop(second);
    drop(first);
    assert!(reference.list().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_batched_references_before_cache_work_and_update_events() {
    let fixture = Fixture::new();
    let (cache, started, release) = MockCache::blocking();
    let (reference, location) = build(&fixture, cache.clone());

    let observed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut subscription = fixture.bus.subscribe(&["reference.updated"], Some(location.clone()));
    let collector = {
        let reference = reference.clone();
        let observed = observed.clone();
        tokio::spawn(async move {
            while subscription.recv().await.is_some() {
                let names = names(&reference).await;
                observed.lock().unwrap_or_else(|error| error.into_inner()).push(names);
            }
        })
    };

    let sdk = Source::Git(GitSource {
        repository: "owner/repo".to_owned(),
        branch: Some("feature/docs".to_owned()),
        description: Some("SDK documentation".to_owned()),
        hidden: Some(true),
    });

    let mut registrations = Vec::new();
    state::batch(async {
        registrations.push(reference.transform(|editor| editor.add("docs", local("/docs"))).await);
        assert_eq!(names(&reference).await, vec!["docs".to_owned()]);

        registrations.push(
            reference
                .transform(move |editor| {
                    editor.add("sdk", sdk.clone());
                    editor.add("invalid", git("invalid", None));
                    editor.add("invalid-branch", git("owner/repo", Some("../escape")));
                    editor.add("file", git("file:///docs", None));
                })
                .await,
        );

        let infos = reference.list().await;
        let listed: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
        assert_eq!(listed, vec!["docs", "sdk"]);
        assert_eq!(infos[1].path, git_cache_path(&fixture.root.join("repos"), "owner/repo", Some("feature/docs")));
        assert_eq!(infos[1].description.as_deref(), Some("SDK documentation"));
        assert_eq!(infos[1].hidden, Some(true));

        tokio::task::yield_now().await;
        assert!(cache.operations().is_empty());
        assert!(observed_names(&observed).is_empty());
    })
    .await;

    wait_for("one reference update", || observed_names(&observed).len() == 1).await;
    assert_eq!(observed_names(&observed), vec![vec!["docs".to_owned(), "sdk".to_owned()]]);

    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .expect("cache work starts")
        .expect("the cache start signal arrives");

    let operations = cache.operations();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].reference.label, "owner/repo");
    assert_eq!(operations[0].branch.as_deref(), Some("feature/docs"));
    assert_eq!(operations[0].refresh, Some(Refresh::Daily));

    assert_eq!(names(&reference).await, vec!["docs".to_owned(), "sdk".to_owned()]);
    tokio::time::timeout(Duration::from_secs(5), tokio::time::sleep(Duration::from_millis(150)))
        .await
        .expect("waits briefly for a second operation");
    assert_eq!(cache.operations().len(), 1);
    assert_eq!(observed_names(&observed).len(), 1);

    release.send(()).expect("releases the cache work");
    collector.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lets_update_listeners_replace_references_and_refetch_current_info() {
    let fixture = Fixture::new();
    let (reference, location) = build(&fixture, MockCache::new());
    let observed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let (settled, settled_receiver) = watch::channel(false);

    let reentry: Arc<Mutex<Option<Registration>>> = Arc::new(Mutex::new(None));
    let listener = {
        let reference = reference.clone();
        let reentered = Arc::new(AtomicBool::new(false));
        let settled = settled.clone();
        let reentry = reentry.clone();
        fixture.bus.listen(Box::new(move |event| {
            if event.r#type() != "reference.updated" || reentered.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let reference = reference.clone();
            let settled = settled.clone();
            let reentry = reentry.clone();
            tokio::spawn(async move {
                let registration = reference.transform(|editor| editor.add("docs", local("/new"))).await;
                *reentry.lock().unwrap_or_else(|error| error.into_inner()) = Some(registration);
                settled.send_replace(true);
            });
            Ok(())
        }))
    };

    let mut subscription = fixture.bus.subscribe(&["reference.updated"], Some(location.clone()));
    let collector = {
        let reference = reference.clone();
        let observed = observed.clone();
        let mut settled = settled_receiver;
        tokio::spawn(async move {
            while subscription.recv().await.is_some() {
                while !*settled.borrow() {
                    if settled.changed().await.is_err() {
                        break;
                    }
                }
                let paths = paths(&reference).await;
                observed.lock().unwrap_or_else(|error| error.into_inner()).push(paths);
            }
        })
    };

    let _registration = reference.transform(|editor| editor.add("docs", local("/old"))).await;

    wait_for("two reference updates", || observed_names(&observed).len() >= 2).await;
    assert_eq!(observed_names(&observed), vec![vec!["/new".to_owned()], vec!["/new".to_owned()]]);
    assert_eq!(paths(&reference).await, vec!["/new".to_owned()]);

    drop(listener);
    collector.abort();
    drop(reentry);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_normalized_sources_for_the_owning_scope() {
    let fixture = Fixture::new();
    let (reference, _location) = build(&fixture, MockCache::new());
    let source = Source::Local(LocalSource {
        path: "/docs".to_owned(),
        description: Some("Use for API documentation".to_owned()),
        hidden: Some(true),
    });

    let registration = reference
        .transform({
            let source = source.clone();
            move |editor| editor.add("docs", source.clone())
        })
        .await;
    assert_eq!(
        reference.list().await,
        vec![Info {
            name: "docs".to_owned(),
            path: "/docs".to_owned(),
            description: Some("Use for API documentation".to_owned()),
            hidden: Some(true),
            source: source.clone(),
        }]
    );

    registration.dispose().await;
    assert!(reference.list().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derives_git_paths_without_exposing_cache_operations() {
    let fixture = Fixture::new();
    let (reference, _location) = build(&fixture, MockCache::new());
    let source = git("owner/repo", Some("main"));

    let _registration = reference
        .transform({
            let source = source.clone();
            move |editor| editor.add("sdk", source.clone())
        })
        .await;

    assert_eq!(
        reference.list().await,
        vec![Info {
            name: "sdk".to_owned(),
            path: git_cache_path(&fixture.root.join("repos"), "owner/repo", Some("main")),
            description: None,
            hidden: None,
            source,
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_configured_git_descriptions() {
    let fixture = Fixture::new();
    let (reference, _location) = build(&fixture, MockCache::new());
    let source = Source::Git(GitSource {
        repository: "owner/repo".to_owned(),
        branch: None,
        description: Some("Use for SDK implementation details".to_owned()),
        hidden: None,
    });

    let _registration = reference
        .transform({
            let source = source.clone();
            move |editor| editor.add("sdk", source.clone())
        })
        .await;

    assert_eq!(
        reference.list().await,
        vec![Info {
            name: "sdk".to_owned(),
            path: git_cache_path(&fixture.root.join("repos"), "owner/repo", None),
            description: Some("Use for SDK implementation details".to_owned()),
            hidden: None,
            source,
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_reference_precedence_and_insertion_order_across_documents() {
    let entries = vec![
        document(Some("/config/first/opencode.json"), r#"{"shared":"./shared","first":"./first"}"#),
        document(Some("/config/second/opencode.json"), r#"{"shared":"./shared","second":"./second"}"#),
        document(
            None,
            concat!(
                r#"{"location":"./local","home":"~/project","absolute":"/absolute/x","#,
                r#""bad/name":"./bad","bad name":"./bad","bad`name":"./bad","bad,name":"./bad","#,
                r#""invalid":"invalid","file":"file:///docs","#,
                r#""escape":{"repository":"owner/repo","branch":"../escape"}}"#
            ),
        ),
    ];

    let sources = sources_from_entries(&entries, "/location", "/home");
    let listed: Vec<&str> = sources.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(listed, vec!["shared", "first", "second", "location", "home", "absolute", "invalid", "file", "escape"]);
    assert_eq!(local_path(&sources, "shared"), Flavor::NATIVE.resolve(&["/config/second", "./shared"]));
    assert_eq!(local_path(&sources, "location"), Flavor::NATIVE.resolve(&["/location", "./local"]));
    assert_eq!(local_path(&sources, "home"), Flavor::NATIVE.join(&["/home", "project"]));
    assert_eq!(local_path(&sources, "absolute"), "/absolute/x");
    for name in ["invalid", "file", "escape"] {
        assert!(matches!(find(&sources, name), Source::Git(_)));
    }

    let fixture = Fixture::new();
    let (reference, _location) = build(&fixture, MockCache::new());
    let _external = reference.transform(|editor| editor.add("external", local("/references/external"))).await;
    let _configured = reference
        .transform(move |editor| {
            for (name, source) in &sources {
                editor.add(name.clone(), source.clone());
            }
        })
        .await;

    assert_eq!(names(&reference).await, vec!["external", "shared", "first", "second", "location", "home", "absolute"]);
}

fn document(path: Option<&str>, references: &str) -> Entry {
    let text = format!(r#"{{"references":{references}}}"#);
    let info = decode_json::<ConfigEntryDocumentInfo>(&text).expect("decodes a document");
    Entry::Document(Document { path: path.map(PathBuf::from), info, origin: Origin::File })
}

fn find<'a>(sources: &'a [(String, Source)], name: &str) -> &'a Source {
    &sources.iter().find(|(candidate, _)| candidate == name).expect("finds the source").1
}

fn local_path(sources: &[(String, Source)], name: &str) -> String {
    match find(sources, name) {
        Source::Local(local) => local.path.clone(),
        Source::Git(_) => panic!("{name} is a Git source"),
    }
}
