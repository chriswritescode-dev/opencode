//! MIME detection from bytes and from a path's extension.

use opencode_util::path::Flavor;

use crate::generated::mime_types::TYPES;

pub const OCTET_STREAM: &str = "application/octet-stream";

/// Media types the read tool inlines: images and PDFs.
pub const MEDIA_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"];

/// `Mime.detect`: a media signature, else text, else an opaque stream.
pub fn detect(bytes: &[u8]) -> &'static str {
    detect_media(bytes).unwrap_or(if is_text(bytes) { "text/plain" } else { OCTET_STREAM })
}

/// `Mime.detectMedia`: only the byte signatures, without the text fallback.
pub fn detect_media(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    let signature = ascii(bytes, 0, 6);
    if signature == b"GIF87a" || signature == b"GIF89a" {
        return Some("image/gif");
    }
    if bytes.len() >= 18 && bytes.starts_with(&[0x42, 0x4d]) {
        let pixels = u32::from_le_bytes(bytes[10..14].try_into().unwrap());
        let dib = u32::from_le_bytes(bytes[14..18].try_into().unwrap());
        // Pixels follow the 14-byte file header and a Windows or OS/2 DIB header, possibly beyond this sample.
        if [12, 16, 40, 52, 56, 64, 108, 124].contains(&dib) && pixels >= 14 + dib {
            return Some("image/bmp");
        }
    }
    if bytes.starts_with(&[0x25, 0x50, 0x44, 0x46, 0x2d]) {
        return Some("application/pdf");
    }
    if bytes.starts_with(&[0x52, 0x49, 0x46, 0x46])
        && bytes[8.min(bytes.len())..].starts_with(&[0x57, 0x45, 0x42, 0x50])
    {
        return Some("image/webp");
    }
    if bytes.len() >= 12 && ascii(bytes, 4, 8) == b"ftyp" {
        // AVIF, HEIC, QuickTime and M4A share MP4's container; the major brand tells them apart, and generic HEIF
        // brands list `avif`/`avis` among the compatible brands that follow the minor version.
        let brand = ascii(bytes, 8, 12);
        let box_end = (u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize).min(bytes.len());
        let compatible = (0..box_end.saturating_sub(16) / 4)
            .map(|index| ascii(bytes, 16 + index * 4, 20 + index * 4))
            .collect::<Vec<_>>();
        let has = |wanted: &[u8]| compatible.contains(&wanted);
        let generic = brand == b"mif1" || brand == b"msf1";
        if brand == b"avif" || brand == b"avis" || (generic && (has(b"avif") || has(b"avis"))) {
            return Some("image/avif");
        }
        if generic || [&b"heic"[..], b"heix", b"hevc", b"hevx"].contains(&brand) {
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
    None
}

/// `Mime.isTextual`: whether a MIME type carries text a model can read.
pub fn is_textual(mime: &str) -> bool {
    mime.is_empty()
        || mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
        || mime == "application/xml"
        || mime.ends_with("+xml")
        || mime == "application/javascript"
        || mime == "application/x-javascript"
}

/// `FSUtil.mimeType`: the extension table's MIME type, else `application/octet-stream`.
pub fn lookup(path: &str) -> &'static str {
    if path.is_empty() {
        return OCTET_STREAM;
    }
    let mut name = String::with_capacity(path.len() + 2);
    name.push_str("x.");
    name.push_str(path);
    let extension = extension(&name, Flavor::NATIVE);
    let extension = extension.strip_prefix('.').unwrap_or_default().to_ascii_lowercase();
    if extension.is_empty() {
        return OCTET_STREAM;
    }
    TYPES.binary_search_by_key(&extension.as_str(), |(key, _)| *key).map_or(OCTET_STREAM, |index| TYPES[index].1)
}

pub fn extname(path: &str) -> &str {
    extension(path, Flavor::NATIVE)
}

fn ascii(bytes: &[u8], start: usize, end: usize) -> &[u8] {
    &bytes[start.min(bytes.len())..end.min(bytes.len())]
}

/// Node's `path.extname` on `path`, using `flavor`'s separators.
fn extension(path: &str, flavor: Flavor) -> &str {
    let bytes = path.as_bytes();
    let mut start_part = 0;
    let mut start_dot = None;
    let mut end = None;
    let mut matched_slash = true;
    let mut pre_dot_state = 0;
    for index in (0..bytes.len()).rev() {
        if flavor.is_sep(bytes[index] as char) {
            if !matched_slash {
                start_part = index + 1;
                break;
            }
            continue;
        }
        if end.is_none() {
            matched_slash = false;
            end = Some(index + 1);
        }
        if bytes[index] == b'.' {
            match start_dot {
                None => start_dot = Some(index),
                Some(_) if pre_dot_state != 1 => pre_dot_state = 1,
                Some(_) => {}
            }
        } else if start_dot.is_some() {
            pre_dot_state = -1;
        }
    }
    let (Some(start_dot), Some(end)) = (start_dot, end) else { return "" };
    if pre_dot_state == 0 || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1) {
        return "";
    }
    &path[start_dot..end]
}

fn is_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if bytes.contains(&0) {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => {}
        Err(error) if error.error_len().is_none() => {}
        Err(_) => return false,
    }
    let controls = bytes.iter().filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32)).count();
    (controls as f64 / bytes.len() as f64) <= 0.3
}
