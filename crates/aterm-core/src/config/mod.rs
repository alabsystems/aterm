// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runtime terminal configuration: settings a host changes without
//! recreating the terminal.
//!
//! # Example
//!
//! ```
//! use aterm_core::config::TerminalConfig;
//! use aterm_core::terminal::{CursorStyle, Terminal};
//!
//! let mut term = Terminal::new(24, 80);
//!
//! let mut config = TerminalConfig::default();
//! config.scrollback_limit = Some(50_000);
//! config.cursor_style = CursorStyle::SteadyBar;
//! config.cursor_blink = false;
//!
//! let changes = term.apply_config(&config);
//! for change in changes {
//!     println!("Changed: {:?}", change);
//! }
//! ```

mod types;

#[cfg(test)]
#[path = "../../test_support/config/tests.rs"]
mod tests;

pub use types::{BiDiConfig, BiDiMode, ConfigChange, TerminalConfig};
