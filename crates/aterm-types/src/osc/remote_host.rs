// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Remote host information from OSC 1337.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Remote host information from OSC 1337 RemoteHost.
///
/// Tracks the current SSH session host as reported by shells via the
/// OSC 1337 RemoteHost=user@hostname sequence.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHost {
    /// Username on the remote host.
    pub user: String,
    /// Fully-qualified hostname.
    pub hostname: String,
}

impl RemoteHost {
    /// Parse "user@hostname" format.
    ///
    /// Returns `None` if the format is invalid:
    /// - Missing `@` symbol
    /// - Empty user (starts with `@`)
    /// - Empty hostname (ends with `@`)
    ///
    /// If multiple `@` symbols are present, the first one is used as the
    /// delimiter (e.g., "user@host@domain" -> user="user", hostname="host@domain").
    pub fn parse(value: &str) -> Option<Self> {
        // `split_once('@')` is byte-for-byte equivalent to the old
        // find-then-slice: it splits on the FIRST '@', excludes the delimiter,
        // and an empty user/hostname reproduces the old `at_pos == 0` /
        // `at_pos == len - 1` rejects — with no manual index arithmetic for the
        // Trust gate to discharge.
        let (user, hostname) = value.split_once('@')?;
        if user.is_empty() || hostname.is_empty() {
            return None;
        }
        Some(Self {
            user: user.to_string(),
            hostname: hostname.to_string(),
        })
    }
}

impl std::fmt::Display for RemoteHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Trust gate: `write_str` instead of `write!` — runtime-argument
        // `format_args!` cannot be lowered natively. Byte-identical: the
        // nested `{}` never inherits `f`'s flags, and `str`'s `Display` with
        // default options is a plain `write_str`.
        f.write_str(&self.user)?;
        f.write_str("@")?;
        f.write_str(&self.hostname)
    }
}
