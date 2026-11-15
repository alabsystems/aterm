// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Coherent control-query reads over the existing nonblocking main-thread transport.

use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::control::control_media::{MainHopError, MainPost, main_thread_refusal};

/// One hang detector for the whole query, including every pending sample.
const MAIN_THREAD_REPLY_HANG: std::time::Duration = std::time::Duration::from_secs(60);
const MAIN_THREAD_REPLY_LATE: &str = "main-thread reply did not arrive within 60s";

/// Waits for a main-thread read whose reply may describe a pending sample. The caller projects
/// each reply into a sample, pending state, or terminal refusal without changing its event's
/// reply type. Each hop remains nonblocking on the UI thread, under one shared hang detector.
pub(super) fn call_main_ready<T, R>(
    proxy: &EventLoopProxy<Wake>,
    make: impl FnMut(std::sync::mpsc::Sender<R>) -> Wake,
    ready: impl FnMut(R) -> Result<Option<T>, MainHopError>,
) -> Result<T, MainHopError> {
    read_main_ready(
        &mut MainPost::new(proxy),
        make,
        ready,
        std::time::Instant::now(),
    )
}

fn read_main_ready<T, R>(
    post: &mut MainPost<'_>,
    mut make: impl FnMut(std::sync::mpsc::Sender<R>) -> Wake,
    mut ready: impl FnMut(R) -> Result<Option<T>, MainHopError>,
    started: std::time::Instant,
) -> Result<T, MainHopError> {
    loop {
        if let Some(refusal) = main_thread_refusal() {
            return Err(refusal);
        }
        main_reply_remaining(started)?;
        let (answer, _hop) = post.ask(&mut make)?;
        if let Some(sample) = ready(recv_main_at(&answer, started)?)? {
            return Ok(sample);
        }
        // A busy producer must not turn a read into a stream of unpaced UI events.
        std::thread::sleep(main_reply_remaining(started)?.min(std::time::Duration::from_millis(5)));
    }
}

fn recv_main_at<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    started: std::time::Instant,
) -> Result<T, MainHopError> {
    let within = main_reply_remaining(started)?;
    match rx.recv_timeout(within) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(MainHopError::Reason(MAIN_THREAD_REPLY_LATE))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(MainHopError::Reason("main-thread reply dropped"))
        }
    }
}

fn main_reply_remaining(started: std::time::Instant) -> Result<std::time::Duration, MainHopError> {
    MAIN_THREAD_REPLY_HANG
        .checked_sub(started.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or(MainHopError::Reason(MAIN_THREAD_REPLY_LATE))
}

#[cfg(test)]
#[path = "control_query_read_ready_tests.rs"]
mod tests;
