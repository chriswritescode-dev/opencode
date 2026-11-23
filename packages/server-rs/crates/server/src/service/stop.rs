use std::path::Path;
use std::time::Duration;

use opencode_local::registration::{self, Registration};

use super::ensure::Pool;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_ATTEMPTS: usize = 100;

pub async fn stop(file: &Path) -> Result<(), String> {
    let Some(registration) = registration::read(file) else {
        return Ok(());
    };
    terminate(file, &registration, None).await
}

pub(super) async fn terminate(file: &Path, registration: &Registration, pool: Option<&Pool>) -> Result<(), String> {
    if !crate::registration::is_same(file, registration) {
        return Ok(());
    }
    signal(registration.pid, Signal::Term);
    if !stopped(registration.pid, pool).await {
        signal(registration.pid, Signal::Kill);
        if !stopped(registration.pid, pool).await {
            return Err(format!("Server process {} is still running", registration.pid));
        }
    }
    crate::registration::remove_if_same(file, registration);
    Ok(())
}

#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

fn signal(pid: u32, signal: Signal) {
    #[cfg(unix)]
    {
        let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else { return };
        let raw = match signal {
            Signal::Term => rustix::process::Signal::TERM,
            Signal::Kill => rustix::process::Signal::KILL,
        };
        let _ = rustix::process::kill_process(pid, raw);
    }
    #[cfg(windows)]
    {
        let _ = signal;
        let _ = opencode_util::win::terminate(pid);
    }
}

fn running(pid: u32) -> bool {
    #[cfg(unix)]
    return i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
    #[cfg(windows)]
    return opencode_util::win::process_alive(pid);
}

async fn stopped(pid: u32, pool: Option<&Pool>) -> bool {
    for attempt in 0..=POLL_ATTEMPTS {
        if !running(pid) || pool.is_some_and(|pool| pool.exited(pid)) {
            return true;
        }
        if attempt < POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    false
}
