// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-acaffeine` — the menu-bar keep-awake mug, `aterm acaffeine`. On Windows, where there is
//! no mug, the verb runs a holder process that keeps the PC from its idle sleep.

#![warn(missing_docs)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cli;
pub mod code;
pub mod held;
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
pub mod trouble;
#[cfg(windows)]
mod win;

/// Creates an isolated `.noindex` temporary directory for `aterm-acaffeine` tests so test churn
/// stays out of Spotlight and `fseventsd`.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let clean: String = tag
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let short = &clean[..clean.floor_char_boundary(12)];
    aterm_tempfile::Builder::new()
        .prefix(&format!("acaf-{short}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("create aterm-acaffeine test scratch dir")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
