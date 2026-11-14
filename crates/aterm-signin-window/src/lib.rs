// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's sign-in window: one web page in a small titled window of aterm's own, hosted by
//! the hidden verb [`VERB`] of the `aterm` executable.
//!
//! The sign-in that opens it (`aterm-agent`'s `harness::signin_window`) starts
//! `aterm __sign-in-window`, with no other word, and talks to it over standard input and
//! output, one JSON object per line ([`wire`]): its first line says what to open (the title,
//! the account's data store and the hosts the page may go to, so none of it is in a process
//! listing), and after it it asks for a page to be loaded, for a script's answer, for the
//! store to be emptied, or for the window to close, and hears when a page finished loading
//! and when a person closed the window. The page's sign-in state lives in a website data
//! store of the account's own (a UUID): no account shares a session with another, or with any
//! browser of the person's. A whole page is sent only to the hosts the first line names and
//! to the waiting sign-in's own loopback port; any other navigation is refused before it
//! starts. The window is ordered to the front without making aterm the active application, so
//! it never takes the keyboard from what a person is typing; it has a fixed size and no other
//! chrome than its title and close button.
//!
//! The host decides nothing about a sign-in: what is read, set and pressed is the opener's.

pub mod wire;

#[cfg(target_os = "macos")]
mod host;

/// The hidden first word of `aterm` that runs the window's host ([`main_entry`]).
pub const VERB: &str = "__sign-in-window";

/// The host's exit where this process has no desktop session to open a window in.
pub const EXIT_NO_DESKTOP: u8 = 3;

/// Runs the window until its opener closes it, a person closes it, or its standard input
/// ends; `args` are the words after [`VERB`], and there are none.
pub fn main_entry(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    {
        host::run(args)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        eprintln!("aterm: the sign-in window is macOS only");
        std::process::ExitCode::from(2)
    }
}
