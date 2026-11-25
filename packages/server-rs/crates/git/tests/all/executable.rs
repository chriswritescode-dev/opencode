use std::cell::Cell;
use std::path::absolute;

use opencode_git::{Platform, darwin_developer_dirs, resolve_git_executable};

#[test]
fn uses_an_absolute_executable_on_windows() {
    let expected = absolute("bin/git.exe").expect("resolves the absolute path");
    assert_eq!(
        resolve_git_executable(Platform::Windows, Some("bin/git.exe"), &[], |_| false),
        expected.to_string_lossy()
    );
}

#[test]
fn uses_a_non_shim_path_executable_on_macos_without_probing_developer_directories() {
    let calls = Cell::new(0);
    let resolved = resolve_git_executable(
        Platform::Darwin,
        Some("/opt/homebrew/bin/git"),
        &["/Library/Developer/CommandLineTools".to_string()],
        |_| {
            calls.set(calls.get() + 1);
            true
        },
    );
    assert_eq!(resolved, "/opt/homebrew/bin/git");
    assert_eq!(calls.get(), 0);
}

#[test]
fn resolves_the_apple_shim_on_macos_from_developer_directories() {
    let dirs =
        ["/Applications/Xcode.app/Contents/Developer".to_string(), "/Library/Developer/CommandLineTools".to_string()];
    let resolved = resolve_git_executable(Platform::Darwin, Some("/usr/bin/git"), &dirs, |file| {
        file == "/Library/Developer/CommandLineTools/usr/bin/git"
    });
    assert_eq!(resolved, "/Library/Developer/CommandLineTools/usr/bin/git");
}

#[test]
fn falls_back_to_path_lookup_on_macos_when_no_developer_tools_exist_or_the_candidate_is_invalid() {
    let dirs =
        ["/Applications/Xcode.app/Contents/Developer".to_string(), "/Library/Developer/CommandLineTools".to_string()];
    assert_eq!(resolve_git_executable(Platform::Darwin, Some("/usr/bin/git"), &dirs, |_| false), "git");
    assert_eq!(
        resolve_git_executable(Platform::Darwin, Some("/usr/bin/git"), &["relative/dir".to_string()], |_| true),
        "git"
    );
    assert_eq!(resolve_git_executable(Platform::Darwin, Some("/usr/bin/git"), &["/".to_string()], |_| true), "git");
    assert_eq!(
        resolve_git_executable(Platform::Darwin, None, &["/Library/Developer/CommandLineTools".to_string()], |_| true),
        "git"
    );
}

#[test]
fn orders_macos_developer_directory_candidates_by_developer_dir_then_xcode_select_link() {
    assert_eq!(
        darwin_developer_dirs(Some("/Custom/Developer"), |_| Some("/Library/Developer/CommandLineTools".to_string())),
        ["/Custom/Developer"]
    );
    assert_eq!(
        darwin_developer_dirs(Some(""), |_| Some("/Applications/Xcode-beta.app/Contents/Developer".to_string())),
        [
            "/Applications/Xcode-beta.app/Contents/Developer",
            "/Applications/Xcode.app/Contents/Developer",
            "/Library/Developer/CommandLineTools"
        ]
    );
    assert_eq!(
        darwin_developer_dirs(None, |_| None),
        ["/Applications/Xcode.app/Contents/Developer", "/Library/Developer/CommandLineTools"]
    );
}

#[test]
fn leaves_lookup_to_the_process_on_linux() {
    assert_eq!(resolve_git_executable(Platform::Linux, Some("/usr/bin/git"), &[], |_| true), "git");
}

#[test]
fn falls_back_to_path_lookup_when_git_is_unresolved() {
    assert_eq!(resolve_git_executable(Platform::Windows, None, &[], |_| false), "git");
}
