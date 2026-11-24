const ADJECTIVES: [&str; 29] = [
    "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing", "happy", "hidden", "jolly",
    "kind", "lucky", "mighty", "misty", "neon", "nimble", "playful", "proud", "quick", "quiet", "shiny", "silent",
    "stellar", "sunny", "swift", "tidy", "witty",
];

const NOUNS: [&str; 31] = [
    "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest", "garden", "harbor",
    "island", "knight", "lagoon", "meadow", "moon", "mountain", "nebula", "orchid", "otter", "panda", "pixel",
    "planet", "river", "rocket", "sailor", "squid", "star", "tiger", "wizard", "wolf",
];

pub(crate) fn create() -> String {
    format!("{}-{}", ADJECTIVES[rand::random_range(0..ADJECTIVES.len())], NOUNS[rand::random_range(0..NOUNS.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_joins_a_known_adjective_and_noun() {
        let slug = create();
        let mut parts = slug.split('-');
        let adjective = parts.next().expect("a slug carries an adjective");
        let noun = parts.next().expect("a slug carries a noun");
        assert!(parts.next().is_none(), "a slug carries two parts");
        assert!(ADJECTIVES.contains(&adjective), "{adjective} is a known adjective");
        assert!(NOUNS.contains(&noun), "{noun} is a known noun");
    }
}
