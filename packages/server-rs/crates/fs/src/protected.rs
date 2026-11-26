use crate::path;

pub const DARWIN_HOME: &[&str] =
    &["Music", "Pictures", "Movies", "Downloads", "Desktop", "Documents", "Public", "Applications", "Library"];

pub const WIN32_HOME: &[&str] =
    &["AppData", "Downloads", "Desktop", "Documents", "Pictures", "Music", "Videos", "OneDrive"];

pub fn is_home(directory: &str, home: &str) -> bool {
    path::resolve(directory) == path::resolve(home)
}

pub fn names() -> &'static [&'static str] {
    match std::env::consts::OS {
        "macos" => DARWIN_HOME,
        "windows" => WIN32_HOME,
        _ => &[],
    }
}
