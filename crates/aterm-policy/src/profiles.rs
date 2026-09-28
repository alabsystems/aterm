// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Built-in policy profiles.
//!
//! Each profile is a complete [`Policy`](crate::Policy) document that ships
//! inside the crate. Hosts load one at startup.
//!
//! The three profiles form a refinement chain `Hardened ⊆ Standard ⊆
//! Permissive`. The test-only `refinement::response_rank` helper in this module
//! provides the numeric ordering the refinement tests assert the chain with —
//! the rank is *not* authoritative policy semantics.
//!
//! ## Builder helpers
//!
//! The three public constructors are `permissive`, `standard`, `hardened`.
//! Each is `pub fn -> Policy` and is the only supported way to obtain a
//! profile in Phase 0. Tests and external callers should clone the returned
//! value rather than mutating it in place.

use crate::{Defaults, OriginTag, Policy, Profile, RateLimit, Response, Rule, SCHEMA_VERSION};

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
    // Mirrors the `ResponseRateLimiter` defaults: 100 KiB/s refill, and a burst
    // sized on the WIRE form of the largest legitimate response. Tokens are
    // *bytes* written via `send_response`.
    //
    // The old comment here read "a single legitimate response never exceeds
    // `capacity_bytes` by construction because `MAX_OSC52_QUERY_RESPONSE_BYTES
    // = 64 KiB` in the handler", and it was false in a way that cost real
    // answers: that cap is on the DECODED clipboard, and the response is
    // base64, which expands 3 bytes to 4. So a clipboard of 49,145..65,536
    // bytes passed the handler's cap, produced a >64 KiB wire response, and was
    // dropped here without a byte being sent — `try_consume` refuses anything
    // larger than capacity outright, however full the bucket is. The capacity
    // now holds base64(64 KiB) plus framing.
    //
    // `aterm-core`'s `osc52_query_answers_every_clipboard_it_admits` pins the
    // relationship from the other side of this crate boundary, so the two
    // numbers cannot drift apart again in silence.
    RateLimit {
        id: "response".to_owned(),
        capacity_bytes: 96 * 1024,
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

/// Refinement helpers for the `Hardened ⊆ Standard ⊆ Permissive` invariant:
/// the scalar rank the refinement tests (`tests.rs`) use to catch an inversion.
#[cfg(test)]
pub(crate) mod refinement {
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
