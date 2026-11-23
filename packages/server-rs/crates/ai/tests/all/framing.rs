use bytes::Bytes;
use futures_util::StreamExt;

use opencode_ai::route::executor::ByteStream;
use opencode_ai::route::framing::{Definition, FrameStream, document, lines, sse, sse_events, sse_with_done};
use opencode_ai::schema::AiErrorReason;

const SAMPLE: &str = concat!(
    "\u{feff}data: bom\n\n",
    ": keepalive\n\n",
    "event: message\ndata: {\"a\":1}\n\n",
    "data: line1\ndata: line2\n\n",
    "event: done\ndata: [DONE]\n\n",
    "data: null\n\n",
    "data: : keepalive\n\n",
    "id: 7\nretry: 100\ndata: x\r\n\r\n",
    "data: héllo 世界\n\n",
);

fn byte_stream(chunks: Vec<Vec<u8>>) -> ByteStream {
    Box::pin(futures_util::stream::iter(chunks.into_iter().map(|chunk| Ok(Bytes::from(chunk)))))
}

async fn run(definition: &Definition, chunks: Vec<Vec<u8>>) -> Vec<String> {
    let mut frames: FrameStream = definition.frame(byte_stream(chunks));
    let mut output = Vec::new();
    while let Some(frame) = frames.next().await {
        match frame {
            Ok(frame) => output.push(frame),
            Err(error) => panic!("unexpected framing error: {}", error.message()),
        }
    }
    output
}

#[tokio::test]
async fn sse_frames_do_not_depend_on_chunking() {
    let expected = vec![
        "bom".to_string(),
        "{\"a\":1}".to_string(),
        "line1\nline2".to_string(),
        "x".to_string(),
        "héllo 世界".to_string(),
    ];
    assert_eq!(run(&sse(), vec![SAMPLE.as_bytes().to_vec()]).await, expected);
    let bytes = SAMPLE.as_bytes();
    for split in 0..=bytes.len() {
        let chunks = vec![bytes[..split].to_vec(), bytes[split..].to_vec()];
        assert_eq!(run(&sse(), chunks).await, expected, "split at byte {split}");
    }
    let single_byte_chunks: Vec<Vec<u8>> = bytes.iter().map(|byte| vec![*byte]).collect();
    assert_eq!(run(&sse(), single_byte_chunks).await, expected);
}

#[tokio::test]
async fn sse_variants_filter_dropped_frames() {
    assert_eq!(
        run(&sse_with_done(), vec![SAMPLE.as_bytes().to_vec()]).await,
        vec![
            "bom".to_string(),
            "{\"a\":1}".to_string(),
            "line1\nline2".to_string(),
            "[DONE]".to_string(),
            "x".to_string(),
            "héllo 世界".to_string(),
        ]
    );
    assert_eq!(
        run(&sse_events(["message".to_string()]), vec![SAMPLE.as_bytes().to_vec()]).await,
        vec![
            "bom".to_string(),
            "{\"a\":1}".to_string(),
            "line1\nline2".to_string(),
            "x".to_string(),
            "héllo 世界".to_string(),
        ]
    );
    assert!(run(&sse_events(["other".to_string()]), vec![SAMPLE.as_bytes().to_vec()]).await.is_empty());
}

#[tokio::test]
async fn line_frames_split_on_every_line_ending() {
    let sample = "a\nb\r\nc\rd\n\n";
    let expected = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()];
    assert_eq!(run(&lines(), vec![sample.as_bytes().to_vec()]).await, expected);
    let bytes = sample.as_bytes();
    for split in 0..=bytes.len() {
        let chunks = vec![bytes[..split].to_vec(), bytes[split..].to_vec()];
        assert_eq!(run(&lines(), chunks).await, expected, "split at byte {split}");
    }
}

#[tokio::test]
async fn document_frames_emit_the_whole_body() {
    let sample = "héllo 世界".as_bytes();
    assert_eq!(run(&document(), vec![sample.to_vec()]).await, vec!["héllo 世界".to_string()]);
    let single_byte_chunks: Vec<Vec<u8>> = sample.iter().map(|byte| vec![*byte]).collect();
    assert_eq!(run(&document(), single_byte_chunks).await, vec!["héllo 世界".to_string()]);
}

#[tokio::test]
async fn oversized_sse_event_is_invalid_provider_output() {
    let mut frames = sse().frame(byte_stream(vec![vec![b'a'; 10 * 1024 * 1024 + 1]]));
    let error = match frames.next().await {
        Some(Err(error)) => error,
        other => panic!("expected framing error, got {other:?}"),
    };
    match error.reason {
        AiErrorReason::InvalidProviderOutput { route, .. } => assert_eq!(route.as_deref(), Some("sse")),
        other => panic!("expected invalid provider output, got {other:?}"),
    }
}
