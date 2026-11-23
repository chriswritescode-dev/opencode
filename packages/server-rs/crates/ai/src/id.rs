use std::sync::atomic::{AtomicU64, Ordering};

use ring::rand::{SecureRandom, SystemRandom};

pub trait IdSource: Send + Sync {
    fn uuid(&self) -> String;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RandomIds;

impl IdSource for RandomIds {
    fn uuid(&self) -> String {
        let mut bytes = [0u8; 16];
        SystemRandom::new().fill(&mut bytes).expect("system randomness is available");
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        )
    }
}

#[derive(Debug, Default)]
pub struct SequenceIds {
    counter: AtomicU64,
}

impl SequenceIds {
    pub fn new() -> Self {
        Self { counter: AtomicU64::new(0) }
    }

    pub fn reset(&self) {
        self.counter.store(0, Ordering::SeqCst);
    }
}

impl IdSource for SequenceIds {
    fn uuid(&self) -> String {
        let value = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        format!("00000000-0000-4000-8000-{value:012}")
    }
}
