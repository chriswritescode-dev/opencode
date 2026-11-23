use std::collections::HashMap;
use std::io::Write;

use bytes::Bytes;
use http::{HeaderValue, header};
use http_body_util::Full;

use crate::response;

const MIN_SIZE: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Brotli,
    Gzip,
    Deflate,
}

impl Algorithm {
    fn as_str(self) -> &'static str {
        match self {
            Algorithm::Brotli => "br",
            Algorithm::Gzip => "gzip",
            Algorithm::Deflate => "deflate",
        }
    }
}

pub fn apply(accept_encoding: Option<&str>, mut response: response::Response) -> response::Response {
    let status = response.status().as_u16();
    if status < 200 || status == 204 || status == 206 || status == 304 {
        return response;
    }
    if let Some(encoding) = response.headers().get(header::CONTENT_ENCODING).and_then(|value| value.to_str().ok()) {
        if encoding.trim().eq_ignore_ascii_case("identity") {
            response.headers_mut().remove(header::CONTENT_ENCODING);
        }
        return response;
    }
    let body = response.body().clone().into_inner().unwrap_or_default();
    if body.is_empty() {
        return response;
    }
    let cache_control = response.headers().get(header::CACHE_CONTROL).and_then(|value| value.to_str().ok());
    if cache_control.is_some_and(has_no_transform) {
        return response;
    }
    let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok());
    if !content_type.is_some_and(compressible) {
        return response;
    }
    let Some(algorithm) = negotiate(accept_encoding) else {
        response::vary_with(&mut response, "Accept-Encoding");
        return response;
    };
    if body.len() < MIN_SIZE {
        response::vary_with(&mut response, "Accept-Encoding");
        return response;
    }
    let Ok(compressed) = compress(algorithm, &body) else {
        response::vary_with(&mut response, "Accept-Encoding");
        return response;
    };
    *response.body_mut() = Full::new(compressed);
    response.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static(algorithm.as_str()));
    response.headers_mut().remove(header::CONTENT_LENGTH);
    response::vary_with(&mut response, "Accept-Encoding");
    let etag = response.headers().get(header::ETAG).and_then(|value| value.to_str().ok()).map(str::to_owned);
    if let Some(etag) = etag
        && !etag.starts_with("W/")
        && let Ok(value) = HeaderValue::from_str(&format!("W/{etag}"))
    {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

fn has_no_transform(cache_control: &str) -> bool {
    let lower = cache_control.to_lowercase();
    lower.match_indices("no-transform").any(|(index, _)| {
        let before = lower[..index].chars().next_back();
        let after = lower[index + "no-transform".len()..].chars().next();
        before.is_none_or(|character| character == ',' || character.is_whitespace())
            && after.is_none_or(|character| character == ',' || character == ';' || character.is_whitespace())
    })
}

fn compressible(content_type: &str) -> bool {
    let media = content_type.split(';').next().unwrap_or_default().trim().to_lowercase();
    media.starts_with("text/")
        || matches!(
            media.as_str(),
            "application/json" | "application/javascript" | "application/xml" | "image/svg+xml" | "application/wasm"
        )
        || media.ends_with("+json")
        || media.ends_with("+xml")
}

fn negotiate(header: Option<&str>) -> Option<Algorithm> {
    let accepted = parse_accept_encoding(header?)?;
    [Algorithm::Brotli, Algorithm::Gzip, Algorithm::Deflate].into_iter().find(|algorithm| {
        accepted.get(algorithm.as_str()).or_else(|| accepted.get("*")).is_some_and(|quality| *quality > 0.0)
    })
}

fn parse_accept_encoding(header: &str) -> Option<HashMap<String, f64>> {
    let mut accepted = HashMap::new();
    for part in header.split(',') {
        let member = part
            .trim()
            .to_lowercase()
            .split(';')
            .map(|segment| segment.trim_matches([' ', '\t']))
            .collect::<Vec<_>>()
            .join(";");
        if member.is_empty() {
            continue;
        }
        let (coding, quality) = parse_member(&member)?;
        accepted.insert(coding.to_owned(), quality);
    }
    Some(accepted)
}

fn parse_member(member: &str) -> Option<(&str, f64)> {
    let Some((coding, parameter)) = member.split_once(';') else {
        return is_token(member).then_some((member, 1.0));
    };
    if !is_token(coding) {
        return None;
    }
    parse_quality(parameter.strip_prefix("q=")?).map(|quality| (coding, quality))
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                        | b'-'
                )
        })
}

fn parse_quality(value: &str) -> Option<f64> {
    let (head, rest) = value.as_bytes().split_first()?;
    let whole = match head {
        b'0' => 0.0,
        b'1' => 1.0,
        _ => return None,
    };
    let digits = match rest {
        [] => return Some(whole),
        [b'.', digits @ ..] => digits,
        _ => return None,
    };
    if digits.len() > 3 {
        return None;
    }
    let valid = match head {
        b'0' => digits.iter().all(u8::is_ascii_digit),
        _ => digits.iter().all(|digit| *digit == b'0'),
    };
    if !valid {
        return None;
    }
    let mut quality = whole;
    let mut scale = 1.0;
    for digit in digits {
        scale /= 10.0;
        quality += f64::from(digit - b'0') * scale;
    }
    Some(quality)
}

fn compress(algorithm: Algorithm, body: &[u8]) -> std::io::Result<Bytes> {
    match algorithm {
        Algorithm::Brotli => {
            let params = brotli::enc::BrotliEncoderParams { quality: 4, ..Default::default() };
            let mut input = body;
            let mut output = Vec::new();
            brotli::BrotliCompress(&mut input, &mut output, &params)?;
            Ok(Bytes::from(output))
        }
        Algorithm::Gzip => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(body)?;
            Ok(Bytes::from(encoder.finish()?))
        }
        Algorithm::Deflate => {
            let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(body)?;
            Ok(Bytes::from(encoder.finish()?))
        }
    }
}
