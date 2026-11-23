use http::StatusCode;

use crate::{app, app_with_cors, basic, request_with, text};

const FORBIDDEN_BODY: &str = r#"{"_tag":"ForbiddenError","message":"Cross-site requests to the API are not allowed"}"#;

fn navigate(site: &'static str) -> [(&'static str, &'static str); 3] {
    [("sec-fetch-site", site), ("sec-fetch-mode", "navigate"), ("sec-fetch-dest", "document")]
}

fn cors(origin: &str) -> [(&str, &str); 3] {
    [("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "cors"), ("origin", origin)]
}

#[tokio::test]
async fn cross_site_navigations_to_the_api_are_forbidden() {
    let app = app();
    let response = app.handle(request_with("GET", "/api/fs/read/page.html", &navigate("cross-site"))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    assert_eq!(text(response).await, FORBIDDEN_BODY);

    let same_site = app.handle(request_with("GET", "/api/fs/read/page.html", &navigate("same-site"))).await;
    assert_eq!(same_site.status(), StatusCode::FORBIDDEN);

    let post = app.handle(request_with("POST", "/api/path", &navigate("cross-site"))).await;
    assert_eq!(post.status(), StatusCode::FORBIDDEN);

    let credential = basic("opencode", "wrong");
    let mut unauthorized_navigation = navigate("cross-site").to_vec();
    unauthorized_navigation.push(("authorization", credential.as_str()));
    let unauthenticated = app.handle(request_with("GET", "/api/fs/read/page.html", &unauthorized_navigation)).await;
    assert_eq!(unauthenticated.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn native_clients_same_origin_and_typed_in_urls_pass() {
    let app = app();
    let bare = app.handle(request_with("GET", "/api/fs/read/page.html", &[])).await;
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);

    let same_origin = app.handle(request_with("GET", "/api/fs/read/page.html", &navigate("same-origin"))).await;
    assert_eq!(same_origin.status(), StatusCode::UNAUTHORIZED);

    let typed_in = app.handle(request_with("GET", "/api/fs/read/page.html", &navigate("none"))).await;
    assert_eq!(typed_in.status(), StatusCode::UNAUTHORIZED);

    let same_origin_cors = app
        .handle(request_with(
            "GET",
            "/api/fs/read/page.html",
            &[("sec-fetch-site", "same-origin"), ("sec-fetch-mode", "cors")],
        ))
        .await;
    assert_eq!(same_origin_cors.status(), StatusCode::UNAUTHORIZED);

    let outside_the_api = app.handle(request_with("GET", "/", &navigate("cross-site"))).await;
    assert_eq!(outside_the_api.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn artifact_view_pages_are_exempt() {
    let app = app();
    let trailing =
        app.handle(request_with("GET", "/api/experimental/artifacts/art_1/view/", &navigate("cross-site"))).await;
    assert_eq!(trailing.status(), StatusCode::UNAUTHORIZED);

    let bare = app.handle(request_with("GET", "/api/experimental/artifacts/art_1/view", &navigate("cross-site"))).await;
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);

    let empty_id = app.handle(request_with("GET", "/api/experimental/artifacts//view/", &navigate("cross-site"))).await;
    assert_eq!(empty_id.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_site_calls_need_an_allowed_origin() {
    let app = app();
    for origin in ["https://app.opencode.ai", "oc://renderer", "http://localhost:3000"] {
        let response = app.handle(request_with("GET", "/api/fs/read/page.html", &cors(origin))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let foreign = app.handle(request_with("GET", "/api/fs/read/page.html", &cors("https://example.com"))).await;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    let opaque =
        app.handle(request_with("POST", "/api/path", &[("sec-fetch-site", "cross-site"), ("origin", "null")])).await;
    assert_eq!(opaque.status(), StatusCode::FORBIDDEN);

    let no_cors_foreign = app
        .handle(request_with(
            "POST",
            "/api/path",
            &[("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "no-cors"), ("origin", "https://example.com")],
        ))
        .await;
    assert_eq!(no_cors_foreign.status(), StatusCode::FORBIDDEN);

    let media = app
        .handle(request_with(
            "GET",
            "/api/fs/read/page.html",
            &[("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "no-cors")],
        ))
        .await;
    assert_eq!(media.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_host_header_allows_same_host_origins() {
    let app = app();
    let same_host = app
        .handle(request_with(
            "GET",
            "/api/info",
            &[
                ("sec-fetch-site", "cross-site"),
                ("sec-fetch-mode", "cors"),
                ("host", "192.168.1.10:4096"),
                ("origin", "http://192.168.1.10:4096"),
            ],
        ))
        .await;
    assert_eq!(same_host.status(), StatusCode::UNAUTHORIZED);

    let other_port = app
        .handle(request_with(
            "GET",
            "/api/info",
            &[
                ("sec-fetch-site", "cross-site"),
                ("sec-fetch-mode", "cors"),
                ("host", "192.168.1.10:4096"),
                ("origin", "http://192.168.1.10:4097"),
            ],
        ))
        .await;
    assert_eq!(other_port.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn configured_extra_origins_pass() {
    let app = app_with_cors(vec!["http://192.168.1.10:3001".to_owned()]);
    let allowed = app.handle(request_with("GET", "/api/fs/read/page.html", &cors("http://192.168.1.10:3001"))).await;
    assert_eq!(allowed.status(), StatusCode::UNAUTHORIZED);

    let rejected = app.handle(request_with("GET", "/api/fs/read/page.html", &cors("http://192.168.1.10:3002"))).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn forbidden_responses_still_carry_the_cors_vary_header() {
    let app = app();
    let response = app.handle(request_with("GET", "/api/fs/read/page.html", &navigate("cross-site"))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        response
            .headers()
            .get("vary")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|vary| vary.starts_with("Origin"))
    );
}
