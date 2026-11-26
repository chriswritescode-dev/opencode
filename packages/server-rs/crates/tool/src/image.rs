use std::sync::RwLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_config::entry::Entry;
use opencode_contract::generated::ConfigEntryDocumentInfoMediaImage;

use ::image::ExtendedColorType;
use ::image::ImageEncoder;
use ::image::codecs::jpeg::JpegEncoder;
use ::image::codecs::png::PngEncoder;
use ::image::imageops::FilterType;

pub const DEFAULT_AUTO_RESIZE: bool = true;
pub const DEFAULT_MAX_WIDTH: u32 = 2_000;
pub const DEFAULT_MAX_HEIGHT: u32 = 2_000;
pub const DEFAULT_MAX_BASE64_BYTES: usize = 5 * 1024 * 1024;

const PNG_PASSTHROUGH_MAX_BASE64_BYTES: usize = 384 * 1024;
const SMALL_PNG_MAX_PIXELS: u64 = 256 * 256;
const JPEG_QUALITIES: [u8; 4] = [80, 70, 55, 40];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub auto_resize: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub max_base64_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            auto_resize: DEFAULT_AUTO_RESIZE,
            max_width: DEFAULT_MAX_WIDTH,
            max_height: DEFAULT_MAX_HEIGHT,
            max_base64_bytes: DEFAULT_MAX_BASE64_BYTES,
        }
    }
}

impl Limits {
    fn apply(&mut self, image: &ConfigEntryDocumentInfoMediaImage) {
        if let Some(auto_resize) = image.auto_resize {
            self.auto_resize = auto_resize;
        }
        if let Some(max_width) = image.max_width {
            self.max_width = max_width as u32;
        }
        if let Some(max_height) = image.max_height {
            self.max_height = max_height as u32;
        }
        if let Some(max_base64_bytes) = image.max_base64_bytes {
            self.max_base64_bytes = max_base64_bytes as usize;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized {
    pub mime: String,
    pub content: String,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    #[error("Image could not be decoded: {resource}")]
    Decode { resource: String },
    #[error(
        "Image {resource} is {width}x{height} with base64 size {bytes}, exceeding configured limits {max_width}x{max_height}/{max_bytes} bytes"
    )]
    Size { resource: String, width: u32, height: u32, bytes: usize, max_width: u32, max_height: u32, max_bytes: usize },
}

pub struct Image {
    limits: RwLock<Limits>,
}

impl Default for Image {
    fn default() -> Self {
        Self::new()
    }
}

impl Image {
    pub fn new() -> Self {
        Image { limits: RwLock::new(Limits::default()) }
    }

    pub fn limits(&self) -> Limits {
        *self.limits.read().expect("the image limits lock is not poisoned")
    }

    pub fn configure(&self, entries: &[Entry]) {
        let mut limits = Limits::default();
        for entry in entries {
            if let Entry::Document(document) = entry
                && let Some(image) = document.info.media.as_ref().and_then(|media| media.image.as_ref())
            {
                limits.apply(image);
            }
        }
        *self.limits.write().expect("the image limits lock is not poisoned") = limits;
    }

    pub fn normalize(&self, resource: &str, mime: &str, content: &str) -> Result<Option<Normalized>, ImageError> {
        normalize(resource, mime, content, self.limits())
    }
}

pub fn normalize(resource: &str, mime: &str, content: &str, limits: Limits) -> Result<Option<Normalized>, ImageError> {
    let bytes = content.len();
    let decoded = STANDARD
        .decode(content)
        .ok()
        .and_then(|bytes| ::image::load_from_memory(&bytes).ok())
        .ok_or_else(|| ImageError::Decode { resource: resource.to_owned() })?;
    let width = decoded.width();
    let height = decoded.height();
    let size_error = || ImageError::Size {
        resource: resource.to_owned(),
        width,
        height,
        bytes,
        max_width: limits.max_width,
        max_height: limits.max_height,
        max_bytes: limits.max_base64_bytes,
    };
    let fits = width <= limits.max_width && height <= limits.max_height && bytes <= limits.max_base64_bytes;
    let small_png = bytes <= PNG_PASSTHROUGH_MAX_BASE64_BYTES || width as u64 * height as u64 <= SMALL_PNG_MAX_PIXELS;
    if fits && (!limits.auto_resize || mime != "image/png" || small_png) {
        return Ok(None);
    }
    if !limits.auto_resize {
        return Err(size_error());
    }
    let transparent = mime != "image/jpeg" && has_transparency(&decoded, width, height);
    if fits {
        if transparent {
            return Ok(None);
        }
        let jpeg = encode_jpeg(&decoded, JPEG_QUALITIES[0]);
        return Ok(jpeg
            .filter(|jpeg| base64_length(jpeg.len()) < bytes)
            .map(|jpeg| Normalized { mime: "image/jpeg".to_owned(), content: STANDARD.encode(jpeg) }));
    }

    let scale = (limits.max_width as f64 / width as f64).min(limits.max_height as f64 / height as f64).min(1.0);
    for (width, height) in sizes(width, height, scale) {
        let resized = if width == decoded.width() && height == decoded.height() {
            None
        } else {
            Some(decoded.resize_exact(width, height, FilterType::Lanczos3))
        };
        let resized = resized.as_ref().unwrap_or(&decoded);
        let png_first = transparent || width as u64 * height as u64 <= SMALL_PNG_MAX_PIXELS;
        for (mime, encoder) in encoders(png_first) {
            let candidate = match encoder {
                Encoder::Png => encode_png(resized),
                Encoder::Jpeg(quality) => encode_jpeg(resized, quality),
            };
            let Some(candidate) = candidate else { continue };
            if base64_length(candidate.len()) <= limits.max_base64_bytes {
                return Ok(Some(Normalized { mime: mime.to_owned(), content: STANDARD.encode(candidate) }));
            }
        }
    }
    Err(size_error())
}

fn sizes(width: u32, height: u32, scale: f64) -> Vec<(u32, u32)> {
    let mut sizes: Vec<(u32, u32)> = Vec::new();
    for _ in 0..32 {
        let previous = sizes.last().copied().unwrap_or_else(|| {
            (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1))
        });
        let next = if sizes.is_empty() {
            previous
        } else {
            (
                if previous.0 == 1 { 1 } else { ((previous.0 as f64 * 0.75).floor() as u32).max(1) },
                if previous.1 == 1 { 1 } else { ((previous.1 as f64 * 0.75).floor() as u32).max(1) },
            )
        };
        if sizes.contains(&next) {
            continue;
        }
        sizes.push(next);
    }
    sizes
}

#[derive(Clone, Copy)]
enum Encoder {
    Png,
    Jpeg(u8),
}

fn encoders(png_first: bool) -> Vec<(&'static str, Encoder)> {
    let png = ("image/png", Encoder::Png);
    let jpeg: Vec<(&'static str, Encoder)> =
        JPEG_QUALITIES.iter().map(|quality| ("image/jpeg", Encoder::Jpeg(*quality))).collect();
    if png_first {
        std::iter::once(png).chain(jpeg).collect()
    } else {
        jpeg.into_iter().chain(std::iter::once(png)).collect()
    }
}

fn base64_length(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

fn has_transparency(image: &::image::DynamicImage, width: u32, height: u32) -> bool {
    let pixels = image.to_rgba8();
    let columns = width.min(64);
    let rows = height.min(64);
    let mut transparent = 0usize;
    for row in 0..rows {
        let y = (((row as f64 + 0.5) * height as f64) / rows as f64).floor() as u32;
        for column in 0..columns {
            let x = (((column as f64 + 0.5) * width as f64) / columns as f64).floor() as u32;
            if pixels.get_pixel(x, y)[3] < 250 {
                transparent += 1;
            }
        }
    }
    transparent as f64 / (columns as f64 * rows as f64) > 0.05
}

fn encode_png(image: &::image::DynamicImage) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        PngEncoder::new(&mut buffer)
            .write_image(rgba.as_raw(), rgba.width(), rgba.height(), ExtendedColorType::Rgba8)
            .ok()?;
    } else {
        let rgb = image.to_rgb8();
        PngEncoder::new(&mut buffer)
            .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
            .ok()?;
    }
    Some(buffer)
}

fn encode_jpeg(image: &::image::DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let rgb = image.to_rgb8();
    JpegEncoder::new_with_quality(&mut buffer, quality)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .ok()
        .map(|()| buffer)
}
