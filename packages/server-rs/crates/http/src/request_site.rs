use http::{Request, header};

use crate::cors;

pub fn rejected<B>(request: &Request<B>, extra: &[String]) -> bool {
    let site = request.headers().get("sec-fetch-site").and_then(|value| value.to_str().ok());
    if !matches!(site, Some("cross-site" | "same-site")) {
        return false;
    }
    let path = request.uri().path();
    if path != "/api" && !path.starts_with("/api/") {
        return false;
    }
    if is_artifact_view(path) {
        return false;
    }
    if request.headers().get("sec-fetch-mode").and_then(|value| value.to_str().ok()) == Some("navigate") {
        return true;
    }
    let Some(origin) = request.headers().get(header::ORIGIN) else {
        return false;
    };
    let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
    !origin.to_str().is_ok_and(|origin| cors::is_allowed_request_origin(Some(origin), host, extra))
}

fn is_artifact_view(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/experimental/artifacts/") else {
        return false;
    };
    let Some((id, rest)) = rest.split_once('/') else {
        return false;
    };
    !id.is_empty() && (rest == "view" || rest.starts_with("view/"))
}
