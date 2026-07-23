// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 133 / OSC 633 shell-integration capability-nonce authority
//! (#7937 F01-2, #7960).
//!
//! # The OSC 133/633 spoof, and why this module exists
//!
//! A malicious file piped through the PTY (`cat evil.txt`) can write a full
//! OSC 133 A/B/C/D command cycle or the OSC 633 variants, producing spoofed
//! success badges in block-folding UIs, bogus exit-status chrome, contaminated
//! semantic memory, and forged prompt callbacks to voice, prediction, and
//! session-memory observers. This is parallel in structure to the Terminal DCS
//! 2000 p RCE that [`super::modal_auth::ModalProtocolAuth`] already defends
//! against: both rely on the fact that the terminal cannot tell a legitimate
//! shell-integration emission apart from a PTY-origin byte sequence under the
//! pre-nonce wire protocol.
//!
//! # The structural fix
//!
//! Shell integration emissions must carry a nonce committed out-of-band at
//! session init. The host:
//!
//! 1. Generates a 32-byte CSPRNG nonce at terminal init.
//! 2. Installs it here via [`super::Terminal::authorize_shell_integration`].
//! 3. Injects it into the spawned shell's environment (`ATERM_SHELL_NONCE`)
//!    and the shell-integration preamble emits every OSC 133/633 sequence
//!    with an `id=<64-hex>` parameter.
//!
//! When [`aterm_types::TerminalModes::require_shell_integration_nonce`] is
//! set, the OSC 133/633 handlers require every A/B/C/D (and 633 E/F/G/H/P)
//! to carry a valid `id=` parameter matching the authorized nonce. Missing
//! or wrong nonce → silently drop the sequence (no callback, no state
//! transition, no response). Comparison is constant-time via the same
//! `black_box`-guarded accumulator used by `modal_auth`, so an attacker
//! cannot use timing to recover the nonce byte-by-byte.
//!
//! # Nonce format
//!
//! 32-byte binary value. Wire encoding: `id=` followed by 64 hex characters
//! (upper- or lower-case). The `id=` key can appear in any OSC parameter
//! position from index 2 onward — this tolerates the variable arity of OSC
//! 633 sub-ops (A/B have two params before the nonce, D has three, E has the
//! commandline before the nonce, etc.).
//!
//! # Default-off posture
//!
//! [`aterm_types::TerminalModes::require_shell_integration_nonce`] defaults to
//! `false` to keep pre-existing shell integrations (third-party scripts that
//! do not yet emit `id=`) working. Hosts that ship a nonced preamble flip the
//! bit at session init. This matches the minimum-disruption landing pattern
//! from `allow_palette_reconfigure` (#7937 F01-3). The fail-closed posture
//! is gated behind that host policy bit by design.

/// Host-side authorization state for OSC 133/633 shell-integration
/// capability-nonce.
///
/// Holds the nonce authorized by the host application. Lives on
/// [`super::Terminal`] and is **not** exposed through `TerminalHandler`
/// directly — the OSC 133/633 handlers reach it via the
/// `shell_integration_auth` handler borrow slot, which only grants
/// read access via [`ShellIntegrationAuth::verify_nonce`].
#[derive(Default, Debug)]
pub(crate) struct ShellIntegrationAuth {
    /// Host-authorized nonce, or `None` if not yet authorized.
    nonce: Option<[u8; 32]>,
    /// Number of OSC 133/633 sequences silently dropped because the
    /// wire protocol required a nonce and either (a) none was
    /// authorized, (b) `id=` was missing, or (c) `id=` was malformed
    /// or mismatched. Exposed for host-side metrics / hardening audits.
    dropped_count: u64,
}

impl ShellIntegrationAuth {
    /// Create a fresh authorization state with no nonce authorized.
    pub(crate) const fn new() -> Self {
        Self {
            nonce: None,
            dropped_count: 0,
        }
    }

    /// Authorize shell integration with the given 32-byte nonce. Replaces
    /// any previously authorized nonce. After this call, an OSC 133/633
    /// sequence carrying `id=<hex>` with a hex decoding equal to `nonce`
    /// passes the structural gate in [`verify_nonce`].
    pub(crate) fn authorize(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
    }

    /// Revoke any previously authorized nonce. Subsequent OSC 133/633
    /// sequences cannot satisfy [`verify_nonce`] until `authorize` is
    /// called again.
    pub(crate) fn revoke(&mut self) {
        self.nonce = None;
    }

    /// Number of OSC 133/633 sequences silently dropped since this
    /// state was last reset or constructed. Exposed for host-side
    /// metrics / tamper audits.
    #[must_use]
    pub(crate) fn dropped_count(&self) -> u64 {
        self.dropped_count
    }

    /// Verify a claimed nonce against the authorized nonce.
    ///
    /// `params` is the OSC parameter slice as produced by the VTE parser
    /// (split on `;`). This method scans from index 2 onward for the
    /// first occurrence of `id=<hex>` and performs a constant-time
    /// comparison of the decoded bytes against the authorized nonce.
    ///
    /// Returns `true` iff:
    /// 1. A nonce was previously authorized, AND
    /// 2. The params contain an `id=<64-hex>` element, AND
    /// 3. The decoded bytes equal the authorized nonce under
    ///    constant-time compare.
    ///
    /// Returns `false` otherwise and increments the drop counter.
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
            true
        } else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            false
        }
    }

    /// Engine-consulting variant of [`Self::verify_nonce`] (#7994).
    ///
    /// Consults the [`aterm_policy::engine::PolicyEngine`] first with the
    /// matching `OSC 133` or `OSC 633` probe at the given `origin`.
    /// Behavior:
    ///
    /// * Engine matches a rule whose response is `Execute` → proceed to
    ///   the existing nonce check (a valid nonce is still required).
    /// * Engine matches a rule with any other response → return `false`
    ///   immediately and increment the drop counter. The engine has
    ///   explicitly denied shell integration from this origin.
    /// * Engine absent / falls through → defer entirely to the nonce
    ///   check (Release N backward-compat; design §6.3).
    ///
    /// `command` should be `133` or `633` to match the caller's dispatch
    /// family. The probe's subcommand byte is left wildcard — the engine
    /// can express `OSC 133` or `OSC 633` at the major-only granularity.
    pub(crate) fn verify_nonce_with_engine(
        &mut self,
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        command: u32,
        params: &[&[u8]],
    ) -> bool {
        // Build a minimal OSC probe. The first param is the subcommand
        // glyph (e.g. "A"); we use "A" as a representative — the engine's
        // OSC 133/633 rules normally match at the major level in the
        // standard profile.
        let subcommand = params
            .get(1)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("A")
            .to_owned();
        let seq = aterm_policy::selector::DispatchedSequence::osc(command, [subcommand]);
        let decision = super::policy_bridge::engine_decision(engine, &seq, origin);
        match decision {
            super::policy_bridge::BridgeDecision::Deny => {
                self.dropped_count = self.dropped_count.saturating_add(1);
                false
            }
            super::policy_bridge::BridgeDecision::Allow
            | super::policy_bridge::BridgeDecision::Fallback => self.verify_nonce(params),
        }
    }

    /// Provenance ceremony: lift this shell-integration authorization
    /// state into a [`aterm_provenance::HostAuthorizationToken`] borrowed
    /// for the auth state's lifetime.
    ///
    /// Part of the #8001 `authorize_*` wiring (design §6 migration table).
    /// Callers must first pass [`verify_nonce`] for the current OSC
    /// 133/633 dispatch — this method is a pure borrow, so the caller is
    /// responsible for the ordering. (The design trade-off is documented
    /// in the #8001 design caveat list: a nonce-bound
    /// `AuthorizedShellIntegration<'_>` wrapper was considered but would
    /// require plumbing a new borrow through `handler_osc`. The existing
    /// verify-then-mint pattern matches `ClipboardAuth`.)
    ///
    /// The returned token is borrowed against `&self`, so it cannot
    /// outlive the OSC 133/633 dispatch frame.
    #[allow(
        dead_code,
        reason = "audit-only provenance ceremony retained until production callers consume the host-authorization token directly"
    )]
    #[must_use]
    pub(crate) fn as_host_auth_token(&self) -> aterm_provenance::HostAuthorizationToken<'_> {
        let _ = &self.nonce;
        aterm_provenance::HostAuthorizationToken::__new_for_capability_only()
    }
}

/// Scan OSC parameters from index 2 onward for an `id=<hex>` tag and
/// return the hex slice (without the `id=` prefix).
///
/// Returns the hex bytes of the *first* `id=` occurrence. Subsequent
/// `id=` tags are ignored — this matches how VS Code's OSC 633 parser
/// handles repeated keys.
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
/// Bitwise-OR-accumulates all XOR differences, then checks the sum at
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
        // Saturating_add is correctness insurance; show it does not panic
        // on overflow. Directly write near-MAX to exercise the guard
        // without iterating 2^64 times.
        let mut auth = ShellIntegrationAuth::new();
        auth.dropped_count = u64::MAX - 1;
        let params: &[&[u8]] = &[b"133", b"A"];
        assert!(!auth.verify_nonce(params));
        assert_eq!(auth.dropped_count, u64::MAX);
        // Next call must not panic — saturating_add caps at MAX.
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

    /// #8001 ceremony: the `ShellIntegrationAuth` auth state exposes an
    /// `as_host_auth_token` that lifts `Provenance<_, Pty>` to
    /// `Provenance<_, Host>`. The caller must have verified the nonce
    /// before calling this — the token is purely an audit marker; the
    /// structural check is the presence of `&ShellIntegrationAuth`.
    #[test]
    fn as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let mut auth = ShellIntegrationAuth::new();
        auth.authorize(nonce_with_byte(0x42));
        let tok = auth.as_host_auth_token();
        let pty: Provenance<String, aterm_provenance::Pty> =
            Provenance::from_pty("prompt".to_string());
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref(), "prompt");
    }
}
