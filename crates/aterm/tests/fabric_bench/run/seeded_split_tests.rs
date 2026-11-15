// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The bench target compiles this module too, where `#[test]` functions are not: each test
// imports what it uses, so nothing here is left unused there.

#[test]
fn a_broker_s_stamps_split_its_start_from_the_bench_s_spawn() {
    use super::*;
    use aterm_link::start_stamps::PREFIX;

    let stderr = |line: &str| {
        format!("aterm link broker: a note\n{line}\naterm link broker: unix socket on /s\n")
    };
    let line = format!(
        "{PREFIX} main=1100000 broker=2100000 key=3100000 opened=903100000 locked=4100000 \
         armed=903600000 rules=904100000 served=905100000 listening=906100000"
    );
    let split = Split::of(&stderr(&line), Some(100_000), Some(907_100_000)).expect("a split");
    let ends: Vec<&str> = split.steps.iter().map(|(_, to, _)| to.as_str()).collect();
    assert_eq!(ends, split_ends().collect::<Vec<_>>());
    assert_eq!(split.ms_to("opened"), Some(899.0));
    assert_eq!(split.ms_to("seen"), Some(1.0));
    assert_eq!(split.total_ms(), 907.0);
    assert!(
        split
            .brief()
            .starts_with("spawn -> main 1.0, main -> broker 1.0, ")
    );
    let json = split.json();
    assert_eq!(json["spawn_to_seen_ms"].as_f64(), Some(907.0));
    assert_eq!(json["steps"][4]["from"].as_str(), Some("locked"));
    assert_eq!(json["steps"][4]["ms"].as_f64(), Some(899.0));
}

#[test]
fn no_stamps_line_or_no_spawn_read_is_no_split() {
    use super::*;
    use aterm_link::start_stamps::Stamps;

    let stderr = |line: &str| {
        format!("aterm link broker: a note\n{line}\naterm link broker: unix socket on /s\n")
    };
    assert_eq!(Split::of("listening /s\n", Some(1), Some(2)), None);
    let line = Stamps::default().line();
    assert_eq!(Split::of(&stderr(&line), None, Some(2)), None);
    assert!(Split::of(&stderr(&line), Some(1), Some(2)).is_some());
}

#[test]
fn the_table_has_a_column_for_each_step_and_a_dash_where_a_start_has_none() {
    use super::*;
    use aterm_link::start_stamps::PREFIX;

    let stderr = |line: &str| {
        format!("aterm link broker: a note\n{line}\naterm link broker: unix socket on /s\n")
    };
    let full = Split::of(
        &stderr(&format!(
            "{PREFIX} main=2000000 broker=3000000 key=4000000 locked=5000000 opened=6000000 \
             armed=6500000 rules=7000000 served=8000000 listening=9000000"
        )),
        Some(1_000_000),
        Some(10_000_000),
    )
    .expect("a split");
    // No `main` (a broker entered other than through `aterm`'s main): its time is the broker's.
    let folded = Split::of(
        &stderr(&format!("{PREFIX} broker=3000000 listening=9000000")),
        Some(1_000_000),
        None,
    )
    .expect("a split");
    let md = split_table(&[
        ("sealed", Some(&full)),
        ("plain", Some(&folded)),
        ("none", None),
    ]);
    let cells = |row: &str| row.split('|').count();
    assert_eq!(cells(&md[0]), cells(&md[1]));
    assert_eq!(cells(&md[0]), cells(&md[2]));
    assert_eq!(
        md[2],
        "| sealed | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 0.5 | 0.5 | 1.0 | 1.0 | 1.0 | 9.0 |"
    );
    assert_eq!(
        md[3],
        "| plain | – | 2.0 | – | – | – | – | – | – | 6.0 | – | 8.0 |"
    );
    assert_eq!(
        md[4],
        "| none | – | – | – | – | – | – | – | – | – | – | – |"
    );
    assert_eq!(md.last().map(String::as_str), Some(""));
}
