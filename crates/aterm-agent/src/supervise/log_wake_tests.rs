// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A `status` line as the host prints it: the log's words before the `seq=` stamp.
const LIVE: &str = "OK schema=1 sid=s-1 phase=running hold=0 program=claude agent=busy \
                    agent_detail=- log_seq=812 log_ep=1760000000 run=r3 turn=t17 step=p204 need=- \
                    need_since_ms=- rollout=- seq=104 hash=0\n";

/// [`LogHead::of_status`] reads the cursor and the open turn from the log's own words only.
#[test]
fn a_status_reply_names_the_logs_cursor_and_its_open_turn() {
    assert_eq!(
        LogHead::of_status(LIVE),
        Some(LogHead {
            seq: 812,
            ep: 1_760_000_000,
            base: None,
            turn_open: true,
            need_open: false,
            reader_owes: true,
        })
    );
    assert!(LogHead::of_status(LIVE).is_some_and(|h| h.owes_a_wake()));
    let rested = LIVE.replace("turn=t17", "turn=-");
    assert_eq!(
        LogHead::of_status(&rested).map(|h| h.turn_open),
        Some(false),
        "{rested}"
    );
    assert_eq!(
        LogHead::of_status("OK schema=1 sid=s-1 phase=running seq=104 hash=0\n"),
        None,
        "an older host's status has no log"
    );
    let foreign = LIVE
        .replace("hold=0", "hold=0 turn=t99")
        .replace("turn=t17", "turn=-");
    assert_eq!(
        LogHead::of_status(&foreign).map(|h| h.turn_open),
        Some(false),
        "a turn= ahead of log_seq= is not the log's: {foreign}"
    );
    assert_eq!(
        LogHead::of_status(&LIVE.replace("log_ep=1760000000", "log_ep=x")),
        None,
        "a cursor with no epoch is none"
    );
    let named = LIVE.replace("log_ep=1760000000", "log_ep=1760000000 log_base=1759999990");
    assert_eq!(
        LogHead::of_status(&named).map(|h| (h.ep, h.base, h.turn_open, h.need_open)),
        Some((1_760_000_000, Some(1_759_999_990), true, false)),
        "a host's log_base= names the log, and the words after it still read: {named}"
    );
    assert_eq!(
        LogHead::of_status(&named.replace("log_base=1759999990", "log_base=x")),
        None,
        "a log_base= that does not parse names no log"
    );
    let foreign = LIVE.replace("hold=0", "hold=0 log_base=7");
    assert_eq!(
        LogHead::of_status(&foreign).map(|h| h.base),
        Some(None),
        "a log_base= ahead of log_seq= is not the log's: {foreign}"
    );
    assert_eq!(
        LogHead::of_status(&format!("aterm-ctl: ERR usage\n{LIVE}")).map(|h| h.seq),
        Some(812),
        "the reply's OK line is read"
    );
}

/// A head that already shows what the wait would wake for (an open need, or a reader word
/// that owes no record) owes no wake; `idle` still owes one, and a `need=` ahead of the
/// log's words is not the log's.
#[test]
fn a_head_that_already_names_the_wake_owes_none() {
    let boxed = LIVE.replace("need=-", "need=approval:bash");
    let head = LogHead::of_status(&boxed).expect("a head");
    assert!(head.need_open && !head.owes_a_wake(), "{boxed}");
    for word in ["unknown", "prompt", "wall:unresponsive", "survey", "-"] {
        let read = LIVE.replace("agent=busy", &format!("agent={word}"));
        let head = LogHead::of_status(&read).expect("a head");
        assert!(!head.reader_owes && !head.owes_a_wake(), "{read}");
    }
    let none = LIVE.replace("agent=busy agent_detail=- ", "");
    assert!(
        LogHead::of_status(&none).is_some_and(|h| !h.owes_a_wake()),
        "{none}"
    );
    let idle = LIVE.replace("agent=busy", "agent=idle");
    assert!(
        LogHead::of_status(&idle).is_some_and(|h| h.owes_a_wake()),
        "{idle}"
    );
    let foreign = LIVE.replace("hold=0", "hold=0 need=approval:bash");
    assert!(
        LogHead::of_status(&foreign).is_some_and(|h| h.owes_a_wake()),
        "a need= ahead of log_seq= is not the log's: {foreign}"
    );
}
