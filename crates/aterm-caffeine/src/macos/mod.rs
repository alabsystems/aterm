// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The AppKit half: the app and what each click, choice and tick does ([`app`]), the status
//! item and its menu ([`ui`]), the activity that keeps the Mac awake ([`awake`]), the update
//! watch ([`watch`]), the bridge that keeps it awake while an update restart's new mug
//! starts ([`bridge`]) and the SIGTERM a person's quit sends ([`term`]). The app is reached only from `aterm caffeine serve`, never from a test
//! or a headless path: `run` creates `NSApplication`, which opens a WindowServer connection.
//! The bridge (`aterm caffeine bridge`) is Foundation only.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::instance::Instance;
use crate::paths::Paths;

mod app;
mod awake;
mod bridge;
mod term;
mod ui;
mod watch;

/// What `serve` hands the menu-bar app.
#[derive(Debug)]
pub(crate) struct ServeContext {
    /// The mug's files.
    pub paths: Paths,
    /// Its launchd job's label (Enable at Startup writes that job's plist).
    pub label: String,
    /// The executable the job runs: where `serve` was started, not the copy it runs from.
    pub origin: PathBuf,
    /// The app version the about row prints.
    pub version: String,
    /// End with [`crate::launchd::EXIT_RESTART`] when the origin's code changes: only under
    /// launchd, which starts it again.
    pub restart_on_update: bool,
    /// This process's hold on the one-mug lock, kept until it returns.
    pub instance: Instance,
}

/// Run the menu-bar mug on this thread, the process main thread, until Quit (or an update
/// restart); what `serve` returns.
pub(crate) fn serve(ctx: ServeContext) -> ExitCode {
    app::run(ctx)
}

/// Let a bridge an update restart left go: a `serve` that will not run holds nothing for it to
/// bridge to ([`bridge::release`]).
pub(crate) fn release_bridge(paths: &Paths) {
    bridge::release(paths);
}

/// `aterm caffeine bridge`: keep the Mac awake while the mug restarts onto an update
/// ([`bridge`]); what a restarting mug runs.
pub(crate) fn bridge() -> ExitCode {
    bridge::run()
}
