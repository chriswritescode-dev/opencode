pub fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && tag(bytes, 0, 4) == b"RIFF" {
        let riff_type = tag(bytes, 8, 12);
        if riff_type == b"WEBP" {
            return Some("image/webp");
        }
        if riff_type == b"WAVE" {
            return Some("audio/wav");
        }
    }
    if bytes.starts_with(&[0x25, 0x50, 0x44, 0x46]) {
        return Some("application/pdf");
    }
    if bytes.len() >= 12 && tag(bytes, 4, 8) == b"ftyp" {
        let brand = tag(bytes, 8, 12);
        let box_end = declared_box_end(bytes).min(bytes.len());
        let compatible = |value: &[u8]| {
            (0..box_end.saturating_sub(16) / 4).any(|index| tag(bytes, 16 + index * 4, 20 + index * 4) == value)
        };
        let generic = brand == b"mif1" || brand == b"msf1";
        if brand == b"avif" || brand == b"avis" || (generic && (compatible(b"avif") || compatible(b"avis"))) {
            return Some("image/avif");
        }
        if generic || brand == b"heic" || brand == b"heix" || brand == b"hevc" || brand == b"hevx" {
            return Some("image/heic");
        }
        if brand == b"qt  " {
            return Some("video/quicktime");
        }
        if brand == b"M4A " {
            return Some("audio/mp4");
        }
        return Some("video/mp4");
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if bytes.starts_with(&[0x49, 0x44, 0x33]) {
        return Some("audio/mpeg");
    }
    if bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0 {
        return Some(if (bytes[1] & 0x06) == 0 { "audio/aac" } else { "audio/mpeg" });
    }
    if bytes.starts_with(&[0x4f, 0x67, 0x67, 0x53]) {
        return Some("audio/ogg");
    }
    if bytes.starts_with(&[0x66, 0x4c, 0x61, 0x43]) {
        return Some("audio/flac");
    }
    None
}

pub fn extension(media_type: &str) -> Option<&'static str> {
    let media_type = media_type.split(';').next().unwrap_or_default().trim().to_lowercase();
    let alias = match media_type.as_str() {
        "audio/mp3" => Some("mp3"),
        "audio/m4a" | "audio/x-m4a" => Some("m4a"),
        "audio/webm" => Some("webm"),
        "audio/wave" | "audio/x-wav" => Some("wav"),
        "audio/x-flac" => Some("flac"),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    EXTENSIONS.iter().find(|(_, known)| *known == media_type).map(|(extension, _)| *extension)
}

const EXTENSIONS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("pdf", "application/pdf"),
    ("mp4", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("flac", "audio/flac"),
    ("aac", "audio/aac"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
];

fn tag(bytes: &[u8], start: usize, end: usize) -> &[u8] {
    bytes.get(start..end).unwrap_or_default()
}

fn declared_box_end(bytes: &[u8]) -> usize {
    match bytes {
        [a, b, c, d, ..] => u32::from_be_bytes([*a, *b, *c, *d]) as usize,
        _ => 0,
    }
}
