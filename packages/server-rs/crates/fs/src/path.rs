//! Path rules shared with the TypeScript server: containment, lexical resolution and `~` expansion.

use std::borrow::Cow;
use std::path::Path;

use opencode_util::path::{Flavor, realpath};

/// `FSUtil.windowsPath` on this platform: on Windows, MSYS and WSL drives become `C:/`.
pub fn windows_path(path: &str) -> Cow<'_, str> {
    opencode_util::path::windows_path(Flavor::NATIVE, path)
}

/// `FSUtil.contains`: whether `child` stays inside `parent`.
pub fn contains(parent: &str, child: &str) -> bool {
    let flavor = Flavor::NATIVE;
    !flavor.outside(&flavor.relative(&resolve_lexical(parent), &resolve_lexical(child)))
}

/// `FSUtil.resolve`: resolve lexically, then use the real path when it exists.
pub fn resolve(path: &str) -> String {
    let resolved = resolve_lexical(path);
    realpath(Path::new(&resolved)).and_then(|real| real.to_str().map(str::to_owned)).unwrap_or(resolved)
}

fn resolve_lexical(path: &str) -> String {
    let flavor = Flavor::NATIVE;
    let normalized = windows_path(path);
    if flavor.is_absolute(&normalized) {
        return flavor.resolve(&[normalized.as_ref()]);
    }
    let cwd = std::env::current_dir().map(|cwd| cwd.to_string_lossy().into_owned()).unwrap_or_default();
    flavor.resolve(&[&cwd, normalized.as_ref()])
}

/// `FileAccess.resolvePath`: expand a leading `~`, normalize Windows shell paths, then resolve from `directory`.
pub fn resolve_path(directory: &str, input: &str, home: &str) -> String {
    let flavor = Flavor::NATIVE;
    let normalized = windows_path(input);
    let expanded = match normalized.as_ref() {
        "~" => Cow::Borrowed(home),
        value if value.starts_with("~/") || (flavor == Flavor::Windows && value.starts_with("~\\")) => {
            Cow::Owned(flavor.join(&[home, &value[2..]]))
        }
        _ => normalized,
    };
    flavor.resolve(&[directory, expanded.as_ref()])
}
