// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::sig::testkit;

fn index_with_groups() -> Index {
    let body = String::from(
        r#"
schema = 2
index_build = 41
valid_until = "2026-07-05T12:00:00Z"
machine_id = "m3"
roster_seq = 3
[programs.trust]
repo = "trust"
coherence_group = "rustc"
[programs.ay]
repo = "ay"
coherence_group = "rustc"
[programs.ny]
repo = "ny"
[programs.claude]
repo = "aterm"
[programs.codex]
repo = "aterm"
"#,
    );
    crate::manifest::parse_index(&testkit::machine_signed(body.into_bytes())).unwrap()
}

fn channel(pins: &[(&str, u64)]) -> Channel {
    Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build: 0,
        min_build_by_program: Default::default(),
        yanked: vec![],
        pin: pins.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        pin_by_target: Default::default(),
    }
}

#[test]
fn plan_groups_partitions_by_coherence_group() {
    let idx = index_with_groups();
    let ch = channel(&[("trust", 4821), ("ay", 18), ("ny", 9), ("dotfiles", 1)]);
    let groups = plan_groups(&idx, &ch);
    assert_eq!(
        groups,
        vec![
            Group {
                group: Some("rustc".into()),
                members: vec!["ay".into(), "trust".into()]
            },
            Group {
                group: None,
                members: vec!["ny".into()]
            },
        ]
    );
}

/// A vendor-direct program is never planned, however the index names and pins it, so
/// no group lane can reinstall, downgrade or tombstone what its vendor lane installed.
#[test]
fn plan_groups_excludes_vendor_direct_programs() {
    let idx = index_with_groups();
    let mut ch = channel(&[
        ("ny", 9),
        ("claude", 2_026_091_901),
        ("codex", 2_026_091_901),
    ]);
    ch.yanked = vec!["claude@2026091901".into()];
    let planned: Vec<String> = plan_groups(&idx, &ch)
        .into_iter()
        .flat_map(|g| g.members)
        .collect();
    assert_eq!(planned, ["ny"]);
}

fn d(name: &str, dec: ApplyDecision) -> (String, ApplyDecision) {
    (name.to_string(), dec)
}

#[test]
fn all_up_to_date_does_nothing() {
    let decs = vec![
        d("ay", ApplyDecision::UpToDate),
        d("trust", ApplyDecision::UpToDate),
    ];
    let mut staged = Vec::new();
    let mut flipped = Vec::new();
    let out = transact(
        &decs,
        &mut |n| {
            staged.push(n.to_string());
            Ok(())
        },
        &mut |n| {
            flipped.push(n.to_string());
            Ok(())
        },
        &mut |_| Ok(()),
    );
    assert_eq!(out, TxnOutcome::UpToDate);
    assert!(
        staged.is_empty() && flipped.is_empty(),
        "nothing staged/flipped"
    );
}

#[test]
fn happy_path_stages_all_then_flips_all() {
    use std::cell::RefCell;
    let decs = vec![
        d("ay", ApplyDecision::Install),
        d("trust", ApplyDecision::Install),
    ];
    let order = RefCell::new(Vec::new());
    let out = transact(
        &decs,
        &mut |n| {
            order.borrow_mut().push(format!("stage:{n}"));
            Ok(())
        },
        &mut |n| {
            order.borrow_mut().push(format!("flip:{n}"));
            Ok(())
        },
        &mut |n| {
            order.borrow_mut().push(format!("rollback:{n}"));
            Ok(())
        },
    );
    assert_eq!(out, TxnOutcome::Applied(vec!["ay".into(), "trust".into()]));
    assert_eq!(
        order.into_inner(),
        vec!["stage:ay", "stage:trust", "flip:ay", "flip:trust"],
        "and nothing is rolled back"
    );
}

#[test]
fn a_stage_failure_aborts_with_nothing_flipped() {
    let decs = vec![
        d("ay", ApplyDecision::Install),
        d("trust", ApplyDecision::Install),
    ];
    let mut flipped = Vec::new();
    let out = transact(
        &decs,
        &mut |n| {
            // trust fails to stage
            if n == "trust" {
                Err(String::from("stage refused"))
            } else {
                Ok(())
            }
        },
        &mut |n| {
            flipped.push(n.to_string());
            Ok(())
        },
        &mut |_| Ok(()),
    );
    assert_eq!(
        out,
        TxnOutcome::Aborted {
            failed: "trust".into(),
            during_flip: false,
            why: "stage refused".into(),
            rolled_back: Vec::new(),
        }
    );
    assert!(flipped.is_empty(), "a stage failure must flip nothing");
}

/// A failed flip's own error is the abort's cause, and each member already flipped is rolled
/// back once, in flip order, with its rollback's own answer; the failed member is not among
/// them, and a first member that fails rolls nothing back.
#[test]
fn a_flip_failure_rolls_back_the_already_flipped() {
    const CAUSE: &str = "resolve check: libLLVM not found";
    const STUCK: &str = "re-pointing current: no space left";
    let decs = vec![
        d("ay", ApplyDecision::Install),
        d("trust-ir", ApplyDecision::Install),
        d("trust", ApplyDecision::Install),
    ];
    let pass = |refused: &str| {
        let mut rolled = Vec::new();
        let out = transact(
            &decs,
            &mut |_| Ok(()),
            &mut |n| {
                if n == refused {
                    Err(CAUSE.to_string())
                } else {
                    Ok(())
                }
            },
            &mut |n| {
                rolled.push(n.to_string());
                if n == "trust-ir" {
                    Err(STUCK.to_string())
                } else {
                    Ok(())
                }
            },
        );
        (out, rolled)
    };
    let (out, rolled) = pass("trust");
    assert_eq!(
        out,
        TxnOutcome::Aborted {
            failed: "trust".into(),
            during_flip: true,
            why: CAUSE.into(),
            rolled_back: vec![
                ("ay".into(), Ok(())),
                ("trust-ir".into(), Err(STUCK.into())),
            ],
        },
        "each flipped member carries its own rollback's answer"
    );
    assert_eq!(rolled, ["ay", "trust-ir"], "each is rolled back once");
    let (out, rolled) = pass("ay");
    assert_eq!(
        out,
        TxnOutcome::Aborted {
            failed: "ay".into(),
            during_flip: true,
            why: CAUSE.into(),
            rolled_back: Vec::new(),
        }
    );
    assert!(rolled.is_empty(), "nothing had flipped: {rolled:?}");
}

/// The hold is asked once, after the last stage and before the first flip; holding
/// flips nothing and names the members that were staged.
#[test]
fn a_hold_after_the_stage_flips_nothing() {
    use std::cell::RefCell;
    let decs = vec![
        d("ay", ApplyDecision::UpToDate),
        d("trust", ApplyDecision::Install),
        d("trust-ir", ApplyDecision::Install),
    ];
    let order = RefCell::new(Vec::new());
    let out = transact_holding(
        &decs,
        &mut |n| {
            order.borrow_mut().push(format!("stage:{n}"));
            Ok(())
        },
        &mut || {
            order.borrow_mut().push("hold".to_string());
            Some(("a build is running".to_string(), 7))
        },
        &mut |n| {
            order.borrow_mut().push(format!("flip:{n}"));
            Ok(())
        },
        &mut |_| Ok(()),
    );
    assert_eq!(
        out,
        TxnOutcome::Deferred {
            members: vec!["trust".into(), "trust-ir".into()],
            why: "a build is running".into(),
            since: 7,
        }
    );
    assert_eq!(
        order.into_inner(),
        ["stage:trust", "stage:trust-ir", "hold"],
        "staged, asked once, flipped nothing"
    );
    let out = transact_holding(
        &decs,
        &mut |_| Ok(()),
        &mut || None,
        &mut |_| Ok(()),
        &mut |_| Ok(()),
    );
    assert!(matches!(out, TxnOutcome::Applied(_)));
    let revoked = vec![d("trust", ApplyDecision::Tombstone)];
    let out = transact_holding(
        &revoked,
        &mut |_| Ok(()),
        &mut || panic!("a tombstoned group is never held"),
        &mut |_| Ok(()),
        &mut |_| Ok(()),
    );
    assert!(matches!(out, TxnOutcome::Tombstoned(_)));
}

#[test]
fn any_tombstoned_member_tombstones_the_group_without_touching_anything() {
    use std::cell::Cell;
    let decs = vec![
        d("ay", ApplyDecision::Install),
        d("trust", ApplyDecision::Tombstone),
    ];
    let touched = Cell::new(false);
    let out = transact(
        &decs,
        &mut |_| {
            touched.set(true);
            Ok(())
        },
        &mut |_| {
            touched.set(true);
            Ok(())
        },
        &mut |_| {
            touched.set(true);
            Ok(())
        },
    );
    assert_eq!(out, TxnOutcome::Tombstoned(vec!["trust".into()]));
    assert!(
        !touched.get(),
        "a tombstoned group stages/flips/rolls nothing"
    );
}
