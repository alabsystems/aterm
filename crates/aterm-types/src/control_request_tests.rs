// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn read(line: &str) -> (Option<Selector<'_>>, &str, &str) {
    let Head {
        selector,
        verb,
        rest,
    } = head(line);
    (selector, verb, rest)
}

/// Of the `s-` words only the exact legacy form (`s-` and 20 lowercase hex digits) is a sid:
/// the loose forms an earlier grammar took (`s-` and any hex) name nobody.
#[test]
fn a_sid_is_an_exact_tab_or_legacy_id() {
    for good in [
        "tqn5r6e0148",
        "s-00000000000000000000",
        "s-0f1e0000000000000000",
        "s-abcdef0123456789abcd",
    ] {
        assert_eq!(Sid::parse(good).map(|s| s.as_str()), Some(good), "{good}");
    }
    for bad in [
        "",
        "s-",
        "s-0",
        "s-0f1e",
        "s-0123456789abcdef012",
        "s-0123456789abcdef01234",
        "s-abcdef0123456789abc",
        "s-abcdef0123456789abcde",
        "S-1",
        "s1",
        "s-g",
        "s-1 ",
        "s-../x",
        "./s-1",
        "s-1/..",
        "s-nope",
        "self",
        "s-ABCDEF0123456789ABCD",
        "s-0F1e",
    ] {
        assert_eq!(Sid::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn a_selector_names_a_tab_by_its_id_in_either_form() {
    for good in ["tqn5r6e0148", "s-0123456789abcdef0123"] {
        assert_eq!(Sid::parse(good).map(|s| s.as_str()), Some(good), "{good}");
        let line = format!("@{good} text");
        assert_eq!(
            read(&line),
            (
                Some(Selector::Sid(Sid::parse(good).expect("a sid"))),
                "text",
                ""
            )
        );
        let rest = format!("@{good} lines");
        assert_eq!(
            subscribe_target(&rest).map(|(sid, rest)| (sid.as_str(), rest)),
            Some((good, " lines"))
        );
    }
    // Negative controls: a malformed tab ID of each form names nobody.
    for bad in [
        "tqn5r6e014",
        "tqn5r6e01488",
        "tqn5r6e014i",
        "Tqn5r6e0148",
        "t-qn5r6e014",
        "s-0123456789abcdeg0123",
        "s-0123456789ABCDEF0123",
    ] {
        assert_eq!(Sid::parse(bad), None, "{bad:?}");
        assert_eq!(Selector::parse(bad), Selector::Invalid(bad), "{bad:?}");
    }
}

#[test]
fn selector_parses_the_wire_forms() {
    assert_eq!(Selector::parse(""), Selector::Invalid(""));
    assert_eq!(Selector::parse("."), Selector::SelfTok);
    assert_eq!(Selector::parse("12"), Selector::Local(12));
    assert_eq!(Selector::parse("007"), Selector::Local(7));
    for good in ["tqn5r6e0148", "t0f1e000000", "s-0f1e0000000000000000"] {
        assert_eq!(
            Selector::parse(good),
            Selector::Sid(Sid::parse(good).expect("a sid"))
        );
    }
    let huge = "99999999999999999999";
    assert_eq!(Selector::parse(huge), Selector::Invalid(huge));
    for bad in [
        "+5",
        "-1",
        "S-1",
        "s-0F1E",
        "s-0f1e",
        "./s-1",
        "../x",
        "self",
        "*",
        "s-a,s-b",
        "t0f1e000000,t0f1e000001",
    ] {
        assert_eq!(Selector::parse(bad), Selector::Invalid(bad), "{bad:?}");
    }
    assert!(Selector::SelfTok.is_self() && !Selector::Local(0).is_self());
}

#[test]
fn send_payload_turns_one_trailing_line_end_into_enter_and_keeps_the_rest() {
    for (typed, sent) in [
        (&b"ls\\n"[..], &b"ls\r"[..]),
        (b"ls\n", b"ls\r"),
        (b"ls\r\n", b"ls\r"),
        (b"ls\\n\n", b"ls\r"),
        (b"ls\\n\r\n", b"ls\r"),
        (b"\\n", b"\r"),
        (b"\n", b"\r"),
        (b"", b""),
        (b"ls", b"ls"),
        (b"a\\nb", b"a\\nb"),
        (b"a\nb", b"a\nb"),
        (b"ls\n\n", b"ls\n\r"),
        (b"ls\\n\\n", b"ls\\n\r"),
        (b"\xff\xfe\\n", b"\xff\xfe\r"),
    ] {
        assert_eq!(send_payload(typed), sent, "{typed:?}");
    }
}

#[test]
fn head_reads_separators_once() {
    let sid = Selector::Sid(Sid::parse("s-1a000000000000000000").expect("a sid"));
    assert_eq!(read("text"), (None, "text", ""));
    assert_eq!(read("text --json"), (None, "text", "--json"));
    assert_eq!(read("send  hello"), (None, "send", " hello"));
    assert_eq!(read("send hello \r"), (None, "send", "hello "));
    assert_eq!(read("  feed-bin 5"), (None, "feed-bin", "5"));
    assert_eq!(read("\tfeed-bin\t5"), (None, "feed-bin", "5"));
    assert_eq!(
        read("@s-1a000000000000000000\tfeed-bin 5"),
        (Some(sid), "feed-bin", "5")
    );
    assert_eq!(
        read("@s-1a000000000000000000 \t screen x"),
        (Some(sid), "screen", "x")
    );
    assert_eq!(read("@. text"), (Some(Selector::SelfTok), "text", ""));
    assert_eq!(read("@ text"), (None, "@", "text"));
    assert_eq!(read("@"), (None, "@", ""));
    assert_eq!(read("@7"), (Some(Selector::Local(7)), "", ""));
    assert_eq!(read(""), (None, "", ""));
    assert_eq!(read("   "), (None, "", ""));
    assert_eq!(read("text\u{a0}x"), (None, "text\u{a0}x", ""));
    assert_eq!(read("feed-bin\x0b4"), (None, "feed-bin\x0b4", ""));
    let line = "  @s-1a000000000000000000\tsend  hi\r";
    let spans = Spans::of(line);
    assert_eq!(spans.head(line), head(line));
    assert_eq!(&line[spans.verb.clone()], "send");
    assert_eq!(&line[spans.rest.clone()], " hi");
    let sid = Selector::Sid(Sid::parse("t1a00000000").expect("a current sid"));
    assert_eq!(
        read("@t1a00000000\tfeed-bin 5"),
        (Some(sid), "feed-bin", "5")
    );
    assert_eq!(read("@t1a00000000 \t screen x"), (Some(sid), "screen", "x"));
    let line = "  @t1a00000000\tsend  hi\r";
    let spans = Spans::of(line);
    assert_eq!(spans.head(line), head(line));
    assert_eq!(&line[spans.verb.clone()], "send");
    assert_eq!(&line[spans.rest.clone()], " hi");
}

#[test]
fn words_split_where_the_head_does() {
    let split = |text| words(text).collect::<Vec<_>>();
    assert_eq!(split("@. screen since=4"), ["@.", "screen", "since=4"]);
    assert_eq!(split(" \t@.\t\tscreen  "), ["@.", "screen"]);
    assert_eq!(split("screen\u{a0}x\x0bcells"), ["screen\u{a0}x\x0bcells"]);
    assert!(split("").is_empty() && split(" \t ").is_empty());
}

#[test]
fn subscribe_names_one_session_only_by_a_sid() {
    let (sid, tail) = subscribe_target("@s-1a000000000000000000 screen trim").expect("one sid");
    assert_eq!(
        (sid.as_str(), tail),
        ("s-1a000000000000000000", " screen trim")
    );
    let (sid, tail) = subscribe_target("\t@s-1a000000000000000000\tcells").expect("one sid");
    assert_eq!((sid.as_str(), tail), ("s-1a000000000000000000", "\tcells"));
    let (sid, tail) = subscribe_target("@tqn5r6e0148 screen trim").expect("one sid");
    assert_eq!((sid.as_str(), tail), ("tqn5r6e0148", " screen trim"));
    let (sid, tail) = subscribe_target("\t@tqn5r6e0148\tcells").expect("one sid");
    assert_eq!((sid.as_str(), tail), ("tqn5r6e0148", "\tcells"));
    let (sid, tail) = subscribe_target("@t0123456789 screen trim").expect("another sid");
    assert_eq!((sid.as_str(), tail), ("t0123456789", " screen trim"));
    let (sid, tail) = subscribe_target("@t1a00000000 screen trim").expect("one sid");
    assert_eq!((sid.as_str(), tail), ("t1a00000000", " screen trim"));
    let (sid, tail) = subscribe_target("\t@t1a00000000\tcells").expect("one sid");
    assert_eq!((sid.as_str(), tail), ("t1a00000000", "\tcells"));
    for local in [
        "@* screen",
        "@s-1a,s-2b screen",
        "@t1a00000000,t2b00000000 screen",
        "@tqn5r6e0148,t0123456789 screen",
        "@s-1a000000000000000000,s-2b000000000000000000 screen",
        "@s-1a screen",
        "@. screen",
        "@ screen",
        "@7 screen",
        "@self screen",
        "@T1A00000000 screen",
        "@t1A00000000 screen",
        "@S-1A screen",
        "@s-1A screen",
        "screen",
        "",
    ] {
        assert_eq!(subscribe_target(local), None, "{local:?}");
    }
}
