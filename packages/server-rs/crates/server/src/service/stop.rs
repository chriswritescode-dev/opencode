use std::path::Path;
use std::time::Duration;

use opencode_local::registration::{self, Registration};
use rustix::process::{Pid, Signal};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_ATTEMPTS: usize = 100;

pub async fn stop(file: &Path) {
    let Some(registration) = registration::read(file) else {
        return;
    };
    terminate(file, &registration).await;
}

pub(crate) async fn terminate(file: &Path, registration: &Registration) {
    if !registration::read(file).is_some_and(|current| current.same(registration)) {
        return;
    }
    if let Some(pid) = i32::try_from(registration.pid).ok().and_then(Pid::from_raw) {
        let _ = rustix::process::kill_process(pid, Signal::TERM);
        if !stopped(pid).await {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
            let _ = stopped(pid).await;
        }
    }
    if registration::read(file).is_some_and(|latest| latest.same(registration)) {
        let _ = std::fs::remove_file(file);
    }
}

async fn stopped(pid: Pid) -> bool {
    for attempt in 0..=POLL_ATTEMPTS {
        if rustix::process::test_kill_process(pid).is_err() {
            return true;
        }
        if attempt < POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    false
}
