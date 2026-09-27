// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shell's state as the engine tracks it ([`ShellState`]). (The domain
//! abstraction this module once pointed at — `Domain`, `Pane`,
//! `DomainRegistry` in `aterm_types::domain` — was never implemented and was
//! deleted on 2026-09-25.)

mod shell_state;

pub use shell_state::ShellState;
