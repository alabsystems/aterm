// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC / escape-sequence **policy engine**: the data model, the three built-in profiles, the decision-tree engine ([`engine::PolicyEngine`]) and the token-bucket rate limiter ([`limits`]).

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

/// Policy schema version shipped by this crate.
pub const SCHEMA_VERSION: u32 = 1;

/// The policy engine's trust tier for [`Rule::origin_min`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "PascalCase"))]
#[non_exhaustive]
pub enum OriginTag {
    /// Host-minted: host application code, system config, cannot be forged by the PTY.
    Host,
    /// Persistent on-disk configuration (per-user `aterm.toml`, system `/etc/aterm/`).
    ConfigFile,
    /// Live user input (typed keys, clipboard paste explicitly initiated by the user).
    User,
    /// User-typed input that has passed the bracketed-paste / shell-prompt gate.
    UserTyped,
    /// Local AI agent acting on the user's behalf (e.g. AI Assistant, AI Model).
    Ai,
    /// Bytes from the PTY slave whose shape is structurally safe (well-formed ASCII / UTF-8 from an expected-well-behaved command).
    PtySafe,
    /// Bytes from the PTY slave with no shape guarantees.
    Pty,
    /// Explicitly network-originated, untrusted (SSH stdout, curl output).
    NetworkUntrusted,
}

impl OriginTag {
    /// Numeric rank used by [`Self::dominates`].
    #[must_use]
    #[inline]
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

    /// Returns `true` iff `self` dominates `required` — i.e. `self` is as trusted as or more trusted than `required`.
    #[must_use]
    #[inline]
    pub const fn dominates(self, required: OriginTag) -> bool {
        self.trust_rank() <= required.trust_rank()
    }
}

/// Built-in policy profile selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "PascalCase"))]
#[non_exhaustive]
pub enum Profile {
    /// xterm-compatible, `unmatched = Execute`.
    Permissive,
    /// Default for interactive sessions.
    Standard,
    /// Maximum restriction.
    Hardened,
}

impl Profile {
    /// Return the canonical PascalCase profile name (`"Permissive"`, `"Standard"`, `"Hardened"`).
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permissive => "Permissive",
            Self::Standard => "Standard",
            Self::Hardened => "Hardened",
        }
    }

    /// Return the numeric strictness rank (`0 = Permissive`, `1 = Standard`, `2 = Hardened`).
    #[must_use]
    #[inline]
    pub const fn strictness_rank(self) -> u8 {
        match self {
            Self::Permissive => 0,
            Self::Standard => 1,
            Self::Hardened => 2,
        }
    }

    /// Return `true` when `self` is at least as strict as `other`.
    #[must_use]
    #[inline]
    pub const fn is_at_least_as_strict_as(self, other: Self) -> bool {
        self.strictness_rank() >= other.strictness_rank()
    }
}

/// Policy decision returned by [`engine::PolicyEngine::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Response {
    /// Silently drop the sequence.
    Drop,
    /// Log + drop.
    Warn,
    /// Proceed to the handler as if no policy were in effect.
    Execute,
    /// Delegate to the host for user consent.
    Ask,
    /// Apply a built-in rewrite (e.g. strip control bytes from an OSC 52 set).
    Rewrite,
}

impl Response {
    /// Return the canonical snake_case response name.
    #[must_use]
    #[inline]
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
    #[inline]
    pub const fn allows_execution(self) -> bool {
        matches!(self, Self::Execute | Self::Rewrite)
    }

    /// Return `true` when this response drops the sequence (`Drop` or `Warn`).
    #[must_use]
    #[inline]
    pub const fn is_drop(self) -> bool {
        matches!(self, Self::Drop | Self::Warn)
    }
}

/// Policy defaults: fallthrough behavior when no rule matches.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Defaults {
    /// Response used when evaluation falls through every rule.
    pub unmatched: Response,
    /// Whether OSC 133 / OSC 633 shell-integration sequences require a 64-hex-digit nonce.
    #[cfg_attr(feature = "toml", serde(default))]
    pub shell_integration_require_nonce: bool,
}

/// One rule in a [`Policy`] document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Rule {
    /// Sequence selector in its raw string form; compiled by [`selector::SequenceSelector::parse`].
    pub sequence: String,
    /// Minimum acceptable origin.
    pub origin_min: OriginTag,
    /// What to do with matched + origin-admitted dispatches.
    pub response: Response,
    /// Named rate-limit reference.
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

/// Named token-bucket configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct RateLimit {
    /// Identifier referenced by [`Rule::rate_limit`].
    pub id: String,
    /// Maximum burst size in bytes.
    pub capacity_bytes: u32,
    /// Steady-state refill rate in bytes per second.
    pub refill_per_second: u32,
    /// Hard cap on a single sequence's consumption, independent of the bucket's current level.
    #[cfg_attr(feature = "toml", serde(default))]
    pub per_sequence_max: u32,
}

impl RateLimit {
    /// Encode rate-limit numeric parameters (`capacity_bytes`, `refill_per_second`, `per_sequence_max`, `schema_version`) into 16 little-endian bytes.
    #[must_use]
    #[inline]
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
    #[inline]
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

/// Root policy document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(deny_unknown_fields))]
pub struct Policy {
    /// Schema version.
    pub schema_version: u32,
    /// Profile tag.
    pub profile: Profile,
    /// Fallthrough defaults for unmatched dispatches.
    pub defaults: Defaults,
    /// Ordered rule list.
    #[cfg_attr(feature = "toml", serde(default))]
    pub rules: Vec<Rule>,
    /// Named rate-limit table.
    #[cfg_attr(feature = "toml", serde(default))]
    pub rate_limits: Vec<RateLimit>,
}

#[cfg(feature = "toml")]
impl Policy {
    /// Serialize this policy as a TOML string.
    /// # Errors
    /// Returns [`aterm_toml::ser::Error`] if the policy contains a shape the TOML serializer cannot represent.
    #[inline]
    pub fn to_toml(&self) -> Result<String, aterm_toml::ser::Error> {
        aterm_toml::to_string(self)
    }
}
