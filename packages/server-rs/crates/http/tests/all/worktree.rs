use std::path::Path;

use bytes::Bytes;
use http::{Request, StatusCode};
use serde_json::{Value, json};

use crate::{authorized, basic, ready_app_with_services, services_at, temp_directory, text};

fn location_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/location?location%5Bdirectory%5D={encoded}")
}

fn worktree_uri(project_id: &str) -> String {
    format!("/api/worktree?projectID={project_id}")
}

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Bytes> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", basic("opencode", "pw"))
        .header("content-type", "application/json")
        .body(Bytes::from(serde_json::to_vec(body).expect("a JSON body")))
        .expect("a request")
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

fn real(path: &Path) -> String {
    std::fs::canonicalize(path).expect("a canonical path").to_string_lossy().into_owned()
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn list_reads_the_saved_inventory_without_booting_a_location() {
    let root = temp_directory("worktree-list");
    let directory = root.join("repo");
    std::fs::create_dir_all(&directory).expect("create the repository");
    git(&directory, &["init"]).await;
    git(&directory, &["commit", "--allow-empty", "-m", "root"]).await;
    let app = ready_app_with_services(services_at(&root));

    let location = app.handle(authorized("GET", &location_uri(&directory))).await;
    assert_eq!(location.status(), StatusCode::OK);
    let location = body_json(location).await;
    let project_id = location["project"]["id"].as_str().expect("a project id").to_owned();
    let canonical = location["project"]["directory"].as_str().expect("a project directory").to_owned();
    let loaded = body_json(app.handle(authorized("GET", "/api/debug/location")).await).await;

    std::fs::remove_dir_all(&directory).expect("remove the checkout");

    let listed = app.handle(authorized("GET", &worktree_uri(&project_id))).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(body_json(listed).await, json!([{ "directory": canonical }]));

    let created = app.handle(json_request("POST", "/api/worktree", &json!({ "projectID": project_id }))).await;
    assert_eq!(created.status(), StatusCode::BAD_REQUEST);
    let created = body_json(created).await;
    assert_eq!(created["_tag"], "WorktreeError");
    assert_eq!(created["name"], "WorktreeError");
    assert_eq!(created["data"]["message"], format!("Worktree directory unavailable: {canonical}"));
    assert!(created["data"].get("forceRequired").is_none());

    let refreshed =
        app.handle(json_request("POST", "/api/worktree/refresh", &json!({ "projectID": project_id }))).await;
    assert_eq!(refreshed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(refreshed).await["data"]["message"], format!("Worktree directory unavailable: {canonical}"));

    assert_eq!(body_json(app.handle(authorized("GET", "/api/debug/location")).await).await, loaded);

    let missing = app.handle(authorized("GET", "/api/worktree?projectID=unknown")).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing = body_json(missing).await;
    assert_eq!(missing["_tag"], "ProjectNotFoundError");
    assert_eq!(missing["projectID"], "unknown");
    assert_eq!(missing["message"], "Project not found: unknown");

    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    let legacy = app.handle(authorized("GET", &format!("/api/worktree?location%5Bdirectory%5D={encoded}"))).await;
    assert_eq!(legacy.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(legacy).await["_tag"], "InvalidRequestError");
}

#[tokio::test]
async fn refresh_discovers_both_clones_while_list_alone_skips_external_worktrees() {
    let root = temp_directory("worktree-clones");
    let first = root.join("first");
    let second = root.join("second");
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(&first).expect("create the first clone");
    git(&first, &["init"]).await;
    git(&first, &["commit", "--allow-empty", "-m", "root"]).await;
    git(&first, &["remote", "add", "origin", "https://github.com/example/clones.git"]).await;
    git(&first, &["clone", "--no-hardlinks", &real(&first), &second.to_string_lossy()]).await;
    git(&second, &["remote", "set-url", "origin", "https://github.com/example/clones.git"]).await;
    let app = ready_app_with_services(services_at(&root));

    let first_location = app.handle(authorized("GET", &location_uri(&first))).await;
    assert_eq!(first_location.status(), StatusCode::OK);
    let first_location = body_json(first_location).await;
    let second_location = app.handle(authorized("GET", &location_uri(&second))).await;
    assert_eq!(second_location.status(), StatusCode::OK);
    let second_location = body_json(second_location).await;
    assert_eq!(first_location["project"]["id"], second_location["project"]["id"]);
    let project_id = second_location["project"]["id"].as_str().expect("a project id").to_owned();
    assert_eq!(first_location["project"]["directory"], real(&first));
    assert_eq!(second_location["project"]["directory"], real(&second));

    let listed = app.handle(authorized("GET", &worktree_uri(&project_id))).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(body_json(listed).await.as_array().expect("an array of worktrees").len(), 2);

    git(&first, &["worktree", "add", "--detach", &a.to_string_lossy(), "HEAD"]).await;
    git(&second, &["worktree", "add", "--detach", &b.to_string_lossy(), "HEAD"]).await;

    let listed = app.handle(authorized("GET", &worktree_uri(&project_id))).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(body_json(listed).await.as_array().expect("an array of worktrees").len(), 2);

    let refreshed =
        app.handle(json_request("POST", "/api/worktree/refresh", &json!({ "projectID": project_id }))).await;
    assert_eq!(refreshed.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(refreshed).await, "");

    let listed = body_json(app.handle(authorized("GET", &worktree_uri(&project_id))).await).await;
    let listed = listed.as_array().expect("an array of worktrees");
    assert_eq!(listed.len(), 4);
    for expected in [
        json!({ "directory": real(&first) }),
        json!({ "directory": real(&second) }),
        json!({ "directory": real(&a), "strategy": "git" }),
        json!({ "directory": real(&b), "strategy": "git" }),
    ] {
        assert!(listed.iter().any(|row| row == &expected), "missing {expected}");
    }

    let a_real = real(&a);
    let second_real = real(&second);
    std::fs::remove_dir_all(&second).expect("remove the second clone");
    git(&first, &["worktree", "remove", &a.to_string_lossy()]).await;
    let refreshed =
        app.handle(json_request("POST", "/api/worktree/refresh", &json!({ "projectID": project_id }))).await;
    assert_eq!(refreshed.status(), StatusCode::NO_CONTENT);

    let listed = body_json(app.handle(authorized("GET", &worktree_uri(&project_id))).await).await;
    let listed = listed.as_array().expect("an array of worktrees");
    assert!(!listed.iter().any(|row| row == &json!({ "directory": a_real, "strategy": "git" })));
    assert!(!listed.iter().any(|row| row == &json!({ "directory": second_real })));
    assert!(listed.iter().any(|row| row == &json!({ "directory": real(&b), "strategy": "git" })));
}

#[tokio::test]
async fn remove_loads_canonical_strategies_and_enforces_project_ownership() {
    let root = temp_directory("worktree-remove");
    let directory = root.join("repo");
    let other = root.join("other");
    let linked = root.join("linked");
    std::fs::create_dir_all(&directory).expect("create the repository");
    std::fs::create_dir_all(&other).expect("create the other repository");
    for (checkout, name) in [(&directory, "remove"), (&other, "other")] {
        git(checkout, &["init"]).await;
        git(checkout, &["commit", "--allow-empty", "-m", "root"]).await;
        let remote = format!("https://github.com/example/{name}.git");
        git(checkout, &["remote", "add", "origin", &remote]).await;
    }
    git(&directory, &["worktree", "add", "--detach", &linked.to_string_lossy(), "HEAD"]).await;
    let app = ready_app_with_services(services_at(&root));

    let project_id = body_json(app.handle(authorized("GET", &location_uri(&linked))).await).await["project"]["id"]
        .as_str()
        .expect("a project id")
        .to_owned();
    let foreign_id = body_json(app.handle(authorized("GET", &location_uri(&other))).await).await["project"]["id"]
        .as_str()
        .expect("a project id")
        .to_owned();
    assert_ne!(project_id, foreign_id);

    let foreign = app
        .handle(json_request(
            "DELETE",
            "/api/worktree",
            &json!({ "projectID": foreign_id, "directory": real(&linked), "force": true }),
        ))
        .await;
    assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);
    let foreign = body_json(foreign).await;
    assert_eq!(foreign["_tag"], "WorktreeError");
    assert_eq!(foreign["name"], "WorktreeError");
    assert!(linked.is_dir());

    let removed = app
        .handle(json_request(
            "DELETE",
            "/api/worktree",
            &json!({ "projectID": project_id, "directory": real(&linked), "force": false }),
        ))
        .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(removed).await, "");

    let locations = body_json(app.handle(authorized("GET", "/api/debug/location")).await).await;
    let locations = locations.as_array().expect("an array of locations");
    assert!(locations.iter().any(|row| row == &json!({ "directory": real(&directory) })));

    let listed = body_json(app.handle(authorized("GET", &worktree_uri(&project_id))).await).await;
    assert_eq!(listed, json!([{ "directory": real(&directory) }]));
}

#[tokio::test]
async fn creates_lists_and_removes_worktrees_by_project_id() {
    let root = temp_directory("worktree-create");
    let project = root.join("project");
    let destination = root.join("worktrees");
    std::fs::create_dir_all(&project).expect("create the project");
    git(&project, &["init"]).await;
    git(&project, &["commit", "--allow-empty", "-m", "root"]).await;
    let app = ready_app_with_services(services_at(&root));

    let location = body_json(app.handle(authorized("GET", &location_uri(&project))).await).await;
    let project_id = location["project"]["id"].as_str().expect("a project id").to_owned();
    let canonical = location["project"]["directory"].as_str().expect("a project directory").to_owned();
    assert_eq!(canonical, real(&project));

    let listed = body_json(app.handle(authorized("GET", &worktree_uri(&project_id))).await).await;
    assert_eq!(listed, json!([{ "directory": canonical }]));

    let created = app
        .handle(json_request(
            "POST",
            "/api/worktree",
            &json!({
                "projectID": project_id,
                "directory": destination.to_string_lossy(),
                "name": "api",
            }),
        ))
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(body_json(created).await, json!({ "directory": real(&destination.join("api")) }));

    let listed = body_json(app.handle(authorized("GET", &worktree_uri(&project_id))).await).await;
    let listed = listed.as_array().expect("an array of worktrees");
    assert!(listed.iter().any(|row| row == &json!({ "directory": real(&destination.join("api")), "strategy": "git" })));

    let removed = app
        .handle(json_request(
            "DELETE",
            "/api/worktree",
            &json!({
                "projectID": project_id,
                "directory": real(&destination.join("api")),
                "force": false,
            }),
        ))
        .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(removed).await, "");
}
