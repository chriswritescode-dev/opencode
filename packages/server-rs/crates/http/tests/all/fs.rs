use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_config::paths::GlobalPaths;
use opencode_http::response::Response;
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};

use crate::{authorized, authorized_with, body, ready_app_with_services, services_at, temp_directory, text};

fn read_path(path: &str) -> String {
    format!("/api/fs/read/{}", opencode_api_types::js::encode_component(path))
}

fn list_path_at(directory: &Path, path: &str) -> String {
    format!(
        "/api/fs/list?location%5Bdirectory%5D={}&path={}",
        opencode_api_types::js::encode_component(&directory.to_string_lossy()),
        opencode_api_types::js::encode_component(path)
    )
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

async fn body_json(response: Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

fn post_json(path: &str, body: &str) -> Request<Bytes> {
    with_body(
        authorized_with(
            "POST",
            path,
            &[("content-type", "application/json"), ("content-length", &body.len().to_string())],
        ),
        body,
    )
}

fn with_body(request: Request<Bytes>, body: &str) -> Request<Bytes> {
    let (parts, _) = request.into_parts();
    Request::from_parts(parts, Bytes::copy_from_slice(body.as_bytes()))
}

#[tokio::test]
async fn reads_files_with_ranges_conditional_requests_and_confinement() {
    let root = temp_directory("fs-read");
    let project = root.join("project");
    let outside = root.join("outside");
    std::fs::create_dir_all(project.join("nested")).expect("create the nested directory");
    std::fs::create_dir_all(&outside).expect("create the outside directory");
    std::fs::write(project.join("clip.mp4"), "0123456789").expect("write clip");
    std::fs::write(project.join("empty.bin"), "").expect("write empty");
    let large: Vec<u8> = (0..300_000u32).map(|index| (index * 31 + (index >> 8)) as u8).collect();
    std::fs::write(project.join("large.mp4"), &large).expect("write large");
    std::fs::write(project.join("notes.txt"), "compressible ".repeat(1024)).expect("write notes");
    std::fs::write(outside.join("secret.txt"), "secret").expect("write secret");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, project.join("escape-link")).expect("create the escape symlink");

    let app = ready_app_with_services(services_at(&project));

    let full = app.handle(authorized("GET", &read_path("clip.mp4"))).await;
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(header(&full, "accept-ranges"), Some("bytes"));
    assert_eq!(header(&full, "content-length"), Some("10"));
    assert_eq!(header(&full, "content-type"), Some("video/mp4"));
    assert_eq!(header(&full, "cache-control"), Some("no-cache, no-transform"));
    let etag = header(&full, "etag").expect("an etag").to_owned();
    assert!(etag.starts_with("W/\"") && etag.ends_with('"') && etag.contains('-'), "{etag}");
    let last_modified = header(&full, "last-modified").expect("a last-modified").to_owned();
    assert_eq!(body(full).await.as_ref(), b"0123456789");

    let head = app.handle(authorized("HEAD", &read_path("clip.mp4"))).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header(&head, "accept-ranges"), Some("bytes"));
    assert_eq!(header(&head, "content-length"), Some("10"));
    assert_eq!(header(&head, "content-type"), Some("video/mp4"));
    assert_eq!(header(&head, "content-range"), None);
    assert_eq!(header(&head, "etag"), Some(etag.as_str()));
    assert_eq!(header(&head, "last-modified"), Some(last_modified.as_str()));
    assert_eq!(body(head).await.len(), 0);

    for (range, expected, content_range) in [
        ("bytes=0-0", "0", "bytes 0-0/10"),
        ("bytes=2-5", "2345", "bytes 2-5/10"),
        ("bytes=7-99", "789", "bytes 7-9/10"),
        ("bytes=6-", "6789", "bytes 6-9/10"),
        ("bytes=-3", "789", "bytes 7-9/10"),
        ("bytes=-99", "0123456789", "bytes 0-9/10"),
    ] {
        let partial = app.handle(authorized_with("GET", &read_path("clip.mp4"), &[("range", range)])).await;
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(header(&partial, "content-range"), Some(content_range), "{range}");
        assert_eq!(header(&partial, "content-length"), Some(expected.len().to_string().as_str()), "{range}");
        assert_eq!(header(&partial, "content-type"), Some("video/mp4"), "{range}");
        assert_eq!(header(&partial, "etag"), Some(etag.as_str()), "{range}");
        assert_eq!(body(partial).await.as_ref(), expected.as_bytes(), "{range}");
    }

    for (file, range, total) in [
        ("clip.mp4", "bytes=10-12", 10),
        ("clip.mp4", "bytes=10-", 10),
        ("clip.mp4", "bytes=5-2", 10),
        ("clip.mp4", "bytes=-0", 10),
        ("empty.bin", "bytes=0-0", 0),
    ] {
        let unsatisfiable = app.handle(authorized_with("GET", &read_path(file), &[("range", range)])).await;
        assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE, "{file} {range}");
        assert_eq!(header(&unsatisfiable, "content-range"), Some(format!("bytes */{total}").as_str()));
        assert_eq!(body(unsatisfiable).await.len(), 0);
    }

    for range in ["bytes=0-1, 4-5", "items=0-3", "bytes=", "bytes=-", "bytes=abc-def", "bytes=0-1-2"] {
        let fallback = app.handle(authorized_with("GET", &read_path("clip.mp4"), &[("range", range)])).await;
        assert_eq!(fallback.status(), StatusCode::OK, "{range}");
        assert_eq!(header(&fallback, "content-range"), None, "{range}");
        assert_eq!(header(&fallback, "content-length"), Some("10"), "{range}");
        assert_eq!(body(fallback).await.as_ref(), b"0123456789", "{range}");
    }

    let strong = &etag[2..];
    let combined = format!("\"other\", {etag}");
    for (name, value) in [
        ("if-none-match", etag.as_str()),
        ("if-none-match", strong),
        ("if-none-match", combined.as_str()),
        ("if-none-match", "*"),
        ("if-modified-since", last_modified.as_str()),
    ] {
        let not_modified = app.handle(authorized_with("GET", &read_path("clip.mp4"), &[(name, value)])).await;
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED, "{name}: {value}");
        assert_eq!(header(&not_modified, "etag"), Some(etag.as_str()));
        assert_eq!(header(&not_modified, "last-modified"), Some(last_modified.as_str()));
        assert_eq!(header(&not_modified, "cache-control"), Some("no-cache, no-transform"));
        assert_eq!(body(not_modified).await.len(), 0);
    }

    let past = app.handle(authorized_with(
        "GET",
        &read_path("clip.mp4"),
        &[("if-modified-since", "Wed, 01 Jan 2020 00:00:00 GMT")],
    ));
    assert_eq!(past.await.status(), StatusCode::OK);

    let mismatch = app.handle(authorized_with(
        "GET",
        &read_path("clip.mp4"),
        &[("if-none-match", "W/\"mismatch\""), ("if-modified-since", &last_modified)],
    ));
    assert_eq!(mismatch.await.status(), StatusCode::OK);

    for if_range in [etag.as_str(), strong, last_modified.as_str()] {
        let matched = app.handle(authorized_with(
            "GET",
            &read_path("clip.mp4"),
            &[("range", "bytes=1-4"), ("if-range", if_range)],
        ));
        let matched = matched.await;
        assert_eq!(matched.status(), StatusCode::PARTIAL_CONTENT, "{if_range}");
        assert_eq!(header(&matched, "content-range"), Some("bytes 1-4/10"), "{if_range}");
        assert_eq!(body(matched).await.as_ref(), b"1234", "{if_range}");
    }

    for if_range in ["W/\"stale\"", "\"stale\"", "Wed, 01 Jan 2020 00:00:00 GMT", "not-a-date"] {
        let mismatched = app
            .handle(authorized_with("GET", &read_path("clip.mp4"), &[("range", "bytes=1-4"), ("if-range", if_range)]))
            .await;
        assert_eq!(mismatched.status(), StatusCode::OK, "{if_range}");
        assert_eq!(header(&mismatched, "content-range"), None, "{if_range}");
        assert_eq!(header(&mismatched, "content-length"), Some("10"), "{if_range}");
        assert_eq!(body(mismatched).await.as_ref(), b"0123456789", "{if_range}");
    }

    let mut forbidden = vec!["../outside/secret.txt", "nested"];
    #[cfg(unix)]
    forbidden.push("escape-link/secret.txt");
    for path in forbidden {
        let escaped = app.handle(authorized("GET", &read_path(path))).await;
        assert_eq!(escaped.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body_json(escaped).await["_tag"], "FileNotFoundError");
    }

    let text_response =
        app.handle(authorized_with("GET", &read_path("notes.txt"), &[("accept-encoding", "gzip")])).await;
    assert_eq!(text_response.status(), StatusCode::OK);
    assert_eq!(header(&text_response, "content-encoding"), None);
    assert_eq!(header(&text_response, "content-length"), Some("13312"));
    assert_eq!(body(text_response).await.as_ref(), "compressible ".repeat(1024).as_bytes());

    let full_large = app.handle(authorized("GET", &read_path("large.mp4"))).await;
    assert_eq!(full_large.status(), StatusCode::OK);
    assert_eq!(header(&full_large, "content-length"), Some(large.len().to_string().as_str()));
    assert_eq!(body(full_large).await.as_ref(), large.as_slice());

    let start = 65_536usize;
    let end = 131_071usize;
    let range = format!("bytes={start}-{end}");
    let partial_large = app.handle(authorized_with("GET", &read_path("large.mp4"), &[("range", &range)])).await;
    assert_eq!(partial_large.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header(&partial_large, "content-range"), Some(format!("bytes {start}-{end}/{}", large.len()).as_str()));
    assert_eq!(header(&partial_large, "content-length"), Some((end - start + 1).to_string().as_str()));
    assert_eq!(body(partial_large).await.as_ref(), &large[start..=end]);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn opens_no_file_for_head_and_closes_it_when_a_download_is_dropped() {
    use http_body_util::BodyExt;

    let project = temp_directory("fs-handles");
    let file = project.join("large.bin");
    std::fs::write(&file, vec![0u8; 32 * 1024 * 1024]).expect("write large.bin");
    let real = std::fs::canonicalize(&file).expect("canonicalize large.bin");
    let app = ready_app_with_services(services_at(&project));

    let head = app.handle(authorized("HEAD", &read_path("large.bin"))).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header(&head, "content-length"), Some("33554432"));
    assert_eq!(open_handles(&real), 0);

    let download = app.handle(authorized("GET", &read_path("large.bin"))).await;
    assert_eq!(download.status(), StatusCode::OK);
    let mut download_body = download.into_body();
    let frame = download_body.frame().await;
    assert!(matches!(frame, Some(Ok(_))), "the download sends a frame");
    assert_eq!(open_handles(&real), 1);

    drop(download_body);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while open_handles(&real) != 0 {
        assert!(std::time::Instant::now() < deadline, "the dropped download closes its file");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[cfg(target_os = "linux")]
fn open_handles(real: &Path) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| std::fs::read_link(entry.path()).is_ok_and(|target| target == real))
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn serves_downloads_with_explicit_content_headers() {
    let project = temp_directory("fs-download");
    for (name, content) in [
        ("page.html", "<!doctype html><script>document.title = 'x'</script>"),
        ("logo.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
        ("feed.xml", "<feed/>"),
        ("schema.dtd", "<!ELEMENT a EMPTY>"),
        ("app.js", "export {}"),
        ("photo.png", "png"),
        ("clip.mp4", "0123456789"),
        ("naïve 100% (1).txt", "text"),
    ] {
        std::fs::write(project.join(name), content).expect("write the fixture");
    }
    let app = ready_app_with_services(services_at(&project));

    let expect_download = |response: &Response, filename: &str| {
        assert_eq!(
            header(response, "content-disposition"),
            Some(format!("attachment; filename=\"{filename}\"; filename*=UTF-8''{filename}").as_str()),
            "{filename}"
        );
        assert_eq!(
            header(response, "content-security-policy"),
            Some("default-src 'none'; img-src 'self'; media-src 'self'; sandbox")
        );
        assert_eq!(header(response, "x-content-type-options"), Some("nosniff"));
    };

    for (file, content_type) in [
        ("page.html", "text/plain; charset=utf-8"),
        ("logo.svg", "text/plain; charset=utf-8"),
        ("feed.xml", "text/plain; charset=utf-8"),
        ("schema.dtd", "text/plain; charset=utf-8"),
        ("app.js", "text/plain; charset=utf-8"),
        ("photo.png", "image/png"),
        ("clip.mp4", "video/mp4"),
    ] {
        let full = app.handle(authorized("GET", &read_path(file))).await;
        assert_eq!(full.status(), StatusCode::OK, "{file}");
        assert_eq!(header(&full, "content-type"), Some(content_type), "{file}");
        expect_download(&full, file);
        let head = app.handle(authorized("HEAD", &read_path(file))).await;
        assert_eq!(header(&head, "content-type"), Some(content_type), "{file}");
        expect_download(&head, file);
    }

    let html = app.handle(authorized("GET", &read_path("page.html"))).await;
    assert_eq!(body(html).await.as_ref(), b"<!doctype html><script>document.title = 'x'</script>");

    let partial = app.handle(authorized_with("GET", &read_path("clip.mp4"), &[("range", "bytes=2-5")])).await;
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header(&partial, "content-type"), Some("video/mp4"));
    expect_download(&partial, "clip.mp4");
    assert_eq!(body(partial).await.as_ref(), b"2345");

    let cached = app.handle(authorized_with("GET", &read_path("clip.mp4"), &[("if-none-match", "*")])).await;
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    expect_download(&cached, "clip.mp4");

    let named = app.handle(authorized("GET", &read_path("naïve 100% (1).txt"))).await;
    assert_eq!(
        header(&named, "content-disposition"),
        Some("attachment; filename=\"na_ve 100_ (1).txt\"; filename*=UTF-8''na%C3%AFve%20100%25%20%281%29.txt")
    );
}

#[tokio::test]
async fn lists_parents_and_siblings_relative_to_the_location() {
    let root = temp_directory("fs-list");
    let current = root.join("root").join("current");
    std::fs::create_dir_all(&current).expect("create the current directory");
    std::fs::create_dir_all(root.join("root").join("sibling").join("nested")).expect("create the sibling");
    std::fs::write(root.join("root").join("sibling").join("file.txt"), "outside").expect("write the sibling file");
    let app = ready_app_with_services(services_at(&current));

    let listing = app.handle(authorized("GET", &list_path_at(&current, "."))).await;
    assert_eq!(listing.status(), StatusCode::OK);
    let listing = body_json(listing).await;
    assert_eq!(listing["location"]["directory"], current.to_string_lossy().as_ref());

    let sibling = root.join("root").join("sibling");
    let expected = json!([
        { "path": "../sibling/nested/", "type": "directory" },
        { "path": "../sibling/file.txt", "type": "file" },
    ]);
    for path in ["..", "../sibling", "../sibling/nested"] {
        let response = app.handle(authorized("GET", &list_path_at(&current, path))).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let relative = app.handle(authorized("GET", &list_path_at(&current, "../sibling"))).await;
    assert_eq!(relative.status(), StatusCode::OK);
    assert_eq!(body_json(relative).await["data"], expected);
    let absolute = app.handle(authorized("GET", &list_path_at(&current, sibling.to_string_lossy().as_ref()))).await;
    assert_eq!(absolute.status(), StatusCode::OK);
    assert_eq!(body_json(absolute).await["data"], expected);

    let loaded = app.handle(authorized("GET", "/api/debug/location")).await;
    assert_eq!(loaded.status(), StatusCode::OK);
    assert_eq!(body_json(loaded).await, json!([{ "directory": current.to_string_lossy().as_ref() }]));

    for path in ["../sibling/file.txt", "missing"] {
        let missing = app.handle(authorized("GET", &list_path_at(&current, path))).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body_json(missing).await["_tag"], "FileNotFoundError");
    }
}

#[tokio::test]
async fn writes_nested_files_and_finds_them() {
    let project = temp_directory("fs-write");
    let app = ready_app_with_services(services_at(&project));

    let target = project.join("nested").join("new.txt");
    let uri = format!("/api/experimental/fs/write?path={}", opencode_api_types::js::encode_component("nested/new.txt"));
    let request = post_json(&uri, "hello write");
    let written = app.handle(request).await;
    assert_eq!(written.status(), StatusCode::OK);
    let written = body_json(written).await;
    assert_eq!(written["location"]["directory"], project.to_string_lossy().as_ref());
    assert_eq!(written["data"]["path"], target.to_string_lossy().as_ref());
    assert_eq!(std::fs::read_to_string(&target).expect("read the written file"), "hello write");

    let read = app.handle(authorized("GET", &read_path("nested/new.txt"))).await;
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(body(read).await.as_ref(), b"hello write");

    let find_uri = format!("/api/fs/find?query={}", opencode_api_types::js::encode_component("new.txt"));
    let found = app.handle(authorized("GET", &find_uri)).await;
    assert_eq!(found.status(), StatusCode::OK);
    let found = body_json(found).await;
    assert!(
        found["data"].as_array().expect("an array").iter().any(|entry| entry["path"] == "nested/new.txt"),
        "{found}"
    );
}

#[tokio::test]
async fn confines_watch_requests_like_read_and_reports_a_disabled_watcher() {
    let root = temp_directory("fs-watch");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("create the project");
    std::fs::write(project.join("a.ts"), "a").expect("write a.ts");
    std::fs::write(root.join("outside.ts"), "outside").expect("write outside.ts");
    let app = ready_app_with_services(disabled_services(&project));

    let watch_uri = "/api/experimental/fs/watch";
    for (path, expected) in [("../outside.ts", StatusCode::NOT_FOUND), ("missing.ts", StatusCode::NOT_FOUND)] {
        let request = post_json(watch_uri, &format!("{{\"path\":\"{path}\"}}"));
        let response = app.handle(request).await;
        assert_eq!(response.status(), expected, "{path}");
        assert_eq!(body_json(response).await["_tag"], "FileNotFoundError");
    }

    let request = post_json(watch_uri, "{\"path\":\"a.ts\"}");
    let response = app.handle(request).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(response).await["_tag"], "ServiceUnavailableError");

    let body = "{\"path\":\"a.ts\"}";
    let request = with_body(
        authorized_with(
            "DELETE",
            watch_uri,
            &[("content-type", "application/json"), ("content-length", &body.len().to_string())],
        ),
        body,
    );
    assert_eq!(app.handle(request).await.status(), StatusCode::NO_CONTENT);
}

fn disabled_services(start: &Path) -> Arc<GlobalServices> {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let database = Arc::new(database);
    let home = temp_directory("fs-watch-home");
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.clone().into_os_string())).expect("global paths");
    let bus = opencode_bus::Bus::new(database.clone(), opencode_bus::BusOptions::default());
    Arc::new(GlobalServices::new(
        database,
        paths,
        start.to_string_lossy().into_owned(),
        Arc::new(|name: &str| (name == "OPENCODE_DISABLE_FILEWATCHER").then(|| OsString::from("1"))),
        bus,
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
    ))
}
