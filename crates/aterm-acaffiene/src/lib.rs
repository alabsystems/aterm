// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-acaffiene` — the menu-bar keep-awake mug, `aterm acaffiene`.

#![warn(missing_docs)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cli;
pub mod code;
pub mod icon;
pub mod instance;
pub mod launchd;
pub mod legacy;
#[cfg(target_os = "macos")]
mod macos;
pub mod menu;
pub mod model;
pub mod notify;
pub mod paths;
pub mod state;
