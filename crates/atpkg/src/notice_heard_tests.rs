// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Sink;
use std::cell::RefCell;

/// The two sinks' records, one route at a time.
#[derive(Debug, Default)]
pub(crate) struct Heard {
    /// What the host's aterm.log was handed in process ([`Sink::HostLog`]).
    pub(crate) log: Vec<String>,
    /// What stderr printed ([`Sink::HostReadStderr`], [`Sink::Stderr`]).
    pub(crate) stderr: String,
}

thread_local! {
    static CAPTURE: RefCell<Option<(Sink, Heard)>> = const { RefCell::new(None) };
}

/// Every notice this thread says while `run` runs, routed to `sink`.
pub(crate) fn while_running(sink: Sink, run: impl FnOnce()) -> Heard {
    CAPTURE.with(|c| *c.borrow_mut() = Some((sink, Heard::default())));
    run();
    CAPTURE
        .with(|c| c.borrow_mut().take())
        .map(|(_, heard)| heard)
        .unwrap_or_default()
}

fn logged(line: &str) {
    CAPTURE.with(|c| {
        if let Some((_, heard)) = c.borrow_mut().as_mut() {
            heard.log.push(line.to_owned());
        }
    });
}

/// Route `said`/`log` into the capture, when one is open on this thread.
pub(crate) fn capturing(said: &str, log: &str) -> bool {
    let Some(sink) = CAPTURE.with(|c| c.borrow().as_ref().map(|(sink, _)| *sink)) else {
        return false;
    };
    let mut stderr = Vec::new();
    super::route(said, log, sink, &mut stderr, logged);
    CAPTURE.with(|c| {
        if let Some((_, heard)) = c.borrow_mut().as_mut() {
            heard.stderr.push_str(&String::from_utf8_lossy(&stderr));
        }
    });
    true
}
