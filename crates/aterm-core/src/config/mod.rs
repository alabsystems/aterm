// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration management for aterm-core terminals.
//!
//! This module provides runtime configuration hot-reload support, allowing
//! integrators to change terminal settings without recreating the terminal.
//!
//! # Example
//!
//! ```
//! use aterm_core::config::TerminalConfig;
//! use aterm_core::terminal::{CursorStyle, Terminal};
//!
//! let mut term = Terminal::new(24, 80);
//!
//! // Create a new configuration
//! let mut config = TerminalConfig::default();
//! config.scrollback_limit = Some(50_000);
//! config.cursor_style = CursorStyle::SteadyBar;
//! config.cursor_blink = false;
//!
//! // Apply configuration changes
//! let changes = term.apply_config(&config);
//!
//! // Check what changed
//! for change in changes {
//!     println!("Changed: {:?}", change);
//! }
//! ```

mod types;

/// Builder pattern for constructing [`TerminalConfig`].
pub mod builder;

#[cfg(test)]
#[path = "../../test_support/config/tests.rs"]
mod tests;

pub use builder::ConfigBuilder;
pub use types::{
    BiDiConfig, BiDiMode, ConfigChange, DiskBackendConfig, ScrollbackBackend, TerminalConfig,
};
