use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_bus::{Bus, BusOptions};
use opencode_contract::generated::{
    FileDiffInfo, FileDiffInfoStatus, VcsBranch, VcsFileStatus, VcsFileStatusStatus, VcsInfo,
};
use opencode_git::{Git, RunOptions};
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_location::project::Vcs as LocationVcs;
use opencode_location::state::Registration;
use opencode_storage::{Database, DatabaseLocation};
use opencode_util::temp::TempDir;
use opencode_vcs::git_provider::GitProvider;
use opencode_vcs::provider::{ProviderDiffInput, ProviderFuture, VcsProvider, VcsScope};
use opencode_vcs::vcs::Vcs;

const REPOSITORY_CONFIG: &str =
    "\n[core]\n\tfsmonitor = false\n[commit]\n\tgpgsign = false\n[user]\n\temail = test@opencode.test\n\tname = Test\n";

pub fn identity_env() -> Vec<(String, String)> {
    [
        ("GIT_AUTHOR_NAME", "Test"),
        ("GIT_AUTHOR_EMAIL", "test@opencode.test"),
        ("GIT_COMMITTER_NAME", "Test"),
        ("GIT_COMMITTER_EMAIL", "test@opencode.test"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect()
}

pub struct Fixture {
    _tmp: TempDir,
    pub root: PathBuf,
    pub git: Arc<Git>,
    pub bus: Arc<Bus>,
    pub database: Arc<Database>,
}

impl Fixture {
    pub fn new() -> Self {
        let tmp = TempDir::new().expect("creates a temporary directory");
        let root = tmp.path().to_path_buf();
        let git = Arc::new(Git::new(Arc::new(|name: &str| std::env::var_os(name))));
        let (database, _) = Database::open(DatabaseLocation::Memory, &root).expect("opens the in-memory database");
        let database = Arc::new(database);
        let bus = Bus::new(database.clone(), BusOptions::default());
        Fixture { _tmp: tmp, root, git, bus, database }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn location(&self, directory: &Path, worktree: &Path, vcs: bool) -> LocationInfo {
        LocationInfo {
            directory: text(directory),
            workspace_id: None,
            project: LocationProject { id: "project".to_owned(), directory: text(worktree), canonical: text(worktree) },
            vcs: vcs.then(|| LocationVcs::Git { store: text(&worktree.join(".git")) }),
        }
    }

    pub async fn git(&self, directory: &Path, args: &[&str]) {
        let options = RunOptions { cwd: Some(text(directory)), env: identity_env(), ..Default::default() };
        let output = self.git.execute(args, options).await.expect("git starts");
        assert_eq!(output.status, 0, "git {} failed: {}", args.join(" "), output.stderr_text());
    }

    pub async fn git_env(&self, directory: &Path, args: &[&str], env: &[(&str, &str)]) {
        let options = RunOptions {
            cwd: Some(text(directory)),
            env: identity_env()
                .into_iter()
                .chain(env.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())))
                .collect(),
            ..Default::default()
        };
        let output = self.git.execute(args, options).await.expect("git starts");
        assert_eq!(output.status, 0, "git {} failed: {}", args.join(" "), output.stderr_text());
    }

    pub async fn init_git(&self, directory: &Path) {
        tokio::fs::create_dir_all(directory).await.expect("creates the project directory");
        self.git(directory, &["init", "-b", "main"]).await;
        let mut config = tokio::fs::OpenOptions::new()
            .append(true)
            .open(directory.join(".git").join("config"))
            .await
            .expect("opens the repository config");
        tokio::io::AsyncWriteExt::write_all(&mut config, REPOSITORY_CONFIG.as_bytes())
            .await
            .expect("appends the repository config");
    }

    pub async fn commit_all(&self, directory: &Path, message: &str) {
        self.git(directory, &["add", "-A"]).await;
        self.git(directory, &["commit", "--no-gpg-sign", "-m", message]).await;
    }

    pub async fn write(&self, path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.expect("creates the parent directory");
        }
        tokio::fs::write(path, content).await.expect("writes the file");
    }

    pub async fn read(&self, path: &Path) -> String {
        tokio::fs::read_to_string(path).await.expect("reads the file").replace("\r\n", "\n")
    }

    pub async fn service(&self, info: &LocationInfo) -> Arc<Vcs> {
        Vcs::new(self.bus.clone(), info)
    }

    pub async fn git_service(&self, directory: &Path, worktree: &Path) -> (Arc<Vcs>, Registration) {
        let info = self.location(directory, worktree, true);
        let vcs = self.service(&info).await;
        let provider = Arc::new(GitProvider::new(self.git.clone(), text(directory), text(worktree)));
        let registration = vcs.transform(move |editor| editor.add(provider.clone())).await;
        (vcs, registration)
    }
}

pub type Factory<T> = Arc<dyn Fn(VcsScope) -> ProviderFuture<T> + Send + Sync>;
pub type DiffFactory = Arc<dyn Fn(ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>> + Send + Sync>;

pub struct TestProvider {
    id: String,
    name: String,
    info: Factory<VcsInfo>,
    base: Option<Factory<Option<opencode_contract::generated::VcsBase>>>,
    branches: Factory<Vec<String>>,
    status: Factory<Vec<VcsFileStatus>>,
    diff: DiffFactory,
    init: Option<Factory<()>>,
}

impl TestProvider {
    pub fn new(id: &str) -> Self {
        TestProvider {
            id: id.to_owned(),
            name: "Custom VCS".to_owned(),
            info: ok(VcsInfo {
                provider: None,
                branch: VcsBranch { current: Some("feature".to_owned()), default: Some("main".to_owned()) },
            }),
            base: None,
            branches: ok(vec!["feature".to_owned(), "main".to_owned()]),
            status: ok(vec![VcsFileStatus {
                file: "file.txt".to_owned(),
                additions: 1.0,
                deletions: 0.0,
                status: VcsFileStatusStatus::Added,
            }]),
            diff: Arc::new(|input| {
                Box::pin(async move {
                    Ok(vec![FileDiffInfo {
                        file: "file.txt".to_owned(),
                        patch: "+hello".to_owned(),
                        additions: 1.0,
                        deletions: 0.0,
                        status: FileDiffInfoStatus::Added,
                        more: input.untracked.map(|_| opencode_contract::generated::FileDiffInfoMore::True),
                    }])
                })
            }),
            init: None,
        }
    }

    pub fn with_info(mut self, info: impl Fn(VcsScope) -> ProviderFuture<VcsInfo> + Send + Sync + 'static) -> Self {
        self.info = Arc::new(info);
        self
    }

    pub fn with_base(
        mut self,
        base: impl Fn(VcsScope) -> ProviderFuture<Option<opencode_contract::generated::VcsBase>> + Send + Sync + 'static,
    ) -> Self {
        self.base = Some(Arc::new(base));
        self
    }

    pub fn with_status(
        mut self,
        status: impl Fn(VcsScope) -> ProviderFuture<Vec<VcsFileStatus>> + Send + Sync + 'static,
    ) -> Self {
        self.status = Arc::new(status);
        self
    }

    pub fn with_diff(
        mut self,
        diff: impl Fn(ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>> + Send + Sync + 'static,
    ) -> Self {
        self.diff = Arc::new(diff);
        self
    }

    pub fn with_init(mut self, init: impl Fn(VcsScope) -> ProviderFuture<()> + Send + Sync + 'static) -> Self {
        self.init = Some(Arc::new(init));
        self
    }
}

impl VcsProvider for TestProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn init(&self, scope: VcsScope) -> Option<ProviderFuture<()>> {
        self.init.as_ref().map(|init| init(scope))
    }

    fn info(&self, scope: VcsScope) -> ProviderFuture<VcsInfo> {
        (self.info)(scope)
    }

    fn base(&self, scope: VcsScope) -> Option<ProviderFuture<Option<opencode_contract::generated::VcsBase>>> {
        self.base.as_ref().map(|base| base(scope))
    }

    fn branches(&self, input: opencode_vcs::provider::BranchesInput) -> ProviderFuture<Vec<String>> {
        (self.branches)(input.scope)
    }

    fn status(&self, scope: VcsScope) -> ProviderFuture<Vec<VcsFileStatus>> {
        (self.status)(scope)
    }

    fn diff(&self, input: ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>> {
        (self.diff)(input)
    }
}

pub fn ok<T: Clone + Send + Sync + 'static>(value: T) -> Factory<T> {
    Arc::new(move |_scope| {
        let value = value.clone();
        Box::pin(async move { Ok(value) })
    })
}

fn text(path: &Path) -> String {
    path.to_str().expect("path is UTF-8").to_owned()
}
