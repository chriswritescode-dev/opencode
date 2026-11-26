use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencode_bus::{Bus, BusOptions};
use opencode_config::shell::{Search, ShellSelect};
use opencode_contract::generated::{AbsolutePath, LocationRef, ShellInfoStatus};
use opencode_shell::daemon_pipe::DaemonCommands;
use opencode_shell::output::Limits;
use opencode_shell::service::{Options, Shell};
use opencode_storage::{Database, DatabaseLocation};
use opencode_terminal::persistent::{self, PersistentPty};
use opencode_util::temp::TempDir;
use tokio::time::sleep;

fn binary() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("OPENCODE_PTY_BIN").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(configured));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join("opencode-pty")).find(|candidate| candidate.is_file())
}

fn environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn bus(root: &Path) -> Arc<Bus> {
    let (database, _) = Database::open(DatabaseLocation::Memory, root).expect("open the database");
    Bus::new(Arc::new(database), BusOptions::default())
}

fn pty(
    root: &Path,
    id: &str,
    inherited: Option<opencode_terminal::daemon::Handoff>,
    env: &[(String, String)],
) -> Arc<PersistentPty> {
    PersistentPty::new(
        persistent::Options {
            root: root.to_owned(),
            id: id.to_owned(),
            env: env.to_vec(),
            inherited,
            default_command: "/bin/sh".to_owned(),
        },
        bus(root),
    )
}

async fn shell(root: &Path, directory: &Path, data: &Path, commands: Arc<DaemonCommands>) -> Arc<Shell> {
    let select = Arc::new(ShellSelect::new(
        Search { bin: PathBuf::from("/usr/bin"), path: std::env::var_os("PATH"), gitbash: None },
        |name| std::env::var_os(name),
    ));
    Shell::new(
        bus(root),
        Options {
            location: LocationRef {
                directory: AbsolutePath(directory.to_string_lossy().into_owned()),
                workspace_id: None,
            },
            project_id: "prj_restart".to_owned(),
            data: data.to_owned(),
            env: environment(),
            shell: select,
            limits: Arc::new(Limits::default),
            daemon: Some(commands),
        },
    )
    .await
}

async fn wait_for_output(file: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_to_string(file).is_ok_and(|output| output.contains(expected)) {
            return;
        }
        assert!(Instant::now() < deadline, "the command wrote no output in time");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn takes_a_command_back_from_the_daemon_after_a_handoff() {
    if binary().is_none() {
        println!("skipping the restart test: set OPENCODE_PTY_BIN or install opencode-pty");
        return;
    }
    let dir = TempDir::new().expect("a temporary directory");
    let root = dir.path().join("state");
    let data = dir.path().join("data");
    let directory = dir.path().join("work");
    std::fs::create_dir_all(&directory).expect("the working directory");
    let env = environment();

    let first_pty = pty(&root, "server-a", None, &env);
    let first_commands = DaemonCommands::new(first_pty.clone(), bus(&root));
    let first = shell(&root, &directory, &data, first_commands).await;
    let info = first
        .create(
            opencode_shell::service::CreateInput {
                command: "printf first; sleep 1; printf second".to_owned(),
                cwd: None,
                timeout: None,
                metadata: None,
                shell: Some("/bin/sh".to_owned()),
                tool_call: None,
                waited: false,
            },
            |_| {},
        )
        .await
        .expect("the daemon runs the command");
    assert_eq!(info.status, ShellInfoStatus::Running);
    wait_for_output(Path::new(&info.file), "first").await;

    let handoff = first_pty.handoff().await.expect("prepare the handoff").expect("a prepared handoff");
    assert!(first.handoff_prepared().await);
    drop(first);
    drop(first_pty);

    let second_pty = pty(&root, "server-b", Some(handoff), &env);
    let second_commands = DaemonCommands::new(second_pty.clone(), bus(&root));
    let second = shell(&root, &directory, &data, second_commands).await;
    let recovered = second.get(&info.id.0).expect("the command was taken back");
    assert_eq!(recovered.command, info.command);
    assert_eq!(recovered.file, info.file);
    assert!(matches!(recovered.status, ShellInfoStatus::Running | ShellInfoStatus::Exited));

    let done = tokio::time::timeout(Duration::from_secs(10), second.wait(&info.id.0))
        .await
        .expect("the command ends")
        .expect("the command is known");
    assert_eq!(done.status, ShellInfoStatus::Exited);
    assert_eq!(done.exit, Some(0.0));
    let result = second.result(done, None).await;
    assert_eq!(result.capture.expect("the output file is known").output, "firstsecond");

    second_pty.shutdown().await.expect("the daemon stops");
}
