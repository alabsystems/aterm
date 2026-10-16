// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CAN (0x18) and SUB (0x1A) cancel a control string, which then delivers nothing (as in
//! xterm): a program killed mid OSC 52, OSC 8, title, kitty graphics command or
//! DECRQSS/XTGETTCAP query leaves a truncated string that the next CAN or SUB ends. Each
//! case is fed whole, byte by byte and split at every byte, with ST as the negative control.

use aterm_core::terminal::{ClipboardAccess, Terminal};

/// Whole, one byte at a time, and split once at every byte.
fn chunkings(bytes: &[u8]) -> Vec<Vec<&[u8]>> {
    let mut all = vec![vec![bytes], bytes.chunks(1).collect()];
    all.extend((1..bytes.len()).map(|at| {
        let (head, tail) = bytes.split_at(at);
        vec![head, tail]
    }));
    all
}

/// A fresh terminal (clipboard writes authorized) fed `bytes` in `chunks`.
fn fed(chunks: &[&[u8]]) -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.authorize_clipboard_access(ClipboardAccess::Write);
    for chunk in chunks {
        term.process(chunk);
    }
    term
}

/// `body` wrapped as a string introduced by `intro` and ended by `end`.
fn string(intro: &[u8], body: &[u8], end: &[u8]) -> Vec<u8> {
    [intro, body, end].concat()
}

const CANCELS: [&[u8]; 2] = [b"\x18", b"\x1a"];

#[test]
fn a_canceled_osc_52_writes_no_clipboard() {
    for cancel in CANCELS {
        let bytes = string(b"\x1b]52;c;", b"aGk=", cancel);
        for chunks in chunkings(&bytes) {
            let event = fed(&chunks).take_osc_event();
            assert_eq!(
                event, None,
                "{chunks:?}: a canceled OSC 52 set the clipboard"
            );
        }
    }
    let event = fed(&[&string(b"\x1b]52;c;", b"aGk=", b"\x1b\\")]).take_osc_event();
    assert_eq!(event, Some((52, "hi".to_owned())), "ST still delivers");
}

#[test]
fn a_canceled_title_is_not_set() {
    for cancel in CANCELS {
        let bytes = string(b"\x1b]2;", b"half a title", cancel);
        for chunks in chunkings(&bytes) {
            let term = fed(&chunks);
            assert_eq!(
                term.title(),
                "",
                "{chunks:?}: a canceled OSC 2 set the title"
            );
        }
    }
    let term = fed(&[&string(b"\x1b]2;", b"half a title", b"\x07")]);
    assert_eq!(term.title(), "half a title", "BEL still delivers");
}

#[test]
fn a_canceled_hyperlink_is_not_opened() {
    for cancel in CANCELS {
        let mut bytes = string(b"\x1b]8;;", b"https://example.com/x", cancel);
        bytes.extend_from_slice(b"X");
        for chunks in chunkings(&bytes) {
            let term = fed(&chunks);
            assert_eq!(
                term.hyperlink_at(0, 0),
                None,
                "{chunks:?}: the cell after a canceled OSC 8 carries its link"
            );
        }
    }
    let mut bytes = string(b"\x1b]8;;", b"https://example.com/x", b"\x1b\\");
    bytes.extend_from_slice(b"X");
    assert_eq!(
        fed(&[&bytes]).hyperlink_at(0, 0),
        Some("https://example.com/x")
    );
}

/// A canceled DECRQSS or XTGETTCAP query is not answered: the reply would
/// reach whoever reads the PTY next (the shell, after a killed program) as
/// typed input.
#[test]
fn a_canceled_query_is_not_answered() {
    let queries: [(&[u8], &[u8], &[u8]); 3] = [
        (b"\x1bP$q", b"m", b"\x1bP1$r0m\x1b\\"),
        (b"\x1bP$q", b"", b"\x1bP0$r\x1b\\"),
        (b"\x1bP+q", b"544E", b"\x1bP1+r544E="),
    ];
    for (intro, body, answer) in queries {
        for cancel in CANCELS {
            let bytes = string(intro, body, cancel);
            for chunks in chunkings(&bytes) {
                let reply = fed(&chunks).take_response();
                assert_eq!(reply, None, "{chunks:?}: a canceled query was answered");
            }
        }
        let reply = fed(&[&string(intro, body, b"\x1b\\")]).take_response();
        let reply = reply.unwrap_or_default();
        assert!(reply.starts_with(answer), "{body:?}: ST answered {reply:?}");
    }
}

/// A kitty graphics transmit-and-display whose APC is canceled draws nothing,
/// and the canceled command leaves no budget or buffer behind: the complete
/// command after it still draws.
#[test]
fn a_canceled_kitty_command_draws_nothing() {
    let raw = [255u8, 0, 0, 255].repeat(4); // 2x2 px RGBA
    let encoded = aterm_codec::base64::encode(&raw).expect("encode");
    let command = [b"a=T,f=32,s=2,v=2,c=2,r=1,q=2;", encoded.as_bytes()].concat();
    for cancel in CANCELS {
        let bytes = string(b"\x1b_G", &command, cancel);
        for chunks in chunkings(&bytes) {
            let mut term = fed(&chunks);
            assert!(
                term.images_row(0).is_empty(),
                "{chunks:?}: a canceled kitty command drew an image"
            );
            term.process(&string(b"\x1b[2;1H\x1b_G", &command, b"\x1b\\"));
            assert!(
                !term.images_row(1).is_empty(),
                "{chunks:?}: the complete command after the cancel drew nothing"
            );
        }
    }
}

/// A canceled chunk ends the chunked kitty transfer it belonged to, whether
/// it was the final chunk (`m=0`) or a continuation (`m=1`): the transfer
/// draws nothing, and the next program's complete command is not taken as
/// its continuation, so it draws. The same transfer ended by ST draws.
#[test]
fn a_canceled_chunk_ends_its_chunked_transfer() {
    let raw = [255u8, 0, 0, 255].repeat(4); // 2x2 px RGBA
    let encoded = aterm_codec::base64::encode(&raw).expect("encode");
    let (head, rest) = encoded.as_bytes().split_at(8);
    let first = string(b"\x1b_Ga=T,f=32,s=2,v=2,c=2,r=1,q=2,m=1;", head, b"\x1b\\");
    let fresh = [
        &b"\x1b[3;1H\x1b_Ga=T,f=32,s=2,v=2,c=2,r=1,q=2;"[..],
        encoded.as_bytes(),
        b"\x1b\\",
    ]
    .concat();
    for more in [&b"\x1b_Gm=0;"[..], b"\x1b_Gm=1;"] {
        for cancel in CANCELS {
            let canceled = string(more, &rest[..2], cancel);
            for chunks in chunkings(&canceled) {
                let mut term = fed(&[&first]);
                for chunk in &chunks {
                    term.process(chunk);
                }
                assert!(
                    term.images_row(0).is_empty(),
                    "{chunks:?}: a transfer with a canceled chunk drew an image"
                );
                term.process(&fresh);
                assert!(
                    !term.images_row(2).is_empty(),
                    "{chunks:?}: the complete command after a canceled chunk drew nothing"
                );
            }
        }
    }
    let term = fed(&[&first, &string(b"\x1b_Gm=0;", rest, b"\x1b\\")]);
    assert!(
        !term.images_row(0).is_empty(),
        "ST still completes the transfer"
    );
}
