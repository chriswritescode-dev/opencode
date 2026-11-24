use bytes::Bytes;
use opencode_sidecar::frame::{self, Broken, DEFAULT_MAX_FRAME, Frame};
use opencode_sidecar::protocol::Message;

fn ping() -> Message {
    Message::Ping { seq: 7 }
}

#[tokio::test]
async fn complete_frames_survive_every_stream_cut() {
    let variants: [Option<&[u8]>; 2] = [None, Some(b"attached")];
    for bytes in variants {
        let encoded = frame::encode(&ping(), bytes, DEFAULT_MAX_FRAME).expect("a frame");
        let first = frame::encode(&Message::Pong { seq: 1, rss: 0 }, None, DEFAULT_MAX_FRAME).expect("a frame");
        for cut in 0..=encoded.len() {
            let stream = [first.as_slice(), &encoded[..cut]].concat();
            let mut input = stream.as_slice();
            assert_eq!(
                frame::read(&mut input, DEFAULT_MAX_FRAME).await,
                Ok(Some(Frame { message: Message::Pong { seq: 1, rss: 0 }, bytes: None }))
            );
            match cut {
                0 => assert_eq!(frame::read(&mut input, DEFAULT_MAX_FRAME).await, Ok(None)),
                cut if cut == encoded.len() => {
                    assert_eq!(
                        frame::read(&mut input, DEFAULT_MAX_FRAME).await,
                        Ok(Some(Frame { message: ping(), bytes: bytes.map(Bytes::copy_from_slice) }))
                    );
                    assert_eq!(frame::read(&mut input, DEFAULT_MAX_FRAME).await, Ok(None));
                }
                got => assert_eq!(
                    frame::read(&mut input, DEFAULT_MAX_FRAME).await,
                    Err(Broken::Truncated { got, want: if got < 5 { 5 } else { encoded.len() } })
                ),
            }
        }
    }
}

#[tokio::test]
async fn caps_and_kinds_are_checked_before_the_payload() {
    let cases: [(u8, Option<&[u8]>); 2] = [(frame::JSON, None), (frame::BINARY, Some(b"bytes"))];
    for (kind, bytes) in cases {
        let encoded = frame::encode(&ping(), bytes, DEFAULT_MAX_FRAME).expect("a frame");
        let len = encoded.len() - 5;
        let mut input = encoded.as_slice();
        assert!(frame::read(&mut input, len).await.expect("a frame").is_some());
        for over in [len as u32 + 1, u32::MAX] {
            let header = [over.to_le_bytes().as_slice(), &[kind]].concat();
            assert_eq!(
                frame::read(&mut header.as_slice(), len).await,
                Err(Broken::TooLarge { kind, len: over as usize })
            );
        }
    }
    for kind in 2..=u8::MAX {
        let header = [0, 0, 0, 0, kind];
        assert_eq!(frame::read(&mut header.as_slice(), DEFAULT_MAX_FRAME).await, Err(Broken::UnknownKind(kind)));
    }
}

#[test]
fn encode_refuses_a_message_over_the_cap() {
    let error = frame::encode(&ping(), Some(&[0u8; 64]), 16).expect_err("a frame over the cap");
    assert!(matches!(error, frame::FrameError::TooLarge { max: 16, .. }));
}

#[tokio::test]
async fn a_binary_frame_round_trips_its_bytes() {
    let message = Message::Chunk { id: 3, value: serde_json::json!({ "ok": true }) };
    let encoded = frame::encode(&message, Some(b"raw"), DEFAULT_MAX_FRAME).expect("a frame");
    let mut input = encoded.as_slice();
    let got = frame::read(&mut input, DEFAULT_MAX_FRAME).await.expect("a frame").expect("a frame");
    assert_eq!(got.message, message);
    assert_eq!(got.bytes.as_deref(), Some(&b"raw"[..]));
    assert_eq!(frame::read(&mut input, DEFAULT_MAX_FRAME).await, Ok(None));
}

#[tokio::test]
async fn malformed_json_gives_bad_json() {
    let payload = b"not json";
    let mut stream = Vec::new();
    stream.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    stream.push(frame::JSON);
    stream.extend_from_slice(payload);
    let result = frame::read(&mut stream.as_slice(), DEFAULT_MAX_FRAME).await;
    assert!(matches!(result, Err(Broken::BadJson(_))));
}
