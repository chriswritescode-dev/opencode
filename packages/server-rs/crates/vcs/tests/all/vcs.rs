use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Definition, PublishOptions};
use opencode_contract::generated::requests::VcsMode;
use opencode_contract::generated::{
    AbsolutePath, FileDiffInfo, FileDiffInfoStatus, LocationRef, VcsBase, VcsBaseSource, VcsBranch, VcsFileStatus,
    VcsFileStatusStatus, VcsInfo,
};
use opencode_location::location::LocationInfo;
use opencode_location::state::batch;
use opencode_vcs::provider::{ProviderDiffInput, ProviderError};
use opencode_vcs::vcs::{DiffOptions, InitializeError};
use opencode_vcs::watcher::{LocationWatcher, LocationWatcherPolicy};
use serde_json::json;

use crate::support::{Fixture, TestProvider};

fn vcs_info(provider: Option<&str>, current: Option<&str>, default: Option<&str>) -> VcsInfo {
    VcsInfo {
        provider: provider.map(str::to_owned),
        branch: VcsBranch { current: current.map(str::to_owned), default: default.map(str::to_owned) },
    }
}

fn location_ref(info: &LocationInfo) -> LocationRef {
    LocationRef { directory: AbsolutePath(info.directory.clone()), workspace_id: None }
}

fn publish_changed(fixture: &Fixture, location: &LocationRef, file: &Path) {
    fixture
        .bus
        .publish(
            &Definition::ephemeral("filesystem.changed"),
            json!({ "file": file.to_string_lossy(), "event": "change" }),
            PublishOptions { location: Some(location.clone()), ..Default::default() },
        )
        .expect("publishes a filesystem change");
}

#[tokio::test]
async fn routes_initialization_through_the_requested_provider() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let read_only = Arc::new(TestProvider::new("read-only"));
    let custom = Arc::new(TestProvider::new("custom").with_init(move |scope| {
        let recorded = recorded.clone();
        Box::pin(async move {
            recorded.lock().expect("the calls lock is not poisoned").push(scope.worktree);
            Ok(())
        })
    }));
    let _registration = vcs
        .transform(move |editor| {
            editor.add(read_only.clone());
            editor.add(custom.clone());
        })
        .await;

    assert_eq!(vcs.initialize("missing").await, Err(InitializeError::Unknown));
    assert_eq!(vcs.initialize("read-only").await, Err(InitializeError::Unsupported));
    assert!(calls.lock().expect("the calls lock is not poisoned").is_empty());
    vcs.initialize("custom").await.expect("initializes the custom provider");
    assert_eq!(*calls.lock().expect("the calls lock is not poisoned"), [directory.to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn initialization_reports_missing_and_conflicting_directories() {
    let fixture = Fixture::new();
    let missing = fixture.path("missing");
    let vcs = fixture.service(&fixture.location(&missing, &missing, false)).await;
    assert_eq!(vcs.initialize("custom").await, Err(InitializeError::Missing));

    let conflict = fixture.path("conflict");
    tokio::fs::create_dir_all(conflict.join(".git")).await.expect("creates the marker directory");
    let vcs = fixture.service(&fixture.location(&conflict, &conflict, false)).await;
    assert_eq!(vcs.initialize("custom").await, Err(InitializeError::Conflict));
}

#[tokio::test]
async fn returns_empty_results_outside_version_control() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    assert_eq!(vcs.info().await, vcs_info(None, None, None));
    assert!(vcs.branches(Default::default()).await.is_empty());
    assert_eq!(vcs.base().await.expect("resolves no base"), None);
    assert!(vcs.status().await.is_empty());
    assert!(vcs.diff(VcsMode::Working, Default::default()).await.expect("diffs the working copy").is_empty());
    assert!(vcs.diff(VcsMode::Branch, Default::default()).await.expect("diffs the branch").is_empty());
}

#[tokio::test]
async fn serves_scoped_providers_and_restores_the_fallback_after_disposal() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    let registration = vcs
        .transform(move |editor| {
            editor.add(Arc::new(TestProvider::new("custom")));
            editor.set_default("custom");
        })
        .await;

    assert_eq!(vcs.info().await, vcs_info(Some("custom"), Some("feature"), Some("main")));
    assert_eq!(vcs.base().await.expect("resolves no base"), None);
    assert_eq!(vcs.branches(Default::default()).await, ["feature", "main"]);
    assert_eq!(
        vcs.status().await,
        [VcsFileStatus {
            file: "file.txt".to_owned(),
            additions: 1.0,
            deletions: 0.0,
            status: VcsFileStatusStatus::Added,
        }]
    );
    assert_eq!(
        vcs.diff(VcsMode::Working, Default::default()).await.expect("diffs the working copy"),
        [FileDiffInfo {
            file: "file.txt".to_owned(),
            patch: "+hello".to_owned(),
            additions: 1.0,
            deletions: 0.0,
            status: FileDiffInfoStatus::Added,
            more: None,
        }]
    );

    registration.dispose().await;
    assert_eq!(vcs.info().await, vcs_info(None, None, None));
    assert!(vcs.status().await.is_empty());
}

#[tokio::test]
async fn automatically_selects_a_provider_matching_the_resolved_repository() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    let (vcs, _git_registration) = fixture.git_service(&project, &project).await;
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));

    let overlay = vcs.transform(move |editor| editor.add(Arc::new(TestProvider::new("git")))).await;
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("feature"), Some("main")));

    overlay.dispose().await;
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));
}

#[tokio::test]
async fn reads_batched_providers_without_refreshing_intermediate_selections() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;
    let reads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut registrations = Vec::new();

    batch(async {
        let intermediate_reads = reads.clone();
        registrations.push(
            vcs.transform(move |editor| {
                let intermediate_reads = intermediate_reads.clone();
                editor.add(Arc::new(TestProvider::new("custom").with_info(move |_| {
                    let reads = intermediate_reads.clone();
                    Box::pin(async move {
                        reads.lock().expect("the reads lock is not poisoned").push("intermediate".to_owned());
                        Ok(VcsInfo {
                            provider: None,
                            branch: VcsBranch { current: Some("intermediate".to_owned()), default: None },
                        })
                    })
                })));
                editor.set_default("custom");
            })
            .await,
        );

        let status = vcs.status().await;
        assert_eq!(status.first().map(|row| row.file.as_str()), Some("file.txt"));
        assert_eq!(vcs.info().await, vcs_info(None, None, None));

        let final_reads = reads.clone();
        registrations.push(
            vcs.transform(move |editor| {
                let final_reads = final_reads.clone();
                editor.add(Arc::new(TestProvider::new("custom").with_info(move |_| {
                    let reads = final_reads.clone();
                    Box::pin(async move {
                        reads.lock().expect("the reads lock is not poisoned").push("final".to_owned());
                        Ok(VcsInfo {
                            provider: None,
                            branch: VcsBranch { current: Some("final".to_owned()), default: None },
                        })
                    })
                })));
            })
            .await,
        );
    })
    .await;

    assert_eq!(reads.lock().expect("the reads lock is not poisoned").as_slice(), ["final"]);
    assert_eq!(vcs.info().await, vcs_info(Some("custom"), Some("final"), None));
}

#[tokio::test]
async fn passes_location_scope_and_bounded_diff_options() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;
    let observed: Arc<Mutex<Vec<ProviderDiffInput>>> = Arc::new(Mutex::new(Vec::new()));

    let _registration = vcs
        .transform({
            let observed = observed.clone();
            move |editor| {
                let observed = observed.clone();
                editor.add(Arc::new(TestProvider::new("custom").with_diff(move |input| {
                    observed.lock().expect("the observed lock is not poisoned").push(input);
                    Box::pin(async { Ok(Vec::new()) })
                })));
                editor.set_default("custom");
            }
        })
        .await;

    let rows = vcs
        .diff(VcsMode::Committed, DiffOptions { context: Some(3), base: Some("release".to_owned()), untracked: None })
        .await
        .expect("diffs the committed changes");
    assert!(rows.is_empty());

    let text = directory.to_string_lossy().into_owned();
    let inputs = observed.lock().expect("the observed lock is not poisoned");
    assert_eq!(inputs.len(), 1);
    let input = &inputs[0];
    assert_eq!(input.scope.directory, text);
    assert_eq!(input.scope.worktree, text);
    assert_eq!(input.scope.canonical, text);
    assert_eq!(input.mode, VcsMode::Committed);
    assert_eq!(input.base.as_deref(), Some("release"));
    assert_eq!(input.context, 3);
    assert_eq!(input.max_output_bytes, 10_000_000);
    assert_eq!(input.untracked, None);
}

#[tokio::test]
async fn validates_provider_results_and_bounds_oversized_patches() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    let _registration = vcs
        .transform(move |editor| {
            editor.add(Arc::new(
                TestProvider::new("custom")
                    .with_status(|_| {
                        Box::pin(async {
                            Ok(vec![VcsFileStatus {
                                file: "file.txt".to_owned(),
                                additions: -1.0,
                                deletions: 0.0,
                                status: VcsFileStatusStatus::Added,
                            }])
                        })
                    })
                    .with_diff(|_| {
                        Box::pin(async {
                            Ok(vec![FileDiffInfo {
                                file: "file.txt".to_owned(),
                                patch: "x".repeat(10_000_001),
                                additions: 1.0,
                                deletions: 0.0,
                                status: FileDiffInfoStatus::Added,
                                more: None,
                            }])
                        })
                    }),
            ));
            editor.set_default("custom");
        })
        .await;

    assert!(vcs.status().await.is_empty());
    let rows = vcs.diff(VcsMode::Working, Default::default()).await.expect("diffs the working copy");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].patch.len() < 1000);
    assert_eq!(rows[0].additions, 1.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_provider_interruption() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    let _registration = vcs
        .transform(move |editor| {
            editor.add(Arc::new(
                TestProvider::new("custom")
                    .with_base(|_| Box::pin(std::future::pending::<Result<Option<VcsBase>, ProviderError>>()))
                    .with_status(|_| Box::pin(std::future::pending::<Result<Vec<VcsFileStatus>, ProviderError>>()))
                    .with_diff(|_| Box::pin(std::future::pending::<Result<Vec<FileDiffInfo>, ProviderError>>())),
            ));
            editor.set_default("custom");
        })
        .await;

    let status = tokio::spawn({
        let vcs = vcs.clone();
        async move { vcs.status().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    status.abort();
    assert!(status.await.expect_err("the status task is interrupted").is_cancelled());

    let diff = tokio::spawn({
        let vcs = vcs.clone();
        async move { vcs.diff(VcsMode::Committed, Default::default()).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    diff.abort();
    assert!(diff.await.expect_err("the diff task is interrupted").is_cancelled());

    let base = tokio::spawn({
        let vcs = vcs.clone();
        async move { vcs.base().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    base.abort();
    assert!(base.await.expect_err("the base task is interrupted").is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caches_branch_info_and_publishes_head_changes() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    let (vcs, _git_registration) = fixture.git_service(&project, &project).await;

    fixture.write(&project.join("file.txt"), "one\n").await;
    fixture.commit_all(&project, "initial").await;
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));

    let info = fixture.location(&project, &project, true);
    let location = location_ref(&info);
    let mut updates = fixture.bus.subscribe(&["vcs.branch.updated"], Some(location.clone()));

    fixture.git(&project, &["checkout", "-q", "-b", "feature"]).await;

    publish_changed(&fixture, &location, &project.join("HEAD"));
    assert!(tokio::time::timeout(Duration::from_millis(250), updates.recv()).await.is_err());
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));

    publish_changed(&fixture, &location, &project.join(".git").join("HEAD"));
    let event = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .expect("publishes a branch update")
        .expect("receives a branch update");
    assert_eq!(event.data()["branch"], json!("feature"));
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("feature"), Some("main")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watches_head_and_publishes_exactly_one_branch_update() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "one\n").await;
    fixture.commit_all(&project, "initial").await;
    let (vcs, _git_registration) = fixture.git_service(&project, &project).await;

    let info = fixture.location(&project, &project, true);
    let mut updates = fixture.bus.subscribe(&["vcs.branch.updated"], Some(location_ref(&info)));
    let watcher = LocationWatcher::start(fixture.git.clone(), fixture.bus.clone(), &info, LocationWatcherPolicy::new());
    tokio::time::sleep(Duration::from_millis(300)).await;

    fixture.git(&project, &["checkout", "-q", "-b", "feature"]).await;
    let event = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .expect("publishes a branch update")
        .expect("receives a branch update");
    assert_eq!(event.data()["branch"], json!("feature"));

    assert!(tokio::time::timeout(Duration::from_millis(400), updates.recv()).await.is_err());
    assert_eq!(vcs.info().await.branch.current.as_deref(), Some("feature"));
    watcher.shutdown();
}

#[tokio::test]
async fn reports_provider_failures_and_validates_optional_base_metadata() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let vcs = fixture.service(&fixture.location(&directory, &directory, false)).await;

    let _registration = vcs
        .transform(move |editor| {
            editor.add(Arc::new(
                TestProvider::new("custom")
                    .with_base(|_| {
                        Box::pin(async {
                            Ok(Some(VcsBase {
                                name: "release".to_owned(),
                                r#ref: "refs/heads/release".to_owned(),
                                source: VcsBaseSource::Reflog,
                            }))
                        })
                    })
                    .with_diff(|_| Box::pin(async { Err(ProviderError::message("failed")) })),
            ));
            editor.set_default("custom");
        })
        .await;

    assert_eq!(
        vcs.base().await.expect("resolves the review base"),
        Some(VcsBase {
            name: "release".to_owned(),
            r#ref: "refs/heads/release".to_owned(),
            source: VcsBaseSource::Reflog,
        })
    );
    let error = vcs.diff(VcsMode::Working, Default::default()).await.expect_err("reports the provider failure");
    assert_eq!(error.message, "VCS provider could not produce a diff");

    let _failing = vcs
        .transform(move |editor| {
            editor.add(Arc::new(
                TestProvider::new("failing").with_base(|_| Box::pin(async { Err(ProviderError::message("failed")) })),
            ));
            editor.set_default("failing");
        })
        .await;

    assert!(vcs.base().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_stops_bus_refreshes() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    let (vcs, _git_registration) = fixture.git_service(&project, &project).await;
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));

    let info = fixture.location(&project, &project, true);
    let location = location_ref(&info);
    let mut updates = fixture.bus.subscribe(&["vcs.branch.updated"], Some(location.clone()));

    vcs.close();
    fixture.git(&project, &["checkout", "-q", "-b", "feature"]).await;

    publish_changed(&fixture, &location, &project.join(".git").join("HEAD"));
    assert!(tokio::time::timeout(Duration::from_millis(250), updates.recv()).await.is_err());
    assert_eq!(vcs.info().await, vcs_info(Some("git"), Some("main"), None));
}
