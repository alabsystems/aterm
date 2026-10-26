// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ask's hold on an episode before the disk fills (`disk-free@v1` revision 2,
//! `docs/DESIGN-disk-free-ask-2026-10-24.md` §8.3 and §8.7).

use aterm_spec::derive::Model;

/// THE HOLD (revision 2): one volume's open episode, read by two hosts whose plain floors
/// differ. P's floor is the lower, and P's fill ring projects a crossing of it, then stops
/// projecting one; Q's floor is the higher, and Q projects nothing, so Q reads the volume short or
/// not by its plain floor alone. Each host's look that steps the open episode without ending it
/// holds the episode to its own plain floor unless the lease holds another live host's at least
/// as high (`Entry::floor`), and P's projecting look records its projection (`Entry::projected`)
/// in a record of its own, never in that floor. Both hosts live, and aterm's own freeing is done,
/// so a pressed look with no episode open opens one.
/// - `free`: below P's floor (0); past it by the margin an episode ends past and below Q's (1);
///   past Q's by that margin (2);
/// - `ep`: an episode is open;
/// - `bar`: the floor the lease holds the episode to: none (0), P's plain floor (1), Q's (2);
/// - `rec`: P's projection record stands (`Projected::stands`): refreshed within `COLD_HOLD_MS`
///   by a projecting look of P's. P's warm look that projects nothing drops it, and so does that
///   much time with no projecting look of P's (`Lapse`).
///
/// P's look that projects a crossing is pressed whatever free space reads (`OpenProjected`,
/// `Project`); P's look that projects nothing (its projection lapsed) is pressed only below P's
/// floor (`OpenShortP`, `HoldP`) and otherwise ends the episode once free space clears the floor
/// the episode is held to, P's own or Q's, by the margin (`EndP`). Q's look is pressed below Q's
/// floor (`OpenShortQ`, `HoldQ`) and otherwise ends the episode past Q's floor by the margin,
/// unless P's record stands (`EndQ`, else `HoldQ`).
///
/// The rest record what an end saw, for the laws to judge: `sshort`, whether an episode ended
/// while a host whose floor the lease held read the volume short; `sproj`, whether one ended
/// while a projection record stood. `EndP` and `EndQ` record from their own state what their
/// guards are meant to hold, and each mutant is the same end under a weaker guard:
/// `EndAtOwnPlain` is P's lapsed look ending the episode on its own plain floor while the lease
/// holds Q's (the single floor slot of the first version, which a projected floor had taken from
/// Q's and which P's plain floor then replaced), and `EndWhileProjected` is Q's look ending it
/// while P's record stands.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_projected_hold_model() -> Model {
    aterm_spec::ty_model! {
        DiskProjectedHold {
            const Buggy = 0;
            var free = 2;
            var ep = 0;
            var bar = 0;
            var rec = 0;
            var sshort = 0;
            var sproj = 0;

            action Fall when (free > 0) { free = free - 1; }
            action Rise when (2 > free) { free = free + 1; }
            action Lapse when (rec == 1) { rec = 0; }
            action OpenProjected when (ep == 0) {
                ep = 1;
                bar = 1;
                rec = 1;
            }
            action OpenShortP when (ep == 0 && free == 0) {
                ep = 1;
                bar = 1;
                rec = 0;
            }
            action OpenShortQ when (ep == 0 && free <= 1) {
                ep = 1;
                bar = 2;
                rec = 0;
            }
            action Project when (ep == 1) { rec = 1; }
            action HoldP when (ep == 1 && (free == 0 || (bar == 2 && free == 1))) { rec = 0; }
            action EndP when (ep == 1 && free > 0 && (bar == 1 || free == 2)) {
                ep = 0;
                bar = 0;
                rec = 0;
                sshort = if (free == 0 || (bar == 2 && free == 1)) { 1 } else { sshort };
            }
            action HoldQ when (ep == 1 && (free <= 1 || rec == 1)) { bar = 2; }
            action EndQ when (ep == 1 && free == 2 && rec == 0) {
                ep = 0;
                bar = 0;
                rec = 0;
                sshort = if (free == 0 || (bar == 2 && free == 1)) { 1 } else { sshort };
                sproj = if (rec == 1) { 1 } else { sproj };
            }

            action EndAtOwnPlain when (Buggy == 1 && ep == 1 && free > 0) {
                ep = 0;
                bar = 0;
                rec = 0;
                sshort = if (free == 0 || (bar == 2 && free == 1)) { 1 } else { sshort };
            }
            action EndWhileProjected when (Buggy == 2 && ep == 1 && free == 2) {
                ep = 0;
                bar = 0;
                rec = 0;
                sshort = if (free == 0 || (bar == 2 && free == 1)) { 1 } else { sshort };
                sproj = if (rec == 1) { 1 } else { sproj };
            }

            invariant NoEndWhileShort: sshort == 0;
            invariant NoEndWhileProjected: sproj == 0;
            bound FreeBox: free <= 2;
            bound BarBox: bar <= 2;
        }
    }
}
