// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of F21's coverage law: an honest write sealed under an epoch a removed Mac minted
//! past its cutoff survives the heal, and nothing re-puts over it blind
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F21).

use aterm_spec::derive::*;

/// `KeyedEpochs`: the heal of a split in which R (removed by A on side 1) minted an epoch E on side
/// 2 and Y put `S` = v2 under it. A holds E through R's `Wrap`; X, let in by A on side 1, holds no
/// `Wrap` of E; W, let in by Y on side 2, wrote under E and lacks A's side-1 epoch (`gap == 1`).
///
/// `win` is the value of `S` the holders read (2 for Y's v2, 3 for a later edit), `sx` the value X
/// reads (1 for v1), `hx` whether X hides an audience it cannot open whole (F21 rule 7), `covered`
/// whether a full reseal covers E (rule 4), and `t` whether R's own write `T` reads anywhere.
///
/// `Heal` exchanges the records: E becomes keyed and X hides `[everyone]`; before it X knows no
/// epoch it cannot open, so it hides nothing. `CloseGap` wraps A's epoch to W. `PartialMint` is
/// W's publish while it lacks A's epoch: a sealed-away epoch holding only its change, with every
/// dependent of E in its past. `Reseal` is a holder's reseal from the counting view (rule 5): its
/// epoch is wrapped to every member, W among them, and carries every winner, so W lacks no epoch
/// it reads either. `Edit` is a person's edit of `S` under an epoch X holds. `BlindReput` is X's
/// full re-put of what it reads (a removal's rotation, a coalesce, a fresh epoch): the code runs
/// it only while X hides nothing.
///
/// The mutants: `Buggy == 1` lets a partial mint claim coverage (rule 4 with neither clause: the
/// partial mint lacks a newest epoch and attests nothing, so dropping one clause alone still
/// leaves it uncovered), `Buggy == 2` drops rule 7 (X never hides), and `Buggy == 3` reseals from
/// a document, which re-signs R's `T`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keyed_epochs_model() -> Model {
    aterm_spec::ty_model! {
        KeyedEpochs {
            const Buggy = 0;

            var healed = 0;
            var gap = 1;
            var partial = 0;
            var covered = 0;
            var edited = 0;
            var win = 2;
            var sx = 1;
            var hx = 0;
            var t = 0;

            action Heal when (healed == 0) {
                healed = 1;
                hx = if Buggy == 2 { 0 } else { 1 };
            }
            action CloseGap when (healed == 1 && gap == 1) {
                gap = 0;
            }
            action PartialMint when (healed == 1 && gap == 1 && partial == 0) {
                partial = 1;
                covered = if Buggy == 1 { 1 } else { covered };
                hx = if Buggy == 1 { 0 } else { hx };
            }
            action Reseal when (healed == 1 && covered == 0) {
                covered = 1;
                hx = 0;
                sx = win;
                gap = 0;
                t = if Buggy == 3 { 1 } else { t };
            }
            action Edit when (healed == 1 && edited == 0) {
                edited = 1;
                win = 3;
                sx = 3;
            }
            action BlindReput when (healed == 1 && hx == 0) {
                win = sx;
            }

            invariant HonestWriteOnACutEpochStands: healed == 0 || edited == 1 || win == 2;
            invariant ReadersAgree: healed == 0 || hx == 1 || sx == win;
            invariant RemovedWriteNeverReturns: t == 0;
            bound CoverageBox: win <= 3 && sx <= 3;
        }
    }
}
