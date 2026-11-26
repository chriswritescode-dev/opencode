use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const EXITED_LIMIT: usize = 25;
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const DIRECTORY: &str = "shell";

pub fn output_directory(data: &Path, project_id: &str) -> PathBuf {
    data.join(DIRECTORY).join(project_id)
}

pub fn is_output_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 3 + 12 + 4 || !name.starts_with("sh_") || !name.ends_with(".out") {
        return false;
    }
    bytes[3..15].iter().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn cleanup(data: &Path, running: &HashSet<PathBuf>) -> std::io::Result<()> {
    cleanup_before(data, running, SystemTime::now().checked_sub(RETENTION).unwrap_or(SystemTime::UNIX_EPOCH))
}

fn cleanup_before(data: &Path, running: &HashSet<PathBuf>, cutoff: SystemTime) -> std::io::Result<()> {
    let root = data.join(DIRECTORY);
    let projects = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_output_name(name) || running.contains(&file.path()) {
                continue;
            }
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if modified >= cutoff {
                continue;
            }
            let _ = std::fs::remove_file(file.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_shell_output_names() {
        assert!(is_output_name("sh_0123456789ab.out"));
        assert!(is_output_name("sh_0123456789abABCDEFGHIJKLMN.out"));
        assert!(!is_output_name("sh_0123456789AB.out"));
        assert!(!is_output_name("sh_0123456789a.out"));
        assert!(!is_output_name("notes.out"));
    }
}
