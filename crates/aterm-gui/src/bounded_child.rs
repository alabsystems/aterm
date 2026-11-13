// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{self, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Stdio, Tether as _};

/// The most bytes of a child's stdout, and of its stderr, a read keeps.
pub(crate) const OUTPUT_CAP: u64 = 8 << 20;
/// Timeout for bounded document reads.
pub(crate) const READ_CAP: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const REAP_RESERVE: Duration = Duration::from_secs(60);

/// What a bounded child said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    /// Its exit code; `None` for one a signal ended.
    pub(crate) code: Option<i32>,
    /// Its stdout, at most [`OUTPUT_CAP`] bytes, lossily UTF-8.
    pub(crate) stdout: String,
    /// Its stderr, the same.
    pub(crate) stderr: String,
}

type ReaderWork = Box<dyn FnOnce() + Send + 'static>;

/// A reader started before the child. Dropping its input without a pipe
/// cancels it, so a second reader that cannot start leaves no child running.
struct Reader<R> {
    input: SyncSender<Option<R>>,
    output: Receiver<Vec<u8>>,
}

impl<R> Reader<R> {
    fn hand_off(self, pipe: Option<R>, which: &str) -> Result<Receiver<Vec<u8>>, String> {
        self.input
            .send(pipe)
            .map_err(|_| format!("{which} reader ended before taking its pipe"))?;
        Ok(self.output)
    }
}

fn reader<R: Read + Send + 'static>(
    which: &str,
    start: &mut impl FnMut(&str, ReaderWork) -> io::Result<()>,
) -> Result<Reader<R>, String> {
    let (input, pipe) = sync_channel::<Option<R>>(1);
    let (finished, output) = sync_channel(1);
    start(
        which,
        Box::new(move || {
            let mut buf = Vec::new();
            if let Ok(Some(pipe)) = pipe.recv() {
                let _ = pipe.take(OUTPUT_CAP).read_to_end(&mut buf);
            }
            let _ = finished.send(buf);
        }),
    )
    .map_err(|e| format!("{which} reader did not start: {e}"))?;
    Ok(Reader { input, output })
}

fn collect(output: Receiver<Vec<u8>>, by: Option<Instant>, which: &str) -> Result<String, String> {
    let bytes = match by {
        Some(by) => output
            .recv_timeout(by.saturating_duration_since(Instant::now()))
            .map_err(|_| format!("{which} did not answer before the cap"))?,
        None => output
            .recv()
            .map_err(|_| format!("{which} reader ended without an answer"))?,
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn end(child: &mut aterm_uds::spawnfd::Child) {
    let _ = child.kill();
    let mut exit = aterm_uds::deadline::ExitWait::new(child);
    let by = Instant::now() + REAP_RESERVE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                exit.wait(left);
            }
        }
    }
}

/// Keeps the direct child owned through setup errors and unwinds. An uncut
/// command (no `cap`, such as a bus install) gets no kill request: even on an
/// exceptional exit from its caller it is waited for.
struct RunningChild {
    child: aterm_uds::spawnfd::Child,
    bounded: bool,
    running: bool,
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        if self.running {
            if self.bounded {
                end(&mut self.child);
            } else {
                let _ = self.child.wait();
            }
        }
    }
}

/// Run `program args` with stdin null and stdout/stderr captured, requesting a kill at `cap`. A
/// capped read is tethered: a window that ends, killed outright included, takes it along. An
/// uncut command (no `cap`, such as a bus install) is not: it ends by itself and must not be cut
/// off halfway.
pub(crate) fn run_bounded(
    program: &Path,
    args: &[String],
    cap: Option<Duration>,
) -> Result<Ran, String> {
    run_bounded_observing(program, args, cap, Instant::now, || {})
}

/// The same owned command, with its start clock and observation of a live,
/// unexpired child supplied by the caller.
pub(crate) fn run_bounded_observing(
    program: &Path,
    args: &[String],
    cap: Option<Duration>,
    started: impl FnOnce() -> Instant,
    pending: impl FnMut(),
) -> Result<Ran, String> {
    run_with_readers(
        program,
        args,
        cap,
        |which, work| {
            std::thread::Builder::new()
                .name(format!("aterm-child-{which}"))
                .spawn(work)
                .map(drop)
        },
        started,
        pending,
    )
}

/// `program args` as a command this window runs of its own: stdin null, stdout and stderr
/// captured, and without `ATERM_PARENT_SESSION_ID`. A window started from another aterm's tab
/// inherits that tab's session id, but nothing it runs is run in that tab: the `aterm ctl --host`
/// its menu runs (Take Over, Hand Back, Step In) would otherwise name that tab as the fleet
/// drive's requester (`aterm_ctl::requester_here`, ATERM-KI-1109-3).
fn owned_command(program: &Path, args: &[String]) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .args(args)
        .env_remove(aterm_types::domain::ENV_PARENT_SESSION_ID)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_with_readers(
    program: &Path,
    args: &[String],
    cap: Option<Duration>,
    mut start: impl FnMut(&str, ReaderWork) -> io::Result<()>,
    started: impl FnOnce() -> Instant,
    mut pending: impl FnMut(),
) -> Result<Ran, String> {
    let stdout = reader("stdout", &mut start)?;
    let stderr = reader("stderr", &mut start)?;
    let mut command = owned_command(program, args);
    if cap.is_some() {
        command.tether();
    }
    let child = command
        .spawn()
        .map_err(|e| format!("{} did not start: {e}", program.display()))?;
    let mut owned = RunningChild {
        child,
        bounded: cap.is_some(),
        running: true,
    };
    let stdout = stdout.hand_off(owned.child.stdout.take(), "stdout");
    let stderr = stderr.hand_off(owned.child.stderr.take(), "stderr");
    let (stdout, stderr) = (stdout?, stderr?);
    let deadline = cap.map(|cap| {
        let now = started();
        let at = now
            .checked_add(cap)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
        (at, cap)
    });
    let status = loop {
        match owned.child.try_wait() {
            Ok(Some(status)) => {
                owned.running = false;
                break Ok(status);
            }
            Ok(None) if deadline.is_some_and(|(at, _)| Instant::now() >= at) => {
                let secs = deadline.map_or(0, |(_, cap)| cap.as_secs());
                break Err(format!("no answer in {secs} s: kill requested"));
            }
            Ok(None) => {
                pending();
                std::thread::sleep(POLL);
            }
            Err(e) => {
                break Err(format!("could not wait for it: {e}"));
            }
        }
    };
    let status = status?;
    let by = deadline.map(|(at, _)| at);
    let stdout = collect(stdout, by, "stdout")?;
    let stderr = collect(stderr, by, "stderr")?;
    Ok(Ran {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// Return a backtick fence longer than any backtick run in `text` (at least three).
pub(crate) fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

#[cfg(all(test, unix))]
#[path = "bounded_child_tests.rs"]
mod tests;
