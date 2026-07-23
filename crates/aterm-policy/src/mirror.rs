// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mirror-field wiring for the six `TerminalModes::allow_*` booleans
//! (§4.5 + §6.1 of `designs/2026-04-19-osc-policy-engine.md`).
//!
//! During the two-release deprecation window laid out in §6.2, six existing
//! `TerminalModes` booleans continue to co-exist with the [`PolicyEngine`].
//! Handlers still read `modes.allow_osc52_query`, `modes.allow_window_ops`,
//! etc., but the **source of truth** moves into the policy. The invariant
//! that ties the two representations together is:
//!
//! ```text
//! policy_engine.effective_response_pty(field) == Execute
//!     iff   modes.allow_<field> == true
//! ```
//!
//! for each of the six [`MirrorField`] variants. This module provides:
//!
//! * [`MirrorField`] — enum naming the six migrating booleans.
//! * [`MirrorSnapshot`] — six booleans produced by reading the current policy
//!   state, ready to assign into `TerminalModes`.
//! * [`PolicyEngine::standard`] — convenience constructor for the Standard
//!   profile (symmetric with [`PolicyEngine::hardened`]).
//! * [`PolicyEngine::mirror_snapshot`] — read path: derive the six booleans
//!   from the current policy.
//! * [`PolicyEngine::set_mirror_bool`] — write path: mutate the policy so
//!   `effective_response_pty(field)` matches the caller-supplied bool.
//! * [`PolicyEngine::effective_response_pty`] — evaluator used by both paths.
//!
//! # Wiring into `Terminal` is out of scope
//!
//! Handlers still read `modes.allow_*` directly; swapping them to consult the
//! engine (step (c) of §6.2 release N) lands in a follow-up issue — this
//! module only builds the plumbing the follow-up consumes.
//!
//! # The odd one out: `require_shell_integration_nonce`
//!
//! Per design §6.4, `require_shell_integration_nonce` is not a per-sequence
//! rule; it gates the capability mint path. We still expose it through
//! [`MirrorField`] to keep the snapshot API symmetric, but the effective
//! response is derived from [`crate::Defaults::shell_integration_require_nonce`]
//! directly: `Execute` means "the nonce gate is active". This preserves the
//! uniform invariant `Execute ⟺ bool_set` while documenting the inversion of
//! semantic meaning compared to the five `allow_*` fields.

use crate::{
    OriginTag, Response, Rule, engine::PolicyEngine, profiles, selector::DispatchedSequence,
};

// ---------------------------------------------------------------------------
// MirrorField
// ---------------------------------------------------------------------------

/// One of the six `TerminalModes` booleans migrating through the policy
/// engine deprecation window (§6.1).
///
/// The variants cover every `allow_*` / `require_*` field the design §2
/// table identifies as "policy-bearing"; no other boolean on `TerminalModes`
/// is mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MirrorField {
    /// Mirror of `TerminalModes::allow_osc52_query`. OSC 52 clipboard query.
    AllowOsc52Query,
    /// Mirror of `TerminalModes::allow_osc52_set`. OSC 52 clipboard write.
    AllowOsc52Set,
    /// Mirror of `TerminalModes::allow_window_ops`. CSI `t` XTWINOPS (move,
    /// resize, iconify, title-stack, title reports).
    AllowWindowOps,
    /// Mirror of `TerminalModes::allow_notifications`. OSC 9 / OSC 99 / OSC
    /// 777. The probe sequence uses OSC 9; the engine's rule set covers the
    /// other two separately.
    AllowNotifications,
    /// Mirror of `TerminalModes::allow_palette_reconfigure`. OSC 4 / OSC 21
    /// indexed palette **set**. Queries are not gated by this field. Probe
    /// uses `OSC 4 set`.
    AllowPaletteReconfigure,
    /// Mirror of `TerminalModes::require_shell_integration_nonce`. OSC 133 /
    /// OSC 633 shell-integration nonce requirement.
    ///
    /// Per design §6.4 this bool is not a per-sequence rule; it gates the
    /// capability mint. The [`PolicyEngine::effective_response_pty`] probe
    /// for this field returns `Execute` iff
    /// [`crate::Defaults::shell_integration_require_nonce`] is `true` — the
    /// uniform `Execute ⟺ bool_true` invariant, even though the semantic
    /// meaning ("nonce gate is active") is the inverse of the five
    /// permissive `allow_*` flags.
    RequireShellIntegrationNonce,
}

impl MirrorField {
    /// All six variants in declaration order. Stable for iteration in tests
    /// and FFI enumerators.
    pub const ALL: [Self; 6] = [
        Self::AllowOsc52Query,
        Self::AllowOsc52Set,
        Self::AllowWindowOps,
        Self::AllowNotifications,
        Self::AllowPaletteReconfigure,
        Self::RequireShellIntegrationNonce,
    ];

    /// Canonical selector alias string used by the mirror write path (§3.4
    /// aliases). For [`Self::RequireShellIntegrationNonce`] this is `None`
    /// because that field is not rule-gated (see the variant docs).
    #[must_use]
    pub const fn canonical_alias(self) -> Option<&'static str> {
        match self {
            Self::AllowOsc52Query => Some("OSC 52 query"),
            Self::AllowOsc52Set => Some("OSC 52 set"),
            Self::AllowWindowOps => Some("CSI t"),
            Self::AllowNotifications => Some("OSC 9"),
            Self::AllowPaletteReconfigure => Some("OSC 4 set"),
            Self::RequireShellIntegrationNonce => None,
        }
    }

    /// Marker stored in [`Rule::prompt_id`] so mirror-written rules can be
    /// identified and rewritten on subsequent [`PolicyEngine::set_mirror_bool`]
    /// calls without growing the rule list without bound.
    #[must_use]
    pub const fn prompt_marker(self) -> &'static str {
        match self {
            Self::AllowOsc52Query => "__mirror__:allow_osc52_query",
            Self::AllowOsc52Set => "__mirror__:allow_osc52_set",
            Self::AllowWindowOps => "__mirror__:allow_window_ops",
            Self::AllowNotifications => "__mirror__:allow_notifications",
            Self::AllowPaletteReconfigure => "__mirror__:allow_palette_reconfigure",
            Self::RequireShellIntegrationNonce => "__mirror__:require_shell_integration_nonce",
        }
    }

    /// Build the concrete [`DispatchedSequence`] used to probe
    /// [`PolicyEngine::evaluate`] for this mirror field.
    ///
    /// Returns `None` for [`Self::RequireShellIntegrationNonce`] — that
    /// field is read directly from
    /// [`crate::Defaults::shell_integration_require_nonce`], not by probing
    /// the rule engine.
    #[must_use]
    pub fn probe_sequence(self) -> Option<DispatchedSequence> {
        match self {
            Self::AllowOsc52Query => Some(DispatchedSequence::osc(
                52,
                [String::from("c"), String::from("?")],
            )),
            Self::AllowOsc52Set => Some(DispatchedSequence::osc(
                52,
                [String::from("c"), String::from("SGVsbG8=")],
            )),
            Self::AllowWindowOps => Some(DispatchedSequence::csi(Some(1), 't', [])),
            Self::AllowNotifications => Some(DispatchedSequence::osc(9, [String::from("probe")])),
            Self::AllowPaletteReconfigure => Some(DispatchedSequence::osc(
                4,
                [String::from("3"), String::from("#112233")],
            )),
            Self::RequireShellIntegrationNonce => None,
        }
    }
}

// ---------------------------------------------------------------------------
// MirrorSnapshot
// ---------------------------------------------------------------------------

/// Snapshot of the six mirror-field booleans derived from the current
/// [`PolicyEngine`] state.
///
/// Produced by [`PolicyEngine::mirror_snapshot`]. The follow-up issue that
/// wires this into `Terminal` will assign these fields into
/// `TerminalModes::allow_*` on every policy update. This struct deliberately
/// owns no references so callers can stash it across an FFI boundary or a
/// mutex drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MirrorSnapshot {
    /// Mirror of [`MirrorField::AllowOsc52Query`].
    pub allow_osc52_query: bool,
    /// Mirror of [`MirrorField::AllowOsc52Set`].
    pub allow_osc52_set: bool,
    /// Mirror of [`MirrorField::AllowWindowOps`].
    pub allow_window_ops: bool,
    /// Mirror of [`MirrorField::AllowNotifications`].
    pub allow_notifications: bool,
    /// Mirror of [`MirrorField::AllowPaletteReconfigure`].
    pub allow_palette_reconfigure: bool,
    /// Mirror of [`MirrorField::RequireShellIntegrationNonce`].
    pub require_shell_integration_nonce: bool,
}

impl MirrorSnapshot {
    /// Read the bool for a single field. Symmetric with
    /// [`Self::set`]; used by tests and FFI enumerators that don't want to
    /// pattern-match on the struct.
    #[must_use]
    pub const fn get(&self, field: MirrorField) -> bool {
        match field {
            MirrorField::AllowOsc52Query => self.allow_osc52_query,
            MirrorField::AllowOsc52Set => self.allow_osc52_set,
            MirrorField::AllowWindowOps => self.allow_window_ops,
            MirrorField::AllowNotifications => self.allow_notifications,
            MirrorField::AllowPaletteReconfigure => self.allow_palette_reconfigure,
            MirrorField::RequireShellIntegrationNonce => self.require_shell_integration_nonce,
        }
    }

    /// Write the bool for a single field in place.
    pub const fn set(&mut self, field: MirrorField, value: bool) {
        match field {
            MirrorField::AllowOsc52Query => self.allow_osc52_query = value,
            MirrorField::AllowOsc52Set => self.allow_osc52_set = value,
            MirrorField::AllowWindowOps => self.allow_window_ops = value,
            MirrorField::AllowNotifications => self.allow_notifications = value,
            MirrorField::AllowPaletteReconfigure => self.allow_palette_reconfigure = value,
            MirrorField::RequireShellIntegrationNonce => {
                self.require_shell_integration_nonce = value;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PolicyEngine mirror API
// ---------------------------------------------------------------------------

impl PolicyEngine {
    /// Convenience constructor for the Standard profile. Symmetric with
    /// [`Self::hardened`].
    #[must_use]
    pub fn standard() -> Self {
        Self::new(profiles::standard())
    }

    /// Compute the effective response the engine would return for this
    /// field's probe sequence at `Pty` origin.
    ///
    /// This is the read half of the §6.1 invariant. [`MirrorSnapshot`]
    /// callers map `Execute → true` and anything else → `false`. The method
    /// is exposed directly so tests can assert the raw response value, not
    /// just the derived bool.
    ///
    /// For [`MirrorField::RequireShellIntegrationNonce`] no sequence is
    /// probed — the response is `Execute` iff
    /// [`crate::Defaults::shell_integration_require_nonce`] is `true`
    /// (see the variant docs for the rationale).
    #[must_use]
    pub fn effective_response_pty(&self, field: MirrorField) -> Response {
        if field == MirrorField::RequireShellIntegrationNonce {
            return if self.policy().defaults.shell_integration_require_nonce {
                Response::Execute
            } else {
                Response::Drop
            };
        }
        let Some(seq) = field.probe_sequence() else {
            // Unreachable given the guard above, but totally defensive:
            // an unknown mirror field falls closed.
            return Response::Drop;
        };
        self.evaluate(&seq, OriginTag::Pty).response
    }

    /// Read the six mirror booleans out of the current policy state.
    ///
    /// Implements the read half of the §6.1 invariant — callers assign this
    /// snapshot into `TerminalModes` to keep the booleans consistent with
    /// the policy. Called after every [`Self::replace_policy`] /
    /// [`Self::set_mirror_bool`] mutation by the follow-up `Terminal` wiring
    /// issue.
    #[must_use]
    pub fn mirror_snapshot(&self) -> MirrorSnapshot {
        let mut snap = MirrorSnapshot::default();
        for field in MirrorField::ALL {
            snap.set(
                field,
                self.effective_response_pty(field) == Response::Execute,
            );
        }
        snap
    }

    /// Write the policy state so `effective_response_pty(field) == Execute`
    /// iff `value == true`, establishing the §6.1 invariant for this field.
    ///
    /// Implementation:
    ///
    /// 1. Remove every existing rule tagged with
    ///    [`MirrorField::prompt_marker`] for this field (keeps the rule
    ///    vector from growing across repeated `set` calls).
    /// 2. Prepend a fresh rule whose selector is the field's canonical
    ///    alias, `origin_min = NetworkUntrusted` (so every origin passes the
    ///    gate — including `Pty`), and `response =
    ///    if value { Execute } else { Drop }`.
    /// 3. Rebuild the compiled decision tree.
    ///
    /// [`MirrorField::RequireShellIntegrationNonce`] is handled specially:
    /// it writes to [`crate::Defaults::shell_integration_require_nonce`]
    /// directly and does not touch the rule list.
    pub fn set_mirror_bool(&mut self, field: MirrorField, value: bool) {
        let mut policy = self.policy().clone();

        if field == MirrorField::RequireShellIntegrationNonce {
            policy.defaults.shell_integration_require_nonce = value;
            self.replace_policy(policy);
            return;
        }

        let marker = field.prompt_marker();
        policy
            .rules
            .retain(|r| r.prompt_id.as_deref() != Some(marker));

        let Some(alias) = field.canonical_alias() else {
            // RequireShellIntegrationNonce handled above; any future variant
            // without an alias falls through as a no-op.
            self.replace_policy(policy);
            return;
        };

        let response = if value {
            Response::Execute
        } else {
            Response::Drop
        };
        policy.rules.insert(
            0,
            Rule {
                sequence: alias.to_owned(),
                origin_min: OriginTag::NetworkUntrusted,
                response,
                rate_limit: None,
                prompt_id: Some(marker.to_owned()),
            },
        );

        self.replace_policy(policy);
    }
}
