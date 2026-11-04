// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the askpass helper's reply codec — behaviour only.

use super::*;

#[test]
fn decode_secret_round_trips() {
    let s = decode_secret("7077").expect("valid hex");
    assert_eq!(s.as_bytes(), b"pw");
}

#[test]
fn decode_secret_empty_is_empty() {
    let s = decode_secret("").expect("empty is valid");
    assert!(s.is_empty());
}

#[test]
fn decode_secret_rejects_malformed_and_line_cutting_bytes() {
    assert!(decode_secret("abc").is_none(), "odd length");
    assert!(decode_secret("zz").is_none(), "non-hex");
    assert!(decode_secret("0a").is_none(), "LF would cut the line");
    assert!(decode_secret("0d").is_none(), "CR would cut the line");
    assert!(decode_secret("00").is_none(), "NUL");
    assert!(
        decode_secret(&"61".repeat(1100)).is_none(),
        "over sudo's 1023-byte read"
    );
}

#[test]
fn an_unknown_reply_never_reaches_the_refusal_diagnostic() {
    let unknown = b"ERR askpass secret=public-synthetic-canary";
    let shown = refusal_reply(unknown);
    assert!(!shown.contains("public-synthetic-canary"), "{shown}");
    assert_eq!(refusal_reply(b"ERR askpass timeout"), "ERR askpass timeout");
}

#[test]
fn the_server_fixed_refusal_reasons_reach_the_diagnostic() {
    for reason in ["off", "owner-only", "debugger-as-root"] {
        let line = format!("ERR askpass refused {reason}");
        assert_eq!(refusal_reply(line.as_bytes()), line);
    }
    assert_eq!(
        refusal_reply(b"ERR askpass refused the peer is not aterm"),
        "ERR askpass refused"
    );
}

#[test]
fn sudo_output_is_exactly_one_plain_line_and_empty_writes_nothing() {
    let mut out = Vec::new();
    write_password(&mut out, &Secret::from_bytes(b"public-synthetic")).unwrap();
    assert_eq!(out, b"public-synthetic\n");
    write_password(&mut out, &Secret::from_bytes(b"")).unwrap();
    assert_eq!(out, b"public-synthetic\n");
    assert_eq!(
        refusal_reply(b"ERR askpass refused public-synthetic"),
        "ERR askpass refused"
    );
}

#[cfg(unix)]
#[test]
fn dedicated_reply_transport_accepts_a_complete_line_and_refuses_partial_eof() {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    for (reply, valid) in [
        (b"OK askpass secret=7077\n".as_slice(), true),
        (b"OK askpass secret=7077", false),
    ] {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(reply).unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        let result = read_answer_within(&reader, Duration::from_secs(60));
        if valid {
            assert_eq!(result.unwrap().as_bytes(), reply);
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        }
    }
}

/// A plain sudo's refusal names the route that works, and the refusals an agent can act on
/// stay distinct from the generic one. NEGATIVE CONTROL: another refusal's free text never passes.
#[test]
fn the_refusals_an_agent_can_act_on_say_what_to_do() {
    let unreadable = refusal_reply(b"ERR askpass refused command-unreadable");
    assert!(unreadable.starts_with("ERR askpass refused command-unreadable"));
    assert!(unreadable.contains("aterm ctl sudo -- <command>"));
    assert_eq!(
        refusal_reply(b"ERR askpass refused not-frontmost; focus the tab and retry"),
        "ERR askpass refused not-frontmost; focus the tab and retry"
    );
    assert!(
        refusal_reply(b"ERR askpass refused unattributed")
            .starts_with("ERR askpass refused unattributed: ")
    );
    assert!(
        refusal_reply(b"ERR askpass refused headless")
            .starts_with("ERR askpass refused headless: ")
    );
    assert!(
        refusal_reply(b"ERR askpass refused no window hosts that session")
            .starts_with("ERR askpass refused no window hosts that session")
    );
    assert_eq!(
        refusal_reply(b"ERR askpass refused anything else"),
        "ERR askpass refused"
    );
}

/// A read of a nonblocking stream says `WouldBlock` at once, so an answer that arrives here was
/// waited for in `poll(2)`, where a host that ends cannot pass it by.
#[cfg(unix)]
#[test]
fn an_answer_is_waited_for_in_poll() {
    let (near, far) = aterm_uds::kept_out(crate::CtlStream::pair).unwrap();
    near.set_nonblocking(true).unwrap();
    let answering = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        (&far).write_all(b"ok\n").unwrap();
        far
    });
    let answer = read_answer_within(&near, Duration::from_secs(60)).unwrap();
    assert_eq!(answer.as_bytes(), b"ok\n");
    drop(answering.join().unwrap());
}

/// An answer left behind by a host that has closed is still read, though macOS then refuses the
/// timeout option.
#[cfg(unix)]
#[test]
fn an_answer_outlives_the_host_that_sent_it() {
    let (near, far) = aterm_uds::kept_out(crate::CtlStream::pair).unwrap();
    (&far).write_all(b"ERR refused\n").unwrap();
    drop(far);
    let answer = read_answer_within(&near, Duration::from_secs(60)).unwrap();
    assert_eq!(answer.as_bytes(), b"ERR refused\n");
}
