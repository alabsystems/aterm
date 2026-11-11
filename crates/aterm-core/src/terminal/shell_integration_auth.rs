// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 133 / OSC 633 shell-integration capability-nonce authority.

/// Host-side authorization state for the OSC 133/633 capability nonce.
#[derive(Clone, Default, Debug)]
pub(crate) struct ShellIntegrationAuth {
    /// Host-authorized nonce, or `None` if not yet authorized.
    nonce: Option<[u8; 32]>,
    /// OSC 133/633 sequences dropped for a missing, malformed or mismatched nonce (or none
    /// authorized).
    dropped_count: u64,
    /// OSC 133/633 sequences whose nonce verified (saturating). The crash-restore
    /// journal keyframes the slice that moved it: a restore replays raw output into an
    /// engine that authorizes no nonce, so a verified mark left in the replayed tail
    /// would be dropped there, and the restore could not be exact.
    verified_count: u64,
    /// The nonce was handed to a shell that has not taken it yet
    /// ([`Self::authorize_on_first_mark`]): an adopted shell's re-key, read from a file at
    /// its next prompt, which may be hours away. Marks signed with it verify, but until the
    /// first does it is not in use ([`Self::nonce_in_use`] is `None`), so the host neither
    /// reports integration live nor carries a key the shell may never have seen. Cleared by
    /// the first valid mark and by [`Self::authorize`].
    awaiting_first_mark: bool,
    /// A typed re-key's way back ([`Self::authorize_rekey`]): the authorization it replaced,
    /// kept while the new key waits in a one-use file for a line typed at the prompt. The
    /// host restores it ([`Self::withdraw_rekey`]) if the line never ran, or forgets it
    /// ([`Self::settle_rekey`]) once the file is gone. Also cleared by the first mark signed
    /// with the key, [`Self::authorize`] and [`Self::authorize_on_first_mark`].
    typed_prior: Option<Prior>,
}

/// The authorization a typed re-key replaced ([`ShellIntegrationAuth::typed_prior`]).
#[derive(Clone, Copy, Debug)]
struct Prior {
    nonce: Option<[u8; 32]>,
    awaiting_first_mark: bool,
}

impl ShellIntegrationAuth {
    /// Create a fresh authorization state with no nonce authorized.
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            nonce: None,
            dropped_count: 0,
            verified_count: 0,
            awaiting_first_mark: false,
            typed_prior: None,
        }
    }

    /// Authorize the 32-byte `nonce`, replacing any earlier one.
    #[inline]
    pub(crate) const fn authorize(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
        self.awaiting_first_mark = false;
        self.typed_prior = None;
    }

    /// [`Self::authorize`] for a nonce the shell has not taken yet (`awaiting_first_mark`).
    #[inline]
    pub(crate) const fn authorize_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
        self.awaiting_first_mark = true;
        self.typed_prior = None;
    }

    /// [`Self::authorize_on_first_mark`] for a typed re-key, which can be taken back.
    #[inline]
    pub(crate) const fn authorize_rekey(&mut self, nonce: [u8; 32]) {
        let prior = match self.typed_prior.take() {
            Some(p) => p,
            None => Prior {
                nonce: self.nonce,
                awaiting_first_mark: self.awaiting_first_mark,
            },
        };
        self.nonce = Some(nonce);
        self.awaiting_first_mark = true;
        self.typed_prior = Some(prior);
    }

    /// Whether `nonce` is the typed re-key authorized now, not yet settled and
    /// not yet taken by a mark.
    #[inline]
    fn rekey_pending(&self, nonce: &[u8; 32]) -> bool {
        self.typed_prior.is_some()
            && self.awaiting_first_mark
            && self.nonce.as_ref().is_some_and(|n| n == nonce)
    }

    /// Take back the typed re-key `nonce`, restoring the authorization it replaced, so a key whose
    /// line never ran authorizes nothing.
    #[inline]
    pub(crate) fn withdraw_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        if !self.rekey_pending(nonce) {
            return false;
        }
        if let Some(prior) = self.typed_prior.take() {
            self.nonce = prior.nonce;
            self.awaiting_first_mark = prior.awaiting_first_mark;
        }
        true
    }

    /// Keep the typed re-key `nonce`.
    #[inline]
    pub(crate) fn settle_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        if !self.rekey_pending(nonce) {
            return false;
        }
        self.typed_prior = None;
        true
    }

    /// The authorized nonce once the shell uses it.
    #[must_use]
    #[inline]
    pub(crate) const fn nonce_in_use(&self) -> Option<[u8; 32]> {
        if self.awaiting_first_mark {
            None
        } else {
            self.nonce
        }
    }

    /// Snapshot of an awaiting nonce (`awaiting_first_mark`) and any unsettled typed-rekey prior, for [`super::PaneStepCarry`].
    #[must_use]
    #[inline]
    pub(crate) const fn awaiting_carry(
        &self,
    ) -> (
        Option<super::ShellIntegrationNonce>,
        Option<(Option<super::ShellIntegrationNonce>, bool)>,
    ) {
        if !self.awaiting_first_mark {
            return (None, None);
        }
        let awaiting = match self.nonce {
            Some(n) => Some(super::ShellIntegrationNonce(n)),
            None => return (None, None),
        };
        let prior = match self.typed_prior {
            Some(p) => Some((
                match p.nonce {
                    Some(n) => Some(super::ShellIntegrationNonce(n)),
                    None => None,
                },
                p.awaiting_first_mark,
            )),
            None => None,
        };
        (awaiting, prior)
    }

    /// Restore an awaiting nonce and optional typed-rekey prior from [`super::PaneStepCarry`].
    #[inline]
    pub(crate) fn apply_awaiting_carry(
        &mut self,
        awaiting: Option<super::ShellIntegrationNonce>,
        prior: Option<(Option<super::ShellIntegrationNonce>, bool)>,
    ) {
        if let Some(super::ShellIntegrationNonce(nonce)) = awaiting {
            self.nonce = Some(nonce);
            self.awaiting_first_mark = true;
            self.typed_prior = prior.and_then(|(pn, pa)| {
                if pn.is_none() && pa {
                    None
                } else {
                    Some(Prior {
                        nonce: pn.map(|n| n.0),
                        awaiting_first_mark: pa,
                    })
                }
            });
        }
    }

    /// OSC 133/633 sequences dropped so far, for host metrics.
    #[must_use]
    #[inline]
    pub(crate) const fn dropped_count(&self) -> u64 {
        self.dropped_count
    }

    /// OSC 133/633 sequences whose nonce verified so far, for the crash-restore journal.
    #[must_use]
    #[inline]
    pub(crate) const fn verified_count(&self) -> u64 {
        self.verified_count
    }

    /// Verify a claimed nonce.
    pub(crate) fn verify_nonce(&mut self, params: &[&[u8]]) -> bool {
        let Some(expected) = self.nonce.as_ref() else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        let Some(claimed_hex) = find_nonce_hex(params) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        let Some(claimed) = decode_hex_32(claimed_hex) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        if aterm_digest::ct_eq(&claimed, expected) {
            self.awaiting_first_mark = false;
            self.typed_prior = None;
            self.verified_count = self.verified_count.saturating_add(1);
            true
        } else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            false
        }
    }

    /// [`Self::verify_nonce`] behind the policy verdict `gate`
    /// ([`super::policy_gates::PolicyState::shell_integration_gate`]): `Deny` (a rule with a
    /// response other than `Execute`) drops and counts; `Allow` and `Fallback` (no policy or no
    /// matching rule) still require a valid nonce.
    #[inline]
    pub(super) fn verify_nonce_with_engine(
        &mut self,
        gate: super::policy_bridge::BridgeDecision,
        params: &[&[u8]],
    ) -> bool {
        match gate {
            super::policy_bridge::BridgeDecision::Deny => {
                self.dropped_count = self.dropped_count.saturating_add(1);
                false
            }
            super::policy_bridge::BridgeDecision::Allow
            | super::policy_bridge::BridgeDecision::Fallback => self.verify_nonce(params),
        }
    }
}

/// The subcommand glyph the OSC 133/633 policy probe uses.
#[must_use]
#[inline]
pub(super) fn probe_subcommand<'a>(params: &'a [&'a [u8]]) -> &'a str {
    params
        .get(1)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("A")
}

/// The OSC 133 / OSC 633 policy probe (`command` `133` or `633`, `subcommand` from
/// [`probe_subcommand`]): the one definition, shared by the compiled gate table and its live
/// fallback.
#[must_use]
#[inline]
pub(super) fn probe_shell_integration(
    command: u32,
    subcommand: &str,
) -> aterm_policy::selector::DispatchedSequence {
    aterm_policy::selector::DispatchedSequence::osc(command, [subcommand.to_owned()])
}

/// Scan OSC parameters from index 2 for an `id=<hex>` tag and return the hex
/// (without `id=`). Only the first occurrence counts, as in VS Code's OSC 633
/// parser.
#[inline]
fn find_nonce_hex<'a>(params: &'a [&'a [u8]]) -> Option<&'a [u8]> {
    params.get(2..)?.iter().find_map(|p| p.strip_prefix(b"id="))
}

/// Decode 64 hex digits (either case) to a 32-byte nonce; `None` on a wrong length or a
/// non-hex byte. It does not stop at the first bad byte, so timing does not leak its
/// position.
fn decode_hex_32(hex: &[u8]) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let mut any_bad = 0u8;
    for i in 0..32 {
        let (hi, bad_hi) = hex_digit(hex[i * 2]);
        let (lo, bad_lo) = hex_digit(hex[i * 2 + 1]);
        any_bad |= bad_hi | bad_lo;
        out[i] = (hi << 4) | lo;
    }
    if any_bad != 0 { None } else { Some(out) }
}

/// Decode one ASCII hex digit: `(value, 0)`, or `(0, 1)` when invalid. Branch-free for the
/// caller, so `decode_hex_32` does not leak the first invalid byte's position.
#[inline]
const fn hex_digit(b: u8) -> (u8, u8) {
    match b {
        b'0'..=b'9' => (b - b'0', 0),
        b'a'..=b'f' => (b - b'a' + 10, 0),
        b'A'..=b'F' => (b - b'A' + 10, 0),
        _ => (0, 1),
    }
}

#[cfg(test)]
#[path = "shell_integration_auth_tests.rs"]
mod tests;
