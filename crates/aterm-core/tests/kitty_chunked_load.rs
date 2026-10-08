// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a kitty chunked load (`m=1`) ends, against the real engine. A load
//! assembles from its chunks whatever unrelated output falls between them,
//! and a chunk may repeat the first chunk's keys. It ends where the program
//! that began it is over (the shell's prompt or command start), and, as in
//! kitty, at a command that cannot belong to it (a delete, a transmit on
//! another medium). The next program's first command is then read as itself
//! instead of being lost as a chunk. A put arriving mid-load is shown and the
//! load goes on, as kitty does. Only a first chunk kitty holds in flight
//! opens a load: any other ignores its `m=1`. A command kitty refuses before
//! it routes one (naming both `i=` and `I=`, or a query naming no `i=`) is
//! neither a chunk nor ends a load.

use aterm_core::terminal::Terminal;

/// A 2x2 px RGBA image, base64.
fn pixels() -> String {
    aterm_codec::base64::encode(&[255u8, 0, 0, 255].repeat(4)).expect("encode")
}

/// `ESC _ G <control> ; <payload> ESC \`.
fn apc(control: &str, payload: &str) -> Vec<u8> {
    format!("\x1b_G{control};{payload}\x1b\\").into_bytes()
}

/// Move the cursor to the 0-based `row`, column 0.
fn row(n: u16) -> Vec<u8> {
    format!("\x1b[{};1H", n + 1).into_bytes()
}

/// The image shown two cells wide and one row high at the cursor.
const SHOW: &str = "a=T,f=32,s=2,v=2,c=2,r=1,q=2";

/// A load of the image split in two: the first chunk (`m=1`) and the payload
/// left for the final one.
fn split_load() -> (Vec<u8>, String) {
    let encoded = pixels();
    let (head, rest) = encoded.split_at(8);
    (apc(&format!("{SHOW},m=1"), head), rest.to_owned())
}

/// The next program's first command: the whole image at the cursor.
fn complete() -> Vec<u8> {
    apc(SHOW, &pixels())
}

/// A program killed after the first chunk of a load: the shell's next prompt
/// (OSC 133 or 633 `A`), or its next command start (`C`), ends the load, so
/// the next program's complete command draws. Without either, that command
/// is read as the load's final chunk and draws nothing, as in kitty.
#[test]
fn a_prompt_or_command_start_ends_an_abandoned_load() {
    let (first, _) = split_load();
    let boundaries: [(&[u8], &[u8]); 3] = [
        (b"", b"\x1b]133;A\x07"),
        (b"", b"\x1b]633;A\x07"),
        (b"\x1b]133;A\x07\x1b]133;B\x07", b"\x1b]133;C\x07"),
    ];
    for (before, boundary) in boundaries {
        let mut term = Terminal::new(24, 80);
        term.process(before);
        term.process(&first);
        term.process(boundary);
        term.process(&row(2));
        term.process(&complete());
        assert!(
            !term.images_row(2).is_empty(),
            "{:?}: the command after the boundary drew nothing",
            String::from_utf8_lossy(boundary)
        );
    }
    let mut term = Terminal::new(24, 80);
    term.process(&first);
    term.process(&row(2));
    term.process(&complete());
    assert!(
        (0..24).all(|r| term.images_row(r).is_empty()),
        "with no boundary the next command is the load's final chunk"
    );
}

/// A load still assembles across unrelated output (a title) between its
/// chunks, and whether its final chunk carries only `m=` or repeats every
/// key of the first.
#[test]
fn a_load_assembles_across_a_title_and_repeated_keys() {
    let (first, rest) = split_load();
    for between in [&b""[..], b"\x1b]2;title\x07"] {
        for last in ["m=0".to_owned(), format!("{SHOW},m=0")] {
            let mut term = Terminal::new(24, 80);
            term.process(&first);
            term.process(between);
            term.process(&apc(&last, &rest));
            assert!(
                !term.images_row(0).is_empty(),
                "{between:?} then {last:?}: the load did not assemble"
            );
        }
    }
}

/// A delete between chunks runs and aborts the load, as the protocol says
/// it must: the stray final chunk draws nothing, and the next command draws.
#[test]
fn a_delete_between_chunks_runs_and_ends_the_load() {
    let (first, rest) = split_load();
    let mut term = Terminal::new(24, 80);
    term.process(&row(4));
    term.process(&complete());
    term.process(&row(0));
    term.process(&first);
    term.process(&apc("a=d,d=A,q=2", ""));
    assert!(term.images_row(4).is_empty(), "the delete did not run");
    term.process(&apc("m=0", &rest));
    assert!(term.images_row(0).is_empty(), "the aborted load drew");
    term.process(&row(2));
    term.process(&complete());
    assert!(
        !term.images_row(2).is_empty(),
        "the next command drew nothing"
    );
}

/// A query on another medium between chunks is answered as its own command
/// and ends the load, so the next command draws.
#[test]
fn a_command_on_another_medium_ends_the_load() {
    let (first, _) = split_load();
    let mut term = Terminal::new(24, 80);
    term.process(&first);
    term.process(&apc("a=q,i=5,t=f", ""));
    let reply = term.take_response().unwrap_or_default();
    assert!(
        reply.starts_with(b"\x1b_Gi=5;ENOTSUPPORTED"),
        "the query was not answered: {:?}",
        String::from_utf8_lossy(&reply)
    );
    term.process(&row(2));
    term.process(&complete());
    assert!(
        !term.images_row(2).is_empty(),
        "the next command drew nothing"
    );
}

/// A put between chunks shows its image and leaves the load in flight: the
/// final chunk still completes it, at the cursor it arrives at.
#[test]
fn a_put_between_chunks_shows_and_the_load_goes_on() {
    let (first, rest) = split_load();
    let mut term = Terminal::new(24, 80);
    term.process(&apc("a=t,f=32,s=2,v=2,i=1,q=2", &pixels()));
    term.process(&first);
    term.process(&row(4));
    term.process(&apc("a=p,i=1,c=2,r=1,q=2", ""));
    assert!(!term.images_row(4).is_empty(), "the put drew nothing");
    term.process(&row(2));
    term.process(&apc("m=0", &rest));
    assert!(!term.images_row(2).is_empty(), "the load did not complete");
}

/// The error code a reply refuses with (`EINVAL`, `ENOENT`, ...), or `None`
/// for no reply or an `OK`.
fn refusal(reply: Option<Vec<u8>>) -> Option<String> {
    let reply = String::from_utf8(reply?).ok()?;
    let (_, verdict) = reply.split_once(';')?;
    let code: String = verdict
        .chars()
        .take_while(char::is_ascii_uppercase)
        .collect();
    code.starts_with('E').then_some(code)
}

/// A first chunk (`m=1`) holds a load in flight exactly where kitty's does: a
/// transmit, query or frame load on the direct medium that kitty accepts.
/// Any other command ignores its `m=1`, runs at once with its usual answer
/// and leaves no load open, and one that ends a load in flight opens none in
/// its place, so the next program's complete command draws. Each chunk kitty
/// refuses sits beside one it holds that differs only by a key or by what
/// the store holds, where that next command is the load's final chunk and
/// draws nothing.
#[test]
fn only_a_first_chunk_kitty_accepts_holds_a_load() {
    let (first, _) = split_load();
    let image_42 = apc("a=t,f=32,s=2,v=2,i=42,q=2", &pixels());
    let head = &pixels()[..8];
    // (what came before, the chunk marked m=1, whether a load is held, the
    // refusal)
    let cases = [
        // A put, a delete, a query on another medium.
        ("", "a=p,i=1", false, Some("ENOENT")),
        ("", "a=d,d=A", false, None),
        ("", "a=q,i=5,t=f", false, Some("ENOTSUPPORTED")),
        ("a load", "a=d,d=A", false, None),
        ("a load", "a=q,i=5,t=f", false, Some("ENOTSUPPORTED")),
        // A frame for an image the store does not hold, or naming none.
        ("", "a=f,i=42,s=2,v=2", false, Some("ENOENT")),
        ("image 42", "a=f,i=42,s=2,v=2", true, None),
        ("", "a=f,s=2,v=2", false, None),
        // A query naming no i=.
        ("", "a=q,f=100", false, None),
        ("", "a=q,i=5,f=100", true, None),
        // Both i= and I=.
        ("", "a=t,i=1,I=2,f=100", false, Some("EINVAL")),
        ("", "a=t,i=1,f=100", true, None),
        // A size kitty does not load: raw pixels with no area, or a side
        // declared over its limit whatever the format.
        ("", "a=T,f=32", false, None),
        ("", "a=T,f=32,i=7", false, Some("EINVAL")),
        ("", "a=T,f=32,s=10001,v=1", false, None),
        ("", "a=T,f=32,s=2,v=2", true, None),
        ("", "a=T,f=100", true, None),
        ("", "a=T,f=100,s=10001,i=7", false, Some("EINVAL")),
        ("", "a=T,f=100,s=10000,i=7", true, None),
    ];
    for (before, marked, held, refused) in cases {
        let mut term = Terminal::new(24, 80);
        let sent: &[u8] = match before {
            "a load" => &first,
            "image 42" => &image_42,
            _ => &[],
        };
        term.process(sent);
        term.process(&apc(&format!("{marked},m=1"), head));
        assert_eq!(
            refusal(term.take_response()).as_deref(),
            refused,
            "{marked} after {before:?}: the answer"
        );
        term.process(&row(2));
        term.process(&complete());
        assert_eq!(
            term.images_row(2).is_empty(),
            held,
            "{marked} after {before:?}: whether it held the next command"
        );
    }
}

/// A command kitty refuses before it routes one, one naming both `i=` and
/// `I=` or a query naming no `i=`, is answered between a load's chunks and
/// leaves the load in flight: the final chunk still completes it, and a
/// delete refused so deletes nothing. Without the refusing key the same
/// command is a chunk of the load (a query, a transmit) or ends it (a
/// delete), and the load draws nothing.
#[test]
fn a_command_refused_before_routing_leaves_the_load_in_flight() {
    let (first, rest) = split_load();
    // (the command between the chunks, its payload, whether the load draws,
    // whether a delete ran, the refusal)
    let cases: [(&str, &str, bool, bool, Option<&str>); 6] = [
        ("a=q,t=d,f=24,s=1,v=1", "AAAA", true, false, None),
        ("a=q,i=5,t=d,f=24,s=1,v=1", "AAAA", false, false, None),
        ("a=t,i=1,I=2", "", true, false, Some("EINVAL")),
        ("a=t,i=1", "", false, false, None),
        ("a=d,d=A,i=1,I=2", "", true, false, Some("EINVAL")),
        ("a=d,d=A,i=1", "", false, true, None),
    ];
    for (between, payload, draws, deleted, refused) in cases {
        let mut term = Terminal::new(24, 80);
        term.process(&row(4));
        term.process(&complete());
        term.process(&row(0));
        term.process(&first);
        term.process(&apc(between, payload));
        assert_eq!(
            refusal(term.take_response()).as_deref(),
            refused,
            "{between}: the answer"
        );
        term.process(&apc("m=0", &rest));
        assert_eq!(
            !term.images_row(0).is_empty(),
            draws,
            "{between}: whether the load drew"
        );
        assert_eq!(
            term.images_row(4).is_empty(),
            deleted,
            "{between}: whether a delete ran"
        );
    }
}

/// A chunk's `q=` holds for the answer to the whole load, as in kitty.
#[test]
fn a_chunk_can_quiet_the_answer_to_its_load() {
    let encoded = pixels();
    let (head, rest) = encoded.split_at(8);
    for (last, answer) in [("m=0", Some(&b"\x1b_Gi=9;OK\x1b\\"[..])), ("m=0,q=1", None)] {
        let mut term = Terminal::new(24, 80);
        term.process(&apc("a=t,f=32,s=2,v=2,i=9,m=1", head));
        term.process(&apc(last, rest));
        assert_eq!(term.take_response().as_deref(), answer, "{last}");
    }
}
