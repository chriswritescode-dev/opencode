const NEGATIVE_INFINITY: f64 = f64::NEG_INFINITY;

pub struct Prepared {
    pub target: String,
    target_lower_codes: Vec<u16>,
    next_beginning_indexes: Vec<usize>,
    bitflags: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Found {
    pub target: String,
    pub score: f64,
    pub raw_score: f64,
    pub indexes: Vec<usize>,
}

struct PreparedSearch {
    lower_codes: Vec<u16>,
    bitflags: u32,
    contains_space: bool,
    space_searches: Vec<Vec<u16>>,
}

pub fn prepare(target: &str) -> Prepared {
    let (target_lower_codes, bitflags) = lower_info(target);
    Prepared {
        target: target.to_owned(),
        target_lower_codes,
        next_beginning_indexes: prepare_next_beginning_indexes(target),
        bitflags,
    }
}

pub fn go(query: &str, targets: &[&Prepared], limit: usize) -> Vec<Found> {
    if query.is_empty() {
        return Vec::new();
    }
    let prepared = prepare_search(query);
    let limit = if limit == 0 { usize::MAX } else { limit };
    let mut heap = MinHeap::default();
    let mut results_len = 0usize;
    for target in targets {
        if prepared.bitflags & target.bitflags != prepared.bitflags {
            continue;
        }
        let result = algorithm(
            &prepared.lower_codes,
            &prepared.space_searches,
            prepared.contains_space,
            target,
            &target.next_beginning_indexes,
            false,
        );
        let Some(result) = result else { continue };
        if results_len < limit {
            heap.add(result);
            results_len += 1;
        } else if result.raw_score > heap.items[0].raw_score {
            heap.replace_top(result);
        }
    }
    let mut results = Vec::with_capacity(results_len);
    for _ in 0..results_len {
        if let Some(result) = heap.poll() {
            results.push(result);
        }
    }
    results.reverse();
    results
}

fn prepare_search(value: &str) -> PreparedSearch {
    let trimmed = value.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}');
    let (lower_codes, bitflags) = lower_info(trimmed);
    let contains_space = lower_codes.contains(&32);
    let space_searches = if contains_space {
        let mut parts: Vec<&str> = Vec::new();
        for part in trimmed.split(|character: char| character.is_whitespace() || character == '\u{feff}') {
            if part.is_empty() || parts.contains(&part) {
                continue;
            }
            parts.push(part);
        }
        parts.into_iter().map(|part| lower_info(part).0).collect()
    } else {
        Vec::new()
    };
    PreparedSearch { lower_codes, bitflags, contains_space, space_searches }
}

fn algorithm(
    search_codes: &[u16],
    space_searches: &[Vec<u16>],
    contains_space: bool,
    target: &Prepared,
    next_beginning_indexes: &[usize],
    allow_spaces: bool,
) -> Option<Found> {
    if !allow_spaces && contains_space {
        return algorithm_spaces(search_codes, space_searches, target);
    }
    let search_len = search_codes.len();
    if search_len == 0 {
        return None;
    }
    let target_codes = &target.target_lower_codes;
    let target_len = target_codes.len();

    let mut matches_simple = Vec::with_capacity(search_len);
    let mut search_index = 0usize;
    let mut target_index = 0usize;
    loop {
        if target_codes.get(target_index) == Some(&search_codes[search_index]) {
            matches_simple.push(target_index);
            search_index += 1;
            if search_index == search_len {
                break;
            }
        }
        target_index += 1;
        if target_index >= target_len {
            return None;
        }
    }

    let mut search_index = 0usize;
    let mut success_strict = false;
    let mut matches_strict = Vec::with_capacity(search_len);
    let mut target_index = if matches_simple[0] == 0 { 0 } else { next_beginning_indexes[matches_simple[0] - 1] };
    let mut backtrack_count = 0usize;
    if target_index != target_len {
        loop {
            if target_index >= target_len {
                if search_index == 0 {
                    break;
                }
                backtrack_count += 1;
                if backtrack_count > 200 {
                    break;
                }
                search_index -= 1;
                let last_match = matches_strict.pop().expect("a strict match for every consumed search unit");
                target_index = next_beginning_indexes[last_match];
            } else if search_codes[search_index] == target_codes[target_index] {
                matches_strict.push(target_index);
                search_index += 1;
                if search_index == search_len {
                    success_strict = true;
                    break;
                }
                target_index += 1;
            } else {
                target_index = next_beginning_indexes[target_index];
            }
        }
    }

    let mut substring_index =
        if search_len <= 1 { None } else { index_of(target_codes, search_codes, matches_simple[0]) };
    let mut is_substring = substring_index.is_some();
    let mut is_substring_beginning = match substring_index {
        Some(index) => index == 0 || next_beginning_indexes[index - 1] == index,
        None => false,
    };
    if is_substring && !is_substring_beginning {
        let mut index = 0usize;
        while index < next_beginning_indexes.len() {
            if index > substring_index.expect("a substring index") {
                let matched =
                    (0..search_len).all(|offset| target_codes.get(index + offset) == Some(&search_codes[offset]));
                if matched {
                    substring_index = Some(index);
                    is_substring_beginning = true;
                    break;
                }
            }
            let step = next_beginning_indexes[index];
            if step <= index {
                break;
            }
            index = step;
        }
        is_substring = substring_index.is_some();
    }

    let matches_best = if !success_strict {
        if let Some(index) = substring_index {
            for (offset, slot) in matches_simple.iter_mut().enumerate() {
                *slot = index + offset;
            }
        }
        &matches_simple
    } else if is_substring_beginning {
        let index = substring_index.expect("a substring beginning index");
        for (offset, slot) in matches_simple.iter_mut().enumerate() {
            *slot = index + offset;
        }
        &matches_simple
    } else {
        &matches_strict
    };

    let raw_score = calculate_score(
        search_len,
        target_len,
        success_strict,
        is_substring,
        is_substring_beginning,
        next_beginning_indexes,
        matches_best,
    );
    let mut indexes = matches_best.clone();
    indexes.sort_unstable();
    Some(Found { target: target.target.clone(), score: normalize_score(raw_score), raw_score, indexes })
}

fn algorithm_spaces(search_codes: &[u16], space_searches: &[Vec<u16>], target: &Prepared) -> Option<Found> {
    let search_len = space_searches.len();
    if search_len == 0 {
        return None;
    }
    let mut next_beginning_indexes = target.next_beginning_indexes.clone();
    let mut seen: Vec<usize> = Vec::new();
    let mut score = 0.0;
    let mut result: Option<Found> = None;
    let mut first_seen_index_last_search = 0usize;
    for (index, search) in space_searches.iter().enumerate() {
        let found = algorithm(search, &[], false, target, &next_beginning_indexes, false)?;
        if index + 1 != search_len && !found.indexes.is_empty() {
            let consecutive = found.indexes.windows(2).all(|pair| pair[1] == pair[0] + 1);
            if consecutive {
                let new_beginning_index = found.indexes[found.indexes.len() - 1] + 1;
                if new_beginning_index < next_beginning_indexes.len() {
                    let to_replace = next_beginning_indexes[new_beginning_index - 1];
                    let mut position = new_beginning_index - 1;
                    loop {
                        if next_beginning_indexes[position] != to_replace {
                            break;
                        }
                        next_beginning_indexes[position] = new_beginning_index;
                        if position == 0 {
                            break;
                        }
                        position -= 1;
                    }
                }
            }
        }
        score += found.raw_score / search_len as f64;
        let first_index = found.indexes[0];
        if first_index < first_seen_index_last_search {
            score -= (first_seen_index_last_search - first_index) as f64 * 2.0;
        }
        first_seen_index_last_search = first_index;
        for index in &found.indexes {
            if !seen.contains(index) {
                seen.push(*index);
            }
        }
        result = Some(found);
    }

    let reset = target.next_beginning_indexes.clone();
    if let Some(found) = algorithm(search_codes, space_searches, true, target, &reset, true)
        && found.raw_score > score
    {
        return Some(found);
    }

    let mut found = result.expect("a successful space search");
    found.raw_score = score;
    found.score = normalize_score(score);
    found.indexes = seen;
    found.indexes.sort_unstable();
    Some(found)
}

fn calculate_score(
    search_len: usize,
    target_len: usize,
    success_strict: bool,
    is_substring: bool,
    is_substring_beginning: bool,
    next_beginning_indexes: &[usize],
    matches: &[usize],
) -> f64 {
    let mut score = 0.0;
    let mut extra_match_group_count = 0.0;
    for index in 1..search_len {
        if matches[index] != matches[index - 1] + 1 {
            score -= matches[index] as f64;
            extra_match_group_count += 1.0;
        }
    }
    let unmatched_distance = (matches[search_len - 1] - matches[0]) as f64 - (search_len as f64 - 1.0);
    score -= (12.0 + unmatched_distance) * extra_match_group_count;
    if matches[0] != 0 {
        score -= (matches[0] * matches[0]) as f64 * 0.2;
    }
    if !success_strict {
        score *= 1000.0;
    } else {
        let mut unique_beginning_indexes = 1usize;
        let mut index = next_beginning_indexes[0];
        while index < target_len {
            unique_beginning_indexes += 1;
            index = next_beginning_indexes[index];
        }
        if unique_beginning_indexes > 24 {
            score *= (unique_beginning_indexes - 24) as f64 * 10.0;
        }
    }
    score -= (target_len - search_len) as f64 / 2.0;
    if is_substring {
        score /= 1.0 + (search_len * search_len) as f64;
    }
    if is_substring_beginning {
        score /= 1.0 + (search_len * search_len) as f64;
    }
    score -= (target_len - search_len) as f64 / 2.0;
    score
}

fn normalize_score(score: f64) -> f64 {
    if score == NEGATIVE_INFINITY {
        return 0.0;
    }
    if score > 1.0 {
        return score;
    }
    (((1.0 - score).powf(0.04307) - 1.0) * -2.0).exp()
}

fn lower_info(value: &str) -> (Vec<u16>, u32) {
    let lower = remove_accents(value).to_lowercase();
    let codes: Vec<u16> = lower.encode_utf16().collect();
    let mut bitflags = 0u32;
    for &code in &codes {
        if code == 32 {
            continue;
        }
        let bit = if (97..=122).contains(&code) {
            code - 97
        } else if (48..=57).contains(&code) {
            26
        } else if code <= 127 {
            30
        } else {
            31
        };
        bitflags |= 1u32 << bit;
    }
    (codes, bitflags)
}

fn prepare_next_beginning_indexes(target: &str) -> Vec<usize> {
    let folded = remove_accents(target);
    let codes: Vec<u16> = folded.encode_utf16().collect();
    let target_len = codes.len();
    let mut beginning_indexes = Vec::new();
    let mut was_upper = false;
    let mut was_alphanumeric = false;
    for (index, &code) in codes.iter().enumerate() {
        let is_upper = (65..=90).contains(&code);
        let is_alphanumeric = is_upper || (97..=122).contains(&code) || (48..=57).contains(&code);
        if (is_upper && !was_upper) || !was_alphanumeric || !is_alphanumeric {
            beginning_indexes.push(index);
        }
        was_upper = is_upper;
        was_alphanumeric = is_alphanumeric;
    }
    let mut next = vec![target_len; target_len];
    if target_len == 0 {
        return next;
    }
    let mut last = beginning_indexes.first().copied();
    let mut last_index = 0usize;
    for (index, slot) in next.iter_mut().enumerate() {
        match last {
            Some(value) if value > index => *slot = value,
            _ => {
                last_index += 1;
                last = beginning_indexes.get(last_index).copied();
                *slot = last.unwrap_or(target_len);
            }
        }
    }
    next
}

fn index_of(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= haystack.len()).then_some(from);
    }
    if from > haystack.len() || haystack.len() - from < needle.len() {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|window| window == needle).map(|position| position + from)
}

fn remove_accents(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        let code = character as u32;
        if (0x0300..=0x036f).contains(&code) {
            continue;
        }
        result.push(match ACCENTS.binary_search_by_key(&character, |entry| entry.0) {
            Ok(index) => ACCENTS[index].1,
            Err(_) => character,
        });
    }
    result
}

const ACCENTS: &[(char, char)] = &[
    ('\u{c0}', 'A'),
    ('\u{c1}', 'A'),
    ('\u{c2}', 'A'),
    ('\u{c3}', 'A'),
    ('\u{c4}', 'A'),
    ('\u{c5}', 'A'),
    ('\u{c7}', 'C'),
    ('\u{c8}', 'E'),
    ('\u{c9}', 'E'),
    ('\u{ca}', 'E'),
    ('\u{cb}', 'E'),
    ('\u{cc}', 'I'),
    ('\u{cd}', 'I'),
    ('\u{ce}', 'I'),
    ('\u{cf}', 'I'),
    ('\u{d1}', 'N'),
    ('\u{d2}', 'O'),
    ('\u{d3}', 'O'),
    ('\u{d4}', 'O'),
    ('\u{d5}', 'O'),
    ('\u{d6}', 'O'),
    ('\u{d9}', 'U'),
    ('\u{da}', 'U'),
    ('\u{db}', 'U'),
    ('\u{dc}', 'U'),
    ('\u{dd}', 'Y'),
    ('\u{e0}', 'a'),
    ('\u{e1}', 'a'),
    ('\u{e2}', 'a'),
    ('\u{e3}', 'a'),
    ('\u{e4}', 'a'),
    ('\u{e5}', 'a'),
    ('\u{e7}', 'c'),
    ('\u{e8}', 'e'),
    ('\u{e9}', 'e'),
    ('\u{ea}', 'e'),
    ('\u{eb}', 'e'),
    ('\u{ec}', 'i'),
    ('\u{ed}', 'i'),
    ('\u{ee}', 'i'),
    ('\u{ef}', 'i'),
    ('\u{f1}', 'n'),
    ('\u{f2}', 'o'),
    ('\u{f3}', 'o'),
    ('\u{f4}', 'o'),
    ('\u{f5}', 'o'),
    ('\u{f6}', 'o'),
    ('\u{f9}', 'u'),
    ('\u{fa}', 'u'),
    ('\u{fb}', 'u'),
    ('\u{fc}', 'u'),
    ('\u{fd}', 'y'),
    ('\u{ff}', 'y'),
    ('\u{100}', 'A'),
    ('\u{101}', 'a'),
    ('\u{102}', 'A'),
    ('\u{103}', 'a'),
    ('\u{104}', 'A'),
    ('\u{105}', 'a'),
    ('\u{106}', 'C'),
    ('\u{107}', 'c'),
    ('\u{108}', 'C'),
    ('\u{109}', 'c'),
    ('\u{10a}', 'C'),
    ('\u{10b}', 'c'),
    ('\u{10c}', 'C'),
    ('\u{10d}', 'c'),
    ('\u{10e}', 'D'),
    ('\u{10f}', 'd'),
    ('\u{112}', 'E'),
    ('\u{113}', 'e'),
    ('\u{114}', 'E'),
    ('\u{115}', 'e'),
    ('\u{116}', 'E'),
    ('\u{117}', 'e'),
    ('\u{118}', 'E'),
    ('\u{119}', 'e'),
    ('\u{11a}', 'E'),
    ('\u{11b}', 'e'),
    ('\u{11c}', 'G'),
    ('\u{11d}', 'g'),
    ('\u{11e}', 'G'),
    ('\u{11f}', 'g'),
    ('\u{120}', 'G'),
    ('\u{121}', 'g'),
    ('\u{122}', 'G'),
    ('\u{123}', 'g'),
    ('\u{124}', 'H'),
    ('\u{125}', 'h'),
    ('\u{128}', 'I'),
    ('\u{129}', 'i'),
    ('\u{12a}', 'I'),
    ('\u{12b}', 'i'),
    ('\u{12c}', 'I'),
    ('\u{12d}', 'i'),
    ('\u{12e}', 'I'),
    ('\u{12f}', 'i'),
    ('\u{130}', 'I'),
    ('\u{134}', 'J'),
    ('\u{135}', 'j'),
    ('\u{136}', 'K'),
    ('\u{137}', 'k'),
    ('\u{139}', 'L'),
    ('\u{13a}', 'l'),
    ('\u{13b}', 'L'),
    ('\u{13c}', 'l'),
    ('\u{13d}', 'L'),
    ('\u{13e}', 'l'),
    ('\u{143}', 'N'),
    ('\u{144}', 'n'),
    ('\u{145}', 'N'),
    ('\u{146}', 'n'),
    ('\u{147}', 'N'),
    ('\u{148}', 'n'),
    ('\u{14c}', 'O'),
    ('\u{14d}', 'o'),
    ('\u{14e}', 'O'),
    ('\u{14f}', 'o'),
    ('\u{150}', 'O'),
    ('\u{151}', 'o'),
    ('\u{154}', 'R'),
    ('\u{155}', 'r'),
    ('\u{156}', 'R'),
    ('\u{157}', 'r'),
    ('\u{158}', 'R'),
    ('\u{159}', 'r'),
    ('\u{15a}', 'S'),
    ('\u{15b}', 's'),
    ('\u{15c}', 'S'),
    ('\u{15d}', 's'),
    ('\u{15e}', 'S'),
    ('\u{15f}', 's'),
    ('\u{160}', 'S'),
    ('\u{161}', 's'),
    ('\u{162}', 'T'),
    ('\u{163}', 't'),
    ('\u{164}', 'T'),
    ('\u{165}', 't'),
    ('\u{168}', 'U'),
    ('\u{169}', 'u'),
    ('\u{16a}', 'U'),
    ('\u{16b}', 'u'),
    ('\u{16c}', 'U'),
    ('\u{16d}', 'u'),
    ('\u{16e}', 'U'),
    ('\u{16f}', 'u'),
    ('\u{170}', 'U'),
    ('\u{171}', 'u'),
    ('\u{172}', 'U'),
    ('\u{173}', 'u'),
    ('\u{174}', 'W'),
    ('\u{175}', 'w'),
    ('\u{176}', 'Y'),
    ('\u{177}', 'y'),
    ('\u{178}', 'Y'),
    ('\u{179}', 'Z'),
    ('\u{17a}', 'z'),
    ('\u{17b}', 'Z'),
    ('\u{17c}', 'z'),
    ('\u{17d}', 'Z'),
    ('\u{17e}', 'z'),
    ('\u{1a0}', 'O'),
    ('\u{1a1}', 'o'),
    ('\u{1af}', 'U'),
    ('\u{1b0}', 'u'),
    ('\u{1cd}', 'A'),
    ('\u{1ce}', 'a'),
    ('\u{1cf}', 'I'),
    ('\u{1d0}', 'i'),
    ('\u{1d1}', 'O'),
    ('\u{1d2}', 'o'),
    ('\u{1d3}', 'U'),
    ('\u{1d4}', 'u'),
    ('\u{1d5}', 'U'),
    ('\u{1d6}', 'u'),
    ('\u{1d7}', 'U'),
    ('\u{1d8}', 'u'),
    ('\u{1d9}', 'U'),
    ('\u{1da}', 'u'),
    ('\u{1db}', 'U'),
    ('\u{1dc}', 'u'),
    ('\u{1de}', 'A'),
    ('\u{1df}', 'a'),
    ('\u{1e0}', 'A'),
    ('\u{1e1}', 'a'),
    ('\u{1e2}', 'Æ'),
    ('\u{1e3}', 'æ'),
    ('\u{1e6}', 'G'),
    ('\u{1e7}', 'g'),
    ('\u{1e8}', 'K'),
    ('\u{1e9}', 'k'),
    ('\u{1ea}', 'O'),
    ('\u{1eb}', 'o'),
    ('\u{1ec}', 'O'),
    ('\u{1ed}', 'o'),
    ('\u{1ee}', 'Ʒ'),
    ('\u{1ef}', 'ʒ'),
    ('\u{1f0}', 'j'),
    ('\u{1f4}', 'G'),
    ('\u{1f5}', 'g'),
    ('\u{1f8}', 'N'),
    ('\u{1f9}', 'n'),
    ('\u{1fa}', 'A'),
    ('\u{1fb}', 'a'),
    ('\u{1fc}', 'Æ'),
    ('\u{1fd}', 'æ'),
    ('\u{1fe}', 'Ø'),
    ('\u{1ff}', 'ø'),
    ('\u{200}', 'A'),
    ('\u{201}', 'a'),
    ('\u{202}', 'A'),
    ('\u{203}', 'a'),
    ('\u{204}', 'E'),
    ('\u{205}', 'e'),
    ('\u{206}', 'E'),
    ('\u{207}', 'e'),
    ('\u{208}', 'I'),
    ('\u{209}', 'i'),
    ('\u{20a}', 'I'),
    ('\u{20b}', 'i'),
    ('\u{20c}', 'O'),
    ('\u{20d}', 'o'),
    ('\u{20e}', 'O'),
    ('\u{20f}', 'o'),
    ('\u{210}', 'R'),
    ('\u{211}', 'r'),
    ('\u{212}', 'R'),
    ('\u{213}', 'r'),
    ('\u{214}', 'U'),
    ('\u{215}', 'u'),
    ('\u{216}', 'U'),
    ('\u{217}', 'u'),
    ('\u{218}', 'S'),
    ('\u{219}', 's'),
    ('\u{21a}', 'T'),
    ('\u{21b}', 't'),
    ('\u{21e}', 'H'),
    ('\u{21f}', 'h'),
    ('\u{226}', 'A'),
    ('\u{227}', 'a'),
    ('\u{228}', 'E'),
    ('\u{229}', 'e'),
    ('\u{22a}', 'O'),
    ('\u{22b}', 'o'),
    ('\u{22c}', 'O'),
    ('\u{22d}', 'o'),
    ('\u{22e}', 'O'),
    ('\u{22f}', 'o'),
    ('\u{230}', 'O'),
    ('\u{231}', 'o'),
    ('\u{232}', 'Y'),
    ('\u{233}', 'y'),
    ('\u{1e00}', 'A'),
    ('\u{1e01}', 'a'),
    ('\u{1e02}', 'B'),
    ('\u{1e03}', 'b'),
    ('\u{1e04}', 'B'),
    ('\u{1e05}', 'b'),
    ('\u{1e06}', 'B'),
    ('\u{1e07}', 'b'),
    ('\u{1e08}', 'C'),
    ('\u{1e09}', 'c'),
    ('\u{1e0a}', 'D'),
    ('\u{1e0b}', 'd'),
    ('\u{1e0c}', 'D'),
    ('\u{1e0d}', 'd'),
    ('\u{1e0e}', 'D'),
    ('\u{1e0f}', 'd'),
    ('\u{1e10}', 'D'),
    ('\u{1e11}', 'd'),
    ('\u{1e12}', 'D'),
    ('\u{1e13}', 'd'),
    ('\u{1e14}', 'E'),
    ('\u{1e15}', 'e'),
    ('\u{1e16}', 'E'),
    ('\u{1e17}', 'e'),
    ('\u{1e18}', 'E'),
    ('\u{1e19}', 'e'),
    ('\u{1e1a}', 'E'),
    ('\u{1e1b}', 'e'),
    ('\u{1e1c}', 'E'),
    ('\u{1e1d}', 'e'),
    ('\u{1e1e}', 'F'),
    ('\u{1e1f}', 'f'),
    ('\u{1e20}', 'G'),
    ('\u{1e21}', 'g'),
    ('\u{1e22}', 'H'),
    ('\u{1e23}', 'h'),
    ('\u{1e24}', 'H'),
    ('\u{1e25}', 'h'),
    ('\u{1e26}', 'H'),
    ('\u{1e27}', 'h'),
    ('\u{1e28}', 'H'),
    ('\u{1e29}', 'h'),
    ('\u{1e2a}', 'H'),
    ('\u{1e2b}', 'h'),
    ('\u{1e2c}', 'I'),
    ('\u{1e2d}', 'i'),
    ('\u{1e2e}', 'I'),
    ('\u{1e2f}', 'i'),
    ('\u{1e30}', 'K'),
    ('\u{1e31}', 'k'),
    ('\u{1e32}', 'K'),
    ('\u{1e33}', 'k'),
    ('\u{1e34}', 'K'),
    ('\u{1e35}', 'k'),
    ('\u{1e36}', 'L'),
    ('\u{1e37}', 'l'),
    ('\u{1e38}', 'L'),
    ('\u{1e39}', 'l'),
    ('\u{1e3a}', 'L'),
    ('\u{1e3b}', 'l'),
    ('\u{1e3c}', 'L'),
    ('\u{1e3d}', 'l'),
    ('\u{1e3e}', 'M'),
    ('\u{1e3f}', 'm'),
    ('\u{1e40}', 'M'),
    ('\u{1e41}', 'm'),
    ('\u{1e42}', 'M'),
    ('\u{1e43}', 'm'),
    ('\u{1e44}', 'N'),
    ('\u{1e45}', 'n'),
    ('\u{1e46}', 'N'),
    ('\u{1e47}', 'n'),
    ('\u{1e48}', 'N'),
    ('\u{1e49}', 'n'),
    ('\u{1e4a}', 'N'),
    ('\u{1e4b}', 'n'),
    ('\u{1e4c}', 'O'),
    ('\u{1e4d}', 'o'),
    ('\u{1e4e}', 'O'),
    ('\u{1e4f}', 'o'),
    ('\u{1e50}', 'O'),
    ('\u{1e51}', 'o'),
    ('\u{1e52}', 'O'),
    ('\u{1e53}', 'o'),
    ('\u{1e54}', 'P'),
    ('\u{1e55}', 'p'),
    ('\u{1e56}', 'P'),
    ('\u{1e57}', 'p'),
    ('\u{1e58}', 'R'),
    ('\u{1e59}', 'r'),
    ('\u{1e5a}', 'R'),
    ('\u{1e5b}', 'r'),
    ('\u{1e5c}', 'R'),
    ('\u{1e5d}', 'r'),
    ('\u{1e5e}', 'R'),
    ('\u{1e5f}', 'r'),
    ('\u{1e60}', 'S'),
    ('\u{1e61}', 's'),
    ('\u{1e62}', 'S'),
    ('\u{1e63}', 's'),
    ('\u{1e64}', 'S'),
    ('\u{1e65}', 's'),
    ('\u{1e66}', 'S'),
    ('\u{1e67}', 's'),
    ('\u{1e68}', 'S'),
    ('\u{1e69}', 's'),
    ('\u{1e6a}', 'T'),
    ('\u{1e6b}', 't'),
    ('\u{1e6c}', 'T'),
    ('\u{1e6d}', 't'),
    ('\u{1e6e}', 'T'),
    ('\u{1e6f}', 't'),
    ('\u{1e70}', 'T'),
    ('\u{1e71}', 't'),
    ('\u{1e72}', 'U'),
    ('\u{1e73}', 'u'),
    ('\u{1e74}', 'U'),
    ('\u{1e75}', 'u'),
    ('\u{1e76}', 'U'),
    ('\u{1e77}', 'u'),
    ('\u{1e78}', 'U'),
    ('\u{1e79}', 'u'),
    ('\u{1e7a}', 'U'),
    ('\u{1e7b}', 'u'),
    ('\u{1e7c}', 'V'),
    ('\u{1e7d}', 'v'),
    ('\u{1e7e}', 'V'),
    ('\u{1e7f}', 'v'),
    ('\u{1e80}', 'W'),
    ('\u{1e81}', 'w'),
    ('\u{1e82}', 'W'),
    ('\u{1e83}', 'w'),
    ('\u{1e84}', 'W'),
    ('\u{1e85}', 'w'),
    ('\u{1e86}', 'W'),
    ('\u{1e87}', 'w'),
    ('\u{1e88}', 'W'),
    ('\u{1e89}', 'w'),
    ('\u{1e8a}', 'X'),
    ('\u{1e8b}', 'x'),
    ('\u{1e8c}', 'X'),
    ('\u{1e8d}', 'x'),
    ('\u{1e8e}', 'Y'),
    ('\u{1e8f}', 'y'),
    ('\u{1e90}', 'Z'),
    ('\u{1e91}', 'z'),
    ('\u{1e92}', 'Z'),
    ('\u{1e93}', 'z'),
    ('\u{1e94}', 'Z'),
    ('\u{1e95}', 'z'),
    ('\u{1e96}', 'h'),
    ('\u{1e97}', 't'),
    ('\u{1e98}', 'w'),
    ('\u{1e99}', 'y'),
    ('\u{1e9b}', 'ſ'),
    ('\u{1ea0}', 'A'),
    ('\u{1ea1}', 'a'),
    ('\u{1ea2}', 'A'),
    ('\u{1ea3}', 'a'),
    ('\u{1ea4}', 'A'),
    ('\u{1ea5}', 'a'),
    ('\u{1ea6}', 'A'),
    ('\u{1ea7}', 'a'),
    ('\u{1ea8}', 'A'),
    ('\u{1ea9}', 'a'),
    ('\u{1eaa}', 'A'),
    ('\u{1eab}', 'a'),
    ('\u{1eac}', 'A'),
    ('\u{1ead}', 'a'),
    ('\u{1eae}', 'A'),
    ('\u{1eaf}', 'a'),
    ('\u{1eb0}', 'A'),
    ('\u{1eb1}', 'a'),
    ('\u{1eb2}', 'A'),
    ('\u{1eb3}', 'a'),
    ('\u{1eb4}', 'A'),
    ('\u{1eb5}', 'a'),
    ('\u{1eb6}', 'A'),
    ('\u{1eb7}', 'a'),
    ('\u{1eb8}', 'E'),
    ('\u{1eb9}', 'e'),
    ('\u{1eba}', 'E'),
    ('\u{1ebb}', 'e'),
    ('\u{1ebc}', 'E'),
    ('\u{1ebd}', 'e'),
    ('\u{1ebe}', 'E'),
    ('\u{1ebf}', 'e'),
    ('\u{1ec0}', 'E'),
    ('\u{1ec1}', 'e'),
    ('\u{1ec2}', 'E'),
    ('\u{1ec3}', 'e'),
    ('\u{1ec4}', 'E'),
    ('\u{1ec5}', 'e'),
    ('\u{1ec6}', 'E'),
    ('\u{1ec7}', 'e'),
    ('\u{1ec8}', 'I'),
    ('\u{1ec9}', 'i'),
    ('\u{1eca}', 'I'),
    ('\u{1ecb}', 'i'),
    ('\u{1ecc}', 'O'),
    ('\u{1ecd}', 'o'),
    ('\u{1ece}', 'O'),
    ('\u{1ecf}', 'o'),
    ('\u{1ed0}', 'O'),
    ('\u{1ed1}', 'o'),
    ('\u{1ed2}', 'O'),
    ('\u{1ed3}', 'o'),
    ('\u{1ed4}', 'O'),
    ('\u{1ed5}', 'o'),
    ('\u{1ed6}', 'O'),
    ('\u{1ed7}', 'o'),
    ('\u{1ed8}', 'O'),
    ('\u{1ed9}', 'o'),
    ('\u{1eda}', 'O'),
    ('\u{1edb}', 'o'),
    ('\u{1edc}', 'O'),
    ('\u{1edd}', 'o'),
    ('\u{1ede}', 'O'),
    ('\u{1edf}', 'o'),
    ('\u{1ee0}', 'O'),
    ('\u{1ee1}', 'o'),
    ('\u{1ee2}', 'O'),
    ('\u{1ee3}', 'o'),
    ('\u{1ee4}', 'U'),
    ('\u{1ee5}', 'u'),
    ('\u{1ee6}', 'U'),
    ('\u{1ee7}', 'u'),
    ('\u{1ee8}', 'U'),
    ('\u{1ee9}', 'u'),
    ('\u{1eea}', 'U'),
    ('\u{1eeb}', 'u'),
    ('\u{1eec}', 'U'),
    ('\u{1eed}', 'u'),
    ('\u{1eee}', 'U'),
    ('\u{1eef}', 'u'),
    ('\u{1ef0}', 'U'),
    ('\u{1ef1}', 'u'),
    ('\u{1ef2}', 'Y'),
    ('\u{1ef3}', 'y'),
    ('\u{1ef4}', 'Y'),
    ('\u{1ef5}', 'y'),
    ('\u{1ef6}', 'Y'),
    ('\u{1ef7}', 'y'),
    ('\u{1ef8}', 'Y'),
    ('\u{1ef9}', 'y'),
    ('\u{212a}', 'K'),
    ('\u{212b}', 'A'),
];

#[derive(Default)]
struct MinHeap {
    items: Vec<Found>,
}

impl MinHeap {
    fn add(&mut self, result: Found) {
        self.items.push(result);
        let mut index = self.items.len() - 1;
        while index > 0 {
            let parent = (index - 1) >> 1;
            if self.items[index].raw_score < self.items[parent].raw_score {
                self.items.swap(index, parent);
                index = parent;
            } else {
                break;
            }
        }
    }

    fn replace_top(&mut self, result: Found) {
        if self.items.is_empty() {
            self.items.push(result);
            return;
        }
        self.items[0] = result;
        self.sift_down();
    }

    fn poll(&mut self) -> Option<Found> {
        if self.items.is_empty() {
            return None;
        }
        let top = self.items.swap_remove(0);
        self.sift_down();
        Some(top)
    }

    fn sift_down(&mut self) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let value = std::mem::take(&mut self.items[0]);
        let mut hole = 0usize;
        let mut child = 1usize;
        while child < len {
            let right = child + 1;
            hole = child;
            if right < len && self.items[right].raw_score < self.items[child].raw_score {
                hole = right;
            }
            let parent = (hole - 1) >> 1;
            self.items[parent] = std::mem::take(&mut self.items[hole]);
            child = 1 + (hole << 1);
        }
        while hole > 0 {
            let parent = (hole - 1) >> 1;
            if value.raw_score < self.items[parent].raw_score {
                self.items[hole] = std::mem::take(&mut self.items[parent]);
                hole = parent;
            } else {
                break;
            }
        }
        self.items[hole] = value;
    }
}
