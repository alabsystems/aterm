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
//! The domain types (`DomainId`, `PaneId`, `DomainState`, `DomainType`,
//! `SpawnConfig`, `DomainError`, `DomainResult`, `Pane`, `Domain`,
//! `DomainRegistry`) live in `aterm_types::domain`, so extracted crates
//! (`aterm-agent`) depend on `aterm-types` for them without importing
//! `aterm-core`. This module keeps only [`ShellState`].

mod shell_state;

pub use shell_state::ShellState;
