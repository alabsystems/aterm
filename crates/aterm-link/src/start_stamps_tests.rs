// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn stamps(at: &[(&str, u64)]) -> Stamps {
    let mut s = Stamps::default();
    for &(phase, ns) in at {
        s.mark_at(phase, ns);
    }
    s
}

#[test]
fn a_line_reads_back_as_the_stamps_it_wrote() {
    let s = stamps(&[(MAIN, 10), (BROKER, 12), (KEY, 15), (LISTENING, 99)]);
    let line = s.line();
    assert_eq!(
        line,
        format!("{PREFIX} main=10 broker=12 key=15 listening=99")
    );
    assert_eq!(Stamps::parse(&line), Some(s.clone()));
    assert_eq!(Stamps::parse(&format!("{line}\n")), Some(s.clone()));
    let stderr = format!(
        "aterm link broker: a note\n{line}\naterm link broker: unix socket on /s (guarded)\n"
    );
    assert_eq!(Stamps::find(&stderr), Some(s));
}

#[test]
fn another_line_or_a_bad_word_is_no_stamps() {
    assert_eq!(Stamps::parse("aterm link broker: unix socket on /s"), None);
    assert_eq!(Stamps::parse(&format!("{PREFIX} main=x")), None);
    assert_eq!(Stamps::parse(&format!("{PREFIX} main")), None);
    assert_eq!(Stamps::parse(&format!("{PREFIX} =5")), None);
    assert_eq!(Stamps::find("listening /s\n"), None);
    // The line with no stamp (no clock read) is still the line: no step but spawn to seen.
    let none = Stamps::parse(PREFIX).expect("an empty line of stamps");
    assert_eq!(
        none.steps(1, Some(4)),
        Some(vec![("spawn".to_string(), "seen".to_string(), 3)])
    );
}

#[test]
fn the_steps_run_spawn_to_seen_in_the_start_s_order() {
    // Stamped out of order: the steps follow ORDER, not the line.
    let s = stamps(&[
        (BROKER, 130),
        (MAIN, 110),
        (KEY, 140),
        (LOCKED, 150),
        (OPENED, 900),
        (ARMED, 905),
        (RULES, 910),
        (SERVED, 920),
        (LISTENING, 925),
    ]);
    let steps = s.steps(100, Some(930)).expect("one clock");
    let named: Vec<(&str, &str, u64)> = steps
        .iter()
        .map(|(a, b, ns)| (a.as_str(), b.as_str(), *ns))
        .collect();
    assert_eq!(
        named,
        [
            ("spawn", MAIN, 10),
            (MAIN, BROKER, 20),
            (BROKER, KEY, 10),
            (KEY, LOCKED, 10),
            (LOCKED, OPENED, 750),
            (OPENED, ARMED, 5),
            (ARMED, RULES, 5),
            (RULES, SERVED, 10),
            (SERVED, LISTENING, 5),
            (LISTENING, "seen", 5),
        ]
    );
    // The steps add up to spawn -> seen.
    assert_eq!(steps.iter().map(|s| s.2).sum::<u64>(), 830);
}

#[test]
fn a_missing_phase_folds_into_the_next_step_and_a_backwards_stamp_is_refused() {
    let s = stamps(&[(BROKER, 130), (OPENED, 900), (LISTENING, 925)]);
    assert_eq!(
        s.steps(100, None),
        Some(vec![
            ("spawn".to_string(), BROKER.to_string(), 30),
            (BROKER.to_string(), OPENED.to_string(), 770),
            (OPENED.to_string(), LISTENING.to_string(), 25),
        ])
    );
    assert_eq!(s.steps(200, None), None, "a spawn after the broker's entry");
    assert_eq!(s.steps(100, Some(920)), None, "seen before listening");
}

#[test]
fn a_start_entered_after_main_stamps_main_then_the_broker() {
    // This test binary's only caller of `main_entered`, as `aterm`'s main is the only one there.
    main_entered();
    let s = Stamps::broker_entered();
    if aterm_uds::process::continuous_ns().is_none() {
        assert_eq!(s, Stamps::default(), "no clock, no stamp");
        return;
    }
    let main = s.at(MAIN).expect("main's stamp, read by main_entered");
    let broker = s.at(BROKER).expect("the broker's stamp");
    assert!(main <= broker, "main {main} after the broker {broker}");
    // The first call counts: a later one leaves main's stamp as it was.
    main_entered();
    assert_eq!(Stamps::broker_entered().at(MAIN), Some(main));
}
