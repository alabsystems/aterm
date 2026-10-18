// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unsaved Settings draft rides a seamless self-update: text typed into a Settings field and
//! not saved is carried through the handoff rather than holding the update until it is saved or
//! discarded.
//!
//! The carrier is the handoff layout: the park's capture puts each Settings view's drafts on its
//! leaf (`aterm-gui/src/app_restore.rs` `App::capture_handoff_layout`), the successor reopens the
//! view and takes them (`App::reopen_carried_settings_drafts`), and the Commit re-captures with
//! the same function and compares Settings leaves in full
//! (`app_update_handoff::commit_layout_topology`).

use aterm_spec::derive::Model;

/// One Settings view through one update attempt: the outgoing view's draft, the draft the
/// parked layout carries, and what the successor's view holds.
///
/// State: `draft` — the outgoing view's draft version (0 is none; each keystroke makes a new
/// one). `phase` — 0 no attempt, 1 the preflight admitted the attempt and no lane is chosen, 2
/// parked (layout captured, successor restoring), 3 replaced (the outgoing process is gone).
/// `carried` — the draft version the parked layout carries. `older` — the successor is an older
/// build, which parses the layout but ignores the drafts field. `restored`/`succ` — the
/// successor reopened the view, and the draft it holds (`Surfaced`: it could not reopen the
/// carried draft and says so, with the text, in the message center; that happens at the Commit,
/// since before it the parent still holds the draft and a stood-down successor writes nothing).
///
/// The environment's moves: the person's `Type` (typing continues while the successor boots,
/// until the Commit); the lane choice (`Park` to a successor at least this build, `ParkOlder` to
/// an older one, `ColdExec` with no terminal open, so the app is exec'd with no layout); the
/// successor's `Restore` and `RestoreUnreadable` (the carry did not reach a view that could
/// take it). The App's are `Start` (the preflight), `StandDown`, `Commit` and `Rollback`.
///
/// `NoDraftLost`: once the outgoing process is replaced, the successor's view holds exactly the
/// outgoing draft, or the successor said which draft it could not reopen.
///
/// Each `Buggy` loses the draft: `1` commits without comparing the Commit-time capture to the
/// parked one (typing during boot leaves the successor an older draft); `2` lets the cold lane
/// spend a token that carries a draft (the exec reopens nothing); `3` hands a draft to an older
/// successor (it reopens the view without it).
///
/// Tier-1 (`aterm-gui/src/settings_draft_carry_conformance.rs`) drives a real outgoing and a
/// real successor `App` through the preflight, the handoff capture serialized to the layout wire
/// and parsed back, the restore, the Commit comparison and the lane gate; it projects every
/// variable at every step, requires the real verdicts to be the model's guards, and replays the
/// `Buggy` machines as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_settings_draft_carry_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateSettingsDraftCarry {
            const Buggy = 0;
            const MaxEdits = 2;
            const Surfaced = 9;
            var draft = 0;
            var phase = 0;
            var carried = 0;
            var older = 0;
            var restored = 0;
            var succ = 0;

            action Type when (phase <= 2 && draft <= MaxEdits - 1) {
                draft = draft + 1;
            }
            action Start when (phase == 0) {
                phase = 1;
            }
            action StandDown when (phase == 1) {
                phase = 0;
            }
            action Park when (phase == 1) {
                phase = 2;
                carried = draft;
                older = 0;
            }
            action ParkOlder when (phase == 1 && (draft == 0 || Buggy == 3)) {
                phase = 2;
                carried = draft;
                older = 1;
            }
            action ColdExec when (phase == 1 && (draft == 0 || Buggy == 2)) {
                phase = 3;
                restored = 1;
                succ = 0;
            }
            action Restore when (phase == 2 && restored == 0) {
                restored = 1;
                succ = if older == 1 { 0 } else { carried };
            }
            action RestoreUnreadable when (
                phase == 2 && restored == 0 && older == 0 && carried > 0
            ) {
                restored = 1;
                succ = Surfaced;
            }
            action Commit when (
                phase == 2 && restored == 1 && (carried == draft || Buggy == 1)
            ) {
                phase = 3;
            }
            action Rollback when (phase == 2) {
                phase = 0;
                carried = 0;
                older = 0;
                restored = 0;
                succ = 0;
            }

            invariant NoDraftLost: phase <= 2 || succ == draft || succ == Surfaced;
        }
    }
}
