// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Built-in policy profiles (§7 of the design).
//!
//! Each profile is a complete [`Policy`](crate::Policy) document that ships
//! inside the crate. Hosts load one at startup; the engine (#7992) may then
//! apply an operator override.
//!
//! The three profiles form a refinement chain `Hardened ⊆ Standard ⊆
//! Permissive` (§4.5). The [`refinement::response_rank`] helper in this module
//! provides the numeric ordering used by the tests that assert the chain — the
//! rank is *not* authoritative policy semantics; it is a scaffold for the
//! Kani `policy_refinement.rs` harness landing in #7998.
//!
//! ## Builder helpers
//!
//! The three public constructors are `permissive`, `standard`, `hardened`.
//! Each is `pub fn -> Policy` and is the only supported way to obtain a
//! profile in Phase 0. Tests and external callers should clone the returned
//! value rather than mutating it in place.

use crate::{Defaults, OriginTag, Policy, Profile, RateLimit, Response, Rule, SCHEMA_VERSION};

/// Return whether the Hardened built-in profile has an OSC rule for `major`.
///
/// Kept as a small allocation-free mirror of [`hardened`] for symbolic proof
/// harnesses that need to avoid pulling selector string parsing into the
/// checked trace.
#[must_use]
#[cfg(any(kani, test))]
pub(crate) const fn hardened_covers_osc_major(major: u32) -> bool {
    matches!(major, 4 | 9 | 52 | 99 | 777)
}

/// Return the Hardened built-in profile's unmatched fallback response.
///
/// This mirrors [`hardened`] without allocating the full profile document, so
/// symbolic proof harnesses can validate fail-closed defaults without pulling
/// selector parsing or collection construction into the checked trace.
#[must_use]
#[cfg(any(kani, test))]
pub(crate) const fn hardened_unmatched_response() -> Response {
    Response::Drop
}

/// Return the Standard built-in profile's unmatched fallback response.
///
/// Allocation-free mirror of [`standard`]`().defaults.unmatched`, same rationale
/// as [`hardened_unmatched_response`]: lets the monotonicity proof compare the
/// three built-ins' fallbacks without pulling `String`/`Vec`/`Option<&str>` rule
/// construction into the model-checked trace. Kept honest by
/// `unmatched_response_accessors_mirror_builtin_defaults`.
#[must_use]
#[cfg(any(kani, test))]
pub(crate) const fn standard_unmatched_response() -> Response {
    Response::Warn
}

/// Return the Permissive built-in profile's unmatched fallback response.
///
/// Allocation-free mirror of [`permissive`]`().defaults.unmatched`; see
/// [`hardened_unmatched_response`]. Guarded against drift by
/// `unmatched_response_accessors_mirror_builtin_defaults`.
#[must_use]
#[cfg(any(kani, test))]
pub(crate) const fn permissive_unmatched_response() -> Response {
    Response::Execute
}

/// Return the Hardened response for an OSC major in allocation-free proof code.
///
/// Covered majors return a conservative non-default placeholder because the
/// full selector parameters and origin gate decide the exact rule result.
/// Uncovered majors return the unmatched default directly.
#[must_use]
#[cfg(any(kani, test))]
#[inline(never)]
pub(crate) fn hardened_osc_response_for_proof(major: u32) -> Response {
    if hardened_covers_osc_major(major) {
        Response::Execute
    } else {
        hardened_unmatched_response()
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn rule(
    sequence: &str,
    origin_min: OriginTag,
    response: Response,
    rate_limit: Option<&str>,
    prompt_id: Option<&str>,
) -> Rule {
    Rule {
        sequence: sequence.to_owned(),
        origin_min,
        response,
        rate_limit: rate_limit.map(str::to_owned),
        prompt_id: prompt_id.map(str::to_owned),
    }
}

fn clipboard_limit() -> RateLimit {
    RateLimit {
        id: "clipboard".to_owned(),
        capacity_bytes: 16_384,
        refill_per_second: 1_024,
        per_sequence_max: 65_536,
    }
}

fn notifications_limit() -> RateLimit {
    RateLimit {
        id: "notifications".to_owned(),
        capacity_bytes: 16,
        refill_per_second: 1,
        per_sequence_max: 256,
    }
}

fn palette_limit() -> RateLimit {
    // Mirrors the pre-#7995 hardcoded `MAX_RESPONSES_PER_SEQUENCE = 16` from
    // `handler_osc_color.rs` (#7883). Tokens represent response *pairs*
    // emitted by an OSC 4 / OSC 21 query (not bytes). `per_sequence_max = 16`
    // is the single-sequence cap that prevents a 256-index query from
    // amplifying into ~5 KiB of PTY back-pressure; `capacity_bytes = 64` /
    // `refill_per_second = 16` keep a modest cross-sequence throttle so a
    // loop of well-formed 16-pair queries cannot saturate the PTY either.
    RateLimit {
        id: "palette".to_owned(),
        capacity_bytes: 64,
        refill_per_second: 16,
        per_sequence_max: 16,
    }
}

fn response_limit() -> RateLimit {
    // Mirrors the pre-#7995 `ResponseRateLimiter` defaults (commit
    // 2134b5559): 64 KiB burst, 100 KiB/s refill. Tokens represent *bytes*
    // written via `send_response`. No per-sequence cap — a single
    // legitimate response never exceeds `capacity_bytes` by construction
    // because `MAX_OSC52_QUERY_RESPONSE_BYTES = 64 KiB` in the handler.
    RateLimit {
        id: "response".to_owned(),
        capacity_bytes: 64 * 1024,
        refill_per_second: 100 * 1024,
        per_sequence_max: 0,
    }
}

// ---------------------------------------------------------------------------
// Permissive (xterm-compatible)
// ---------------------------------------------------------------------------

/// xterm-compatible baseline. `unmatched = Execute`, no rules narrow anything
/// except the clipboard-set rate limiter (kept for DoS protection).
///
/// Not recommended for production; shipped for legacy integration testing
/// (§7.1).
#[must_use]
pub fn permissive() -> Policy {
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Permissive,
        defaults: Defaults {
            unmatched: Response::Execute,
            shell_integration_require_nonce: false,
        },
        rules: vec![
            // Even the permissive profile keeps the response rate limiter —
            // it's a pure DoS mitigation, not a security gate.
            rule(
                "response any",
                OriginTag::NetworkUntrusted,
                Response::Execute,
                Some("clipboard"),
                None,
            ),
        ],
        rate_limits: vec![clipboard_limit(), response_limit()],
    }
}

// ---------------------------------------------------------------------------
// Standard (interactive default)
// ---------------------------------------------------------------------------

/// Default for interactive sessions (§7.2).
///
/// - Clipboard set: `Ask` (host prompt).
/// - Clipboard query: `Drop`.
/// - Notifications: `Warn` + rate limited.
/// - Palette reconfigure: `Execute` from `Ai` or higher; `Drop` from `Pty`.
/// - Window ops 1-10: `Drop`; 11-21 / 22-23: `Execute`.
/// - Shell integration nonce: required.
/// - Modal protocols: `Host` only.
/// - `unmatched = Warn`.
#[must_use]
pub fn standard() -> Policy {
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: Defaults {
            unmatched: Response::Warn,
            shell_integration_require_nonce: true,
        },
        rules: vec![
            rule(
                "OSC 52 set",
                OriginTag::User,
                Response::Ask,
                Some("clipboard"),
                Some("clipboard-write"),
            ),
            rule("OSC 52 query", OriginTag::Host, Response::Drop, None, None),
            rule(
                "OSC 4 query",
                OriginTag::PtySafe,
                Response::Execute,
                Some("palette"),
                None,
            ),
            rule(
                "OSC 4 set",
                OriginTag::Ai,
                Response::Execute,
                Some("palette"),
                None,
            ),
            rule(
                "OSC 21 set named",
                OriginTag::ConfigFile,
                Response::Execute,
                Some("palette"),
                None,
            ),
            rule("CSI t", OriginTag::Host, Response::Drop, None, None),
            rule(
                "OSC 9",
                OriginTag::User,
                Response::Warn,
                Some("notifications"),
                None,
            ),
            rule(
                "OSC 99",
                OriginTag::User,
                Response::Warn,
                Some("notifications"),
                None,
            ),
            rule(
                "OSC 777",
                OriginTag::User,
                Response::Warn,
                Some("notifications"),
                None,
            ),
            rule("DCS 2000p", OriginTag::Host, Response::Execute, None, None),
            rule(
                "response any",
                OriginTag::NetworkUntrusted,
                Response::Execute,
                Some("clipboard"),
                None,
            ),
        ],
        rate_limits: vec![
            clipboard_limit(),
            notifications_limit(),
            palette_limit(),
            response_limit(),
        ],
    }
}

// ---------------------------------------------------------------------------
// Hardened (maximum restriction)
// ---------------------------------------------------------------------------

/// Maximum restriction (§7.3). Everything that could reach host state from
/// `Pty` is dropped; clipboard, modal protocols, and notifications are all
/// denied. Only essential responses (DA1, CPR) pass.
///
/// The Hardened profile is the **fail-closed fallback** loaded when a TOML
/// policy fails to parse (§4.4); tests in this crate exercise that path.
#[must_use]
pub fn hardened() -> Policy {
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Hardened,
        defaults: Defaults {
            unmatched: Response::Drop,
            shell_integration_require_nonce: true,
        },
        rules: vec![
            // Clipboard: drop both directions regardless of origin.
            rule("OSC 52 set", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 52 query", OriginTag::Host, Response::Drop, None, None),
            // Palette query is read-only; allow from ConfigFile or higher.
            rule(
                "OSC 4 query",
                OriginTag::ConfigFile,
                Response::Execute,
                Some("palette"),
                None,
            ),
            // Palette set requires ConfigFile origin (persistent, non-PTY).
            rule(
                "OSC 4 set",
                OriginTag::ConfigFile,
                Response::Execute,
                Some("palette"),
                None,
            ),
            // Window ops, notifications: drop.
            rule("CSI t", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 9", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 99", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 777", OriginTag::Host, Response::Drop, None, None),
            // Modal protocols: Host only.
            rule("DCS 2000p", OriginTag::Host, Response::Execute, None, None),
            rule("DCS 1000p", OriginTag::Host, Response::Execute, None, None),
            // Essential responses only (DA1/CPR); everything else drops via
            // `defaults.unmatched`. The `response any` rule below still fires
            // but with a Host-only origin gate, so Pty-origin response writes
            // fall through to the unmatched-drop default.
            rule(
                "response any",
                OriginTag::Host,
                Response::Execute,
                Some("clipboard"),
                None,
            ),
        ],
        rate_limits: vec![
            clipboard_limit(),
            notifications_limit(),
            palette_limit(),
            response_limit(),
        ],
    }
}

// ---------------------------------------------------------------------------
// Refinement scaffolding
// ---------------------------------------------------------------------------

/// Refinement helpers for the `Hardened ⊆ Standard ⊆ Permissive` invariant
/// (§4.5).
///
/// The real refinement proof lands in #7998 (Kani + TLA+). This module
/// provides the scalar rank used by the Phase 0 tests to guard against
/// accidental inversion while the engine is still being built.
pub mod refinement {
    use crate::Response;

    /// Map a response to a numeric "strictness" rank:
    ///
    /// * `Drop   = 4` (strictest — no host observable effect)
    /// * `Warn   = 3`
    /// * `Rewrite= 2`
    /// * `Ask    = 1`
    /// * `Execute= 0` (loosest — unrestricted)
    ///
    /// The refinement invariant is `rank(hardened.unmatched) >=
    /// rank(standard.unmatched) >= rank(permissive.unmatched)`.
    #[must_use]
    pub const fn response_rank(r: Response) -> u8 {
        match r {
            Response::Drop => 4,
            Response::Warn => 3,
            Response::Rewrite => 2,
            Response::Ask => 1,
            Response::Execute => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hardened, hardened_covers_osc_major, hardened_osc_response_for_proof,
        hardened_unmatched_response, permissive, permissive_unmatched_response, standard,
        standard_unmatched_response,
    };
    use crate::Response;

    #[test]
    fn hardened_osc_coverage_mirror_matches_builtin_rules() {
        for covered in [4, 9, 52, 99, 777] {
            assert!(hardened_covers_osc_major(covered));
        }

        for unknown in [0, 1, 2, 7, 8, 10, 11, 133, 633, 1337] {
            assert!(!hardened_covers_osc_major(unknown));
        }

        assert_eq!(hardened_unmatched_response(), Response::Drop);
        assert_eq!(hardened_osc_response_for_proof(200), Response::Drop);
        assert_eq!(hardened_osc_response_for_proof(52), Response::Execute);
    }

    /// Drift guard: the allocation-free `*_unmatched_response()` accessors used
    /// by `policy_monotonicity` MUST equal the corresponding built-in profile's
    /// real `defaults.unmatched`. If a profile's fallback is ever retuned, this
    /// fails until the accessor is updated — so the proof can never silently
    /// validate a stale constant instead of the shipped policy.
    #[test]
    fn unmatched_response_accessors_mirror_builtin_defaults() {
        assert_eq!(hardened_unmatched_response(), hardened().defaults.unmatched);
        assert_eq!(standard_unmatched_response(), standard().defaults.unmatched);
        assert_eq!(
            permissive_unmatched_response(),
            permissive().defaults.unmatched
        );
    }
}
