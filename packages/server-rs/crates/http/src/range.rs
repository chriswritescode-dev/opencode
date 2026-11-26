#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteRange {
    Satisfiable { start: u64, end: u64 },
    Unsatisfiable,
}

pub fn parse(header: Option<&str>, size: u64) -> Option<ByteRange> {
    let text = header?.trim();
    if !text.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case("bytes=")) {
        return None;
    }
    let (first, second) = text[6..].split_once('-')?;
    let first = first.trim();
    let second = second.trim();
    if first.is_empty() && second.is_empty() {
        return None;
    }
    if first.is_empty() {
        let suffix = digits(second)?;
        if suffix == 0 || size == 0 {
            return Some(ByteRange::Unsatisfiable);
        }
        return Some(ByteRange::Satisfiable { start: size.saturating_sub(suffix), end: size - 1 });
    }
    let start = digits(first)?;
    let end = if second.is_empty() { size.saturating_sub(1) } else { digits(second)? };
    if start > end || start >= size {
        return Some(ByteRange::Unsatisfiable);
    }
    Some(ByteRange::Satisfiable { start, end: end.min(size - 1) })
}

fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(text.bytes().fold(0u64, |value, byte| value.saturating_mul(10).saturating_add(u64::from(byte - b'0'))))
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, parse};

    #[test]
    fn parses_single_ranges() {
        assert_eq!(parse(Some("bytes=0-0"), 10), Some(ByteRange::Satisfiable { start: 0, end: 0 }));
        assert_eq!(parse(Some("bytes=2-5"), 10), Some(ByteRange::Satisfiable { start: 2, end: 5 }));
        assert_eq!(parse(Some("bytes=7-99"), 10), Some(ByteRange::Satisfiable { start: 7, end: 9 }));
        assert_eq!(parse(Some("bytes=6-"), 10), Some(ByteRange::Satisfiable { start: 6, end: 9 }));
        assert_eq!(parse(Some("bytes=-3"), 10), Some(ByteRange::Satisfiable { start: 7, end: 9 }));
        assert_eq!(parse(Some("bytes=-99"), 10), Some(ByteRange::Satisfiable { start: 0, end: 9 }));
    }

    #[test]
    fn rejects_malformed_and_multiple_ranges() {
        for header in ["bytes=0-1, 4-5", "items=0-3", "bytes=", "bytes=-", "bytes=abc-def", "bytes=0-1-2"] {
            assert_eq!(parse(Some(header), 10), None, "{header}");
        }
        assert_eq!(parse(None, 10), None);
    }

    #[test]
    fn reports_unsatisfiable_ranges() {
        assert_eq!(parse(Some("bytes=10-12"), 10), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=10-"), 10), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=5-2"), 10), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=-0"), 10), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=0-0"), 0), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=99999999999999999999-"), 10), Some(ByteRange::Unsatisfiable));
        assert_eq!(parse(Some("bytes=-99999999999999999999"), 10), Some(ByteRange::Satisfiable { start: 0, end: 9 }));
    }
}
