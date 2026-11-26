use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_bus::Subscription;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_git::Git;
use opencode_storage::project;
use opencode_storage::worktree as storage_worktree;
use opencode_util::path::Flavor;
use opencode_vcs::worktree::{CreateInput, RemoveInput, StrategiesLoader, Worktree, WorktreeError};
use opencode_vcs::worktree_strategies::{
    GitWorktreeStrategy, WorktreeEntryKind, WorktreeListEntry, WorktreeStrategies, WorktreeStrategy,
};
use rusqlite::params;

use crate::support::Fixture;

const PROJECT: &str = "worktree-project";

struct WorktreeFixture {
    fixture: Fixture,
    canonical: PathBuf,
    data: PathBuf,
    strategies: Arc<WorktreeStrategies>,
    worktree: Arc<Worktree>,
}

impl WorktreeFixture {
    async fn new() -> Self {
        let fixture = Fixture::new();
        let canonical = fixture.path("repo");
        fixture.init_git(&canonical).await;
        fixture.git(&canonical, &["commit", "--allow-empty", "-m", "root"]).await;
        fixture
            .database
            .read(|connection| project::upsert(connection, PROJECT, &text(&canonical), Some("git")))
            .expect("inserts the project");
        fixture
            .database
            .read(|connection| storage_worktree::insert_ignore(connection, PROJECT, &text(&canonical), None))
            .expect("seeds the canonical worktree row");
        let data = fixture.path("data");
        let strategies = Arc::new(WorktreeStrategies::new(fixture.git.clone(), PROJECT, &text(&canonical), &data));
        let worktree =
            Arc::new(Worktree::new(fixture.database.clone(), fixture.bus.clone(), loader(strategies.clone())));
        WorktreeFixture { fixture, canonical, data, strategies, worktree }
    }

    fn canonical_text(&self) -> String {
        text(&self.canonical)
    }

    fn default_parent(&self, name: &str) -> String {
        Flavor::NATIVE.join(&[&text(&self.data), "worktree", "worktr", name])
    }

    fn create(&self) -> CreateInput {
        CreateInput { project_id: PROJECT.to_owned(), ..Default::default() }
    }

    fn remove(&self, directory: &str, force: bool) -> RemoveInput {
        RemoveInput { project_id: PROJECT.to_owned(), directory: directory.to_owned(), force }
    }

    fn stored(&self) -> Vec<(String, Option<String>)> {
        let mut rows = self
            .fixture
            .database
            .read(|connection| storage_worktree::list(connection, PROJECT))
            .expect("lists the worktree rows");
        rows.sort_by(|a, b| a.directory.cmp(&b.directory));
        rows.into_iter().map(|row| (row.directory, row.strategy)).collect()
    }

    fn set_strategy(&self, directory: &str, strategy: Option<&str>) {
        self.fixture
            .database
            .read(|connection| storage_worktree::upsert(connection, PROJECT, directory, strategy, true))
            .expect("sets the strategy");
    }

    fn set_commands(&self, command: &str) {
        let commands = serde_json::json!({ "start": command }).to_string();
        self.fixture
            .database
            .read(|connection| {
                connection.execute("UPDATE project SET commands = ?1 WHERE id = ?2", params![commands, PROJECT])?;
                Ok(())
            })
            .expect("sets the setup command");
    }

    fn insert_row(&self, directory: &str, strategy: Option<&str>) {
        self.fixture
            .database
            .read(|connection| storage_worktree::insert_ignore(connection, PROJECT, directory, strategy))
            .expect("inserts the worktree row");
    }

    fn delete_row(&self, directory: &str) {
        self.fixture
            .database
            .read(|connection| storage_worktree::delete(connection, PROJECT, directory))
            .expect("deletes the worktree row");
    }

    async fn add_worktree(&self, directory: &Path) {
        let directory = text(directory);
        self.fixture.git(&self.canonical, &["worktree", "add", "--detach", &directory, "HEAD"]).await;
    }

    async fn remove_worktree(&self, directory: &Path) {
        let directory = text(directory);
        self.fixture.git(&self.canonical, &["worktree", "remove", "--force", &directory]).await;
    }

    async fn git_head(&self, directory: &Path) -> String {
        let output = self
            .fixture
            .git
            .execute(
                &["rev-parse", "HEAD"],
                opencode_git::RunOptions { cwd: Some(text(directory)), ..Default::default() },
            )
            .await
            .expect("runs git");
        assert_eq!(output.status, 0, "git rev-parse failed: {}", output.stderr_text());
        output.text().trim().to_owned()
    }

    async fn subscribe(&self) -> Subscription {
        self.fixture.bus.subscribe(&["worktree.updated"], None)
    }
}

fn loader(strategies: Arc<WorktreeStrategies>) -> StrategiesLoader {
    Arc::new(move |_directory| {
        let strategies = strategies.clone();
        Box::pin(async move { Ok(strategies) })
    })
}

fn text(path: &Path) -> String {
    path.to_str().expect("path is UTF-8").to_owned()
}

async fn next_update(subscription: &mut Subscription) -> serde_json::Value {
    let event = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .expect("a worktree update arrives")
        .expect("the subscription is open");
    event.data().clone()
}

type ListFn = Arc<dyn Fn(String) -> BoxFuture<'static, Result<Vec<WorktreeListEntry>, WorktreeError>> + Send + Sync>;

struct TestStrategy {
    id: String,
    inner: Arc<dyn WorktreeStrategy>,
    list: Option<ListFn>,
    create_error: Option<String>,
}

impl TestStrategy {
    fn alias(id: &str, git: Arc<Git>) -> Arc<dyn WorktreeStrategy> {
        Arc::new(GitWorktreeStrategy::new(id, git))
    }

    fn broken_create(id: &str, git: Arc<Git>, message: &str) -> Arc<dyn WorktreeStrategy> {
        Arc::new(TestStrategy {
            id: id.to_owned(),
            inner: Self::alias(id, git),
            list: None,
            create_error: Some(message.to_owned()),
        })
    }

    fn broken_list(id: &str, git: Arc<Git>) -> Arc<dyn WorktreeStrategy> {
        Arc::new(TestStrategy {
            id: id.to_owned(),
            inner: Self::alias(id, git),
            list: Some(Arc::new(|_| Box::pin(async { Err(WorktreeError::Message("cannot enumerate".to_owned())) }))),
            create_error: None,
        })
    }

    fn reported(
        id: &str,
        git: Arc<Git>,
        directory: PathBuf,
    ) -> (Arc<dyn WorktreeStrategy>, Arc<std::sync::Mutex<Vec<String>>>) {
        let sources = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = sources.clone();
        let strategy = Arc::new(TestStrategy {
            id: id.to_owned(),
            inner: Self::alias(id, git),
            list: Some(Arc::new(move |source| {
                let directory = directory.clone();
                let recorded = recorded.clone();
                Box::pin(async move {
                    recorded.lock().expect("the sources lock is not poisoned").push(source);
                    Ok(vec![WorktreeListEntry { directory: text(&directory), kind: WorktreeEntryKind::Worktree }])
                })
            })),
            create_error: None,
        });
        (strategy, sources)
    }
}

impl WorktreeStrategy for TestStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn create<'a>(
        &'a self,
        input: opencode_vcs::worktree_strategies::StrategyCreateInput<'a>,
    ) -> BoxFuture<'a, Result<String, WorktreeError>> {
        match &self.create_error {
            Some(message) => {
                let message = message.clone();
                Box::pin(async move { Err(WorktreeError::Message(message)) })
            }
            None => self.inner.create(input),
        }
    }

    fn remove<'a>(
        &'a self,
        input: opencode_vcs::worktree_strategies::StrategyRemoveInput<'a>,
    ) -> BoxFuture<'a, Result<(), WorktreeError>> {
        self.inner.remove(input)
    }

    fn list<'a>(&'a self, directory: &'a str) -> BoxFuture<'a, Result<Vec<WorktreeListEntry>, WorktreeError>> {
        match &self.list {
            Some(list) => list(directory.to_owned()),
            None => self.inner.list(directory),
        }
    }
}

async fn start_config(project: &Path, home: &Path) -> ConfigService {
    std::fs::create_dir_all(home).expect("creates the home directory");
    let config = home.join("config");
    std::fs::create_dir_all(&config).expect("creates the config directory");
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.as_os_str().to_owned()),
        "OPENCODE_CONFIG_DIR" => Some(config.as_os_str().to_owned()),
        _ => None,
    })
    .expect("builds the paths");
    let options = ConfigOptions {
        project: Some(true),
        global: Some(false),
        system: Some(SystemOption::Disabled),
        claude: Some(false),
        file: None,
        content: None,
    };
    ConfigService::start(
        options,
        ConfigLocation { directory: project.to_path_buf(), project_id: PROJECT.to_owned() },
        paths,
        ServiceDeps {
            credentials: Arc::new(()),
            wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
            channel: "local".to_owned(),
            flag_overrides: None,
        },
    )
    .await
}

async fn config_with(project: &Path, home: &Path, directory: &str) -> ConfigService {
    let info = format!(r#"{{"worktree":{{"directory":{}}}}}"#, serde_json::to_string(directory).expect("encodes"));
    std::fs::write(project.join("opencode.json"), info).expect("writes the project config");
    start_config(project, home).await
}

#[tokio::test]
async fn creates_and_removes_a_git_worktree_directory() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("created");
    let target = parent.join("worktree");
    let mut updates = fixture.subscribe().await;

    let created = fixture
        .worktree
        .create(CreateInput { directory: Some(text(&parent)), name: Some("worktree".to_owned()), ..fixture.create() })
        .await
        .expect("creates the worktree");

    assert_eq!(created.directory, text(&target));
    assert_eq!(
        fixture.stored(),
        vec![(created.directory.clone(), Some("git".to_owned())), (fixture.canonical_text(), None)]
    );
    assert_eq!(next_update(&mut updates).await, serde_json::json!({ "projectID": PROJECT }));

    fixture.worktree.remove(fixture.remove(&created.directory, false)).await.expect("removes the worktree");

    assert_eq!(fixture.stored(), vec![(fixture.canonical_text(), None)]);
    assert!(!target.exists());
}

#[tokio::test]
async fn defaults_to_the_data_worktree_directory_and_suffixes_duplicates() {
    let fixture = WorktreeFixture::new().await;

    let created = fixture
        .worktree
        .create(CreateInput { from: Some(fixture.canonical_text()), name: Some("task".to_owned()), ..fixture.create() })
        .await
        .expect("creates the first worktree");
    let duplicate = fixture
        .worktree
        .create(CreateInput { from: Some(fixture.canonical_text()), name: Some("task".to_owned()), ..fixture.create() })
        .await
        .expect("creates the duplicate worktree");

    assert_eq!(created.directory, fixture.default_parent("task"));
    assert_eq!(duplicate.directory, fixture.default_parent("task-2"));
    assert!(Path::new(&created.directory).join(".git").exists());
    fixture.worktree.remove(fixture.remove(&created.directory, false)).await.expect("removes the first");
    fixture.worktree.remove(fixture.remove(&duplicate.directory, false)).await.expect("removes the duplicate");
}

#[tokio::test]
async fn runs_the_project_setup_script_with_worktree_paths() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("setup");
    fixture
        .set_commands("printf '%s\\n' \"$OPENCODE_WORKTREE_BASE\" \"$OPENCODE_WORKTREE_PATH\" \"$PWD\" > setup.json");

    let created = fixture
        .worktree
        .create(CreateInput { directory: Some(text(&parent)), name: Some("worktree".to_owned()), ..fixture.create() })
        .await
        .expect("creates the worktree and runs the setup script");

    let written = std::fs::read_to_string(Path::new(&created.directory).join("setup.json"))
        .expect("the setup script wrote a file");
    assert_eq!(
        written.lines().collect::<Vec<_>>(),
        [fixture.canonical_text().as_str(), created.directory.as_str(), created.directory.as_str()]
    );
    fixture.worktree.remove(fixture.remove(&created.directory, true)).await.expect("removes the worktree");
}

#[tokio::test]
async fn creates_a_git_worktree_from_a_selected_branch() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("branch");
    fixture.fixture.git(&fixture.canonical, &["branch", "feature-base"]).await;
    let branch = {
        let output = fixture
            .fixture
            .git
            .execute(
                &["rev-parse", "feature-base"],
                opencode_git::RunOptions { cwd: Some(fixture.canonical_text()), ..Default::default() },
            )
            .await
            .expect("runs git");
        output.text().trim().to_owned()
    };

    let created = fixture
        .worktree
        .create(CreateInput {
            branch: Some("feature-base".to_owned()),
            directory: Some(text(&parent)),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect("creates from the branch");

    assert_eq!(fixture.git_head(Path::new(&created.directory)).await, branch);
}

#[tokio::test]
async fn does_not_interpret_a_branch_as_a_git_option() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("options");

    let error = fixture
        .worktree
        .create(CreateInput {
            branch: Some("--no-checkout".to_owned()),
            directory: Some(text(&parent)),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect_err("the branch is not an option");

    assert!(matches!(error, WorktreeError::Git(_)));
    assert!(!parent.join("worktree").exists());
}

#[tokio::test]
async fn rejects_a_missing_source_directory() {
    let fixture = WorktreeFixture::new().await;
    let missing = fixture.fixture.path("does-not-exist");

    let error = fixture
        .worktree
        .create(CreateInput {
            from: Some(text(&missing)),
            directory: Some(text(&fixture.fixture.path("missing"))),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect_err("the source is missing");

    assert!(matches!(error, WorktreeError::DirectoryUnavailable { .. }));
}

#[tokio::test]
async fn requires_a_tracked_project_worktree() {
    let fixture = WorktreeFixture::new().await;
    fixture.delete_row(&fixture.canonical_text());

    let error = fixture
        .worktree
        .create(CreateInput {
            from: Some(fixture.canonical_text()),
            directory: Some(text(&fixture.fixture.path("untracked"))),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect_err("the source row is missing");

    assert!(matches!(
        error,
        WorktreeError::SourceDirectoryNotFound { project_id, directory: Some(directory) }
            if project_id == PROJECT && directory == fixture.canonical_text()
    ));
}

#[tokio::test]
async fn creates_from_another_managed_worktree() {
    let fixture = WorktreeFixture::new().await;
    let source_parent = fixture.fixture.path("managed-source");
    let target_parent = fixture.fixture.path("managed-target");
    let source = fixture
        .worktree
        .create(CreateInput {
            from: Some(fixture.canonical_text()),
            directory: Some(text(&source_parent)),
            name: Some("source".to_owned()),
            ..fixture.create()
        })
        .await
        .expect("creates the source worktree");
    fixture.delete_row(&fixture.canonical_text());

    let created = fixture
        .worktree
        .create(CreateInput {
            from: Some(source.directory.clone()),
            directory: Some(text(&target_parent)),
            name: Some("target".to_owned()),
            ..fixture.create()
        })
        .await
        .expect("creates from the managed worktree");

    assert_eq!(created.directory, text(&target_parent.join("target")));
    fixture.worktree.remove(fixture.remove(&created.directory, false)).await.expect("removes the target");
    fixture.worktree.remove(fixture.remove(&source.directory, false)).await.expect("removes the source");
}

#[tokio::test]
async fn requires_force_to_remove_a_dirty_git_worktree() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("dirty");
    let created = fixture
        .worktree
        .create(CreateInput { directory: Some(text(&parent)), name: Some("worktree".to_owned()), ..fixture.create() })
        .await
        .expect("creates the worktree");
    std::fs::write(Path::new(&created.directory).join("dirty.txt"), "dirty").expect("writes the dirty file");

    let error = fixture
        .worktree
        .remove(fixture.remove(&created.directory, false))
        .await
        .expect_err("a dirty worktree needs force");

    assert!(matches!(error, WorktreeError::Git(ref error) if error.force_required));
    assert!(fixture.stored().contains(&(created.directory.clone(), Some("git".to_owned()))));
    assert!(Path::new(&created.directory).join("dirty.txt").exists());

    fixture.worktree.remove(fixture.remove(&created.directory, true)).await.expect("removes with force");
    assert!(!Path::new(&created.directory).exists());
}

#[tokio::test]
async fn reports_unavailable_recorded_strategy_ids() {
    let fixture = WorktreeFixture::new().await;
    fixture.set_strategy(&fixture.canonical_text(), Some("acme/missing"));

    let error = fixture
        .worktree
        .remove(fixture.remove(&fixture.canonical_text(), false))
        .await
        .expect_err("the strategy is unavailable");

    assert!(matches!(error, WorktreeError::StrategyUnavailable { strategy } if strategy == "acme/missing"));
}

#[tokio::test]
async fn preserves_worktrees_whose_stored_strategy_is_unavailable() {
    let fixture = WorktreeFixture::new().await;
    let unavailable = fixture.fixture.path("unavailable");
    std::fs::create_dir_all(&unavailable).expect("creates the directory");
    fixture.insert_row(&text(&unavailable), Some("acme/missing"));

    let error = fixture
        .worktree
        .remove(fixture.remove(&text(&unavailable), false))
        .await
        .expect_err("the recorded strategy is unavailable");

    assert!(matches!(error, WorktreeError::StrategyUnavailable { .. }));
    assert!(fixture.stored().contains(&(text(&unavailable), Some("acme/missing".to_owned()))));
}

#[tokio::test]
async fn adds_a_numeric_suffix_when_the_destination_exists() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("suffix");
    std::fs::create_dir_all(parent.join("worktree")).expect("creates the first destination");
    std::fs::create_dir_all(parent.join("worktree-2")).expect("creates the second destination");

    let created = fixture
        .worktree
        .create(CreateInput {
            from: Some(fixture.canonical_text()),
            directory: Some(text(&parent)),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect("creates a suffixed worktree");

    assert_eq!(created.directory, text(&parent.join("worktree-3")));
    assert!(parent.join("worktree").is_dir());
    assert!(parent.join("worktree-2").is_dir());
    fixture.worktree.remove(fixture.remove(&created.directory, false)).await.expect("removes the worktree");
}

#[tokio::test]
async fn fails_after_ten_destination_conflicts() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("conflicts");
    for index in 0..10 {
        let name = if index == 0 { "worktree".to_owned() } else { format!("worktree-{}", index + 1) };
        std::fs::create_dir_all(parent.join(name)).expect("creates a conflicting destination");
    }

    let error = fixture
        .worktree
        .create(CreateInput {
            from: Some(fixture.canonical_text()),
            directory: Some(text(&parent)),
            name: Some("worktree".to_owned()),
            ..fixture.create()
        })
        .await
        .expect_err("ten conflicts exhaust the suffixes");

    assert!(matches!(
        error,
        WorktreeError::DestinationExists { directory } if directory == text(&parent.join("worktree-10"))
    ));
}

#[tokio::test]
async fn does_not_publish_an_update_when_refresh_finds_no_changes() {
    let fixture = WorktreeFixture::new().await;
    let mut updates = fixture.subscribe().await;

    fixture.worktree.refresh(PROJECT).await.expect("refreshes an unchanged project");

    assert!(
        tokio::time::timeout(Duration::from_millis(200), updates.recv()).await.is_err(),
        "an unchanged refresh publishes nothing"
    );
}

#[tokio::test]
async fn refresh_finds_and_prunes_an_external_git_worktree() {
    let fixture = WorktreeFixture::new().await;
    let target = fixture.fixture.path("external");
    let unchanged = fixture.fixture.path("existing");
    fixture.add_worktree(&target).await;
    fixture.add_worktree(&unchanged).await;
    fixture.insert_row(&text(&target), None);
    fixture.insert_row(&text(&unchanged), Some("git"));
    let mut updates = fixture.subscribe().await;

    fixture.worktree.refresh(PROJECT).await.expect("refreshes the project");

    assert_eq!(
        fixture.stored(),
        vec![
            (text(&unchanged), Some("git".to_owned())),
            (text(&target), Some("git".to_owned())),
            (fixture.canonical_text(), None),
        ]
    );
    assert_eq!(next_update(&mut updates).await, serde_json::json!({ "projectID": PROJECT }));

    fixture.remove_worktree(&target).await;
    fixture.remove_worktree(&unchanged).await;
    fixture.worktree.refresh(PROJECT).await.expect("prunes the removed worktrees");
    assert_eq!(fixture.stored(), vec![(fixture.canonical_text(), None)]);
}

#[tokio::test]
async fn refresh_ignores_stale_git_worktree_registrations() {
    let fixture = WorktreeFixture::new().await;
    let stale = fixture.fixture.path("stale");
    let target = fixture.fixture.path("after-stale");
    fixture.add_worktree(&stale).await;
    std::fs::remove_dir_all(&stale).expect("removes the stale directory");
    fixture.add_worktree(&target).await;

    fixture.worktree.refresh(PROJECT).await.expect("refreshes the project");

    assert_eq!(fixture.stored(), vec![(text(&target), Some("git".to_owned())), (fixture.canonical_text(), None)]);
}

#[tokio::test]
async fn refresh_ignores_directories_that_are_no_longer_git_checkouts() {
    let fixture = WorktreeFixture::new().await;
    std::fs::remove_dir_all(fixture.canonical.join(".git")).expect("removes the git metadata");

    fixture.worktree.refresh(PROJECT).await.expect("refreshes the project");

    assert_eq!(fixture.stored(), vec![(fixture.canonical_text(), None)]);
}

#[tokio::test]
async fn refresh_seeds_the_canonical_checkout_when_inventory_is_empty() {
    let fixture = WorktreeFixture::new().await;
    fixture.delete_row(&fixture.canonical_text());

    fixture.worktree.refresh(PROJECT).await.expect("seeds the inventory");

    assert_eq!(
        fixture.worktree.list(PROJECT).await.expect("lists the worktrees"),
        vec![opencode_vcs::worktree::WorktreeDirectory { directory: fixture.canonical_text(), strategy: None }]
    );
}

#[tokio::test]
async fn refresh_removes_missing_ordinary_checkouts() {
    let fixture = WorktreeFixture::new().await;
    let missing = fixture.fixture.path("missing-checkout");
    fixture.insert_row(&text(&missing), None);

    fixture.worktree.refresh(PROJECT).await.expect("prunes the missing checkout");

    assert!(!fixture.stored().contains(&(text(&missing), None)));
}

#[tokio::test]
async fn defaults_to_git_and_a_configured_directory_without_config() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("configured");
    let registration = fixture
        .strategies
        .transform({
            let parent = text(&parent);
            move |editor| editor.configure(parent.clone())
        })
        .await;

    let created = fixture
        .worktree
        .create(CreateInput { name: Some("configured".to_owned()), ..fixture.create() })
        .await
        .expect("creates in the configured directory");
    assert_eq!(created.directory, text(&parent.join("configured")));
    assert!(fixture.stored().contains(&(created.directory.clone(), Some("git".to_owned()))));

    registration.dispose().await;
    let fallback = fixture
        .worktree
        .create(CreateInput { name: Some("default".to_owned()), ..fixture.create() })
        .await
        .expect("creates in the default directory");
    assert_eq!(fallback.directory, fixture.default_parent("default"));
}

#[tokio::test]
async fn selects_the_last_registration_and_restores_earlier_strategies() {
    let fixture = WorktreeFixture::new().await;
    let parent = fixture.fixture.path("strategies");
    let first = fixture
        .strategies
        .transform({
            let strategy = TestStrategy::alias("first", fixture.fixture.git.clone());
            move |editor| editor.add(strategy.clone())
        })
        .await;
    let second = fixture
        .strategies
        .transform({
            let strategy = TestStrategy::alias("second", fixture.fixture.git.clone());
            move |editor| editor.add(strategy.clone())
        })
        .await;
    fixture
        .strategies
        .transform({
            let parent = text(&parent);
            move |editor| editor.configure(parent.clone())
        })
        .await;

    let created = fixture
        .worktree
        .create(CreateInput { name: Some("second".to_owned()), ..fixture.create() })
        .await
        .expect("creates with the last strategy");
    assert!(fixture.stored().contains(&(created.directory.clone(), Some("second".to_owned()))));

    second.dispose().await;
    let earlier = fixture
        .worktree
        .create(CreateInput { name: Some("first".to_owned()), ..fixture.create() })
        .await
        .expect("creates with the earlier strategy");
    assert!(fixture.stored().contains(&(earlier.directory.clone(), Some("first".to_owned()))));

    first.dispose().await;
    let fallback = fixture
        .worktree
        .create(CreateInput { name: Some("git".to_owned()), ..fixture.create() })
        .await
        .expect("creates with git");
    assert!(fixture.stored().contains(&(fallback.directory.clone(), Some("git".to_owned()))));

    let error = fixture
        .worktree
        .remove(fixture.remove(&created.directory, false))
        .await
        .expect_err("the disposed strategy is unavailable");
    assert!(matches!(error, WorktreeError::StrategyUnavailable { .. }));
    fixture.worktree.refresh(PROJECT).await.expect("refreshes the project");
    assert!(fixture.stored().contains(&(created.directory.clone(), Some("second".to_owned()))));
}

#[tokio::test]
async fn does_not_fall_back_to_git_when_a_registered_strategy_fails() {
    let fixture = WorktreeFixture::new().await;
    let _broken = fixture
        .strategies
        .transform({
            let strategy = TestStrategy::broken_create("broken", fixture.fixture.git.clone(), "backend failed");
            move |editor| editor.add(strategy.clone())
        })
        .await;

    let error = fixture
        .worktree
        .create(CreateInput {
            directory: Some(text(&fixture.fixture.path("failures"))),
            name: Some("failure".to_owned()),
            ..fixture.create()
        })
        .await
        .expect_err("the selected strategy fails");

    assert!(matches!(error, WorktreeError::Operation { ref message, .. } if message.contains("backend failed")));
    assert_eq!(fixture.stored(), vec![(fixture.canonical_text(), None)]);
}

#[tokio::test]
async fn rejects_a_source_override_belonging_to_another_project() {
    let fixture = WorktreeFixture::new().await;
    let other = fixture.fixture.path("other");
    fixture.fixture.init_git(&other).await;
    fixture.fixture.git(&other, &["commit", "--allow-empty", "-m", "root"]).await;

    let error = fixture
        .worktree
        .create(CreateInput { from: Some(text(&other)), name: Some("nope".to_owned()), ..fixture.create() })
        .await
        .expect_err("the source belongs to another project");

    assert!(matches!(
        error,
        WorktreeError::SourceDirectoryNotFound { project_id, .. } if project_id == PROJECT
    ));
}

#[tokio::test]
async fn cannot_remove_a_worktree_belonging_to_another_project() {
    let fixture = WorktreeFixture::new().await;
    let other = fixture.fixture.path("other");
    fixture.fixture.init_git(&other).await;
    fixture.fixture.git(&other, &["commit", "--allow-empty", "-m", "root"]).await;
    let linked = other.join("linked");
    fixture.fixture.git(&other, &["worktree", "add", "--detach", &text(&linked), "HEAD"]).await;
    fixture
        .fixture
        .database
        .read(|connection| project::upsert(connection, "other-project", &text(&other), Some("git")))
        .expect("inserts the other project");
    fixture
        .fixture
        .database
        .read(|connection| storage_worktree::insert_ignore(connection, "other-project", &text(&linked), Some("git")))
        .expect("inserts the other worktree");

    let error = fixture
        .worktree
        .remove(fixture.remove(&text(&linked), true))
        .await
        .expect_err("the worktree belongs to another project");

    assert!(matches!(error, WorktreeError::InvalidDirectory { .. }));
    assert!(linked.is_dir());
}

#[tokio::test]
async fn list_is_independent_of_an_ambient_workspace_location() {
    let fixture = WorktreeFixture::new().await;

    assert_eq!(
        fixture.worktree.list(PROJECT).await.expect("lists the worktrees"),
        vec![opencode_vcs::worktree::WorktreeDirectory { directory: fixture.canonical_text(), strategy: None }]
    );
}

#[tokio::test]
async fn only_refresh_scans_and_prunes() {
    let fixture = WorktreeFixture::new().await;
    let directory = fixture.fixture.path("discovered");
    std::fs::create_dir_all(&directory).expect("creates the discovered directory");
    let (strategy, sources) = TestStrategy::reported("discovered-copy", fixture.fixture.git.clone(), directory.clone());
    let _reported = fixture.strategies.transform(move |editor| editor.add(strategy.clone())).await;

    assert!(!fixture.stored().iter().any(|(stored, _)| stored == &text(&directory)));
    assert!(sources.lock().expect("the sources lock is not poisoned").is_empty());

    fixture.worktree.refresh(PROJECT).await.expect("discovers the directory");
    assert!(fixture.stored().contains(&(text(&directory), Some("discovered-copy".to_owned()))));
    assert_eq!(*sources.lock().expect("the sources lock is not poisoned"), [fixture.canonical_text()]);

    std::fs::remove_dir(&directory).expect("removes the discovered directory");
    assert!(fixture.stored().contains(&(text(&directory), Some("discovered-copy".to_owned()))));
    fixture.worktree.refresh(PROJECT).await.expect("prunes the directory");
    assert!(!fixture.stored().iter().any(|(stored, _)| stored == &text(&directory)));
}

#[tokio::test]
async fn a_failed_strategy_does_not_block_discovery() {
    let fixture = WorktreeFixture::new().await;
    let _broken = fixture
        .strategies
        .transform({
            let strategy = TestStrategy::broken_list("broken-discovery", fixture.fixture.git.clone());
            move |editor| editor.add(strategy.clone())
        })
        .await;
    let target = fixture.fixture.path("external");
    fixture.add_worktree(&target).await;

    fixture.worktree.refresh(PROJECT).await.expect("discovers with the git strategy");

    assert!(fixture.stored().contains(&(text(&target), Some("git".to_owned()))));
}

#[tokio::test]
async fn applies_directory_config_and_restores_defaults() {
    let fixture = WorktreeFixture::new().await;
    let home = fixture.fixture.path("home");
    let config = config_with(&fixture.canonical, &home, "copies").await;
    fixture.strategies.observe_config(&config, &home).await;
    let _custom = fixture
        .strategies
        .transform({
            let strategy = TestStrategy::alias("custom", fixture.fixture.git.clone());
            move |editor| editor.add(strategy.clone())
        })
        .await;

    let first = fixture
        .worktree
        .create(CreateInput { name: Some("one".to_owned()), ..fixture.create() })
        .await
        .expect("creates with the configured directory");
    assert_eq!(first.directory, text(&fixture.canonical.join("copies/one")));
    assert!(fixture.stored().contains(&(first.directory.clone(), Some("custom".to_owned()))));

    std::fs::remove_file(fixture.canonical.join("opencode.json")).expect("removes the project config");
    config.reload().await;
    for _ in 0..200 {
        let settings = fixture.strategies.get().await.expect("reads the strategies");
        if settings.directory == text(&fixture.data.join("worktree/worktr")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let third = fixture
        .worktree
        .create(CreateInput { name: Some("three".to_owned()), ..fixture.create() })
        .await
        .expect("creates with the default directory");
    assert_eq!(third.directory, fixture.default_parent("three"));
}

async fn assert_directory_config(mode: &str) {
    let fixture = WorktreeFixture::new().await;
    let home = fixture.fixture.path("home");
    std::fs::create_dir_all(&home).expect("creates the home directory");
    let directory = match mode {
        "relative" => ".lane/trees".to_owned(),
        "absolute" => text(&home.join("absolute")),
        _ => "~/copies".to_owned(),
    };
    let config = config_with(&fixture.canonical, &home, &directory).await;
    fixture.strategies.observe_config(&config, &home).await;

    let created = fixture
        .worktree
        .create(CreateInput { name: Some("task".to_owned()), ..fixture.create() })
        .await
        .expect("creates with the configured directory");

    let expected = match mode {
        "relative" => fixture.canonical.join(".lane/trees/task"),
        "absolute" => home.join("absolute/task"),
        _ => home.join("copies/task"),
    };
    assert_eq!(created.directory, text(&expected));
}

#[tokio::test]
async fn resolves_relative_directory_config() {
    assert_directory_config("relative").await;
}

#[tokio::test]
async fn resolves_absolute_directory_config() {
    assert_directory_config("absolute").await;
}

#[tokio::test]
async fn resolves_home_directory_config() {
    assert_directory_config("home").await;
}

#[tokio::test]
async fn creates_from_an_explicit_source_clone() {
    let fixture = WorktreeFixture::new().await;
    let home = fixture.fixture.path("home");
    fixture.fixture.git(&fixture.canonical, &["remote", "add", "origin", "git@github.com:owner/repo.git"]).await;
    let clone = fixture.fixture.path("clone");
    fixture
        .fixture
        .git(&fixture.fixture.root.clone(), &["clone", "--no-hardlinks", &fixture.canonical_text(), &text(&clone)])
        .await;
    fixture.insert_row(&text(&clone), None);
    let config = config_with(&fixture.canonical, &home, ".lane/trees").await;
    fixture.strategies.observe_config(&config, &home).await;
    let clone_head = fixture.git_head(&clone).await;

    let created = fixture
        .worktree
        .create(CreateInput { from: Some(text(&clone)), name: Some("selected-clone".to_owned()), ..fixture.create() })
        .await
        .expect("creates from the clone");

    assert_eq!(created.directory, text(&fixture.canonical.join(".lane/trees/selected-clone")));
    assert_eq!(fixture.git_head(Path::new(&created.directory)).await, clone_head);
}
