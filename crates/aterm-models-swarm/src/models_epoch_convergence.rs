// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of re-key convergence: one ranked minter per due re-key, the remover's grace,
//! the settle grace that bounds every hold (a Mac with no seat that a prefix may still decide
//! among them), the recheck under the author lock, and a head that arrives settled moving no
//! rank (`crates/aterm-secrets/src/client/remove.rs`, `tend_epochs_at`, and
//! `crates/aterm-secrets/src/client/log_entries.rs`'s `REMOVER_GRACE_MS` and `SETTLE_GRACE_MS`).

use aterm_spec::derive::*;

/// `EpochConvergence`: four lab shapes on an injected clock, each picked by its first action.
/// In the first three the Macs run their vault passes (the ranked gap closer, then the ranked
/// tender) over windows of `log_entries::GAP_STEP_MS` steps; `t` counts the windows run, and
/// each `…Passes` action runs the next. Every shape counts mints by audience: `mb` is the most
/// epochs the passes minted of any one audience before a heal (one ranked minter per due), and
/// `mn` the fewest of an audience the shape starts due (every hold ends).
///
/// `Rotation`: A removes a Mac and its batch (the `Remove`, A's rotations of `[everyone]` and
/// Commons) reaches every seated Mac but B, which holds the `Remove` alone (`ab` 1; 3 once it
/// holds the batch). Window 0 is every step before A's removal grace (`REMOVER_GRACE_MS`) ends,
/// window 1 the steps past it to every rank's, window 2 the steps after the heal. `Deliver`
/// brings B the batch within the grace (`fast`), `Heal` gives every seated Mac every record
/// after window 1. `cc` is 1 once a pass after the heal signs anything at all.
///
/// `Prefix`: X removes a Mac on its side while it holds a prefix of A's concurrent removal batch
/// (its `Remove`, its `[everyone]` `Epoch` and the first of its `Wrap`s: `ab` 2; 3 once it holds
/// it all), so X's newest epochs of `[everyone]` and Commons are due a re-key covering both. The
/// batch also wraps S, a Mac with no seat, last, so the prefix may still decide S
/// (`Log::undecided`): it holds `[everyone]`'s re-key, and Commons' too, which reads
/// `[everyone]`'s leave-out. No record marks a batch's end, so a whole batch that ends in
/// `Wrap`s reads as a prefix until its minter signs again: past `SETTLE_GRACE_MS` from first
/// sight the hold ends whether the batch settled or not. Window 0 is every step before the
/// settle grace ends, window 1 the steps past it. `Rest` brings X the rest of A's batch. `mab`
/// is what X held of A's batch when it first minted `[everyone]` (2 a prefix, 3 the whole), and
/// `mt` the window.
///
/// `Settled`: three concurrent Commons re-keys leave the audience due one covering it; B, a
/// carrier ranked `k` >= 1, holds two of them. Window 0 is B's pass at first sight, window 1
/// its pass 30 seconds on, window 2 its passes at each step to its rank's. `Head` brings B the
/// third, settled: before window 0 (`head` 1) or after it (`head` 2).
///
/// `Recheck`: the `Prefix` removals with no Mac outside the seats, X holding A's `Remove` alone
/// (`ab` 1), at one reading of the clock past every removal grace: X's newest epochs of both
/// audiences are due, and nothing holds them. `Observe` is X's scheduling look (`rd` 1 once it
/// found a re-key ready), `Arrive` a prefix of A's batch arriving (`ab` 2: its `Epoch` and first
/// `Wrap`s, first seen now, within its settle grace), and `Author` X signing under the author
/// lock (`rd` 2), looking again first: it re-keys only what is still ready, so a prefix that
/// arrived since the look holds `[everyone]`, while Commons, which no undecided Mac holds, is
/// re-keyed either way. `mab` 1 there is a `[everyone]` mint over A's `Remove` alone.
///
/// The laws: no `[everyone]` epoch is minted from a prefix of another Mac's batch before the
/// settle grace ends (`NoMintFromUnsettled`); a rotation delivered faster than the grace is the
/// only mint (`OneMintWhenFast`), one withheld past it costs one more mint of each audience, the
/// withheld Mac's, and then quiescence: after the heal nothing is signed (`AtMostTwoMints`);
/// and every hold has a time bound (`HoldEnds`): past the remover's grace a Mac the rotation did
/// not reach mints its own, past the settle grace X mints each due audience whatever it holds
/// of A's batch and whatever S's `Wrap` it lacks, and past its rank's step B mints though a head
/// arrived since. The settle grace is that bound and claims nothing more: a batch that ends in
/// `Wrap`s is closed only by it.
///
/// Mutants: 1 every Mac's turn first at once, with no grace, as the exchange's repair once
/// minted (`remove::UNRANKED`), so B mints its own rotation at its first pass; 2 a new head
/// moves the ranks to when this Mac first saw it (`remove::HEADS_MOVE_RANKS`); 3 every epoch
/// reads as settled, so X mints from the prefix (`rekey::UNSETTLED_OK`); 4 no newest epochs are
/// secure twins, so the heal's two rotations are due a coalesce (`rekey::TWINS_DUE`); 5 the
/// author signs from the scheduling look, every due audience, however its heads changed since
/// (`remove::STALE_AUTHORING`); 6 a Mac with no seat that a prefix may still decide holds the
/// re-key past the settle grace, as before the grace bounded it (`remove::UNDECIDED_HOLDS`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn epoch_convergence_model() -> Model {
    aterm_spec::ty_model! {
        EpochConvergence {
            const Buggy = 0;

            var sc = 0;
            var t = 0;
            var ab = 0;
            var fast = 0;
            var mb = 0;
            var mn = 0;
            var cc = 0;
            var healed = 0;
            var past = 0;
            var mab = 0;
            var mt = 0;
            var head = 0;
            var rd = 0;

            action Rotation when (sc == 0) {
                sc = 1;
                ab = 1;
            }
            action Prefix when (sc == 0) {
                sc = 2;
                ab = 2;
            }
            action Settled when (sc == 0) {
                sc = 3;
            }
            action Recheck when (sc == 0) {
                sc = 4;
                ab = 1;
            }

            action Deliver when (sc == 1 && ab == 1 && t <= 1) {
                ab = 3;
                fast = 1;
            }
            action Heal when (sc == 1 && t == 2 && healed == 0) {
                ab = 3;
                healed = 1;
            }
            // Mutant 1 lets B mint its own rotation at its first pass; mutant 4 finds the heal's
            // two rotations due a coalesce.
            action RotationPasses when (sc == 1 && (t <= 1 || (t == 2 && healed == 1))) {
                t = t + 1;
                mb = if t == 0 {
                    if Buggy == 1 && ab == 1 { 1 } else { mb }
                } else {
                    if t == 1 && ab == 1 { 1 } else { mb }
                };
                mn = if t == 0 {
                    if Buggy == 1 && ab == 1 { 1 } else { mn }
                } else {
                    if t == 1 && ab == 1 { 1 } else { mn }
                };
                cc = if t == 2 && Buggy == 4 && mb == 1 { 1 } else { cc };
                past = if t == 1 { 1 } else { past };
            }

            action Rest when (sc == 2 && ab == 2 && t <= 1) {
                ab = 3;
            }
            // `[everyone]` is re-keyed past the grace or once A's batch is whole, Commons too
            // (S's hold on it, through `[everyone]`'s leave-out, ends alike): `mb` turns 1 with
            // the first of the two mints, `mn` with the second. Mutant 3 reads the prefix as
            // settled, so X
            // re-keys `[everyone]` from it at once, while S still holds Commons to the grace;
            // mutant 6 holds `[everyone]` past the grace while the prefix may still decide S,
            // and X re-keys Commons alone.
            action PrefixPasses when (sc == 2 && t <= 1) {
                t = t + 1;
                mb = if mb == 0 && (t == 1 || ab == 3 || Buggy == 3) { 1 } else { mb };
                mn = if mn == 0 &&
                    ((if Buggy == 6 { 0 } else { t }) == 1 || ab == 3 || Buggy == 3) &&
                    (t == 1 || ab == 3) {
                    1
                } else {
                    mn
                };
                mab = if mab == 0 &&
                    ((if Buggy == 6 { 0 } else { t }) == 1 || ab == 3 || Buggy == 3) {
                    ab
                } else {
                    mab
                };
                mt = if mab == 0 &&
                    ((if Buggy == 6 { 0 } else { t }) == 1 || ab == 3 || Buggy == 3) {
                    t
                } else {
                    mt
                };
                past = if t == 1 { 1 } else { past };
            }

            action Head when (sc == 3 && head == 0 && t <= 1) {
                head = t + 1;
            }
            // Mutant 2 moves B's step to when it first saw a head that came after its first pass.
            action SettledPasses when (sc == 3 && t <= 2) {
                t = t + 1;
                mb = if t == 2 && mb == 0 {
                    if Buggy == 2 && head == 2 { 0 } else { 1 }
                } else {
                    mb
                };
                mn = if t == 2 && mn == 0 {
                    if Buggy == 2 && head == 2 { 0 } else { 1 }
                } else {
                    mn
                };
                past = if t == 2 { 1 } else { past };
            }

            action Observe when (sc == 4 && rd == 0) {
                rd = 1;
            }
            action Arrive when (sc == 4 && ab == 1) {
                ab = 2;
            }
            // Mutant 5 re-keys every due audience from the look; mutant 3 reads the prefix as
            // settled.
            action Author when (sc == 4 && rd == 1) {
                rd = 2;
                mb = 1;
                mn = if ab == 1 || Buggy == 5 || Buggy == 3 { 1 } else { 0 };
                mab = if ab == 1 || Buggy == 5 || Buggy == 3 { ab } else { 0 };
            }

            invariant NoMintFromUnsettled: mab <= 1 || mab == 3 || mt == 1;
            invariant OneMintWhenFast: fast == 0 || mb + cc == 0;
            invariant AtMostTwoMints: mb + cc <= 1;
            invariant HoldEnds: past == 0 || mn == 1 || (sc == 1 && ab == 3);
            bound EpochConvergenceBox:
                sc <= 4 && t <= 3 && ab <= 3 && fast <= 1 && mb <= 1 && mn <= 1 && cc <= 1 &&
                healed <= 1 && past <= 1 && mab <= 3 && mt <= 1 && head <= 2 && rd <= 2;
        }
    }
}
