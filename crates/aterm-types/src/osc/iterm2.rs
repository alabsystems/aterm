// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The OSC 1337 shell-integration version report.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Terminal shell integration version report (OSC 1337 ShellIntegrationVersion).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iterm2ShellIntegrationVersion {
    /// Shell integration version number (Pn).
    pub version: u32,
    /// Optional shell name (Ps).
    pub shell: Option<String>,
}

impl Iterm2ShellIntegrationVersion {}
