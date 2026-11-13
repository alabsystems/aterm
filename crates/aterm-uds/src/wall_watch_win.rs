// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wall watch's Windows arm: the power manager's suspend and resume announcements
//! (`PowerRegisterSuspendResumeNotification` with a callback, through
//! [`aterm_sysprobe::power::windows::listen`]: no window and no message loop), which ring an
//! event a loop waits on beside its own ([`Doorbell`], [`any`]).
//!
//! The sleep's length is read off two clocks between the suspend and the resume: the wall
//! clock, and the unbiased interrupt time, which stops while the machine is suspended. std's
//! `Instant` is no such clock here: the performance counter keeps counting through a suspend.
//!
//! Windows announces a set of the wall clock to windows only (`WM_TIMECHANGE`), so nothing here
//! hears a step ([`super::STEPS_ANNOUNCED`] is false): an owner that needs one keeps reading
//! its clocks for it.

use super::Heard;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_sysprobe::power::windows::{PowerEvent, listen, unbiased_time};

use crate::win::ffi::{CloseHandle, RawHandle, WaitForSingleObject};

/// `WAIT_OBJECT_0`: the first handle is signalled.
const WAIT_OBJECT_0: u32 = 0;
/// `WAIT_TIMEOUT`: the wait's time ran out.
const WAIT_TIMEOUT: u32 = 258;
/// `INFINITE`: a wait with no deadline.
const INFINITE: u32 = u32::MAX;
/// `MAXIMUM_WAIT_OBJECTS`: the most handles one wait takes.
const MAXIMUM_WAIT_OBJECTS: usize = 64;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        lpEventAttributes: *const core::ffi::c_void,
        bManualReset: i32,
        bInitialState: i32,
        lpName: *const u16,
    ) -> RawHandle;
    fn SetEvent(hEvent: RawHandle) -> i32;
    fn ResetEvent(hEvent: RawHandle) -> i32;
    fn WaitForMultipleObjects(
        nCount: u32,
        lpHandles: *const RawHandle,
        bWaitAll: i32,
        dwMilliseconds: u32,
    ) -> u32;
}

/// A wake any thread rings, as often as it likes, for a loop that waits on it beside others
/// ([`any`]): a manual-reset event that [`Doorbell::ring`] sets and [`Doorbell::clear`] resets,
/// the twin of Unix's pipe doorbell (`aterm_uds::ready::Doorbell`). It stays rung until
/// cleared, so a wait that found it rung does not take the ring. The loop clears it before it
/// reads the state a ring announces, so a ring that comes during that read wakes its next wait.
#[derive(Debug)]
pub struct Doorbell {
    event: RawHandle,
}

// SAFETY: an event handle is a kernel object any thread may set, reset and wait on.
unsafe impl Send for Doorbell {}
// SAFETY: as above; no method needs exclusive access.
unsafe impl Sync for Doorbell {}

impl Doorbell {
    /// A doorbell not rung: an unnamed event no child process inherits.
    ///
    /// # Errors
    /// The event could not be made.
    pub fn new() -> io::Result<Self> {
        // SAFETY: no security attributes (so the handle is not inheritable) and no name;
        // manual reset, not signalled.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { event })
    }

    /// Ring: every wait on it ([`any`]) wakes until [`Self::clear`].
    pub fn ring(&self) {
        // SAFETY: the event this owns.
        let _ = unsafe { SetEvent(self.event) };
    }

    /// Take every ring so far: `true` when it was rung since the last clear.
    pub fn clear(&self) -> bool {
        let rung = self.rung();
        // SAFETY: the event this owns.
        let _ = unsafe { ResetEvent(self.event) };
        rung
    }

    /// Whether it is rung now, taking nothing.
    fn rung(&self) -> bool {
        // SAFETY: the event this owns, asked with no wait.
        unsafe { WaitForSingleObject(self.event, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        // SAFETY: the handle this owns, closed once.
        let _ = unsafe { CloseHandle(self.event) };
    }
}

/// Wait until one of `bells` is rung or `deadline` comes (`None`: no deadline), taking no
/// ring. Returns the rung ones' indices; an empty vector means the deadline came.
///
/// # Errors
/// No bell or more than 64, or the wait failed.
pub fn any(bells: &[&Doorbell], deadline: Option<Instant>) -> io::Result<Vec<usize>> {
    if bells.is_empty() || bells.len() > MAXIMUM_WAIT_OBJECTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a wait takes 1 to 64 doorbells",
        ));
    }
    let handles: Vec<RawHandle> = bells.iter().map(|bell| bell.event).collect();
    loop {
        // The longest timed wait is one below `INFINITE`; a deadline further off is waited for
        // again.
        let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let millis = left.map_or(INFINITE, |left| {
            // Rounded up, so a wait never ends before its deadline and spins to it.
            let millis = left.as_nanos().div_ceil(1_000_000);
            u32::try_from(millis).map_or(INFINITE - 1, |millis| millis.min(INFINITE - 1))
        });
        // SAFETY: `handles` holds `len` live event handles, borrowed from `bells` for the call.
        let woke =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, millis) };
        if woke == WAIT_TIMEOUT {
            if left.is_some_and(|left| left > Duration::from_millis(u64::from(millis))) {
                continue;
            }
            return Ok(Vec::new());
        }
        if (woke as usize) < handles.len() {
            // The wait names the first rung bell only: ask each, so one wake reports all.
            return Ok(bells
                .iter()
                .enumerate()
                .filter(|(index, bell)| *index == woke as usize || bell.rung())
                .map(|(index, _)| index)
                .collect());
        }
        return Err(io::Error::last_os_error());
    }
}

/// Never made: nothing announces a step to a process without a window here.
#[derive(Debug)]
pub(super) enum Steps {}

impl Steps {
    pub(super) fn start() -> Option<Self> {
        None
    }

    pub(super) fn take(&self) -> bool {
        match *self {}
    }
}

/// The suspend and resume registration; it hears until dropped.
pub(super) type Listening = aterm_sysprobe::power::windows::Listening;

/// Both clocks a sleep is measured on: the time this machine has been awake since boot, and
/// the wall.
type Clocks = (Duration, SystemTime);

fn clocks() -> Option<Clocks> {
    Some((unbiased_time()?, SystemTime::now()))
}

/// The wall clock's run past the awake clock from `asleep` to `woke`: how long the machine was
/// suspended, or `None` where the wall went backwards meanwhile (it was set back) and says
/// nothing.
pub(super) fn slept_between(asleep: Clocks, woke: Clocks) -> Option<Duration> {
    let awake = woke.0.saturating_sub(asleep.0);
    let wall = woke.1.duration_since(asleep.1).ok()?;
    Some(wall.saturating_sub(awake))
}

/// The suspend and resume listener whose announcements ring `heard`: a suspend notes both
/// clocks, and the resume after it measures the sleep by them. The callback runs on a thread of
/// the power manager's and only notes, rings and returns.
pub(super) fn power(heard: &Arc<Heard>) -> Option<Listening> {
    let heard = Arc::clone(heard);
    let asleep_at: Mutex<Option<Clocks>> = Mutex::new(None);
    listen(Box::new(move |event| match event {
        PowerEvent::Suspend => {
            *asleep_at.lock().unwrap_or_else(PoisonError::into_inner) = clocks();
        }
        PowerEvent::Resume => {
            let asleep = asleep_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let slept = asleep
                .zip(clocks())
                .and_then(|(asleep, woke)| slept_between(asleep, woke));
            heard.resumed(slept);
        }
    }))
}

#[cfg(test)]
#[path = "wall_watch_win_tests.rs"]
mod tests;
