// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The settings witness: whether a running Claude Code has re-read its
//! settings since the lab wrote them. Every lab's scratch `settings.json`
//! carries a `ConfigChange` hook ([`hooks`]), `runs/<tag>/config-change.sh`
//! ([`script`], written from outside the jail like `apikey.sh`), which appends
//! the hook's input, one JSON object naming the file, as a line of
//! `<scratch>/config-change.log`. Claude Code (2.1.306, read from its bundle)
//! runs the hook when its watcher detects a change to a settings file, and
//! applies that file's settings, its `env` among them, once the hook returns.
//! So a line appended after a write shows that the settings Claude Code applies
//! next are that write's or a later one's.
//!
//! A write is no proof of a re-read. Claude Code's watcher sees a change only
//! when the file event reaches it, and on a loaded machine an event can arrive
//! late or not at all. On 2026-10-21 the selftest's forced survey card did not
//! show in 3 runs out of 3. The flag was written 3 s before the turn, and the
//! lab assumed Claude Code had read it. A fresh lab that wrote the flag 2.5 s
//! after the composer drew saw no re-read in 60 s, while another that wrote it
//! 15 s after the composer drew saw it re-read in 1.9 s (`docs/measured/
//! forever-lab-2026-10-20.md`, H2). So
//! [`until_reread`] writes, writes again every [`REWRITE_EVERY`] while
//! nothing answers, and stops at a one-minute hang detector.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::Fail;
use crate::pyjson::J;
use crate::run::Run;

/// The witness's log in the scratch: one line per re-read Claude Code reported.
pub const LOG: &str = "config-change.log";

/// The hook's script in the run directory (the jail cannot write there).
pub const SCRIPT: &str = "config-change.sh";

/// How long an unanswered write stands before it is made again.
pub const REWRITE_EVERY: Duration = Duration::from_secs(3);

/// The hang detector: no re-read this long after the first write ends the wait.
pub const HANG: Duration = Duration::from_secs(60);

/// The hook's script: the hook's input and a newline appended to `log`, and
/// exit 0 whatever happened. A hook that fails would draw an error in the tab,
/// and exit 2 would block the change.
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

/// How many re-reads of `settings` the log records: its lines that hold a
/// JSON object whose `hook_event_name` is `ConfigChange` and whose
/// `file_path` is `settings`.
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
    /// The writes made by the time a line answered (every one the same
    /// write, so the line may be an earlier one's).
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

/// Advance only when the loop pauses; a delayed test thread changes no
/// simulated deadline. Real time remains a one-minute hang detector.
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

/// Run `write` until the witness counts a line it did not count when the
/// first write was made: `seen` is read once, just after that write, so a line
/// that lands during it (an older change's) is never the answer. Claude Code
/// applies the file after the hook that wrote the line returns, so once a
/// later line comes it applies the first write or a later one. Every write is
/// the same write, so a line that comes after a rewrite answers too, even one
/// that lands while the rewrite is being made. While no line comes, the write
/// is made again every `every`. The wait gives up `hang` after the first
/// write. `wait` is the clock and the pause between reads; each round reads
/// the clock once, after the log, so no write follows a reading at or past
/// `hang`.
///
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
    let mut next = wait.elapsed() + every;
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
            next = wait.elapsed() + every;
        } else {
            wait.pause()?;
        }
    }
}

/// Set (or, with no value, remove) `key` in the scratch settings' env block
/// of a lab whose Claude Code is running, and return once Claude Code
/// re-read the file ([`until_reread`] on the witness's log), written again
/// every [`REWRITE_EVERY`] and given up [`HANG`] after the first write.
///
/// # Errors
/// The settings could not be written, the run was interrupted, or Claude Code
/// reported no re-read within [`HANG`].
pub fn set_env_reread(run: &Run, key: &str, value: Option<&str>) -> Result<Reread, Fail> {
    set_env_reread_with_wait(run, key, value, &WallWait::new())
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
        || rereads(&crate::util::scratch_text(&log), &settings),
        REWRITE_EVERY,
        HANG,
        wait,
    )
}

#[cfg(test)]
mod tests {
    use aterm_uds::spawnfd::Stdio;
    use std::cell::Cell;

    use super::*;

    const SETTINGS: &str = "/l/runs/t/scratch/home/.claude/settings.json";

    fn line(event: &str, file: &str) -> String {
        format!(
            "{{\"session_id\":\"s\",\"hook_event_name\":\"{event}\",\"source\":\"user_settings\",\"file_path\":\"{file}\"}}"
        )
    }

    /// A line counts when it is a `ConfigChange` naming the lab's settings
    /// file. NEGATIVE CONTROLS: another file's change, another hook event, a
    /// line that is not JSON, and an object with no `file_path` count nothing.
    #[test]
    fn a_reread_is_a_config_change_line_naming_the_settings_file() {
        let ok = line("ConfigChange", SETTINGS);
        assert_eq!(rereads(&format!("{ok}\n{ok}\n"), SETTINGS), 2);
        for (what, text) in [
            (
                "another file",
                line("ConfigChange", "/l/runs/t/proj/.claude/settings.json"),
            ),
            ("another event", line("PreToolUse", SETTINGS)),
            ("not JSON", format!("ConfigChange {SETTINGS}")),
            (
                "no file_path",
                "{\"hook_event_name\":\"ConfigChange\"}".to_string(),
            ),
            ("nothing", String::new()),
        ] {
            assert_eq!(rereads(&text, SETTINGS), 0, "{what}: {text}");
        }
    }

    /// The wait's clock steps a third of `every` at each pause, so a rewrite
    /// due a step or more sooner than `every` after the last write ends, or
    /// due any time later, comes at another step than the one at `every`.
    fn stepped() -> SteppedWait<impl Fn()> {
        SteppedWait::new(REWRITE_EVERY / 3, || {})
    }

    /// The first write is never answered (the watcher missed it), a rewrite
    /// is: the wait writes again `every` after the first write and returns on
    /// the line that came after it, whether the line lands after the wait read
    /// the log again (one step later) or is already there at the first read
    /// after the rewrite (it landed while the rewrite was being made, the
    /// moment a baseline read after each write swallowed).
    #[test]
    fn an_unanswered_write_is_made_again_until_a_line_answers() {
        let after_a_read = REWRITE_EVERY + REWRITE_EVERY / 3;
        for (what, from_read, after) in [
            ("after a read", 2u32, after_a_read),
            ("during the rewrite", 1, REWRITE_EVERY),
        ] {
            let writes = Cell::new(0u32);
            let reads = Cell::new(0u32);
            let r = until_reread(
                || {
                    writes.set(writes.get() + 1);
                    reads.set(0);
                    Ok(())
                },
                || {
                    reads.set(reads.get() + 1);
                    usize::from(writes.get() >= 2 && reads.get() >= from_read)
                },
                REWRITE_EVERY,
                HANG,
                &stepped(),
            )
            .expect(what);
            assert_eq!(r, Reread { writes: 2, after }, "{what}");
            assert_eq!(writes.get(), 2, "{what}: no write after the answer");
        }
    }

    /// NEGATIVE CONTROLS: a line that lands during the first write, an older
    /// change's, is no answer when no later line comes, and neither is a log
    /// that never grows. Both end at the hang detector, at the first step at
    /// or past `hang`, after a write at every step a multiple of `every`
    /// before it: `hang / every` writes.
    #[test]
    fn a_line_from_before_the_write_ends_or_none_at_all_is_no_reread() {
        let made = u32::try_from(HANG.as_millis() / REWRITE_EVERY.as_millis()).unwrap();
        for (what, bumps) in [("a line during the write", true), ("no line", false)] {
            let lines = Cell::new(0usize);
            let writes = Cell::new(0u32);
            let wait = stepped();
            let r = until_reread(
                || {
                    writes.set(writes.get() + 1);
                    if bumps && writes.get() == 1 {
                        lines.set(lines.get() + 1);
                    }
                    Ok(())
                },
                || lines.get(),
                REWRITE_EVERY,
                HANG,
                &wait,
            );
            let e = r.expect_err(what);
            assert!(e.msg.unwrap_or_default().contains("no re-read"), "{what}");
            assert_eq!(wait.elapsed(), HANG, "{what}: the hang detector's moment");
            assert_eq!(writes.get(), made, "{what}: one write every `every`");
        }
    }

    /// A clock that moves by `tick` at every reading and never at a pause,
    /// as `WallWait`'s does when the thread is held up between two reads.
    struct Ticking {
        next: Cell<Duration>,
        last: Cell<Duration>,
        tick: Duration,
    }

    impl Wait for Ticking {
        fn elapsed(&self) -> Duration {
            self.last.set(self.next.get());
            self.next.set(self.next.get() + self.tick);
            self.last.get()
        }

        fn pause(&self) -> Result<(), Fail> {
            Ok(())
        }
    }

    /// No write follows a reading at or past `hang`, even on a clock that
    /// moves between reads: a round reads the clock once, and that reading
    /// decides both the hang detector and the rewrite. NEGATIVE CONTROL: a
    /// round that read the clock again for the rewrite wrote after its hang
    /// check had passed and the clock had reached `hang`.
    #[test]
    fn no_write_follows_a_reading_at_or_past_hang() {
        let late: Vec<_> = [1, 1000, 7000]
            .into_iter()
            .filter_map(|ms| {
                let wait = Ticking {
                    next: Cell::new(Duration::ZERO),
                    last: Cell::new(Duration::ZERO),
                    tick: Duration::from_millis(ms),
                };
                let mut at = Vec::new();
                let r = until_reread(
                    || {
                        at.push(wait.last.get());
                        Ok(())
                    },
                    || 0,
                    REWRITE_EVERY,
                    HANG,
                    &wait,
                );
                assert!(r.is_err(), "tick {ms} ms: a silent log is no re-read");
                at.iter().any(|t| *t >= HANG).then_some((ms, at))
            })
            .collect();
        assert!(
            late.is_empty(),
            "(tick in ms, the readings before each write): {late:?}"
        );
    }

    /// The hook as the scratch settings carry it: `ConfigChange` runs the
    /// script by its quoted path; the script appends its input and a newline to
    /// the log and exits 0. NEGATIVE CONTROL: with the log's directory gone it
    /// still exits 0 (a failing hook would draw an error in the tab) and
    /// writes nothing.
    #[test]
    fn the_hook_runs_the_script_which_appends_its_input_and_never_fails() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path().join("a dir");
        std::fs::create_dir(&dir).unwrap();
        let sh = dir.join(SCRIPT);
        let h = hooks(&sh);
        let cmd = h
            .get("ConfigChange")
            .and_then(J::as_arr)
            .and_then(|a| a.first())
            .and_then(|m| m.get("hooks"))
            .and_then(J::as_arr)
            .and_then(|a| a.first())
            .and_then(|c| c.get("command"))
            .and_then(J::as_str)
            .unwrap()
            .to_string();
        assert_eq!(cmd, format!("'{}'", sh.display()));
        // The input is a file, not a pipe: the negative control's script exits
        // without reading it (its redirection fails first), so a write into a
        // pipe would break whenever the script had already gone.
        let fed = root.path().join("input");
        let run = |log: &Path, input: &str| {
            std::fs::write(&sh, script(log)).unwrap();
            std::fs::write(&fed, input).unwrap();
            aterm_uds::spawnfd::command("/bin/sh")
                .arg(&sh)
                .stdin(std::fs::File::open(&fed).unwrap())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
                .wait()
                .unwrap()
                .code()
        };
        let log = dir.join(LOG);
        let input = line("ConfigChange", SETTINGS);
        assert_eq!(run(&log, &input), Some(0));
        assert_eq!(run(&log, &input), Some(0));
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text, format!("{input}\n{input}\n"));
        assert_eq!(rereads(&text, SETTINGS), 2);
        let gone = root.path().join("gone").join(LOG);
        assert_eq!(run(&gone, &input), Some(0));
        assert!(!gone.exists());
    }
}
