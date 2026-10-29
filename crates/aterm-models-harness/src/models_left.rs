// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE LEFT IN THE COMPOSER (`docs/DESIGN-harness-occupancy-2026-10-20.md`; the slice-2 plan's
//! 2.9b), the same at every door: the hold door's line, the stuck line, a wake's line at a break,
//! and the facts riding a point's or a turn end's act.
//!
//! The model (`LeftWrite`): one episode of one fact or wake, one composer, one loop. A write may
//! type and submit its line (`Types`, the door's own, `OccupancyDoor`'s `Type`), or write it and
//! find it not submitted (`Leaves`): then the line is LEFT, neither told nor untold, and no door
//! writes the episode again while it stands. The person sends it or clears it; the policy's draft
//! step sends it, under the owner's `prompt` alone (`DraftSends`); the session may end. The loop's
//! next look settles it: the session's submit count rose since, told (`LookTold`, the row naming
//! the Enter); it did not and the composer holds no draft, released (`LookReleased`, a wake's
//! claim given back); the session gone, released (`GoneReleased`). `reached` counts the lines of
//! the episode the agent took and `booked` the told rows: no episode reaches the agent twice, and
//! once its line settles, each line that reached it is booked told and none that did not. Both
//! counts stop at 2, the first count a law refuses, so a mutant's loop stays a small graph.
//!
//! Before 2.9b every door booked a left line untold at once (`ReleaseOnLeft`): the draft it left
//! went out with no told row, and a later write told the episode again. `sent` is the session's
//! submit count risen since the line was left or, on a host that serves no count, a turn seen
//! running since with the composer clear; the Tier-1 bind walks both
//! (`tests/conformance_left.rs`).
//!
//! `known` is whether the look may weigh that count at all. Each host process counts its own
//! submits from 0, so a successor loop that reads the line back from its predecessor's row, or a
//! loop whose host came back from an outage, holds a count no look may weigh (`Recount`). Its
//! look at a clear composer cannot say whether the person sent the line or cleared it, so it
//! books it told (`LookUncounted`): a line that went is never typed again, and one cleared unsent
//! is a tell lost, which the ghost `lost` records and `BookedAsTold` allows. Released there
//! (`ReleaseUncounted`), a line the person sent is typed a second time.

use aterm_spec::derive::{Liveness, Model};

/// THE LEFT LINE.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn left_write_model() -> Model {
    aterm_spec::ty_model! {
        LeftWrite {
            const Buggy = 0;
            var st = 0;
            var comp = 0;
            var sent = 0;
            var pr = 1;
            var gone = 0;
            var reached = 0;
            var booked = 0;
            var obs = 0;
            var known = 1;
            var lost = 0;

            action Types when (st == 0 && comp == 0 && pr == 1 && gone == 0) {
                st = 2;
                reached = if (2 > reached) { reached + 1 } else { reached };
                booked = if (2 > booked) { booked + 1 } else { booked };
            }
            action Leaves when (st == 0 && comp == 0 && pr == 1 && gone == 0) {
                st = 1;
                comp = 1;
                sent = 0;
                known = 1;
            }
            action PersonSends when (comp == 1 && gone == 0) {
                comp = 0;
                reached = if (2 > reached) { reached + 1 } else { reached };
                sent = 1;
            }
            action PersonClears when (comp == 1 && gone == 0) { comp = 0; }
            action DraftSends when (comp == 1 && pr == 1 && gone == 0) {
                booked = if (st == 1 && 2 > booked) { booked + 1 } else { booked };
                st = if (st == 1) { 2 } else { st };
                comp = 0;
                reached = if (2 > reached) { reached + 1 } else { reached };
            }
            action LookTold when (st == 1 && comp == 0 && sent == 1 && known == 1 && gone == 0) {
                st = 2;
                booked = if (2 > booked) { booked + 1 } else { booked };
                sent = 0;
            }
            action LookReleased when (
                st == 1 && comp == 0 && sent == 0 && known == 1 && gone == 0
            ) {
                st = 0;
            }
            action Recount when (st == 1 && known == 1 && gone == 0) { known = 0; }
            action LookUncounted when (st == 1 && comp == 0 && known == 0 && gone == 0) {
                st = 2;
                booked = if (2 > booked) { booked + 1 } else { booked };
                lost = if (sent == 0) { 1 } else { lost };
                sent = 0;
            }
            action GoneReleased when (st == 1 && gone == 1) {
                st = 0;
                sent = 0;
            }
            action Withdraw when (pr == 1) { pr = 0; }
            action Regrant when (pr == 0) { pr = 1; }
            action Ends when (gone == 0) {
                gone = 1;
                comp = 0;
            }

            action KeepOnClear when (
                Buggy == 1 && st == 1 && comp == 0 && sent == 0 && gone == 0
            ) {
                st = 3;
            }
            action ReleaseOnLeft when (
                Buggy == 2 && st == 0 && comp == 0 && pr == 1 && gone == 0
            ) {
                comp = 1;
                sent = 0;
            }
            action SubmitWithoutPrompt when (Buggy == 3 && comp == 1 && pr == 0 && gone == 0) {
                comp = 0;
                reached = if (2 > reached) { reached + 1 } else { reached };
                sent = 1;
                obs = 1;
            }
            action ToldOnClear when (
                Buggy == 4 && st == 1 && comp == 0 && sent == 0 && gone == 0
            ) {
                st = 2;
                booked = if (2 > booked) { booked + 1 } else { booked };
            }
            action ReleaseUncounted when (
                Buggy == 5 && st == 1 && comp == 0 && known == 0 && gone == 0
            ) {
                st = 0;
                sent = 0;
            }

            invariant ToldOnce: reached <= 1;
            invariant AClearedLineGivesItsClaimBack: 3 > st;
            invariant NothingSentWithoutPrompt: obs == 0;
            invariant BookedAsTold: st == 1 || gone == 1 || booked == reached + lost;
        }
    }
}

/// `ALeftLineSettles`, `[]<>(st == 0 || st == 2 || comp == 1)`: a line left whose composer is
/// clear (sent or cleared) is settled at a look, told or released (told where its count may not be
/// weighed), and so is one whose session ended.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(left_write_model)]
pub fn left_write_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "ALeftLineSettles",
        goal: or_(
            or_(eq(var("st"), int(0)), eq(var("st"), int(2))),
            eq(var("comp"), int(1)),
        ),
        weak: vec!["LookTold", "LookReleased", "GoneReleased", "LookUncounted"],
        strong: vec![],
        mutants: vec!["KeepOnClear"],
    }
}
