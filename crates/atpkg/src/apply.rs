// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Coherence-group transactional apply (§7) — the headline Phase 4 logic.
//!
//! The unit of update is a **coherence group**, not the whole channel and not a single
//! tool. Members of the same `[programs].coherence_group` (the `rustc`-locked
//! trust/trust-mc/ay/clean tuple) move **atomically**; a program with no group applies
//! **independently** so one tiny tool can never wedge the trust update.
//!
//! [`plan_groups`] partitions a channel's pinned programs into groups (pure) and orders
//! them DEPENDENCY-FIRST over the index's `requires` relation (§17.10): a group carrying
//! a requirement follows the group that provides it, with the partition's own order —
//! named groups by name, then singletons by name — as the stable tiebreak; a group's
//! requirements are the union of its members' ([`group_requires`]), since the tuple
//! installs as one unit. [`transact`]
//! runs one group as an all-or-nothing transaction over **injected** stage/flip/rollback
//! actions, so the sequencing is unit-tested without any network or filesystem:
//!
//! 1. **Tombstone short-circuit** — if any member's [`ApplyDecision`] is
//!    [`ApplyDecision::Tombstone`] (its pin is yanked / below floor, §7), the group cannot
//!    form a valid tuple, so **nothing** is staged or flipped: the group tombstones.
//! 2. **Stage-all** — stage every member that needs [`ApplyDecision::Install`]. If *any*
//!    stage fails, **abort**: flip nothing (the group stays on its current builds).
//! 3. **Flip-all** — only once every member staged, flip each. If a flip fails partway,
//!    **roll back** the already-flipped members and abort.
//!
//! The production adapter wires stage = download + [`crate::install::verify_and_stage`],
//! flip = [`crate::activate::activate_channel`] + [`crate::activate::install_shims`], and
//! rollback = re-point to the retained previous build.

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

/// Partition a channel's pinned programs into coherence groups (§7) and order them
/// dependency-first (§17.10). A pinned program the index does not name is excluded
/// (reachability, §5). Grouped members are gathered by their `coherence_group`; ungrouped
/// programs become singleton groups. Deterministic order: named groups first (by name),
/// then ungrouped singletons, members sorted — and then [`order_by_requires`] moves every
/// group behind the groups it requires, that partition order as the stable tiebreak.
#[must_use]
pub fn plan_groups(index: &Index, channel: &Channel) -> Vec<Group> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut singletons: Vec<String> = Vec::new();
    for program in channel.pin.keys() {
        // Reachability: only programs the verified index NAMES are installable.
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
    order_by_requires(index, out)
}

/// A group's requirements as ONE unit: the union of its members' `requires`
/// ([`crate::manifest::Program::requires`]) in member order, deduplicated, MINUS the
/// members themselves — a dependency inside an atomic tuple is satisfied by the tuple's
/// own transaction. What the plan orders on, what the priority queue pulls ahead of a
/// bumped group, and what the pass checks before starting the group.
#[must_use]
pub fn group_requires(index: &Index, group: &Group) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in &group.members {
        let Some(p) = index.program(m) else {
            continue;
        };
        for dep in &p.requires {
            if !group.members.contains(dep) && !out.contains(dep) {
                out.push(dep.clone());
            }
        }
    }
    out
}

/// Dependency-first over `plan`: a group is emitted only once every group carrying one of
/// its requirements has been (Kahn's algorithm, first-ready-in-plan-order wins), so plan
/// order is the stable tiebreak and a plan with no `requires` comes back untouched. A
/// requirement OUTSIDE the plan (not pinned, or not named) constrains nothing here — the
/// pass's requirement gate decides what to do about it. A cycle cannot reach this
/// function (index parse refuses one, [`crate::manifest::validate_requires`]); were one
/// to, the first remaining group in plan order is emitted and the walk goes on, so the
/// output is always a PERMUTATION of the input — never lost work.
fn order_by_requires(index: &Index, plan: Vec<Group>) -> Vec<Group> {
    let position: BTreeMap<&str, usize> = plan
        .iter()
        .enumerate()
        .flat_map(|(i, g)| g.members.iter().map(move |m| (m.as_str(), i)))
        .collect();
    let deps: Vec<Vec<usize>> = plan
        .iter()
        .enumerate()
        .map(|(i, g)| {
            group_requires(index, g)
                .iter()
                .filter_map(|d| position.get(d.as_str()).copied())
                .filter(|&j| j != i)
                .collect()
        })
        .collect();
    let n = plan.len();
    let mut emitted = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    while order.len() < n {
        let ready = (0..n).find(|&i| !emitted[i] && deps[i].iter().all(|&j| emitted[j]));
        let Some(i) = ready.or_else(|| (0..n).find(|&i| !emitted[i])) else {
            break;
        };
        emitted[i] = true;
        order.push(i);
    }
    let mut slots: Vec<Option<Group>> = plan.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
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
    /// The listed members are held by a LOCAL pin — the group's upgrade was suppressed on
    /// its current builds. Never a Tombstone/floor bypass (a pin is consulted only after
    /// [`crate::gate::decide`], and only when the group is NOT tombstoning); nothing was
    /// staged or flipped. Constructed by [`crate::flow`]'s group apply directly —
    /// [`transact`] never returns it.
    Pinned(Vec<String>),
    /// The group is held on its current builds because one of its `requires` is NOT met
    /// (§17.10): `dep` is not installed, dev-linked, system-satisfied or installed through
    /// its protocol, and `dep_state` is the dependency's own canonical row — the two
    /// halves of the `blocked by <dep>: <dep state>` state the caller records. A DEFERRAL,
    /// never a failure: nothing was resolved, downloaded, staged or flipped, and the next
    /// pass retries. Constructed by [`crate::flow`]'s group transaction directly (the
    /// requirement gate, [`crate::requires::unmet_requirement`]) — [`transact`] never
    /// returns it.
    Blocked { dep: String, dep_state: String },
    /// A member failed; the whole group was aborted. `during_flip` distinguishes a
    /// stage-phase abort (nothing was flipped) from a flip-phase abort (already-flipped
    /// members were rolled back).
    Aborted { failed: String, during_flip: bool },
}

/// Run one coherence group as an all-or-nothing transaction over injected actions (see the
/// module docs). `stage`/`flip` return `true` on success; `rollback` undoes a member that
/// was flipped. Pure control flow — no I/O of its own — so it is exhaustively unit-tested.
pub fn transact(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> bool,
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

    // 3. Stage every member FIRST. A single failure aborts the group with nothing flipped.
    for name in &installs {
        if !stage(name) {
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: false,
            };
        }
    }

    // 4. Only after all staged: flip each. A flip failure rolls back the already-flipped.
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
        index_with_requires("", "", "")
    }

    /// [`index_with_groups`] with a `requires = […]` line under trust / ay / ny, plus a
    /// `clt` and a `brew` that requires it (unpinned unless a channel pins them).
    fn index_with_requires(trust: &str, ay: &str, ny: &str) -> Index {
        let body = format!(
            r#"
schema = 2
index_build = 41
valid_until = "2026-07-05T12:00:00Z"
machine_id = "m3"
roster_seq = 3
[programs.trust]
repo = "trust"
coherence_group = "rustc"
{trust}
[programs.ay]
repo = "ay"
coherence_group = "rustc"
{ay}
[programs.ny]
repo = "ny"
{ny}
[programs.clt]
repo = "clt"
[programs.brew]
repo = "brew"
requires = ["clt"]
"#
        );
        // Machine-signed through the real roster chain: `VerifiedBytes` still has no
        // public constructor, so a fixture index must be signed like a published one.
        crate::manifest::parse_index(&testkit::machine_signed(body.into_bytes())).unwrap()
    }

    fn channel(pins: &[(&str, u64)]) -> Channel {
        Channel {
            name: "stable".into(),
            channel_build: 1,
            min_build: 0,
            yanked: vec![],
            pin: pins.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            meta: BTreeMap::new(),
        }
    }

    // Grouping: the rustc tuple is one group; ny is an independent singleton; an unpinned-
    // but-named or pinned-but-unnamed program is excluded.
    #[test]
    fn plan_groups_partitions_by_coherence_group() {
        let idx = index_with_groups();
        // dotfiles is pinned but NOT named in the index → excluded (reachability).
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

    /// The plan is DEPENDENCY-FIRST (§17.10): a group follows every group it requires —
    /// the union of its members' `requires` counts for a tuple — with the partition's own
    /// order (named groups, then singletons, by name) as the stable tiebreak; a
    /// requirement outside the plan constrains nothing; and a plan with no `requires` is
    /// exactly the partition.
    #[test]
    fn plan_groups_orders_requirements_first_with_plan_order_as_the_tiebreak() {
        let names = |groups: &[Group]| -> Vec<String> {
            groups
                .iter()
                .map(|g| g.group.clone().unwrap_or_else(|| g.members[0].clone()))
                .collect()
        };
        // Only brew's `clt` edge among the pinned: the partition order stands except
        // that brew follows clt.
        let idx = index_with_groups();
        let ch = channel(&[("trust", 1), ("ay", 1), ("ny", 1), ("brew", 1), ("clt", 1)]);
        assert_eq!(
            names(&plan_groups(&idx, &ch)),
            ["rustc", "clt", "brew", "ny"]
        );
        // ny requires trust: rustc already precedes ny — untouched.
        let idx = index_with_requires("", "", "requires = [\"trust\"]");
        let ch = channel(&[("trust", 1), ("ay", 1), ("ny", 1)]);
        assert_eq!(names(&plan_groups(&idx, &ch)), ["rustc", "ny"]);
        // trust requires ny: the WHOLE rustc tuple now follows ny.
        let idx = index_with_requires("requires = [\"ny\"]", "", "");
        let plan = plan_groups(&idx, &ch);
        assert_eq!(names(&plan), ["ny", "rustc"]);
        assert_eq!(
            group_requires(&idx, &plan[1]),
            vec!["ny".to_string()],
            "the tuple's requirement is the union of its members'"
        );
        // A requirement INSIDE the tuple (ay requires trust) is not a group requirement
        // and moves nothing; one outside the plan (trust requires clt, clt unpinned)
        // moves nothing either.
        let idx = index_with_requires("requires = [\"clt\"]", "requires = [\"trust\"]", "");
        let plan = plan_groups(&idx, &ch);
        assert_eq!(names(&plan), ["rustc", "ny"]);
        assert_eq!(group_requires(&idx, &plan[0]), vec!["clt".to_string()]);
        // Stable tiebreak with a chain: brew → clt, ny → brew; singletons are brew, clt,
        // ny by name, and the order becomes clt, brew, ny — every step the first READY
        // group in plan order.
        let idx = index_with_requires("", "", "requires = [\"brew\"]");
        let ch = channel(&[("brew", 1), ("clt", 1), ("ny", 1)]);
        assert_eq!(names(&plan_groups(&idx, &ch)), ["clt", "brew", "ny"]);
        // The output is always a permutation of the partition.
        let ch = channel(&[("trust", 1), ("ay", 1), ("ny", 1), ("brew", 1), ("clt", 1)]);
        let mut a = names(&plan_groups(&idx, &ch));
        a.sort();
        assert_eq!(a, ["brew", "clt", "ny", "rustc"]);
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
                true
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
                true
            },
            &mut |n| {
                order.borrow_mut().push(format!("flip:{n}"));
                true
            },
            &mut |_| {},
        );
        assert_eq!(out, TxnOutcome::Applied(vec!["ay".into(), "trust".into()]));
        // CRUCIAL: every stage happens before any flip (stage-all → flip-all).
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
            &mut |n| n != "trust", // trust fails to stage
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
                during_flip: false
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
            &mut |_| true,         // both stage ok
            &mut |n| n != "trust", // ay flips ok, trust's flip fails
            &mut |n| rolled.push(n.to_string()),
        );
        assert_eq!(
            out,
            TxnOutcome::Aborted {
                failed: "trust".into(),
                during_flip: true
            }
        );
        assert_eq!(
            rolled,
            vec!["ay"],
            "the already-flipped member is rolled back"
        );
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
                true
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
