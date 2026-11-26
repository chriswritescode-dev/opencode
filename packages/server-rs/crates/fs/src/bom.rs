//! The UTF-8 byte-order mark: split it off, join it back and sync bytes to a wanted state.

pub struct Synced {
    pub text: String,
    pub bytes: Option<Vec<u8>>,
}

pub fn has(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xef, 0xbb, 0xbf])
}

/// Splits every leading U+FEFF off `text`, reporting whether any were there.
pub fn split(text: &str) -> (bool, &str) {
    let stripped = text.trim_start_matches('\u{feff}');
    (stripped.len() != text.len(), stripped)
}

/// Strips any leading U+FEFF, then prepends one when `bom` is set.
pub fn join(text: &str, bom: bool) -> String {
    let (_, stripped) = split(text);
    if bom { format!("\u{feff}{stripped}") } else { stripped.to_owned() }
}

/// Decodes `content`, reporting the text without a BOM and the bytes to write when they differ.
pub fn sync_bytes(content: &[u8], bom: bool) -> Synced {
    match std::str::from_utf8(content) {
        Ok(decoded) => {
            let (_, text) = split(decoded);
            let canonical = join(text, bom);
            Synced { text: text.to_owned(), bytes: (decoded != canonical).then(|| canonical.into_bytes()) }
        }
        Err(_) => {
            let decoded = String::from_utf8_lossy(content);
            let (_, text) = split(&decoded);
            Synced { text: text.to_owned(), bytes: None }
        }
    }
}
