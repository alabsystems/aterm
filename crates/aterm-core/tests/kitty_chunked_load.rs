// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty chunked load (`m=1`) boundary and termination tests.

use aterm_core::terminal::Terminal;

fn pixels() -> String {
    aterm_codec::base64::encode(&[255u8, 0, 0, 255].repeat(4)).expect("encode")
}

fn apc(control: &str, payload: &str) -> Vec<u8> {
    format!("\x1b_G{control};{payload}\x1b\\").into_bytes()
}

fn row(n: u16) -> Vec<u8> {
    format!("\x1b[{};1H", n + 1).into_bytes()
}

const SHOW: &str = "a=T,f=32,s=2,v=2,c=2,r=1,q=2";

fn split_load() -> (Vec<u8>, String) {
    let encoded = pixels();
    let (head, rest) = encoded.split_at(8);
    (apc(&format!("{SHOW},m=1"), head), rest.to_owned())
}

fn complete() -> Vec<u8> {
    apc(SHOW, &pixels())
}

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

fn refusal(reply: Option<Vec<u8>>) -> Option<String> {
    let reply = String::from_utf8(reply?).ok()?;
    let (_, verdict) = reply.split_once(';')?;
    let code: String = verdict
        .chars()
        .take_while(char::is_ascii_uppercase)
        .collect();
    code.starts_with('E').then_some(code)
}

#[test]
fn only_a_first_chunk_kitty_accepts_holds_a_load() {
    let (first, _) = split_load();
    let image_42 = apc("a=t,f=32,s=2,v=2,i=42,q=2", &pixels());
    let head = &pixels()[..8];
    let cases = [
        ("", "a=p,i=1", false, Some("ENOENT")),
        ("", "a=d,d=A", false, None),
        ("", "a=q,i=5,t=f", false, Some("ENOTSUPPORTED")),
        ("a load", "a=d,d=A", false, None),
        ("a load", "a=q,i=5,t=f", false, Some("ENOTSUPPORTED")),
        ("", "a=f,i=42,s=2,v=2", false, Some("ENOENT")),
        ("image 42", "a=f,i=42,s=2,v=2", true, None),
        ("", "a=f,s=2,v=2", false, None),
        ("", "a=q,f=100", false, None),
        ("", "a=q,i=5,f=100", true, None),
        ("", "a=t,i=1,I=2,f=100", false, Some("EINVAL")),
        ("", "a=t,i=1,f=100", true, None),
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

#[test]
fn a_command_refused_before_routing_leaves_the_load_in_flight() {
    let (first, rest) = split_load();
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
