pub const HEARTBEAT: &str = ": heartbeat\n\n";

pub fn data_frame(json: &str) -> String {
    format!("data: {}\n\n", json.replace('\n', "\ndata: "))
}

pub fn event_frame(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {}\n\n", data.replace('\n', "\ndata: "))
}

#[cfg(test)]
mod tests {
    use super::{HEARTBEAT, data_frame, event_frame};

    #[test]
    fn heartbeat_is_a_comment_frame() {
        assert_eq!(HEARTBEAT, ": heartbeat\n\n");
    }

    #[test]
    fn data_frame_wraps_json() {
        assert_eq!(data_frame("{\"type\":\"log.synced\"}"), "data: {\"type\":\"log.synced\"}\n\n");
    }

    #[test]
    fn data_frame_splits_embedded_newlines() {
        assert_eq!(data_frame("{\"a\":1}\n{\"b\":2}"), "data: {\"a\":1}\ndata: {\"b\":2}\n\n");
    }

    #[test]
    fn event_frame_names_the_event() {
        assert_eq!(event_frame("message", "{\"a\":1}"), "event: message\ndata: {\"a\":1}\n\n");
    }
}
