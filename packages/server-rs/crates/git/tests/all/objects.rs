use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use opencode_git::{CaptureInput, Git, Repository, RunOptions};

use crate::fixture;
use crate::{TempDir, text};

#[tokio::test]
async fn packs_loose_objects_and_combines_packs_without_losing_or_delegating_any_object() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let project = temp.path().join("project");
    tokio::fs::create_dir_all(&project).await.expect("creates the project directory");
    fixture::init_repo(&git, &project).await;
    write(&project.join("seed.txt"), "seed\n").await;
    fixture::git(&git, &project, &["add", "."]).await;
    fixture::git(&git, &project, &["commit", "-q", "-m", "initial"]).await;

    let source = git.discover(text(&project)).await.expect("discovers the project");
    let storage = temp.path().join("storage");
    let repository =
        git.create_repository(&source.worktree, text(&storage), Some(&source)).await.expect("creates the store");
    let objects = PathBuf::from(&repository.git_directory).join("objects");
    let alternates = objects.join("info").join("alternates");

    let mut trees = vec![capture(&git, &repository, &source).await];

    for index in 0..2100 {
        write(&project.join(format!("gen/d{}/f{index}.txt", index % 20)), &format!("{index}\n")).await;
    }
    trees.push(capture(&git, &repository, &source).await);

    let before = everything(&git, temp.path(), &repository.git_directory, &alternates).await;
    assert!(loose_oids(&objects).await.len() > 2048);

    let lock = PathBuf::from(&repository.git_directory).join("opencode-pack.lock");

    tokio::fs::write(&lock, "other").await.expect("writes the pack lock");
    git.objects_pack(&repository).await.expect("packs objects");
    assert!(!loose_oids(&objects).await.is_empty());
    assert!(lock.exists(), "the fresh pack lock is left for its owner");
    tokio::fs::remove_file(&lock).await.expect("removes the fresh pack lock");

    tokio::fs::write(&lock, "other").await.expect("writes the stale pack lock");
    let old = SystemTime::now().checked_sub(Duration::from_secs(2 * 60 * 60)).expect("computes the old time");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&lock)
        .expect("opens the stale pack lock")
        .set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old))
        .expect("ages the stale pack lock");
    git.objects_pack(&repository).await.expect("packs objects");
    assert!(loose_oids(&objects).await.is_empty());
    assert!(!lock.exists(), "the stale pack lock is removed");
    assert_eq!(everything(&git, temp.path(), &repository.git_directory, &alternates).await, before);

    let prefix = objects.join("pack").join("pack").to_string_lossy().into_owned();
    for batch in 0..16 {
        write(&project.join(format!("batch-{batch}.txt")), &format!("{batch}\n")).await;
        trees.push(capture(&git, &repository, &source).await);
        let list = loose_oids(&objects).await;
        git_stdin(
            &git,
            temp.path(),
            &["--git-dir", repository.git_directory.as_str(), "pack-objects", "-q", prefix.as_str()],
            &format!("{}\n", list.join("\n")),
        )
        .await;
        fixture::git(&git, temp.path(), &["--git-dir", &repository.git_directory, "prune-packed"]).await;
    }

    assert!(packs(&objects).await.len() >= 16);
    let merged = everything(&git, temp.path(), &repository.git_directory, &alternates).await;

    git.objects_pack(&repository).await.expect("packs objects");
    assert_eq!(packs(&objects).await.len(), 1);
    assert_eq!(everything(&git, temp.path(), &repository.git_directory, &alternates).await, merged);

    let last = trees.last().expect("captures trees");
    let files = git.tree_files(&repository, &trees[0], last).await.expect("lists the changed files");
    assert_eq!(files.len(), 2100 + 16);
}

async fn capture(git: &Git, repository: &Repository, source: &Repository) -> String {
    Box::pin(git.tree_capture(CaptureInput {
        repository: repository.clone(),
        scopes: vec![".".to_string()],
        ignores: Some(source.clone()),
        maximum_untracked_file_bytes: None,
    }))
    .await
    .expect("captures the tree")
}

async fn loose_oids(objects: &Path) -> Vec<String> {
    let mut oids = Vec::new();
    let mut reader = tokio::fs::read_dir(objects).await.expect("reads the object directory");
    while let Some(entry) = reader.next_entry().await.expect("reads the object directory") {
        let prefix = entry.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
            continue;
        }
        let mut inner = tokio::fs::read_dir(entry.path()).await.expect("reads the fanout directory");
        while let Some(file) = inner.next_entry().await.expect("reads the fanout directory") {
            oids.push(format!("{prefix}{}", file.file_name().to_string_lossy()));
        }
    }
    oids
}

async fn packs(objects: &Path) -> Vec<String> {
    let mut reader = tokio::fs::read_dir(objects.join("pack")).await.expect("reads the pack directory");
    let mut names = Vec::new();
    while let Some(entry) = reader.next_entry().await.expect("reads the pack directory") {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".pack") {
            names.push(name);
        }
    }
    names
}

async fn everything(git: &Git, directory: &Path, git_directory: &str, alternates: &Path) -> Vec<String> {
    let borrowed = std::fs::read(alternates).expect("reads the alternates file");
    std::fs::remove_file(alternates).expect("removes the alternates file");
    let listed = fixture::git(
        git,
        directory,
        &["--git-dir", git_directory, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)"],
    )
    .await;
    std::fs::write(alternates, borrowed).expect("restores the alternates file");
    let mut object_ids = listed.split('\n').filter(|line| !line.is_empty()).map(str::to_string).collect::<Vec<_>>();
    object_ids.sort();
    object_ids
}

async fn git_stdin(git: &Git, directory: &Path, args: &[&str], input: &str) -> String {
    let output = git
        .execute(
            args,
            RunOptions {
                cwd: Some(text(directory).to_string()),
                stdin: Some(input.as_bytes().to_vec()),
                env: vec![("GIT_ALLOW_PROTOCOL".to_string(), "file".to_string())],
                ..Default::default()
            },
        )
        .await
        .expect("git starts");
    assert_eq!(output.status, 0, "git {} failed: {}", args.join(" "), output.stderr_text());
    output.text().trim().to_string()
}

async fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.expect("creates the parent directory");
    }
    tokio::fs::write(path, content).await.expect("writes the file");
}
