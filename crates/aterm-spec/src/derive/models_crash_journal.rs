// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CRASH JOURNAL'S CLAIM (P1 of `docs/DESIGN-pty-keeper-2026-09-26.md`,
//! §5.5 and §6.3): who may take a dead window's layout, and when.
//!
//! A windowed aterm with `restore_session` on keeps its layout in a journal
//! beside `session.toml` for as long as it runs, and a clean quit removes it.
//! The next launch that finds a journal whose owner is gone and whose owner's
//! end was not clean (its crash marker says killed, signal or panic) reopens
//! that layout. Several launches may look at once, and the owner may be alive,
//! may have quit, may have handed its sessions to an update, may have died
//! while rewriting, or may itself have been a launch that reopened a journal
//! and died again within 90 s — crashed, or stopped (killed) once, or stopped
//! again after the brake let a stopped one through (ruling 285).

use super::Model;

/// One journal and every launch that comes after its owner
/// (`aterm-gui/src/crash_journal.rs`: `JournalOwner::create`/`publish`/
/// `retire` on the owner's side, `claim_at_boot` on the reader's).
///
/// State: the owner — `booted`, `live`, `clean` (it ended through the quit's
/// removal or the update's hand-off exit, whose crash marker goes with it),
/// `probation` (it reopened a crash journal at boot and has not run 90 s
/// since), `second` (the journal it reopened was a stopped probation
/// writer's, which the brake let through once), `crashed` (its unclean end
/// left crash evidence — a signal banner or a panic report — not the empty
/// marker of a kill). Its image — `image` (a complete image is under the
/// journal's public name), `image_probation` (that image carries the
/// probation mark), `image_second` (and the second-chance mark),
/// `torn` (a rewrite is between its unlink and its publish — only a
/// non-atomic writer has such a state), `written` (a complete image was
/// published at least once). The launches after it — `claims` (how many took
/// the image), `live_claim` (one took it while the owner lived), `applied`
/// (one reopened it), `lost` (the owner died uncleanly with no image left
/// although it had published one).
///
/// The owner's moves: `BootFresh` / `BootFromJournal` / `BootSecondChance`
/// (its own launch reopened nothing, reopened a crash journal, or reopened a
/// stopped probation writer's), `Write` (a changed layout, published by temp +
/// rename), `Settle` (90 s after a journal was reopened: the image is
/// rewritten without the marks), `Quit` (the clean quit removes the image),
/// `HandOff` (the update parent's `_exit` after Commit: clean, the image left
/// behind), `Die` (a SIGKILL, a force quit, the system: the empty marker) and
/// `Crash` (a fatal signal or a panic). `Claim` is any later launch's: it
/// takes the image by renaming it (a second launch finds nothing under the
/// name) and reopens it only when the end was unclean and the brake lets it
/// through — the image is not on probation, or it is and its owner was
/// stopped (not crashed) for the first time in a row.
///
/// Invariants: `SingleUse` — an image is taken once, however many launches
/// race for it. `LiveOwnerNeverClaimed` — a running window's journal is never
/// taken. `CleanEndNeverRestored` — a clean end is never reopened as a crash
/// (aterm reopening after Cmd-Q, or the update's parent layout reopened beside
/// its successor). `NoCrashLoop` — an image written by a launch that reopened
/// a journal and CRASHED within 90 s is never reopened. `NoLoop` — nor is one
/// written by a second chance that died within them however it died: a
/// layout that stops aterm cannot loop, and one merely killed comes back at
/// most once more. `NoLoss` — an owner that published an image and then died
/// uncleanly always leaves a complete image under the name.
///
/// `Buggy = 1` is six defects, each caught alone: `Claim` taking a journal
/// whose owner's lock is held (`LiveOwnerNeverClaimed`) by reading and copying
/// it rather than renaming it (`SingleUse`), and reopening whatever it took,
/// the crash marker and the probation marks unread (`CleanEndNeverRestored`,
/// `NoCrashLoop`, `NoLoop`); and `Write` unlinking the old image before the
/// new one is in place (`NoLoss`), which `FinishWrite` completes.
///
/// Tier-1 (`aterm-gui/src/crash_journal_conformance.rs`) replays every
/// schedule of this machine on the shipping owner and claim, against a real
/// directory, a real lock and real crash markers, projects every variable at
/// every step, and replays the `Buggy = 1` machine as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn crash_journal_claim_model() -> Model {
    crate::ty_model! {
        CrashJournalClaim {
            const Buggy = 0;
            const MaxClaims = 2;
            var booted = 0;
            var live = 1;
            var clean = 0;
            var probation = 0;
            var second = 0;
            var crashed = 0;
            var image = 0;
            var image_probation = 0;
            var image_second = 0;
            var torn = 0;
            var written = 0;
            var claims = 0;
            var live_claim = 0;
            var applied = 0;
            var lost = 0;

            action BootFresh when (booted == 0) {
                booted = 1;
            }
            action BootFromJournal when (booted == 0) {
                booted = 1;
                probation = 1;
            }
            action BootSecondChance when (booted == 0) {
                booted = 1;
                probation = 1;
                second = 1;
            }
            action Write when (booted == 1 && live == 1 && torn == 0) {
                image = if Buggy == 1 { 0 } else { 1 };
                torn = if Buggy == 1 { 1 } else { 0 };
                image_probation = probation;
                image_second = second;
                written = if Buggy == 1 { written } else { 1 };
            }
            action FinishWrite when (live == 1 && torn == 1) {
                image = 1;
                torn = 0;
                written = 1;
            }
            action Settle when (live == 1 && probation == 1 && torn == 0) {
                probation = 0;
                second = 0;
                image_probation = 0;
                image_second = 0;
            }
            action Quit when (booted == 1 && live == 1) {
                live = 0;
                clean = 1;
                image = 0;
                torn = 0;
            }
            action HandOff when (booted == 1 && live == 1) {
                live = 0;
                clean = 1;
            }
            action Die when (booted == 1 && live == 1) {
                live = 0;
                lost = if written == 1 && image == 0 { 1 } else { 0 };
            }
            action Crash when (booted == 1 && live == 1) {
                live = 0;
                crashed = 1;
                lost = if written == 1 && image == 0 { 1 } else { 0 };
            }
            action Claim when (
                image == 1 && (live == 0 || Buggy == 1) && claims <= MaxClaims - 1
            ) {
                claims = claims + 1;
                live_claim = if live == 1 { 1 } else { live_claim };
                image = if Buggy == 1 { 1 } else { 0 };
                applied = if (clean == 0 || Buggy == 1)
                    && (image_probation == 0 || (crashed == 0 && image_second == 0) || Buggy == 1)
                {
                    1
                } else {
                    applied
                };
            }

            invariant SingleUse: claims <= 1;
            invariant LiveOwnerNeverClaimed: live_claim == 0;
            invariant CleanEndNeverRestored: applied == 0 || clean == 0;
            invariant NoCrashLoop: applied == 0 || image_probation == 0 || crashed == 0;
            invariant NoLoop: applied == 0 || image_second == 0;
            invariant NoLoss: lost == 0;
        }
    }
}
