// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Owner-local test helpers for aterm-core white-box tests.
//!
//! Public helpers (default_terminal, cell_at, etc.) live in `crate::testing`.
//! This module retains only logger capture helpers for white-box log
//! inspection in tests.

// Shared white-box test toolkit: the logger-capture and terminal helpers below
// are kept for white-box suites; some consumers were removed with the dead
// feature carve, so parts are currently unused in the default build.
#![allow(
    dead_code,
    reason = "shared test toolkit; helpers consumed by gated/un-wired test suites"
)]

use crate::terminal::{ClipboardAccess, Terminal};
use aterm_log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::{Mutex, MutexGuard, Once, OnceLock};

/// Create a standard 24x80 terminal for testing.
///
/// Opts into `allow_palette_reconfigure` (#7937) and `allow_osc52_set`
/// (#7782) for parity with `crate::testing::default_terminal` — OSC 4 /
/// OSC 21 palette-SET and OSC 52 clipboard-SET are now fail-closed by
/// default and pre-existing tests expect the opted-in posture.
pub fn default_terminal() -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.modes_mut().allow_palette_reconfigure = true;
    term.authorize_clipboard_access(ClipboardAccess::Write);
    term
}

// ============================================================================
// Log Capture Helpers
// ============================================================================

struct TestLogger {
    entries: Mutex<Vec<String>>,
}

impl TestLogger {
    fn clear(&self) {
        self.entries.lock().expect("lock test logger").clear();
    }

    fn entries(&self) -> Vec<String> {
        self.entries.lock().expect("lock test logger").clone()
    }
}

impl Log for TestLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Warn
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(&record.metadata()) {
            self.entries.lock().expect("lock test logger").push(format!(
                "{}: {}",
                record.level(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

static TEST_LOGGER: OnceLock<TestLogger> = OnceLock::new();
static LOGGER_INIT: Once = Once::new();
static LOGGER_TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_logger() -> &'static TestLogger {
    TEST_LOGGER.get_or_init(|| TestLogger {
        entries: Mutex::new(Vec::new()),
    })
}

pub(crate) fn install_test_logger() -> MutexGuard<'static, ()> {
    LOGGER_INIT.call_once(|| {
        aterm_log::set_logger(test_logger()).expect("install shared test logger");
        aterm_log::set_max_level(LevelFilter::Trace);
    });

    let guard = LOGGER_TEST_LOCK.lock().expect("lock shared test logger");
    test_logger().clear();
    guard
}

pub(crate) fn matching_test_logs(needle: &str) -> Vec<String> {
    test_logger()
        .entries()
        .into_iter()
        .filter(|entry| entry.contains(needle))
        .collect()
}
