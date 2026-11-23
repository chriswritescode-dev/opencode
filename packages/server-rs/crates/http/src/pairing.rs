use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use opencode_contract::generated::PairingCode;

use crate::Clock;
use crate::expiry::ExpiringMap;

pub const CODE_TTL_SECONDS: u64 = 300;

const CAPACITY: usize = 1_000;
const TTL_MS: u64 = CODE_TTL_SECONDS * 1_000;

pub struct Pairing {
    codes: ExpiringMap<()>,
}

impl Pairing {
    pub fn new() -> Self {
        Self::with_clock(crate::clock())
    }

    pub fn with_clock(clock: Clock) -> Self {
        Self { codes: ExpiringMap::new(CAPACITY, TTL_MS, clock) }
    }

    pub fn issue(&self) -> PairingCode {
        let code = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());
        self.codes.insert(code.clone(), ());
        PairingCode { code, expires_in: CODE_TTL_SECONDS as f64 }
    }

    pub fn consume(&self, code: &str) -> bool {
        self.codes.take(code, |_| true)
    }
}

impl Default for Pairing {
    fn default() -> Self {
        Self::new()
    }
}
