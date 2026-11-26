use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use opencode_registry::watch::watch_files;

use crate::TempDir;

fn receives(receiver: &mpsc::Receiver<()>, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return false;
        }
        if matches!(receiver.recv_timeout(timeout - elapsed), Ok(())) {
            return true;
        }
    }
}

fn later() -> impl Fn(&str) -> Option<String> {
    |_| None
}

#[test]
fn sees_file_changes() {
    let dir = TempDir::new();
    let file = dir.path().join("instructions.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([file.clone()], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, "two").expect("modify the file");
    assert!(receives(&receiver, Duration::from_secs(1)));
}

#[test]
fn sees_a_created_missing_directory() {
    let dir = TempDir::new();
    let file = dir.path().join("missing").join("sub").join("file.md");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([file.clone()], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("create the directory");
    assert!(receives(&receiver, Duration::from_secs(1)));
}

#[test]
fn reports_each_atomic_save() {
    let dir = TempDir::new();
    let file = dir.path().join("SKILL.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([file.clone()], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    for content in ["two", "three"] {
        let temporary = dir.path().join("SKILL.md.tmp");
        std::fs::write(&temporary, content).expect("write the temporary file");
        std::fs::rename(&temporary, &file).expect("rename over the watched file");
        assert!(receives(&receiver, Duration::from_secs(1)), "atomic save {content}");
    }
}

#[test]
fn ignores_an_unrelated_sibling_of_a_watched_file() {
    let dir = TempDir::new();
    let file = dir.path().join("SKILL.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([file], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    std::fs::write(dir.path().join("unrelated.md"), "two").expect("write an unrelated sibling");
    assert!(!receives(&receiver, Duration::from_millis(500)));
}

#[test]
fn ignores_an_unrelated_sibling_of_a_missing_path() {
    let dir = TempDir::new();
    let file = dir.path().join("missing").join("sub").join("file.md");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([file], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    std::fs::write(dir.path().join("unrelated.md"), "two").expect("write an unrelated sibling");
    assert!(!receives(&receiver, Duration::from_millis(500)));
}

#[cfg(unix)]
#[test]
fn watches_a_symlink_target() {
    let dir = TempDir::new();
    let real = dir.path().join("real.md");
    let link = dir.path().join("link.md");
    std::fs::write(&real, "one").expect("write the real file");
    std::os::unix::fs::symlink(&real, &link).expect("create the symlink");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files([link], later(), move || {
        let _ = sender.send(());
    });
    thread::sleep(Duration::from_millis(50));
    std::fs::write(&real, "two").expect("modify the real file");
    assert!(receives(&receiver, Duration::from_secs(1)));
}

#[test]
fn does_nothing_when_disabled() {
    let dir = TempDir::new();
    let file = dir.path().join("instructions.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files(
        [file.clone()],
        |name| (name == "OPENCODE_FILEWATCHER_DISABLE").then(|| "1".to_string()),
        move || {
            let _ = sender.send(());
        },
    );
    thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, "two").expect("modify the file");
    assert!(!receives(&receiver, Duration::from_millis(300)));
}

#[test]
fn a_true_disable_value_beats_a_missing_first_variable() {
    let dir = TempDir::new();
    let file = dir.path().join("instructions.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files(
        [file.clone()],
        |name| (name == "OPENCODE_DISABLE_FILEWATCHER").then(|| "TRUE".to_string()),
        move || {
            let _ = sender.send(());
        },
    );
    thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, "two").expect("modify the file");
    assert!(!receives(&receiver, Duration::from_millis(300)));
}

#[test]
fn a_false_disable_value_still_watches() {
    let dir = TempDir::new();
    let file = dir.path().join("instructions.md");
    std::fs::write(&file, "one").expect("write the file");
    let (sender, receiver) = mpsc::channel();
    let _watch = watch_files(
        [file.clone()],
        |_| Some("0".to_string()),
        move || {
            let _ = sender.send(());
        },
    );
    thread::sleep(Duration::from_millis(50));
    std::fs::write(&file, "two").expect("modify the file");
    assert!(receives(&receiver, Duration::from_secs(1)));
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_inside_a_runtime() {
    let dir = TempDir::new();
    let file = dir.path().join("instructions.md");
    std::fs::write(&file, "one").expect("write the file");
    let handle = watch_files([file], later(), || {});
    tokio::spawn(async move { drop(handle) }).await.expect("the drop task completes");
}
