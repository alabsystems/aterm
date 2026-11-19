// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The original forked watcher's private arming receipt. This is carried by
//! its actual spawning Child, never recovered from a name, image or listing.

use crate::process::{Birth, Identity, ProcessPresence};
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const FRAME_LEN: usize = 24;

pub(crate) fn encode(identity: Identity) -> [u8; FRAME_LEN] {
    let mut frame = [0; FRAME_LEN];
    frame[..4].copy_from_slice(b"ATW1");
    frame[4..8].copy_from_slice(&identity.pid.to_ne_bytes());
    frame[8..16].copy_from_slice(&identity.birth.0.to_ne_bytes());
    frame[16..20].copy_from_slice(&identity.idversion.unwrap_or(0).to_ne_bytes());
    frame[20..24].copy_from_slice(&u32::from(identity.idversion.is_some()).to_ne_bytes());
    frame
}

pub(crate) fn decode(frame: &[u8]) -> io::Result<Identity> {
    if frame.len() != FRAME_LEN || &frame[..4] != b"ATW1" {
        return Err(io::Error::other("incomplete tether watcher receipt"));
    }
    let pid = i32::from_ne_bytes(frame[4..8].try_into().expect("fixed frame"));
    let birth = Birth(u64::from_ne_bytes(
        frame[8..16].try_into().expect("fixed frame"),
    ));
    let version = i32::from_ne_bytes(frame[16..20].try_into().expect("fixed frame"));
    let has_version = u32::from_ne_bytes(frame[20..24].try_into().expect("fixed frame"));
    if pid <= 1 || birth.0 == 0 || has_version != u32::from(crate::process::KEEPS_PID_VERSIONS) {
        return Err(io::Error::other("invalid tether watcher identity"));
    }
    Ok(Identity {
        pid,
        birth,
        idversion: (has_version == 1).then_some(version),
    })
}

/// Only called by the actual single-threaded fork on its inherited private
/// arming lane, after it has armed. The program it will protect never holds
/// this lane. Failure starts no requested program.
pub(super) fn arm(fd: i32) -> bool {
    // SAFETY: getpid takes nothing and cannot fail.
    let pid = unsafe { libc::getpid() };
    let Some(lineage) = crate::process::read_lineage(pid) else {
        return false;
    };
    let frame = encode(lineage.identity);
    let mut written = 0;
    while written < frame.len() {
        // SAFETY: the fork owns this inherited lane; the remaining frame is live.
        let got =
            unsafe { libc::write(fd, frame[written..].as_ptr().cast(), frame.len() - written) };
        if got > 0 {
            written += usize::try_from(got).expect("positive write");
        } else if got == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
    true
}

/// Shared only between one actual Child and its delayed exec observation.
/// Sealing a post-birth receipt before observing exec must not lose this
/// original helper's custody. An absent observation is never successful closure.
#[derive(Clone, Debug, Default)]
pub(crate) struct Receipt(Arc<OnceLock<Result<Identity, String>>>);

impl Receipt {
    pub(crate) fn record(&self, identity: io::Result<Identity>) {
        let _ = self.0.set(identity.map_err(|error| error.to_string()));
    }

    pub(crate) fn finish(
        &self,
        window: Duration,
        check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let identity = match self.0.get() {
            Some(Ok(identity)) => *identity,
            Some(Err(why)) => return Err(io::Error::other(why.clone())),
            None => return Err(io::Error::other("tether watcher was not observed")),
        };
        finish_using(identity, window, &mut Native::default(), check)
    }
}

trait View {
    fn identity(&mut self, pid: i32) -> Option<Identity>;
    fn presence(&mut self, pid: i32) -> ProcessPresence;
    fn progress(&mut self, pid: i32) -> Option<u64>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self, duration: Duration);
}

#[derive(Default)]
struct Native(crate::deadline::ProcessProgressProbe);

impl View for Native {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        crate::process::read_lineage(pid).map(|lineage| lineage.identity)
    }
    fn presence(&mut self, pid: i32) -> ProcessPresence {
        crate::process::process_presence(pid)
    }
    fn progress(&mut self, pid: i32) -> Option<u64> {
        self.0.read(pid.unsigned_abs())
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Only positive absence or a different birth discharges the original fork.
/// Exec is not closure, and an exited but unreaped watcher still needs absence.
fn closed(identity: Identity, view: &mut impl View) -> io::Result<bool> {
    if let Some(current) = view.identity(identity.pid) {
        if current.birth != identity.birth {
            return Ok(true);
        }
        if current != identity {
            return Err(io::Error::other("original tether watcher changed image"));
        }
        return Ok(false);
    }
    match view.presence(identity.pid) {
        ProcessPresence::Absent => Ok(true),
        ProcessPresence::Exited => Ok(false),
        ProcessPresence::Present | ProcessPresence::Unknown => Err(io::Error::other(
            "original tether watcher identity is unreadable",
        )),
    }
}

fn finish_using(
    identity: Identity,
    window: Duration,
    view: &mut impl View,
    mut check: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut progress = crate::deadline::ProgressWindow::new(window, view.now());
    loop {
        check()?;
        if closed(identity, view)? {
            return Ok(());
        }
        // Bracket real work by the original native identity. The observer's
        // CPU, elapsed wall time and the arming receipt are never progress.
        let work = view.progress(identity.pid);
        if closed(identity, view)? {
            return Ok(());
        }
        let now = view.now();
        let left = progress.observe(now, 0, || work);
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original tether watcher did not retire during its inactivity window",
            ));
        }
        // None disarms: incomplete or changing native trees do not prove a
        // stall. Cancellation still runs on this owning thread every turn.
        view.pause(left.min(Duration::from_millis(25)));
    }
}

#[cfg(test)]
#[path = "tether_receipt_tests.rs"]
mod tests;
