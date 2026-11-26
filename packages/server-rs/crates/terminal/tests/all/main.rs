mod protocol;
#[cfg(unix)]
mod pty_session;

#[cfg(unix)]
pub use support::{Harness, which};

#[cfg(unix)]
mod support {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};

    use opencode_bus::{Bus, BusError, BusOptions, Event, ListenerHandle};
    use opencode_config::shell::{Search, ShellSelect};
    use opencode_contract::generated::{AbsolutePath, LocationRef};
    use opencode_storage::{Database, DatabaseLocation};
    use opencode_terminal::pty::{Options, Pty};
    use opencode_util::temp::TempDir;

    pub struct Harness {
        _dir: TempDir,
        pub bus: Arc<Bus>,
        pub pty: Arc<Pty>,
        pub shell: Arc<ShellSelect>,
        pub location: LocationRef,
    }

    impl Harness {
        pub fn new(directory: &str) -> Self {
            let dir = TempDir::new().expect("a temporary directory");
            let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
            let bus = Bus::new(Arc::new(database), BusOptions::default());
            let shell = Arc::new(ShellSelect::new(
                Search { bin: PathBuf::from("/usr/bin"), path: std::env::var_os("PATH"), gitbash: None },
                |name| std::env::var_os(name),
            ));
            let location = LocationRef { directory: AbsolutePath(directory.to_owned()), workspace_id: None };
            let pty = Pty::new(bus.clone(), shell.clone(), Options { location: location.clone(), env: environment() });
            Harness { _dir: dir, bus, pty, shell, location }
        }

        pub fn events(&self) -> (Receiver<(String, serde_json::Value)>, ListenerHandle) {
            let (sender, receiver) = channel();
            let handle = self.bus.listen(Box::new(move |event: &Event| -> Result<(), BusError> {
                sender.send((event.r#type().to_owned(), event.data().clone())).ok();
                Ok(())
            }));
            (receiver, handle)
        }
    }

    fn environment() -> Vec<(String, String)> {
        std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect::<Vec<(String, String)>>()
    }

    pub fn which(name: &str) -> Option<String> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|directory| {
                let candidate = directory.join(name);
                candidate.is_file().then(|| candidate.to_string_lossy().into_owned())
            })
        })
    }
}
