use std::collections::HashSet;
use std::fmt::Write;
use std::time::UNIX_EPOCH;

use tokio::io::AsyncWriteExt;

use crate::error::{Operation, OperationError};
use crate::repo::Repository;
use crate::store::{join, now_millis};
use crate::{Git, RunOptions};

const LOOSE_OBJECT_LIMIT: usize = 2048;
const PACK_COUNT_LIMIT: usize = 16;
const STALE_MILLIS: u128 = 60 * 60 * 1000;

struct LooseObject {
    oid: String,
    file: String,
}

impl Git {
    pub async fn objects_pack(&self, repository: &Repository) -> Result<(), OperationError> {
        let objects = join(&repository.git_directory, "objects");
        let pack_directory = join(&objects, "pack");
        let loose = loose_objects(&objects).await;
        let packs = local_packs(&pack_directory).await;
        let combine_packs = packs.len() >= PACK_COUNT_LIMIT;
        if loose.len() < LOOSE_OBJECT_LIMIT && !combine_packs {
            return Ok(());
        }
        self.with_pack_lock(repository, async || {
            sweep_temporary_packs(&pack_directory).await;
            let combined = if combine_packs {
                let mut combined = Vec::with_capacity(packs.len());
                for pack in &packs {
                    let index = join(&pack_directory, &format!("{pack}.idx"));
                    combined.push((pack.clone(), self.pack_index(repository, &index).await?));
                }
                combined
            } else {
                Vec::new()
            };
            let mut seen = HashSet::new();
            let mut object_ids = Vec::new();
            let loose_ids = loose.iter().map(|item| item.oid.clone());
            let packed_ids = combined.iter().flat_map(|(_, oids)| oids.iter().cloned());
            for oid in loose_ids.chain(packed_ids) {
                if seen.insert(oid.clone()) {
                    object_ids.push(oid);
                }
            }
            if object_ids.is_empty() {
                return Ok(());
            }
            let prefix = join(&pack_directory, "pack");
            let output = self
                .repository_operation(
                    Operation::Pack,
                    repository,
                    &[
                        "-c",
                        "pack.threads=2",
                        "-c",
                        "pack.windowMemory=64m",
                        "pack-objects",
                        "-q",
                        "--non-empty",
                        &prefix,
                    ],
                    RunOptions {
                        stdin: Some(format!("{}\n", object_ids.join("\n")).into_bytes()),
                        ..Default::default()
                    },
                )
                .await?;
            let new_packs = output
                .text()
                .split('\n')
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            let mut packed_ids = HashSet::new();
            for name in &new_packs {
                let index = join(&pack_directory, &format!("pack-{name}.idx"));
                packed_ids.extend(self.pack_index(repository, &index).await?);
            }
            let missing = object_ids.iter().filter(|oid| !packed_ids.contains(oid.as_str())).count();
            if missing > 0 {
                return Err(OperationError {
                    operation: Operation::Pack,
                    directory: Some(repository.git_directory.clone()),
                    message: format!(
                        "Packed {} objects but {} are missing; nothing was removed",
                        packed_ids.len(),
                        missing
                    ),
                    cause: None,
                });
            }
            for item in &loose {
                let _ = tokio::fs::remove_file(&item.file).await;
            }
            let new_names = new_packs.iter().map(|name| format!("pack-{name}")).collect::<HashSet<_>>();
            for (pack, _) in &combined {
                if new_names.contains(pack) {
                    continue;
                }
                for extension in [".idx", ".pack", ".rev", ".bitmap", ".mtimes"] {
                    let _ = tokio::fs::remove_file(join(&pack_directory, &format!("{pack}{extension}"))).await;
                }
            }
            Ok(())
        })
        .await
    }

    async fn pack_index(&self, repository: &Repository, file: &str) -> Result<Vec<String>, OperationError> {
        let bytes = tokio::fs::read(file).await.map_err(|error| OperationError {
            operation: Operation::Pack,
            directory: Some(repository.git_directory.clone()),
            message: format!("Failed to read {file}"),
            cause: Some(error.to_string()),
        })?;
        if let Some(ids) = pack_index_object_ids(&bytes) {
            return Ok(ids);
        }
        let output = self
            .repository_operation(
                Operation::Pack,
                repository,
                &["show-index"],
                RunOptions { stdin: Some(bytes), ..Default::default() },
            )
            .await?;
        Ok(output.text().split('\n').filter_map(|line| line.split(' ').nth(1)).map(str::to_string).collect())
    }

    async fn with_pack_lock(
        &self,
        repository: &Repository,
        effect: impl AsyncFnOnce() -> Result<(), OperationError>,
    ) -> Result<(), OperationError> {
        let file = join(&repository.git_directory, "opencode-pack.lock");
        let stale = match tokio::fs::metadata(&file).await {
            Ok(metadata) => {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or(0);
                modified < now_millis().saturating_sub(STALE_MILLIS)
            }
            Err(_) => false,
        };
        if stale {
            let _ = tokio::fs::remove_file(&file).await;
        }
        let acquired = match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&file).await {
            Ok(mut handle) => {
                let _ = handle.write_all(std::process::id().to_string().as_bytes()).await;
                true
            }
            Err(_) => false,
        };
        if !acquired {
            return Ok(());
        }
        let result = effect().await;
        let _ = tokio::fs::remove_file(&file).await;
        result
    }
}

async fn loose_objects(objects: &str) -> Vec<LooseObject> {
    let Ok(mut reader) = tokio::fs::read_dir(objects).await else { return Vec::new() };
    let mut fanout = Vec::new();
    while let Ok(Some(entry)) = reader.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_lower_hex(&name, 2) {
            fanout.push(name);
        }
    }
    let mut listed = Vec::new();
    for prefix in fanout {
        let directory = join(objects, &prefix);
        let Ok(mut reader) = tokio::fs::read_dir(&directory).await else { continue };
        while let Ok(Some(entry)) = reader.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_lower_hex(&name, 38) || is_lower_hex(&name, 62) {
                listed.push(LooseObject { oid: format!("{prefix}{name}"), file: join(&directory, &name) });
            }
        }
    }
    listed
}

async fn local_packs(directory: &str) -> Vec<String> {
    let Ok(mut reader) = tokio::fs::read_dir(directory).await else { return Vec::new() };
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = reader.next_entry().await {
        entries.push(entry.file_name().to_string_lossy().into_owned());
    }
    let present = entries.iter().map(String::as_str).collect::<HashSet<_>>();
    entries
        .iter()
        .filter_map(|entry| {
            if !entry.starts_with("pack-") || !entry.ends_with(".pack") {
                return None;
            }
            let pack = entry.strip_suffix(".pack")?;
            if !present.contains(format!("{pack}.idx").as_str())
                || present.contains(format!("{pack}.keep").as_str())
                || present.contains(format!("{pack}.promisor").as_str())
            {
                return None;
            }
            Some(pack.to_string())
        })
        .collect()
}

async fn sweep_temporary_packs(directory: &str) {
    let Ok(mut reader) = tokio::fs::read_dir(directory).await else { return };
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = reader.next_entry().await {
        entries.push(entry.file_name().to_string_lossy().into_owned());
    }
    let indexed = entries.iter().filter_map(|entry| entry.strip_suffix(".idx")).collect::<HashSet<_>>();
    let cutoff = now_millis().saturating_sub(STALE_MILLIS);
    for entry in &entries {
        let temporary = entry.starts_with("tmp_")
            || (entry.starts_with("pack-")
                && entry.ends_with(".pack")
                && !indexed.contains(entry.strip_suffix(".pack").unwrap_or_default()));
        if !temporary {
            continue;
        }
        let path = join(directory, entry);
        let modified = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        if modified < cutoff {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

fn pack_index_object_ids(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() < 1032 {
        return None;
    }
    if u32::from_be_bytes(bytes.get(0..4)?.try_into().ok()?) != 0xff744f63 {
        return None;
    }
    if u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) != 2 {
        return None;
    }
    let count = u32::from_be_bytes(bytes.get(1028..1032)?.try_into().ok()?) as usize;
    let remaining = bytes.len().checked_sub(1032)?.checked_sub(count.checked_mul(8)?)?;
    let divisor = count.checked_add(2)?;
    if remaining % divisor != 0 {
        return None;
    }
    let hash = remaining / divisor;
    if hash != 20 && hash != 32 {
        return None;
    }
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let start = 1032usize.checked_add(index.checked_mul(hash)?)?;
        let chunk = bytes.get(start..start.checked_add(hash)?)?;
        let mut id = String::with_capacity(hash * 2);
        for byte in chunk {
            let _ = write!(id, "{byte:02x}");
        }
        ids.push(id);
    }
    Some(ids)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
