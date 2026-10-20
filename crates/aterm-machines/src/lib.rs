// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines`: one controller configures and keeps configured a lab of Macs over SSH
//! (docs/DESIGN-machines-2026-10-16.md).
//!
//! Two halves share one vocabulary ([`model`]) and one port to the operating system
//! ([`port::System`]):
//!
//! * the **host agent** ([`agent`]) runs as root under one sudoers rule, accepts only a
//!   signed [`envelope`] (OpenSSH's SSHSIG, checked by `/usr/bin/ssh-keygen`), and drives
//!   every setting through [`engine`]: probe, plan, apply, verify, one [`items::Item`] at a
//!   time;
//! * the **controller** ([`controller`]) holds the inventory, the fleet's SSH and signing
//!   keys and the verified artifact cache, and reaches each host with `/usr/bin/ssh`.
//!
//! Nothing here runs a tool by a `PATH` lookup or through a shell: every child is an
//! absolute program built by `aterm_uds::spawnfd::command` and bounded by a deadline.
//!
//! Inert off unix: the crate drives macOS's own tools over POSIX descriptors, and
//! crates/aterm links it only on macOS (answering `aterm machines` elsewhere with a
//! refusal), so a Windows workspace build compiles nothing here.

#![cfg(unix)]

pub mod agent;
pub mod askpass;
pub mod baseline;
pub mod cli;
pub mod controller;
pub mod engine;
pub mod envelope;
pub mod facts;
pub mod hardware;
pub mod items;
pub mod model;
pub mod plist;
pub mod port;
pub mod protocol;
pub mod sessions;

/// Where the agent lives on a managed host: root:wheel, nothing below it writable by
/// anyone else (design §3).
pub const AGENT_ROOT: &str = "/Library/Management/aterm";

/// The sudoers drop-in enrollment writes (design §4.2).
pub const SUDOERS_FILE: &str = "/etc/sudoers.d/aterm-machines";

/// The schema of every document this crate writes or reads: envelope, baseline, report.
pub const SCHEMA: u32 = 1;
