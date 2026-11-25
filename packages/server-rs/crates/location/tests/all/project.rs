use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::events::LocationEvent;
use opencode_location::project::{
    self, CommandsInput, IconInput, Project, ProjectCommands, ProjectError, ProjectIcon, ProjectService, ProjectTime,
    ResolveOptions, UpdateInput, Vcs,
};
use opencode_storage::project as storage_project;
use opencode_storage::{Database, DatabaseLocation};
use rusqlite::params;
use tokio::sync::broadcast;

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\"/>";

fn temp_root() -> (crate::TempDir, PathBuf) {
    let directory = crate::TempDir::new();
    let root = PathBuf::from(directory.path());
    std::fs::create_dir_all(&root).expect("creates the temporary root");
    (directory, root)
}

fn directory(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("creates the directory");
    path
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn real_path(path: &Path) -> String {
    path_text(&std::fs::canonicalize(path).expect("canonicalizes the path"))
}

fn database() -> Arc<Database> {
    let (database, _) =
        Database::open(DatabaseLocation::Memory, std::env::temp_dir().as_path()).expect("opens the database");
    Arc::new(database)
}

fn git_config() -> &'static Path {
    static CONFIG: OnceLock<PathBuf> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("opencode-location-gitconfig-{}", std::process::id()));
        std::fs::write(&path, b"").expect("writes the empty git config");
        path
    })
}

async fn git(directory: &Path, args: &[&str]) -> String {
    let output = crate::test_git()
        .execute(
            args,
            opencode_git::RunOptions {
                cwd: Some(path_text(directory)),
                env: vec![
                    ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
                    ("GIT_CONFIG_GLOBAL".to_string(), path_text(git_config())),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("runs git");
    assert!(output.status == 0, "git {args:?} failed in {}: {}", directory.display(), output.stderr_text());
    output.text().trim().to_string()
}

async fn init_repo(directory: &Path, commit: bool, remote: Option<&str>) {
    git(directory, &["init"]).await;
    if commit {
        git(
            directory,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@opencode.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "root",
            ],
        )
        .await;
    }
    if let Some(remote) = remote {
        git(directory, &["remote", "add", "origin", remote]).await;
    }
}

async fn root_commit(directory: &Path) -> String {
    git(directory, &["rev-list", "--max-parents=0", "HEAD"]).await
}

fn drain(receiver: &mut broadcast::Receiver<LocationEvent>) -> Vec<LocationEvent> {
    let mut events = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(event) => events.push(event),
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => break,
        }
    }
    events
}

fn set_times(database: &Database, id: &str, created: i64, updated: i64, active: i64) {
    database
        .read(|connection| {
            connection.execute(
                "UPDATE project SET time_created = ?2, time_updated = ?3, time_active = ?4 WHERE id = ?1",
                params![id, created, updated, active],
            )?;
            Ok(())
        })
        .expect("backdates the project row");
}

struct ProjectFixture<'a> {
    id: &'a str,
    worktree: &'a str,
    vcs: Option<&'a str>,
    name: Option<&'a str>,
    icon_url: Option<&'a str>,
    icon_override: Option<&'a str>,
    icon_color: Option<&'a str>,
    commands: Option<&'a str>,
    sandboxes: &'a [&'a str],
    created: i64,
    updated: i64,
    initialized: Option<i64>,
    active: i64,
    archived: Option<i64>,
}

fn insert_project(database: &Database, fixture: ProjectFixture<'_>) {
    let sandboxes = serde_json::to_string(fixture.sandboxes).expect("encodes the sandboxes");
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, vcs, name, icon_url, icon_url_override, icon_color, \
                 time_created, time_updated, time_initialized, time_active, time_archived, sandboxes, commands) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    fixture.id,
                    fixture.worktree,
                    fixture.vcs,
                    fixture.name,
                    fixture.icon_url,
                    fixture.icon_override,
                    fixture.icon_color,
                    fixture.created,
                    fixture.updated,
                    fixture.initialized,
                    fixture.active,
                    fixture.archived,
                    sandboxes,
                    fixture.commands,
                ],
            )?;
            Ok(())
        })
        .expect("inserts the project row");
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("creates the parent directory");
    }
    std::fs::write(path, contents).expect("writes the file");
}

fn svg_url(contents: &str) -> String {
    format!("data:image/svg+xml;base64,{}", STANDARD.encode(contents))
}

#[tokio::test]
async fn markerless_directories_hash_deterministically() {
    let (_temp, root) = temp_root();
    let nested = directory(&root, "notes/drafts");
    let environment = LocalEnvironment::new();
    let service = crate::project_service(database());
    let root_text = path_text(&root);

    let result = service.resolve(&environment, &root_text, ResolveOptions::default()).await.expect("resolves the root");
    let repeated = service
        .resolve(&environment, &format!("{root_text}/."), ResolveOptions::default())
        .await
        .expect("resolves the same directory");
    let child = service
        .resolve(&environment, &path_text(&nested), ResolveOptions::default())
        .await
        .expect("resolves the nested directory");

    let root_real = real_path(&root);
    let nested_real = real_path(&nested);
    assert_eq!(result.id, project::sha1_hex(format!("directory:{root_real}")));
    assert_eq!(child.id, project::sha1_hex(format!("directory:{nested_real}")));
    assert_ne!(result.id, project::GLOBAL_ID);
    assert_ne!(result.id, child.id);
    assert_eq!(repeated.id, result.id);
    assert_eq!(result.directory, root_real);
    assert_eq!(result.canonical, root_real);
    assert_eq!(child.directory, nested_real);
    assert_eq!(child.canonical, nested_real);
    assert_eq!(result.previous, None);
    assert_eq!(result.vcs, None);
}

#[tokio::test]
async fn git_marker_overrides_markerless_directory_projects() {
    let (_temp, root) = temp_root();
    let nested = directory(&root, "packages/app");
    let environment = LocalEnvironment::new();
    let service = crate::project_service(database());
    let nested_text = path_text(&nested);

    let root_project =
        service.resolve(&environment, &path_text(&root), ResolveOptions::default()).await.expect("resolves the root");
    let child_project = service
        .resolve(&environment, &nested_text, ResolveOptions::default())
        .await
        .expect("resolves the nested directory");

    init_repo(&root, true, None).await;
    let repository =
        service.resolve(&environment, &nested_text, ResolveOptions::default()).await.expect("resolves the repository");

    assert_ne!(root_project.id, child_project.id);
    assert_ne!(repository.id, root_project.id);
    assert_ne!(repository.id, child_project.id);
    assert_eq!(repository.directory, real_path(&root));
    assert!(matches!(repository.vcs, Some(Vcs::Git { .. })));
}

#[tokio::test]
async fn normalized_origin_beats_the_root_commit() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, Some("git@github.com:Acme/App.git")).await;
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves the repository");

    assert_eq!(result.id, project::remote_id("git@github.com:Acme/App.git").expect("normalizes the remote"));
    assert_ne!(result.id, root_commit(&root).await);
    assert_eq!(result.directory, real_path(&root));
    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));
}

#[tokio::test]
async fn ssh_and_https_remotes_normalize_to_the_same_id() {
    let (_temp, root) = temp_root();
    let ssh = directory(&root, "ssh");
    let https = directory(&root, "https");
    init_repo(&ssh, true, Some("git@github.com:owner/repo.git")).await;
    init_repo(&https, true, Some("https://github.com/owner/repo.git")).await;
    let service = crate::project_service(database());
    let environment = LocalEnvironment::new();

    let a = service.resolve(&environment, &path_text(&ssh), ResolveOptions::default()).await.expect("resolves ssh");
    let b = service.resolve(&environment, &path_text(&https), ResolveOptions::default()).await.expect("resolves https");

    assert_eq!(a.id, project::remote_id("git@github.com:owner/repo.git").expect("normalizes the remote"));
    assert_eq!(b.id, a.id);
}

#[tokio::test]
async fn file_remotes_are_ignored_and_the_root_commit_is_used() {
    let (_temp, root) = temp_root();
    let remote = format!("file://{}", path_text(&root));
    init_repo(&root, true, Some(&remote)).await;
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves the repository");

    assert_eq!(result.id, root_commit(&root).await);
}

#[tokio::test]
async fn previous_cached_id_is_read_from_the_common_dir() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, Some("git@github.com:owner/repo.git")).await;
    write_file(&root.join(".git/opencode"), "old-id");
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves the repository");

    assert_eq!(result.previous, Some("old-id".to_string()));
    assert_eq!(result.id, project::remote_id("git@github.com:owner/repo.git").expect("normalizes the remote"));
}

#[tokio::test]
async fn resolving_never_writes_the_git_cache() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, Some("git@github.com:owner/repo.git")).await;
    let service = crate::project_service(database());

    service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves the repository");

    assert!(!root.join(".git/opencode").exists(), "resolving never writes the cached id");
}

#[tokio::test]
async fn git_marker_inside_an_outer_mercurial_repository_is_preferred() {
    let (_temp, root) = temp_root();
    std::fs::create_dir_all(root.join(".hg")).expect("creates the outer mercurial marker");
    let nested = directory(&root, "nested");
    init_repo(&nested, true, None).await;
    let app = directory(&nested, "app");
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&app), ResolveOptions::default())
        .await
        .expect("resolves the nested repository");

    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));
    assert_eq!(result.directory, real_path(&nested));
}

#[tokio::test]
async fn mercurial_marker_inside_an_outer_git_repository_is_preferred() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, None).await;
    let nested = directory(&root, "nested");
    std::fs::create_dir_all(nested.join(".hg")).expect("creates the nested mercurial marker");
    let app = directory(&nested, "app");
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&app), ResolveOptions::default())
        .await
        .expect("resolves the nested directory");

    assert!(matches!(result.vcs, Some(Vcs::Hg { .. })));
    assert_eq!(result.directory, real_path(&nested));
}

#[tokio::test]
async fn git_metadata_wins_when_both_markers_share_a_directory() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, None).await;
    std::fs::create_dir_all(root.join(".hg")).expect("creates the mercurial marker");
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves");

    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));
}

#[tokio::test]
async fn empty_mercurial_metadata_resolves_as_hg_with_the_global_id() {
    let (_temp, root) = temp_root();
    std::fs::create_dir_all(root.join(".hg")).expect("creates the mercurial marker");
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves");

    assert!(matches!(result.vcs, Some(Vcs::Hg { .. })));
    assert_eq!(result.id, project::GLOBAL_ID);
}

#[tokio::test]
async fn linked_worktree_resolves_to_the_main_repository() {
    let (_temp, root) = temp_root();
    let main = directory(&root, "repo");
    init_repo(&main, true, Some("git@github.com:owner/repo.git")).await;
    write_file(&main.join(".git/opencode"), "old-id");
    let linked = root.join("linked");
    git(&main, &["worktree", "add", &path_text(&linked), "-b", "linked"]).await;
    let service = crate::project_service(database());
    let mut receiver = service.subscribe();

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&linked), ResolveOptions::default())
        .await
        .expect("resolves the linked worktree");

    let id = project::remote_id("git@github.com:owner/repo.git").expect("normalizes the remote");
    let linked_real = real_path(&linked);
    assert_eq!(result.id, id);
    assert_eq!(result.directory, linked_real);
    assert_eq!(result.canonical, real_path(&main));
    assert_eq!(result.previous, Some("old-id".to_string()));
    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));

    let events = drain(&mut receiver);
    let (project_id, previous, adopted) = events
        .into_iter()
        .find_map(|event| match event {
            LocationEvent::WorktreeResolved { project_id, directory, previous, adopted }
                if directory == linked_real =>
            {
                Some((project_id, previous, adopted))
            }
            _ => None,
        })
        .expect("publishes the linked worktree");
    assert_eq!(project_id, id);
    assert_eq!(previous, "old-id");
    assert!(adopted.is_empty(), "a fresh worktree adopts nothing");
}

#[tokio::test]
async fn list_orders_projects_by_recent_activity() {
    let database = database();
    let service = crate::project_service(database.clone());
    insert_project(
        &database,
        ProjectFixture {
            id: "older",
            worktree: "/older",
            vcs: Some("git"),
            name: Some("Older"),
            icon_url: None,
            icon_override: Some("data:image/png;base64,test"),
            icon_color: Some("#000000"),
            commands: Some(r#"{"start":"bun dev"}"#),
            sandboxes: &["/older/sandbox"],
            created: 1,
            updated: 1,
            initialized: None,
            active: 4,
            archived: None,
        },
    );
    insert_project(
        &database,
        ProjectFixture {
            id: "newer",
            worktree: "/newer",
            vcs: None,
            name: None,
            icon_url: None,
            icon_override: None,
            icon_color: None,
            commands: None,
            sandboxes: &[],
            created: 2,
            updated: 2,
            initialized: Some(3),
            active: 3,
            archived: None,
        },
    );
    insert_project(
        &database,
        ProjectFixture {
            id: "archived",
            worktree: "/archived",
            vcs: None,
            name: None,
            icon_url: None,
            icon_override: None,
            icon_color: None,
            commands: None,
            sandboxes: &[],
            created: 1,
            updated: 1,
            initialized: None,
            active: 1,
            archived: Some(5),
        },
    );

    let older = Project {
        id: "older".to_string(),
        canonical: "/older".to_string(),
        vcs: Some("git".to_string()),
        name: Some("Older".to_string()),
        icon: Some(ProjectIcon {
            url: None,
            r#override: Some("data:image/png;base64,test".to_string()),
            color: Some("#000000".to_string()),
        }),
        commands: Some(ProjectCommands { start: Some("bun dev".to_string()) }),
        time: ProjectTime { created: 1, updated: 1, active: 4, archived: None },
        sandboxes: vec!["/older/sandbox".to_string()],
    };
    let newer = Project {
        id: "newer".to_string(),
        canonical: "/newer".to_string(),
        vcs: None,
        name: None,
        icon: None,
        commands: None,
        time: ProjectTime { created: 2, updated: 2, active: 3, archived: None },
        sandboxes: Vec::new(),
    };
    let archived = Project {
        id: "archived".to_string(),
        canonical: "/archived".to_string(),
        vcs: None,
        name: None,
        icon: None,
        commands: None,
        time: ProjectTime { created: 1, updated: 1, active: 1, archived: Some(5) },
        sandboxes: Vec::new(),
    };

    let active = service.list(false).await.expect("lists the active projects");
    assert_eq!(active, vec![older.clone(), newer.clone()]);
    let all = service.list(true).await.expect("lists every project");
    assert_eq!(all, vec![older, newer, archived]);
}

#[tokio::test]
async fn update_sets_and_clears_project_metadata() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "update");
    let database = database();
    database
        .read(|connection| storage_project::upsert(connection, "update", &path_text(&project_dir), None))
        .expect("inserts the project");
    set_times(&database, "update", 1, 1, 1);
    let service = crate::project_service(database);

    let updated = service
        .update(UpdateInput {
            project_id: "update".to_string(),
            name: Some("Updated".to_string()),
            icon: Some(IconInput {
                r#override: Some("data:image/png;base64,test".to_string()),
                color: Some("blue".to_string()),
            }),
            commands: Some(CommandsInput { start: Some("bun install".to_string()) }),
            ..Default::default()
        })
        .await
        .expect("updates the project");
    assert_eq!(updated.id, "update");
    assert_eq!(updated.name.as_deref(), Some("Updated"));
    assert_eq!(
        updated.icon,
        Some(ProjectIcon {
            url: None,
            r#override: Some("data:image/png;base64,test".to_string()),
            color: Some("blue".to_string()),
        })
    );
    assert_eq!(updated.commands, Some(ProjectCommands { start: Some("bun install".to_string()) }));
    assert_eq!(updated.canonical, path_text(&project_dir));
    assert_eq!(updated.time.created, 1);
    assert_eq!(updated.time.active, 1);
    assert!(updated.time.updated > 1, "the update records a fresh time_updated");

    let mut receiver = service.subscribe();
    let cleared = service
        .update(UpdateInput {
            project_id: "update".to_string(),
            name: Some(String::new()),
            icon: Some(IconInput { r#override: Some(String::new()), color: Some(String::new()) }),
            commands: Some(CommandsInput { start: Some(String::new()) }),
            ..Default::default()
        })
        .await
        .expect("clears the project metadata");
    assert_eq!(cleared.name, None);
    assert_eq!(cleared.icon, None);
    assert_eq!(cleared.commands, None);
    assert_eq!(drain(&mut receiver), vec![LocationEvent::ProjectUpdated(cleared.clone())]);

    let listed = service.list(false).await.expect("lists the project");
    assert_eq!(
        listed,
        vec![Project {
            id: "update".to_string(),
            canonical: path_text(&project_dir),
            vcs: None,
            name: None,
            icon: None,
            commands: None,
            time: ProjectTime { created: 1, updated: cleared.time.updated, active: 1, archived: None },
            sandboxes: Vec::new(),
        }]
    );

    let error = service
        .update(UpdateInput {
            project_id: "missing".to_string(),
            name: Some("Updated".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("fails for an unknown project");
    assert!(matches!(error, ProjectError::NotFound(id) if id == "missing"));
}

#[tokio::test]
async fn activate_records_activity_and_throttles_repeats() {
    let database = database();
    database
        .read(|connection| storage_project::upsert(connection, "activate", "/activate", None))
        .expect("inserts the project");
    set_times(&database, "activate", 1, 1, 1);
    let clock = Arc::new(AtomicI64::new(100_000));
    let service = ProjectService::with_now(
        database,
        crate::test_git(),
        Arc::new({
            let clock = clock.clone();
            move || clock.load(Ordering::Relaxed)
        }),
    );

    service.activate("activate").await.expect("records the activity");
    let first = service.list(false).await.expect("lists the project");
    assert_eq!(first[0].time.updated, 1, "activation leaves time_updated untouched");
    assert_eq!(first[0].time.active, 100_000);

    service.activate("activate").await.expect("throttles the repeat");
    assert_eq!(service.list(false).await.expect("lists the project")[0].time.active, 100_000);

    clock.fetch_add(project::ACTIVATE_INTERVAL_MS - 1, Ordering::Relaxed);
    service.activate("activate").await.expect("still throttles inside the interval");
    assert_eq!(service.list(false).await.expect("lists the project")[0].time.active, 100_000);

    clock.fetch_add(1, Ordering::Relaxed);
    service.activate("activate").await.expect("records once the interval elapses");
    let recorded = service.list(false).await.expect("lists the project");
    assert_eq!(recorded[0].time.updated, 1, "activation still leaves time_updated untouched");
    assert_eq!(recorded[0].time.active, 100_000 + project::ACTIVATE_INTERVAL_MS);

    service.activate("missing").await.expect("silently ignores an unknown project");
    assert_eq!(service.list(false).await.expect("lists the project").len(), 1);
}

#[tokio::test]
async fn sweep_archives_idle_missing_projects_and_resolve_unarchives_them() {
    let (_temp, root) = temp_root();
    let kept = directory(&root, "kept");
    let removed = directory(&root, "removed");
    let database = database();
    let now = 10_000_000;
    let clock = Arc::new(AtomicI64::new(now));
    let service = ProjectService::with_now(
        database.clone(),
        crate::test_git(),
        Arc::new({
            let clock = clock.clone();
            move || clock.load(Ordering::Relaxed)
        }),
    );
    let environment = LocalEnvironment::new();

    let kept_project = service
        .resolve(&environment, &path_text(&kept), ResolveOptions::default())
        .await
        .expect("resolves the kept directory");
    let removed_project = service
        .resolve(&environment, &path_text(&removed), ResolveOptions::default())
        .await
        .expect("resolves the removed directory");
    service
        .update(UpdateInput {
            project_id: removed_project.id.clone(),
            name: Some("Removed".to_string()),
            ..Default::default()
        })
        .await
        .expect("names the removed project");

    let missing_global = root.join("missing-global");
    let missing_updated = root.join("missing-updated");
    let missing_active = root.join("missing-active");
    for (id, path) in [
        (project::GLOBAL_ID, path_text(&missing_global)),
        ("recent-updated", path_text(&missing_updated)),
        ("recent-active", path_text(&missing_active)),
    ] {
        database
            .read(|connection| storage_project::upsert(connection, id, &path, None))
            .expect("inserts the missing project");
    }
    set_times(&database, project::GLOBAL_ID, 1, 1, 1);
    set_times(&database, &kept_project.id, 1, 1, 1);
    set_times(&database, &removed_project.id, 1, 1, 1);
    set_times(&database, "recent-updated", 1, now - 1_000, 1);
    set_times(&database, "recent-active", 1, 1, now - 1_000);

    let mut receiver = service.subscribe();
    std::fs::remove_dir_all(&removed).expect("removes the project directory");
    service.sweep(&environment).await.expect("sweeps the projects");
    service.sweep(&environment).await.expect("sweeps again");
    let events = drain(&mut receiver);

    let active_ids: Vec<String> =
        service.list(false).await.expect("lists the projects").into_iter().map(|project| project.id).collect();
    assert!(active_ids.contains(&kept_project.id), "an existing directory is kept");
    assert!(active_ids.contains(&project::GLOBAL_ID.to_string()), "the global project is never archived");
    assert!(active_ids.contains(&"recent-updated".to_string()), "a recently updated project is skipped");
    assert!(active_ids.contains(&"recent-active".to_string()), "a recently active project is skipped");
    assert!(!active_ids.contains(&removed_project.id), "the missing idle project is archived");

    let archived = service
        .list(true)
        .await
        .expect("lists every project")
        .into_iter()
        .find(|project| project.id == removed_project.id)
        .expect("the removed project is archived");
    assert_eq!(archived.time.archived, Some(now));
    assert_eq!(archived.time.updated, 1);
    assert_eq!(archived.name.as_deref(), Some("Removed"));
    assert_eq!(events, vec![LocationEvent::ProjectUpdated(archived)]);

    std::fs::create_dir_all(&removed).expect("recreates the project directory");
    let mut receiver = service.subscribe();
    service
        .resolve(&environment, &path_text(&removed), ResolveOptions::default())
        .await
        .expect("resolves the recreated directory again");
    service
        .resolve(&environment, &path_text(&removed), ResolveOptions::default())
        .await
        .expect("resolves the recreated directory twice");
    let events = drain(&mut receiver);

    let restored = service
        .list(false)
        .await
        .expect("lists the projects")
        .into_iter()
        .find(|project| project.id == removed_project.id)
        .expect("the removed project is restored");
    assert_eq!(restored.name.as_deref(), Some("Removed"));
    assert_eq!(restored.time.updated, 1, "unarchiving leaves time_updated untouched");
    assert_eq!(restored.time.archived, None);
    assert_eq!(events, vec![LocationEvent::ProjectUpdated(restored)]);
}

#[tokio::test]
async fn git_discovery_is_cached_until_metadata_changes() {
    let (_temp, root) = temp_root();
    let nested = directory(&root, "packages/app");
    init_repo(&root, true, None).await;
    let root_id = root_commit(&root).await;
    let environment = LocalEnvironment::new();
    let git = crate::test_git();
    let service = ProjectService::new(database(), git.clone());
    let nested_text = path_text(&nested);

    let first = service
        .resolve(&environment, &nested_text, ResolveOptions::default())
        .await
        .expect("resolves the nested directory");
    assert_eq!(first.id, root_id);
    let after_first = git.executions();
    assert!(after_first > 0, "the first discovery spawns git");

    let again = service
        .resolve(&environment, &nested_text, ResolveOptions::default())
        .await
        .expect("resolves the nested directory again");
    let at_root =
        service.resolve(&environment, &path_text(&root), ResolveOptions::default()).await.expect("resolves the root");
    assert_eq!(again.id, root_id);
    assert_eq!(at_root.id, root_id);
    assert_eq!(git.executions(), after_first, "a cached discovery spawns nothing");

    self::git(&root, &["remote", "add", "origin", "git@github.com:owner/repo.git"]).await;
    let remote = service
        .resolve(&environment, &nested_text, ResolveOptions::default())
        .await
        .expect("resolves the changed repository");
    assert_eq!(remote.id, project::remote_id("git@github.com:owner/repo.git").expect("normalizes the remote"));
    assert!(git.executions() > after_first, "changed git metadata invalidates the cache");
}

#[tokio::test]
async fn icon_discovery_finds_public_favicon() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    write_file(&project_dir.join("public/favicon.svg"), SVG);

    let icon = project::icon::discover(&path_text(&project_dir)).await.expect("discovers the favicon");

    assert_eq!(icon.path, "public/favicon.svg");
    assert_eq!(icon.url, svg_url(SVG));
}

#[tokio::test]
async fn icon_discovery_prefers_ranked_appiconset_files() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    let ranked = format!("{SVG} ");
    write_file(&project_dir.join("Assets.xcassets/AppIcon.appiconset/000.png"), "other");
    write_file(&project_dir.join("Assets.xcassets/AppIcon.appiconset/icon256.svg"), &ranked);

    let icon = project::icon::discover(&path_text(&project_dir)).await.expect("discovers the appiconset");

    assert_eq!(icon.path, "Assets.xcassets/AppIcon.appiconset/icon256.svg");
    assert_eq!(icon.url, svg_url(&ranked));
}

#[tokio::test]
async fn icon_discovery_skips_oversized_files() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    write_file(&project_dir.join("assets/app-icon.svg"), "x".repeat(64 * 1024 + 1));
    write_file(&project_dir.join("public/favicon.svg"), SVG);

    let icon = project::icon::discover(&path_text(&project_dir)).await.expect("falls back to the favicon");

    assert_eq!(icon.path, "public/favicon.svg");
    assert_eq!(icon.url, svg_url(SVG));
}

#[cfg(unix)]
#[tokio::test]
async fn icon_discovery_skips_escaping_symlinks() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    let outside = root.join("outside.svg");
    write_file(&outside, "outside");
    std::fs::create_dir_all(project_dir.join("assets")).expect("creates the assets directory");
    std::os::unix::fs::symlink(&outside, project_dir.join("assets/app-icon.svg"))
        .expect("creates the escaping symlink");
    write_file(&project_dir.join("public/favicon.svg"), SVG);

    let icon = project::icon::discover(&path_text(&project_dir)).await.expect("falls back to the favicon");

    assert_eq!(icon.path, "public/favicon.svg");
    assert_eq!(icon.url, svg_url(SVG));
}

#[tokio::test]
async fn icon_discovery_renders_composer_backgrounds() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    write_file(&project_dir.join("AppIcon.icon/Assets/logo.svg"), SVG);
    let manifest = project_dir.join("AppIcon.icon/icon.json");
    for fill in [
        serde_json::json!({ "solid": "display-p3:0.075,0.063,0.063,1" }),
        serde_json::json!({ "linear-gradient": ["gray:0.2,1", "srgb:0,0,0,1"] }),
    ] {
        let config = serde_json::json!({
            "fill": fill,
            "groups": [{ "layers": [{ "image-name": "logo.svg" }] }],
        });
        write_file(&manifest, config.to_string());

        let icon = project::icon::discover(&path_text(&project_dir)).await.expect("renders the composer icon");
        assert_eq!(icon.path, "AppIcon.icon/Assets/logo.svg");
        assert!(icon.url.len() <= 64 * 1024, "the composed url stays within the limit");
        let encoded = icon.url.strip_prefix("data:image/svg+xml;base64,").expect("composes an svg data url");
        let markup = String::from_utf8(STANDARD.decode(encoded).expect("decodes the composed svg"))
            .expect("the composed svg is utf-8");
        assert!(markup.contains("<rect"), "the background is a rect: {markup}");
        assert!(
            markup.contains("<image width=\"128\" height=\"128\" href=\"data:image/svg+xml;base64,"),
            "the icon is embedded inertly: {markup}"
        );
    }
}

#[tokio::test]
async fn icon_discovery_rejects_malformed_composer_manifests() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    write_file(&project_dir.join("AppIcon.icon/Assets/logo.svg"), SVG);
    let manifest = project_dir.join("AppIcon.icon/icon.json");
    let missing = serde_json::json!({ "groups": [{ "layers": [{ "image-name": "missing.svg" }] }] });
    for content in ["{".to_string(), "x".repeat(64 * 1024 + 1), missing.to_string()] {
        write_file(&manifest, content);
        assert!(
            project::icon::discover(&path_text(&project_dir)).await.is_none(),
            "a malformed, oversized or missing-layer manifest falls through"
        );
    }
}

#[tokio::test]
async fn icon_refresh_persists_changed_icons_and_preserves_overrides() {
    let (_temp, root) = temp_root();
    let project_dir = directory(&root, "project");
    write_file(&project_dir.join("favicon.svg"), SVG);
    let database = database();
    database
        .read(|connection| storage_project::upsert(connection, "icons", &path_text(&project_dir), None))
        .expect("inserts the project");
    let expected = svg_url(SVG);

    let first = project::icon::refresh(&database, "icons").await.expect("refreshes the icon").expect("writes the icon");
    assert_eq!(first.icon_url.as_deref(), Some(expected.as_str()));
    assert!(
        project::icon::refresh(&database, "icons").await.expect("checks the unchanged icon").is_none(),
        "an unchanged icon is not rewritten"
    );

    let resolved_dir = directory(&root, "resolved");
    write_file(&resolved_dir.join("favicon.svg"), SVG);
    let service = crate::project_service(database.clone());
    let mut receiver = service.subscribe();
    let resolved = service
        .resolve(&LocalEnvironment::new(), &path_text(&resolved_dir), ResolveOptions::default())
        .await
        .expect("resolves the markerless directory");
    let published = drain(&mut receiver)
        .into_iter()
        .find_map(|event| match event {
            LocationEvent::ProjectUpdated(project) if project.id == resolved.id => Some(project),
            _ => None,
        })
        .expect("publishes the resolved icon");
    assert_eq!(published.icon.and_then(|icon| icon.url).as_deref(), Some(expected.as_str()));

    service
        .update(UpdateInput {
            project_id: resolved.id.clone(),
            icon: Some(IconInput { r#override: Some("my-icon".to_string()), color: Some("blue".to_string()) }),
            ..Default::default()
        })
        .await
        .expect("sets the icon override");
    assert!(
        project::icon::refresh(&database, &resolved.id).await.expect("checks the unchanged icon").is_none(),
        "an unchanged icon is not rewritten"
    );

    let changed_content = format!("{SVG} ");
    write_file(&resolved_dir.join("favicon.svg"), &changed_content);
    let changed = project::icon::refresh(&database, &resolved.id)
        .await
        .expect("refreshes the changed icon")
        .expect("writes the changed icon");
    assert_eq!(changed.icon_url.as_deref(), Some(svg_url(&changed_content).as_str()));
    assert_eq!(changed.icon_url_override.as_deref(), Some("my-icon"));
    assert_eq!(changed.icon_color.as_deref(), Some("blue"));
    assert!(
        project::icon::refresh(&database, &resolved.id).await.expect("checks the changed icon").is_none(),
        "a written icon is not rewritten"
    );
}

#[tokio::test]
async fn corrupt_cached_id_falls_back_to_the_root_commit() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, None).await;
    write_file(&root.join(".git/opencode"), "\u{0}".repeat(40));
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves the repository despite the corrupt cache");

    assert_eq!(result.previous, None);
    assert_eq!(result.id, root_commit(&root).await);
    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));
}

#[tokio::test]
async fn empty_git_repository_resolves_to_the_global_id() {
    let (_temp, root) = temp_root();
    init_repo(&root, false, None).await;
    let service = crate::project_service(database());

    let result = service
        .resolve(&LocalEnvironment::new(), &path_text(&root), ResolveOptions::default())
        .await
        .expect("resolves");

    let root_real = real_path(&root);
    assert_eq!(result.id, project::GLOBAL_ID);
    assert_eq!(result.directory, root_real);
    assert_eq!(result.canonical, root_real);
    assert!(matches!(result.vcs, Some(Vcs::Git { .. })));
    assert_eq!(result.previous, None);
}

#[tokio::test]
async fn renamed_canonical_preserves_metadata_and_publishes_once() {
    let (_temp, root) = temp_root();
    let before = directory(&root, "before");
    let after = root.join("after");
    init_repo(&before, true, Some("git@github.com:owner/repo.git")).await;
    let service = crate::project_service(database());
    let environment = LocalEnvironment::new();

    let initial = service
        .resolve(&environment, &path_text(&before), ResolveOptions::default())
        .await
        .expect("resolves the original directory");
    service
        .update(UpdateInput {
            project_id: initial.id.clone(),
            name: Some("Preserved name".to_string()),
            ..Default::default()
        })
        .await
        .expect("names the project");

    let mut receiver = service.subscribe();
    std::fs::rename(&before, &after).expect("renames the repository");
    let renamed = service
        .resolve(&environment, &path_text(&after), ResolveOptions::default())
        .await
        .expect("resolves the renamed directory");
    let updates: Vec<Project> = drain(&mut receiver)
        .into_iter()
        .filter_map(|event| match event {
            LocationEvent::ProjectUpdated(project) if project.id == initial.id => Some(project),
            _ => None,
        })
        .collect();

    let after_real = real_path(&after);
    assert_eq!(renamed.id, initial.id);
    assert_eq!(renamed.directory, after_real);
    assert_eq!(renamed.canonical, after_real);
    let stored = service
        .list(false)
        .await
        .expect("lists the project")
        .into_iter()
        .find(|project| project.id == initial.id)
        .expect("the renamed project is stored");
    assert_eq!(stored.canonical, after_real);
    assert_eq!(stored.name.as_deref(), Some("Preserved name"));
    assert_eq!(updates.len(), 1, "the rename publishes one project update");
    assert_eq!(updates[0].canonical, after_real);
    assert_eq!(updates[0].name.as_deref(), Some("Preserved name"));
}

#[tokio::test]
async fn another_clone_keeps_the_existing_canonical() {
    let (_temp, root) = temp_root();
    let main = directory(&root, "repo");
    let clone = root.join("clone");
    init_repo(&main, true, Some("git@github.com:owner/repo.git")).await;
    git(&root, &["clone", "--no-hardlinks", &path_text(&main), &path_text(&clone)]).await;
    git(&clone, &["remote", "set-url", "origin", "https://github.com/owner/repo.git"]).await;
    let service = crate::project_service(database());
    let environment = LocalEnvironment::new();
    let expected = project::remote_id("git@github.com:owner/repo.git").expect("normalizes the remote");

    let opened = service
        .resolve(&environment, &path_text(&main), ResolveOptions::default())
        .await
        .expect("resolves the main repository");
    assert_eq!(opened.id, expected);
    let mut receiver = service.subscribe();
    let cloned =
        service.resolve(&environment, &path_text(&clone), ResolveOptions::default()).await.expect("resolves the clone");
    let updates =
        drain(&mut receiver).into_iter().filter(|event| matches!(event, LocationEvent::ProjectUpdated(_))).count();

    let main_real = real_path(&main);
    assert_eq!(cloned.id, expected);
    assert_eq!(cloned.directory, real_path(&clone));
    assert_eq!(cloned.canonical, real_path(&clone), "a clone resolves as its own main checkout");
    let stored = service
        .list(false)
        .await
        .expect("lists the project")
        .into_iter()
        .find(|project| project.id == expected)
        .expect("the shared project is stored");
    assert_eq!(stored.canonical, main_real);
    assert_eq!(updates, 0, "another clone does not publish a project update");
}

#[tokio::test]
async fn repeated_resolutions_do_not_publish_project_updates() {
    let (_temp, root) = temp_root();
    init_repo(&root, true, Some("git@github.com:owner/repo.git")).await;
    let service = crate::project_service(database());
    let environment = LocalEnvironment::new();
    let mut receiver = service.subscribe();

    service.resolve(&environment, &path_text(&root), ResolveOptions::default()).await.expect("resolves the repository");
    service.resolve(&environment, &path_text(&root), ResolveOptions::default()).await.expect("resolves it again");
    let updates =
        drain(&mut receiver).into_iter().filter(|event| matches!(event, LocationEvent::ProjectUpdated(_))).count();

    assert_eq!(updates, 0, "resolving never publishes project updates");
}
