// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Origin-aware clipboard policy — the policy layer that decides whether
//! a clipboard action is allowed, denied, or requires user confirmation,
//! based on *who initiated it*.
//!
//! # Context: Phase 3 of escape-sequence security hardening (#7874)
//!
//! Phase 2 of #7874 landed a family of zero-sized capability tokens
//! (`ClipboardAuth`, `ModalAuth`, `ResponseCapability`, etc.) that give
//! a *compile-time* proof that a PTY-origin handler cannot reach a
//! privileged callback without first passing through the host API. Those
//! tokens answer the question **"is this call authorized at all?"**.
//!
//! Phase 3 answers a separate, finer-grained question:
//! **"given that an authorized caller is invoking the clipboard, what
//! should happen based on the *origin* of the request?"**. User-initiated
//! copy, PTY-origin OSC 52 write, paste-injection, and checkpoint-restore
//! are structurally different, and a production-quality clipboard surface
//! must distinguish them even when all four can, in principle, reach the
//! callback.
//!
//! The master design (`designs/2026-04-18-escape-sequence-security-hardening.md`,
//! Section 7 — "Clipboard write policy with `AskHost` default") anticipates
//! this module: the capability gate is necessary but not sufficient. A
//! clipboard write from `OSC52Write` should not be treated the same as a
//! `UserInitiated` write even if both have a `ClipboardWriteCapability`.
//!
//! # Relationship to [`crate::terminal::clipboard_auth`]
//!
//! `ClipboardAuth` (capability tokens) and [`ClipboardPolicy`] are
//! orthogonal layers:
//!
//! | Layer                       | Question                        | Granularity |
//! |-----------------------------|---------------------------------|-------------|
//! | `ClipboardAuth` (Phase 2)   | Is the caller authorized?       | Binary      |
//! | `ClipboardPolicy` (Phase 3) | What should happen per origin?  | 3-way       |
//!
//! A future integration would:
//!
//! 1. Mint a `ClipboardWriteCapability` (existing Phase 2 gate).
//! 2. Call [`ClipboardPolicy::evaluate`] with the observed origin.
//! 3. Deliver [`ClipboardDecision::Allow`] immediately,
//!    [`ClipboardDecision::Deny`] silently, or
//!    [`ClipboardDecision::ConfirmUser`] via a host callback.
//!
//! # Defaults (fail-closed)
//!
//! - `UserInitiated` — `Allow` (user-driven action).
//! - `CheckpointRestore` — `Allow` (deterministic, no untrusted payload).
//! - `PasteInjection` — `ConfirmUser` (bracketed paste from the pasteboard
//!   can carry shell-injection content; confirm when it targets the
//!   pasteboard programmatically).
//! - `OSC52Read` — `Deny` (matches the `allow_osc52_query = false` default).
//! - `OSC52Write` — `ConfirmUser` (matches the design's "AskHost default"
//!   posture; hosts that wish to silently permit can set it to `Allow`,
//!   hosts that wish to block can set it to `Deny`).
//!
//! # Migration notes (wire-in, follow-up)
//!
//! This module is intentionally **not** wired into `handler_osc.rs` in
//! this commit. Wire-in is tracked as a Phase 3 follow-up and will require:
//!
//! 1. Extend `Terminal` with a `ClipboardPolicy` field initialized to
//!    [`ClipboardPolicy::default`].
//! 2. Add a host-facing API
//!    `Terminal::set_clipboard_policy(ClipboardPolicy)` and its FFI
//!    counterpart.
//! 3. In `handler_osc_52_set` / `handler_osc_52_query`, after the
//!    existing `try_mint_*_capability` check, call
//!    [`ClipboardPolicy::evaluate`] with
//!    [`ClipboardOrigin::OSC52Write`] / [`ClipboardOrigin::OSC52Read`].
//! 4. On [`ClipboardDecision::ConfirmUser`], route through a new
//!    `ClipboardConfirmCallback` (new host callback type, not yet added).
//! 5. On [`ClipboardDecision::Deny`], silently drop (match Phase 2
//!    behaviour for unauthorized paths).
//!
//! # Verification posture
//!
//! This module is pure data (no interior mutability, no unsafe, no FFI)
//! — the policy matrix is a total function over `(Origin, Action)` pairs.
//! That means:
//!
//! - Exhaustive enum matching gives a compile-time proof that adding a
//!   new `ClipboardOrigin` or `ClipboardAction` variant will force every
//!   call site to decide its semantics (no silent default).
//! - The default policy is constructible from `const fn` — fail-closed
//!   posture is available without runtime initialization order hazards.

// ---------------------------------------------------------------------------
// Origin — who initiated the clipboard request.
// ---------------------------------------------------------------------------

/// The origin of a clipboard request, used to drive origin-based policy.
///
/// Every origin represents a *structurally distinct* path through the
/// aterm-core data flow. The Phase 3 trust model holds that these paths
/// must be distinguished at the policy layer even when they reach the
/// same callback.
///
/// See module docs for the default policy applied to each origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipboardOrigin {
    /// The local user performed a deliberate UI action (menu, key binding,
    /// context menu) that triggered the clipboard operation.
    ///
    /// This is the most trusted origin — the user is physically present
    /// and driving the terminal.
    UserInitiated,

    /// A PTY-origin OSC 52 query (`OSC 52 ; <selection> ; ? ST`) asked to
    /// *read* the host clipboard and echo it back through the response
    /// stream.
    ///
    /// Least trusted origin for reads: a malicious program that has any
    /// write access to the PTY can attempt to exfiltrate clipboard
    /// contents through the terminal's reply path.
    OSC52Read,

    /// A PTY-origin OSC 52 set (`OSC 52 ; <selection> ; <base64> ST`)
    /// asked to *write* to the host clipboard.
    ///
    /// Least trusted origin for writes: a malicious `cat readme.txt` can
    /// overwrite the user's pasteboard.
    OSC52Write,

    /// The terminal is injecting characters from the system pasteboard
    /// in response to a user paste gesture, routed through bracketed
    /// paste to the PTY.
    ///
    /// Paste content is user-supplied, but it traverses an attacker-
    /// readable medium (the pasteboard) and may reach programs that
    /// treat the stream as authoritative input.
    PasteInjection,

    /// A crash-recovery checkpoint restore is replaying stored state.
    ///
    /// Checkpoint content is host-generated and was previously
    /// authorized; the restore path should not require re-confirmation.
    CheckpointRestore,
}

impl ClipboardOrigin {
    /// All variants, ordered by declaration — used by tests that need to
    /// exhaustively enumerate origins.
    ///
    /// The order matches the `enum` body and MUST be kept in sync with
    /// it if new origins are added. The compile-time exhaustive match in
    /// `default_decision_for` backstops the sync.
    pub const ALL: [Self; 5] = [
        Self::UserInitiated,
        Self::OSC52Read,
        Self::OSC52Write,
        Self::PasteInjection,
        Self::CheckpointRestore,
    ];
}

// ---------------------------------------------------------------------------
// Action — what the origin is asking the clipboard to do.
// ---------------------------------------------------------------------------

/// The class of clipboard action being evaluated.
///
/// The action granularity matches the `ClipboardOperation` payload in
/// `terminal::types` (`Set` / `Query` / `Clear`), but widened to include
/// a `Paste` variant because paste injection is itself a policy decision
/// even though it is not a `ClipboardOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipboardAction {
    /// Write a value to the clipboard (OSC 52 set, UI copy, etc.).
    Write,
    /// Read the clipboard contents back (OSC 52 query).
    Read,
    /// Inject pasteboard contents into the PTY input stream.
    Paste,
}

impl ClipboardAction {
    /// All variants, ordered by declaration.
    pub const ALL: [Self; 3] = [Self::Write, Self::Read, Self::Paste];
}

// ---------------------------------------------------------------------------
// Decision — the policy verdict.
// ---------------------------------------------------------------------------

/// The policy verdict for a given `(origin, action)` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipboardDecision {
    /// Permit the action without user interaction.
    Allow,
    /// Silently drop the action. No callback, no response, no error.
    ///
    /// The choice to silently deny (rather than reply with an error) is
    /// intentional: informative denial leaks information to a PTY-origin
    /// caller about the host's policy posture. A PTY attacker that can
    /// observe denial knows the pasteboard is protected; a PTY attacker
    /// that sees a silent drop does not learn whether the callback was
    /// invoked.
    Deny,
    /// Route the action through a host-provided user-confirmation hook
    /// before proceeding. If the user declines, the action is treated
    /// like `Deny`. If the user approves, the action is treated like
    /// `Allow`.
    ///
    /// The confirmation surface is not specified here — the host
    /// application chooses the UI (modal dialog, toast with buttons,
    /// inline command, etc.).
    ConfirmUser,
}

// ---------------------------------------------------------------------------
// Default policy table.
// ---------------------------------------------------------------------------

/// The fail-closed default decision for each `(origin, action)` pair.
///
/// This function is `const` and exhaustive — adding a new
/// [`ClipboardOrigin`] or [`ClipboardAction`] variant forces an update
/// here (compile-time error on the outer `match`).
#[must_use]
const fn default_decision_for(
    origin: ClipboardOrigin,
    action: ClipboardAction,
) -> ClipboardDecision {
    use ClipboardAction::{Paste, Read, Write};
    use ClipboardDecision::{Allow, ConfirmUser, Deny};
    use ClipboardOrigin::{
        CheckpointRestore, OSC52Read, OSC52Write, PasteInjection, UserInitiated,
    };

    match (origin, action) {
        // UserInitiated — full trust. The user is physically driving the
        // operation and has already confirmed intent through the UI.
        (UserInitiated, Write | Read | Paste) => Allow,

        // CheckpointRestore — deterministic, host-controlled payload.
        (CheckpointRestore, Write | Read | Paste) => Allow,

        // OSC52Read — deny by default (matches allow_osc52_query = false).
        (OSC52Read, Read) => Deny,
        // OSC52Read asking to Write or Paste is a category error (a read
        // origin asking for a write), but we still classify it: deny.
        (OSC52Read, Write | Paste) => Deny,

        // OSC52Write — AskHost default (see design Section 7).
        (OSC52Write, Write) => ConfirmUser,
        // OSC52Write asking to Read or Paste: category error, deny.
        (OSC52Write, Read | Paste) => Deny,

        // PasteInjection — confirm for Paste (the normal case), allow for
        // reads (a paste path reading the pasteboard is what it exists to
        // do), deny for writes (paste origins should not write back).
        (PasteInjection, Paste) => ConfirmUser,
        (PasteInjection, Read) => Allow,
        (PasteInjection, Write) => Deny,
    }
}

// ---------------------------------------------------------------------------
// Policy object.
// ---------------------------------------------------------------------------

/// Origin-keyed clipboard policy. Holds one decision per
/// `(ClipboardOrigin, ClipboardAction)` pair.
///
/// Constructed via [`ClipboardPolicy::default`] (fail-closed defaults) or
/// [`ClipboardPolicy::permissive`] / [`ClipboardPolicy::hardened`] for
/// the Standard/Hardened profiles described in the master design. The
/// public [`ClipboardPolicy::set`] mutator lets hosts override a single
/// cell of the matrix without rebuilding the full policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardPolicy {
    // Encoded as a 5×3 flat matrix to keep it `Copy` and `const`-constructible.
    //
    // Index: origin_idx * 3 + action_idx
    matrix: [ClipboardDecision; 15],
}

impl ClipboardPolicy {
    const ORIGIN_COUNT: usize = 5;
    const ACTION_COUNT: usize = 3;

    const fn idx(origin: ClipboardOrigin, action: ClipboardAction) -> usize {
        let origin_idx = match origin {
            ClipboardOrigin::UserInitiated => 0,
            ClipboardOrigin::OSC52Read => 1,
            ClipboardOrigin::OSC52Write => 2,
            ClipboardOrigin::PasteInjection => 3,
            ClipboardOrigin::CheckpointRestore => 4,
        };
        let action_idx = match action {
            ClipboardAction::Write => 0,
            ClipboardAction::Read => 1,
            ClipboardAction::Paste => 2,
        };
        origin_idx * Self::ACTION_COUNT + action_idx
    }

    /// Fail-closed default policy (see module docs).
    #[must_use]
    pub const fn new_default() -> Self {
        let mut matrix = [ClipboardDecision::Deny; 15];
        let mut origin_idx = 0;
        while origin_idx < Self::ORIGIN_COUNT {
            let origin = ClipboardOrigin::ALL[origin_idx];
            let mut action_idx = 0;
            while action_idx < Self::ACTION_COUNT {
                let action = ClipboardAction::ALL[action_idx];
                matrix[origin_idx * Self::ACTION_COUNT + action_idx] =
                    default_decision_for(origin, action);
                action_idx += 1;
            }
            origin_idx += 1;
        }
        Self { matrix }
    }

    /// Permissive profile — `Allow` for everything except `OSC52Read`
    /// and `OSC52Write`, which stay at `ConfirmUser` (never silently
    /// drop; always show the host-side prompt). Used by testing,
    /// demo modes, and legacy-shell compat.
    #[must_use]
    pub const fn permissive() -> Self {
        let mut policy = Self::new_default();
        policy.matrix[Self::idx(ClipboardOrigin::OSC52Read, ClipboardAction::Read)] =
            ClipboardDecision::ConfirmUser;
        policy.matrix[Self::idx(ClipboardOrigin::OSC52Write, ClipboardAction::Write)] =
            ClipboardDecision::ConfirmUser;
        policy
    }

    /// Hardened profile — `Deny` for all non-UserInitiated / non-
    /// CheckpointRestore actions. No `ConfirmUser` prompts; no silent
    /// PTY-origin clipboard access at all.
    #[must_use]
    pub const fn hardened() -> Self {
        let mut matrix = [ClipboardDecision::Deny; 15];
        let mut action_idx = 0;
        while action_idx < Self::ACTION_COUNT {
            matrix[Self::idx(
                ClipboardOrigin::UserInitiated,
                ClipboardAction::ALL[action_idx],
            )] = ClipboardDecision::Allow;
            matrix[Self::idx(
                ClipboardOrigin::CheckpointRestore,
                ClipboardAction::ALL[action_idx],
            )] = ClipboardDecision::Allow;
            action_idx += 1;
        }
        Self { matrix }
    }

    /// Evaluate the policy for a given origin + action.
    ///
    /// Total function — returns a decision for every `(origin, action)`
    /// pair.
    #[must_use]
    pub const fn evaluate(
        &self,
        origin: ClipboardOrigin,
        action: ClipboardAction,
    ) -> ClipboardDecision {
        self.matrix[Self::idx(origin, action)]
    }

    /// Override a single cell of the policy matrix. Used by hosts to
    /// tighten or loosen a specific origin's behaviour relative to a
    /// baseline profile (e.g., start from `default`, then upgrade
    /// `OSC52Write` to `Deny` without touching `PasteInjection`).
    pub const fn set(
        &mut self,
        origin: ClipboardOrigin,
        action: ClipboardAction,
        decision: ClipboardDecision,
    ) {
        self.matrix[Self::idx(origin, action)] = decision;
    }
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self::new_default()
    }
}

// ---------------------------------------------------------------------------
// Tests — exhaustive origin × action coverage.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_user_initiated_is_allow_for_all_actions() {
        let policy = ClipboardPolicy::default();
        for action in ClipboardAction::ALL {
            assert_eq!(
                policy.evaluate(ClipboardOrigin::UserInitiated, action),
                ClipboardDecision::Allow,
                "UserInitiated must be Allow for {action:?} in default policy",
            );
        }
    }

    #[test]
    fn default_checkpoint_restore_is_allow_for_all_actions() {
        let policy = ClipboardPolicy::default();
        for action in ClipboardAction::ALL {
            assert_eq!(
                policy.evaluate(ClipboardOrigin::CheckpointRestore, action),
                ClipboardDecision::Allow,
                "CheckpointRestore must be Allow for {action:?} in default policy",
            );
        }
    }

    #[test]
    fn default_osc52_read_is_deny() {
        let policy = ClipboardPolicy::default();
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Read, ClipboardAction::Read),
            ClipboardDecision::Deny,
        );
        // Category-error pairs also deny (read-origin asking for a write).
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Read, ClipboardAction::Write),
            ClipboardDecision::Deny,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Read, ClipboardAction::Paste),
            ClipboardDecision::Deny,
        );
    }

    #[test]
    fn default_osc52_write_is_confirm_user() {
        let policy = ClipboardPolicy::default();
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Write, ClipboardAction::Write),
            ClipboardDecision::ConfirmUser,
        );
        // Category-error pairs still deny.
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Write, ClipboardAction::Read),
            ClipboardDecision::Deny,
        );
    }

    #[test]
    fn default_paste_injection_confirms_paste_denies_write_allows_read() {
        let policy = ClipboardPolicy::default();
        assert_eq!(
            policy.evaluate(ClipboardOrigin::PasteInjection, ClipboardAction::Paste),
            ClipboardDecision::ConfirmUser,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::PasteInjection, ClipboardAction::Write),
            ClipboardDecision::Deny,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::PasteInjection, ClipboardAction::Read),
            ClipboardDecision::Allow,
        );
    }

    #[test]
    fn hardened_denies_all_non_user_non_checkpoint() {
        let policy = ClipboardPolicy::hardened();
        for origin in ClipboardOrigin::ALL {
            for action in ClipboardAction::ALL {
                let decision = policy.evaluate(origin, action);
                match origin {
                    ClipboardOrigin::UserInitiated | ClipboardOrigin::CheckpointRestore => {
                        assert_eq!(
                            decision,
                            ClipboardDecision::Allow,
                            "hardened: {origin:?} {action:?} must be Allow"
                        );
                    }
                    _ => {
                        assert_eq!(
                            decision,
                            ClipboardDecision::Deny,
                            "hardened: {origin:?} {action:?} must be Deny"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn permissive_promotes_osc52_to_confirm() {
        let policy = ClipboardPolicy::permissive();
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Read, ClipboardAction::Read),
            ClipboardDecision::ConfirmUser,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Write, ClipboardAction::Write),
            ClipboardDecision::ConfirmUser,
        );
        // UserInitiated remains Allow.
        assert_eq!(
            policy.evaluate(ClipboardOrigin::UserInitiated, ClipboardAction::Write),
            ClipboardDecision::Allow,
        );
    }

    #[test]
    fn set_overrides_single_cell() {
        let mut policy = ClipboardPolicy::default();
        // Default: OSC52Write => Write is ConfirmUser. Tighten to Deny.
        policy.set(
            ClipboardOrigin::OSC52Write,
            ClipboardAction::Write,
            ClipboardDecision::Deny,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::OSC52Write, ClipboardAction::Write),
            ClipboardDecision::Deny,
        );
        // Other cells untouched — PasteInjection still ConfirmUser for
        // Paste, UserInitiated still Allow.
        assert_eq!(
            policy.evaluate(ClipboardOrigin::PasteInjection, ClipboardAction::Paste),
            ClipboardDecision::ConfirmUser,
        );
        assert_eq!(
            policy.evaluate(ClipboardOrigin::UserInitiated, ClipboardAction::Write),
            ClipboardDecision::Allow,
        );
    }

    #[test]
    fn matrix_is_total_function() {
        // Sanity check that evaluate returns for every origin × action
        // pair without panic. This is a structural backstop against
        // future enum variants that forget to update the matrix.
        let policy = ClipboardPolicy::default();
        let mut seen = 0usize;
        for origin in ClipboardOrigin::ALL {
            for action in ClipboardAction::ALL {
                let _decision: ClipboardDecision = policy.evaluate(origin, action);
                seen += 1;
            }
        }
        assert_eq!(
            seen,
            ClipboardOrigin::ALL.len() * ClipboardAction::ALL.len()
        );
    }

    #[test]
    fn default_and_new_default_agree() {
        assert_eq!(ClipboardPolicy::default(), ClipboardPolicy::new_default());
    }
}
