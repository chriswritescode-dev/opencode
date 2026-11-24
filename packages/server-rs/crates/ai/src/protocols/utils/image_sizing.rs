use crate::media::protocol::Identity;
use crate::modalities::image::ImageRequest;
use crate::schema::AiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Shape<'a> {
    pub aspect: Option<&'a str>,
    pub resolution: Option<&'a str>,
}

pub fn shape(request: &ImageRequest) -> Shape<'_> {
    Shape { aspect: request.aspect.as_deref(), resolution: request.resolution.as_deref() }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub multiple: i64,
    pub min_edge: Option<i64>,
    pub max_edge: Option<i64>,
    pub min_pixels: Option<i64>,
    pub max_pixels: Option<i64>,
    pub max_ratio: Option<f64>,
}

pub fn is_requested(shape: &Shape<'_>) -> bool {
    shape.aspect.is_some() || shape.resolution.is_some()
}

pub fn tier(resolution: &str) -> Option<f64> {
    let lower = resolution.to_ascii_lowercase();
    let rest = lower.strip_suffix('k')?;
    if rest.is_empty() {
        return None;
    }
    let mut seen_dot = false;
    for (index, character) in rest.char_indices() {
        if character.is_ascii_digit() {
            continue;
        }
        if character == '.' && !seen_dot && index > 0 && index + 1 < rest.len() {
            seen_dot = true;
            continue;
        }
        return None;
    }
    rest.parse::<f64>().ok()
}

pub fn fit(shape: &Shape<'_>, limits: &Limits) -> Size {
    let max_ratio = limits.max_ratio.unwrap_or(f64::INFINITY);
    let ratio = max_ratio.min((1.0 / max_ratio).max(aspect_ratio(shape)));
    let area = target_pixels(shape);
    let width = (area * ratio).sqrt();
    let height = (area / ratio).sqrt();
    let min_edge = limits.min_edge.unwrap_or(0) as f64;
    let min_pixels = limits.min_pixels.unwrap_or(0) as f64;
    let up = (min_edge / width.min(height)).max((min_pixels / area).sqrt());
    let scaled = 1.0f64.max(up);
    let max_edge = limits.max_edge.map_or(f64::INFINITY, |value| value as f64);
    let max_pixels = limits.max_pixels.map_or(f64::INFINITY, |value| value as f64);
    let down = (max_edge / (width.max(height) * scaled)).min((max_pixels / area).sqrt() / scaled);
    let round = if down <= 1.0 {
        Rounding::Floor
    } else if up >= 1.0 {
        Rounding::Ceil
    } else {
        Rounding::Nearest
    };
    let multiple = limits.multiple as f64;
    let snap = |edge: f64| {
        let value = to_fixed(edge * scaled * down.min(1.0) / multiple, 6);
        (round.apply(value) * multiple).max(multiple) as i64
    };
    Size { width: snap(width), height: snap(height) }
}

pub fn nearest(shape: &Shape<'_>, sizes: &[Size]) -> Size {
    let ratio = aspect_ratio(shape).ln();
    let area = target_pixels(shape).ln();
    let distance = |size: &Size| {
        (
            ((size.width as f64 / size.height as f64).ln() - ratio).abs(),
            ((size.width as f64 * size.height as f64).ln() - area).abs(),
        )
    };
    let mut best = sizes[0];
    for size in &sizes[1..] {
        let (size_ratio, size_area) = distance(size);
        let (best_ratio, best_area) = distance(&best);
        if (size_ratio - best_ratio).abs() > 1e-9 {
            if size_ratio < best_ratio {
                best = *size;
            }
            continue;
        }
        if size_area < best_area {
            best = *size;
        }
    }
    best
}

pub fn exact(identity: &Identity, shape: &Shape<'_>, limits: &Limits) -> Result<Option<Size>, AiError> {
    if !is_requested(shape) {
        return Ok(None);
    }
    let size = fit(shape, limits);
    let Some(requested) = missed(shape, &size) else {
        return Ok(Some(size));
    };
    let operation = if shape.resolution.is_none() { "media.aspect" } else { "media.resolution" };
    Err(identity.unsupported(
        operation,
        format!(
            "{} can't render {requested}; the closest size it takes is {}. Change aspect or resolution, or set the size in providerOptions",
            identity.name,
            describe(&size)
        ),
    ))
}

fn missed(shape: &Shape<'_>, size: &Size) -> Option<String> {
    let aspect_missed = shape
        .aspect
        .is_some_and(|_| ((size.width as f64 / size.height as f64) / aspect_ratio(shape)).ln().abs() > 0.02);
    let resolution_missed = shape.resolution.is_some_and(|resolution| {
        let Some(requested) = tier(resolution) else {
            return true;
        };
        let rendered = ((size.width as f64 * size.height as f64).sqrt() / (requested * 1024.0)).log2().abs();
        rendered > 0.5
    });
    if !aspect_missed && !resolution_missed {
        return None;
    }
    let parts: Vec<&str> = [shape.aspect, shape.resolution].into_iter().flatten().collect();
    Some(parts.join(" at "))
}

fn aspect_ratio(shape: &Shape<'_>) -> f64 {
    let aspect = shape.aspect.unwrap_or("1:1");
    let Some((width, height)) = aspect.split_once(':') else {
        return f64::NAN;
    };
    width.parse::<f64>().unwrap_or(f64::NAN) / height.parse::<f64>().unwrap_or(f64::NAN)
}

fn target_pixels(shape: &Shape<'_>) -> f64 {
    (tier(shape.resolution.unwrap_or("1K")).unwrap_or(1.0) * 1024.0).powi(2)
}

fn describe(size: &Size) -> String {
    let divisor = gcd(size.width, size.height);
    let width = size.width / divisor;
    let height = size.height / divisor;
    let ratio = if width <= 32 && height <= 32 {
        format!("{width}:{height}")
    } else {
        format!("{:.2}:1", size.width as f64 / size.height as f64)
    };
    format!("{}x{} ({ratio})", size.width, size.height)
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

fn to_fixed(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

enum Rounding {
    Floor,
    Ceil,
    Nearest,
}

impl Rounding {
    fn apply(&self, value: f64) -> f64 {
        match self {
            Rounding::Floor => value.floor(),
            Rounding::Ceil => value.ceil(),
            Rounding::Nearest => value.round(),
        }
    }
}
