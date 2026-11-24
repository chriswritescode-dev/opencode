pub fn group<Item, Key: PartialEq>(items: Vec<Item>, speaker: impl Fn(&Item) -> Key) -> Vec<Vec<Item>> {
    let mut turns: Vec<Vec<Item>> = Vec::new();
    for item in items {
        let key = speaker(&item);
        let continues = turns.last().is_some_and(|turn| turn.first().is_some_and(|first| speaker(first) == key));
        if continues {
            if let Some(turn) = turns.last_mut() {
                turn.push(item);
            }
            continue;
        }
        turns.push(vec![item]);
    }
    turns
}
