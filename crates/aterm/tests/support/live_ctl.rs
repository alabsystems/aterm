// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One persistent control connection to a live headless instance (`aterm_agent::RelayClient`,
//! authenticated with the token beside the socket), and the typed readings the live lanes
//! assert on.
//!
//! A request is one round trip on the open connection, so a poll costs no
//! process launch and two requests reach the server in the order written.
//! Every reply is read under a hang detector ([`READ_HANG`]); a reply that
//! never comes fails the test naming the request.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Duration;

use aterm_agent::RelayClient;
use aterm_uds::CtlStream;

/// A reply not read by now is a hung instance: past the longest server-side
/// wait any request here asks for ([`AWAIT_MS`]).
const READ_HANG: Duration = Duration::from_secs(120);

/// The server-side bound on every `await` here, in ms: a minute, a hang detector
/// (`aterm_spec::wait::MUST_HAPPEN`).
pub const AWAIT_MS: u128 = aterm_spec::wait::MUST_HAPPEN.as_millis();

/// `key=value` tokens of one reply line, by key; a repeated key keeps its
/// first value.
pub fn fields(line: &str) -> BTreeMap<&str, &str> {
    let mut out = BTreeMap::new();
    for tok in line.split_whitespace() {
        if let Some((key, value)) = tok.split_once('=') {
            out.entry(key).or_insert(value);
        }
    }
    out
}

/// The connection.
pub struct Ctl {
    client: RelayClient<CtlStream>,
}

impl Ctl {
    /// Connect to the instance listening at `sock` and authenticate.
    ///
    /// # Panics
    /// When the socket refuses or its token cannot be read: the instance was
    /// booted ready, so either is a failure of the instance.
    pub fn connect(sock: &str) -> Self {
        let client = RelayClient::connect_local_beside(sock)
            .unwrap_or_else(|e| panic!("connect to the instance at {sock}: {e}"));
        client
            .transport()
            .set_read_timeout(Some(READ_HANG))
            .expect("bound the control connection's reads");
        Self { client }
    }

    /// `req`, whose reply is one line (`OK …` or `ERR …`), returned verbatim.
    pub fn line(&mut self, req: &str) -> String {
        self.client
            .request_line(req)
            .unwrap_or_else(|e| panic!("`{req}`: no reply ({e})"))
    }

    /// `req`, which must answer `OK` on one line.
    pub fn ok(&mut self, req: &str) -> String {
        let reply = self.line(req);
        assert!(
            reply == "OK" || reply.starts_with("OK "),
            "`{req}` answered {reply:?}"
        );
        reply
    }

    /// `req`, whose `OK <n>` reply counts `n` body lines: the header and the
    /// lines. It must answer `OK`.
    pub fn counted(&mut self, req: &str) -> (String, Vec<String>) {
        let (header, body) = self
            .client
            .request_counted(req)
            .unwrap_or_else(|e| panic!("`{req}`: no reply ({e})"));
        assert!(header.starts_with("OK "), "`{req}` answered {header:?}");
        (header, body.lines().map(str::to_owned).collect())
    }

    /// `modes`: every terminal mode the instance reports, by name.
    pub fn modes(&mut self) -> BTreeMap<String, String> {
        self.counted("modes")
            .1
            .iter()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    /// `text`: the screen, one string per row.
    pub fn text(&mut self) -> Vec<String> {
        self.counted("text").1
    }

    /// `status`'s one line.
    pub fn status(&mut self) -> String {
        self.ok("status")
    }

    /// The terminal's foreground process group (`who`'s `fgpgid=`), `None`
    /// while the instance cannot read it.
    pub fn fg_pgrp(&mut self) -> Option<i32> {
        self.counted("who")
            .1
            .iter()
            .find_map(|row| fields(row).get("fgpgid")?.parse().ok())
    }

    /// Wait (server-side, a minute) for `re` — one wire token, `.` for a
    /// space — to be on screen; `false` when it never came.
    pub fn await_match(&mut self, re: &str) -> bool {
        let reply = self.line(&format!("await match {re} timeout={AWAIT_MS}"));
        reply.starts_with("OK match ")
    }

    /// [`Self::await_match`] that must succeed.
    #[track_caller]
    pub fn expect_on_screen(&mut self, re: &str) {
        if !self.await_match(re) {
            let screen = self.text().join("\n");
            panic!("`{re}` never reached the screen; the screen:\n{screen}");
        }
    }

    /// Type `text` into the session (no Enter), byte for byte: a `feed-bin`
    /// frame, so nothing in it is read as an option or rewritten, as the
    /// inline `send <text>` form would.
    pub fn send(&mut self, text: &str) {
        let reply = self
            .client
            .request_frame("feed-bin", text.as_bytes())
            .unwrap_or_else(|e| panic!("feed-bin {text:?}: no reply ({e})"));
        assert!(
            reply.starts_with("OK"),
            "feed-bin {text:?} answered {reply:?}"
        );
    }

    /// Type `line` and press Enter.
    pub fn type_line(&mut self, line: &str) {
        self.send(line);
        self.ok("key enter");
    }
}
