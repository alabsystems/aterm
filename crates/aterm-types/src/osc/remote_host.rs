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
