// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Remote host information from OSC 1337.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Remote host from OSC 1337 `RemoteHost=user@hostname`: the current SSH session's host as the
/// shell reports it.
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
        // `write_str`, not `write!`: Trust cannot lower a runtime-argument `format_args!`.
        f.write_str(&self.user)?;
        f.write_str("@")?;
        f.write_str(&self.hostname)
    }
}
