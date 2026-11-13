// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The settings witness.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::Fail;
use crate::pyjson::J;
use crate::run::Run;

/// The witness's log in the scratch: one line per re-read Claude Code reported.
pub const LOG: &str = "config-change.log";

/// The hook's script in the run directory (the jail cannot write there).
pub const SCRIPT: &str = "config-change.sh";

/// The rewrite pace: an unanswered write is made again this long after it was made.
pub const REWRITE_EVERY: Duration = Duration::from_secs(3);

/// The hang detector: no re-read this long after the first write ends the wait.
pub const HANG: Duration = Duration::from_secs(60);

/// The hook's script.
#[must_use]
pub fn script(log: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         # the lab's settings witness: Claude Code runs this ConfigChange hook when it detects a change to a settings\n\
         # file, before it applies it; the hook's input (one JSON object naming the file) becomes one line of the log\n\
         {{ /bin/cat; /bin/echo; }} >> {} 2>/dev/null\n\
         exit 0\n",
        crate::util::shell_quote(&log.to_string_lossy())
    )
}

/// The scratch settings' `hooks` value: every `ConfigChange` runs `script`.
#[must_use]
pub fn hooks(script: &Path) -> J {
    let command = J::obj([
        ("type", J::s("command")),
        (
            "command",
            J::s(crate::util::shell_quote(&script.to_string_lossy())),
        ),
    ]);
    J::obj([(
        "ConfigChange",
        J::Arr(vec![J::obj([("hooks", J::Arr(vec![command]))])]),
    )])
}

/// How many re-reads of `settings` the log records.
#[must_use]
pub fn rereads(log: &str, settings: &str) -> usize {
    log.lines()
        .filter(|l| {
            J::parse(l).is_ok_and(|j| {
                j.get("hook_event_name").and_then(J::as_str) == Some("ConfigChange")
                    && j.get("file_path").and_then(J::as_str) == Some(settings)
            })
        })
        .count()
}

/// A write Claude Code re-read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reread {
    /// The writes made by the time a line answered.
    pub writes: u32,
    /// From the first write to the line that answered.
    pub after: Duration,
}

/// The reread loop's monotonic clock and interruptible pause.
pub(crate) trait Wait {
    fn elapsed(&self) -> Duration;
    fn pause(&self) -> Result<(), Fail>;
}

pub(crate) struct WallWait(Instant);

impl WallWait {
    pub(crate) fn new() -> Self {
        Self(Instant::now())
    }
}

impl Wait for WallWait {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn pause(&self) -> Result<(), Fail> {
        crate::stop::sleep(Duration::from_millis(100)).map_err(Into::into)
    }
}

/// Advance only when the loop pauses; a delayed test thread changes no simulated deadline.
#[cfg(test)]
pub(crate) struct SteppedWait<F> {
    elapsed: std::cell::Cell<Duration>,
    step: Duration,
    started: Instant,
    paused: F,
}

#[cfg(test)]
impl<F: Fn()> SteppedWait<F> {
    pub(crate) fn new(step: Duration, paused: F) -> Self {
        Self {
            elapsed: std::cell::Cell::new(Duration::ZERO),
            step,
            started: Instant::now(),
            paused,
        }
    }
}

#[cfg(test)]
impl<F: Fn()> Wait for SteppedWait<F> {
    fn elapsed(&self) -> Duration {
        assert!(self.started.elapsed() < HANG, "reread loop never finished");
        self.elapsed.get()
    }

    fn pause(&self) -> Result<(), Fail> {
        (self.paused)();
        self.elapsed.set(self.elapsed.get() + self.step);
        Ok(())
    }
}

/// Run `write` until the witness counts a line it did not count when the first write was made.
/// # Errors
/// `write` failed, the run was interrupted, or nothing answered within `hang`.
pub(crate) fn until_reread(
    mut write: impl FnMut() -> Result<(), Fail>,
    seen: impl Fn() -> usize,
    every: Duration,
    hang: Duration,
    wait: &impl Wait,
) -> Result<Reread, Fail> {
    let start = wait.elapsed();
    write()?;
    let base = seen();
    let mut writes = 1u32;
    let mut next = start + every;
    loop {
        let answered = seen() > base;
        let now = wait.elapsed();
        if answered {
            return Ok(Reread {
                writes,
                after: now.saturating_sub(start),
            });
        }
        if now.saturating_sub(start) >= hang {
            return Err(Fail::die(format!(
                "Claude Code reported no re-read of its settings in {} s ({writes} writes): its ConfigChange \
                 hook logged nothing after them",
                hang.as_secs()
            )));
        }
        if now >= next {
            write()?;
            writes += 1;
            next = now + every;
        } else {
            wait.pause()?;
        }
    }
}

/// Set or remove `key` in the scratch settings' env block and wait until Claude Code re-reads the file.
///
/// # Errors
/// The settings could not be written, the run was interrupted, or nothing answered within [`HANG`].
pub fn set_env_reread(run: &Run, key: &str, value: Option<&str>) -> Result<Reread, Fail> {
    set_env_reread_with_wait(run, key, value, &WallWait::new())
}

fn reread_counter(log: std::path::PathBuf, settings: String) -> impl Fn() -> usize {
    let state = std::cell::RefCell::new((crate::util::LineTailCursor::default(), 0usize));
    move || {
        let mut st = state.borrow_mut();
        let (reset, delta) = st.0.poll_scratch_text(&log).unwrap_or_default();
        if reset {
            st.1 = 0;
        }
        if !delta.is_empty() {
            st.1 += rereads(&delta, &settings);
        }
        st.1
    }
}

/// Deep-merge `patch` into the scratch settings ([`crate::verbs::settings_merge`]) and wait until
/// Claude Code re-reads the file.
///
/// # Errors
/// The settings could not be written, the run was interrupted, or nothing answered within [`HANG`].
pub fn merge_reread(run: &Run, patch: &J) -> Result<Reread, Fail> {
    let settings = run.settings().to_string_lossy().into_owned();
    let log = run.w.join(LOG);
    until_reread(
        || crate::verbs::settings_merge(run, patch).map(drop),
        reread_counter(log, settings),
        REWRITE_EVERY,
        HANG,
        &WallWait::new(),
    )
}

pub(crate) fn set_env_reread_with_wait(
    run: &Run,
    key: &str,
    value: Option<&str>,
    wait: &impl Wait,
) -> Result<Reread, Fail> {
    let settings = run.settings().to_string_lossy().into_owned();
    let log = run.w.join(LOG);
    until_reread(
        || crate::verbs::set_env(run, key, value).map(drop),
        reread_counter(log, settings),
        REWRITE_EVERY,
        HANG,
        wait,
    )
}

#[cfg(test)]
#[path = "settings_witness_tests.rs"]
mod tests;
