// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Coherence-group transactional apply (§7).

use std::collections::BTreeMap;

use crate::gate::ApplyDecision;
use crate::manifest::{Channel, Index};

/// A coherence group to apply atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The coherence-group name, or `None` for an ungrouped singleton program.
    pub group: Option<String>,
    /// The member program names, sorted.
    pub members: Vec<String>,
}

/// Partition a channel's pinned programs into coherence groups (§7).
#[must_use]
pub fn plan_groups(index: &Index, channel: &Channel) -> Vec<Group> {
    plan_groups_with(index, channel, &|_| false)
}

/// [`plan_groups`] on this machine.
#[must_use]
pub fn plan_groups_here(
    layout: &crate::store::Layout,
    index: &Index,
    channel: &Channel,
    triple: &str,
) -> Vec<Group> {
    plan_groups_with(index, channel, &|program| {
        matches!(
            crate::alab::source_in(layout, index, channel, program, triple),
            crate::alab::Source::Index { .. }
        )
    })
}

/// [`plan_groups`] with `index_owns` naming the vendor-table programs the index lane owns here.
#[must_use]
pub fn plan_groups_with(
    index: &Index,
    channel: &Channel,
    index_owns: &dyn Fn(&str) -> bool,
) -> Vec<Group> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut singletons: Vec<String> = Vec::new();
    for program in channel.pin.keys() {
        if crate::vendor_direct::is_vendor(program) && !index_owns(program) {
            continue;
        }
        let Some(p) = index.program(program) else {
            continue;
        };
        match &p.coherence_group {
            Some(g) => grouped.entry(g.clone()).or_default().push(program.clone()),
            None => singletons.push(program.clone()),
        }
    }
    let mut out: Vec<Group> = grouped
        .into_iter()
        .map(|(g, mut members)| {
            members.sort();
            Group {
                group: Some(g),
                members,
            }
        })
        .collect();
    singletons.sort();
    out.extend(singletons.into_iter().map(|m| Group {
        group: None,
        members: vec![m],
    }));
    out
}

/// The result of applying one group transactionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnOutcome {
    /// Every member was already on its pinned build — nothing to do.
    UpToDate,
    /// The listed members were staged and flipped to their pinned builds.
    Applied(Vec<String>),
    /// The listed members' pins are yanked/below-floor — the group is tombstoned, nothing applied.
    Tombstoned(Vec<String>),
    /// The listed members are held by a local pin.
    Pinned(Vec<String>),
    /// The group is held on its current builds because `member`'s pinned `build` publishes no
    /// artifact for `triple` — a tuple that cannot fully exist on this host is a correct state, not
    /// a failure, so nothing is staged or flipped and the next pass retries.
    Unpublished {
        member: String,
        build: u64,
        triple: String,
    },
    /// The group holds the Trust toolchain and its flip waits for quiet ([`crate::quiet`]).
    Deferred {
        members: Vec<String>,
        why: String,
        since: i64,
    },
    /// A member failed; the whole group was aborted.
    Aborted {
        failed: String,
        during_flip: bool,
        /// The failed stage's or flip's own cause.
        why: String,
        /// The members that had already flipped when `failed`'s flip failed, in flip order,
        /// each with what its rollback answered: `Err` names what it could not put back.
        /// Empty for a stage failure, and for a flip failure of the first member.
        rolled_back: Vec<(String, Result<(), String>)>,
    },
}

/// Run one coherence group as an all-or-nothing transaction over injected actions.
///
/// A stage or flip that fails says why, and the group's [`TxnOutcome::Aborted`] carries that
/// cause; after a failed flip it also carries each member already flipped with its rollback's
/// own answer, so a rollback that could not put a member back is never reported as done.
pub fn transact(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    flip: &mut dyn FnMut(&str) -> Result<(), String>,
    rollback: &mut dyn FnMut(&str) -> Result<(), String>,
) -> TxnOutcome {
    transact_holding(decisions, stage, &mut || None, flip, rollback)
}

/// [`transact`] with a hold asked once, after every member staged and before the first flip.
pub fn transact_holding(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    hold: &mut dyn FnMut() -> Option<(String, i64)>,
    flip: &mut dyn FnMut(&str) -> Result<(), String>,
    rollback: &mut dyn FnMut(&str) -> Result<(), String>,
) -> TxnOutcome {
    let tombstoned: Vec<String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Tombstone)
        .map(|(n, _)| n.clone())
        .collect();
    if !tombstoned.is_empty() {
        return TxnOutcome::Tombstoned(tombstoned);
    }

    let installs: Vec<&String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Install)
        .map(|(n, _)| n)
        .collect();
    if installs.is_empty() {
        return TxnOutcome::UpToDate;
    }

    for name in &installs {
        if let Err(why) = stage(name) {
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: false,
                why,
                rolled_back: Vec::new(),
            };
        }
    }

    if let Some((why, since)) = hold() {
        return TxnOutcome::Deferred {
            members: installs.into_iter().cloned().collect(),
            why,
            since,
        };
    }

    let mut flipped: Vec<&str> = Vec::new();
    for name in &installs {
        if let Err(why) = flip(name) {
            let rolled_back = flipped
                .iter()
                .map(|done| ((*done).to_string(), rollback(done)))
                .collect();
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: true,
                why,
                rolled_back,
            };
        }
        flipped.push(name);
    }
    TxnOutcome::Applied(installs.into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
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
}
