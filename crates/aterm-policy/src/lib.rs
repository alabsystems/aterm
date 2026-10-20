// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC / escape-sequence **policy engine**: the data model, the three built-in
//! profiles, the decision-tree engine ([`engine::PolicyEngine`]) and the
//! token-bucket rate limiter ([`limits`]).
//!
//! It is live: the window installs a `standard`-profile engine on every
//! session before its reader produces a byte (`aterm-gui` `spawn.rs`, via
//! `Terminal::apply_policy_engine`), and `aterm-core`'s `policy_bridge.rs` routes
//! the capability `try_mint` paths (responses, clipboard write/query, window ops,
//! shell integration) through [`engine::PolicyEngine::evaluate`]. On a matching
//! rule the engine's response is authoritative; on fallthrough the legacy
//! `TerminalModes::allow_*` bit is, so the two never need mirroring. The
//! `"response"` and `"palette"` rate limits of every built-in profile are the
//! authoritative source for the 64 KiB/100 KiB/s response bucket and the
//! 16-pair OSC 4 / OSC 21 per-sequence cap; with no engine installed the
//! handlers fall back to the legacy constants. A checkpoint deliberately does
//! not carry the policy — the host re-installs it after a restore (`aterm-core`
//! `terminal/checkpoint.rs`).
//!
//! ## Profile refinement invariant
//!
//! Every profile is a complete [`Policy`] document. The three built-ins
//! satisfy the ordering `Hardened ⊆ Standard ⊆ Permissive` over the
//! unmatched-default response, pinned by the refinement tests in `tests.rs`. See
//! [`profiles::permissive`], [`profiles::standard`], [`profiles::hardened`].
//!
//! # Example
//!
//! ```
//! use aterm_policy::profiles;
//!
//! let hardened = profiles::hardened();
//! assert_eq!(hardened.schema_version, aterm_policy::SCHEMA_VERSION);
//! assert_eq!(hardened.profile, aterm_policy::Profile::Hardened);
//! ```

#![warn(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(feature = "toml")]
use serde::{Deserialize, Serialize};

pub mod aliases;
pub mod engine;
pub mod limits;
pub mod profiles;
pub mod selector;

#[cfg(test)]
mod tests;

/// Policy schema version shipped by this crate. The reader rejects any value
/// other than this constant (§5.1 of the design).
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// OriginTag — the policy engine's trust lattice.
// ---------------------------------------------------------------------------

/// The policy engine's trust tier for [`Rule::origin_min`].
///
/// `NetworkUntrusted` being the bottom is load-bearing: the built-in profiles
/// use `origin_min = NetworkUntrusted` as the "allow from any origin" floor.
/// The order is pinned by exhaustive proofs in `tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "PascalCase"))]
#[non_exhaustive]
pub enum OriginTag {
    /// Host-minted: host application code, system config, cannot be forged by
    /// the PTY. Dominates every other origin.
    Host,
    /// Persistent on-disk configuration (per-user `aterm.toml`, system
    /// `/etc/aterm/`). Loaded before any PTY bytes flow.
    ConfigFile,
    /// Live user input (typed keys, clipboard paste explicitly initiated by
    /// the user).
    User,
    /// User-typed input that has passed the bracketed-paste / shell-prompt
    /// gate. Ranked just below `User`.
    UserTyped,
    /// Local AI agent acting on the user's behalf (e.g. AI Assistant, AI Model).
    Ai,
    /// Bytes from the PTY slave whose shape is structurally safe (well-formed
    /// ASCII / UTF-8 from an expected-well-behaved command). Still untrusted.
    PtySafe,
    /// Bytes from the PTY slave with no shape guarantees. The default for any
    /// byte whose provenance is unclear.
    Pty,
    /// Explicitly network-originated, untrusted (SSH stdout, curl output).
    /// Subordinate to every other origin.
    NetworkUntrusted,
}

impl OriginTag {
    /// Numeric rank used by [`Self::dominates`]. Lower number = more trusted.
    #[must_use]
    pub const fn trust_rank(self) -> u8 {
        match self {
            Self::Host => 0,
            Self::ConfigFile => 1,
            Self::User => 2,
            Self::UserTyped => 3,
            Self::Ai => 4,
            Self::PtySafe => 5,
            Self::Pty => 6,
            Self::NetworkUntrusted => 7,
        }
    }

    /// Returns `true` iff `self` dominates `required` — i.e. `self` is as
    /// trusted as or more trusted than `required`.
    ///
    /// This is the origin-gate check from design §4.2:
    ///
    /// ```text
    /// if selector_matches(rule.sequence, seq):
    ///     if origin.dominates(rule.origin_min):   // <-- this function
    ///         return rule.response
    /// ```
    #[must_use]
    pub const fn dominates(self, required: OriginTag) -> bool {
        self.trust_rank() <= required.trust_rank()
    }
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// Built-in policy profile selector.
///
/// Each variant names one of the three built-in policy documents in
/// [`profiles`]. The profile field is redundant with the rule set (the rules
/// *are* the profile) but the tag is carried so that the FFI surface and host
/// UIs can display the profile's common name and reason about refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "PascalCase"))]
#[non_exhaustive]
pub enum Profile {
    /// xterm-compatible, `unmatched = Execute`. Legacy testing only.
    Permissive,
    /// Default for interactive sessions. `unmatched = Warn`.
    Standard,
    /// Maximum restriction. `unmatched = Drop`, every sequence requires at
    /// least `Host | ConfigFile | User` origin.
    Hardened,
}

impl Profile {
    /// Return the canonical PascalCase profile name (`"Permissive"`, `"Standard"`, `"Hardened"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permissive => "Permissive",
            Self::Standard => "Standard",
            Self::Hardened => "Hardened",
        }
    }

    /// Return the numeric strictness rank (`0 = Permissive`, `1 = Standard`, `2 = Hardened`).
    #[must_use]
    pub const fn strictness_rank(self) -> u8 {
        match self {
            Self::Permissive => 0,
            Self::Standard => 1,
            Self::Hardened => 2,
        }
    }

    /// Return `true` when `self` is at least as strict as `other`.
    #[must_use]
    pub const fn is_at_least_as_strict_as(self, other: Self) -> bool {
        self.strictness_rank() >= other.strictness_rank()
    }
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// Policy decision returned by [`engine::PolicyEngine::evaluate`].
///
/// The `Ask` and `Rewrite` variants carry no inline payload in the TOML
/// schema; the referenced prompt is named by a sibling rule field
/// ([`Rule::prompt_id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Response {
    /// Silently drop the sequence. No host observable side effect.
    Drop,
    /// Log + drop. Visible in host metrics.
    Warn,
    /// Proceed to the handler as if no policy were in effect.
    Execute,
    /// Delegate to the host for user consent. See `Rule::prompt_id`.
    Ask,
    /// Apply a built-in rewrite (e.g. strip control bytes from an OSC 52 set).
    Rewrite,
}

impl Response {
    /// Return the canonical snake_case response name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Warn => "warn",
            Self::Execute => "execute",
            Self::Ask => "ask",
            Self::Rewrite => "rewrite",
        }
    }

    /// Return `true` when this response immediately permits execution (`Execute` or `Rewrite`).
    #[must_use]
    pub const fn allows_execution(self) -> bool {
        matches!(self, Self::Execute | Self::Rewrite)
    }

    /// Return `true` when this response drops the sequence (`Drop` or `Warn`).
    #[must_use]
    pub const fn is_drop(self) -> bool {
        matches!(self, Self::Drop | Self::Warn)
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Policy defaults: fallthrough behavior when no rule matches (§4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Defaults {
    /// Response used when evaluation falls through every rule.
    pub unmatched: Response,
    /// Whether OSC 133 / OSC 633 shell-integration sequences require a
    /// 64-hex-digit nonce. Mirrors
    /// `TerminalModes::require_shell_integration_nonce` (§6.4).
    #[cfg_attr(feature = "toml", serde(default))]
    pub shell_integration_require_nonce: bool,
}

// ---------------------------------------------------------------------------
// Rule
// ---------------------------------------------------------------------------

/// One rule in a [`Policy`] document.
///
/// Rules are evaluated top-to-bottom; the first rule whose `sequence` matches
/// the dispatched escape sequence AND whose `origin_min` is dominated by the
/// byte's origin wins. A rule that matches by selector but fails the origin
/// test does **not** short-circuit — evaluation continues with the next rule
/// (§4.2). This lets operators write fallback rules for the same selector.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Rule {
    /// Sequence selector in its raw string form; compiled by
    /// [`selector::SequenceSelector::parse`].
    pub sequence: String,
    /// Minimum acceptable origin. Origins that dominate this tag are admitted;
    /// subordinate origins fall through to the next rule.
    pub origin_min: OriginTag,
    /// What to do with matched + origin-admitted dispatches.
    pub response: Response,
    /// Named rate-limit reference. Resolved against [`Policy::rate_limits`] by
    /// id. `None` means "no rate limit on this rule".
    #[cfg_attr(
        feature = "toml",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub rate_limit: Option<String>,
    /// Prompt identifier, only meaningful when `response == Ask`.
    #[cfg_attr(
        feature = "toml",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub prompt_id: Option<String>,
}

// ---------------------------------------------------------------------------
// RateLimit
// ---------------------------------------------------------------------------

/// Named token-bucket configuration (§3.1).
///
/// Schema only; bucket state lives in the engine's [`limits::RateLimiterSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct RateLimit {
    /// Identifier referenced by [`Rule::rate_limit`].
    pub id: String,
    /// Maximum burst size in bytes. Must be > 0.
    pub capacity_bytes: u32,
    /// Steady-state refill rate in bytes per second. May be 0 for a
    /// one-shot-per-session bucket.
    pub refill_per_second: u32,
    /// Hard cap on a single sequence's consumption, independent of the
    /// bucket's current level. `0` means "no per-sequence cap".
    #[cfg_attr(feature = "toml", serde(default))]
    pub per_sequence_max: u32,
}

impl RateLimit {
    /// Encode rate-limit numeric parameters (`capacity_bytes`, `refill_per_second`,
    /// `per_sequence_max`, `schema_version`) into 16 little-endian bytes.
    #[must_use]
    pub const fn encode_params_le(
        capacity_bytes: u32,
        refill_per_second: u32,
        per_sequence_max: u32,
        schema_version: u32,
    ) -> [u8; 16] {
        [
            capacity_bytes as u8,
            (capacity_bytes >> 8) as u8,
            (capacity_bytes >> 16) as u8,
            (capacity_bytes >> 24) as u8,
            refill_per_second as u8,
            (refill_per_second >> 8) as u8,
            (refill_per_second >> 16) as u8,
            (refill_per_second >> 24) as u8,
            per_sequence_max as u8,
            (per_sequence_max >> 8) as u8,
            (per_sequence_max >> 16) as u8,
            (per_sequence_max >> 24) as u8,
            schema_version as u8,
            (schema_version >> 8) as u8,
            (schema_version >> 16) as u8,
            (schema_version >> 24) as u8,
        ]
    }

    /// Decode 16 little-endian bytes into rate-limit numeric parameters.
    #[must_use]
    pub const fn decode_params_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
        let cap = (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24);
        let refill = (bytes[4] as u32)
            | ((bytes[5] as u32) << 8)
            | ((bytes[6] as u32) << 16)
            | ((bytes[7] as u32) << 24);
        let max_seq = (bytes[8] as u32)
            | ((bytes[9] as u32) << 8)
            | ((bytes[10] as u32) << 16)
            | ((bytes[11] as u32) << 24);
        let ver = (bytes[12] as u32)
            | ((bytes[13] as u32) << 8)
            | ((bytes[14] as u32) << 16)
            | ((bytes[15] as u32) << 24);
        (cap, refill, max_seq, ver)
    }
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Root policy document (§3.1 + Appendix A).
///
/// The TOML document a policy file parses into (`Policy::to_toml`, behind the
/// `toml` feature, writes it back). Checkpoints deliberately omit it; the host
/// re-installs the engine after a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Policy {
    /// Schema version. Must equal [`SCHEMA_VERSION`]; the reader rejects any
    /// other value (§5.1).
    pub schema_version: u32,
    /// Profile tag. Informational; the effective behavior is the rule set.
    pub profile: Profile,
    /// Fallthrough defaults for unmatched dispatches.
    pub defaults: Defaults,
    /// Ordered rule list. First match (modulo origin fallthrough) wins.
    #[cfg_attr(feature = "toml", serde(default))]
    pub rules: Vec<Rule>,
    /// Named rate-limit table. Referenced by `Rule::rate_limit`.
    #[cfg_attr(feature = "toml", serde(default))]
    pub rate_limits: Vec<RateLimit>,
}

#[cfg(feature = "toml")]
impl Policy {
    /// Serialize this policy as a TOML string (§5.1).
    ///
    /// # Errors
    ///
    /// Returns [`aterm_toml::ser::Error`] if the policy contains a shape the TOML
    /// serializer cannot represent. No built-in profile does.
    pub fn to_toml(&self) -> Result<String, aterm_toml::ser::Error> {
        aterm_toml::to_string(self)
    }
}
