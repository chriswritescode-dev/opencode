use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opencode_local::registration::Registration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Replaced,
    Unreadable,
}

pub fn register(file: &Path, registration: &Registration) -> io::Result<()> {
    let parent = file.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = PathBuf::from(format!("{}.{}.tmp", file.display(), registration.id.as_deref().unwrap_or_default()));
    let text = serde_json::to_string(registration).map_err(io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&temp)?.write_all(text.as_bytes())?;
    rename_with_retry(&temp, file)
}

fn rename_with_retry(temp: &Path, file: &Path) -> io::Result<()> {
    let mut error = match std::fs::rename(temp, file) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(10));
        match std::fs::rename(temp, file) {
            Ok(()) => return Ok(()),
            Err(next) => error = next,
        }
    }
    Err(error)
}

pub fn ownership(file: &Path, registration: &Registration) -> Ownership {
    match opencode_local::registration::read(file) {
        Some(found) if found == *registration => Ownership::Owned,
        Some(_) => Ownership::Replaced,
        None => Ownership::Unreadable,
    }
}

pub fn remove_if_owned(file: &Path, registration: &Registration) {
    if ownership(file, registration) == Ownership::Owned {
        let _ = std::fs::remove_file(file);
    }
}
