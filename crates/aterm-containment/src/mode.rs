// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Containment mode enum — the 4 security levels.
//!
//! Level encoding: Master=3, User=2, Safety=1, Containment=0 (pinned by the
//! tests below). Higher value = more capability. Non-escalation means mode can
//! only decrease or stay the same.

use std::fmt;

/// The 4 containment modes, ordered by decreasing capability.
///
/// What each mode actually enforces is stated once, in [`crate::actuator`]; the
/// crate-root table summarises it.
///
/// - **Master**: Full trust — developer mode. No confinement.
/// - **User**: The default. No confinement; the shell keeps the launching
///   shell's resource limits.
/// - **Safety**: Hardened resource limits (rlimits; the Job Object on Windows)
///   and no OS sandbox.
/// - **Containment**: Hostile agent. The macOS Seatbelt sandbox: no network,
///   writes confined to the temp roots, no read or write of the credential and
///   private-data stores, plus the hardened resource limits. Refuses to start
///   where no OS sandbox exists.
///
/// Mode is set ONLY by the launcher (its `--containment` / `--sandbox` /
/// `--no-sandbox` flag). aterm cannot upgrade its own mode. Mode is immutable
/// after initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ContainmentMode {
    /// Hostile agent — the OS-sandboxed mode; see [`crate::actuator`].
    Containment = 0,
    /// Hardened resource limits, no OS sandbox.
    Safety = 1,
    /// Normal usage — standard safeguards.
    User = 2,
    /// Full trust — developer mode.
    Master = 3,
}

impl ContainmentMode {
    /// Numeric capability level. Higher = more capability.
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }

    /// The variant's canonical name — identical to its `Display` rendering.
    /// Lets `Display` (and the audit trail) format through a single static
    /// string write instead of the `write!`/`format_args!` machinery (the
    /// aterm-cap `Tier::name` Trust idiom).
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Master => "Master",
            Self::User => "User",
            Self::Safety => "Safety",
            Self::Containment => "Containment",
        }
    }

    /// Parse from string (case-insensitive). Used for flag parsing.
    #[must_use]
    pub(crate) fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "master" => Some(Self::Master),
            "user" => Some(Self::User),
            "safety" => Some(Self::Safety),
            "containment" => Some(Self::Containment),
            _ => None,
        }
    }
}

impl fmt::Display for ContainmentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `write_str` instead of `write!`: byte-identical (the literal is the
        // whole message), and it keeps the `format_args!` expansion — whose
        // lowering the Trust strict gate cannot always model — out of this MIR.
        f.write_str(self.name())
    }
}

impl PartialOrd for ContainmentMode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ContainmentMode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.level().cmp(&other.level())
    }
}

/// Error when parsing a containment mode from a string.
#[derive(Debug, Clone)]
pub struct ParseModeError(pub(crate) String);

// Hand-written `Display`/`Error` (was `#[derive(aterm_error::Error)]` with
// `#[error("invalid containment mode: {0:?} (expected: master, user, safety,
// containment)")]`): the derive's generated `fmt` expands a runtime-argument
// `format_args!`, whose unsafe `fmt::Arguments::new` constructor the Trust
// strict gate's native lowering fails closed on. `Debug::fmt(&self.0, f)` is
// byte-identical to the `{0:?}` placeholder (str's `Debug` consults no
// formatter options), and the piece writes are verbatim, so the rendered
// message is unchanged. `source()` returns `None` exactly like the derive
// (no `#[source]`/`#[from]` field).
impl fmt::Display for ParseModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid containment mode: ")?;
        fmt::Debug::fmt(&self.0, f)?;
        f.write_str(" (expected: master, user, safety, containment)")
    }
}

impl std::error::Error for ParseModeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl std::str::FromStr for ContainmentMode {
    type Err = ParseModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_loose(s).ok_or_else(|| ParseModeError(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_encoding() {
        assert_eq!(ContainmentMode::Containment.level(), 0);
        assert_eq!(ContainmentMode::Safety.level(), 1);
        assert_eq!(ContainmentMode::User.level(), 2);
        assert_eq!(ContainmentMode::Master.level(), 3);
    }

    #[test]
    fn test_ordering() {
        assert!(ContainmentMode::Master > ContainmentMode::User);
        assert!(ContainmentMode::User > ContainmentMode::Safety);
        assert!(ContainmentMode::Safety > ContainmentMode::Containment);
    }

    #[test]
    fn test_parse() {
        assert_eq!(
            "master".parse::<ContainmentMode>().unwrap(),
            ContainmentMode::Master
        );
        assert_eq!(
            "SAFETY".parse::<ContainmentMode>().unwrap(),
            ContainmentMode::Safety
        );
        assert_eq!(
            "User".parse::<ContainmentMode>().unwrap(),
            ContainmentMode::User
        );
        assert!("invalid".parse::<ContainmentMode>().is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(ContainmentMode::Master.to_string(), "Master");
        assert_eq!(ContainmentMode::Containment.to_string(), "Containment");
    }

    /// Verify parse rejects inputs that could bypass mode selection.
    /// An attacker controlling the `--containment` value might try numeric
    /// values, padding, or similar to escalate.
    #[test]
    fn test_parse_rejects_bypass_attempts() {
        // Numeric values (the level encoding) must not be accepted
        assert!("0".parse::<ContainmentMode>().is_err());
        assert!("1".parse::<ContainmentMode>().is_err());
        assert!("2".parse::<ContainmentMode>().is_err());
        assert!("3".parse::<ContainmentMode>().is_err());

        // Padding / whitespace
        assert!(" master".parse::<ContainmentMode>().is_err());
        assert!("master ".parse::<ContainmentMode>().is_err());
        assert!("master\n".parse::<ContainmentMode>().is_err());

        // Substring / prefix
        assert!("mast".parse::<ContainmentMode>().is_err());
        assert!("contain".parse::<ContainmentMode>().is_err());

        // Empty
        assert!("".parse::<ContainmentMode>().is_err());

        // Null byte injection
        assert!("master\0".parse::<ContainmentMode>().is_err());
    }

    /// Verify that all 4 display strings round-trip through parse.
    #[test]
    fn test_display_parse_roundtrip() {
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ] {
            let s = mode.to_string();
            let parsed: ContainmentMode = s.parse().unwrap();
            assert_eq!(parsed, mode, "roundtrip failed for {mode}");
        }
    }

    /// Verify ParseModeError includes the rejected input for diagnostics.
    #[test]
    fn test_parse_error_includes_input() {
        let err = "bogus".parse::<ContainmentMode>().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bogus"),
            "error message should include rejected input, got: {msg}"
        );
    }
}
