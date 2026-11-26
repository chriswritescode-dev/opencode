use std::collections::HashSet;
use std::fs::{FileTimes, OpenOptions};
use std::path::Path;
use std::time::{Duration, SystemTime};

use opencode_shell::retention;
use opencode_util::temp::TempDir;

#[test]
fn removes_expired_output_files_across_projects() {
    let dir = TempDir::new().expect("a temporary directory");
    let data = dir.path();
    let first = data.join("shell").join("first");
    let second = data.join("shell").join("second");
    std::fs::create_dir_all(&first).expect("the first project directory");
    std::fs::create_dir_all(&second).expect("the second project directory");
    let old = first.join("sh_0123456789abABCDEFGHIJKLMN.out");
    let recent = second.join("sh_0123456789abNOPQRSTUVWXYZ0.out");
    let unrelated = first.join("notes.out");
    std::fs::write(&old, "old").expect("an expired output file");
    std::fs::write(&recent, "recent").expect("a recent output file");
    std::fs::write(&unrelated, "unrelated").expect("an unrelated file");
    let expired = SystemTime::now().checked_sub(Duration::from_secs(8 * 24 * 60 * 60)).expect("eight days ago");
    set_modified(&old, expired);
    set_modified(&unrelated, expired);

    retention::cleanup(data, &HashSet::new()).expect("the sweep runs");

    assert!(!old.exists());
    assert!(recent.exists());
    assert!(unrelated.exists());
}

#[test]
fn keeps_an_expired_file_that_a_daemon_still_reports_running() {
    let dir = TempDir::new().expect("a temporary directory");
    let data = dir.path();
    let project = data.join("shell").join("project");
    std::fs::create_dir_all(&project).expect("the project directory");
    let running = project.join("sh_0123456789ab.out");
    std::fs::write(&running, "running").expect("a running output file");
    let expired = SystemTime::now().checked_sub(Duration::from_secs(8 * 24 * 60 * 60)).expect("eight days ago");
    set_modified(&running, expired);

    retention::cleanup(data, &HashSet::from([running.clone()])).expect("the sweep runs");

    assert!(running.exists());
}

fn set_modified(path: &Path, modified: SystemTime) {
    let file = OpenOptions::new().write(true).open(path).expect("the file opens");
    file.set_times(FileTimes::new().set_modified(modified)).expect("the mtime is set");
}
