// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CALLER'S OWN ATERM VOUCHES FOR A WAIT ANOTHER SERVES: while `aterm ctl await task` waits on
//! a worker another aterm on this Mac hosts, or one on another Mac over the fleet, it holds
//! `await elsewhere` open on its own aterm, the one whose tab it runs in. That aterm parks the
//! await for this process, by its pid and birth, as it parks every await it serves, so its jobs
//! pass reads this process as waiting on aterm rather than as a call that makes no progress. No
//! name or argument of this process is read for it.
//!
//! Each await is clamped to the server's longest wait ([`LEG`]), so the vouch is asked again
//! before the last one ends ([`OVERLAP`]): the next is open before the last is let go, and the
//! aterm never reads a moment with neither. Dropping the [`Vouch`] closes it at once.

use std::io::{self, Write as _};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use aterm_uds::CtlStream;

use super::{Acceptance, TargetOrigin, open_accepted};

/// The longest one vouch asks for: the server's clamp on every `await`.
const LEG: Duration = super::await_task::LEG_MAX;

/// How long before one vouch ends the next is opened.
const OVERLAP: Duration = Duration::from_secs(30);

/// How long opening one vouch may take; past it the wait goes on unvouched until the next leg.
const OPEN_WITHIN: Duration = Duration::from_secs(5);

/// A vouch held on the caller's own aterm until dropped ([`hold`]).
pub(crate) struct Vouch {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Vouch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Holds `await elsewhere` on the aterm at `own` until dropped, each vouch [`LEG`] long and the
/// next opened [`OVERLAP`] before it ends; `None` where no thread starts. A vouch the aterm does
/// not take leaves the wait as it was: it is asked again at the next leg, and nothing is said of
/// it.
pub(crate) fn hold(own: &str) -> Option<Vouch> {
    hold_paced(own, LEG, OVERLAP)
}

fn hold_paced(own: &str, leg: Duration, overlap: Duration) -> Option<Vouch> {
    let own = own.to_string();
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("aterm-ctl-vouch".to_string())
        .spawn(move || {
            let mut held: Option<CtlStream> = None;
            loop {
                // The next vouch opens before the last is let go.
                let next = open(&own, leg);
                held = next.or(held);
                match stopped.recv_timeout(leg.saturating_sub(overlap)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .ok()?;
    Some(Vouch {
        stop: Some(stop),
        thread: Some(thread),
    })
}

/// One `await elsewhere timeout=<ask>` on the aterm at `own`, the connection held open; `None`
/// where it is not taken.
fn open(own: &str, ask: Duration) -> Option<CtlStream> {
    let Ok(Acceptance::Served(stream)) =
        open_accepted(own, TargetOrigin::Pinned, Some(OPEN_WITHIN))
    else {
        return None;
    };
    let line = format!("await elsewhere timeout={}\n", ask.as_millis());
    send(&stream, &line).ok()?;
    Some(stream)
}

fn send(stream: &CtlStream, line: &str) -> io::Result<()> {
    (&*stream).write_all(line.as_bytes())?;
    (&*stream).flush()
}

#[cfg(all(test, unix))]
#[path = "vouch_tests.rs"]
mod tests;
