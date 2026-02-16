const HAIKUS: [[&str; 3]; 10] = [
    [
        "commit by commit",
        "the path reveals what was done",
        "nothing forgotten",
    ],
    [
        "who changed this and why?",
        "the graph remembers it all",
        "blame without the shame",
    ],
    [
        "a dead end appears",
        "not failure but evidence",
        "of roads not taken",
    ],
    [
        "steps form a river",
        "each diff a stone underneath",
        "flow is provenance",
    ],
    [
        "the merge base whispers",
        "from where branches first diverged",
        "common ancestor",
    ],
    [
        "artifact transformed",
        "raw diff to structural change",
        "two lenses, one truth",
    ],
    [
        "human then agent",
        "then tool — the actor chain shows",
        "hands that shaped the code",
    ],
    [
        "intent in the step",
        "what you meant, not what you typed",
        "the why survives all",
    ],
    [
        "validate the doc",
        "every field in its place",
        "the schema holds firm",
    ],
    [
        "graph of many paths",
        "release told as a story",
        "beginning to end",
    ],
];

pub fn run() {
    use rand::Rng;
    let i = rand::rng().random_range(0..HAIKUS.len());
    println!("{}", HAIKUS[i].join("\n"));
}
