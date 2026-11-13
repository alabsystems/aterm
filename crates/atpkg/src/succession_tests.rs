// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::manifest::{Channel, Program};

fn program(repo: &str) -> Program {
    Program {
        repo: repo.into(),
        policy: "prebuilt-only".into(),
        coherence_group: None,
        system: None,
        unavailable_hint: None,
        requires: Vec::new(),
    }
}

fn index(build: u64, repos: &[(&str, &str)], pins: &[(&str, u64)]) -> Index {
    Index {
        schema: 2,
        index_build: build,
        generated_at: String::new(),
        valid_until: "2099-01-01T00:00:00Z".into(),
        machine_id: None,
        roster_seq: Some(5),
        programs: repos
            .iter()
            .map(|(p, r)| ((*p).to_string(), program(r)))
            .collect(),
        channels: vec![Channel {
            name: "stable".into(),
            channel_build: build,
            min_build: 0,
            min_build_by_program: BTreeMap::new(),
            yanked: Vec::new(),
            pin: pins.iter().map(|(p, b)| ((*p).to_string(), *b)).collect(),
            pin_by_target: BTreeMap::new(),
        }],
    }
}

fn sources(i: &Index) -> Sources {
    Sources::of(i, 5)
}

#[test]
fn a_raised_or_carried_pin_under_the_same_repo_succeeds() {
    let prior = index(
        77,
        &[("ay", "ay"), ("codex", "codex-alab")],
        &[("ay", 8256), ("codex", 2_026_102_501)],
    );
    let carried = index(
        78,
        &[("ay", "ay"), ("codex", "codex-alab")],
        &[("ay", 8256), ("codex", 2_026_102_501)],
    );
    let raised = index(
        78,
        &[("ay", "ay"), ("codex", "codex-alab")],
        &[("ay", 8300), ("codex", 2_026_102_601)],
    );
    assert_eq!(check(&sources(&prior), &carried), []);
    assert_eq!(check(&sources(&prior), &raised), []);
}

#[test]
fn a_repo_switch_needs_a_new_build_above_the_trusted_pin() {
    let prior = index(77, &[("codex", "codex-alab")], &[("codex", 2_026_102_501)]);
    let back = index(78, &[("codex", "aterm")], &[("codex", 2_026_102_501)]);
    assert_eq!(
        check(&sources(&prior), &back),
        [Break::RepoSwitch {
            channel: "stable".into(),
            program: "codex".into(),
            from_repo: "codex-alab".into(),
            to_repo: "aterm".into(),
            from: 2_026_102_501,
            to: 2_026_102_501,
        }]
    );
    let lowered = index(78, &[("codex", "aterm")], &[("codex", 2_026_101_501)]);
    let breaks = check(&sources(&prior), &lowered);
    assert!(
        matches!(
            breaks.as_slice(),
            [Break::Downgrade { .. }, Break::RepoSwitch { .. }]
        ),
        "{breaks:?}"
    );
    // The control: the same switch under a new build is what the publisher signs.
    let switched = index(78, &[("codex", "aterm")], &[("codex", 2_026_102_601)]);
    assert_eq!(check(&sources(&prior), &switched), []);
}

#[test]
fn a_repo_switch_needs_a_new_build_on_every_target_cell_it_keeps() {
    const LINUX: &str = "x86_64-unknown-linux-gnu";
    let with_cell = |build, repo, pin, cell: Option<u64>| {
        let mut i = index(build, &[("ay", repo)], &[("ay", pin)]);
        if let Some(cell) = cell {
            i.channels[0]
                .pin_by_target
                .insert(LINUX.into(), [("ay".to_string(), cell)].into());
        }
        i
    };
    let prior = sources(&with_cell(10, "ay", 5, Some(9)));
    // The pin rises, but Linux would still fetch the old repo's build 9 from the new one.
    assert_eq!(
        check(&prior, &with_cell(11, "ay-new", 6, Some(9))),
        [Break::TargetRepoSwitch {
            channel: "stable".into(),
            triple: LINUX.into(),
            program: "ay".into(),
            from_repo: "ay".into(),
            to_repo: "ay-new".into(),
            from: 9,
            to: 9,
        }]
    );
    // The controls: the cell names a new build, or the overlay is retired, or the repo
    // stays and the cell is carried.
    assert_eq!(check(&prior, &with_cell(11, "ay-new", 6, Some(10))), []);
    assert_eq!(check(&prior, &with_cell(11, "ay-new", 6, None)), []);
    assert_eq!(check(&prior, &with_cell(11, "ay", 6, Some(9))), []);
}

#[test]
fn a_lowered_pin_or_target_cell_is_refused_and_a_retired_one_is_not() {
    let mut prior = index(10, &[("ay", "ay"), ("nn", "nn")], &[("ay", 5), ("nn", 108)]);
    prior.channels[0].pin_by_target.insert(
        "x86_64-unknown-linux-gnu".into(),
        [("ay".to_string(), 9)].into(),
    );
    let mut next = index(11, &[("ay", "ay"), ("nn", "nn")], &[("ay", 4), ("nn", 108)]);
    next.channels[0].pin_by_target.insert(
        "x86_64-unknown-linux-gnu".into(),
        [("ay".to_string(), 8)].into(),
    );
    let breaks = check(&sources(&prior), &next);
    assert_eq!(breaks.len(), 2, "{breaks:?}");
    assert!(
        breaks.iter().any(
            |b| matches!(b, Break::Downgrade { program, from: 5, to: 4, .. } if program == "ay")
        )
    );
    assert!(breaks.iter().any(|b| matches!(b, Break::TargetDowngrade { triple, from: 9, to: 8, .. } if triple == "x86_64-unknown-linux-gnu")));
    // Retiring a program, its overlay or a whole target is the publisher's to do.
    let retired = index(11, &[("nn", "nn")], &[("nn", 108)]);
    assert_eq!(check(&sources(&prior), &retired), []);
    // And a program the trusted index never pinned may arrive at any build in any repo.
    let added = index(
        11,
        &[("ay", "ay"), ("nn", "nn"), ("ty", "ty")],
        &[("ay", 5), ("nn", 108), ("ty", 1)],
    );
    assert_eq!(check(&sources(&prior), &added), []);
}

#[test]
fn the_record_round_trips_and_never_moves_back() {
    let layout = crate::testdir::layout("atpkg-succession-record-");
    let mut t77 = index(77, &[("codex", "codex-alab")], &[("codex", 2_026_102_501)]);
    t77.channels[0].pin_by_target.insert(
        "x86_64-unknown-linux-gnu".into(),
        [("codex".to_string(), 1)].into(),
    );
    let s77 = sources(&t77);
    let refused = Refusal {
        label: "atpkg-index-78".into(),
        index_build: 78,
        machine_id: "m22".into(),
        trusted_build: 77,
        breaks: check(
            &s77,
            &index(78, &[("codex", "aterm")], &[("codex", 2_026_102_501)]),
        ),
    };
    let wrote = trust(&layout, Some(&s77), std::slice::from_ref(&refused)).unwrap();
    assert_eq!(wrote.new_refusals, [refused.row()]);
    let read = Record::read(&layout).expect("the record reads back");
    assert_eq!(read.sources, s77);
    assert_eq!(read.refused, [refused.row()]);
    assert!(
        read.refused[0]
            .sentence()
            .contains("moves from repo \"codex-alab\" to \"aterm\"")
    );

    let again = trust(&layout, Some(&s77), std::slice::from_ref(&refused)).unwrap();
    assert_eq!(
        again.new_refusals,
        [],
        "a refusal already recorded is said once"
    );

    let s76 = sources(&index(
        76,
        &[("codex", "codex-alab")],
        &[("codex", 2_026_102_001)],
    ));
    trust(&layout, Some(&s76), &[]).unwrap();
    let held = Record::read(&layout).unwrap();
    assert_eq!(held.sources, s77, "an older index never rewinds the record");
    assert_eq!(held.refused, [], "the last selection refused nothing");

    let mut s76_next_generation = s76.clone();
    s76_next_generation.roster_seq = 6;
    trust(&layout, Some(&s76_next_generation), &[]).unwrap();
    assert_eq!(
        Record::read(&layout).unwrap().sources,
        s76_next_generation,
        "a new generation re-bases the record, as it re-bases the build floor"
    );
    trust(&layout, Some(&s77), &[]).unwrap();
    assert_eq!(
        Record::read(&layout).unwrap().sources,
        s76_next_generation,
        "an older generation, however high its build, never rewinds the record"
    );
}
