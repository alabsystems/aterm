// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Measurements of the open-work reader over its three corpora (`turn_end_corpora_tests.rs`).
// They call only the readers' long-standing doors (`open_work`, `open_work_strict`,
// `open_work_read_as`, `land_open_words`, `clean_defense`), so this file and the corpora's run
// on an older tree too and the two runs compare.

use super::corpora::corpus_rows;

/// What each reader reads in `reply`, by the reader's name.
fn readings(reply: &str) -> Vec<(&'static str, String)> {
    vec![
        ("open_work", format!("{:?}", open_work(Some(reply)))),
        (
            "open_work+finished",
            format!("{:?}", open_work(Some(&format!("{reply}{FINISHED}")))),
        ),
        (
            "open_work_strict",
            format!("{:?}", open_work_strict(Some(reply))),
        ),
        (
            "open_work_strict(defense_body)",
            format!("{:?}", open_work_strict(Some(&needs::defense_body(reply)))),
        ),
        (
            "open_work_read_as(without_needs, land, no defense)",
            format!(
                "{:?}",
                open_work_read_as(&needs::without_needs(reply), Some(&LAND_FAMILIES), false)
            ),
        ),
        (
            "land_open_words",
            format!("{:?}", land_open_words(Some(reply))),
        ),
        ("clean_defense", format!("{:?}", clean_defense(Some(reply)))),
    ]
}

#[test]
#[ignore = "a measurement: ATERM_OPEN_WORK_DUMP=<file> writes what each reader reads on every \
            corpus row, and ATERM_OPEN_WORK_AGAINST=<file> holds them to another tree's dump"]
fn the_open_work_readers_over_the_corpora() {
    let mut dump = String::new();
    for (corpus, row, reply) in corpus_rows() {
        for (reader, read) in readings(&reply) {
            dump.push_str(&format!("{corpus}\t{row}\t{reader}\t{read}\n"));
        }
    }
    let lines = dump.lines().count();
    let open = dump.lines().filter(|l| !l.ends_with("\t[]")).count();
    println!("readings: {lines} lines, {open} with a word or a verdict");
    if let Some(path) = std::env::var_os("ATERM_OPEN_WORK_DUMP") {
        std::fs::write(&path, &dump).expect("the dump writes");
    }
    if let Some(path) = std::env::var_os("ATERM_OPEN_WORK_AGAINST") {
        let other = std::fs::read_to_string(&path).expect("the other dump reads");
        let differ: Vec<String> = dump
            .lines()
            .zip(other.lines())
            .filter(|(a, b)| a != b)
            .map(|(a, b)| format!("here:  {a}\nthere: {b}"))
            .collect();
        assert!(
            differ.is_empty() && dump.lines().count() == other.lines().count(),
            "{} of {lines} readings differ ({} here, {} there):\n{}",
            differ.len(),
            dump.lines().count(),
            other.lines().count(),
            differ[..differ.len().min(20)].join("\n")
        );
        println!("all {lines} readings equal the other tree's");
    }
}

/// A reader timed on a reply, by its name.
type Timed = (&'static str, fn(&str) -> usize);

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn the_open_work_readers_cost_on_the_longest_rows() {
    let mut rows = corpus_rows();
    rows.sort_by_key(|(_, _, reply)| std::cmp::Reverse(reply.len()));
    let readers: [Timed; 4] = [
        ("land_open_words", |r| land_open_words(Some(r)).len()),
        // The Land yield's read of a long reply that defends itself: the whole reply, strictly.
        ("land_open_words(defended)", |r| {
            land_open_words(Some(&format!("{r}\nNothing left: the work is on main."))).len()
        }),
        ("open_work_strict", |r| open_work_strict(Some(r)).len()),
        ("open_work", |r| open_work(Some(r)).len()),
    ];
    for (corpus, row, reply) in rows.iter().take(5) {
        let mut line = format!("{corpus}#{row} ({} B):", reply.len());
        for (name, read) in readers {
            let mut took: Vec<u128> = (0..200)
                .map(|_| {
                    let at = std::time::Instant::now();
                    std::hint::black_box(read(std::hint::black_box(reply)));
                    at.elapsed().as_micros()
                })
                .collect();
            took.sort_unstable();
            line.push_str(&format!(
                " {name} p50={}us p95={}us;",
                took[took.len() / 2],
                took[took.len() * 95 / 100]
            ));
        }
        println!("{line}");
    }
}
