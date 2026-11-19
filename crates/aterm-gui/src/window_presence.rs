// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WINDOW PRESENCE (`window-last`): when a window of this account last ran, for a survey taken
//! while none runs ([`aterm_agent::survey_facts::record_window_last`]; the fleet census counts a
//! Mac warm while it reads within [`aterm_agent::fleet_census::WARM_WINDOW_S`]).
//!
//! A windowed instance writes it at its start, at each wake from sleep the watchdog's wall watch
//! hears ([`Presence::ring`], `crate::wake_rings::wall_moved`), at its quit ([`quit`]:
//! `App::end_run`, and a terminating signal's `the_window_dies`) and every [`PRESENCE_EVERY`]
//! between, the backstop for a window that ends without its quit (a crash, a kill). A wait does
//! not run while the Mac sleeps, so the wake's write, not the backstop, dates a window that woke.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use aterm_uds::park_bell::ParkBell;

/// The backstop between two writes while nothing rings: a window that crashed reads at most this
/// long before its end.
pub(crate) const PRESENCE_EVERY: Duration = Duration::from_secs(60 * 60);

// The backstop is a small part of the horizon a reader judges it by: a window that crashed still
// reads warm for nearly the whole of it. The reader, the fleet census, is built on unix alone.
#[cfg(unix)]
const _: () = assert!(PRESENCE_EVERY.as_secs() * 24 <= aterm_agent::fleet_census::WARM_WINDOW_S);

/// What ended one wait of the beat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Heard {
    /// The backstop's period ran out.
    Due,
    /// The Mac woke from sleep, or the wall clock stepped ([`Presence::ring`]).
    Woke,
    /// The window quits ([`quit`]), or its control server's store is gone.
    Quit,
}

/// THE BEAT: `write` once at its start, then once each time `wait` (given [`PRESENCE_EVERY`] or
/// a test's period, `every`) ends for the backstop or a wake; a quit ends it without a write,
/// since the quit wrote ([`Presence::quit`]).
pub(crate) fn beat(
    every: Duration,
    mut write: impl FnMut(),
    mut wait: impl FnMut(Duration) -> Heard,
) {
    write();
    while wait(every) != Heard::Quit {
        write();
    }
}

/// What a wait has been told since it last looked.
#[derive(Debug, Default)]
struct Told {
    woke: bool,
    quit: bool,
}

/// One window's presence: what rings its beat ([`ParkBell`]), and the state directory it writes
/// under.
#[derive(Debug)]
pub(crate) struct Presence {
    bell: ParkBell<Told>,
    /// The harness state directory the beat writes `window-last` under, once its beat started.
    state: OnceLock<PathBuf>,
}

impl Presence {
    /// A presence nothing has rung, with no beat.
    pub(crate) const fn new() -> Self {
        Self {
            bell: ParkBell::new(Told {
                woke: false,
                quit: false,
            }),
            state: OnceLock::new(),
        }
    }

    /// The Mac woke (or the wall stepped): the beat writes now.
    pub(crate) fn ring(&self) {
        self.bell.ring(|told| told.woke = true);
    }

    /// The window quits: `write` this presence now, once, under the state directory its beat
    /// started with (nothing where no beat started), and end the beat.
    pub(crate) fn quit(&self, write: impl FnOnce(&Path)) {
        let first = !self
            .bell
            .ring(|told| std::mem::replace(&mut told.quit, true));
        if first && let Some(state) = self.state.get() {
            write(state);
        }
    }

    /// Wait at most `every` for a ring or the quit, taking a ring heard before the wait.
    pub(crate) fn wait(&self, every: Duration) -> Heard {
        self.bell
            .wait(every, |told| {
                if told.quit {
                    Some(Heard::Quit)
                } else {
                    std::mem::take(&mut told.woke).then_some(Heard::Woke)
                }
            })
            .unwrap_or(Heard::Due)
    }
}

/// This process's window presence: the wall watch rings it (`crate::wake_rings::wall_moved`).
pub(crate) static PRESENCE: Presence = Presence::new();

/// Write `window-last` under `state` at the wall clock's second now.
fn write_now(state: &Path) {
    if let Some(now) = aterm_time::unix_secs() {
        let _ = aterm_agent::survey_facts::record_window_last(state, now);
    }
}

/// Start a windowed instance's beat on a thread of its own. `alive` says whether the control
/// server's store stands: once it does not, the beat ends at its next wake without a write.
#[cfg(not(test))]
pub(crate) fn start(alive: impl Fn() -> bool + Send + 'static) {
    let _ = crate::qos::spawn_lane_thread(
        "aterm-window-presence",
        crate::qos::Role::Background,
        move || {
            let Ok(env) = aterm_agent::harness::cli::Env::from_process() else {
                return;
            };
            let state = PRESENCE.state.get_or_init(|| env.state);
            beat(
                PRESENCE_EVERY,
                || write_now(state),
                |every| {
                    let heard = PRESENCE.wait(every);
                    if alive() { heard } else { Heard::Quit }
                },
            );
        },
    );
}

/// The window quits (`App::end_run`, or a terminating signal's `the_window_dies`): its presence
/// written now, first of the quit's steps, so a kill that cuts the quit short still finds it,
/// and the beat ended. Nothing where no beat started (a headless instance, a test).
pub(crate) fn quit() {
    PRESENCE.quit(write_now);
}

#[cfg(test)]
#[path = "window_presence_tests.rs"]
mod tests;
