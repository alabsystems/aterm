// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for `aos-cadence`'s schedule and its MEASURED line.

use super::*;

#[test]
fn every_take_types_the_same_keys_at_the_same_person_s_cadence() {
    let keys = keys();
    assert_eq!(keys.len(), 158, "four lines and their Enters");
    assert_eq!(keys.iter().filter(|&&c| c == '\n').count(), TEXT.len());
    for line in TEXT {
        let cols: usize = GRID[1].parse().expect("columns");
        assert!(line.chars().count() < cols, "{line:?} wraps");
        let mut glyphs = line.chars().filter(|c| *c != ' ');
        let first = glyphs.next().expect("a glyph");
        assert!(
            glyphs.any(|c| c != first),
            "{line:?}: a run opens on two distinct glyphs"
        );
    }
    let gaps = gaps(keys.len());
    assert_eq!(gaps, super::gaps(keys.len()), "a fixed seed");
    let ms: Vec<u64> = gaps
        .iter()
        .map(|g| u64::try_from(g.as_millis()).expect("small"))
        .collect();
    assert!(ms.iter().all(|m| (GAP_MS.0..=GAP_MS.1).contains(m)));
    let mean = ms.iter().sum::<u64>() / u64::try_from(ms.len()).expect("small");
    assert!(
        (140..=200).contains(&mean),
        "a person's mean gap: {mean} ms"
    );
    assert!(ms.iter().collect::<std::collections::BTreeSet<_>>().len() > 50);
    assert_eq!(
        keys.iter()
            .map(|&c| key_body(c))
            .take(5)
            .collect::<Vec<_>>(),
        ["t", "h", "e", "space", "q"]
    );
    assert_eq!(key_body('\n'), "enter");
    assert_eq!(ORDER, ['A', 'B', 'B', 'A']);
}

#[test]
fn a_take_line_names_its_build_its_program_its_recording_and_each_figure() {
    let t = Take {
        attached: true,
        logged: Some(154),
        figures: FIGURES.iter().map(|_| "1.5".to_owned()).collect(),
        held: Some((4.0, 180.0, 230.5)),
    };
    let line = measured(2, 'B', (true, false), 158, &t);
    assert!(line.starts_with(
        "MEASURED take=2 build=B program=echo attach=true typed=158 attached=true logged=154 n_present=1.5 "
    ));
    assert!(
        line.ends_with(" input_p99_ms=1.5 held_p50_ms=4.0 held_p95_ms=180.0 held_p99_ms=230.5"),
        "{line}"
    );
    assert!(take_ok((true, false), &t));
    assert!(
        !take_ok((true, true), &t),
        "a recording that ran through a program that echoes nothing fails its take"
    );
    let stopped = Take {
        attached: false,
        ..Take::default()
    };
    assert!(
        !take_ok((true, false), &stopped),
        "a recording that stopped fails its take"
    );
    assert!(take_ok((true, true), &stopped), "unless nothing was echoed");
    assert!(
        take_ok((false, false), &stopped),
        "the control attaches nothing"
    );
    assert!(
        measured(1, 'A', (false, true), 158, &stopped)
            .contains(" program=silent attach=false typed=158 attached=false logged=- ")
    );
    assert!(measured(1, 'A', (false, false), 158, &stopped).ends_with(" held_p99_ms=-"));
}

#[test]
fn a_logged_key_is_matched_to_its_press_from_the_newest() {
    let ms = |n: u64| Duration::from_millis(n);
    let ns = |n: u64| i128::from(n) * 1_000_000;
    let keys = ['a', 'b', '\n', 'c'];
    let sent = [ms(10), ms(200), ms(400), ms(600)];
    assert_eq!(
        held_ns(&[ns(205), ns(205), ns(603)], &keys, &sent),
        [195_000_000, 5_000_000, 3_000_000],
        "'a' waited for 'b', 'b' and 'c' for their own echo; the Enter is never logged"
    );
    assert_eq!(
        held_ns(&[ns(603)], &keys, &sent),
        [3_000_000],
        "a log that holds only the newest key"
    );
    assert_eq!(held_ns(&[ns(5)], &['a'], &[ms(10)]), [0], "never negative");
    assert!(held_ns(&[], &keys, &sent).is_empty());
}
