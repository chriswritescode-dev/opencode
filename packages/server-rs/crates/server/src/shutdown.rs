use std::time::Duration;

pub const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
pub enum Signal {
    Interrupt,
    Terminate,
}

impl Signal {
    pub fn exit_code(self) -> u8 {
        match self {
            Signal::Interrupt => 130,
            Signal::Terminate => 143,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Signal::Interrupt => "SIGINT",
            Signal::Terminate => "SIGTERM",
        }
    }
}

pub struct Watch(platform::Watch);

impl Watch {
    pub fn new() -> std::io::Result<Self> {
        platform::Watch::new().map(Self)
    }

    pub async fn next(&mut self) -> Signal {
        self.0.next().await
    }
}

pub async fn teardown<F>(watch: &mut Watch, signal: Signal, cleanup: F) -> std::process::ExitCode
where
    F: std::future::Future<Output = ()>,
{
    tokio::select! {
        () = cleanup => std::process::ExitCode::from(signal.exit_code()),
        () = tokio::time::sleep(DEADLINE) => {
            eprintln!("shutdown did not finish within {} seconds; exiting.", DEADLINE.as_secs());
            std::process::ExitCode::from(1)
        }
        again = watch.next() => {
            eprintln!("received {} again during shutdown; exiting.", again.name());
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::io::IsTerminal;

    use tokio::signal::unix;

    use super::Signal;

    pub struct Watch {
        interrupt: unix::Signal,
        terminate: unix::Signal,
        hangup: Option<unix::Signal>,
    }

    impl Watch {
        pub fn new() -> std::io::Result<Self> {
            let terminal = std::io::stdin().is_terminal() || std::io::stdout().is_terminal();
            Ok(Self {
                interrupt: unix::signal(unix::SignalKind::interrupt())?,
                terminate: unix::signal(unix::SignalKind::terminate())?,
                hangup: terminal.then(|| unix::signal(unix::SignalKind::hangup())).transpose()?,
            })
        }

        pub async fn next(&mut self) -> Signal {
            tokio::select! {
                _ = self.interrupt.recv() => Signal::Interrupt,
                _ = self.terminate.recv() => Signal::Terminate,
                _ = hangup(&mut self.hangup) => Signal::Terminate,
            }
        }
    }

    async fn hangup(signal: &mut Option<unix::Signal>) {
        match signal {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use super::Signal;

    pub struct Watch;

    impl Watch {
        pub fn new() -> std::io::Result<Self> {
            Ok(Self)
        }

        pub async fn next(&mut self) -> Signal {
            let _ = tokio::signal::ctrl_c().await;
            Signal::Interrupt
        }
    }
}
