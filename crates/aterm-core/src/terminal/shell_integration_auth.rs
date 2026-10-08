// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 133 / OSC 633 shell-integration capability-nonce authority.
//!
//! A file piped through the PTY can write a full OSC 133 A/B/C/D cycle (or
//! the OSC 633 variants) and forge prompt marks, exit statuses and prompt
//! callbacks. So shell-integration emissions can be required to carry a nonce
//! committed out of band: the host generates a 32-byte CSPRNG nonce, installs
//! it via [`super::Terminal::authorize_shell_integration`], and passes it to
//! the shell (`ATERM_SHELL_NONCE`), whose preamble adds `id=<64-hex>` to every
//! OSC 133/633 sequence.
//!
//! When [`aterm_types::TerminalModes::require_shell_integration_nonce`] is set,
//! every A/B/C/D (and 633 E/F/G/H/P) without a matching `id=` is dropped: no
//! callback, no state transition, no response. The comparison is constant-time.
//! `id=` may appear at any parameter index from 2 on, since OSC 633 sub-ops
//! carry payload params ahead of it; hex is case-insensitive.
//!
//! The requirement defaults to off so third-party integrations without `id=`
//! keep working; hosts that ship a nonced preamble turn it on at session init.

/// Host-side authorization state for the OSC 133/633 capability nonce.
///
/// Handlers reach it only through the `shell_integration_auth` borrow slot,
/// which exposes [`ShellIntegrationAuth::verify_nonce`] and nothing that
/// changes the authorization.
#[derive(Default, Debug)]
pub(crate) struct ShellIntegrationAuth {
    /// Host-authorized nonce, or `None` if not yet authorized.
    nonce: Option<[u8; 32]>,
    /// OSC 133/633 sequences dropped for a missing, malformed or mismatched
    /// nonce (or none authorized), for host metrics.
    dropped_count: u64,
    /// The nonce was handed to a shell that has not yet taken it
    /// ([`Self::authorize_on_first_mark`]): an adopted shell's re-key, read
    /// from a file at its next prompt, which may be hours away. Marks signed
    /// with it verify as usual, but until the first one does the nonce is not
    /// in use ([`Self::nonce_in_use`] is `None`), so the host neither reports
    /// integration as live nor carries across an update a key the shell may
    /// never have seen. Cleared by the first valid mark, [`Self::authorize`]
    /// and [`Self::revoke`].
    awaiting_first_mark: bool,
    /// A typed re-key's way back ([`Self::authorize_rekey`]): the authorization
    /// it replaced, kept while the new key waits in a one-use file for a line
    /// typed at the prompt to read. The host restores it
    /// ([`Self::withdraw_rekey`]) if the line never ran, or forgets it
    /// ([`Self::settle_rekey`]) once the file is gone. Also cleared by the first
    /// mark signed with the key, [`Self::authorize`] and
    /// [`Self::authorize_on_first_mark`].
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
    pub(crate) const fn new() -> Self {
        Self {
            nonce: None,
            dropped_count: 0,
            awaiting_first_mark: false,
            typed_prior: None,
        }
    }

    /// Authorize shell integration with the given 32-byte nonce, replacing any
    /// earlier one; sequences carrying `id=<hex>` equal to it then pass
    /// [`verify_nonce`].
    pub(crate) fn authorize(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
        self.awaiting_first_mark = false;
        self.typed_prior = None;
    }

    /// [`Self::authorize`] for a nonce the shell has not taken yet (see
    /// `awaiting_first_mark`): it verifies marks from now on, and it is in use
    /// from the first mark that carries it.
    pub(crate) fn authorize_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
        self.awaiting_first_mark = true;
        self.typed_prior = None;
    }

    /// [`Self::authorize_on_first_mark`] for a typed re-key, which can be taken
    /// back: the authorization it replaces is kept (`typed_prior`) until the
    /// host settles the key either way. A second typed re-key before the first
    /// is settled keeps the first one's prior — the authorization the shell
    /// had before any of them.
    pub(crate) fn authorize_rekey(&mut self, nonce: [u8; 32]) {
        let prior = self.typed_prior.take().unwrap_or(Prior {
            nonce: self.nonce,
            awaiting_first_mark: self.awaiting_first_mark,
        });
        self.nonce = Some(nonce);
        self.awaiting_first_mark = true;
        self.typed_prior = Some(prior);
    }

    /// Whether `nonce` is the typed re-key authorized now, not yet settled and
    /// not yet taken by a mark.
    fn rekey_pending(&self, nonce: &[u8; 32]) -> bool {
        self.typed_prior.is_some()
            && self.awaiting_first_mark
            && self.nonce.as_ref().is_some_and(|n| n == nonce)
    }

    /// Take back the typed re-key `nonce`: the authorization it replaced is
    /// restored, so a key whose line never ran authorizes nothing. `false`,
    /// changing nothing, when `nonce` is not the pending typed re-key — a
    /// mark already carried it (the shell has it and uses it: a working key is
    /// never moved), another authorization replaced it, or it was settled.
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

    /// keep the typed re-key `nonce`: its file was consumed, so the shell holds
    /// the key, and the authorization it replaced is forgotten. The key stays
    /// not-in-use until its first mark, exactly as after
    /// [`Self::authorize_on_first_mark`]. `false`, changing nothing, when
    /// `nonce` is not the pending typed re-key.
    pub(crate) fn settle_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        if !self.rekey_pending(nonce) {
            return false;
        }
        self.typed_prior = None;
        true
    }

    /// Revoke the authorized nonce; no sequence satisfies [`verify_nonce`]
    /// until `authorize` is called again.
    #[cfg(test)]
    pub(crate) fn revoke(&mut self) {
        self.nonce = None;
        self.awaiting_first_mark = false;
        self.typed_prior = None;
    }

    /// The authorized nonce once the shell is using it: always for one set by
    /// [`Self::authorize`] (at spawn or from a handoff carry), and for one set
    /// by [`Self::authorize_on_first_mark`] only after a mark signed with it
    /// arrived. This is what `status integration=` reports and what the
    /// handoff carry takes ([`super::Terminal::checkpoint_carry`]): the
    /// adopted shell keeps signing with it, so the successor must authorize
    /// the same value. A re-key the shell never took is not carried.
    #[must_use]
    pub(crate) fn nonce_in_use(&self) -> Option<[u8; 32]> {
        self.nonce.filter(|_| !self.awaiting_first_mark)
    }

    /// OSC 133/633 sequences dropped since construction or the last reset,
    /// for host metrics.
    #[must_use]
    pub(crate) fn dropped_count(&self) -> u64 {
        self.dropped_count
    }

    /// Verify a claimed nonce against the authorized nonce.
    ///
    /// Scans `params` (the OSC parameters split on `;`) from index 2 for the
    /// first `id=<hex>` and returns `true` iff a nonce is authorized, the hex
    /// is 64 digits, and the decoded bytes equal the nonce (constant-time).
    /// Otherwise increments the drop counter and returns `false`.
    pub(crate) fn verify_nonce(&mut self, params: &[&[u8]]) -> bool {
        let Some(expected) = self.nonce.as_ref() else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        // OSC parameter 0 is the command number ("133"/"633"), parameter 1
        // is the subcommand char ("A"/"B"/...). The nonce can appear in any
        // position from index 2 onward — OSC 633 E/F/G/H/P carry payload
        // params ahead of it, and we must not require a fixed column.
        let Some(claimed_hex) = find_nonce_hex(params) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        let Some(claimed) = decode_hex_32(claimed_hex) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        if constant_time_eq_32(&claimed, expected) {
            // The shell has the key: a re-key it had not taken is in use now,
            // and a typed one is past taking back.
            self.awaiting_first_mark = false;
            self.typed_prior = None;
            true
        } else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            false
        }
    }

    /// Engine-consulting variant of [`Self::verify_nonce`].
    ///
    /// `gate` is the policy verdict for this dispatch
    /// ([`super::policy_gates::PolicyState::shell_integration_gate`], compiled
    /// from [`probe_shell_integration`] per installed policy for the subcommand
    /// letters shell integration uses, evaluated live otherwise):
    ///
    /// * a rule with response `Execute` proceeds to the nonce check (a valid
    ///   nonce is still required);
    /// * a rule with any other response returns `false` and counts a drop;
    /// * no policy, or no matching rule, defers to the nonce check.
    ///
    /// The probe is keyed on the subcommand: [`probe_subcommand`] substitutes
    /// `"A"` when `params[1]` is absent or not UTF-8, and a policy can name
    /// `"OSC 133;C"`, so the compiled table is keyed per subcommand.
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

/// The subcommand glyph the OSC 133/633 policy probe uses: `params[1]` when
/// it is valid UTF-8, `"A"` otherwise (including when absent). Shared with
/// [`super::policy_gates`] so the compiled table uses the same key.
#[must_use]
pub(super) fn probe_subcommand<'a>(params: &'a [&'a [u8]]) -> &'a str {
    params
        .get(1)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("A")
}

/// Probe sequence for the OSC 133 / OSC 633 policy lookup. `command` is `133`
/// or `633`; `subcommand` comes from [`probe_subcommand`]. The one definition,
/// shared by the compiled gate table and its live fallback.
#[must_use]
pub(super) fn probe_shell_integration(
    command: u32,
    subcommand: &str,
) -> aterm_policy::selector::DispatchedSequence {
    aterm_policy::selector::DispatchedSequence::osc(command, [subcommand.to_owned()])
}

/// Scan OSC parameters from index 2 for an `id=<hex>` tag and return the hex
/// (without `id=`). Only the first occurrence counts, as in VS Code's OSC 633
/// parser.
fn find_nonce_hex<'a>(params: &'a [&'a [u8]]) -> Option<&'a [u8]> {
    params.get(2..)?.iter().find_map(|p| p.strip_prefix(b"id="))
}

/// Decode a 64-character hex string to a 32-byte nonce.
///
/// Returns `None` if the input length is wrong or any byte is not a
/// valid hex digit. Accepts upper and lower case. Does not short-
/// circuit on the first bad byte, so timing does not leak the position
/// of the first invalid character.
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

/// Decode a single ASCII hex digit.
///
/// Returns `(value, 0)` on success, `(0, 1)` on invalid input. The
/// dual return avoids a branch so `decode_hex_32` does not leak the
/// position of the first invalid byte through timing.
#[inline]
fn hex_digit(b: u8) -> (u8, u8) {
    match b {
        b'0'..=b'9' => (b - b'0', 0),
        b'a'..=b'f' => (b - b'a' + 10, 0),
        b'A'..=b'F' => (b - b'A' + 10, 0),
        _ => (0, 1),
    }
}

/// Constant-time equality check on two 32-byte nonces.
///
/// Bitwise-or-accumulates all XOR differences, then checks the sum at
/// the end. [`std::hint::black_box`] prevents LLVM from rewriting the
/// loop into an early-exit form that would leak the mismatch position
/// through timing.
#[inline]
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff: u8 = 0;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nonce_with_byte(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn hex_of(nonce: &[u8; 32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        for &b in nonce {
            out.push(to_hex_digit(b >> 4));
            out.push(to_hex_digit(b & 0xF));
        }
        out
    }

    fn to_hex_digit(v: u8) -> u8 {
        match v {
            0..=9 => b'0' + v,
            10..=15 => b'a' + (v - 10),
            _ => unreachable!("only called with 4-bit nibbles"),
        }
    }

    #[test]
    fn verify_nonce_unauthorized_returns_false() {
        let mut auth = ShellIntegrationAuth::new();
        let nonce_hex = hex_of(&nonce_with_byte(0xAB));
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&nonce_hex);
        let params: &[&[u8]] = &[b"133", b"A", &id_param];
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count(), 1);
    }

    #[test]
    fn verify_nonce_matching_returns_true() {
        let mut auth = ShellIntegrationAuth::new();
        let nonce = nonce_with_byte(0x11);
        auth.authorize(nonce);
        let hex = hex_of(&nonce);
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&hex);
        let params: &[&[u8]] = &[b"133", b"A", &id_param];
        assert!(auth.verify_nonce(params));
        assert_eq!(auth.dropped_count(), 0);
    }

    #[test]
    fn verify_nonce_wrong_nonce_returns_false() {
        let mut auth = ShellIntegrationAuth::new();
        auth.authorize(nonce_with_byte(0x11));
        let hex = hex_of(&nonce_with_byte(0x22)); // different nonce
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&hex);
        let params: &[&[u8]] = &[b"133", b"A", &id_param];
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count(), 1);
    }

    #[test]
    fn verify_nonce_missing_id_param_returns_false() {
        let mut auth = ShellIntegrationAuth::new();
        auth.authorize(nonce_with_byte(0x11));
        let params: &[&[u8]] = &[b"133", b"A"];
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count(), 1);
    }

    #[test]
    fn verify_nonce_malformed_hex_returns_false() {
        let mut auth = ShellIntegrationAuth::new();
        auth.authorize(nonce_with_byte(0x11));
        // 63 chars (too short)
        let short = b"id=ababababababababababababababababababababababababababababababab";
        let params_short: &[&[u8]] = &[b"133", b"A", short];
        assert!(!auth.verify_nonce(params_short));
        // 64 chars but 'g' is not hex
        let bad = b"id=ggababababababababababababababababababababababababababababababab";
        let params_bad: &[&[u8]] = &[b"133", b"A", bad];
        assert!(!auth.verify_nonce(params_bad));
        assert!(auth.dropped_count() >= 2);
    }

    #[test]
    fn verify_nonce_ignores_id_in_position_1() {
        // `id=...` at params[1] is the subcommand slot, not a nonce — the
        // nonce scanner starts at index 2 by design. A malicious input
        // that places `id=...` where the subcommand goes should *not*
        // count as a valid nonce (and will also not parse as a valid A/B/C/D).
        let mut auth = ShellIntegrationAuth::new();
        let nonce = nonce_with_byte(0x11);
        auth.authorize(nonce);
        let hex = hex_of(&nonce);
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&hex);
        let params: &[&[u8]] = &[b"133", &id_param];
        assert!(!auth.verify_nonce(params));
    }

    #[test]
    fn verify_nonce_scans_past_payload_params() {
        // OSC 633 E carries commandline before the nonce:
        //   params = ["633", "E", "<commandline>", "id=<hex>"]
        let mut auth = ShellIntegrationAuth::new();
        let nonce = nonce_with_byte(0x33);
        auth.authorize(nonce);
        let hex = hex_of(&nonce);
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&hex);
        let params: &[&[u8]] = &[b"633", b"E", b"ls -la", &id_param];
        assert!(auth.verify_nonce(params));
    }

    #[test]
    fn revoke_blocks_future_verification() {
        let mut auth = ShellIntegrationAuth::new();
        let nonce = nonce_with_byte(0x44);
        auth.authorize(nonce);
        auth.revoke();
        let hex = hex_of(&nonce);
        let mut id_param = b"id=".to_vec();
        id_param.extend_from_slice(&hex);
        let params: &[&[u8]] = &[b"133", b"A", &id_param];
        assert!(!auth.verify_nonce(params));
    }

    #[test]
    fn dropped_counter_saturates() {
        // Start near the maximum to exercise the saturating add without
        // iterating 2^64 times.
        let mut auth = ShellIntegrationAuth::new();
        auth.dropped_count = u64::MAX - 1;
        let params: &[&[u8]] = &[b"133", b"A"];
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count, u64::MAX);
        // Must not panic: saturating_add caps at the maximum.
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count, u64::MAX);
    }

    #[test]
    fn constant_time_eq_detects_late_mismatch() {
        let a = [0x42u8; 32];
        let mut b = [0x42u8; 32];
        b[31] = 0x43;
        assert!(!constant_time_eq_32(&a, &b));
        b[31] = 0x42;
        assert!(constant_time_eq_32(&a, &b));
    }

    #[test]
    fn decode_hex_32_roundtrip() {
        let expected: [u8; 32] = [
            0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76,
            0x54, 0x32, 0x10, 0x00, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xAA, 0xBB, 0xCC, 0xDD,
        ];
        let hex = b"000123456789abcdeffedcba987654321000ff112233445566778899aabbccdd";
        assert_eq!(decode_hex_32(hex), Some(expected));
        // Upper case accepted.
        let hex_upper = b"000123456789ABCDEFFEDCBA987654321000FF112233445566778899AABBCCDD";
        assert_eq!(decode_hex_32(hex_upper), Some(expected));
    }
}
