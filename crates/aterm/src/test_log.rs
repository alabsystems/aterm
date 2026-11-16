// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `aterm_log` capture this test binary installs: its one logger, shared by every test that
//! reads what a step logged.

use std::cell::{Cell, RefCell};

thread_local! {
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static LEAST: Cell<aterm_log::Level> = const { Cell::new(aterm_log::Level::Info) };
}

struct Capture;

impl aterm_log::Log for Capture {
    fn enabled(&self, _: &aterm_log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &aterm_log::Record<'_>) {
        if record.level() > LEAST.with(Cell::get) {
            return;
        }
        LINES.with(|lines| {
            if let Some(lines) = lines.borrow_mut().as_mut() {
                lines.push(format!("{} {}", record.level(), record.args()));
            }
        });
    }

    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

/// What `body` answers, and the `aterm_log` records (level and message) at `least` or more severe
/// written on this thread while it ran. Every record reaches the capture, which keeps nothing
/// outside a `body`; installing it fails loudly where another logger is already installed.
pub(crate) fn logged_on_this_thread<T>(
    least: aterm_log::Level,
    body: impl FnOnce() -> T,
) -> (T, Vec<String>) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        aterm_log::set_logger(&CAPTURE).expect("this test binary installs no other logger");
        aterm_log::set_max_level(aterm_log::LevelFilter::Trace);
    });
    LEAST.with(|held| held.set(least));
    LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
    let answer = body();
    let logged = LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default());
    (answer, logged)
}
