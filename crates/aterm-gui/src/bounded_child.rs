// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOUNDED CHILD (`docs/DESIGN-owner-surfaces-2026-10-16.md` §1, §3, §4):
//! every `aterm …` command the window reads runs as a CHILD — never in this
//! process, whose descriptor budget the fleet client would spend
//! (`control.rs`'s `control_connections_cap`) — through [`run_bounded`]:
//! stdin null, stdout and stderr prefixes read on threads of their own,
//! each capped at [`OUTPUT_CAP`], and a kill requested at its cap. A bounded read
//! also refuses streams that have not answered by then; it never joins a
//! reader still blocked on a descendant's pipe. The Agents rows' and
//! the menu bar's document tabs (`App::run_cli_plan`), the fleet glance's
//! reader (`fleet_glance::ChildRunner`) and the Overview's read of the
//! fabric's log (`overview_fabric::ChildRunner`) are its callers.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// The most bytes of a child's stdout, and of its stderr, a read keeps.
pub(crate) const OUTPUT_CAP: u64 = 8 << 20;
/// A document read's child/output hang detector. `survey` ends itself first
/// (3 s a fleet peer, `--timeout`); killed-child cleanup has its own reserve.
pub(crate) const READ_CAP: Duration = Duration::from_secs(30);
/// How often [`run_bounded`] looks whether its child has exited.
const POLL: Duration = Duration::from_millis(20);
/// Cleanup may wait this long for the killed direct child, then leaves the
/// existing spawnfd handle/reaper to finish; it does not certify exit.
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

/// Read `pipe` to its end, at most [`OUTPUT_CAP`] bytes, on a thread of its
/// own, so a child that fills one pipe while the other is unread never
/// stalls. A reader still blocked after refusal owns only its stream and
/// bounded buffer; its lifetime is not established by the caller's deadline.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (finished, output) = sync_channel(1);
    let _ = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(OUTPUT_CAP).read_to_end(&mut buf);
        }
        let _ = finished.send(buf);
    });
    output
}

/// Collect one reader under the same absolute deadline as the child and the
/// other stream. No deadline deliberately preserves the uncut command path.
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

/// Kill the owned direct child, then wait only through the cleanup reserve.
/// A retained descendant or an unsettled kernel wait is not claimed dead.
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

/// Run `program args` (stdin null, stdout and stderr read), requesting a kill at
/// `cap` (`None`: a command that must never be cut off midway, such as the
/// message bus's install, waits for its own end). `Err` says why there is no
/// answer: it did not start, could not be waited for, reached the cap, or a
/// stream had not answered by it. Refusal never joins a blocked reader.
pub(crate) fn run_bounded(
    program: &Path,
    args: &[String],
    cap: Option<Duration>,
) -> Result<Ran, String> {
    let mut child = aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{} did not start: {e}", program.display()))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = cap.map(|cap| (Instant::now() + cap, cap));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if deadline.is_some_and(|(at, _)| Instant::now() >= at) => {
                end(&mut child);
                let secs = deadline.map_or(0, |(_, cap)| cap.as_secs());
                break Err(format!("no answer in {secs} s: kill requested"));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                end(&mut child);
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

/// A fence for `text`: one backtick more than its longest run, at least three,
/// so a command's output that holds a fence of its own stays inside the block.
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
mod tests {
    use super::*;

    /// A bounded child that answers is read whole, its exit code with it; one
    /// that never exits is SIGKILLed at the cap and answers `Err`. The hang
    /// detector is a minute; the cap is what ends the wait. NEGATIVE CONTROL:
    /// the answering child is not killed.
    #[test]
    fn a_bounded_child_is_read_or_killed_at_its_cap() {
        // An already-reached deadline is a state, not a latency budget.
        // Ready output is still consumed; an open but unanswered stream is
        // refused without waiting for its sender to disappear.
        let past = Instant::now();
        let (finished, output) = sync_channel(1);
        finished.send(b"ready".to_vec()).unwrap();
        assert_eq!(collect(output, Some(past), "stdout").unwrap(), "ready");
        let (held_open, output) = sync_channel(1);
        assert!(collect(output, Some(past), "stderr").is_err());
        drop(held_open);
        let (finished, output) = sync_channel(1);
        finished.send(b"uncut".to_vec()).unwrap();
        assert_eq!(collect(output, None, "stdout").unwrap(), "uncut");

        let ran = run_bounded(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "echo out; echo err >&2; exit 3".to_string(),
            ],
            Some(Duration::from_secs(60)),
        )
        .expect("it answered");
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (Some(3), "out\n", "err\n")
        );
        let started = Instant::now();
        let hung = run_bounded(
            Path::new("/bin/sleep"),
            &["600".to_string()],
            Some(Duration::from_millis(200)),
        );
        assert!(hung.is_err(), "{hung:?}");
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the cap ended it"
        );
        assert!(
            run_bounded(
                Path::new("/no/such/aterm"),
                &[],
                Some(Duration::from_secs(1))
            )
            .is_err()
        );
    }
}
