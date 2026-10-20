// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-caffeine` — the menu-bar keep-awake mug, `aterm caffeine`.
//!
//! A coffee mug with an "a" on it sits in the menu bar. A click fills it and keeps this Mac
//! awake (no idle sleep of the system or the display) until the next click; a right click
//! (or a Control-click) opens a menu: a one-line about row, Activate for 1, 8 or 24 hours or
//! Forever, Enable at Startup, and Quit.
//!
//! # Where it runs
//!
//! The mug is its own process: `aterm caffeine serve`, the launchd job [`launchd`] names, in the
//! user's GUI domain. It is never a child of an aterm tab, whose window's job launchd tears down
//! when that window quits. `aterm caffeine` (or `on`) bootstraps that job and returns.
//!
//! A process that creates `NSApplication` from `aterm.app/Contents/MacOS/aterm` checks in with
//! LaunchServices as `com.aterm.aterm`, and from then on every Finder, Dock or Spotlight open
//! of aterm is handed to it as a reopen and no window opens (measured 2026-10-19 with stand-in
//! bundles: accessory and prohibited policies alike, first registration wins). So `serve`
//! first copies the whole bundle it was started from into [`paths::Paths::code_dir`] (an APFS
//! clone per file, the signature still sealing it) and re-executes from the copy ([`code`]): a
//! running app at another path is not the one an open of `aterm.app` names. A build tree's bare
//! executable has no bundle and runs where it is.
//!
//! # State
//!
//! The mode ([`model::Mode`]: off, forever, or until a wall-clock deadline) is a small file
//! ([`state`]) the mug and `aterm caffeine on|off` both write; a writer posts a Darwin
//! notification ([`notify`]) and the mug reads the file again. A deadline is wall-clock, so a
//! sleep of the Mac is not added to it, and a restart of the Mac turns the mug off (the file
//! names the boot it was written in). Quit turns it off.
//!
//! # Layout
//!
//! The model ([`model`]), the menu ([`menu`]), the state file's text ([`state`]), the icon
//! raster ([`icon`]), the launchd plist ([`launchd`]) and the argument grammar ([`cli`]) are
//! pure and compile and test everywhere. The AppKit half (`macos`) and the process plumbing
//! (the job, the bundle copy, the instance lock) are macOS; elsewhere the verb says it is
//! macOS only.

#![warn(missing_docs)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cli;
pub mod code;
pub mod icon;
pub mod instance;
pub mod launchd;
#[cfg(target_os = "macos")]
mod macos;
pub mod menu;
pub mod model;
pub mod notify;
pub mod paths;
pub mod state;
