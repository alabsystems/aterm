// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reset_fetched() {
    FETCHED.with(|slot| {
        if let Some(fetched) = slot.get() {
            *fetched.borrow_mut() = Fetched::default();
        }
    });
}

#[test]
fn default_reads_and_empty_callbacks_leave_optional_state_unallocated() {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let terminal = std::sync::Mutex::new(Terminal::new(4, 8));
        let mut prefetch = Prefetch::default();
        assert!(!Flags::of(&crate::term_lock(&terminal)).any());
        let ((), callbacks) = aterm_heap_count::allocations_in(|| {
            assert!(take_clipboard_query().is_none());
            assert!(take_kitty_medium(KittyMedium::TempFile, "/not-requested").is_none());
        });
        let slices: &[&[u8]] = &[
            b"agent output",
            b"\x1b]52;c;?\x07",
            b"\x1b_Ga=T,t=f;L3RtcC9ub3Q=\x1b\\",
        ];
        let ((), reads) = aterm_heap_count::allocations_in(|| {
            for _ in 0..32 {
                for slice in slices {
                    let fetched = prefetch.reads(&terminal, slice);
                    assert_eq!(fetched.covered(), slice.len());
                    drop(fetched);
                    prefetch.held(&crate::term_lock(&terminal));
                }
            }
        });
        let absent = FETCHED.with(|slot| slot.get().is_none());
        send.send((callbacks, reads, absent)).unwrap();
    });
    let (callbacks, reads, absent) = receive
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("default-reader thread hang detector");
    worker.join().unwrap();
    assert_eq!(callbacks, 0, "empty callbacks need no prefetch state");
    assert_eq!(
        reads, 0,
        "neither first nor repeated disabled reads allocate"
    );
    assert!(absent);
    assert_eq!(
        size_of::<OnceCell<Box<RefCell<Fetched>>>>(),
        size_of::<usize>(),
        "optional state must not grow the per-thread template"
    );
}

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

#[test]
fn disabling_prefetch_discards_the_carried_request_before_reenable() {
    for (whole, expected) in [
        (
            file("a=T,f=100,t=f", "/tmp/admitted.png"),
            (0, 0, vec!["/tmp/admitted.png".to_string()]),
        ),
        (b"\x1b]52;c;?\x07".to_vec(), (1, 1, Vec::new())),
    ] {
        reset_fetched();
        let (head, tail) = whole.split_at(whole.len() / 2);
        assert_eq!(reads(head, ALL), (0, 0, Vec::new()));
        assert_eq!(reads(tail, ALL), expected, "an admitted split completes");

        assert_eq!(reads(head, ALL), (0, 0, Vec::new()));
        assert_eq!(reads(b"gap", Flags::default()), (0, 0, Vec::new()));
        assert_eq!(
            reads(tail, ALL),
            (0, 0, Vec::new()),
            "a disabled slice revokes the earlier carried request"
        );
        assert_eq!(
            reads(&whole, ALL),
            expected,
            "new admitted requests still work"
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

struct Ingested {
    decodes: (usize, usize),
    media: Vec<(KittyMedium, String)>,
    response: String,
    commands: u64,
    host_reads: u64,
    displayed: bool,
}

/// Use the real prefetch scanner and terminal ingestion, substituting only host I/O.
fn ingest(parts: &[&[u8]], flags: Flags) -> Ingested {
    use aterm_core::terminal::kitty_graphics::take_kitty_payload_decodes_for_test;

    reset_fetched();
    let mut terminal = Terminal::new(4, 8);
    terminal.set_cell_pixel_size(10, 20);
    terminal.set_kitty_file_resolver(take_kitty_medium);
    let mut media = Vec::new();
    take_kitty_payload_decodes_for_test();
    for part in parts {
        let mut at = 0;
        while at < part.len() {
            let fetched = Burst::begin_with(
                &part[at..],
                flags,
                String::new,
                |kind, name| {
                    media.push((kind, name.to_string()));
                    Some(vec![17, 34, 51, 255])
                },
                |_, _, _| {},
            );
            let end = at + fetched.covered();
            assert!(end > at, "ingestion must advance");
            terminal.process(&part[at..end]);
            drop(fetched);
            at = end;
        }
    }
    Ingested {
        decodes: take_kitty_payload_decodes_for_test(),
        media,
        response: String::from_utf8(terminal.take_response().unwrap_or_default()).unwrap(),
        commands: terminal.kitty_graphics_commands(),
        host_reads: terminal.host_io_ordinal(),
        displayed: !terminal.cell_frame(4, 8).images[0].is_empty(),
    }
}

/// Exercise the production flags and `reads`/`held` path for inline graphics, which requires
/// no host file access even when the real medium resolver is installed.
fn ingest_inline_as_reader(parts: &[&[u8]], enabled: bool) -> Ingested {
    use aterm_core::terminal::kitty_graphics::take_kitty_payload_decodes_for_test;

    reset_fetched();
    let term = std::sync::Mutex::new(Terminal::new(4, 8));
    {
        let mut terminal = crate::term_lock(&term);
        terminal.set_cell_pixel_size(10, 20);
        if enabled {
            terminal.set_kitty_file_resolver(take_kitty_medium);
        }
        assert_eq!(terminal.kitty_file_resolver_installed(), enabled);
    }
    take_kitty_payload_decodes_for_test();
    for part in parts {
        feed_as_reader(&term, part);
    }
    let mut terminal = crate::term_lock(&term);
    Ingested {
        decodes: take_kitty_payload_decodes_for_test(),
        media: Vec::new(),
        response: String::from_utf8(terminal.take_response().unwrap_or_default()).unwrap(),
        commands: terminal.kitty_graphics_commands(),
        host_reads: terminal.host_io_ordinal(),
        displayed: !terminal.cell_frame(4, 8).images[0].is_empty(),
    }
}

#[test]
fn inline_images_decode_once_through_enabled_prefetch_and_terminal_ingestion() {
    // The small APC fits the prefetch carry; its old eager parse decoded the same payload twice.
    let pixels = [17, 34, 51, 255].repeat(32 * 16);
    let encoded = aterm_codec::base64::encode(&pixels).unwrap();
    let apc = [
        b"\x1b_Ga=T,f=32,s=32,v=16,i=7;".as_slice(),
        encoded.as_bytes(),
        b"\x1b\\",
    ]
    .concat();
    assert!(apc.len() <= 4096);
    for enabled in [true, false] {
        for cut in [0, 2, 41, apc.len() - 1] {
            let result = ingest_inline_as_reader(&[&apc[..cut], &apc[cut..]], enabled);
            assert_eq!(
                result.decodes,
                (1, encoded.len()),
                "enabled={enabled}, cut at {cut}"
            );
            assert_eq!(result.commands, 1);
            assert_eq!(result.host_reads, 0);
            assert!(result.displayed && result.response.contains(";OK\x1b\\"));
        }
    }

    let mut st8 = apc[..apc.len() - 2].to_vec();
    st8.push(ST8);
    let ended = ingest_inline_as_reader(&[&st8], true);
    assert_eq!(ended.decodes, (1, encoded.len()));
    assert!(ended.displayed);
    for cancel in [CAN, SUB] {
        let mut canceled = apc[..apc.len() - 2].to_vec();
        canceled.push(cancel);
        let ended = ingest_inline_as_reader(&[&canceled], true);
        assert_eq!(ended.decodes, (0, 0));
        assert!(!ended.displayed && ended.media.is_empty());
    }
}

#[test]
fn large_inline_ingestion_preserves_the_dropped_carry_control() {
    let pixels = [17, 34, 51, 255].repeat(64 * 64);
    let encoded = aterm_codec::base64::encode(&pixels).unwrap();
    let apc = [
        b"\x1b_Ga=T,f=32,s=64,v=64,i=8;".as_slice(),
        encoded.as_bytes(),
        b"\x1b\\",
    ]
    .concat();
    assert!(apc.len() > CARRY_MAX);
    // The whole sequence exercises deferred decoding. The split is a control: prefetch already
    // discarded an unfinished APC this large, while the terminal retained and completed it.
    for cut in [0, CARRY_MAX + 1] {
        let result = ingest_inline_as_reader(&[&apc[..cut], &apc[cut..]], true);
        assert_eq!(result.decodes, (1, encoded.len()), "cut at {cut}");
        assert_eq!(result.commands, 1);
        assert!(result.media.is_empty());
        assert_eq!(result.host_reads, 0);
        assert!(result.displayed && result.response.contains(";OK\x1b\\"));
    }
}

#[test]
fn external_media_keep_their_reads_after_shared_control_parsing() {
    use aterm_core::terminal::kitty_graphics::KittyMedium::{File, SharedMemory, TempFile};
    for (control, kind) in [
        ("a=T,t=f", File),
        ("a=T,t=t", TempFile),
        ("a=T,t=s", SharedMemory),
        ("t=f", File),
        ("a=T,a=bad,t=f", File),
        ("a=p,a=T,t=f", File),
        ("a=T,t=d,t=f", File),
        ("a=T,t=f,m=1,m=bad", File),
    ] {
        let apc = file(&format!("{control},f=32,s=1,v=1,i=9"), "/fixture");
        for cut in [0, 5, apc.len() - 1] {
            let result = ingest(&[&apc[..cut], &apc[cut..]], ALL);
            assert_eq!(result.decodes, (2, 2 * 12), "{control}, cut {cut}");
            assert_eq!(result.media, [(kind, "/fixture".to_string())], "{control}");
            assert_eq!(result.host_reads, 1, "{control}");
            assert!(result.response.contains(";OK\x1b\\"), "{}", result.response);
        }
    }
}

#[test]
fn ignored_and_invalid_graphics_preserve_engine_results_without_host_reads() {
    let pixels = [17, 34, 51, 255];
    let encoded = aterm_codec::base64::encode(&pixels).unwrap();
    for control in [
        "a=T,t=f,t=d",
        "a=T,t=s,t=bad",
        "a=T,t=f,t=",
        "a=q,a=bad,t=f",
        "a=p,t=f",
        "a=d,t=f",
        "a=T,t=f,m=0,m=1",
        "a=T,s=1,s=bad",
    ] {
        let apc = [
            format!("\x1b_Gf=32,s=1,v=1,i=10,{control};").as_bytes(),
            encoded.as_bytes(),
            b"\x1b\\",
        ]
        .concat();
        let result = ingest(&[&apc], ALL);
        assert_eq!(result.decodes, (1, encoded.len()), "{control}");
        assert!(result.media.is_empty(), "{control}");
        assert_eq!(result.commands, 1);
        assert_eq!(result.host_reads, 0);
        if control.ends_with("s=bad") {
            assert!(result.response.contains("EINVAL"), "{}", result.response);
        }
    }
    for (apc, attempts) in [
        (b"\x1b_Ga=\xff,t=f,i=11;AAAA\x1b\\".as_slice(), 0),
        (b"\x1b_Ga=T,f=32,s=1,v=1,i=11;\xff\x1b\\".as_slice(), 0),
        (b"\x1b_Ga=T,f=32,s=1,v=1,i=11;!!!\x1b\\".as_slice(), 1),
        (b"\x1b_Ga=T,t=f,f=32,s=1,v=1,i=11;!!!\x1b\\".as_slice(), 2),
        (b"\x1b_Ga=T,t=f,f=32,s=1,v=1,i=11;/w==\x1b\\".as_slice(), 2),
    ] {
        let result = ingest(&[apc], ALL);
        assert_eq!(result.decodes.0, attempts, "{apc:?}");
        assert!(result.media.is_empty());
        assert_eq!(result.commands, 1);
        assert_eq!(result.host_reads, 0);
        if attempts > 0 {
            assert!(result.response.contains("EINVAL"), "{}", result.response);
        }
    }
}
