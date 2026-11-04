// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weighing quorum candidates in pairs (`eval/weigh.rs`): the walk finds what the pair loop it
//! replaced found, on every log, and its cost is counted (never timed) on the Sybil floods of
//! `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2.

use super::super::eval::{self, WeighCost};
use super::*;

/// Every record `log` holds, the genesis first.
fn held(log: &Log) -> Vec<Record> {
    log.records().cloned().collect()
}

fn sybil(tag: u8) -> (Mac, String) {
    (Mac(tag), format!("m{tag}"))
}

/// The removal flood from distinct Sybils: A removes X on our branch; on X's branch X admits Y and
/// Z, then for each `k` X admits a Sybil, the Sybil removes the one before it at that one's entry,
/// and X, Y and Z remove the Sybil at its own entry (`5k + 3` records, `k` quorums).
fn distinct_flood(k: u8) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    theirs.keep(admit(&theirs, X, Y, "m4"));
    theirs.keep(admit(&theirs, X, Z, "m5"));
    let mut before: Option<(Mac, RecordId)> = None;
    for i in 1..=k {
        let (mac, label) = sybil(100 + i);
        let entry = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        if let Some((prev, at)) = before {
            theirs.keep(remove(&theirs, mac, prev, &at));
        }
        for by in [X, Y, Z] {
            theirs.keep(remove(&theirs, by, mac, &entry));
        }
        before = Some((mac, entry));
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// What X does for P in [`redundant_removal_flood`] after each Sybil repeats Y's removal of P.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Restore {
    /// Nothing: Y's removal of P stands for good.
    Never,
    /// X lets P in again once, after Y's removal and before the Sybils.
    Once,
    /// X lets P in again after every Sybil's removal, and W joins the shares.
    EachTime,
    /// As `EachTime`, and P lets a fresh Mac in after each restore, which Y removes at its entry.
    EachTimeLettingIn,
}

/// Sybils repeating a removal another member made: A removes X on our branch; on X's branch X
/// admits Y and Z (and W), then P; P admits `m` Macs; Y removes P at its entry; then for each `k`
/// X admits a Sybil, the Sybil removes P at P's entry too, and the sharers remove the Sybil at its
/// own entry (`6 + m + 5k` records with no restore).
fn redundant_removal_flood(m: u8, k: u8, restore: Restore) -> Log {
    let each = matches!(restore, Restore::EachTime | Restore::EachTimeLettingIn);
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    theirs.keep(admit(&theirs, X, Y, "m4"));
    theirs.keep(admit(&theirs, X, Z, "m5"));
    let mut sharers = vec![X, Y, Z];
    if each {
        theirs.keep(admit(&theirs, X, W, "m6"));
        sharers.push(W);
    }
    let entry = *theirs.keep(admit(&theirs, X, P, "m7")).id();
    for i in 0..m {
        let (mac, label) = sybil(100 + i);
        theirs.keep(admit(&theirs, P, mac, &label));
    }
    theirs.keep(remove(&theirs, Y, P, &entry));
    if restore == Restore::Once {
        theirs.keep(admit(&theirs, X, P, "m7"));
    }
    for i in 1..=k {
        let (mac, label) = sybil(200 + i);
        let at = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        theirs.keep(remove(&theirs, mac, P, &entry));
        if each {
            theirs.keep(admit(&theirs, X, P, "m7"));
        }
        if restore == Restore::EachTimeLettingIn {
            let (fresh, label) = sybil(15 + i);
            let let_in = *theirs.keep(admit(&theirs, P, fresh, &label)).id();
            theirs.keep(remove(&theirs, Y, fresh, &let_in));
        }
        for &by in &sharers {
            theirs.keep(remove(&theirs, by, mac, &at));
        }
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// Sybils that each restore a removed Mac: A removes X on our branch; on X's branch X admits Y, Z,
/// W, V and P, and Y removes P at its entry; then for each `k` X admits a Sybil, the Sybil lets P
/// in again, P lets a fresh Mac in, and X, Y, Z, W and V remove the Sybil at its entry (`8 + 8k`
/// records). With `resign`, X then lets each Sybil in again, it lets one more Mac in, and Y
/// removes it at that second entry, so no share is final (`8 + 11k` records).
fn sybil_restore_flood(k: u8, resign: bool) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6"), (V, "m7")] {
        theirs.keep(admit(&theirs, X, who, label));
    }
    let entry = *theirs.keep(admit(&theirs, X, P, "m8")).id();
    theirs.keep(remove(&theirs, Y, P, &entry));
    for i in 1..=k {
        let (mac, label) = sybil(200 + i);
        let at = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        theirs.keep(admit(&theirs, mac, P, "m8"));
        let (fresh, fresh_label) = sybil(99 + i);
        theirs.keep(admit(&theirs, P, fresh, &fresh_label));
        for by in [X, Y, Z, W, V] {
            theirs.keep(remove(&theirs, by, mac, &at));
        }
        if resign {
            let again = *theirs.keep(admit(&theirs, X, mac, &label)).id();
            let (more, more_label) = sybil(149 + i);
            theirs.keep(admit(&theirs, mac, more, &more_label));
            theirs.keep(remove(&theirs, Y, mac, &again));
        }
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// A two-level restorer chain per Sybil: A removes X on our branch; on X's branch X admits Y, Z,
/// W, V, R, P and Q, and Y removes P and Q at their entries; then for each `k` X admits a Sybil,
/// the Sybil lets P in again, P lets Q in again, Q lets a fresh Mac in, Y removes that Mac at its
/// entry, and X, Y, Z, W, V and R remove the Sybil at its entry (`11 + 11k` records).
fn restore_chain_flood(k: u8) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6"), (V, "m7"), (R, "m8")] {
        theirs.keep(admit(&theirs, X, who, label));
    }
    let p_in = *theirs.keep(admit(&theirs, X, P, "m9")).id();
    let q_in = *theirs.keep(admit(&theirs, X, Q, "m10")).id();
    theirs.keep(remove(&theirs, Y, P, &p_in));
    theirs.keep(remove(&theirs, Y, Q, &q_in));
    for i in 1..=k {
        let (mac, label) = sybil(200 + i);
        let at = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        theirs.keep(admit(&theirs, mac, P, "m9"));
        theirs.keep(admit(&theirs, P, Q, "m10"));
        let (fresh, fresh_label) = sybil(99 + i);
        let let_in = *theirs.keep(admit(&theirs, Q, fresh, &fresh_label)).id();
        theirs.keep(remove(&theirs, Y, fresh, &let_in));
        for by in [X, Y, Z, W, V, R] {
            theirs.keep(remove(&theirs, by, mac, &at));
        }
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// Q, a Mac only [`restore_chain_flood`] and the restore-chain logs use.
const Q: Mac = Mac(15);

/// Removals of one Mac at many cuts: A removes X on our branch; on X's branch X admits Y, Z, W
/// and P, then a Sybil; P admits `m` Macs; the Sybil removes P at each of those admissions; X, Y,
/// Z and W remove the Sybil at its entry; then `c` Sybils follow as in [`distinct_flood`], each
/// removed by X, Y, Z and W (`2m + 6c + 10` records once `c` > 0).
fn many_cuts_flood(m: u8, c: u8) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    let sharers = [X, Y, Z, W];
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6"), (P, "m7")] {
        theirs.keep(admit(&theirs, X, who, label));
    }
    let (cutter, cutter_label) = sybil(250);
    let cutter_in = *theirs.keep(admit(&theirs, X, cutter, &cutter_label)).id();
    let mut cuts = Vec::new();
    for i in 0..m {
        let (mac, label) = sybil(100 + i);
        cuts.push(*theirs.keep(admit(&theirs, P, mac, &label)).id());
    }
    for cut in &cuts {
        theirs.keep(remove(&theirs, cutter, P, cut));
    }
    for by in sharers {
        theirs.keep(remove(&theirs, by, cutter, &cutter_in));
    }
    let mut before: Option<(Mac, RecordId)> = None;
    for i in 0..c {
        let (mac, label) = sybil(200 - c + i);
        let entry = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        if let Some((prev, at)) = before {
            theirs.keep(remove(&theirs, mac, prev, &at));
        }
        for by in sharers {
            theirs.keep(remove(&theirs, by, mac, &entry));
        }
        before = Some((mac, entry));
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// Sybils each removing one Mac at each of its names: A removes X on our branch; on X's branch X
/// admits Y, Z, W and P, and P signs `j` names; then for each `k` X admits a Sybil, the Sybil
/// removes P at each of P's names, and X, Y, Z and W remove the Sybil at its entry
/// (`8 + j + (j + 5)k` records). Nobody lets P in again, and every Sybil's removals of P are void
/// only where its own quorum's records are, so none is live in every state.
fn name_cuts_flood(j: u8, k: u8) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    let sharers = [X, Y, Z, W];
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6"), (P, "m7")] {
        theirs.keep(admit(&theirs, X, who, label));
    }
    let mut names = Vec::new();
    for c in 0..j {
        let name = sign(
            &theirs,
            P,
            Body::Name {
                name: format!("p{c}"),
            },
        );
        names.push(*theirs.keep(name).id());
    }
    for i in 1..=k {
        let (mac, label) = sybil(100 + i);
        let at = *theirs.keep(admit(&theirs, X, mac, &label)).id();
        for name in &names {
            theirs.keep(remove(&theirs, mac, P, name));
        }
        for by in sharers {
            theirs.keep(remove(&theirs, by, mac, &at));
        }
    }
    heal(&mut ours, &mut theirs);
    ours
}

/// One branch of [`concurrent_reach_logs`]: X admits `removed`, then a Sybil (tag `tag`);
/// `removed` admits the Macs `macs`; the Sybil removes `removed` at its entry; and `sharers`
/// remove the Sybil at the Sybil's entry, which this returns.
fn removed_on_a_branch(
    log: &mut Log,
    removed: Mac,
    tag: u8,
    macs: std::ops::Range<u8>,
    sharers: &[Mac],
) -> RecordId {
    let label = format!("m{}", removed.0);
    let entry = *log.keep(admit(log, X, removed, &label)).id();
    let (sybil_mac, sybil_label) = sybil(tag);
    let at = *log.keep(admit(log, X, sybil_mac, &sybil_label)).id();
    for tag in macs {
        let (mac, label) = sybil(tag);
        log.keep(admit(log, removed, mac, &label));
    }
    log.keep(remove(log, sybil_mac, removed, &entry));
    for &by in sharers {
        log.keep(remove(log, by, sybil_mac, &at));
    }
    at
}

/// Two quorums on concurrent branches that reach different numbers of Macs, weighed at a third
/// cutoff after both: X admits Y, Z and W; on one branch a Sybil removes P, which let six Macs
/// in, on the other one removes Q, which let two in, and X, Y, Z and W remove each Sybil; once
/// healed, X admits F and they remove F too. Returns the log in both orders of the branches,
/// and the two Sybils' entries.
fn concurrent_reach_logs() -> (Log, Log, [RecordId; 2]) {
    let sharers = [X, Y, Z, W];
    let mut one = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6")] {
        one.keep(admit(&one, X, who, label));
    }
    let mut two = one.clone();
    let p_cut = removed_on_a_branch(&mut one, P, 201, 100..106, &sharers);
    let q_cut = removed_on_a_branch(&mut two, Q, 202, 110..112, &sharers);
    heal(&mut one, &mut two);
    let (f, f_label) = sybil(203);
    let at = one.keep(admit(&one, X, f, &f_label));
    two.keep(at.clone());
    for by in sharers {
        two.keep(one.keep(remove(&one, by, f, at.id())));
    }
    (one, two, [p_cut, q_cut])
}

/// The quorums that took effect, by member and cutoff.
fn quorums(log: &Log) -> BTreeSet<(MemberId, RecordId)> {
    log.membership()
        .quorums()
        .iter()
        .map(|q| (q.member, q.cutoff))
        .collect()
}

/// The members of the quorums that took effect.
fn quorum_members(log: &Log) -> BTreeSet<MemberId> {
    log.membership()
        .quorums()
        .iter()
        .map(|q| q.member)
        .collect()
}

/// The walk's cost and the pair loop's, on `log`'s records.
fn costs(log: &Log) -> (WeighCost, WeighCost) {
    let records = held(log);
    (
        eval::weigh_cost(&records),
        eval::weigh_cost_by_pairs(&records),
    )
}

/// One Sybil per removal, each removing the Sybil before it: every quorum's member signed a
/// membership record that every later cutoff holds, so the pair loop re-ran a full pass for
/// about half of all pairs (K³/2). The walk re-checks nothing past the passes that weigh each
/// candidate apart: X's own removals of each Sybil keep it out in both states.
///
/// Measured at K = 64 (323 records): 4,416 `is_member_upto` calls, every one of them weighing a
/// candidate apart; the pair loop 139,173; the bound 13,041.
/// NEGATIVE CONTROL: the pair loop in place of the walk (`accepted_by_pairs`): 143,589, fails.
/// NEGATIVE CONTROL: the walk re-checks every actor at every step that differs from plain:
/// 139,173, fails.
#[test]
fn a_removal_flood_from_distinct_sybils_weighs_in_quadratic_evaluations() {
    let log = distinct_flood(64);
    let records = held(&log);
    assert_eq!(records.len(), 323);
    assert_eq!(members(&log), ids(&[A, B]));
    assert_eq!(log.membership().quorums().len(), 64);
    let cost = eval::weigh_cost(&records);
    assert!(
        cost.evaluations <= 323 * 323 / 8,
        "{} is_member_upto calls",
        cost.evaluations
    );
}

/// Sybils repeating a removal another member made of a Mac that let 100 others in: each Sybil's
/// quorum voids its removal of P, and P's admissions all read that removal, so the pair loop's
/// passes, and a reach with no pins, re-check P and the 100 Macs at every step. Y's removal of P
/// is live in every state, no `Admit` of P follows it, and none P signed comes after: it keeps P
/// out and P's admissions cut, so nothing is re-checked.
///
/// Measured at m = 100, K = 50 (356 records): 7,956 calls; the pair loop 199,056; the bound
/// 15,842.
/// NEGATIVE CONTROL: the pins off (`pinned_out` and `pinned_cut` always false): 131,681, fails.
#[test]
fn a_redundant_removal_flood_weighs_in_quadratic_evaluations() {
    let log = redundant_removal_flood(100, 50, Restore::Never);
    let records = held(&log);
    assert_eq!(records.len(), 356);
    assert_eq!(members(&log), ids(&[A, B]));
    assert_eq!(log.membership().quorums().len(), 50);
    let cost = eval::weigh_cost(&records);
    assert!(
        cost.evaluations <= 356 * 356 / 8,
        "{} is_member_upto calls",
        cost.evaluations
    );
}

/// The same flood with P let in again: once before the Sybils (the first Sybil's removal then
/// keeps P out, and its quorum does not hold apart), or after every Sybil's removal (Y's removal
/// is then final but not the last word on P: it leaves P's admissions cut for good, and only P
/// is re-checked).
///
/// Measured: 357 records 7,956 calls (the pair loop 191,412, bound 15,931); 457 records 9,232
/// (the pair loop 200,332, bound 26,106).
/// NEGATIVE CONTROL: final only where no `Admit` of the target follows (`cuts_for_good` set to
/// `pins_out`): 131,732 calls at 457 records, fails.
#[test]
fn a_redundant_removal_flood_after_restores_weighs_in_quadratic_evaluations() {
    for (restore, len, held_quorums) in [(Restore::Once, 357, 49), (Restore::EachTime, 457, 50)] {
        let log = redundant_removal_flood(100, 50, restore);
        let records = held(&log);
        assert_eq!(records.len(), len);
        assert_eq!(members(&log), ids(&[A, B]));
        assert_eq!(log.membership().quorums().len(), held_quorums);
        let cost = eval::weigh_cost(&records);
        assert!(
            cost.evaluations <= len * len / 8,
            "{} is_member_upto calls at {len} records",
            cost.evaluations
        );
    }
}

/// Sybils that each let a removed Mac in again, which then lets a fresh Mac in: each Sybil's
/// restore is toggled by its own quorum, but the shares that remove the Sybil are final and live
/// in every state, so the restore reads ineffective either way and reaches nobody; only the fresh
/// Mac the restore let in is re-checked.
///
/// Measured at K = 50 (408 records): 7,801 calls; the pair loop 137,700; the bound 20,808.
/// NEGATIVE CONTROL: no dead-`Admit` pin (`dead` always false): 34,133 calls, fails.
#[test]
fn a_sybil_restore_flood_weighs_in_quadratic_evaluations() {
    let log = sybil_restore_flood(50, false);
    let records = held(&log);
    assert_eq!(records.len(), 408);
    assert_eq!(members(&log), ids(&[A, B]));
    assert_eq!(log.membership().quorums().len(), 50);
    let cost = eval::weigh_cost(&records);
    assert!(
        cost.evaluations <= 408 * 408 / 8,
        "{} is_member_upto calls",
        cost.evaluations
    );
}

/// A two-level restorer chain per Sybil re-checks few Macs, but each restore of Q reached every
/// `Admit` Q signed, once per restore: the bookkeeping, not the re-checks, grew. The reach now
/// follows only an `Admit` that can change a Mac or restore one; Q's `Admit`s let in Macs Y
/// removed at entry, so none is followed. Work counts the calls and every record the walk's
/// bookkeeping reads.
///
/// Doubling the Sybils at most quadruples the work and a half (4.5 times, where a cubic shape
/// gives 8), so the test holds at any size where the shape stays quadratic.
///
/// Measured at K = 40 (451 records): work 16,782 (4,380 calls); the pair loop's 73,800 calls;
/// the bound 25,425. At K = 20 (231 records): work 4,192, so doubling cost 4.00 times.
/// NEGATIVE CONTROL: every `Admit` the restored Mac signed followed (`victims` unfiltered): work
/// 57,082 at K = 40 and 8,942 at K = 20 (6.38 times), fails.
#[test]
fn a_restore_chain_flood_keeps_its_bookkeeping_quadratic() {
    let log = restore_chain_flood(40);
    let records = held(&log);
    assert_eq!(records.len(), 451);
    assert_eq!(members(&log), ids(&[A, B]));
    let (walk, pairs) = costs(&log);
    let (half, _) = costs(&restore_chain_flood(20));
    assert!(
        2 * walk.work() <= 9 * half.work(),
        "work {walk:?} at K = 40 against {half:?} at K = 20"
    );
    assert!(walk.work() <= 451 * 451 / 8, "work {walk:?}");
    assert!(
        walk.work() <= pairs.work(),
        "work {walk:?} against {pairs:?}"
    );
}

/// A Sybil that removes P at each of P's 48 admissions: its quorum toggles all 48 removals at
/// every later cutoff, each with a cutoff of its own that reaches every one of those admissions,
/// so following them would read about 48² records a step. A step whose pin and reach reads pass a
/// full re-check (every actor, and every reader record up to the cutoff) re-checks every actor
/// instead, so the walk's work stays below the pair loop's re-checks, and its calls within the
/// quadratic bound.
///
/// Measured at m = 48, c = 40 (346 records): work 57,916 (11,424 calls, 78 steps re-checked
/// whole); the pair loop's 78,912 calls; the bound 14,964.
/// NEGATIVE CONTROL: no budget: work 746,027, fails.
/// NEGATIVE CONTROL: the pair loop in place of the walk (`accepted_by_pairs` in `weigh_all`):
/// 82,848 calls, past the bound, fails.
#[test]
fn a_removal_at_many_cuts_reads_no_more_than_a_full_re_check_a_step() {
    let log = many_cuts_flood(48, 40);
    let records = held(&log);
    assert_eq!(records.len(), 346);
    // The Sybil's quorum voids its removals of P, so the Macs P let in stand.
    let mut want = ids(&[A, B]);
    want.extend((100..148).map(|tag| Mac(tag).id()));
    assert_eq!(members(&log), want);
    let (walk, pairs) = costs(&log);
    assert!(
        walk.evaluations <= 346 * 346 / 8,
        "{} is_member_upto calls",
        walk.evaluations
    );
    assert!(
        walk.work() <= pairs.work(),
        "work {walk:?} against {pairs:?}"
    );
}

/// Sybils each removing P at both of P's names: every Sybil's quorum toggles its own two removals
/// of P, while the other Sybils' removals of P stand in both states and keep P out, so nothing is
/// re-checked. A pin's scan of the removals that stand in some states but not every one stops at
/// the first that stands in both, within a few entries; a step charged with every entry of those
/// lists (about two a Sybil, twice a toggled removal) passed a full re-check at most cutoffs and
/// re-checked every actor there, as the pair loop did.
///
/// Measured at j = 2, K = 50 (358 records): 2,966 calls; the pair loop 72,789; the bound 16,020.
/// NEGATIVE CONTROL: each pin charged its lists' whole length before scanning them: 69,143 calls
/// (1,161 steps re-checked whole), fails.
#[test]
fn a_removal_of_one_mac_at_each_of_its_names_weighs_in_quadratic_evaluations() {
    let log = name_cuts_flood(2, 50);
    let records = held(&log);
    assert_eq!(records.len(), 358);
    assert_eq!(members(&log), ids(&[A, B]));
    assert_eq!(log.membership().quorums().len(), 50);
    let cost = eval::weigh_cost(&records);
    assert!(
        cost.evaluations <= 358 * 358 / 8,
        "{} is_member_upto calls",
        cost.evaluations
    );
}

/// The restorer chain: M lets P in again after Y removed it, and P lets T in. K removes M from
/// the genesis, and a quorum removes K; W's quorum holds only while K's removal of M stands.
fn restorer_chain() -> Log {
    let mut lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (X, "m4"),
        (M, "m5"),
        (K, "m6"),
        (Y, "m7"),
        (W, "m8"),
        (V, "m9"),
        (U, "m10"),
    ]);
    let p_in = *lab.keep(admit(&lab, X, P, "m11")).id();
    lab.keep(remove(&lab, Y, P, &p_in));
    lab.keep(admit(&lab, M, P, "m11"));
    lab.keep(admit(&lab, P, T, "m12"));
    let ck = *lab
        .keep(sign(&lab, K, Body::Name { name: "k".into() }))
        .id();
    lab.keep(remove(&lab, K, M, lab.network()));
    for by in [A, B, C, X, Y, V] {
        lab.keep(remove(&lab, by, K, &ck));
    }
    let cw = *lab
        .keep(sign(&lab, W, Body::Name { name: "w".into() }))
        .id();
    for by in [A, B, C, X, Y] {
        lab.keep(remove(&lab, by, W, &cw));
    }
    lab
}

/// M, K and T: Macs only the restorer-chain logs use.
const M: Mac = Mac(16);
const K: Mac = Mac(17);
const T: Mac = Mac(18);

/// With K's removal of M void, M is in, so is P and so is T, and W's quorum no longer holds:
/// re-checking T needs the reach through M's restore of P.
/// NEGATIVE CONTROL: no restore reaches (`restores` all false): W's quorum takes effect too.
/// NEGATIVE CONTROL: only the Macs the toggled records are about (no reach): the same.
#[test]
fn a_mac_let_in_after_a_restore_votes_once_a_quorum_voids_the_cut_of_its_restorer() {
    let log = restorer_chain();
    assert_eq!(quorum_members(&log), ids(&[K]));
    assert!(!log.membership().removal(&W.id()).is_some_and(|r| r.quorum));
    let membership = log.membership();
    for mac in [M, P, T] {
        assert!(membership.contains(&mac.id()));
    }
}

/// X lets P in again after Y's removal and P lets T in (before Y's removal with `t_first`), K
/// removes P at its entry and a quorum removes K, and A, B and C remove W, which holds only while
/// K's removal of P stands.
fn restored_late_cut(t_first: bool) -> Log {
    let mut lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (X, "m4"),
        (Y, "m5"),
        (K, "m6"),
        (W, "m7"),
    ]);
    let p_in = *lab.keep(admit(&lab, X, P, "m8")).id();
    if t_first {
        lab.keep(admit(&lab, P, T, "m9"));
    }
    lab.keep(remove(&lab, Y, P, &p_in));
    lab.keep(admit(&lab, X, P, "m8"));
    if !t_first {
        lab.keep(admit(&lab, P, T, "m9"));
    }
    let ck = *lab
        .keep(sign(&lab, K, Body::Name { name: "k".into() }))
        .id();
    lab.keep(remove(&lab, K, P, &p_in));
    for by in [A, B, C, X, Y] {
        lab.keep(remove(&lab, by, K, &ck));
    }
    let cw = *lab
        .keep(sign(&lab, W, Body::Name { name: "w".into() }))
        .id();
    for by in [A, B, C] {
        lab.keep(remove(&lab, by, W, &cw));
    }
    lab
}

/// A removal a restore undid pins nobody out: Y's removal of P is live everywhere, but an `Admit`
/// of P follows it, and one P signed after that, so it neither keeps P out nor cuts T.
/// NEGATIVE CONTROL: every standing `Remove` pins (`pins_out` and `cuts_for_good` all true): W's
/// quorum takes effect too.
#[test]
fn a_remove_a_restore_undid_pins_no_mac_out() {
    let log = restored_late_cut(false);
    assert_eq!(quorum_members(&log), ids(&[K]));
    let membership = log.membership();
    assert!(membership.contains(&P.id()) && membership.contains(&T.id()));
}

/// A final removal leaves its Mac to a later restore: P lets T in before Y's removal, so Y's
/// removal cuts T for good (no `Admit` P signed follows a restore) while X's restore still lets P
/// in once K's removal is void.
/// NEGATIVE CONTROL: a final `Remove` also pins its target out (`pins_out` set to
/// `cuts_for_good`): W's quorum takes effect too.
#[test]
fn a_final_remove_leaves_its_mac_to_a_later_restore() {
    let log = restored_late_cut(true);
    assert_eq!(quorum_members(&log), ids(&[K]));
    let membership = log.membership();
    assert!(membership.contains(&P.id()));
    assert!(!membership.contains(&T.id()));
}

/// Three levels of restore: M lets P in again, P lets Q in again, Q lets T in; K removes M and a
/// quorum removes K, and W's quorum holds only while K's removal of M stands.
fn three_level_restore() -> Log {
    let mut lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (X, "m4"),
        (M, "m5"),
        (K, "m6"),
        (Y, "m7"),
        (W, "m8"),
        (V, "m9"),
        (U, "m10"),
    ]);
    let p_in = *lab.keep(admit(&lab, X, P, "m11")).id();
    let q_in = *lab.keep(admit(&lab, X, Q, "m12")).id();
    lab.keep(remove(&lab, Y, P, &p_in));
    lab.keep(remove(&lab, Y, Q, &q_in));
    lab.keep(admit(&lab, M, P, "m11"));
    lab.keep(admit(&lab, P, Q, "m12"));
    lab.keep(admit(&lab, Q, T, "m13"));
    let ck = *lab
        .keep(sign(&lab, K, Body::Name { name: "k".into() }))
        .id();
    lab.keep(remove(&lab, K, M, lab.network()));
    for by in [A, B, C, X, Y, V, U] {
        lab.keep(remove(&lab, by, K, &ck));
    }
    let cw = *lab
        .keep(sign(&lab, W, Body::Name { name: "w".into() }))
        .id();
    for by in [A, B, C, X, Y] {
        lab.keep(remove(&lab, by, W, &cw));
    }
    lab
}

/// A fork inside a restorer chain: M, on one branch, lets P in again after Y removed it, and on
/// another signs a name; K removes M at that name, which leaves the restore outside its cutoff;
/// a quorum removes K, and W's quorum holds only while K's removal stands.
fn forked_restore() -> Log {
    let lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (X, "m4"),
        (M, "m5"),
        (K, "m6"),
        (Y, "m7"),
        (W, "m8"),
        (V, "m9"),
        (U, "m10"),
    ]);
    let mut one = lab.clone();
    let p_in = *one.keep(admit(&one, X, P, "m11")).id();
    one.keep(remove(&one, Y, P, &p_in));
    let mut two = one.clone();
    one.keep(admit(&one, M, P, "m11"));
    let named = *two
        .keep(sign(&two, M, Body::Name { name: "m".into() }))
        .id();
    heal(&mut one, &mut two);
    one.keep(admit(&one, P, T, "m12"));
    let ck = *one
        .keep(sign(&one, K, Body::Name { name: "k".into() }))
        .id();
    one.keep(remove(&one, K, M, &named));
    for by in [A, B, C, X, Y, V] {
        one.keep(remove(&one, by, K, &ck));
    }
    let cw = *one
        .keep(sign(&one, W, Body::Name { name: "w".into() }))
        .id();
    for by in [A, B, C, X, Y] {
        one.keep(remove(&one, by, W, &cw));
    }
    one
}

/// Both logs need the reach through every level: with K's removal of M void, W's quorum no
/// longer holds.
/// NEGATIVE CONTROL: no restore reaches (`restores` all false), no reach at all, or every
/// standing `Remove` pinning: W's quorum takes effect too, fails.
#[test]
fn every_level_of_a_restore_chain_votes_once_its_restorers_cut_is_void() {
    for log in [three_level_restore(), forked_restore()] {
        assert_eq!(quorum_members(&log), ids(&[K]));
        let membership = log.membership();
        for mac in [M, P, T] {
            assert!(membership.contains(&mac.id()));
        }
    }
    assert!(three_level_restore().membership().contains(&Q.id()));
}

/// The defeat log: W is removed at two cutoffs. X's removal of W at the first is one of that
/// quorum's shares; a quorum removes X at X's own record, which voids that share and so defeats
/// the quorum at the first cutoff, while the quorum at W's later record still takes effect and
/// voids W's removals of B and C. Returns the log and W's removals.
fn defeat_log() -> (Log, [RecordId; 2]) {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (X, "m4"), (W, "m5")]);
    let mut main = lab.clone();
    let c1 = *main
        .keep(sign(&main, W, Body::Name { name: "c1".into() }))
        .id();
    let mut xb = main.clone();
    let c2 = *main
        .keep(sign(&main, W, Body::Name { name: "c2".into() }))
        .id();
    let wb = *main.keep(remove(&main, W, B, lab.network())).id();
    let wc = *main.keep(remove(&main, W, C, lab.network())).id();
    let cx = *xb.keep(sign(&xb, X, Body::Name { name: "cx".into() })).id();
    xb.keep(remove(&xb, X, W, &c1));
    heal(&mut main, &mut xb);
    for by in [A, B, C] {
        main.keep(remove(&main, by, W, &c2));
    }
    for by in [A, B] {
        main.keep(remove(&main, by, W, &c1));
    }
    for by in [A, B, C] {
        main.keep(remove(&main, by, X, &cx));
    }
    (main, [wb, wc])
}

/// A member's holding cutoffs need not reduce to one: an honest log holds two for W, and the
/// earlier one's defeat leaves the later one to take effect (`accepted` keeps one candidate per
/// member and cutoff).
/// NEGATIVE CONTROL: only each member's causally earliest holding cutoff weighed: W's quorum at
/// the later cutoff is missing, fails.
#[test]
fn a_member_removed_at_two_cutoffs_keeps_the_later_quorum_when_the_earlier_is_defeated() {
    let (log, removals) = defeat_log();
    assert_eq!(members(&log), ids(&[A, B, C]));
    let records = held(&log);
    let c2 = records
        .iter()
        .find(|r| matches!(r.body(), Body::Name { name } if name == "c2"))
        .unwrap();
    let cx = records
        .iter()
        .find(|r| matches!(r.body(), Body::Name { name } if name == "cx"))
        .unwrap();
    assert_eq!(
        quorums(&log),
        BTreeSet::from([(W.id(), *c2.id()), (X.id(), *cx.id())])
    );
    let voided = log.membership().voided().clone();
    for removal in &removals {
        assert!(voided.contains(removal));
    }
}

/// The fork log: M signs two records at one sequence on two branches, a removal of D and a
/// name; one quorum removes M at the removal, another at the name. Returns the two healed logs
/// (each holds the fork in the other order) with the shares in both, and the two cutoffs.
fn fork_logs() -> (Log, Log, [RecordId; 2]) {
    let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3"), (M, "m4")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    let r1a = *one.keep(remove(&one, M, D, lab.network())).id();
    let r1b = *two
        .keep(sign(&two, M, Body::Name { name: "m".into() }))
        .id();
    heal(&mut one, &mut two);
    let mut shares = Vec::new();
    for by in [B, C] {
        shares.push(one.keep(remove(&one, by, M, &r1a)));
    }
    for by in [C, D] {
        shares.push(one.keep(remove(&one, by, M, &r1b)));
    }
    for share in shares {
        two.keep(share);
    }
    (one, two, [r1a, r1b])
}

/// Neither of the fork log's cutoffs is in the other's past: both quorums take effect, and D is
/// in, whichever record a Mac holds first.
/// NEGATIVE CONTROL: only the first candidate per member in record order weighed: the two orders'
/// memberships differ, fails.
#[test]
fn incomparable_cutoffs_of_a_forked_member_both_take_effect_in_every_arrival_order() {
    let (one, two, [r1a, r1b]) = fork_logs();
    let order = |log: &Log| log.record_index(&r1a).unwrap() < log.record_index(&r1b).unwrap();
    assert_ne!(
        order(&one),
        order(&two),
        "the logs hold the fork in both orders"
    );
    for log in [&one, &two] {
        assert_eq!(members(log), ids(&[B, C, D]));
        assert_eq!(log.membership().quorums().len(), 2);
    }
    assert_eq!(one.membership(), two.membership());
}

/// A small xorshift generator: the same seed, the same logs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

/// The Macs `log` seats (its founders and every Mac an `Admit` another Mac signed lets in), and
/// every Mac its records name, by tag.
fn macs_in(log: &Log, tags: &BTreeMap<MemberId, u8>) -> (Vec<Mac>, Vec<Mac>) {
    let mut seated = BTreeSet::new();
    let mut known = BTreeSet::new();
    for record in log.records() {
        match record.body() {
            Body::Genesis { founders, .. } => {
                for f in founders {
                    if let Some(member) = f.member() {
                        seated.insert(tags[&member]);
                    }
                }
            }
            Body::Admit { entrant, .. } if record.actor() != &entrant.member() => {
                seated.insert(tags[&entrant.member()]);
            }
            _ => {}
        }
    }
    known.extend(seated.iter().copied());
    for record in log.records() {
        if let Body::Remove { member, .. } = record.body() {
            known.insert(tags[member]);
        }
    }
    (
        seated.into_iter().map(Mac).collect(),
        known.into_iter().map(Mac).collect(),
    )
}

/// The cutoffs `log` holds for `who`: the genesis for a founder, its own records, and the
/// `Admit`s that let it in.
fn cutoffs_of(log: &Log, who: Mac) -> Vec<RecordId> {
    let id = who.id();
    log.records()
        .filter(|r| match r.body() {
            Body::Genesis { founders, .. } => founders.iter().any(|f| f.member() == Some(id)),
            Body::Admit { entrant, .. } => r.actor() == &id || entrant.member() == id,
            _ => r.actor() == &id,
        })
        .map(|r| *r.id())
        .collect()
}

/// A random log from `seed`: 3 to 5 founders, 15 to 50 steps on up to three branches (clones,
/// healed by merging), each an admission (a fresh Mac 60% of the time), a removal at a cutoff
/// the branch holds, a quorum (2 to 4 seated signers removing one Mac at one cutoff), a name, a
/// split or a merge. Records the log refuses are skipped.
pub(super) fn generated_log(seed: u64) -> Log {
    let mut rng = Rng(seed);
    let founders = [A, B, C, D, E];
    let count = 3 + rng.below(3);
    let seats: Vec<(Mac, String)> = founders[..count]
        .iter()
        .map(|&mac| (mac, format!("m{}", mac.0)))
        .collect();
    let seats: Vec<(Mac, &str)> = seats.iter().map(|(m, l)| (*m, l.as_str())).collect();
    let lab = found(&seats);
    let mut tags: BTreeMap<MemberId, u8> = founders.iter().map(|m| (m.id(), m.0)).collect();
    let mut branches = vec![lab.clone()];
    let mut fresh = 20u8;
    let steps = 15 + rng.below(36);
    for _ in 0..steps {
        let b = rng.below(branches.len());
        let (seated, known) = macs_in(&branches[b], &tags);
        let log = &mut branches[b];
        let roll = rng.below(100);
        if roll < 22 {
            let by = rng.pick(&seated);
            let who = if rng.below(10) < 6 {
                let mac = Mac(fresh);
                fresh += 1;
                tags.insert(mac.id(), mac.0);
                mac
            } else {
                rng.pick(&known)
            };
            if who.0 != by.0 {
                let _ = log.insert(admit(log, by, who, &format!("m{}", who.0)));
            }
        } else if roll < 42 {
            let (by, who) = (rng.pick(&seated), rng.pick(&known));
            let cutoffs = cutoffs_of(log, who);
            if who.0 != by.0 && !cutoffs.is_empty() {
                let at = rng.pick(&cutoffs);
                let _ = log.insert(remove(log, by, who, &at));
            }
        } else if roll < 62 {
            let who = rng.pick(&known);
            let cutoffs = cutoffs_of(log, who);
            let signers: Vec<Mac> = seated.iter().copied().filter(|m| m.0 != who.0).collect();
            if !cutoffs.is_empty() && signers.len() >= 2 {
                let at = rng.pick(&cutoffs);
                let mut pool = signers;
                let take = 2 + rng.below(pool.len().min(4) - 1);
                for _ in 0..take {
                    let by = pool.remove(rng.below(pool.len()));
                    let _ = log.insert(remove(log, by, who, &at));
                }
            }
        } else if roll < 75 {
            let by = rng.pick(&seated);
            let _ = log.insert(sign(log, by, Body::Name { name: "n".into() }));
        } else if roll < 85 && branches.len() < 3 {
            let split = branches[b].clone();
            branches.push(split);
        } else if branches.len() > 1 {
            let other = (b + 1 + rng.below(branches.len() - 1)) % branches.len();
            let (lo, hi) = (b.min(other), b.max(other));
            let (left, right) = branches.split_at_mut(hi);
            heal(&mut left[lo], &mut right[0]);
            branches.remove(hi);
        }
    }
    let mut all = branches.remove(0);
    for other in &branches {
        all.merge(other).unwrap();
    }
    all
}

/// The same records in another causal order, from `seed`: Kahn's algorithm, each step taking a
/// random record whose parents are all placed, inserted into a log of the genesis alone.
fn permuted(log: &Log, seed: u64) -> Log {
    let mut rng = Rng(seed);
    let records = held(log);
    let at: BTreeMap<RecordId, usize> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (*r.id(), i))
        .collect();
    let mut waiting: Vec<usize> = records
        .iter()
        .map(|r| r.parents().iter().filter(|p| at.contains_key(*p)).count())
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); records.len()];
    for (i, r) in records.iter().enumerate() {
        for p in r.parents() {
            if let Some(&p) = at.get(p) {
                children[p].push(i);
            }
        }
    }
    let mut out = Log::new(log.genesis().clone()).unwrap();
    let mut ready: Vec<usize> = children[0].clone();
    for &c in &ready {
        waiting[c] -= 1;
    }
    ready.retain(|&c| waiting[c] == 0);
    ready.dedup();
    while !ready.is_empty() {
        let i = ready.swap_remove(rng.below(ready.len()));
        out.keep(records[i].clone());
        for &c in &children[i] {
            waiting[c] -= 1;
            if waiting[c] == 0 {
                ready.push(c);
            }
        }
    }
    assert_eq!(out.records().count(), records.len());
    out
}

/// Every hand-made log of this file and of the quorum tests it pins, small.
pub(super) fn corpus() -> Vec<(String, Log)> {
    let mut logs: Vec<(String, Log)> = vec![
        ("restorer chain".into(), restorer_chain()),
        ("restored late cut".into(), restored_late_cut(false)),
        ("final cut".into(), restored_late_cut(true)),
        ("three-level restore".into(), three_level_restore()),
        ("forked restore".into(), forked_restore()),
        ("defeat".into(), defeat_log().0),
        ("admit chain".into(), admit_chain()),
        ("distinct K=8".into(), distinct_flood(8)),
        ("§4.2 N=32".into(), section_4_2_flood(32)),
        (
            "redundant m=10 K=8".into(),
            redundant_removal_flood(10, 8, Restore::Never),
        ),
        (
            "restored once m=10 K=8".into(),
            redundant_removal_flood(10, 8, Restore::Once),
        ),
        (
            "restored each m=10 K=8".into(),
            redundant_removal_flood(10, 8, Restore::EachTime),
        ),
        (
            "restored letting in m=10 K=8".into(),
            redundant_removal_flood(10, 8, Restore::EachTimeLettingIn),
        ),
        ("sybil restore K=8".into(), sybil_restore_flood(8, false)),
        (
            "sybil restore resigning K=8".into(),
            sybil_restore_flood(8, true),
        ),
        ("restore chain K=6".into(), restore_chain_flood(6)),
        ("every kind".into(), super::every_kind().0),
    ];
    let (one, two, _) = fork_logs();
    logs.push(("fork, removal first".into(), one));
    logs.push(("fork, name first".into(), two));
    let (one, two, _) = concurrent_reach_logs();
    logs.push(("concurrent reach, P first".into(), one));
    logs.push(("concurrent reach, Q first".into(), two));
    logs.push(("many cuts m=6 c=4".into(), many_cuts_flood(6, 4)));
    logs.push(("name cuts j=2 K=8".into(), name_cuts_flood(2, 8)));
    logs.push(("name cuts j=3 K=6".into(), name_cuts_flood(3, 6)));
    logs
}

pub(in crate::membership_log) fn relation_baseline_corpus() -> Vec<(String, Log)> {
    let mut logs = corpus();
    logs.extend([
        ("distinct K=64".into(), distinct_flood(64)),
        (
            "redundant m=100 K=50".into(),
            redundant_removal_flood(100, 50, Restore::Never),
        ),
        ("restore chain K=40".into(), restore_chain_flood(40)),
        ("many cuts m=48 c=40".into(), many_cuts_flood(48, 40)),
        ("section4.2 N=256".into(), section_4_2_flood(256)),
    ]);
    logs
}

/// The admit-chain log: P lets Q in, W removes P from the genesis after signing n1, a quorum
/// removes C at C's record, and another removes W at n1. With W's removal of P void, Q's
/// membership changes only through P's `Admit` of it being effective again.
fn admit_chain() -> Log {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (W, "m4"), (P, "m5")]);
    lab.keep(admit(&lab, P, Q, "m6"));
    let n1 = *lab
        .keep(sign(&lab, W, Body::Name { name: "n1".into() }))
        .id();
    lab.keep(remove(&lab, W, P, lab.network()));
    let nc = *lab
        .keep(sign(&lab, C, Body::Name { name: "nc".into() }))
        .id();
    for by in [A, B] {
        lab.keep(remove(&lab, by, C, &nc));
    }
    for by in [A, B, C] {
        lab.keep(remove(&lab, by, W, &n1));
    }
    lab
}

/// How many generated logs the differential and order tests read.
pub(super) const GENERATED: u64 = 200;

/// The walk finds what the pair loop it replaced finds: the same quorums take effect, and one
/// undoes and strips another in the same pairs, on every hand-made log and on generated ones;
/// and at every step the walk's members equal a full pass, and every pair's weighing equals
/// [`Graph::weigh`]'s (a checked walk).
/// NEGATIVE CONTROLS, each failing at a checked step whose members differ from a full pass: only
/// the Macs the toggled records are about (no reach); no restore reaches (`restores` all false);
/// every standing `Remove` pins; a final `Remove` pins its target out.
#[test]
fn the_weighing_walk_agrees_with_weighing_each_pair() {
    let generated = (1..=GENERATED).map(|seed| (format!("generated {seed}"), generated_log(seed)));
    let mut weighed_pairs = 0;
    for (name, log) in corpus().into_iter().chain(generated) {
        let records = held(&log);
        let by_pairs = eval::weigh_relations(&records, true);
        let walk = eval::weigh_relations(&records, false);
        assert_eq!(walk, by_pairs, "{name}");
        weighed_pairs += by_pairs.1.len();
    }
    assert!(weighed_pairs > 0, "some pair undoes another");
}

/// The walk never costs more `is_member_upto` calls than the pair loop it replaced, on any log;
/// its work (their scans and its bookkeeping) stays within three times the pair loop's, and below
/// it on every named flood.
/// NEGATIVE CONTROL: a step that reads as plain re-checks every actor anyway: 3,171 calls against
/// the pair loop's 2,541 on the distinct flood at K = 16, fails.
#[test]
fn weighing_never_costs_more_evaluations_than_weighing_each_pair() {
    let named = vec![
        ("distinct K=16".to_owned(), distinct_flood(16)),
        (
            "redundant m=20 K=10".into(),
            redundant_removal_flood(20, 10, Restore::Never),
        ),
        (
            "restored once m=20 K=10".into(),
            redundant_removal_flood(20, 10, Restore::Once),
        ),
        (
            "restored each m=20 K=10".into(),
            redundant_removal_flood(20, 10, Restore::EachTime),
        ),
        ("§4.2 N=64".into(), section_4_2_flood(64)),
        ("sybil restore K=12".into(), sybil_restore_flood(12, false)),
        ("restore chain K=12".into(), restore_chain_flood(12)),
    ];
    for (name, log) in &named {
        let (walk, pairs) = costs(log);
        assert!(walk.work() <= pairs.work(), "{name}: {walk:?} {pairs:?}");
    }
    let generated = (1..=GENERATED).map(|seed| (format!("generated {seed}"), generated_log(seed)));
    for (name, log) in corpus().into_iter().chain(generated) {
        let (walk, pairs) = costs(&log);
        assert!(
            walk.evaluations <= pairs.evaluations,
            "{name}: {walk:?} {pairs:?}"
        );
        assert!(
            walk.scans + walk.bookkeeping <= 3 * pairs.scans,
            "{name}: {walk:?} {pairs:?}"
        );
    }
}

/// Two Macs that hold the same records agree, whatever order the records reached them in: the
/// same membership, the same relations among the candidates by content, and the same number of
/// `is_member_upto` calls (nothing in the walk reads where a record sits in the order).
/// NEGATIVE CONTROL: the walk ordered by the cutoff's place in the order: the concurrent-reach
/// log, P's branch first, costs 70 calls as built and 74 in another arrival order, fails.
/// NEGATIVE CONTROL: only the first candidate per member in record order weighed: the fork log's
/// orders disagree, fails.
#[test]
fn membership_and_weighing_cost_are_the_same_in_every_arrival_order() {
    let mut logs = vec![
        ("distinct K=16".to_owned(), distinct_flood(16)),
        ("§4.2 N=32".into(), section_4_2_flood(32)),
        (
            "redundant m=10 K=8".into(),
            redundant_removal_flood(10, 8, Restore::EachTime),
        ),
    ];
    logs.extend(corpus());
    logs.extend((1..=GENERATED / 4).map(|seed| (format!("generated {seed}"), generated_log(seed))));
    let mut fork_orders = BTreeSet::new();
    let fork = fork_logs().2;
    for (name, log) in &logs {
        let records = held(log);
        let want = (
            log.membership(),
            eval::weigh_relations(&records, false),
            eval::weigh_cost(&records).evaluations,
        );
        for seed in 1..=3u64 {
            let other = permuted(log, seed * 7919 + records.len() as u64);
            if let (Some(a), Some(b)) = (other.record_index(&fork[0]), other.record_index(&fork[1]))
            {
                fork_orders.insert(a < b);
            }
            let theirs = held(&other);
            let got = (
                other.membership(),
                eval::weigh_relations(&theirs, false),
                eval::weigh_cost(&theirs).evaluations,
            );
            assert_eq!(got, want, "{name}, order {seed}");
        }
    }
    assert_eq!(fork_orders.len(), 2, "the fork log came in both orders");
    let (one, two, [p_cut, q_cut]) = concurrent_reach_logs();
    let order = |log: &Log| log.record_index(&p_cut).unwrap() < log.record_index(&q_cut).unwrap();
    assert_ne!(
        order(&one),
        order(&two),
        "the logs hold the branches in both orders"
    );
    assert_eq!(one.membership(), two.membership());
    let calls = |log: &Log| eval::weigh_cost(&held(log)).evaluations;
    assert_eq!(
        calls(&one),
        calls(&two),
        "the concurrent-reach log's two orders"
    );
}
