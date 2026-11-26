use std::time::{Duration, SystemTime};

use futures_util::StreamExt;

pub async fn cleanup(files: &[String], retention: Duration) {
    cleanup_owned(files.to_vec(), retention, SystemTime::now()).await;
}

pub async fn cleanup_at(files: &[String], retention: Duration, now: SystemTime) {
    cleanup_owned(files.to_vec(), retention, now).await;
}

async fn cleanup_owned(files: Vec<String>, retention: Duration, now: SystemTime) {
    let cutoff = now.checked_sub(retention).unwrap_or(SystemTime::UNIX_EPOCH);
    futures_util::stream::iter(files.into_iter().map(|file| async move {
        let metadata = match tokio::fs::metadata(&file).await {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        let Ok(mtime) = metadata.modified() else { return };
        if mtime >= cutoff {
            return;
        }
        if metadata.is_dir() {
            let _ = tokio::fs::remove_dir_all(&file).await;
        } else {
            let _ = tokio::fs::remove_file(&file).await;
        }
    }))
    .buffer_unordered(8)
    .for_each(|()| async {})
    .await;
}
