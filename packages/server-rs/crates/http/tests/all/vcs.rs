use std::path::Path;

use http::StatusCode;
use serde_json::{Value, json};

use crate::{authorized, ready_app_with_services, services_at, temp_directory, text};

fn uri(path: &str, directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("{path}?location%5Bdirectory%5D={encoded}")
}

fn uri_with(path: &str, directory: &Path, params: &[(&str, &str)]) -> String {
    let mut result = uri(path, directory);
    for (name, value) in params {
        result.push('&');
        result.push_str(name);
        result.push('=');
        result.push_str(value);
    }
    result
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write a fixture file");
}

async fn git(directory: &Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("git")
        .arg("-c")
        .arg("user.email=test@opencode.test")
        .arg("-c")
        .arg("user.name=Test")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .current_dir(directory)
        .output()
        .await
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("git stdout is utf-8")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn init_initializes_git_and_refreshes_the_location() {
    let start = temp_directory("vcs-init-start");
    let directory = temp_directory("vcs-init-target");
    write(&directory.join("hello.txt"), "hello\n");
    let app = ready_app_with_services(services_at(&start));

    let before = body_json(app.handle(authorized("GET", &uri("/api/location", &directory))).await).await;
    let original = before["project"]["id"].as_str().expect("a project id").to_owned();

    let initialized = app.handle(authorized("POST", &uri("/api/vcs/init", &directory))).await;
    assert_eq!(initialized.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(initialized).await, "");
    assert_eq!(git(&directory, &["rev-parse", "--is-inside-work-tree"]).await, "true\n");

    let location = body_json(app.handle(authorized("GET", &uri("/api/location", &directory))).await).await;
    assert_eq!(location["project"]["directory"], directory.to_string_lossy().as_ref());
    let refreshed = location["project"]["id"].as_str().expect("a project id").to_owned();
    assert_ne!(refreshed, original);

    let projects = body_json(app.handle(authorized("GET", "/api/project")).await).await;
    assert!(
        projects
            .as_array()
            .expect("an array of projects")
            .iter()
            .any(|project| project["id"] == refreshed.as_str() && project["vcs"] == "git")
    );

    let info = body_json(app.handle(authorized("GET", &uri("/api/vcs", &directory))).await).await;
    assert_eq!(info["data"]["provider"], "git");

    let diff =
        body_json(app.handle(authorized("GET", &uri_with("/api/vcs/diff", &directory, &[("mode", "working")]))).await)
            .await;
    assert!(diff["data"].as_array().expect("an array of diffs").iter().any(|row| row["file"] == "hello.txt"));

    let repeated = app.handle(authorized("POST", &uri("/api/vcs/init", &directory))).await;
    assert_eq!(repeated.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn init_does_not_create_a_missing_project_directory() {
    let start = temp_directory("vcs-missing-start");
    let app = ready_app_with_services(services_at(&start));
    let directory = start.join("absent");

    let response = app.handle(authorized("POST", &uri("/api/vcs/init", &directory))).await;
    assert_ne!(response.status(), StatusCode::NO_CONTENT);
    assert!(!directory.join(".git").join("HEAD").exists());
}

#[tokio::test]
async fn init_rejects_unknown_providers_without_initializing() {
    let start = temp_directory("vcs-provider-start");
    let directory = temp_directory("vcs-provider-target");
    let app = ready_app_with_services(services_at(&start));

    let unknown =
        app.handle(authorized("POST", &uri_with("/api/vcs/init", &directory, &[("provider", "unknown")]))).await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let body = body_json(unknown).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["field"], "provider");
    assert!(!directory.join(".git").join("HEAD").exists());
    assert!(!directory.join(".hg").join("requires").exists());

    let initialized =
        app.handle(authorized("POST", &uri_with("/api/vcs/init", &directory, &[("provider", "git")]))).await;
    assert_eq!(initialized.status(), StatusCode::NO_CONTENT);
    assert!(directory.join(".git").join("HEAD").exists());
}

#[tokio::test]
async fn init_initializes_mercurial_and_serves_an_untracked_diff() {
    if tokio::process::Command::new("hg").arg("--version").output().await.is_err() {
        return;
    }
    let start = temp_directory("vcs-hg-start");
    let directory = temp_directory("vcs-hg-target");
    write(&directory.join("hello.txt"), "hello\n");
    let app = ready_app_with_services(services_at(&start));

    let initialized =
        app.handle(authorized("POST", &uri_with("/api/vcs/init", &directory, &[("provider", "hg")]))).await;
    assert_eq!(initialized.status(), StatusCode::NO_CONTENT);
    assert!(directory.join(".hg").join("requires").exists());

    let info = body_json(app.handle(authorized("GET", &uri("/api/vcs", &directory))).await).await;
    assert_eq!(info["data"]["provider"], "hg");

    let diff =
        body_json(app.handle(authorized("GET", &uri_with("/api/vcs/diff", &directory, &[("mode", "working")]))).await)
            .await;
    assert!(diff["data"].as_array().expect("an array of diffs").iter().any(|row| row["file"] == "hello.txt"));

    let repeated = app.handle(authorized("POST", &uri_with("/api/vcs/init", &directory, &[("provider", "git")]))).await;
    assert_eq!(repeated.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn serves_review_bases_committed_diffs_and_unavailable_base_errors() {
    let start = temp_directory("vcs-base-start");
    let directory = temp_directory("vcs-base-target");
    git(&directory, &["init", "-b", "main"]).await;
    write(&directory.join("file.txt"), "base\n");
    git(&directory, &["add", "."]).await;
    git(&directory, &["commit", "-m", "initial"]).await;
    git(&directory, &["checkout", "-b", "feature", "main"]).await;
    write(&directory.join("file.txt"), "committed\n");
    git(&directory, &["commit", "-am", "feature"]).await;
    write(&directory.join("file.txt"), "dirty\n");
    let app = ready_app_with_services(services_at(&start));

    let base = body_json(app.handle(authorized("GET", &uri("/api/vcs/base", &directory))).await).await;
    assert_eq!(base["data"], json!({ "name": "main", "ref": "refs/heads/main", "source": "reflog" }));

    git(&directory, &["branch", "-m", "ambiguous"]).await;
    let ambiguous = app.handle(authorized("GET", &uri("/api/vcs/base", &directory))).await;
    assert_eq!(ambiguous.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(ambiguous).await["message"], "Choose a review base");

    let diff = body_json(
        app.handle(authorized(
            "GET",
            &uri_with("/api/vcs/diff", &directory, &[("mode", "committed"), ("base", "refs/heads/main")]),
        ))
        .await,
    )
    .await;
    let rows = diff["data"].as_array().expect("an array of diffs");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["file"], "file.txt");
    assert!(rows[0]["patch"].as_str().expect("a patch").contains("-base\n+committed"));
    assert_eq!(rows[0]["additions"], 1);
    assert_eq!(rows[0]["deletions"], 1);

    let missing = app
        .handle(authorized(
            "GET",
            &uri_with("/api/vcs/diff", &directory, &[("mode", "committed"), ("base", "missing")]),
        ))
        .await;
    assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(missing).await["service"], "vcs");

    git(&directory, &["branch", "-D", "main"]).await;
    let no_base = app.handle(authorized("GET", &uri_with("/api/vcs/diff", &directory, &[("mode", "branch")]))).await;
    assert_eq!(no_base.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(no_base).await;
    assert_eq!(body["service"], "vcs");
    assert_eq!(body["message"], "No review base available");
}

#[tokio::test]
async fn serves_info_status_and_branches() {
    let start = temp_directory("vcs-info-start");
    let directory = temp_directory("vcs-info-target");
    git(&directory, &["init", "-b", "main"]).await;
    write(&directory.join("keep.txt"), "keep\n");
    git(&directory, &["add", "."]).await;
    git(&directory, &["commit", "-m", "keep"]).await;
    git(&directory, &["checkout", "-b", "feature"]).await;
    write(&directory.join("keep.txt"), "changed\n");
    let app = ready_app_with_services(services_at(&start));

    let info = body_json(app.handle(authorized("GET", &uri("/api/vcs", &directory))).await).await;
    assert_eq!(info["data"]["provider"], "git");
    assert_eq!(info["data"]["branch"]["current"], "feature");

    let status = body_json(app.handle(authorized("GET", &uri("/api/vcs/status", &directory))).await).await;
    assert!(status["data"].as_array().expect("an array of statuses").iter().any(|row| row["file"] == "keep.txt"));

    let branches =
        body_json(app.handle(authorized("GET", &uri_with("/api/vcs/branch", &directory, &[("limit", "1")]))).await)
            .await;
    assert_eq!(branches["data"].as_array().expect("an array of branches").len(), 1);
}
