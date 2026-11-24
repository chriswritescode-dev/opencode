use regex_lite::RegexBuilder;

pub fn matches(input: &str, pattern: &str, ignore_case: bool) -> bool {
    let normalized = input.replace('\\', "/");
    let mut escaped = escape(&pattern.replace('\\', "/"));

    if escaped.ends_with(" .*") {
        escaped.truncate(escaped.len() - 3);
        escaped.push_str("( .*)?");
    }

    let source = format!("^{escaped}$");

    RegexBuilder::new(&source)
        .dot_matches_new_line(true)
        .case_insensitive(cfg!(windows) || ignore_case)
        .build()
        .expect("the escaped wildcard pattern is a valid regular expression")
        .is_match(&normalized)
}

fn escape(pattern: &str) -> String {
    pattern
        .chars()
        .flat_map(|character| match character {
            '*' => vec!['.', '*'],
            '?' => vec!['.'],
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => vec!['\\', character],
            other => vec![other],
        })
        .collect()
}
