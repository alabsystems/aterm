// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AppKit half of the menu-bar mug (`app`, `ui`, `awake`, `watch`, `bridge`, `term`, `login_items`).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::instance::Instance;
use crate::launchd::StartupDoors;
use crate::paths::Paths;

mod app;
mod awake;
mod bridge;
mod login_items;
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
    /// End with [`crate::launchd::EXIT_RESTART`] when the origin's code changes.
    pub restart_on_update: bool,
    /// This process's hold on the one-mug lock, kept until it returns.
    pub instance: Instance,
}

/// Run the menu-bar mug on this thread, the process main thread, until Quit (or an update restart).
pub(crate) fn serve(ctx: ServeContext) -> ExitCode {
    app::run(ctx)
}

/// Let a bridge an update restart left go.
pub(crate) fn release_bridge(paths: &Paths) {
    bridge::release(paths);
}

/// The live doors to what decides whether a login loads the login plist.
pub(crate) const STARTUP_DOORS: StartupDoors = StartupDoors {
    blocks: login_items::blocks,
    disabled: crate::launchd::disabled,
    enable: crate::launchd::enable,
};

/// `aterm acaffiene bridge`: keep the Mac awake while the mug restarts onto an update
/// ([`mod@bridge`]).
pub(crate) fn bridge() -> ExitCode {
    bridge::run()
}
