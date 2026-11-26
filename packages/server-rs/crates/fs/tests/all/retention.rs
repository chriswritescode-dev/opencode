use std::time::{Duration, SystemTime};

use opencode_fs::retention::cleanup_at;

use crate::fixture::TempDir;

#[tokio::test]
async fn deletes_only_files_older_than_the_retention_window() {
    let temp = TempDir::new("retention");
    let now = SystemTime::now();
    let old = temp.path().join("old.txt");
    let fresh = temp.path().join("fresh.txt");
    std::fs::write(&old, "old").unwrap();
    std::fs::write(&fresh, "fresh").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(now - Duration::from_secs(2 * 60 * 60))
        .unwrap();

    cleanup_at(
        &[old.to_string_lossy().into_owned(), fresh.to_string_lossy().into_owned()],
        Duration::from_secs(60 * 60),
        now,
    )
    .await;

    assert!(!old.exists());
    assert!(fresh.exists());
}
