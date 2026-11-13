// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The reads one slice makes through counting readers, and what the
/// engine can take of them: (pasteboard reads, queries answered, medium
/// reads by name).
fn reads(slice: &[u8], flags: Flags) -> (usize, usize, Vec<String>) {
    let mut pasteboard = 0;
    let mut media = Vec::new();
    let _burst = Burst::begin_with(
        slice,
        flags,
        || {
            pasteboard += 1;
            "board".to_string()
        },
        |_, name| {
            media.push(name.to_string());
            Some(vec![1])
        },
        |_, _, _| {},
    );
    let answered = std::iter::from_fn(take_clipboard_query)
        .inspect(|board| assert_eq!(board, "board"))
        .count();
    (pasteboard, answered, media)
}

const ALL: Flags = Flags {
    clipboard_query: true,
    kitty_media: true,
};

/// A Kitty command naming `path` (base64 in its payload, as the protocol
/// carries it), ST-terminated.
fn file(control: &str, path: &str) -> Vec<u8> {
    let mut apc = format!("\x1b_G{control};").into_bytes();
    apc.extend_from_slice(
        aterm_codec::base64::encode(path.as_bytes())
            .unwrap()
            .as_bytes(),
    );
    apc.extend_from_slice(b"\x1b\\");
    apc
}

/// A slice is read for exactly the requests the engine sends the host (an OSC 52 query, BEL-
/// or ST-terminated, with one pasteboard read for all of a slice's queries; a Kitty transmit
/// naming a file, whole or cut across two slices) and for nothing else: an OSC 52 set, a
/// canceled query, a Kitty query or chunked load, or anything the session does not admit.
#[test]
fn a_burst_is_read_for_the_requests_the_engine_will_make() {
    let mut burst = b"text\x1b]52;c;?\x07more\x1b]52;p;?\x1b\\".to_vec();
    burst.extend_from_slice(&file("a=T,f=100,t=f", "/tmp/a.png"));
    assert_eq!(reads(&burst, ALL), (1, 2, vec!["/tmp/a.png".to_string()]));

    let mut ignored = b"\x1b]52;c;cGxhbnRlZA==\x07\x1b]52;c;?\x18\x1b]0;?\x07".to_vec();
    ignored.extend_from_slice(&file("a=q,t=f", "/tmp/q.png"));
    ignored.extend_from_slice(&file("a=T,t=f,m=1", "/tmp/chunk"));
    ignored.extend_from_slice(&file("a=T,f=100", "direct"));
    assert_eq!(reads(&ignored, ALL), (0, 0, vec![]));
    assert_eq!(reads(&burst, Flags::default()), (0, 0, vec![]));

    for cut in 1..burst.len() {
        let (head, tail) = burst.split_at(cut);
        let first = reads(head, ALL);
        let second = reads(tail, ALL);
        assert_eq!(
            (first.1 + second.1, [first.2, second.2].concat()),
            (2, vec!["/tmp/a.png".to_string()]),
            "cut at {cut}"
        );
    }
}

thread_local! {
    /// What [`record`] was asked to consume on this thread.
    static CONSUMED: RefCell<Vec<(String, bool)>> = const { RefCell::new(Vec::new()) };
}

/// A consumer that records what it is asked to consume.
fn record(_: KittyMedium, name: &str, read: bool) {
    CONSUMED.with(|consumed| consumed.borrow_mut().push((name.to_string(), read)));
}

/// A MEDIUM IS CONSUMED ONLY ONCE THE ENGINE TOOK IT, AFTER THE SLICE IS
/// FED: of the `t=t` files read for a slice, the one the
/// engine asks for is consumed, with whether its read returned bytes, when
/// the burst drops (after the hold), never at the read or under the hold;
/// the one it never asks for (a raw transmission declaring no size, which
/// the engine refuses before resolving) is not consumed at all.
#[test]
fn only_a_medium_the_engine_took_is_consumed_after_the_slice() {
    let mut burst = file("a=T,f=32,s=1,v=1,t=t", "/tmp/taken");
    burst.extend_from_slice(&file("a=T,f=32,s=1,v=1,t=t", "/tmp/unreadable"));
    burst.extend_from_slice(&file("a=T,f=32,t=t", "/tmp/refused"));
    let fetched = Burst::begin_with(
        &burst,
        ALL,
        String::new,
        |_, name| (name != "/tmp/unreadable").then(|| vec![1]),
        record,
    );
    let consumed = || CONSUMED.with(|consumed| consumed.borrow().clone());
    assert_eq!(consumed(), [], "nothing is consumed at the read");
    assert_eq!(
        take_kitty_medium(KittyMedium::TempFile, "/tmp/taken"),
        Some(vec![1])
    );
    assert_eq!(
        take_kitty_medium(KittyMedium::TempFile, "/tmp/unreadable"),
        None
    );
    assert_eq!(consumed(), [], "nor where the engine takes it");
    drop(fetched);
    assert_eq!(
        CONSUMED.take(),
        [
            ("/tmp/taken".to_string(), true),
            ("/tmp/unreadable".to_string(), false)
        ],
        "the refused medium is not consumed"
    );
}

/// The largest medium a read returns.
const LARGEST: usize = crate::spawn::MAX_KITTY_MEDIUM_BYTES as usize;

/// Feed `parts` (the PTY's reads) slice by slice as the reader does, each
/// medium read returning [`LARGEST`] bytes: the names read, in order, and
/// the most one slice's reads held.
fn feed_largest(parts: &[&[u8]]) -> (Vec<String>, usize) {
    let mut names = Vec::new();
    let mut most = 0;
    for part in parts {
        let mut off = 0;
        while off < part.len() {
            let mut held = 0;
            let fetched = Burst::begin_with(
                &part[off..],
                ALL,
                String::new,
                |_, name| {
                    names.push(name.to_string());
                    held += LARGEST;
                    Some(vec![0; LARGEST])
                },
                |_, _, _| {},
            );
            most = most.max(held);
            assert!(fetched.covered() > 0, "a slice that feeds nothing");
            off += fetched.covered();
        }
    }
    (names, most)
}

/// A burst of short `t=f` commands, each naming a largest-size file, is read slice by slice:
/// no slice holds more than two largest media, and each command is read once, in order,
/// however the PTY's reads cut the burst, including right before an ESC that ends one
/// command and begins the next.
#[test]
fn a_slice_holds_at_most_two_largest_media_and_reads_each_once() {
    let names: Vec<String> = (0..48).map(|n| format!("/m/{n}")).collect();
    let burst: Vec<u8> = names
        .iter()
        .flat_map(|name| file("a=t,f=32,s=10,v=20,t=f", name))
        .collect();
    let (read, most) = feed_largest(&[&burst]);
    assert_eq!(read, names);
    assert!(most <= 2 * LARGEST, "one slice held {most} bytes of media");

    let short = &names[..4];
    let mut adjacent: Vec<u8> = short
        .iter()
        .flat_map(|name| {
            let mut apc = file("a=t,f=32,s=10,v=20,t=f", name);
            apc.truncate(apc.len() - 2);
            apc
        })
        .collect();
    adjacent.extend_from_slice(b"\x1b\\");
    for cut in 1..adjacent.len() {
        let (head, tail) = adjacent.split_at(cut);
        let (read, most) = feed_largest(&[head, tail]);
        assert_eq!(read, short, "cut at {cut}");
        assert!(most <= 2 * LARGEST, "cut at {cut}: {most}");
    }
}
