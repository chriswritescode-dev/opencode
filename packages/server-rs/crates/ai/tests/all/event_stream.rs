use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use futures_util::{StreamExt, stream};

use opencode_ai::protocols::bedrock_event_stream;
use opencode_ai::route::executor::ByteStream;
use opencode_ai::route::framing::FrameStream;
use opencode_ai::schema::{AiError, AiErrorReason, InvalidProviderOutputClassification};

use crate::corpus::Corpus;

fn byte_stream(chunks: Vec<Vec<u8>>) -> ByteStream {
    Box::pin(stream::iter(chunks.into_iter().map(|chunk| Ok(Bytes::from(chunk)))))
}

async fn collect(chunks: Vec<Vec<u8>>) -> Result<Vec<String>, AiError> {
    let mut frames: FrameStream = bedrock_event_stream::framing("bedrock-converse").frame(byte_stream(chunks));
    let mut output = Vec::new();
    while let Some(frame) = frames.next().await {
        match frame {
            Ok(frame) => output.push(frame),
            Err(error) => return Err(error),
        }
    }
    Ok(output)
}

fn bedrock_bodies() -> Vec<Vec<u8>> {
    Corpus::load()
        .cassettes
        .into_iter()
        .filter(|cassette| cassette.protocol == "bedrock-converse")
        .flat_map(|cassette| cassette.steps)
        .flat_map(|step| step.exchanges.unwrap_or_default())
        .filter(|exchange| exchange.response.body_encoding.as_deref() == Some("base64"))
        .map(|exchange| match BASE64_STANDARD.decode(&exchange.response.body) {
            Ok(body) => body,
            Err(error) => panic!("failed to decode recorded Bedrock Converse body: {error}"),
        })
        .collect()
}

fn invalid_provider_output(
    error: AiError,
) -> (String, Option<String>, Option<String>, Option<InvalidProviderOutputClassification>) {
    match error.reason {
        AiErrorReason::InvalidProviderOutput { message, body, route, classification, .. } => {
            (message, body, route, classification)
        }
        other => panic!("unexpected error reason: {other}"),
    }
}

#[tokio::test]
async fn corpus_bedrock_bodies_frame_identically_when_split() {
    let bodies = bedrock_bodies();
    assert!(bodies.len() >= 13, "expected at least 13 recorded Bedrock Converse base64 bodies, found {}", bodies.len());
    for body in bodies {
        let whole = match collect(vec![body.clone()]).await {
            Ok(frames) => frames,
            Err(error) => panic!("whole body failed to frame: {error}"),
        };
        assert!(!whole.is_empty(), "recorded body produced no frames");
        let single_byte_chunks = body.iter().map(|byte| vec![*byte]).collect::<Vec<Vec<u8>>>();
        let split = match collect(single_byte_chunks).await {
            Ok(frames) => frames,
            Err(error) => panic!("single byte chunking failed to frame: {error}"),
        };
        assert_eq!(whole, split);
    }
}

#[tokio::test]
async fn corrupted_checksum_is_an_error() {
    let mut body = bedrock_bodies().remove(0);
    assert!(body.len() >= 16, "recorded body is too short to hold a frame");
    body[8] ^= 0xff;
    let error = collect(vec![body]).await.expect_err("corrupted prelude checksum should not frame");
    let (message, body, route, classification) = invalid_provider_output(error);
    assert_eq!(route.as_deref(), Some("bedrock-converse"));
    assert_eq!(classification, None);
    assert!(body.is_some());
    assert!(
        message.starts_with("Failed to decode Bedrock Converse event-stream frame"),
        "unexpected message: {message}"
    );
}

#[tokio::test]
async fn truncated_stream_reports_incomplete() {
    let mut body = bedrock_bodies().remove(0);
    body.pop();
    let error = collect(vec![body]).await.expect_err("truncated stream should not frame");
    let (message, body, route, classification) = invalid_provider_output(error);
    assert_eq!(route.as_deref(), Some("bedrock-converse"));
    assert_eq!(classification, Some(InvalidProviderOutputClassification::IncompleteStream));
    assert!(body.is_some());
    assert!(message.starts_with("Incomplete Bedrock Converse event-stream frame:"), "unexpected message: {message}");
}
