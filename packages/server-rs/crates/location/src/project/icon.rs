use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_storage::StorageError;
use opencode_storage::project::{ProjectRow, set_icon_url};
use serde::Deserialize;

const MAX_BYTES: u64 = 64 * 1024;
const MAX_URL: usize = 64 * 1024;
const IMAGES: [&str; 6] = [".svg", ".png", ".ico", ".jpg", ".jpeg", ".webp"];
const CANDIDATES: [&str; 52] = [
    "Assets.xcassets/AppIcon.appiconset",
    "ios/App/Assets.xcassets/AppIcon.appiconset",
    "assets/app-icon.svg",
    "assets/app-icon.png",
    "build/icon.svg",
    "build/icon.png",
    "src-tauri/icons/icon.svg",
    "src-tauri/icons/icon.png",
    "resources/icon.svg",
    "resources/icon.png",
    "icons/icon.svg",
    "icons/icon.png",
    "extension/icons/icon.svg",
    "extension/icons/icon.png",
    "public/favicon.svg",
    "app/favicon.svg",
    "src/app/favicon.svg",
    "site/public/favicon.svg",
    "favicon.svg",
    "public/icon.svg",
    "app/icon.svg",
    "src/app/icon.svg",
    "site/public/icon.svg",
    "icon.svg",
    "public/favicon.png",
    "app/favicon.png",
    "src/app/favicon.png",
    "site/public/favicon.png",
    "favicon.png",
    "public/apple-touch-icon.png",
    "app/apple-touch-icon.png",
    "src/app/apple-touch-icon.png",
    "site/public/apple-touch-icon.png",
    "apple-touch-icon.png",
    "public/icon.png",
    "app/icon.png",
    "src/app/icon.png",
    "site/public/icon.png",
    "icon.png",
    "public/favicon.ico",
    "app/favicon.ico",
    "src/app/favicon.ico",
    "site/public/favicon.ico",
    "favicon.ico",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    "public/logo.svg",
    "public/logo.png",
    "logo.svg",
    "logo.png",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    pub path: String,
    pub url: String,
}

pub async fn discover(directory: &str) -> Option<Icon> {
    let root = tokio::fs::canonicalize(directory).await.ok()?;
    let mut names: Vec<String> = CANDIDATES.iter().map(|name| (*name).to_string()).collect();
    names.push("AppIcon.icon".to_string());
    names.push("app/AppIcon.icon".to_string());
    names.push("assets/AppIcon.icon".to_string());
    names.extend(catalogs(&root).await);
    names.push("resources/windows".to_string());
    for name in names {
        let Ok(candidate) = tokio::fs::canonicalize(root.join(&name)).await else {
            continue;
        };
        if candidate == root || !candidate.starts_with(&root) {
            continue;
        }
        if name.ends_with(".icon") {
            if let Some(icon) = composer_icon(&root, &candidate).await {
                return Some(icon);
            }
            continue;
        }
        let windows = name == "resources/windows";
        let files = if name.ends_with(".appiconset") || windows {
            let mut entries: Vec<String> = directory_entries(&candidate)
                .await
                .into_iter()
                .filter(|entry| {
                    entry.kind == EntryKind::File
                        && if windows {
                            windows_digits(&entry.name).is_some()
                        } else {
                            extension(Path::new(&entry.name))
                                .is_some_and(|extension| IMAGES.contains(&extension.as_str()))
                        }
                })
                .map(|entry| entry.name)
                .collect();
            entries.sort();
            entries.sort_by(|a, b| rank(a, windows).partial_cmp(&rank(b, windows)).unwrap_or(Ordering::Equal));
            entries.truncate(128);
            entries.into_iter().map(|entry| candidate.join(entry)).collect()
        } else {
            vec![candidate.clone()]
        };
        for file in files {
            let Some((file, bytes)) = read(&root, &file, &IMAGES).await else {
                continue;
            };
            let url = data_url(&file, &bytes);
            if url.len() <= MAX_URL {
                return Some(make_icon(&root, &file, url));
            }
        }
    }
    None
}

pub async fn refresh(database: &opencode_storage::Database, id: &str) -> Result<Option<ProjectRow>, StorageError> {
    let Some(project) = database.read(|connection| opencode_storage::project::get(connection, id))? else {
        return Ok(None);
    };
    if id == crate::project::GLOBAL_ID {
        return Ok(None);
    }
    let Some(image) = discover(&project.worktree).await else {
        return Ok(None);
    };
    if project.icon_url.as_deref() == Some(image.url.as_str()) {
        return Ok(None);
    }
    database.read(|connection| set_icon_url(connection, id, &image.url))
}

async fn composer_icon(root: &Path, candidate: &Path) -> Option<Icon> {
    let (_, manifest) = read(root, &candidate.join("icon.json"), &[".json"]).await?;
    let config: ComposerConfig = serde_json::from_str(&String::from_utf8_lossy(&manifest)).ok()?;
    for layer in config.groups.iter().flat_map(|group| &group.layers) {
        let Some(name) = layer.image_name.as_deref() else {
            continue;
        };
        let Some((file, bytes)) = read(root, &candidate.join("Assets").join(name), &[".svg", ".png"]).await else {
            continue;
        };
        let plain = data_url(&file, &bytes);
        let url = if file.extension().and_then(|extension| extension.to_str()) == Some("svg") {
            backdrop(&plain, config.fill.as_ref())
        } else {
            plain.clone()
        };
        if url.len() <= MAX_URL {
            return Some(make_icon(root, &file, url));
        }
        if plain.len() <= MAX_URL {
            return Some(make_icon(root, &file, plain));
        }
    }
    None
}

async fn read(root: &Path, name: &Path, extensions: &[&str]) -> Option<(PathBuf, Vec<u8>)> {
    let file = tokio::fs::canonicalize(name).await.ok()?;
    if file == root || !file.starts_with(root) {
        return None;
    }
    let extension = extension(&file)?;
    if !extensions.contains(&extension.as_str()) {
        return None;
    }
    let metadata = tokio::fs::metadata(&file).await.ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(&file).await.ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    Some((file, bytes))
}

async fn catalogs(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = directory_entries(root)
        .await
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::Directory && !entry.name.starts_with('.'))
        .map(|entry| entry.name)
        .collect();
    names.sort();
    names.truncate(128);
    names.into_iter().map(|name| format!("{name}/Assets.xcassets/AppIcon.appiconset")).collect()
}

async fn directory_entries(directory: &Path) -> Vec<Entry> {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut result = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let kind = match entry.file_type().await {
            Ok(file_type) if file_type.is_dir() => EntryKind::Directory,
            Ok(file_type) if file_type.is_file() => EntryKind::File,
            _ => EntryKind::Other,
        };
        result.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), kind });
    }
    result
}

fn data_url(file: &Path, bytes: &[u8]) -> String {
    format!("data:{};base64,{}", mime(file), STANDARD.encode(bytes))
}

fn mime(file: &Path) -> &'static str {
    match extension(file).as_deref() {
        Some(".ico") => "image/x-icon",
        Some(".svg") => "image/svg+xml",
        Some(".png") => "image/png",
        Some(".jpg") | Some(".jpeg") => "image/jpeg",
        Some(".webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn backdrop(url: &str, fill: Option<&ComposerFill>) -> String {
    let colors: Vec<Option<String>> = match fill {
        Some(fill) if fill.solid.as_deref().is_some_and(|solid| !solid.is_empty()) => {
            vec![fill.solid.as_deref().and_then(parse_color)]
        }
        Some(fill) => fill.linear_gradient.as_deref().unwrap_or(&[]).iter().map(|color| parse_color(color)).collect(),
        None => Vec::new(),
    };
    if colors.is_empty() || colors.iter().any(Option::is_none) {
        return url.to_string();
    }
    let colors: Vec<String> = colors.into_iter().flatten().collect();
    let gradient = if colors.len() > 1 {
        let last = (colors.len() - 1) as f64;
        let stops = colors
            .iter()
            .enumerate()
            .map(|(index, color)| {
                format!("<stop offset=\"{}\" stop-color=\"{color}\"/>", format_number(index as f64 / last))
            })
            .collect::<String>();
        format!("<defs><linearGradient id=\"bg\" x2=\"0\" y2=\"1\">{stops}</linearGradient></defs>")
    } else {
        String::new()
    };
    let fill_value = if colors.len() > 1 { "url(#bg)".to_string() } else { colors[0].clone() };
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 128 128\">{gradient}<rect width=\"128\" height=\"128\" rx=\"28\" fill=\"{fill_value}\"/><image width=\"128\" height=\"128\" href=\"{url}\"/></svg>"
    );
    data_url(Path::new("icon.svg"), svg.as_bytes())
}

fn parse_color(value: &str) -> Option<String> {
    let (space, values) = value.split_once(':')?;
    if !matches!(space, "gray" | "srgb" | "display-p3") {
        return None;
    }
    if values.is_empty()
        || !values.chars().all(|character| character.is_ascii_digit() || character == '.' || character == ',')
    {
        return None;
    }
    let numbers: Vec<f64> =
        values.split(',').map(|part| if part.is_empty() { 0.0 } else { part.parse().unwrap_or(f64::NAN) }).collect();
    let expected = if space == "gray" { 2 } else { 4 };
    if numbers.len() != expected {
        return None;
    }
    if numbers.iter().any(|number| !number.is_finite() || *number < 0.0 || *number > 1.0) {
        return None;
    }
    let rgb = if space == "gray" { [numbers[0], numbers[0], numbers[0]] } else { [numbers[0], numbers[1], numbers[2]] };
    Some(format!(
        "color({} {} {} {} / {})",
        if space == "display-p3" { "display-p3" } else { "srgb" },
        format_number(rgb[0]),
        format_number(rgb[1]),
        format_number(rgb[2]),
        format_number(numbers[expected - 1])
    ))
}

fn format_number(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.000_001 { format!("{value:e}") } else { value.to_string() }
}

fn extension(file: &Path) -> Option<String> {
    file.extension().map(|extension| format!(".{}", extension.to_string_lossy().to_lowercase()))
}

fn windows_digits(name: &str) -> Option<&str> {
    if name.len() < "icon-.png".len() {
        return None;
    }
    let prefix = name.get(.."icon-".len())?;
    let suffix = name.get(name.len() - ".png".len()..)?;
    if !prefix.eq_ignore_ascii_case("icon-") || !suffix.eq_ignore_ascii_case(".png") {
        return None;
    }
    let digits = &name["icon-".len()..name.len() - ".png".len()];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(digits)
}

fn rank(name: &str, windows: bool) -> f64 {
    if windows {
        windows_digits(name).map_or(f64::NAN, |digits| (digits.parse::<f64>().unwrap_or(f64::NAN) - 128.0).abs())
    } else if name.contains("128") || name.contains("256") {
        0.0
    } else {
        1.0
    }
}

fn make_icon(root: &Path, file: &Path, url: String) -> Icon {
    Icon { path: relative(root, file), url }
}

fn relative(root: &Path, file: &Path) -> String {
    let Ok(relative) = file.strip_prefix(root) else {
        return String::new();
    };
    relative.components().map(|component| component.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
    Other,
}

struct Entry {
    name: String,
    kind: EntryKind,
}

#[derive(Deserialize)]
struct ComposerConfig {
    groups: Vec<ComposerGroup>,
    fill: Option<ComposerFill>,
}

#[derive(Deserialize)]
struct ComposerGroup {
    layers: Vec<ComposerLayer>,
}

#[derive(Deserialize)]
struct ComposerLayer {
    #[serde(rename = "image-name")]
    image_name: Option<String>,
}

#[derive(Deserialize)]
struct ComposerFill {
    solid: Option<String>,
    #[serde(rename = "linear-gradient")]
    linear_gradient: Option<Vec<String>>,
}
