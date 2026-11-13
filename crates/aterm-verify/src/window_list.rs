// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Developer Tools list as the aterm instance verify runs in reads it, for the run-start
//! line's `(<app> listed: …)` word: verify runs from a checkout, outside any `.app`, so its own
//! read is refused ([`atpkg::devtools::listed_word`]), and the instance's, made inside its bundle,
//! is asked through aterm ctl's own client path.

use std::path::Path;
use std::time::{Duration, Instant};

use atpkg::devtools::WindowList;

/// How long the instance has to answer.
pub const ASK_BOUND: Duration = Duration::from_secs(2);

/// What the instance hosting this process's session (`ATERM_PARENT_SESSION_ID`, found by its
/// discovery graph entry: [`aterm_ctl::hosting_sock`], never another instance) reads of the list,
/// asked once (`privacy --json`, as its owner) on a thread of its own that is waited for at most
/// [`ASK_BOUND`]; `None` with no session, no instance hosting it, or no whole answer in time.
#[must_use]
pub fn hosting_instance() -> Option<WindowList> {
    let sid = std::env::var(aterm_types::domain::ENV_PARENT_SESSION_ID)
        .ok()
        .filter(|sid| !sid.is_empty())?;
    let dir = aterm_uds::control_socket_dir()?;
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("verify-devtools".to_owned())
        .spawn(move || {
            let _ = answer.send(ask(&dir, &sid, Instant::now() + ASK_BOUND));
        })
        .ok()?;
    answered.recv_timeout(ASK_BOUND).ok().flatten()
}

/// One `privacy --json` to the instance `dir`'s graph says hosts `sid`, connected as aterm ctl
/// connects ([`aterm_ctl::connect_then_read_token`]) and asked in its grammar
/// ([`aterm_ctl::exchange_lines`]: its token's `AUTH`, then the request, and the reply's
/// `OK <n>` and `n` lines, one JSON object), every write and read done by `deadline`.
fn ask(dir: &Path, sid: &str, deadline: Instant) -> Option<WindowList> {
    let sid = aterm_types::control_request::Sid::parse(sid)?;
    let sock = aterm_ctl::hosting_sock(dir, sid)?;
    let (stream, token) = aterm_ctl::connect_then_read_token(&sock).ok()?;
    let within = deadline.saturating_duration_since(Instant::now());
    let lines = aterm_ctl::exchange_lines(&stream, &token.ok()?, "privacy --json", within).ok()?;
    devtools_of(&lines.join("\n"))
}

/// `privacy --json`'s `devtools.subject` and `devtools.list`; `None` when either is missing or
/// not a string (an instance with no subject answers `null`).
#[must_use]
pub fn devtools_of(json: &str) -> Option<WindowList> {
    let value = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    let devtools = value.get("devtools")?;
    Some(WindowList {
        subject: devtools.get("subject")?.as_str()?.to_owned(),
        list: devtools.get("list")?.as_str()?.to_owned(),
    })
}

#[cfg(test)]
#[path = "window_list_tests.rs"]
mod tests;
