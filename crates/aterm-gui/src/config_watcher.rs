// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live config hot-reload watcher.
//!
//! A single background thread `stat()`s the user config file
//! (`config_path()` — `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
//! `~/.config/aterm/aterm.toml`) on a fixed cadence and, when its
//! modification time changes, posts a [`Wake::ConfigReload`](crate::Wake) so the
//! UI thread re-reads + re-applies the config to every live session (see
//! `App::reload_config`).
//!
//! WHY mtime-poll, not a filesystem-notification crate: this codebase is
//! hardened, dependency-conscious, and sandbox-sensitive (the `Containment`
//! mode denies reads/writes under `~/.config/aterm`). A ~500 ms `stat` loop is a
//! handful of syscalls per second on one path — negligible — and adds ZERO new
//! dependencies, no inotify/FSEvents/kqueue file descriptors, and no surprising
//! behavior under a sandbox profile. It also degrades gracefully: if the file is
//! absent or unreadable the loop simply waits for it to appear (mtime is read as
//! `None` and any later `Some` is a change), so creating the config later still
//! triggers a reload.
//!
//! Two hard rules borrowed from the notification/clipboard threads:
//!
//! 1. **Never block the UI thread.** The watcher only ever does a non-blocking
//!    `EventLoopProxy::send_event`; the actual re-read + validation + apply
//!    happens on the UI thread, which is the sole owner of the renderer, window,
//!    and per-tab engines.
//! 2. **Self-terminating.** When the proxy `send_event` fails (the event loop is
//!    gone — the app is exiting), the loop breaks and the thread ends. It also
//!    exits as soon as the proxy can't be reached, so it never outlives the app.

use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use crate::Wake;

/// How often the watcher `stat()`s the config file. 500 ms is imperceptible for
/// an interactive "save and see it apply" loop while costing ~2 syscalls/sec on
/// a single path — far cheaper than wiring up a filesystem-notification crate,
/// and with no idle CPU concern (the thread is parked in `sleep` between polls).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Spawn the single, process-wide config-watcher thread.
///
/// `path` is the resolved config path (from `config_path()`); `None` means there
/// is no config path at all (no `$XDG_CONFIG_HOME` and no `$HOME`), in which case
/// we spawn nothing — there is nothing to watch, and the no-config startup path
/// stays byte-identical. The thread captures `proxy` (an [`EventLoopProxy`]) and
/// posts [`Wake::ConfigReload`] on every observed mtime change.
///
/// The baseline mtime is sampled ONCE at spawn (the file as it was at launch), so
/// only EDITS made after startup fire a reload — startup already loaded that same
/// file, so we never re-apply the launch config redundantly.
pub fn spawn(path: Option<std::path::PathBuf>, proxy: EventLoopProxy<Wake>) {
    let Some(path) = path else {
        return; // no config path → nothing to watch (no-config path unchanged)
    };
    std::thread::spawn(move || {
        // Baseline: the file's mtime at launch (or `None` if absent/unreadable).
        // A later transition to a DIFFERENT value — including absent→present —
        // is an edit worth reloading.
        let mut last = mtime(&path);
        loop {
            std::thread::sleep(POLL_INTERVAL);
            let now = mtime(&path);
            if now != last {
                last = now;
                // Non-blocking hand-off to the UI thread. A send failure means
                // the event loop is gone (app exiting) → stop the thread.
                if proxy.send_event(Wake::ConfigReload).is_err() {
                    break;
                }
            }
        }
    });
}

/// The config file's modification time, or `None` when it is absent/unreadable or
/// the platform/filesystem does not expose an mtime. Compared by value across
/// polls: any change (including absent↔present) signals an edit. Errors are
/// folded to `None` rather than logged — a transiently missing file mid-edit is
/// normal, and the UI-side `reload_config` is the place that warns on a genuinely
/// malformed file.
fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
