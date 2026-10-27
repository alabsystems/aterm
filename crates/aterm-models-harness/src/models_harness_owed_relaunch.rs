// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The relaunch a harness restart owes: an agent aterm ended itself comes back by aterm's own
//! line or is closed as someone else's resume, its line is never held by keys its end went past
//! nor by a shell the person moved, it resumes the conversation where it ran, and nothing is
//! relaunched for an end aterm did not claim.

use aterm_spec::derive::Model;

/// ONE conversation's agent in ONE tab across the ends aterm makes (`aterm-agent`'s
/// `harness::relaunch::line_held_by_status`, `relaunch::restarted`, and the upgrade drive's visit
/// of an in-flight record).
///
/// `keys` is the person's last key: `0` none within the whole grace, `1` within it but past the
/// grace the gate that let aterm's end go waited out (the line's own, `relaunch::line_grace`), `2`
/// within that; `after` says it came after aterm's end. `End` waits out that grace, so a key within
/// it never predates the end.
/// `launcher` is who brought the conversation back (`1` aterm's line, `2` someone else), `said` the
/// record's word for it (`1` aterm relaunched it, `2` someone else resumed it, `3` it went stale
/// with its line held), `stale_by` the `after` of the key that held a stale line.
/// `moved` says the shell stands in another directory than the line was planned at (a `cd` the
/// person typed at the prompt, `Cd`); the line then carries a `cd` back (`relaunch::line_at_shell`),
/// and `strayed` says a line resumed the conversation anywhere else.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_owed_relaunch_model() -> Model {
    aterm_spec::ty_model! {
        HarnessOwedRelaunch {
            const Buggy = 0;
            var running = 1;
            var launches = 1;
            var claimed = 0;
            var owed = 0;
            var keys = 0;
            var after = 0;
            var typing = 0;
            var launcher = 0;
            var said = 0;
            var stale_by = 0;
            var lines = 0;
            var at_typing = 0;
            var moved = 0;
            var strayed = 0;

            action Key when (keys <= 1) {
                keys = 2;
                after = if (running == 0 && owed == 1) { 1 } else { 0 };
            }
            action KeysAge when (keys == 2) {
                keys = 1;
            }
            action KeysAway when (keys == 1) {
                keys = 0;
                after = 0;
            }
            action End when (running == 1 && owed == 0 && keys <= 1 && typing == 0) {
                running = 0;
                launches = 0;
                claimed = 1;
                owed = 1;
                after = 0;
                launcher = 0;
                said = 0;
                stale_by = 0;
                lines = 0;
                at_typing = 0;
                moved = 0;
                strayed = 0;
            }
            action PersonExit when (running == 1 && owed == 0) {
                running = 0;
                launches = 0;
                claimed = 0;
                launcher = 0;
                said = 0;
                lines = 0;
            }
            action Type when (running == 0 && typing == 0) {
                typing = 1;
                keys = 2;
                after = owed;
            }
            action Clear when (typing == 1) {
                typing = 0;
            }
            action Cd when (running == 0 && typing == 1) {
                typing = 0;
                moved = 1 - moved;
            }
            action Line when (
                running == 0 && owed == 1 && typing == 0 && (keys <= 1 || after == 0)
            ) {
                running = 1;
                launches = launches + 1;
                launcher = 1;
                lines = lines + 1;
                at_typing = typing;
            }
            action Adopt when (running == 1 && owed == 1 && launcher == 1) {
                owed = 0;
                said = 1;
            }
            action PersonLaunch when (running == 0 && typing == 0) {
                running = 1;
                launches = launches + 1;
                launcher = 2;
            }
            action Close when (running == 1 && owed == 1 && launcher == 2) {
                owed = 0;
                said = 2;
            }
            action Stale when (running == 0 && owed == 1 && keys == 2 && after == 1) {
                owed = 0;
                said = 3;
                stale_by = after;
            }
            action HeldByOldKeys when (
                Buggy == 1 && running == 0 && owed == 1 && keys > 0 && after == 0
            ) {
                owed = 0;
                said = 3;
                stale_by = after;
            }
            action AdoptUntyped when (Buggy == 1 && running == 1 && owed == 1 && launcher == 2) {
                owed = 0;
                said = 1;
            }
            action LineOverAHolder when (
                Buggy == 1 && running == 1 && owed == 1 && launcher == 2 && lines == 0
            ) {
                launches = launches + 1;
                lines = 1;
            }
            action RelaunchAPersonsExit when (
                Buggy == 1 && running == 0 && owed == 0 && claimed == 0 && typing == 0
            ) {
                running = 1;
                launches = launches + 1;
                launcher = 1;
                lines = 1;
            }
            action LineOverTyping when (Buggy == 1 && running == 0 && owed == 1 && typing == 1) {
                running = 1;
                launches = launches + 1;
                launcher = 1;
                lines = lines + 1;
                at_typing = typing;
            }
            action LineWhereTheShellStands when (
                Buggy == 1 && running == 0 && owed == 1 && typing == 0 && moved == 1
                    && (keys <= 1 || after == 0)
            ) {
                running = 1;
                launches = launches + 1;
                launcher = 1;
                lines = lines + 1;
                strayed = 1;
            }
            action StaleWhileMoved when (
                Buggy == 1 && running == 0 && owed == 1 && moved == 1 && keys <= 1
            ) {
                owed = 0;
                said = 3;
                stale_by = 0;
            }

            invariant AnEndAtermMadeIsRelaunchedOrAdopted:
                claimed == 0 || owed == 1 || said == 1 || said == 2 || stale_by == 1;
            invariant TheRecordSaysWhoRelaunched: said <= 0 || said > 1 || launcher == 1;
            invariant OneAgentHoldsTheConversation: launches <= 1;
            invariant OnlyAnEndAtermClaimedIsRelaunched: lines == 0 || claimed == 1;
            invariant NeverOverAPersonTyping: at_typing == 0;
            invariant ResumesWhereItRan: strayed == 0;
        }
    }
}
