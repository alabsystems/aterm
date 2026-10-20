// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN `@` THAT NAMES NOTHING NEVER REACHES A SESSION — on the wire, against a
//! real `aterm --headless`. The 2026-10-16 real-login run found `aterm ctl @
//! task` (what `@$W` becomes when `$W` is empty) answering for the front
//! session; for `send`, `key`, `signal`, `close` or `turn` an agent's failed
//! sid parse would type into, or stop, the person's focused tab. `aterm ctl`
//! refuses such a selector before it dials (its own lib tests); this lane is
//! the server's half: a client that sends one anyway is refused with the one
//! shared line (`aterm_types::control_verbs::EMPTY_SELECTOR`), a leading one
//! closes the connection, and the front session sees nothing of it.
//!
//! NEGATIVE CONTROL: the same word typed through `@.` — the active tab named
//! outright — reaches the same session and is read back off its screen, so the
//! first assertion is about the refusal, not about a session that could not
//! have shown the word anyway.
//!
//! Every wait is a one-minute hang detector. A machine that refuses the
//! headless instance is COULD NOT RUN ([`headless_boot::boot_with`]).

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
    // A leading bare `@` before each authority class a failed sid parse
    // reaches: the one line, then the connection closes.
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
    // A list element that names nothing, where the selector is not leading,
    // beside one that names the live session.
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
    // The front session saw none of it and is still there: the refused
    // `send` typed nothing, the refused `signal`/`close` stopped nothing.
    let mut ctl = live_ctl::Ctl::connect(&inst.sock);
    let screen = ctl.text().join("\n");
    assert!(!screen.contains(WORD), "nothing was typed:\n{screen}");
    // NEGATIVE CONTROL: `@.` names the active tab, and the word arrives.
    ctl.ok(&format!("@. send {WORD}"));
    ctl.expect_on_screen(WORD);
}
