use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use opencode_contract::generated::requests::{VcsMode, VcsUntracked};
use opencode_contract::generated::{
    FileDiffInfo, FileDiffInfoMore, FileDiffInfoStatus, VcsBaseSource, VcsFileStatus, VcsFileStatusStatus,
};
use opencode_git::RunOptions;
use opencode_vcs::provider::BranchOptions;
use opencode_vcs::vcs::{DiffOptions, Vcs};
use tokio::io::AsyncWriteExt;

use crate::support::Fixture;

#[tokio::test]
async fn lists_local_branches_by_recent_activity() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "one\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "z-recent"]).await;
    fixture.write(&project.join("file.txt"), "two\n").await;
    fixture.git(&project, &["add", "-A"]).await;
    fixture
        .git_env(
            &project,
            &["commit", "--no-gpg-sign", "-m", "recent"],
            &[("GIT_AUTHOR_DATE", "2030-01-01T00:00:00Z"), ("GIT_COMMITTER_DATE", "2030-01-01T00:00:00Z")],
        )
        .await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert_eq!(vcs.branches(BranchOptions::default()).await, ["z-recent", "main"]);
    assert_eq!(vcs.branches(BranchOptions { limit: Some(1), ..BranchOptions::default() }).await, ["z-recent"]);
    assert_eq!(vcs.branches(BranchOptions { search: Some("MAIN".to_owned()), limit: Some(1) }).await, ["main"]);
    assert!(vcs.branches(BranchOptions { search: Some("*".to_owned()), ..BranchOptions::default() }).await.is_empty());
}

#[tokio::test]
async fn lists_remote_branches_without_remote_head_symrefs() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    let path = text(&project);
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "one\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["tag", "main"]).await;
    fixture.git(&project, &["remote", "add", "origin", path.as_str()]).await;
    fixture.git(&project, &["remote", "add", "fork", path.as_str()]).await;
    fixture.git(&project, &["fetch", "--quiet", "origin"]).await;
    fixture.git(&project, &["fetch", "--quiet", "fork"]).await;
    fixture.git(&project, &["remote", "set-head", "origin", "main"]).await;
    fixture.git(&project, &["remote", "set-head", "fork", "main"]).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert_eq!(vcs.branches(BranchOptions::default()).await, ["main", "fork/main", "origin/main"]);
    assert_eq!(vcs.branches(BranchOptions { limit: Some(2), ..BranchOptions::default() }).await, ["main", "fork/main"]);
    assert_eq!(
        vcs.branches(BranchOptions { search: Some("origin".to_owned()), ..BranchOptions::default() }).await,
        ["origin/main"]
    );
    assert_eq!(
        vcs.branches(BranchOptions { search: Some("MAIN".to_owned()), limit: Some(2) }).await,
        ["main", "fork/main"]
    );
}

#[tokio::test]
async fn reports_modified_deleted_and_untracked_files() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("keep.txt"), "one\ntwo\n").await;
    fixture.write(&project.join("gone.txt"), "bye\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("keep.txt"), "one\nthree\n").await;
    tokio::fs::remove_file(project.join("gone.txt")).await.expect("removes the tracked file");
    fixture.write(&project.join("new.txt"), "hello\nworld\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let status = vcs.status().await;
    let actual =
        status.iter().map(|row| (row.file.as_str(), row.additions, row.deletions, row.status)).collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("gone.txt", 0.0, 1.0, VcsFileStatusStatus::Deleted),
            ("keep.txt", 1.0, 1.0, VcsFileStatusStatus::Modified),
            ("new.txt", 2.0, 0.0, VcsFileStatusStatus::Added),
        ]
    );
}

#[tokio::test]
async fn batches_untracked_files_without_spawning_git_per_file() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    let count = 40;
    fixture.init_git(&project).await;
    fixture.write(&project.join("keep.txt"), "one\n").await;
    fixture.write(&project.join(".gitignore"), "*.log\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("staged.txt"), "staged\n").await;
    fixture.git(&project, &["add", "staged.txt"]).await;
    for index in 0..count {
        fixture.write(&project.join(format!("evals/nested/file-{index}.md")), &format!("line {index}\nmore\n")).await;
    }
    fixture.write(&project.join("evals/ignored.log"), "ignored\n").await;
    let index = tokio::fs::read(project.join(".git").join("index")).await.expect("reads the index");

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let executions = Arc::new(AtomicUsize::new(0));
    let counted = executions.clone();
    fixture.git.set_observer(Some(Arc::new(move |_args: &[String]| {
        counted.fetch_add(1, Ordering::SeqCst);
    })));
    executions.store(0, Ordering::SeqCst);
    let diff = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    let status = vcs.status().await;
    fixture.git.set_observer(None);
    assert!(executions.load(Ordering::SeqCst) < count);

    assert_eq!(files(&diff), status.iter().map(|row| row.file.as_str()).collect::<Vec<_>>());
    assert_eq!(diff.len(), count + 1);
    assert!(diff.iter().all(|row| !row.file.ends_with(".log")));
    let staged = find_row(&diff, "staged.txt");
    assert_eq!(staged.status, FileDiffInfoStatus::Added);
    assert_eq!(staged.additions, 1.0);
    let nested = find_row(&diff, "evals/nested/file-7.md");
    assert_eq!(nested.status, FileDiffInfoStatus::Added);
    assert_eq!(nested.additions, 2.0);
    assert_eq!(nested.deletions, 0.0);
    assert!(nested.patch.contains("+line 7"));
    assert_eq!(find_status(&status, "evals/nested/file-7.md").additions, 2.0);
    assert_eq!(tokio::fs::read(project.join(".git").join("index")).await.expect("reads the index"), index);
}

#[tokio::test]
async fn lists_an_untracked_folder_as_one_entry_and_entries_from_inside_it() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("keep.txt"), "one\n").await;
    fixture.write(&project.join(".gitignore"), "artifacts/\nresearch/noisy/a-*\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("keep.txt"), "two\n").await;
    fixture.write(&project.join("research/noisy/z-important.ts"), "keep\n").await;
    fixture.write(&project.join("research/deep/inner.md"), "inner\n").await;
    for index in 0..505 {
        fixture.write(&project.join(format!("research/wide/{index:03}.md")), "wide\n").await;
        fixture.write(&project.join(format!("research/noisy/a-{index}.log")), "ignored\n").await;
    }
    for index in 0..50 {
        fixture.write(&project.join(format!("artifacts/{index}.txt")), "ignored\n").await;
        fixture.write(&project.join(format!("research/{index}.md")), &format!("line {index}\n")).await;
    }

    let (root, _root_registration) = fixture.git_service(&project, &project).await;
    let root_folders = root.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds");
    assert_eq!(files(&root_folders), ["keep.txt", "research/"]);
    assert_eq!(root_folders[1].status, FileDiffInfoStatus::Added);
    assert_eq!(root_folders[1].additions, 0.0);
    assert_eq!(root_folders[1].deletions, 0.0);
    assert_eq!(root.diff(VcsMode::Working, options()).await.expect("diff succeeds").len(), 558);
    assert_eq!(root.status().await.len(), 558);

    let (research, _research_registration) = fixture.git_service(&project.join("research"), &project).await;
    let research_folders = research.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds");
    assert_eq!(research_folders.len(), 53);
    let seven = find_row(&research_folders, "research/7.md");
    assert_eq!(seven.status, FileDiffInfoStatus::Added);
    assert_eq!(seven.additions, 1.0);
    assert!(seven.patch.contains("+line 7"));
    let folder_rows =
        research_folders.iter().filter(|row| row.file.ends_with('/')).map(|row| row.file.as_str()).collect::<Vec<_>>();
    assert_eq!(folder_rows, ["research/deep/", "research/noisy/", "research/wide/"]);
    assert!(research_folders.iter().all(|row| row.more.is_none()));

    let (deep, _deep_registration) = fixture.git_service(&project.join("research/deep"), &project).await;
    assert_eq!(
        files(&deep.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds")),
        ["research/deep/inner.md"]
    );

    let (wide, _wide_registration) = fixture.git_service(&project.join("research/wide"), &project).await;
    let wide_rows = wide.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds");
    assert_eq!(wide_rows.len(), 501);
    assert_eq!(wide_rows[wide_rows.len() - 2].file, "research/wide/499.md");
    assert_eq!(wide_rows[wide_rows.len() - 1].file, "research/wide/");
    assert_eq!(wide_rows[wide_rows.len() - 1].more, Some(FileDiffInfoMore::True));

    let (noisy, _noisy_registration) = fixture.git_service(&project.join("research/noisy"), &project).await;
    let noisy_rows = noisy.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds");
    assert_eq!(noisy_rows.len(), 1);
    assert_eq!(noisy_rows[0].file, "research/noisy/z-important.ts");
    assert!(noisy_rows[0].more.is_none());
}

#[tokio::test]
async fn stops_listing_an_untracked_folder_after_the_scan_budget() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join(".gitignore"), "huge/a-*\n").await;
    fixture.commit_all(&project, "initial").await;
    let huge = project.join("huge");
    tokio::fs::create_dir_all(&huge).await.expect("creates the scanned folder");
    for index in 0..5_000 {
        tokio::fs::write(huge.join(format!("a-{index:04}.log")), "").await.expect("writes an ignored file");
    }
    fixture.write(&huge.join("z-visible.ts"), "visible\n").await;

    let statuses = Arc::new(AtomicUsize::new(0));
    let counted = statuses.clone();
    fixture.git.set_observer(Some(Arc::new(move |args: &[String]| {
        if args.iter().any(|arg| arg == "status") {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    })));
    let (vcs, _registration) = fixture.git_service(&huge, &project).await;
    statuses.store(0, Ordering::SeqCst);
    let diff = vcs.diff(VcsMode::Working, folder_options()).await.expect("diff succeeds");
    let counted = statuses.load(Ordering::SeqCst);
    fixture.git.set_observer(None);

    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].file, "huge/");
    assert_eq!(diff[0].more, Some(FileDiffInfoMore::True));
    assert!(counted > 1);
    assert!(counted <= 11);
}

#[tokio::test]
async fn diffs_the_working_copy_against_head_with_patches() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("keep.txt"), "one\ntwo\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("keep.txt"), "one\nthree\n").await;
    fixture.write(&project.join("spaced name.txt"), "hello\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let diff = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    let actual = diff.iter().map(|row| (row.file.as_str(), row.status)).collect::<Vec<_>>();
    assert_eq!(actual, [("keep.txt", FileDiffInfoStatus::Modified), ("spaced name.txt", FileDiffInfoStatus::Added)]);
    assert!(diff[0].patch.contains("-two"));
    assert!(diff[0].patch.contains("+three"));
    assert_eq!(diff[0].additions, 1.0);
    assert_eq!(diff[0].deletions, 1.0);
    assert!(diff[1].patch.contains("+hello"));
    assert_eq!(diff[1].additions, 1.0);
}

#[tokio::test]
async fn respects_the_repository_line_ending_conversion() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    append_config(&project, "[core]\n\tautocrlf = true\n").await;
    let body = (0..20).map(|index| format!("line-{index}")).collect::<Vec<_>>().join("\r\n") + "\r\n";
    fixture.write(&project.join("file.txt"), &body).await;
    fixture.write(&project.join("same.txt"), &body).await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("file.txt"), &body.replace("line-10", "changed")).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let status = vcs.status().await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].file, "file.txt");
    assert_eq!(status[0].additions, 1.0);
    assert_eq!(status[0].deletions, 1.0);
    assert_eq!(status[0].status, VcsFileStatusStatus::Modified);

    let diff = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].file, "file.txt");
    assert_eq!(diff[0].additions, 1.0);
    assert_eq!(diff[0].deletions, 1.0);
    assert!(diff[0].patch.contains("-line-10"));
    assert!(diff[0].patch.contains("+changed"));
}

#[tokio::test]
async fn respects_the_repository_symlink_setting() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    append_config(&project, "[core]\n\tsymlinks = false\n").await;
    fixture.write(&project.join("link"), "target.txt").await;
    let hash = output(&fixture, &project, &["hash-object", "-w", "link"]).await.trim().to_owned();
    let cacheinfo = format!("120000,{hash},link");
    fixture.git(&project, &["update-index", "--add", "--cacheinfo", cacheinfo.as_str()]).await;
    fixture.git(&project, &["commit", "--no-gpg-sign", "-m", "initial"]).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert!(vcs.status().await.is_empty());
    assert!(vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds").is_empty());
}

#[tokio::test]
async fn respects_the_context_option() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    let body = (0..20).map(|index| format!("line-{index}")).collect::<Vec<_>>().join("\n") + "\n";
    fixture.write(&project.join("file.txt"), &body).await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("file.txt"), &body.replace("line-10", "changed")).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let full = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    assert!(full[0].patch.contains("line-0"));
    assert!(full[0].patch.contains("line-19"));
    let tight = vcs
        .diff(VcsMode::Working, DiffOptions { context: Some(1), ..DiffOptions::default() })
        .await
        .expect("diff succeeds");
    assert!(tight[0].patch.contains("line-9"));
    assert!(!tight[0].patch.contains("line-0"));
}

#[tokio::test]
async fn diffs_before_the_first_commit() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("new.txt"), "hello\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let status = vcs.status().await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].file, "new.txt");
    assert_eq!(status[0].additions, 1.0);
    assert_eq!(status[0].deletions, 0.0);
    assert_eq!(status[0].status, VcsFileStatusStatus::Added);

    let diff = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    assert_eq!(diff.len(), 1);
    assert!(diff[0].patch.contains("+hello"));
    assert!(vcs.base().await.expect("base resolves").is_none());
    assert_eq!(vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds"), diff);
    assert!(vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds").is_empty());
}

#[tokio::test]
async fn diffs_a_feature_branch_against_the_default_branch() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "one\n").await;
    fixture.commit_all(&project, "initial").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert!(vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds").is_empty());

    fixture.git(&project, &["checkout", "-q", "-b", "feature", "main"]).await;
    fixture.write(&project.join("file.txt"), "one\ntwo\n").await;
    fixture.commit_all(&project, "feature change").await;

    let diff = vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds");
    let actual = diff.iter().map(|row| (row.file.as_str(), row.status)).collect::<Vec<_>>();
    assert_eq!(actual, [("file.txt", FileDiffInfoStatus::Modified)]);
    assert!(diff[0].patch.contains("+two"));
}

#[tokio::test]
async fn separates_committed_combined_and_working_changes() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "feature", "main"]).await;
    fixture.write(&project.join("file.txt"), "committed\n").await;
    fixture.commit_all(&project, "feature").await;
    fixture.write(&project.join("staged.txt"), "staged\n").await;
    fixture.git(&project, &["add", "staged.txt"]).await;
    fixture.write(&project.join("file.txt"), "unstaged\n").await;
    fixture.write(&project.join("untracked.txt"), "untracked\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let committed = vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds");
    assert_eq!(files(&committed), ["file.txt"]);
    assert!(committed[0].patch.contains("-base\n+committed"));
    assert_eq!(committed[0].additions, 1.0);
    assert_eq!(committed[0].deletions, 1.0);
    let combined = vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds");
    assert_eq!(files(&combined), ["file.txt", "staged.txt", "untracked.txt"]);
    assert!(combined[0].patch.contains("-base\n+unstaged"));
    let working = vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds");
    assert_eq!(files(&working), ["file.txt", "staged.txt", "untracked.txt"]);
    assert!(working[0].patch.contains("-committed\n+unstaged"));
}

#[tokio::test]
async fn keeps_a_locally_undone_commit_visible_only_in_committed_and_working() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "feature", "main"]).await;
    fixture.write(&project.join("file.txt"), "committed\n").await;
    fixture.commit_all(&project, "feature").await;
    fixture.write(&project.join("file.txt"), "base\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert!(vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds").is_empty());
    assert!(
        vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds")[0].patch.contains("-base\n+committed")
    );
    assert!(vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds")[0].patch.contains("-committed\n+base"));
}

#[tokio::test]
async fn shows_local_wip_on_the_default_branch() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.write(&project.join("file.txt"), "dirty\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.name, "main");
    assert_eq!(base.r#ref, "refs/heads/main");
    assert_eq!(base.source, VcsBaseSource::Default);
    assert_eq!(
        vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds"),
        vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds")
    );
    assert!(vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds").is_empty());
}

#[tokio::test]
async fn keeps_unpushed_default_branch_commits_in_the_resolved_review() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    let path = text(&project);
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["remote", "add", "origin", path.as_str()]).await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/main", "HEAD"]).await;
    fixture.git(&project, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]).await;
    fixture.write(&project.join("file.txt"), "committed\n").await;
    fixture.commit_all(&project, "unpushed").await;
    fixture.write(&project.join("file.txt"), "dirty\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.name, "main");
    assert_eq!(base.r#ref, "refs/remotes/origin/main");
    assert_eq!(base.source, VcsBaseSource::Default);
    let committed = vcs.diff(VcsMode::Committed, with_base(&base.r#ref)).await.expect("diff succeeds");
    assert!(committed[0].patch.contains("-base\n+committed"));
    let branch = vcs.diff(VcsMode::Branch, with_base(&base.r#ref)).await.expect("diff succeeds");
    assert!(branch[0].patch.contains("-base\n+dirty"));
    assert!(
        vcs.diff(VcsMode::Working, options()).await.expect("diff succeeds")[0].patch.contains("-committed\n+dirty")
    );
}

#[tokio::test]
async fn scopes_committed_diffs_to_nested_paths_and_retains_metadata() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    let nested = project.join("nested");
    fixture.init_git(&project).await;
    fixture.write(&project.join("outside.txt"), "base\n").await;
    fixture.write(&nested.join("old.txt"), "rename me\n").await;
    fixture.write(&nested.join("gone.txt"), "delete me\n").await;
    write_bytes(&nested.join("image.png"), &[137, 80, 78, 71, 0, 1]).await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "feature", "main"]).await;
    fixture.write(&project.join("outside.txt"), "changed\n").await;
    tokio::fs::rename(nested.join("old.txt"), nested.join("new.txt")).await.expect("renames the tracked file");
    tokio::fs::remove_file(nested.join("gone.txt")).await.expect("removes the tracked file");
    write_bytes(&nested.join("image.png"), &[137, 80, 78, 71, 0, 2]).await;
    fixture.commit_all(&project, "feature").await;

    let (vcs, _registration) = fixture.git_service(&nested, &project).await;
    let committed = vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds");
    let actual = committed.iter().map(|row| (row.file.as_str(), row.status)).collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("nested/gone.txt", FileDiffInfoStatus::Deleted),
            ("nested/image.png", FileDiffInfoStatus::Modified),
            ("nested/new.txt", FileDiffInfoStatus::Added),
            ("nested/old.txt", FileDiffInfoStatus::Deleted),
        ]
    );
    assert!(committed[0].patch.contains("-delete me"));
    assert_eq!(committed[1].additions, 0.0);
    assert_eq!(committed[1].deletions, 0.0);
    assert!(committed[1].patch.contains("Binary files"));
    assert!(committed[2].patch.contains("+rename me"));

    tokio::fs::remove_file(nested.join("image.png")).await.expect("removes the binary file");
    fixture.write(&nested.join("new.txt"), "dirty\n").await;
    fixture.write(&nested.join("gone.txt"), "untracked resurrection\n").await;
    fixture.git(&project, &["add", "nested/new.txt"]).await;
    assert_eq!(vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds"), committed);
}

#[tokio::test]
async fn uses_an_explicit_diff_base_without_treating_upstream_or_git_config_as_a_parent() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "release"]).await;
    fixture.write(&project.join("release.txt"), "release\n").await;
    fixture.commit_all(&project, "release").await;
    fixture.git(&project, &["checkout", "-b", "feature"]).await;
    fixture.write(&project.join("feature.txt"), "feature\n").await;
    fixture.commit_all(&project, "feature").await;
    fixture.git(&project, &["branch", "--set-upstream-to=release", "feature"]).await;
    fixture.git(&project, &["config", "branch.feature.opencode-merge-base", "refs/heads/release"]).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert_choose_base(&vcs).await;
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, with_base("release")).await.expect("diff succeeds")),
        ["feature.txt"]
    );
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds")),
        ["feature.txt", "release.txt"]
    );
    assert_eq!(files(&vcs.diff(VcsMode::Branch, with_base("release")).await.expect("diff succeeds")), ["feature.txt"]);
    assert_eq!(
        files(&vcs.diff(VcsMode::Branch, options()).await.expect("diff succeeds")),
        ["feature.txt", "release.txt"]
    );
    assert_choose_base(&vcs).await;
}

#[tokio::test]
async fn reports_missing_explicit_and_default_bases() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["branch", "-m", "feature"]).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert_choose_base(&vcs).await;
    let committed = vcs.diff(VcsMode::Committed, options()).await.expect_err("committed diff fails");
    assert_eq!(committed.message, "No review base available");
    let branch = vcs.diff(VcsMode::Branch, options()).await.expect_err("branch diff fails");
    assert_eq!(branch.message, "No review base available");
    let missing = vcs.diff(VcsMode::Branch, with_base("missing")).await.expect_err("branch diff fails");
    assert_eq!(missing.message, "No merge base available for missing");
    assert!(vcs.diff(VcsMode::Working, with_base("missing")).await.expect("working diff succeeds").is_empty());
}

#[tokio::test]
async fn diffs_explicit_remote_only_bases_without_borrowing_another_fork_branch() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["remote", "add", "origin", "https://github.com/team/project.git"]).await;
    fixture.git(&project, &["remote", "add", "other", "https://github.com/other/project.git"]).await;
    fixture.git(&project, &["update-ref", "refs/remotes/other/release", "HEAD"]).await;
    fixture.write(&project.join("release.txt"), "release parent\n").await;
    fixture.git(&project, &["checkout", "-b", "feature"]).await;
    fixture.commit_all(&project, "release parent").await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/release", "HEAD"]).await;
    fixture.write(&project.join("feature.txt"), "feature\n").await;
    fixture.commit_all(&project, "feature").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, with_base("refs/remotes/origin/release")).await.expect("diff succeeds")),
        ["feature.txt"]
    );
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds")),
        ["feature.txt", "release.txt"]
    );
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, with_base("refs/remotes/other/release")).await.expect("diff succeeds")),
        ["feature.txt", "release.txt"]
    );
    fixture.git(&project, &["update-ref", "-d", "refs/remotes/origin/release"]).await;
    let removed = vcs
        .diff(VcsMode::Committed, with_base("refs/remotes/origin/release"))
        .await
        .expect_err("the removed base fails");
    assert_eq!(removed.message, "No merge base available for refs/remotes/origin/release");
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, with_base("refs/remotes/other/release")).await.expect("diff succeeds")),
        ["feature.txt", "release.txt"]
    );
}

#[tokio::test]
async fn reviews_every_feature_commit_and_local_changes_against_inferred_v2() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("base.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["remote", "add", "origin", "https://example.com/team/repo.git"]).await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/dev", "HEAD"]).await;
    fixture.git(&project, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/dev"]).await;
    fixture.write(&project.join("v2.txt"), "v2 parent\n").await;
    fixture.commit_all(&project, "v2").await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/v2", "HEAD"]).await;
    fixture.git(&project, &["checkout", "-b", "feature", "origin/v2"]).await;
    fixture.git(&project, &["config", "branch.feature.opencode-merge-base", "refs/remotes/origin/dev"]).await;
    fixture.write(&project.join("feature.txt"), "feature\n").await;
    fixture.commit_all(&project, "feature").await;
    fixture.write(&project.join("second.txt"), "second\n").await;
    fixture.commit_all(&project, "second").await;
    fixture.write(&project.join("third.txt"), "third\n").await;
    fixture.commit_all(&project, "third").await;
    fixture.write(&project.join("untracked.txt"), "local\n").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let config = fixture.read(&project.join(".git").join("config")).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.name, "v2");
    assert_eq!(base.r#ref, "refs/remotes/origin/v2");
    assert_eq!(base.source, VcsBaseSource::Reflog);
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, with_base(&base.r#ref)).await.expect("diff succeeds")),
        ["feature.txt", "second.txt", "third.txt"]
    );
    assert_eq!(
        files(&vcs.diff(VcsMode::Branch, with_base(&base.r#ref)).await.expect("diff succeeds")),
        ["feature.txt", "second.txt", "third.txt", "untracked.txt"]
    );
    assert_eq!(
        files(&vcs.diff(VcsMode::Committed, options()).await.expect("diff succeeds")),
        ["feature.txt", "second.txt", "third.txt", "v2.txt"]
    );
    assert_eq!(fixture.read(&project.join(".git").join("config")).await, config);
    assert!(!project.join(".git").join("opencode-review.json").exists());
    fixture.git(&project, &["config", "branch.feature.opencode-merge-base", "missing"]).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.r#ref, "refs/remotes/origin/v2");
    fixture.git(&project, &["checkout", "-b", "uncertain", "HEAD"]).await;
    assert_choose_base(&vcs).await;
}

#[tokio::test]
async fn ignores_stale_deleted_renamed_and_self_mirroring_creation_hints() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("base.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["branch", "parent"]).await;
    fixture.git(&project, &["checkout", "-b", "feature", "parent"]).await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.source, VcsBaseSource::Reflog);

    fixture.git(&project, &["branch", "-m", "renamed"]).await;
    assert_choose_base(&vcs).await;

    fixture.git(&project, &["checkout", "-b", "deleted", "parent"]).await;
    fixture.git(&project, &["branch", "-D", "parent"]).await;
    assert_choose_base(&vcs).await;

    fixture.git(&project, &["update-ref", "refs/remotes/origin/mirror", "HEAD"]).await;
    fixture.git(&project, &["checkout", "-b", "mirror", "origin/mirror"]).await;
    assert_choose_base(&vcs).await;

    fixture.git(&project, &["checkout", "-b", "stale", "main"]).await;
    fixture.git(&project, &["checkout", "--orphan", "unrelated"]).await;
    fixture.git(&project, &["commit", "--no-gpg-sign", "-m", "unrelated"]).await;
    fixture.git(&project, &["branch", "-f", "main", "HEAD"]).await;
    fixture.git(&project, &["checkout", "stale"]).await;
    assert_choose_base(&vcs).await;

    fixture.git(&project, &["checkout", "-b", "rebased", "unrelated"]).await;
    fixture.git(&project, &["reset", "--soft", "stale"]).await;
    assert_choose_base(&vcs).await;
}

#[tokio::test]
async fn creates_detached_worktrees_without_review_metadata() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("base.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["remote", "add", "origin", "https://example.com/team/repo.git"]).await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/dev", "HEAD"]).await;
    fixture.git(&project, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/dev"]).await;
    fixture.write(&project.join("v2.txt"), "v2\n").await;
    fixture.commit_all(&project, "v2").await;
    fixture.git(&project, &["update-ref", "refs/remotes/origin/v2", "HEAD"]).await;

    let linked = project.join("linked");
    let linked_text = text(&linked);
    fixture.git(&project, &["worktree", "add", "--detach", linked_text.as_str(), "origin/v2"]).await;
    let pointer = fixture.read(&linked.join(".git")).await;
    let git_directory =
        PathBuf::from(pointer.trim().strip_prefix("gitdir: ").expect("the worktree git directory pointer"));
    assert!(!git_directory.join("opencode-review.json").exists());

    let (vcs, _registration) = fixture.git_service(&linked, &linked).await;
    assert_choose_base(&vcs).await;
    assert!(vcs.diff(VcsMode::Committed, with_base("origin/v2")).await.expect("diff succeeds").is_empty());
    assert_eq!(files(&vcs.diff(VcsMode::Committed, with_base("origin/dev")).await.expect("diff succeeds")), ["v2.txt"]);
    fixture.git(&linked, &["checkout", "-b", "child"]).await;
    assert_choose_base(&vcs).await;
    fixture.git(&linked, &["checkout", "-b", "named", "origin/v2"]).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.name, "v2");
    assert_eq!(base.r#ref, "refs/remotes/origin/v2");
    assert_eq!(base.source, VcsBaseSource::Reflog);
    assert!(!git_directory.join("opencode-review.json").exists());
}

#[tokio::test]
async fn rejects_a_branch_creation_anchor_removed_by_rebase() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("base.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "parent", "main"]).await;
    fixture.write(&project.join("parent.txt"), "parent\n").await;
    fixture.commit_all(&project, "parent").await;
    fixture.git(&project, &["checkout", "-b", "feature", "parent"]).await;
    fixture.write(&project.join("feature.txt"), "feature\n").await;
    fixture.commit_all(&project, "feature").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.name, "parent");

    fixture.git(&project, &["rebase", "--onto", "main", "parent", "feature"]).await;
    assert_choose_base(&vcs).await;
}

#[tokio::test]
async fn does_not_infer_beyond_the_bounded_creation_reflog_window() {
    let fixture = Fixture::new();
    let project = fixture.path("project");
    fixture.init_git(&project).await;
    fixture.write(&project.join("file.txt"), "base\n").await;
    fixture.commit_all(&project, "initial").await;
    fixture.git(&project, &["checkout", "-b", "feature", "main"]).await;
    fixture.write(&project.join("file.txt"), "feature\n").await;
    fixture.commit_all(&project, "feature").await;

    let (vcs, _registration) = fixture.git_service(&project, &project).await;
    let base = vcs.base().await.expect("base resolves").expect("base exists");
    assert_eq!(base.source, VcsBaseSource::Reflog);

    let before = output(&fixture, &project, &["rev-parse", "main"]).await.trim().to_owned();
    let after = output(&fixture, &project, &["rev-parse", "HEAD"]).await.trim().to_owned();
    let updates = (0..260)
        .map(|index| {
            let target = if index % 2 == 0 { before.as_str() } else { after.as_str() };
            format!("start\nupdate refs/heads/feature {target}\nprepare\ncommit\n")
        })
        .collect::<String>();
    let result = fixture
        .git
        .execute(
            &["update-ref", "-m", "test", "--stdin"],
            RunOptions { cwd: Some(text(&project)), stdin: Some(updates.into_bytes()), ..RunOptions::default() },
        )
        .await
        .expect("git starts");
    assert_eq!(result.status, 0, "git update-ref failed: {}", result.stderr_text());
    assert_choose_base(&vcs).await;
}

fn text(path: &Path) -> String {
    path.to_str().expect("path is UTF-8").to_owned()
}

fn files(rows: &[FileDiffInfo]) -> Vec<&str> {
    rows.iter().map(|row| row.file.as_str()).collect()
}

fn find_row<'a>(rows: &'a [FileDiffInfo], file: &str) -> &'a FileDiffInfo {
    rows.iter().find(|row| row.file == file).unwrap_or_else(|| panic!("missing diff row {file}"))
}

fn find_status<'a>(rows: &'a [VcsFileStatus], file: &str) -> &'a VcsFileStatus {
    rows.iter().find(|row| row.file == file).unwrap_or_else(|| panic!("missing status row {file}"))
}

fn options() -> DiffOptions {
    DiffOptions::default()
}

fn with_base(base: &str) -> DiffOptions {
    DiffOptions { base: Some(base.to_owned()), ..DiffOptions::default() }
}

fn folder_options() -> DiffOptions {
    DiffOptions { untracked: Some(VcsUntracked::Folders), ..DiffOptions::default() }
}

async fn assert_choose_base(vcs: &Vcs) {
    let error = vcs.base().await.expect_err("base is unavailable");
    assert_eq!(error.message, "Choose a review base");
}

async fn append_config(project: &Path, content: &str) {
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(project.join(".git").join("config"))
        .await
        .expect("opens the repository config");
    file.write_all(content.as_bytes()).await.expect("appends the repository config");
}

async fn write_bytes(path: &Path, content: &[u8]) {
    tokio::fs::write(path, content).await.expect("writes the file");
}

async fn output(fixture: &Fixture, directory: &Path, args: &[&str]) -> String {
    let options = RunOptions { cwd: Some(text(directory)), ..RunOptions::default() };
    let result = fixture.git.execute(args, options).await.expect("git starts");
    assert_eq!(result.status, 0, "git {} failed: {}", args.join(" "), result.stderr_text());
    result.text()
}
