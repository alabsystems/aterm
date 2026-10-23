// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Built-in policy profiles.

use crate::{Defaults, OriginTag, Policy, Profile, RateLimit, Response, Rule, SCHEMA_VERSION};

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
    RateLimit {
        id: "palette".to_owned(),
        capacity_bytes: 64,
        refill_per_second: 16,
        per_sequence_max: 16,
    }
}

fn response_limit() -> RateLimit {
    RateLimit {
        id: "response".to_owned(),
        capacity_bytes: 96 * 1024,
        refill_per_second: 100 * 1024,
        per_sequence_max: 0,
    }
}

/// xterm-compatible baseline.
#[must_use]
pub fn permissive() -> Policy {
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Permissive,
        defaults: Defaults {
            unmatched: Response::Execute,
            shell_integration_require_nonce: false,
        },
        rules: vec![rule(
            "response any",
            OriginTag::NetworkUntrusted,
            Response::Execute,
            Some("clipboard"),
            None,
        )],
        rate_limits: vec![clipboard_limit(), response_limit()],
    }
}

/// Default for interactive sessions (§7.2).
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

/// Maximum restriction (§7.3).
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
            rule("OSC 52 set", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 52 query", OriginTag::Host, Response::Drop, None, None),
            rule(
                "OSC 4 query",
                OriginTag::ConfigFile,
                Response::Execute,
                Some("palette"),
                None,
            ),
            rule(
                "OSC 4 set",
                OriginTag::ConfigFile,
                Response::Execute,
                Some("palette"),
                None,
            ),
            rule("CSI t", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 9", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 99", OriginTag::Host, Response::Drop, None, None),
            rule("OSC 777", OriginTag::Host, Response::Drop, None, None),
            rule("DCS 2000p", OriginTag::Host, Response::Execute, None, None),
            rule("DCS 1000p", OriginTag::Host, Response::Execute, None, None),
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

/// Refinement helpers for the `Hardened ⊆ Standard ⊆ Permissive` invariant.
#[cfg(test)]
pub(crate) mod refinement {
    use crate::Response;

    /// Map a response to a numeric "strictness" rank, from `Drop` (4, strictest) down to
    /// `Execute` (0, loosest).
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
