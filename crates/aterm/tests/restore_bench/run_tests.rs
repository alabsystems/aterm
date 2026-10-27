// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The restore measurement's readings and tables ([`crate::run`]), checked.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_json::json;

use crate::live_ctl::fields;
use crate::run::*;
use crate::spec::Arm;

fn round(admit_ms: f64, restored_ms: f64) -> Round {
    Round {
        sessions: 2,
        birth_to_main_ms: 4.0,
        main_to_hello_ms: 6.0,
        admit_ms,
        admit_max_ms: admit_ms / 2.0,
        placed_ms: admit_ms + 2.0,
        settled_ms: Some(restored_ms - 5.0),
        offers: 2,
        experiment: crate::spec::Applied::default(),
        seam_copies: 0,
        seam_plants: 0,
        seam_ms: 0.0,
        ready_ms: 40.0,
        served_ms: 45.0,
        served_window_ms: 2.5,
        restored_ms,
        imported_ms: restored_ms + 10.0,
        sweep_max_ms: 1.5,
        rungs: BTreeMap::from([("exact".to_string(), 2)]),
        whys: BTreeMap::from([("-".to_string(), 2)]),
        history_lost: 0,
        a_lines: 10_000,
        b_lines: 10_000,
        load: "1.00 2.00 3.00".to_string(),
    }
}

/// The derived readings are B's two clocks added and the bench's less them.
#[test]
fn birth_to_hello_is_b_s_clocks_and_hello_to_restored_the_bench_s_less_them() {
    let r = round(3.0, 50.0);
    assert!((r.birth_to_hello_ms() - 10.0).abs() < 1e-9);
    assert!((r.hello_to_restored_ms() - 40.0).abs() < 1e-9);
}

/// An import is in once the lines and the counted loss reach A's, with B's journal caught
/// up; a META rung has none to wait for. Negative controls: a line short, the ring behind,
/// the journal not live, no `restore=` at all.
#[test]
fn an_import_is_in_when_its_lines_and_its_loss_reach_a_s() {
    let status = |words: &'static str| fields(words);
    let caught_up = status("restore=exact history_lost=0 journal=live journal_ring_lag=0");
    assert!(import_in(&caught_up, 5_000, 5_000));
    assert!(!import_in(&caught_up, 4_999, 5_000), "a line short");
    let lost = status("restore=screen history_lost=4488 journal=live journal_ring_lag=0");
    assert!(import_in(&lost, 512, 5_000), "the loss counts");
    let behind = status("restore=exact history_lost=0 journal=live journal_ring_lag=3");
    assert!(!import_in(&behind, 5_000, 5_000), "B's ring behind");
    let off = status("restore=exact history_lost=0 journal_off=seam");
    assert!(!import_in(&off, 5_000, 5_000), "no live journal");
    assert!(import_in(
        &status("restore=meta restore_why=no-region"),
        0,
        5_000
    ));
    assert!(!import_in(&status("restore=- journal=live"), 5_000, 5_000));
}

#[test]
fn a_completed_sweep_recovers_exactly_as_sessions_even_with_meta_replacements() {
    let expected = BTreeMap::from([("s-a".to_string(), 5_000), ("s-b".to_string(), 5_000)]);
    let ids = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    };
    let restored = |names: &[&str]| ids(names).into_iter().collect::<BTreeSet<_>>();
    let imported = |names: &[&str]| {
        ids(names)
            .into_iter()
            .map(|sid| (sid, Duration::from_millis(3)))
            .collect::<BTreeMap<_, _>>()
    };
    let both_restored = restored(&["s-a", "s-b"]);
    let both_imported = imported(&["s-a", "s-b"]);
    assert_eq!(
        sweep_complete(
            &expected,
            &ids(&["s-b", "s-a"]),
            &both_restored,
            &both_imported
        ),
        (true, true)
    );
    for current in [
        ids(&["s-a"]),
        ids(&["s-a", "s-a"]),
        ids(&["s-a", "s-b", "s-extra"]),
        ids(&["s-a", "s-replacement"]),
    ] {
        assert_eq!(
            sweep_complete(&expected, &current, &both_restored, &both_imported),
            (false, false),
            "{current:?}"
        );
    }
    // META has no history to import, so counting arbitrary restored/imported IDs was unsafe.
    let meta = fields("restore=meta restore_why=no-region");
    assert!(import_in(&meta, 0, u64::MAX));
    let replacements = ["s-replacement-a", "s-replacement-b"];
    assert_eq!(
        sweep_complete(
            &expected,
            &ids(&replacements),
            &restored(&replacements),
            &imported(&replacements)
        ),
        (false, false)
    );
    let current = ids(&["s-a", "s-b"]);
    assert_eq!(
        sweep_complete(&expected, &current, &restored(&["s-a"]), &both_imported),
        (false, false)
    );
    assert_eq!(
        sweep_complete(&expected, &current, &both_restored, &imported(&["s-a"])),
        (true, false)
    );
}

/// An arm took only when B's seam did exactly what the arm asks in every offered region: no
/// copy for the clean arm, a copy each for the copy arm, a copy and a plant each for the
/// lookalike arm, whose plant also costs every recovered session history. Negative controls:
/// the seam missing from B (a drifted name or a build without the seams), one region short, a
/// plant where none was asked, a session the plant cost nothing.
#[test]
fn an_arm_took_only_when_the_seam_did_what_it_asks_in_every_region() {
    assert!(arm_took(Arm::Clean, 8, Some(0), Some(0), &[0; 8]).is_ok());
    assert!(
        arm_took(Arm::Clean, 8, None, None, &[0; 8]).is_ok(),
        "what ships needs no seam"
    );
    assert!(arm_took(Arm::Copy, 8, Some(8), Some(0), &[0; 8]).is_ok());
    assert!(arm_took(Arm::Lookalike, 8, Some(8), Some(8), &[4_467; 8]).is_ok());

    for (arm, copies, plants, lost, what) in [
        (
            Arm::Clean,
            Some(8),
            Some(0),
            &[0u64; 8][..],
            "a copy in the clean arm",
        ),
        (Arm::Copy, Some(0), Some(0), &[0; 8], "no copy made"),
        (Arm::Copy, None, None, &[0; 8], "B has no seam"),
        (Arm::Copy, Some(7), Some(0), &[0; 8], "one region short"),
        (
            Arm::Copy,
            Some(8),
            Some(8),
            &[0; 8],
            "a plant in the copy arm",
        ),
        (
            Arm::Lookalike,
            Some(8),
            Some(7),
            &[4_467; 8],
            "one plant refused",
        ),
        (
            Arm::Lookalike,
            Some(8),
            Some(8),
            &[0; 8],
            "restored from the region",
        ),
        (
            Arm::Lookalike,
            Some(2),
            Some(2),
            &[4_467, 0],
            "a session lost nothing",
        ),
    ] {
        let offers = lost.len() as u64;
        assert!(
            arm_took(arm, offers, copies, plants, lost).is_err(),
            "{what}: {arm:?} {copies:?} {plants:?} {lost:?}"
        );
    }
    assert!(
        arm_took(Arm::Lookalike, 8, Some(8), Some(8), &[4_467; 7]).is_err(),
        "a session's status missing"
    );
}

/// Served is late by at most the span from the last refused request to the served answer, or
/// from B's ready signal when the first request was served.
#[test]
fn served_s_window_runs_from_the_last_refusal_or_ready_to_the_answer() {
    let ready = Instant::now();
    let window = |last_refused: Option<Instant>, answered: Instant| {
        served_window_of(ready, last_refused, answered)
    };
    let answered = ready + Duration::from_millis(400);
    assert_eq!(
        window(Some(ready + Duration::from_millis(397)), answered),
        Duration::from_millis(3)
    );
    assert_eq!(
        window(None, answered),
        Duration::from_millis(400),
        "served at once: the fence lifted somewhere before ready"
    );
}

/// The tables carry a row per session count, each column's median and range (a reading a round
/// does not have left out of it), and a row per round of what came back.
#[test]
fn the_tables_carry_every_size_and_every_round() {
    let spec = crate::spec::parse("out=/s sessions=1,2 rounds=2").expect("parses");
    let mut unsettled = round(3.0, 50.0);
    unsettled.settled_ms = None;
    let sizes = vec![
        (1, vec![round(1.0, 30.0), unsettled]),
        (2, vec![round(2.0, 40.0), round(2.0, 40.0)]),
    ];
    let md = markdown(&spec, &sizes);
    assert!(md.contains("**Arm:** `clean`"), "{md}");
    assert!(
        md.contains(
            "| 1 | 10.0 (10.0–10.0) | 1.0 (1.0–3.0) | 0.5 (0.5–1.5) | 3.0 (3.0–5.0) | 25.0 \
             (25.0–25.0) | 0.0 (0.0–0.0) | 2 |"
        ),
        "the nearest-rank median, the min and the max, the round with no import left out: {md}"
    );
    assert!(
        md.contains("| 1 | 40.0 (40.0–40.0) | 45.0 (45.0–45.0) | 2.5 (2.5–2.5) |"),
        "the bench's clock with served's window: {md}"
    );
    assert_eq!(
        md.lines()
            .filter(|l| l.contains(
                "| `exact` 2 | `-` 2 | 0 | 10000 | 10000 | 0 / 0 | 0 / 0 / 0 | 1.00 2.00 3.00 |"
            ))
            .count(),
        4,
        "a row per round: {md}"
    );
    let json = size_json(2, &sizes[1].1);
    assert_eq!(json["summary"]["admit_ms"]["p50"], json!(2.0));
    assert_eq!(json["rounds"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["rounds"][0]["experiment"],
        json!({"copies": 0, "plants": 0, "refused": 0})
    );
    let one = size_json(1, &sizes[0].1);
    assert_eq!(one["summary"]["settled_ms"]["n"], json!(1));
    assert_eq!(one["rounds"][1]["settled_ms"], json!(null));
}
