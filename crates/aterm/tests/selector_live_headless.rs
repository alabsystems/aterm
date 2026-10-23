// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live headless wire test that an empty `@` selector is refused ([`headless_boot::boot_with`]).

#![cfg(unix)]

use std::io::ErrorKind;
use std::time::Duration;

use aterm_agent::RelayClient;
use aterm_uds::CtlStream;

use crate::{headless_boot, live_ctl};

/// A reply not read by now is a hung instance (AGENTS.md: a hang detector).
const HANG: Duration = Duration::from_secs(60);

/// The word the refused `send` would have typed, and the control types.
const WORD: &str = "aterm-empty-selector-word";

/// A fresh authenticated connection, its reads bounded by [`HANG`].
fn connect(inst: &headless_boot::Instance) -> RelayClient<CtlStream> {
    let client = RelayClient::connect_local_beside(&inst.sock).expect("connect and authenticate");
    client
        .transport()
        .set_read_timeout(Some(HANG))
        .expect("bound the reads");
    client
}

#[test]
fn an_at_that_names_nothing_is_refused_on_the_wire_and_reaches_no_session() {
    let inst = headless_boot::boot_with("atsl", |_, _| {}, |_| true);
    let refusal = aterm_types::control_verbs::EMPTY_SELECTOR;
    for line in [
        format!("@ send {WORD}"),
        "@ task".to_string(),
        "@  key enter".to_string(),
        "@ signal term".to_string(),
        "@ close".to_string(),
        "@ turn typed=1 x".to_string(),
    ] {
        let mut c = connect(&inst);
        assert_eq!(c.request_line(&line).expect("a reply"), refusal, "{line:?}");
        assert_eq!(
            c.next_pushed().map_err(|e| e.kind()),
            Err(ErrorKind::UnexpectedEof),
            "{line:?}: the connection closes after the refusal"
        );
    }
    let (local, sid) = headless_boot::boot_session(&inst);
    for line in [
        "subscribe @ events".to_string(),
        format!("subscribe @{local},@ events"),
        "await task @".to_string(),
        format!("await task @{sid},@"),
    ] {
        let mut c = connect(&inst);
        assert_eq!(c.request_line(&line).expect("a reply"), refusal, "{line:?}");
    }
    let mut ctl = live_ctl::Ctl::connect(&inst.sock);
    let screen = ctl.text().join("\n");
    assert!(!screen.contains(WORD), "nothing was typed:\n{screen}");
    ctl.ok(&format!("@. send {WORD}"));
    ctl.expect_on_screen(WORD);
}
