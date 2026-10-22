// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 133 / OSC 633 shell-integration capability-nonce authority.
//!
//! A file piped through the PTY can write a full OSC 133 A/B/C/D cycle (or the OSC 633
//! variants) and forge prompt marks, exit statuses and prompt callbacks. So emissions can
//! be required to carry a nonce committed out of band: the host generates a 32-byte CSPRNG
//! nonce, installs it via [`super::Terminal::authorize_shell_integration`], and passes it to
//! the shell (`ATERM_SHELL_NONCE`), whose preamble adds `id=<64-hex>` to every OSC 133/633
//! sequence.
//!
//! With [`aterm_types::TerminalModes::require_shell_integration_nonce`] set, every A/B/C/D
//! (and 633 E/F/G/H/P) without a matching `id=` is dropped: no callback, no state change,
//! no response. `id=` may sit at any parameter index from 2 (OSC 633 sub-ops carry payload
//! ahead of it); hex is case-insensitive and compared in constant time. The requirement
//! defaults to off so third-party integrations keep working; hosts that ship a nonced
//! preamble turn it on at session init.

/// Host-side authorization state for the OSC 133/633 capability nonce. Handlers reach it
/// only through the `shell_integration_auth` borrow slot, and use only
/// [`ShellIntegrationAuth::verify_nonce`].
#[derive(Default, Debug)]
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

    /// [`Self::authorize`] for a nonce the shell has not taken yet (`awaiting_first_mark`):
    /// it verifies marks now and is in use from the first mark that carries it.
    #[inline]
    pub(crate) const fn authorize_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.nonce = Some(nonce);
        self.awaiting_first_mark = true;
        self.typed_prior = None;
    }

    /// [`Self::authorize_on_first_mark`] for a typed re-key, which can be taken back: the
    /// authorization it replaces is kept (`typed_prior`) until the host settles the key. A
    /// second typed re-key before the first is settled keeps the first one's prior.
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

    /// Take back the typed re-key `nonce`, restoring the authorization it replaced, so a key
    /// whose line never ran authorizes nothing. `false`, changing nothing, when `nonce` is
    /// not the pending typed re-key: a mark already carried it (a working key is never
    /// moved), another authorization replaced it, or it was settled.
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

    /// Keep the typed re-key `nonce`: its file was consumed, so the shell holds the key, and
    /// the authorization it replaced is forgotten. The key stays not-in-use until its first
    /// mark, as after [`Self::authorize_on_first_mark`]. `false`, changing nothing, when
    /// `nonce` is not the pending typed re-key.
    #[inline]
    pub(crate) fn settle_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        if !self.rekey_pending(nonce) {
            return false;
        }
        self.typed_prior = None;
        true
    }

    /// The authorized nonce once the shell uses it: always for one set by [`Self::authorize`]
    /// (at spawn or from a handoff carry), and for one set by
    /// [`Self::authorize_on_first_mark`] only after a mark signed with it arrived. This is
    /// what `status integration=` reports and what the handoff carry takes
    /// ([`super::Terminal::checkpoint_carry`]), so a re-key the shell never took is not
    /// carried.
    #[must_use]
    #[inline]
    pub(crate) const fn nonce_in_use(&self) -> Option<[u8; 32]> {
        if self.awaiting_first_mark {
            None
        } else {
            self.nonce
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

    /// Verify a claimed nonce: `true` iff a nonce is authorized and the first `id=<hex>` in
    /// `params` (split on `;`) from index 2 is 64 digits decoding to it (constant-time).
    /// Otherwise count a drop and return `false`.
    pub(crate) fn verify_nonce(&mut self, params: &[&[u8]]) -> bool {
        let Some(expected) = self.nonce.as_ref() else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        // Params 0 and 1 are the command number and subcommand; the nonce may sit at any
        // index from 2 (OSC 633 E/F/G/H/P carry payload ahead of it).
        let Some(claimed_hex) = find_nonce_hex(params) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        let Some(claimed) = decode_hex_32(claimed_hex) else {
            self.dropped_count = self.dropped_count.saturating_add(1);
            return false;
        };

        if aterm_digest::ct_eq(&claimed, expected) {
            // The shell has the key: a re-key it had not taken is in use now, and a typed
            // one is past taking back.
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
    /// ([`super::policy_gates::PolicyState::shell_integration_gate`]): `Deny` (a rule with
    /// a response other than `Execute`) drops and counts; `Allow` and `Fallback` (no policy
    /// or no matching rule) still require a valid nonce. The probe is keyed per subcommand
    /// ([`probe_subcommand`]), since a policy can name `"OSC 133;C"`.
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

/// The subcommand glyph the OSC 133/633 policy probe uses: `params[1]` when
/// it is valid UTF-8, `"A"` otherwise (including when absent). Shared with
/// [`super::policy_gates`] so the compiled table uses the same key.
#[must_use]
#[inline]
pub(super) fn probe_subcommand<'a>(params: &'a [&'a [u8]]) -> &'a str {
    params
        .get(1)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("A")
}

/// The OSC 133 / OSC 633 policy probe (`command` `133` or `633`, `subcommand` from
/// [`probe_subcommand`]): the one definition, shared by the compiled gate table and its
/// live fallback.
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
mod tests {
    use super::*;

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

    /// `verify_nonce` accepts only a 64-digit `id=` (either case) at parameter
    /// 2 or later that decodes to the authorized nonce, and every refusal
    /// counts one drop.
    #[test]
    fn verify_nonce_accepts_only_the_authorized_id() {
        let nonce: [u8; 32] = core::array::from_fn(|i| u8::try_from(i * 7).unwrap_or(0));
        let id = |hex: Vec<u8>| [b"id=".to_vec(), hex].concat();
        let ok = id(hex_of(&nonce));
        let upper = id(hex_of(&nonce).to_ascii_uppercase());
        let mut late = nonce;
        late[31] ^= 1;
        let late = id(hex_of(&late));
        let short = ok[..ok.len() - 1].to_vec();
        let mut not_hex = ok.clone();
        not_hex[3] = b'g';
        let cases: [(&str, bool, &[&[u8]], bool); 9] = [
            ("the authorized id", true, &[b"133", b"A", &ok], true),
            ("upper-case hex", true, &[b"133", b"A", &upper], true),
            (
                "after a 633 E payload",
                true,
                &[b"633", b"E", b"ls -la", &ok],
                true,
            ),
            ("none authorized", false, &[b"133", b"A", &ok], false),
            ("the last byte differs", true, &[b"133", b"A", &late], false),
            ("no id", true, &[b"133", b"A"], false),
            ("63 digits", true, &[b"133", b"A", &short], false),
            ("a non-hex digit", true, &[b"133", b"A", &not_hex], false),
            ("id in the subcommand slot", true, &[b"133", &ok], false),
        ];
        for (name, authorized, params, want) in cases {
            let mut auth = ShellIntegrationAuth::new();
            if authorized {
                auth.authorize(nonce);
            }
            assert_eq!(auth.verify_nonce(params), want, "{name}");
            assert_eq!(auth.dropped_count(), u64::from(!want), "{name}: drops");
        }
    }
}
