use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::{self, Decode, decode_json};
use opencode_contract::generated::{
    AbsolutePath, ConfigEntryDocumentInfo, ReferenceGitSource, ReferenceGitSourceType, ReferenceInfo,
    ReferenceLocalSource, ReferenceLocalSourceType, ReferenceSource,
};
use opencode_registry::config;
use opencode_registry::events::RegistryEvents;
use opencode_registry::instructions::Observed;
use opencode_registry::reference::instructions::{self, Summary};
use opencode_registry::reference::{
    EVENT, GitSource, LocalSource, NoRefresh, ReferenceRefresh, ReferenceService, Source,
};
use opencode_registry::repository::{self, Reference};
use serde_json::Value;

use crate::TempDir;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<&'static str>>,
}

impl RegistryEvents for Recorder {
    fn updated(&self, r#type: &'static str) {
        self.events.lock().expect("the event lock is not poisoned").push(r#type);
    }
}

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

#[derive(Default)]
struct RecordingRefresh {
    calls: Mutex<Vec<Vec<GitSource>>>,
}

impl ReferenceRefresh for RecordingRefresh {
    fn refresh(&self, sources: Vec<GitSource>) {
        self.calls.lock().expect("the refresh lock is not poisoned").push(sources);
    }
}

fn service(root: &Path, events: Arc<dyn RegistryEvents>, refresh: Arc<dyn ReferenceRefresh>) -> ReferenceService {
    ReferenceService::new(events, root.join("repos"), refresh)
}

fn local(path: &str) -> Source {
    Source::Local(LocalSource { path: path.to_string(), description: None, hidden: None })
}

fn git(repository: &str, branch: Option<&str>) -> Source {
    Source::Git(GitSource {
        repository: repository.to_string(),
        branch: branch.map(str::to_string),
        description: None,
        hidden: None,
    })
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("the file has a parent")).expect("create the file directory");
    fs::write(path, content).expect("write the file");
}

fn remote(input: &str) -> repository::RemoteReference {
    repository::parse_remote(input).expect("the reference parses")
}

#[test]
fn parses_github_shorthand_and_builds_cache_paths() {
    let reference = remote("owner/repo");
    assert_eq!(reference.host, "github.com");
    assert_eq!(reference.path, "owner/repo");
    assert_eq!(reference.segments, ["owner", "repo"]);
    assert_eq!(reference.owner.as_deref(), Some("owner"));
    assert_eq!(reference.repo, "repo");
    assert_eq!(reference.remote, "https://github.com/owner/repo.git");
    assert_eq!(reference.label, "owner/repo");

    let parsed = Reference::Remote(reference);
    assert_eq!(
        repository::cache_path(Path::new("/cache"), &parsed, None),
        PathBuf::from("/cache/github.com/owner/repo")
    );
    assert_eq!(
        repository::cache_path(Path::new("/cache"), &parsed, Some("main")),
        PathBuf::from("/cache/github.com/owner/repo@main")
    );
    assert_eq!(
        repository::cache_path(Path::new("/cache"), &parsed, Some("feature/x")),
        PathBuf::from("/cache/github.com/owner/repo@feature%2Fx")
    );
    assert_eq!(repository::cache_identity(&parsed), "github.com/owner/repo");
}

#[test]
fn parses_host_paths_and_scp_remotes() {
    let host_path = remote("gitlab.com/group/repo");
    assert_eq!(host_path.host, "gitlab.com");
    assert_eq!(host_path.path, "group/repo");
    assert_eq!(host_path.remote, "https://gitlab.com/group/repo.git");
    assert_eq!(host_path.label, "gitlab.com/group/repo");

    let scp = remote("git@github.com:owner/repo.git");
    assert_eq!(scp.host, "github.com");
    assert_eq!(scp.path, "owner/repo");
    assert_eq!(scp.remote, "git@github.com:owner/repo.git");
    assert_eq!(scp.label, "owner/repo");
}

#[test]
fn caches_a_host_with_a_port_under_a_directory_per_host_part() {
    let reference = remote("ssh://git@example.com:2222/owner/repo");
    assert_eq!(
        repository::cache_path(Path::new("/cache"), &Reference::Remote(reference), None),
        PathBuf::from("/cache/example.com/2222/owner/repo")
    );
}

#[test]
fn keeps_local_file_repositories_distinct_from_remote_repositories() {
    let path = std::env::temp_dir().join("repo.git");
    let url = url::Url::from_file_path(&path).expect("the file URL builds");
    let parsed = repository::parse(url.as_str()).expect("the file reference parses");
    let Reference::File(file) = &parsed else {
        panic!("a file URL parses as a file reference");
    };
    assert_eq!(file.path, path.to_string_lossy());
    assert_eq!(file.label, path.to_string_lossy());
    assert_eq!(file.repo, "repo");
    assert!(!repository::is_remote(&parsed));
    assert!(matches!(repository::parse_remote(url.as_str()), Err(repository::ParseRemoteError::UnsupportedLocal(_))));
}

#[test]
fn rejects_unsafe_references_and_branches() {
    assert!(matches!(repository::parse_remote("not-a-repo"), Err(repository::ParseRemoteError::Invalid(_))));
    assert!(matches!(
        repository::parse_remote("git@github.com:../../../etc/passwd"),
        Err(repository::ParseRemoteError::Invalid(_))
    ));
    assert!(repository::validate_branch("feature/docs.v1").is_ok());
    assert!(repository::validate_branch("-bad").is_err());
    assert!(repository::validate_branch("bad..branch").is_err());
    assert!(repository::validate_branch("bad branch").is_err());

    for input in [
        "..:repo",
        "git@..:repo",
        "../owner/repo",
        "ssh://../repo",
        "ssh://..:22/repo",
        "https://%2e%2e/owner/repo",
        ".:repo",
    ] {
        assert!(
            matches!(repository::parse_remote(input), Err(repository::ParseRemoteError::Invalid(_))),
            "rejects {input}"
        );
    }
}

#[test]
fn compares_cache_identity_independent_of_input_spelling() {
    let shorthand = Reference::Remote(remote("owner/repo"));
    for input in ["https://github.com/owner/repo.git", "github.com/owner/repo", "github:owner/repo"] {
        assert!(repository::same(&shorthand, &Reference::Remote(remote(input))), "matches {input}");
    }
}

#[tokio::test]
async fn lists_local_and_git_sources_and_skips_invalid_ones() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let service = service(&root, Arc::new(NoEvents), Arc::new(NoRefresh));
    let _registration = service
        .transform(|editor| {
            editor.add(
                "docs".to_string(),
                Source::Local(LocalSource {
                    path: "/docs".to_string(),
                    description: Some("Product documentation".to_string()),
                    hidden: None,
                }),
            );
            editor.add(
                "sdk".to_string(),
                Source::Git(GitSource {
                    repository: "owner/repo".to_string(),
                    branch: Some("feature/docs".to_string()),
                    description: Some("SDK documentation".to_string()),
                    hidden: Some(true),
                }),
            );
            editor.add("invalid".to_string(), git("invalid", None));
            editor.add("invalid-branch".to_string(), git("owner/repo", Some("../escape")));
            editor.add("file".to_string(), git("file:///docs", None));
        })
        .await;

    let list = service.list().await.expect("the reference state reads");
    assert_eq!(list.iter().map(|info| info.name.as_str()).collect::<Vec<_>>(), ["docs", "sdk"]);
    assert_eq!(list[0].path, AbsolutePath("/docs".to_string()));
    assert_eq!(list[0].description.as_deref(), Some("Product documentation"));
    assert_eq!(
        list[1].path,
        AbsolutePath(root.join("repos/github.com/owner/repo@feature%2Fdocs").to_string_lossy().into_owned())
    );
    assert_eq!(list[1].hidden, Some(true));
    assert_eq!(
        list[1].source,
        ReferenceSource::Git(ReferenceGitSource {
            r#type: ReferenceGitSourceType::Git,
            repository: "owner/repo".to_string(),
            branch: Some("feature/docs".to_string()),
        })
    );
    assert_eq!(
        list[0].source,
        ReferenceSource::Local(ReferenceLocalSource {
            r#type: ReferenceLocalSourceType::Local,
            path: AbsolutePath("/docs".to_string()),
        })
    );
}

#[tokio::test]
async fn editor_get_list_replace_and_remove_follow_insertion_order() {
    let temp = TempDir::new();
    let service = service(temp.path(), Arc::new(NoEvents), Arc::new(NoRefresh));
    let _registration = service
        .transform(|editor| {
            editor.add(
                "docs".to_string(),
                Source::Local(LocalSource { path: "/docs".into(), description: None, hidden: None }),
            );
            editor.add("sdk".to_string(), git("owner/repo", None));
            assert_eq!(editor.list().len(), 2);
            assert!(matches!(editor.get("docs"), Some(Source::Local(_))));
            assert!(editor.get("missing").is_none());
            editor.add("docs".to_string(), git("owner/repo", None));
            assert!(matches!(editor.get("docs"), Some(Source::Git(_))));
            editor.remove("docs");
            assert!(editor.get("docs").is_none());
            editor.remove("sdk");
        })
        .await;
    assert!(service.list().await.expect("the reference state reads").is_empty());
}

#[tokio::test]
async fn publishes_updates_and_refreshes_valid_git_sources() {
    let temp = TempDir::new();
    let events = Arc::new(Recorder::default());
    let refresh = Arc::new(RecordingRefresh::default());
    let service = service(temp.path(), events.clone(), refresh.clone());
    let _registration = service
        .transform(|editor| {
            editor.add("docs".to_string(), local("/docs"));
            editor.add("sdk".to_string(), git("owner/repo", Some("main")));
            editor.add("invalid".to_string(), git("invalid", None));
        })
        .await;

    assert_eq!(events.events.lock().expect("the event lock is not poisoned").as_slice(), [EVENT]);
    let calls = refresh.calls.lock().expect("the refresh lock is not poisoned");
    assert_eq!(
        calls.as_slice(),
        [[GitSource {
            repository: "owner/repo".to_string(),
            branch: Some("main".to_string()),
            description: None,
            hidden: None,
        }]]
    );
}

#[tokio::test]
async fn config_apply_resolves_paths_and_keeps_the_later_document() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let home = root.join("home");
    let project = root.join("project");
    fs::create_dir_all(&home).expect("create the home directory");
    let document_path = project.join("config/opencode.json");

    let first = document(
        r#"{"references":{"docs":"./docs","shared":"~/shared","absolute":"/abs/path","notes":{"path":"notes","description":"Notes","hidden":true},"sdk":"owner/repo","branch":{"repository":"gitlab.com/group/repo","branch":"feature/x","description":"Branch docs","hidden":true},"bad/alias":"owner/repo","bad alias":"owner/repo","":"owner/repo"}}"#,
    );
    let second = document(r#"{"references":{"docs":{"path":"./later","description":"Later docs"}}}"#);
    let entries = vec![
        Entry::Document(Document { path: Some(document_path), info: first, origin: Origin::File }),
        Entry::Document(Document { path: None, info: second, origin: Origin::File }),
    ];
    let environment = config::reference::ReferenceEnvironment { directory: project.clone(), home: home.clone() };
    let service = service(&root, Arc::new(NoEvents), Arc::new(NoRefresh));
    let _registration = service
        .transform({
            let entries = entries.clone();
            let environment = environment.clone();
            move |editor| config::reference::apply(&entries, &environment, editor)
        })
        .await;

    let list = service.list().await.expect("the reference state reads");
    let names: Vec<&str> = list.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(names, ["docs", "shared", "absolute", "notes", "sdk", "branch"]);
    assert_eq!(list[0].path, AbsolutePath(project.join("later").to_string_lossy().into_owned()));
    assert_eq!(list[0].description.as_deref(), Some("Later docs"));
    assert_eq!(list[1].path, AbsolutePath(home.join("shared").to_string_lossy().into_owned()));
    assert_eq!(list[2].path, AbsolutePath("/abs/path".to_string()));
    assert_eq!(list[3].path, AbsolutePath(project.join("config/notes").to_string_lossy().into_owned()));
    assert_eq!(list[3].hidden, Some(true));
    assert_eq!(list[4].path, AbsolutePath(root.join("repos/github.com/owner/repo").to_string_lossy().into_owned()));
    assert_eq!(
        list[5].path,
        AbsolutePath(root.join("repos/gitlab.com/group/repo@feature%2Fx").to_string_lossy().into_owned())
    );
}

#[test]
fn alias_validation_matches_the_config_reference_plugin() {
    for valid in ["docs", "sdk-1", "a.b", "ümlaut"] {
        assert!(config::reference::valid_alias(valid), "accepts {valid}");
    }
    for invalid in ["", "a/b", "a b", "a\tb", "a,b", "a`b", "a\u{a0}b"] {
        assert!(!config::reference::valid_alias(invalid), "rejects {invalid:?}");
    }
}

#[tokio::test]
async fn register_reloads_config_references_when_the_config_file_changes() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let project = root.join("project");
    let home = root.join("home");
    let config_dir = root.join("global");
    for directory in [&project, &home, &config_dir] {
        fs::create_dir_all(directory).expect("create the workspace directory");
    }
    let config_file = project.join(".opencode/opencode.json");
    write(&config_file, r#"{"references":{"docs":"./docs"}}"#);

    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(config_dir.clone().into_os_string()),
        _ => None,
    })
    .expect("the global paths resolve");
    let options = ConfigOptions {
        project: Some(true),
        global: Some(false),
        system: Some(SystemOption::Disabled),
        claude: Some(false),
        file: None,
        content: None,
    };
    let location = ConfigLocation { directory: project.clone(), project_id: "test".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "local".to_owned(),
        flag_overrides: None,
    };
    let config = ConfigService::start(options, location, paths, deps).await;

    let service = service(&root, Arc::new(NoEvents), Arc::new(NoRefresh));
    let _handle = config::reference::register(
        &config,
        &service,
        config::reference::ReferenceEnvironment { directory: project.clone(), home: home.clone() },
    )
    .await;
    assert_eq!(path(&service).await, project.join(".opencode/docs").to_string_lossy());

    write(&config_file, r#"{"references":{"docs":"./later"}}"#);
    tokio::time::timeout(Duration::from_secs(10), async {
        while path(&service).await != project.join(".opencode/later").to_string_lossy() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the reference reloads");
}

async fn path(service: &ReferenceService) -> String {
    service.list().await.expect("the reference state reads")[0].path.0.clone()
}

#[tokio::test]
async fn guidance_lists_only_described_references_sorted_by_name() {
    let temp = TempDir::new();
    let service = service(temp.path(), Arc::new(NoEvents), Arc::new(NoRefresh));
    let _registration = service
        .transform(|editor| {
            editor.add(
                "zeta".to_string(),
                Source::Local(LocalSource {
                    path: "/zeta".to_string(),
                    description: Some("Last".to_string()),
                    hidden: None,
                }),
            );
            editor.add(
                "alpha".to_string(),
                Source::Local(LocalSource {
                    path: "/alpha".to_string(),
                    description: Some("First".to_string()),
                    hidden: None,
                }),
            );
            editor.add("plain".to_string(), local("/plain"));
        })
        .await;

    let source = instructions::source(service.clone());
    let Observed::Value(value) = source.read().await else {
        panic!("the reference guidance is available");
    };
    let expected = vec![
        Summary { name: "alpha".to_string(), path: "/alpha".to_string(), description: "First".to_string() },
        Summary { name: "zeta".to_string(), path: "/zeta".to_string(), description: "Last".to_string() },
    ];
    assert_eq!(instructions::summaries(&service.list().await.expect("the reference state reads")), expected);
    let text = source.initial(&value).expect("the reference guidance renders");
    assert_eq!(text, instructions::render(&expected));
    assert!(text.contains("<name>alpha</name>"));

    let _cleared = service
        .transform(|editor| {
            editor.remove("alpha");
            editor.remove("zeta");
        })
        .await;
    assert_eq!(source.read().await, Observed::Removed);
}

#[tokio::test]
async fn references_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/references.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let repos = root.join("repos");
    let text = text.replace("{{repos}}", &repos.to_string_lossy());
    let text = text.replace("{{root}}", &root.to_string_lossy());
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert!(!cases.is_empty(), "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        let home = PathBuf::from(case["home"].as_str().expect("case home is a string"));
        let directory = PathBuf::from(case["directory"].as_str().expect("case directory is a string"));
        let mut entries = Vec::new();
        for value in case["documents"].as_array().expect("case documents are an array") {
            let info =
                ConfigEntryDocumentInfo::decode(value, &mut codec::Path::new()).expect("a config document decodes");
            entries.push(Entry::Document(Document { path: None, info, origin: Origin::File }));
        }

        let service = service(&root, Arc::new(NoEvents), Arc::new(NoRefresh));
        let _registration = service
            .transform({
                let environment = config::reference::ReferenceEnvironment { directory, home };
                move |editor| config::reference::apply(&entries, &environment, editor)
            })
            .await;

        let expected: Vec<ReferenceInfo> = case["list"]
            .as_array()
            .expect("the expected list is an array")
            .iter()
            .map(|item| ReferenceInfo::decode(item, &mut codec::Path::new()).expect("a reference decodes"))
            .collect();
        assert_eq!(service.list().await.expect("the reference state reads"), expected, "case {name}: list");

        let source = instructions::source(service.clone());
        let guidance = case["guidance"].as_str().expect("the case guidance is a string");
        if guidance.is_empty() {
            assert_eq!(source.read().await, Observed::Removed, "case {name}: guidance");
            continue;
        }
        let Observed::Value(value) = source.read().await else {
            panic!("case {name}: expected guidance");
        };
        assert_eq!(source.initial(&value).expect("the guidance renders"), guidance, "case {name}: guidance");
    }
}

fn document(json: &str) -> ConfigEntryDocumentInfo {
    decode_json::<ConfigEntryDocumentInfo>(json).expect("the document decodes")
}
