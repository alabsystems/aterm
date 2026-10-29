// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines`: one controller configures and keeps configured a lab of Macs over SSH
//! (docs/DESIGN-machines-2026-10-16.md).

#![cfg(unix)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();
#[cfg(test)]
aterm_secrets::memoized_x25519_in_tests!();

pub mod agent;
pub mod askpass;
pub mod baseline;
pub(crate) mod checkout;
pub mod cli;
pub mod console;
pub mod controller;
pub mod engine;
pub mod envelope;
pub mod facts;
pub mod first_exec;
pub(crate) mod git_credential;
/// GitHub's REST pagination, shared with the publishers' REST lane.
pub(crate) use aterm_update_core::github_pages;
pub mod hardware;
pub mod init;
pub mod init_accounts;
pub mod init_activation;
pub mod init_clt;
pub mod init_develop;
pub mod init_fseventsd;
pub mod init_github;
pub mod init_observe;
pub mod init_preferences;
pub mod init_secrets;
pub mod init_spotlight;
pub mod init_toolset;
pub mod init_trackpad;
pub mod items;
pub mod model;
pub mod plist;
pub mod port;
pub mod protocol;
pub mod sessions;
pub mod sync;

/// The converge engine the families run on (docs/DESIGN-init-system-2026-10-20.md §3).
pub use aterm_converge as converge;
pub use engine::Bound;

/// Where the agent lives on a managed host: root:wheel, nothing below it writable by anyone else
/// (design §3).
pub const AGENT_ROOT: &str = "/Library/Management/aterm";

/// The sudoers drop-in enrollment writes (design §4.2).
pub const SUDOERS_FILE: &str = "/etc/sudoers.d/aterm-machines";

/// The schema of every document this crate writes or reads: envelope, baseline, report.
pub const SCHEMA: u32 = 1;
