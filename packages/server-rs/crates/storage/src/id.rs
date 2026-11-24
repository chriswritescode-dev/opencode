use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static STATE: Mutex<(i64, i64)> = Mutex::new((0, 0));

pub fn ascending() -> String {
    create(false, now_ms())
}

pub fn descending() -> String {
    create(true, now_ms())
}

pub fn create(descending: bool, timestamp: i64) -> String {
    let current = {
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        if timestamp != state.0 {
            state.0 = timestamp;
            state.1 = 0;
        }
        state.1 += 1;
        timestamp.wrapping_mul(0x1000).wrapping_add(state.1)
    };
    let value = if descending { !current } else { current };
    let mut id = String::with_capacity(26);
    for index in 0..6 {
        id.push_str(&format!("{:02x}", (value >> (40 - 8 * index)) & 0xff));
    }
    for byte in rand::random::<[u8; 14]>() {
        id.push(CHARS[usize::from(byte % 62)] as char);
    }
    id
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("the system clock is after the Unix epoch").as_millis() as i64
}
