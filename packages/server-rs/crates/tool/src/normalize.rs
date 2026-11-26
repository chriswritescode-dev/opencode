pub fn normalize_for_match(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '‘' | '’' | '‚' | '‛' => '\'',
            '“' | '”' | '„' | '‟' => '"',
            '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            character => character,
        })
        .collect()
}
