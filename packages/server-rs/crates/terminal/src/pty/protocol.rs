pub const REPLAY_CHUNK: usize = 64 * 1024;

pub fn meta_frame(cursor: i64) -> Vec<u8> {
    let mut frame = serde_json::json!({ "cursor": cursor }).to_string().into_bytes();
    frame.insert(0, 0);
    frame
}

pub fn chunks(data: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (index, character) in data.char_indices() {
        if units >= REPLAY_CHUNK {
            out.push(data[start..index].to_owned());
            start = index;
            units = 0;
        }
        units += character.len_utf16();
    }
    if start < data.len() {
        out.push(data[start..].to_owned());
    }
    out
}

pub fn decode_input(message: &[u8]) -> Option<String> {
    std::str::from_utf8(message).ok().map(str::to_owned)
}
