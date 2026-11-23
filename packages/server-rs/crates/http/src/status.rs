use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Ready,
    Stopping,
    Failed,
}

impl State {
    fn to_u8(self) -> u8 {
        match self {
            State::Starting => 0,
            State::Ready => 1,
            State::Stopping => 2,
            State::Failed => 3,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => State::Starting,
            1 => State::Ready,
            2 => State::Stopping,
            _ => State::Failed,
        }
    }
}

#[derive(Debug)]
pub struct Status {
    state: AtomicU8,
}

impl Status {
    pub fn new() -> Self {
        Self { state: AtomicU8::new(State::Starting.to_u8()) }
    }

    pub fn current(&self) -> State {
        State::from_u8(self.state.load(Ordering::SeqCst))
    }

    pub fn ready(&self) {
        let _ = self.state.compare_exchange(
            State::Starting.to_u8(),
            State::Ready.to_u8(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn fail(&self) {
        let _ = self.state.compare_exchange(
            State::Starting.to_u8(),
            State::Failed.to_u8(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn begin_stopping(&self) {
        self.state.store(State::Stopping.to_u8(), Ordering::SeqCst);
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}
