// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unsaved Settings draft rides a seamless self-update (2026-09-27, the
//! update-robustness plan P2-2): the text a person typed into a Settings field
//! and did not save is CARRIED through the handoff instead of holding the
//! update until they save or discard it. Before this date any Settings view
//! with such a draft was an unconditional preflight blocker ("Review Settings
//! Drafts"), re-probed every few minutes, and one forgotten field kept every
//! automatic update waiting for as long as the tab stayed open.
//!
//! The carrier is the handoff layout itself: the park's capture puts each
//! Settings view's drafts on its leaf (`aterm-gui/src/app_restore.rs`
//! `App::capture_handoff_layout`), the successor reopens the view and takes
//! them (`App::reopen_carried_settings_drafts`), and the Commit re-captures
//! with the same function and compares Settings leaves in full
//! (`app_update_handoff::commit_layout_topology`).

use super::Model;

/// One Settings view through one update attempt: the outgoing view's draft,
/// the draft the parked layout carries, and what the successor's view holds.
///
/// State: `draft` — the outgoing view's draft version (0 is no draft; each
/// keystroke makes a new version). `phase` — 0 no attempt, 1 the preflight
/// admitted the attempt and the lane is not chosen yet, 2 parked (the layout
/// is captured and the successor is restoring), 3 replaced (the outgoing
/// process is gone). `carried` — the draft version the parked layout carries.
/// `older` — the successor is an OLDER build, which parses the layout but
/// ignores the field that carries drafts. `restored`/`succ` — the successor
/// reopened the view, and the draft its view holds (`Surfaced`: it could not
/// reopen the carried draft and said so, with the text, in the message center).
///
/// The environment's moves are the person's (`Type` — typing continues while
/// the successor boots, until the Commit), the lane choice (`Park` to a
/// successor at least this build, `ParkOlder` to an older one, `ColdExec` —
/// no terminal is open, so the app is exec'd with no layout at all), and the
/// successor's (`Restore`, `RestoreUnreadable` — the carry did not survive to
/// a view that could take it). The App's are `Start` (the preflight),
/// `StandDown`, `Commit` and `Rollback`.
///
/// Invariant: `NoDraftLost` — once the outgoing process is replaced, the
/// successor's view holds exactly the outgoing draft, or the successor said
/// out loud which draft it could not reopen.
///
/// `Buggy = 1` commits without comparing the Commit-time capture to the
/// parked one (the person typed while the successor booted: it reopens the
/// older draft). `Buggy = 2` lets the cold lane spend a token that carries a
/// draft (the exec reopens nothing). `Buggy = 3` hands a draft to an older
/// successor (it reopens the view without it). Each loses the draft.
///
/// Tier-1 (`aterm-gui/src/settings_draft_carry_conformance.rs`) drives a real
/// outgoing `App` and a real successor `App` through the real preflight, the
/// real handoff capture serialized to the real layout wire and parsed back,
/// the successor's real restore, the real Commit comparison and the real lane
/// gate; it projects every variable at every step, requires the real verdicts
/// to be the model's guards, and replays the `Buggy` machines as negative
/// controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_settings_draft_carry_model() -> Model {
    crate::ty_model! {
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
