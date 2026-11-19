// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release pump's one re-base of a line it must re-qualify anyway (UB-15,
//! `crates/aterm-release/src/pump.rs`, `Verb::Rebase`).
//!
//! A hop and a re-base cost the same re-qualification, but a hop leaves out every fix landed
//! after the line's base. So where this clone's line must re-qualify anyway (a run of it went
//! red, or a repair worker's fix was accepted), the pump runs `--prepare --commit <green>
//! --supersede` once, onto the newest judged pass that descends from the line's head, and
//! every later red goes back to hops. Re-basing at most once, and only onto a judged pass,
//! keeps the livelock that "never supersede" guarded against (a line re-based onto each new
//! main, re-qualifying for good) out of reach. The pump's re-base takes the line's claim only
//! from a holder a contender could take it from (a session's `--supersede` takes it whatever its
//! holder does), and a run beside a live holder can only end beside it, so the pump re-bases (and
//! hops) only while no other process holds the line live. A fleet line another holder moved (a hop
//! it carried before it left) is not this clone's to retire: no re-base is due there, and a fix is
//! carried as a hop onto the line where it stands. A re-base the release refuses still
//! spends the line's one re-base in this clone, and a line stopped by a refusal that is no red (a
//! notary's, a signing step's, with an older pump's repair worker on it) need not re-qualify, so
//! no re-base is due there and its fix is carried as a hop.
//!
//! # Variables
//! - `phase`: the line. 0 qualifying (its runs go on), 1 red (a run of it ended red), 2 fixed
//!   (a repair worker named a fix the pump accepts), 3 released.
//! - `green`: 1 while a judged pass descends from the line's head and may take it (it declares
//!   the line's version, holds the fix where one is named and carries commits past the head in
//!   the app's cone): `Seen::green` with `Seen::onto` passing.
//! - `rebased`: what the pump reads of the line's one re-base: main's supersession record of its
//!   version, or the pump's memory of a re-base it ran (`Journal::rebased`, `Memory::rebased`).
//! - `rebases`, `unjudged`: history ghosts, the re-bases made and whether one went onto a commit
//!   that was no judged pass. No committed action but `Rebase` moves `rebases`.
//! - `hops`: the hops carried, within `MaxHops`.
//! - `held`: 1 while another process holds the line live: its fleet claim keeps a contender off
//!   and it is no run of this clone (`Main::held`).
//! - `displaced`: history ghost, whether a re-base ran while `held` was 1.
//! - `nored`: 1 while the line is stopped by a refusal that is no red, its repair worker one an
//!   older pump kept (pump.rs `red_worker`'s `requalifies` is false): no re-base is due.
//! - `moved`: 1 while the fleet line is not at this clone's journal's head: another holder moved
//!   it (a hop) and left, or this clone's own hop is recorded nowhere the claim reads yet
//!   (`Main::line` is not `Journal::head`): no re-base is due.
//!
//! # Actions
//! `Judge` (the environment: the follower judges a commit past the head a pass), `Take` and
//! `Leave` (the environment: another process takes the line live, and its lease ends unrenewed or
//! its Mac proves it dead), `Move` (the environment: another holder moves the fleet line off this
//! clone's head and leaves it, while the line re-qualifies), `GoRed`, `Refuse` (a run ends
//! refused, no red, with an older pump's worker on the head), `AcceptFix`, `Rebase` (only at a
//! re-qualify, onto a judged pass, once, beside no live holder, with the fleet line at this
//! clone's head), `RebaseRefused` (the same re-base, refused by the release before it records
//! anything: the line's re-base is spent and the line stays as it was), `Hop` (a fix carried where
//! no re-base is due, beside no live holder, onto the fleet line where it stands), `Release`,
//! `Done`.
//!
//! # Mutants
//! `Buggy == 1` enables `RebaseTwice` (a line re-based again at a later red), which breaks
//! `AtMostOneRebasePerLine`; `Buggy == 2` enables `RebaseOntoUnjudged` (a re-base onto a commit
//! no judged pass vouches for, as onto main's moving tip), which breaks
//! `RebaseOnlyOntoAJudgedPass`; `Buggy == 3` enables `RebaseBesideAHolder` (a re-base that takes
//! the line from a live holder in the middle of its run), which breaks
//! `NoRebaseBesideALiveHolder`.

use aterm_spec::derive::*;

/// The pump's one re-base of a line at a re-qualify.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_pump_rebase_model() -> Model {
    aterm_spec::ty_model! {
        ReleasePumpRebase {
            const Buggy = 0;
            const MaxHops = 2;
            var phase = 0;
            var green = 0;
            var rebased = 0;
            var rebases = 0;
            var unjudged = 0;
            var hops = 0;
            var held = 0;
            var displaced = 0;
            var nored = 0;
            var moved = 0;

            action Judge when (3 > phase && green == 0) {
                green = 1;
            }
            action Take when (3 > phase && held == 0) {
                held = 1;
            }
            action Leave when (held == 1) {
                held = 0;
            }
            action Move when ((phase == 1 || phase == 2) && held == 0 && moved == 0) {
                moved = 1;
            }
            action GoRed when (phase == 0) {
                phase = 1;
                nored = 0;
            }
            action Refuse when (phase == 0) {
                phase = 1;
                nored = 1;
            }
            action AcceptFix when (phase == 1) {
                phase = 2;
            }
            action Rebase when (
                (phase == 1 || phase == 2) && rebased == 0 && green == 1 && held == 0 && nored == 0
                    && moved == 0
            ) {
                phase = 0;
                rebased = 1;
                rebases = rebases + 1;
                green = 0;
            }
            action RebaseRefused when (
                (phase == 1 || phase == 2) && rebased == 0 && green == 1 && held == 0 && nored == 0
                    && moved == 0
            ) {
                rebased = 1;
                rebases = rebases + 1;
            }
            action Hop when (
                phase == 2 && MaxHops > hops
                    && (rebased == 1 || green == 0 || nored == 1 || moved == 1) && held == 0
            ) {
                phase = 0;
                hops = hops + 1;
                green = 0;
                nored = 0;
                moved = 0;
            }
            action Release when (phase == 0) {
                phase = 3;
            }
            action Done when (phase == 3) {
                phase = phase;
            }
            action RebaseTwice when (
                Buggy == 1 && (phase == 1 || phase == 2) && rebased == 1 && green == 1 && 2 > rebases
            ) {
                phase = 0;
                rebases = rebases + 1;
                green = 0;
            }
            action RebaseOntoUnjudged when (
                Buggy == 2 && (phase == 1 || phase == 2) && rebased == 0 && green == 0
            ) {
                phase = 0;
                rebased = 1;
                rebases = rebases + 1;
                unjudged = 1;
            }

            action RebaseBesideAHolder when (
                Buggy == 3 && (phase == 1 || phase == 2) && rebased == 0 && green == 1 && held == 1
            ) {
                phase = 0;
                rebased = 1;
                rebases = rebases + 1;
                green = 0;
                displaced = 1;
            }

            invariant AtMostOneRebasePerLine: rebases <= 1;
            invariant RebaseOnlyOntoAJudgedPass: unjudged == 0;
            invariant NoRebaseBesideALiveHolder: displaced == 0;
            bound HopsWithinCap: hops <= MaxHops;
        }
    }
}
