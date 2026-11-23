use std::io::Read;

use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use http::{HeaderValue, Request, StatusCode, header};
use opencode_http::{compression, response};

use crate::{UNAUTHORIZED_BODY, authorized, body, ready_app, request, text};

fn decompress(encoding: &str, bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    match encoding {
        "gzip" => {
            GzDecoder::new(bytes).read_to_end(&mut output).expect("gzip decompression");
        }
        "deflate" => {
            ZlibDecoder::new(bytes).read_to_end(&mut output).expect("deflate decompression");
        }
        "br" => {
            brotli::BrotliDecompress(&mut &bytes[..], &mut output).expect("brotli decompression");
        }
        other => panic!("unexpected content encoding {other}"),
    }
    output
}

fn accepting(mut request: Request<Bytes>, encoding: &'static str) -> Request<Bytes> {
    request.headers_mut().insert(header::ACCEPT_ENCODING, HeaderValue::from_static(encoding));
    request
}

fn header_value(response: &response::Response, name: header::HeaderName) -> Option<&str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

fn long_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "items": ["x".repeat(2048)] })).expect("json")
}

#[tokio::test]
async fn gzip_brotli_and_deflate_round_trip_the_served_openapi_document() {
    let app = ready_app();
    let document = opencode_contract::SERVED_OPENAPI_JSON.as_bytes();
    for encoding in ["gzip", "br", "deflate"] {
        let response = app.handle(accepting(authorized("GET", "/openapi.json"), encoding)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_value(&response, header::CONTENT_ENCODING), Some(encoding));
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());
        assert_eq!(header_value(&response, header::VARY), Some("Origin, Accept-Encoding"));
        let compressed = body(response).await;
        assert!(compressed.len() < document.len(), "{encoding} did not reduce the document");
        assert_eq!(decompress(encoding, &compressed).as_slice(), document);
    }
}

#[tokio::test]
async fn server_preference_and_negotiation_match_effect() {
    let app = ready_app();
    let cases: [(&'static str, Option<&'static str>); 9] = [
        ("gzip, br", Some("br")),
        ("gzip, deflate", Some("gzip")),
        ("deflate", Some("deflate")),
        ("*", Some("br")),
        ("gzip;q=0, *;q=0.5", Some("br")),
        ("br;q=0, gzip;q=0.5", Some("gzip")),
        ("gzip;q=0", None),
        ("identity", None),
        ("gzip;q=2", None),
    ];
    for (accept, expected) in cases {
        let response = app.handle(accepting(authorized("GET", "/openapi.json"), accept)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_value(&response, header::CONTENT_ENCODING), expected, "{accept}");
        if expected.is_none() {
            assert_eq!(header_value(&response, header::VARY), Some("Origin, Accept-Encoding"), "{accept}");
        }
    }
}

#[tokio::test]
async fn small_bodies_are_left_uncompressed_but_still_vary() {
    let app = ready_app();
    let response = app.handle(accepting(request("GET", "/api/info"), "gzip")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
    assert_eq!(header_value(&response, header::VARY), Some("Origin, Accept-Encoding"));
    assert_eq!(text(response).await, UNAUTHORIZED_BODY);
}

#[tokio::test]
async fn no_transform_disables_compression() {
    for cache_control in ["no-transform", "public, no-transform; max-age=60"] {
        let mut response = response::bytes(StatusCode::OK, "application/json", vec![b'x'; 2048]);
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_control));
        let applied = compression::apply(Some("gzip"), response);
        assert!(applied.headers().get(header::CONTENT_ENCODING).is_none(), "{cache_control}");
        assert_eq!(body(applied).await, Bytes::from(vec![b'x'; 2048]), "{cache_control}");
    }
    let payload = long_json();
    let mut response = response::bytes(StatusCode::OK, "application/json", payload.clone());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let applied = compression::apply(Some("gzip"), response);
    assert_eq!(header_value(&applied, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(decompress("gzip", &body(applied).await), payload);
}

#[tokio::test]
async fn non_compressible_content_types_skip_negotiation_entirely() {
    let response = response::bytes(StatusCode::OK, "image/png", vec![0u8; 2048]);
    let applied = compression::apply(Some("gzip"), response);
    assert!(applied.headers().get(header::CONTENT_ENCODING).is_none());
    assert!(applied.headers().get(header::VARY).is_none());
}

#[tokio::test]
async fn identity_is_stripped_without_compression_and_strong_etags_are_weakened() {
    let payload = long_json();
    let mut response = response::bytes(StatusCode::OK, "application/json", payload.clone());
    response.headers_mut().insert(header::ETAG, HeaderValue::from_static("\"abc\""));
    response.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("identity"));
    let applied = compression::apply(Some("gzip"), response);
    assert!(applied.headers().get(header::CONTENT_ENCODING).is_none());
    assert_eq!(header_value(&applied, header::ETAG), Some("\"abc\""));
    assert!(applied.headers().get(header::VARY).is_none());
    assert_eq!(body(applied).await, Bytes::from(payload.clone()));

    let mut response = response::bytes(StatusCode::OK, "application/json", payload.clone());
    response.headers_mut().insert(header::ETAG, HeaderValue::from_static("\"abc\""));
    let applied = compression::apply(Some("gzip"), response);
    assert_eq!(header_value(&applied, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(header_value(&applied, header::ETAG), Some("W/\"abc\""));
    assert_eq!(decompress("gzip", &body(applied).await), payload);

    let mut response = response::bytes(StatusCode::OK, "application/json", payload.clone());
    response.headers_mut().insert(header::ETAG, HeaderValue::from_static("W/\"abc\""));
    response.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    let applied = compression::apply(Some("gzip"), response);
    assert_eq!(header_value(&applied, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(header_value(&applied, header::ETAG), Some("W/\"abc\""));
    assert_eq!(body(applied).await, Bytes::from(payload));
}

#[tokio::test]
async fn empty_and_204_bodies_are_not_compressed() {
    let mut response = response::empty(StatusCode::NO_CONTENT);
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let applied = compression::apply(Some("gzip"), response);
    assert_eq!(applied.status(), StatusCode::NO_CONTENT);
    assert!(applied.headers().get(header::CONTENT_ENCODING).is_none());

    let mut response = response::empty(StatusCode::OK);
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let applied = compression::apply(Some("gzip"), response);
    assert!(applied.headers().get(header::CONTENT_ENCODING).is_none());
}
