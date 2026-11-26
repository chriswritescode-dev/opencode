use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::BoxFuture;
use image::ImageEncoder;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::ConfigEntryDocumentInfo;
use opencode_tool::image::{Image, ImageError, Limits};
use opencode_tool::{
    Content, Snapshot, ToolCall, ToolOptions, ToolOutput, ToolRegistration, ToolRegistry, ToolRuntime, ToolSpec,
};

fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> String {
    let source = ::image::RgbaImage::from_fn(width, height, |x, y| ::image::Rgba(pixel(x, y)));
    let mut buffer = Vec::new();
    ::image::codecs::png::PngEncoder::new(&mut buffer)
        .write_image(source.as_raw(), source.width(), source.height(), ::image::ExtendedColorType::Rgba8)
        .expect("the fixture PNG encodes");
    STANDARD.encode(buffer)
}

fn opaque(x: u32, y: u32) -> [u8; 4] {
    [(x * 7 + y * 3) as u8, (x ^ y) as u8, (x * y) as u8, 255]
}

fn output(content: &str) -> ::image::DynamicImage {
    ::image::load_from_memory(&STANDARD.decode(content).expect("the normalized base64 decodes"))
        .expect("the normalized image decodes")
}

fn document(json: &str) -> Entry {
    Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(json).expect("the fixture config decodes"),
        origin: Origin::File,
    })
}

#[test]
fn small_png_is_unchanged() {
    let content = png(32, 32, opaque);
    let result = opencode_tool::image::normalize("fixture.png", "image/png", &content, Limits::default())
        .expect("the fixture decodes");
    assert!(result.is_none(), "a small PNG passes through unchanged");
}

#[test]
fn oversize_image_without_auto_resize_reports_its_size() {
    let content = png(10, 10, opaque);
    let limits = Limits { auto_resize: false, max_width: 5, ..Limits::default() };
    let error = opencode_tool::image::normalize("fixture.png", "image/png", &content, limits)
        .expect_err("an oversize image that cannot resize fails");
    assert_eq!(
        error,
        ImageError::Size {
            resource: "fixture.png".to_owned(),
            width: 10,
            height: 10,
            bytes: content.len(),
            max_width: 5,
            max_height: 2_000,
            max_bytes: 5 * 1024 * 1024,
        }
    );
}

#[test]
fn resize_keeps_aspect_and_fits_the_limits() {
    let content = png(400, 200, opaque);
    let limits = Limits { max_width: 100, max_height: 100, ..Limits::default() };
    let normalized = opencode_tool::image::normalize("fixture.png", "image/png", &content, limits)
        .expect("the fixture decodes")
        .expect("an oversize image is resized");
    assert_eq!(normalized.mime, "image/png");
    let image = output(&normalized.content);
    assert_eq!((image.width(), image.height()), (100, 50));
    assert!(normalized.content.len() <= limits.max_base64_bytes);
}

#[test]
fn resize_uses_the_exact_ladder_dimensions() {
    let content = png(300, 101, opaque);
    let limits = Limits { max_width: 200, max_height: 200, ..Limits::default() };
    let normalized = opencode_tool::image::normalize("fixture.png", "image/png", &content, limits)
        .expect("the fixture decodes")
        .expect("an oversize image is resized");
    let image = output(&normalized.content);
    assert_eq!(
        (image.width(), image.height()),
        (200, 67),
        "each ladder size is used exactly, without the aspect-ratio re-rounding of DynamicImage::resize"
    );
}

#[test]
fn transparent_png_stays_png() {
    let content = png(400, 400, |x, y| if x < 200 { [x as u8, y as u8, 0, 0] } else { opaque(x, y) });
    let limits = Limits { max_width: 200, max_height: 200, ..Limits::default() };
    let normalized = opencode_tool::image::normalize("fixture.png", "image/png", &content, limits)
        .expect("the fixture decodes")
        .expect("an oversize image is resized");
    assert_eq!(normalized.mime, "image/png", "a transparent image is not converted to JPEG");
    let image = output(&normalized.content);
    assert_eq!((image.width(), image.height()), (200, 200));
}

#[test]
fn undecodable_bytes_report_a_decode_error() {
    let error = opencode_tool::image::normalize("fixture.png", "image/png", "bm90IGFuIGltYWdl", Limits::default())
        .expect_err("unreadable image bytes fail");
    assert_eq!(error, ImageError::Decode { resource: "fixture.png".to_owned() });
}

struct Files {
    content: Vec<Content>,
}

impl ToolRuntime for Files {
    fn execute<'a>(
        &'a self,
        _input: serde_json::Value,
        _context: &'a opencode_tool::ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, opencode_tool::ToolError>> {
        let content = self.content.clone();
        Box::pin(async move { Ok(ToolOutput { output: serde_json::Value::Null, content, metadata: None }) })
    }
}

async fn snapshot(content: Vec<Content>) -> Snapshot {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(move |editor| {
            editor.add(ToolRegistration {
                spec: ToolSpec {
                    name: "files".to_owned(),
                    description: Some("Return files".to_owned()),
                    ..ToolSpec::default()
                },
                options: ToolOptions { codemode: serde_json::Value::Bool(false), ..ToolOptions::default() },
                runtime: Arc::new(Files { content: content.clone() }),
            });
        })
        .await;
    registry.snapshot(&[], &Default::default()).await.expect("the snapshot builds")
}

async fn execute(snapshot: &Snapshot) -> ToolOutput {
    snapshot
        .execute(
            ToolCall { id: "call".to_owned(), name: "files".to_owned(), input: serde_json::json!({}) },
            &Default::default(),
        )
        .await
        .expect("the tool executes")
}

#[tokio::test]
async fn runtime_replaces_a_normalized_image_data_uri() {
    let content = png(32, 32, opaque);
    let result = execute(
        &snapshot(vec![Content::File {
            uri: format!("data:image/png;base64,{content}"),
            mime: "image/png".to_owned(),
            name: Some("fixture.png".to_owned()),
        }])
        .await,
    )
    .await;
    assert_eq!(
        result.content,
        vec![Content::File {
            uri: format!("data:image/png;base64,{content}"),
            mime: "image/png".to_owned(),
            name: Some("fixture.png".to_owned()),
        }]
    );
}

#[tokio::test]
async fn runtime_drops_images_it_cannot_decode_or_resize() {
    let broken = Content::File {
        uri: "data:image/png;base64,bm90IGFuIGltYWdl".to_owned(),
        mime: "image/png".to_owned(),
        name: Some("broken.png".to_owned()),
    };
    let large = Content::File {
        uri: format!("data:image/png;base64,{}", png(10, 10, opaque)),
        mime: "image/png".to_owned(),
        name: Some("large.png".to_owned()),
    };
    let snapshot = snapshot(vec![broken, large.clone()]).await;
    snapshot.image.configure(&[document(r#"{"media":{"image":{"auto_resize":false,"max_width":5}}}"#)]);
    let result = execute(&snapshot).await;
    assert_eq!(
        result.content,
        vec![
            Content::text("[1 image omitted: could not be decoded.]"),
            Content::text("[1 image omitted: could not be resized below the image size limit.]"),
        ]
    );
}

#[test]
fn configure_folds_every_document_and_resets_to_the_defaults() {
    let image = Image::new();
    image.configure(&[
        document(r#"{"media":{"image":{"max_width":100,"max_height":200}}}"#),
        document(r#"{"media":{"image":{"auto_resize":false,"max_base64_bytes":1024}}}"#),
    ]);
    assert_eq!(image.limits(), Limits { auto_resize: false, max_width: 100, max_height: 200, max_base64_bytes: 1024 });
    image.configure(&[document("{}")]);
    assert_eq!(image.limits(), Limits::default());
}
