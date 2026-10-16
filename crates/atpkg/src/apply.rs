// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Coherence-group transactional apply (§7).
//!
//! The unit of update is a coherence group: members of one `[programs].coherence_group`
//! (the `rustc`-locked trust/trust-mc/ay/clean tuple) move atomically, and a program with
//! no group applies independently, so one tiny tool can never wedge the trust update.
//!
//! [`plan_groups`] partitions a channel's pinned programs into groups (pure). [`transact`]
//! runs one group as an all-or-nothing transaction over injected stage/flip/rollback
//! actions, so the sequencing is unit-tested without network or filesystem:
//!
//! 1. **Tombstone short-circuit**: if any member's [`ApplyDecision`] is
//!    [`ApplyDecision::Tombstone`] (pin yanked or below floor), nothing is staged or flipped.
//! 2. **Stage-all**: stage every member that needs [`ApplyDecision::Install`]; any failure
//!    aborts with nothing flipped.
//! 3. **Flip-all**: only once every member staged, flip each; a failure partway rolls back
//!    the already-flipped members and aborts.
//!
//! Between 2 and 3, [`transact_holding`] asks a hold once: an unattended pass whose group
//! holds the Trust toolchain flips nothing while it is in use ([`TxnOutcome::Deferred`]).
//!
//! In production (`flow`) stage is download + [`crate::install::verify_and_stage`], flip is
//! [`crate::activate::activate_build`] plus the shims (`activate::install_tools`), and
//! rollback re-points to the retained previous build.

use std::collections::BTreeMap;

use crate::gate::ApplyDecision;
use crate::manifest::{Channel, Index};

/// A coherence group to apply atomically. `group` is the shared `coherence_group` name,
/// or `None` for an ungrouped (independent) singleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The coherence-group name, or `None` for an ungrouped singleton program.
    pub group: Option<String>,
    /// The member program names, sorted.
    pub members: Vec<String>,
}

/// Partition a channel's pinned programs into coherence groups (§7). A pinned program the
/// index does not name is excluded (reachability, §5), and so is a vendor-direct program:
/// its vendor lane owns it, and a legacy pin must never downgrade or tombstone the build
/// that lane installed. Grouped members are gathered by their `coherence_group`; ungrouped
/// programs become singleton groups. Deterministic order: named groups first (by name),
/// then ungrouped singletons, members sorted.
#[must_use]
pub fn plan_groups(index: &Index, channel: &Channel) -> Vec<Group> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut singletons: Vec<String> = Vec::new();
    for program in channel.pin.keys() {
        if crate::vendor_direct::is_vendor(program) {
            continue;
        }
        // Reachability: only programs the verified index names are installable.
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
    /// The listed members' pins are yanked/below-floor — the group is tombstoned, nothing
    /// applied (never run a revoked build).
    Tombstoned(Vec<String>),
    /// The listed members are held by a local pin: the group's upgrade was suppressed on
    /// its current builds, with nothing staged or flipped. A pin is consulted only after
    /// [`crate::gate::decide`] and never when the group tombstones. Built by
    /// [`crate::flow`]'s group apply; [`transact`] never returns it.
    Pinned(Vec<String>),
    /// The group is held on its current builds because `member`'s pinned `build` publishes
    /// no artifact for `triple` — a tuple that cannot fully exist on this host is a correct
    /// state, not a failure, so nothing is staged or flipped and the next pass retries.
    /// A Tombstone or a force-upgrade off a revoked current build aborts instead. Built by
    /// [`crate::flow`]'s group transaction directly — [`transact`] never returns it.
    Unpublished {
        member: String,
        build: u64,
        triple: String,
    },
    /// The group holds the Trust toolchain and its flip waits for quiet ([`crate::quiet`]):
    /// an unattended pass found the toolchain it would supersede in use (leased by a run,
    /// run from, or not known), so the members' new builds were staged and nothing was
    /// flipped. `why` is what was using it; `since` is when this wait began, which the
    /// four-hour ceiling counts from. Never over a revoked build, never on a person's pass.
    /// Built by [`crate::flow`]'s group transaction, or by [`transact_holding`].
    Deferred {
        members: Vec<String>,
        why: String,
        since: i64,
    },
    /// A member failed; the whole group was aborted. `during_flip` distinguishes a
    /// stage-phase abort (nothing flipped) from a flip-phase abort (already-flipped members
    /// rolled back). `why` is the failing step's own sentence.
    Aborted {
        failed: String,
        during_flip: bool,
        why: String,
    },
}

/// Run one coherence group as an all-or-nothing transaction over injected actions (module
/// docs). `stage` answers `Err(why)` on failure (the sentence the abort carries), `flip`
/// returns `true` on success, `rollback` undoes a flipped member. No I/O of its own.
pub fn transact(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    flip: &mut dyn FnMut(&str) -> bool,
    rollback: &mut dyn FnMut(&str),
) -> TxnOutcome {
    transact_holding(decisions, stage, &mut || None, flip, rollback)
}

/// [`transact`] with a hold asked once, after every member staged and before the first
/// flip: `Some((why, since))` ends the transaction [`TxnOutcome::Deferred`] with nothing
/// flipped; the caller discards what it staged, as an abort does, and keeps the archives.
/// Asked at the last moment so a build that started during a long extract is seen
/// ([`crate::quiet`]).
pub fn transact_holding(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    hold: &mut dyn FnMut() -> Option<(String, i64)>,
    flip: &mut dyn FnMut(&str) -> bool,
    rollback: &mut dyn FnMut(&str),
) -> TxnOutcome {
    // 1. Tombstone short-circuit: a group with any revoked member cannot form a valid
    //    tuple, so it applies nothing.
    let tombstoned: Vec<String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Tombstone)
        .map(|(n, _)| n.clone())
        .collect();
    if !tombstoned.is_empty() {
        return TxnOutcome::Tombstoned(tombstoned);
    }

    // 2. The members that actually need installing.
    let installs: Vec<&String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Install)
        .map(|(n, _)| n)
        .collect();
    if installs.is_empty() {
        return TxnOutcome::UpToDate;
    }

    // 3. Stage every member first. A single failure aborts the group with nothing flipped.
    for name in &installs {
        if let Err(why) = stage(name) {
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: false,
                why,
            };
        }
    }

    // 4. The hold, once, between the last stage and the first flip.
    if let Some((why, since)) = hold() {
        return TxnOutcome::Deferred {
            members: installs.into_iter().cloned().collect(),
            why,
            since,
        };
    }

    // 5. Only after all staged: flip each. A flip failure rolls back the already-flipped.
    let mut flipped: Vec<&String> = Vec::new();
    for name in &installs {
        if flip(name) {
            flipped.push(name);
        } else {
            for done in &flipped {
                rollback(done);
            }
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: true,
                why: String::from(
                    "the flip failed and the members already flipped were rolled back",
                ),
            };
        }
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
        // `VerifiedBytes` has no public constructor, so a fixture index is machine-signed
        // through the real roster chain.
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

    // Grouping: the rustc tuple is one group; ny is an independent singleton; an unpinned-
    // but-named or pinned-but-unnamed program is excluded.
    #[test]
    fn plan_groups_partitions_by_coherence_group() {
        let idx = index_with_groups();
        // dotfiles is pinned but not named in the index → excluded (reachability).
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
                true
            },
            &mut |_| {},
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
                true
            },
            &mut |_| {},
        );
        assert_eq!(out, TxnOutcome::Applied(vec!["ay".into(), "trust".into()]));
        // Every stage happens before any flip.
        assert_eq!(
            order.into_inner(),
            vec!["stage:ay", "stage:trust", "flip:ay", "flip:trust"]
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
                true
            },
            &mut |_| {},
        );
        assert_eq!(
            out,
            TxnOutcome::Aborted {
                failed: "trust".into(),
                during_flip: false,
                why: "stage refused".into(),
            }
        );
        assert!(flipped.is_empty(), "a stage failure must flip nothing");
    }

    #[test]
    fn a_flip_failure_rolls_back_the_already_flipped() {
        let decs = vec![
            d("ay", ApplyDecision::Install),
            d("trust", ApplyDecision::Install),
        ];
        let mut rolled = Vec::new();
        let out = transact(
            &decs,
            &mut |_| Ok(()),       // both stage ok
            &mut |n| n != "trust", // ay flips ok, trust's flip fails
            &mut |n| rolled.push(n.to_string()),
        );
        assert_eq!(
            out,
            TxnOutcome::Aborted {
                failed: "trust".into(),
                during_flip: true,
                why: "the flip failed and the members already flipped were rolled back".into(),
            }
        );
        assert_eq!(
            rolled,
            vec!["ay"],
            "the already-flipped member is rolled back"
        );
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
                true
            },
            &mut |_| {},
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
        // A hold that lets go flips as before; a tombstone never reaches the hold.
        let out = transact_holding(
            &decs,
            &mut |_| Ok(()),
            &mut || None,
            &mut |_| true,
            &mut |_| {},
        );
        assert!(matches!(out, TxnOutcome::Applied(_)));
        let revoked = vec![d("trust", ApplyDecision::Tombstone)];
        let out = transact_holding(
            &revoked,
            &mut |_| Ok(()),
            &mut || panic!("a tombstoned group is never held"),
            &mut |_| true,
            &mut |_| {},
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
                true
            },
            &mut |_| {
                touched.set(true);
            },
        );
        assert_eq!(out, TxnOutcome::Tombstoned(vec!["trust".into()]));
        assert!(
            !touched.get(),
            "a tombstoned group stages/flips/rolls nothing"
        );
    }
}
