use opencode_contract::codec::decode_json;
use opencode_terminal::Info;
use opencode_terminal::protocol::{REPLAY_CHUNK, chunks, decode_input, meta_frame};
use serde_json::json;

#[test]
fn drops_invalid_binary_input_frames_and_decodes_valid_ones() {
    assert_eq!(decode_input(b"ready").as_deref(), Some("ready"));
    assert_eq!(decode_input(&[0xff, 0xfe, 0xfd]), None);
    assert_eq!(decode_input("hello".as_bytes()).as_deref(), Some("hello"));
}

#[test]
fn encodes_the_cursor_as_a_nul_prefixed_json_control_frame() {
    let frame = meta_frame(42);
    assert_eq!(frame[0], 0);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&frame[1..]).expect("json"), json!({ "cursor": 42 }));
}

#[test]
fn splits_replay_into_bounded_frames() {
    assert!(chunks("").is_empty());
    assert_eq!(chunks("abc"), ["abc"]);
    let big = "x".repeat(REPLAY_CHUNK + 1);
    let frames = chunks(&big);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].encode_utf16().count(), REPLAY_CHUNK);
    assert_eq!(frames.concat(), big);
}

#[test]
fn splits_replay_on_utf16_code_units() {
    let data = "é".repeat(REPLAY_CHUNK + 3);
    let frames = chunks(&data);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].encode_utf16().count(), REPLAY_CHUNK);
    assert_eq!(frames.concat(), data);
}

fn sample(pid: i64) -> serde_json::Value {
    json!({
        "id": "pty_01J5Y5H0AH4Q4NXJ6P4C3P5V2K",
        "title": "demo",
        "command": "cmd.exe",
        "args": [],
        "cwd": "C:\\",
        "status": "running",
        "pid": pid,
    })
}

#[test]
fn info_accepts_pid_zero_and_positive_pids_and_rejects_negative_pids() {
    assert_eq!(decode_json::<Info>(&sample(0).to_string()).expect("pid 0").pid, 0.0);
    assert_eq!(decode_json::<Info>(&sample(48012).to_string()).expect("positive pid").pid, 48012.0);
    assert!(decode_json::<Info>(&sample(-1).to_string()).is_err());
}

#[test]
fn info_accepts_an_exit_code_for_retained_exited_sessions() {
    let mut value = sample(48012);
    value["status"] = json!("exited");
    value["exitCode"] = json!(4);
    assert_eq!(decode_json::<Info>(&value.to_string()).expect("exited").exit_code, Some(4.0));
}
