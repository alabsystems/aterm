// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Domain abstraction for terminal connections.
//!
//! A Domain represents a context for spawning terminal panes. Different domains
//! provide different connection types:
//!
//! - **Local**: Spawns processes on the local machine via PTY
//! - **SSH**: Connects to remote machines via SSH protocol
//! - **WSL**: Connects to Windows Subsystem for Linux instances
//! - **Serial**: Connects to serial port devices
//! - **Mux**: Connects to a remote multiplexer server
//!
//! ## Extraction Note
//!
//! Core domain types (`DomainId`, `PaneId`, `DomainState`, `DomainType`,
//! `SpawnConfig`, `DomainError`, `DomainResult`, `Pane`, `Domain`,
//! `DomainRegistry`) are defined in `aterm-types` and re-exported here for
//! backward compatibility. This enables extracted crates (`aterm-agent`)
//! to depend on `aterm-types` for domain traits without importing
//! `aterm-core`.

mod shell_state;
#[cfg(test)]
#[path = "../../test_support/domain/tests.rs"]
mod tests;

// =============================================================================
// Re-export core domain types from aterm-types.
//
// All trait definitions, error types, identity types, and the registry now
// live in `aterm_types::domain`. Re-exported here so existing
// `crate::domain::*` imports continue to work throughout aterm-core.
// =============================================================================

#[cfg(test)]
pub(crate) use aterm_types::domain::DomainRegistry;
#[cfg(test)]
#[allow(
    unused_imports,
    reason = "domain metadata re-exports preserve aterm-core's compatibility surface"
)]
pub use aterm_types::domain::{
    Domain, DomainCapabilities, DomainConfigError, DomainConnectionInfo, DomainError, DomainId,
    DomainResult, DomainState, DomainType, MuxConnectionConfig, MuxProtocol, MuxTransport, Pane,
    PaneId, SpawnConfig, SshConnectionConfig,
};

pub use shell_state::ShellState;

// =============================================================================
// Remote connection config types — no production consumers.
// Used only by tests.
// =============================================================================

/// SSH-specific configuration.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SshConfig {
    /// SSH host to connect to.
    pub(crate) host: String,
    /// SSH port (default: 22).
    pub(crate) port: u16,
    /// Use SSH agent for authentication.
    /// Retained as part of the test-only remote-config surface.
    pub(crate) use_agent: bool,
    /// Connection timeout in seconds.
    /// Retained as part of the test-only remote-config surface.
    pub(crate) connect_timeout_secs: u32,
}

#[cfg(test)]
impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            use_agent: true,
            connect_timeout_secs: 30,
        }
    }
}

#[cfg(test)]
impl SshConfig {
    /// Create a new SSH config for the given host.
    #[must_use]
    pub(crate) fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            ..Default::default()
        }
    }

    /// Set the port.
    #[must_use]
    pub(crate) fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Serial port configuration.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SerialConfig {
    /// Serial port path (e.g., /dev/ttyUSB0, COM1).
    pub(crate) port: String,
    /// Baud rate.
    pub(crate) baud_rate: u32,
}

#[cfg(test)]
impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud_rate: 115_200,
        }
    }
}

#[cfg(test)]
impl SerialConfig {
    /// Create a config for the given port.
    #[must_use]
    pub(crate) fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            ..Default::default()
        }
    }

    /// Set the baud rate.
    #[must_use]
    pub(crate) fn with_baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }
}
