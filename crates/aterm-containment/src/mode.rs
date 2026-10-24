// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The three containment levels.

use std::fmt;

/// The 3 containment modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ContainmentMode {
    /// Hostile agent — the OS-sandboxed mode; see [`crate::actuator`].
    Containment = 0,
    /// Resource limits (soft on POSIX), no OS sandbox.
    Safety = 1,
    /// Normal usage — standard safeguards.
    User = 2,
}

impl ContainmentMode {
    /// Numeric capability level. Higher = more capability.
    #[inline]
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }

    /// The variant's canonical name.
    #[inline]
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Safety => "Safety",
            Self::Containment => "Containment",
        }
    }

    /// Parse a mode name, case-insensitively.
    #[inline]
    #[must_use]
    pub(crate) fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "safety" => Some(Self::Safety),
            "containment" => Some(Self::Containment),
            _ => None,
        }
    }
}

impl fmt::Display for ContainmentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
///
/// Its `Display` and `Error` are hand-written: no runtime-argument `format_args!`, which the
/// Trust strict gate fails closed on.
#[derive(Debug, Clone)]
pub struct ParseModeError(pub(crate) String);

impl fmt::Display for ParseModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid containment mode: ")?;
        fmt::Debug::fmt(&self.0, f)?;
        f.write_str(" (expected: user, safety, containment)")
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
    fn test_ordering() {
        assert!(ContainmentMode::User > ContainmentMode::Safety);
        assert!(ContainmentMode::Safety > ContainmentMode::Containment);
    }

    #[test]
    fn test_parse_rejects_bypass_attempts() {
        assert!("0".parse::<ContainmentMode>().is_err());
        assert!("1".parse::<ContainmentMode>().is_err());
        assert!("2".parse::<ContainmentMode>().is_err());

        assert!(" user".parse::<ContainmentMode>().is_err());
        assert!("user ".parse::<ContainmentMode>().is_err());
        assert!("user\n".parse::<ContainmentMode>().is_err());

        assert!("use".parse::<ContainmentMode>().is_err());
        assert!("contain".parse::<ContainmentMode>().is_err());

        assert!("".parse::<ContainmentMode>().is_err());

        assert!("user\0".parse::<ContainmentMode>().is_err());
    }

    #[test]
    fn test_display_parse_roundtrip() {
        for mode in [
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ] {
            let s = mode.to_string();
            let parsed: ContainmentMode = s.parse().unwrap();
            assert_eq!(parsed, mode, "roundtrip failed for {mode}");
            let upper: ContainmentMode = s.to_ascii_uppercase().parse().unwrap();
            assert_eq!(upper, mode, "case-insensitive parse failed for {mode}");
        }
    }
}
