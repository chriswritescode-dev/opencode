use opencode_storage::id;

fn is_id_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
}

#[test]
fn ascending_ids_sort_ascending() {
    let _guard = crate::lock_identifiers();
    let generated: Vec<String> = (0..8).map(|_| id::create(false, 1_000_000_000_000)).collect();
    let mut sorted = generated.clone();
    sorted.sort();
    assert_eq!(generated, sorted, "ascending ids sort ascending");
    for value in &generated {
        assert_eq!(value.len(), 26, "an id is 26 characters: {value}");
        assert!(value.chars().all(is_id_character), "an id is alphanumeric: {value}");
    }
}

#[test]
fn descending_ids_sort_descending() {
    let _guard = crate::lock_identifiers();
    let generated: Vec<String> = (0..8).map(|_| id::create(true, 1_000_000_000_001)).collect();
    let mut sorted = generated.clone();
    sorted.sort();
    sorted.reverse();
    assert_eq!(generated, sorted, "descending ids sort descending");
    for value in &generated {
        assert_eq!(value.len(), 26, "an id is 26 characters: {value}");
        assert!(value.chars().all(is_id_character), "an id is alphanumeric: {value}");
    }
}

#[test]
fn a_zero_timestamp_starts_at_one() {
    let _guard = crate::lock_identifiers();
    let value = id::create(false, 0);
    assert!(value.starts_with("000000000001"), "the first zero-timestamp id starts at one: {value}");
    assert_eq!(value.len(), 26, "an id is 26 characters: {value}");
}

#[test]
fn known_timestamp_ids_match_the_pinned_typescript() {
    let _guard = crate::lock_identifiers();
    id::create(false, 0);
    let ascending = id::create(false, 1_700_000_000_000);
    id::create(false, 0);
    let descending = id::create(true, 1_700_000_000_000);

    assert_eq!(&ascending[..12], "bcfe56800001", "the first ascending id after a counter reset: {ascending}");
    assert_eq!(&descending[..12], "4301a97ffffe", "the first descending id after a counter reset: {descending}");
}

#[test]
fn ascending_and_descending_use_the_clock() {
    let _guard = crate::lock_identifiers();
    assert_eq!(id::ascending().len(), 26, "a clock-based ascending id is 26 characters");
    assert_eq!(id::descending().len(), 26, "a clock-based descending id is 26 characters");
}
