// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// AN IMAP QUOTED STRING HOLDS NO NUL: a NUL in a login or mailbox name is dropped as CR and
/// LF are, and `\` and `"` are escaped. CONTROL: plain text is written as it is.
#[test]
fn a_quoted_imap_string_drops_nul_cr_and_lf() {
    assert_eq!(escape_imap_quoted("a\0b\""), "ab\\\"");
    assert_eq!(escape_imap_quoted("x\r\ny\0\\z"), "xy\\\\z");
    assert_eq!(
        escape_imap_quoted("work@example.test"),
        "work@example.test",
        "CONTROL"
    );
}

/// A pipe read the browser's reader takes once from its script, then the pipe's end.
struct Script(Vec<std::io::Result<&'static [u8]>>);

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() {
            return Ok(0);
        }
        let bytes = self.0.remove(0)?;
        buf[..bytes.len()].copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

/// AN INTERRUPTED READ OF THE BROWSER'S PIPE IS READ AGAIN: the message after it is delivered,
/// not lost with the browser's whole session. CONTROL: a read that fails ends the pump, and
/// nothing after it is delivered.
#[test]
fn the_browser_reader_reads_again_after_an_interrupted_read() {
    let pumped = |script: Script| {
        let (tx, rx) = channel();
        pump_cdp_reader(script, &tx);
        drop(tx);
        rx.iter().collect::<Vec<Value>>()
    };
    let interrupted = std::io::Error::from(std::io::ErrorKind::Interrupted);
    assert_eq!(
        pumped(Script(vec![Err(interrupted), Ok(b"{\"id\":1}\0")])),
        vec![json!({"id": 1})]
    );
    let broke = std::io::Error::other("the pipe broke");
    assert_eq!(
        pumped(Script(vec![Err(broke), Ok(b"{\"id\":1}\0")])),
        Vec::<Value>::new(),
        "CONTROL"
    );
    assert_eq!(
        pumped(Script(vec![Ok(b"{\"id\":2}\0")])),
        vec![json!({"id": 2})],
        "CONTROL: a plain read"
    );
}

/// THE FLEET'S RELAY IS READ ONLY WHERE ITS READER IS BUILT, unix: on, for an email, there.
/// CONTROLS: no email, or the relay off, reads no relay anywhere.
#[test]
fn the_relay_is_read_for_an_email_where_its_reader_is_built() {
    let relay = MailboxConfig {
        relay_enabled: true,
        ..MailboxConfig::default()
    };
    assert_eq!(relay.reads_relay_for(Some("work@example.test")), cfg!(unix));
    assert!(!relay.reads_relay_for(None), "CONTROL: no email");
    assert!(
        !MailboxConfig::default().reads_relay_for(Some("work@example.test")),
        "CONTROL: the relay off"
    );
}
