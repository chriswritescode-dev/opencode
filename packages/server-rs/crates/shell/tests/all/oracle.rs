use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use opencode_shell::parse;
use opencode_shell::scan::{Dialect, ScanResult, scan};
use serde::Deserialize;

const TIMEOUT: Duration = Duration::from_secs(2);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("opencode-shell-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Oracle {
    root: TempRoot,
    bin: PathBuf,
    target: PathBuf,
    log: PathBuf,
    shells: Vec<PathBuf>,
}

impl Oracle {
    fn new(name: &str) -> Self {
        let root = TempRoot::new(name);
        let bin = root.path().join("bin");
        let target = root.path().join("target");
        fs::create_dir_all(&bin).expect("create probe directory");
        fs::create_dir_all(&target).expect("create target directory");
        fs::write(root.path().join("a="), "").expect("create qualifier target");
        let probe = bin.join("scan_probe");
        fs::write(&probe, "#!/bin/sh\nprintf '%s\\n' \"scan_probe${1:+ $*}\" >> \"$SCAN_PROBE_LOG\"\n")
            .expect("write probe");
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("chmod probe");
        fs::copy(&probe, bin.join("scan_probe[x")).expect("copy probe");
        let log = root.path().join("log");
        Self { root, bin, target, log, shells: discover_shells() }
    }

    fn observe(&self, shell: &Path, source: &str) -> Vec<String> {
        fs::write(&self.log, "").expect("reset probe log");
        let mut command = Process::new(shell);
        if is_named(shell, "bash") {
            command.args(["--noprofile", "--norc"]);
        } else if is_named(shell, "zsh") {
            command.arg("-f");
        }
        command
            .arg("-c")
            .arg(source)
            .current_dir(self.root.path())
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.root.path())
            .env("LC_ALL", "C")
            .env("SCAN_PROBE_LOG", &self.log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        wait(&mut command);
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

fn wait(command: &mut Process) {
    let mut child = command.spawn().expect("start shell");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait().expect("wait for shell").is_some() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn discover_shells() -> Vec<PathBuf> {
    let mut shells = Vec::new();
    for candidate in ["/bin/bash", "/opt/homebrew/bin/bash", "/usr/local/bin/bash", "bash", "zsh", "dash"] {
        let found = if candidate.starts_with('/') {
            Path::new(candidate).is_file().then(|| PathBuf::from(candidate))
        } else {
            which(candidate)
        };
        if let Some(found) = found
            && !shells.contains(&found)
        {
            shells.push(found);
        }
    }
    shells
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[derive(Deserialize)]
struct OracleCase {
    source: String,
    input: String,
}

fn oracle_sources() -> Vec<String> {
    let corpus = fs::read_to_string(format!("{}/tests/corpus/scan-bash.json", env!("CARGO_MANIFEST_DIR")))
        .expect("scan-bash corpus");
    let cases: Vec<OracleCase> = serde_json::from_str(&corpus).expect("scan-bash corpus json");
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for case in cases.into_iter().filter(|case| case.source.contains("oracle.test.ts")) {
        if seen.insert(case.input.clone()) {
            sources.push(case.input);
        }
    }
    sources
}

fn is_named(shell: &Path, name: &str) -> bool {
    shell.file_name().and_then(|file| file.to_str()) == Some(name)
}

#[test]
fn reports_or_rejects_every_probe_a_real_shell_runs() {
    let oracle = Oracle::new("soundness");
    assert!(oracle.shells.iter().any(|shell| is_named(shell, "bash")), "discovers at least bash on PATH");
    let strict = std::env::var("SHELL_ORACLE_STRICT").as_deref() == Ok("1");
    if strict {
        for name in ["bash", "zsh", "dash"] {
            assert!(oracle.shells.iter().any(|shell| is_named(shell, name)), "strict mode requires {name}");
        }
    }
    let sources = oracle_sources();
    assert_eq!(sources.len(), 713, "oracle fixture list");
    let mut failures = Vec::new();
    let mut unexecuted = Vec::new();
    for source in &sources {
        let runs: Vec<(&PathBuf, Vec<String>)> =
            oracle.shells.iter().map(|shell| (shell, oracle.observe(shell, source))).collect();
        if strict && runs.iter().all(|(_, invocations)| invocations.is_empty()) {
            unexecuted.push(source.clone());
        }
        for (dialect, only) in [(Dialect::Bash, Some("bash")), (Dialect::Zsh, Some("zsh")), (Dialect::Posix, None)] {
            let commands = match scan(source, dialect) {
                ScanResult::Scanned { commands } => commands,
                ScanResult::Opaque { .. } => continue,
            };
            let reported: Vec<String> = commands
                .iter()
                .filter(|command| command.declaration != Some(true))
                .map(|command| command.words.join(" "))
                .collect();
            for (shell, invocations) in &runs {
                if only.is_some_and(|only| !is_named(shell, only)) {
                    continue;
                }
                for invocation in invocations {
                    if !reported.contains(invocation) {
                        failures.push(format!(
                            "{} executed {:?} with {dialect:?} in: {source:?}",
                            shell.display(),
                            invocation
                        ));
                    }
                }
            }
        }
    }
    assert!(
        unexecuted.is_empty(),
        "{} fixtures executed scan_probe in no real shell:\n{}",
        unexecuted.len(),
        unexecuted.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
    assert!(
        failures.is_empty(),
        "{} probe calls were not reported:\n{}",
        failures.len(),
        failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn reports_the_directory_a_real_shell_changes_to() {
    let oracle = Oracle::new("directory");
    let target = oracle.target.to_string_lossy().into_owned();
    let real = fs::canonicalize(&oracle.target).expect("real path of target").to_string_lossy().into_owned();
    for template in ["cd >/dev/null TARGET", "cd 2>/dev/null TARGET", "cd $'TARGET'", "cd \"TARGET\""] {
        let source = template.replace("TARGET", &target);
        let observed = format!("{source}\npwd >> \"$SCAN_PROBE_LOG\"");
        let runs: Vec<(&PathBuf, Vec<String>)> =
            oracle.shells.iter().map(|shell| (shell, oracle.observe(shell, &observed))).collect();
        let changed: Vec<&(&PathBuf, Vec<String>)> = runs
            .iter()
            .filter(|(_, lines)| lines.last().is_some_and(|line| line == &target || line == &real))
            .collect();
        assert!(!changed.is_empty(), "Fixture never changed directory in any real shell: {source}");
        for (shell, _) in changed {
            let options = parse::Options::from_env(std::env::vars());
            if let Ok(parsed) =
                parse::scan_with(&source, &shell.to_string_lossy(), &oracle.root.path().to_string_lossy(), &options)
            {
                assert!(parsed.directories.contains(&target), "{} in: {source}", shell.display());
            }
        }
    }
}
