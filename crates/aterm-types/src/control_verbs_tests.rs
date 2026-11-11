// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn wait_naming_uses_status_framing_except_for_json() {
    use super::{Framing, framing_of};
    for request in ["sys name job=b1", "@s-a sys name task=7"] {
        assert_eq!(framing_of("sys", request), Framing::Status, "{request}");
    }
    for request in [
        "sys name job=b1 --json",
        "@s-a sys name task=7 json",
        "sys jobs",
        "sys why",
    ] {
        assert_eq!(framing_of("sys", request), Framing::Lines, "{request}");
    }
}

/// A home-relative `cwd=` is `~` or `~/<path>` with no `..`; another account's home and a
/// path that may climb out are refused; an absolute or plain path is no home-relative one.
#[test]
fn a_home_cwd_stays_in_the_driven_macs_home() {
    use super::home_cwd;
    for (value, path) in [
        ("~", ""),
        ("~/", ""),
        ("$HOME/trust", "trust"),
        ("$HOME/trust/", "trust"),
        ("~/My Project/src", "My Project/src"),
        ("~/a..b", "a..b"),
        ("~//etc", "etc"),
        ("~///", ""),
    ] {
        assert_eq!(home_cwd(value), Some(Ok(path)), "{value}");
    }
    for refused in ["~bob/trust", "~bob", "~/..", "~/a/../../etc", "~/../x"] {
        assert!(
            matches!(home_cwd(refused), Some(Err(_))),
            "{refused}: {:?}",
            home_cwd(refused)
        );
    }
    for other in ["/Users//a/trust", "trust", "", "./~"] {
        assert_eq!(home_cwd(other), None, "{other}");
    }
}

#[test]
fn the_host_gone_line_round_trips_and_nothing_else_reads_as_it() {
    use super::{host_gone, host_gone_line};
    let sock = "/Users//a b/Library/Application Support/aterm/aterm-4242.sock";
    let line = host_gone_line(4242, sock);
    assert_eq!(line, format!("ERR gone pid=4242 sock={sock}\n"));
    assert_eq!(host_gone(&line), Some((4242, sock)));
    for other in [
        "ERR gone fishing",
        "ERR gone pid= sock=/s",
        "ERR gone pid=+1 sock=/s",
        "ERR gone pid=12 sock=",
        "ERR forward: could not reach the aterm that hosts that session",
        "OK gone pid=1 sock=/s",
    ] {
        assert_eq!(host_gone(other), None, "{other}");
    }
}

#[test]
fn a_rows_fence_round_trips_refuses_bad_shapes_and_hashes_the_rows_joined() {
    use super::RowsFence;
    let f = RowsFence {
        first: 12,
        last: 14,
        hash: 0x00ab_cdef_0123_4567,
    };
    assert_eq!(f.to_string(), "12-14:00abcdef01234567");
    assert_eq!(RowsFence::parse(&f.to_string()), Some(f));
    for bad in [
        "",
        "12-14",
        "14-12:00abcdef01234567",
        "12-14:abc",
        "a-b:00abcdef01234567",
        "12:00abcdef01234567",
        "12-14:00abcdef0123456g",
        "-1-3:00abcdef01234567",
    ] {
        assert_eq!(RowsFence::parse(bad), None, "{bad:?}");
    }
    assert_eq!(
        RowsFence::hash_rows(["ab", "", "❯ c"]),
        aterm_hash::fnv1a_64("ab\n\n❯ c\n".as_bytes())
    );
    assert_ne!(RowsFence::hash_rows(["ab"]), RowsFence::hash_rows(["ab "]));
}

use super::*;

#[test]
fn a_selector_body_names_something_only_when_it_is_a_nonblank_word() {
    for named in [".", "self", "env", "12", "s-0f1e", "*", "s-ab:3"] {
        assert!(selector_body_names_something(named), "{named:?}");
    }
    for nothing in ["", " ", "\t", " s-ab", "s-ab ", "s-\nab", "s-\u{1b}ab"] {
        assert!(!selector_body_names_something(nothing), "{nothing:?}");
    }
    assert!(EMPTY_SELECTOR.starts_with("ERR usage"));
    assert!(!EMPTY_SELECTOR.contains('\n'));
}

#[test]
fn screen_gen_parses_exactly_the_wire_spelling() {
    let g = ScreenGen::parse("3.15").expect("canonical spelling");
    assert_eq!(g, ScreenGen { epoch: 3, seq: 15 });
    assert_eq!(g.to_string(), "3.15");
    let max = ScreenGen {
        epoch: u64::MAX,
        seq: 0,
    };
    assert_eq!(ScreenGen::parse(&max.to_string()), Some(max));
    for bad in [
        "15",
        "3.",
        ".15",
        "3.15.1",
        "-3.15",
        "3.+15",
        "+3.15",
        "x.1",
        " 3.15",
        "",
        "18446744073709551616.1",
        "1.18446744073709551616",
    ] {
        assert_eq!(ScreenGen::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn json_flag_switches_every_json_capable_verb_to_lines() {
    assert!(json_verbs().count() >= 10, "non-vacuity");
    for v in json_verbs() {
        for flag in ["--json", "json"] {
            assert_eq!(
                framing_of(v, &format!("{v} {flag}")),
                Lines,
                "{v} {flag} must frame as Lines (the json_ok `OK 1\\n<body>` shape)"
            );
        }
    }
    assert_eq!(framing_of("metrics", "metrics"), Status);
    assert_eq!(framing_of("cursor", "cursor"), Status);
}

#[test]
fn framing_base_and_sub_forms() {
    assert_eq!(framing_of("text", "text"), Lines);
    assert_eq!(framing_of("who", "who"), Lines);
    assert_eq!(framing_of("cast", "cast"), Bytes);
    assert_eq!(framing_of("subscribe", "subscribe @. screen"), Push);
    assert_eq!(framing_of("cursor", "cursor"), Status);
    assert_eq!(framing_of("bogus", "bogus"), Status);
    assert_eq!(framing_of("image", "image shot.png"), Status);
    assert_eq!(framing_of("image", "image --meta shot.png"), Status);
    assert_eq!(framing_of("image", "image --meta --bytes"), Lines);
    assert_eq!(framing_of("image", "@s-a image bytes"), Lines);
    assert_eq!(framing_of("image", "@child image read"), Lines);
    assert_eq!(framing_of("cast", "@s-a cast frames"), Lines);
    assert_eq!(framing_of("cast", "@s-a cast drift"), Lines);
    assert_eq!(framing_of("cast", "cast drift max_runs=4 rows=10"), Lines);
    assert_eq!(framing_of("video", "video frames"), Lines);
    assert_eq!(framing_of("video", "@s-a video frames count=5"), Lines);
    assert_eq!(framing_of("video", "video 3 full"), Status);
    assert_eq!(framing_of("video", "video status"), Status);
    assert_eq!(framing_of("temporal", "temporal status"), Status);
    assert_eq!(framing_of("temporal", "temporal"), Bytes);
    assert_eq!(framing_of("temporal", "temporal 1200"), Bytes);
    assert_eq!(framing_of("temporal", "@s-a temporal status"), Status);
    assert_eq!(framing_of("temporal", "temporal judge"), Status);
    assert_eq!(framing_of("rewind", "rewind"), Status);
    assert_eq!(framing_of("rewind", "rewind open ago=30"), Status);
    assert_eq!(framing_of("rewind", "rewind step -1"), Status);
    assert_eq!(framing_of("rewind", "rewind text"), Bytes);
    assert_eq!(framing_of("rewind", "@s-a rewind text trim"), Bytes);
    assert_eq!(framing_of("rewind", "rewind close"), Status);
    let forget = spec_for("rewind", "forget").expect("rewind resolves");
    assert_eq!((forget.op, forget.owner_only), (Write, false));
    let all = spec_for("rewind", "forget all").expect("rewind resolves");
    assert_eq!((all.op, all.owner_only), (Write, true));
    let open = spec_for("rewind", "open").expect("rewind resolves");
    assert_eq!((open.op, open.owner_only), (Write, false));
    assert_eq!(framing_of("rewind", "rewind step -1 by=cmd"), Status);
    assert_eq!(framing_of("rewind", "rewind changed 17,41"), Status);
    assert_eq!(framing_of("rewind", "@s-a rewind export"), Status);
    assert_eq!(framing_of("rewind", "rewind check"), Status);
    assert_eq!(framing_of("rewind", "@s-a rewind check export"), Status);
    assert_eq!(framing_of("aos", "aos ls"), Lines);
    assert_eq!(framing_of("aos", "@s-a aos ls"), Lines);
    assert_eq!(framing_of("aos", "aos status"), Status);
    assert_eq!(framing_of("aos", "aos trace hex"), Status);
    assert_eq!(framing_of("aos", "aos attach code=shipped"), Status);
    assert_eq!(framing_of("topic", "topic"), Lines);
    assert_eq!(framing_of("topic", "topic ls"), Lines);
    assert_eq!(framing_of("topic", "topic add build ttl=60"), Status);
    assert_eq!(framing_of("topic", "@s-a topic drop build"), Status);
    assert_eq!(framing_of("await", "await task @s-a,@s-b all"), Lines);
    assert_eq!(framing_of("await", "@s-a await task since=3"), Lines);
    assert_eq!(
        framing_of("await", "await task @s-a:3:1,@s-b any facts=1 fact_since=2"),
        Lines
    );
    assert_eq!(framing_of("await", "@s-a await task facts=1"), Lines);
    assert_eq!(framing_of("await", "await agent idle"), Status);
    assert_eq!(framing_of("await", "await inbox since=1"), Status);
    assert_eq!(
        framing_of("edit", "@s-a edit file:///tmp/x.txt line=3"),
        Status
    );
    assert_eq!(framing_of("await", "await edit ticket=1"), Status);
    assert_eq!(
        framing_of(
            "await",
            "@s-a await edit uri=file:///tmp/x.txt timeout=5000"
        ),
        Status
    );
    assert_eq!(framing_of("task", "@s-a task mark done"), Status);
    assert_eq!(framing_of("task", "@s-a task"), Status);
    for (request, framing) in [
        ("doc", Lines),
        ("doc ls", Lines),
        ("doc ls uri=file:///tmp/x.txt", Lines),
        ("@s-a doc ls", Lines),
        ("doc read 3", Bytes),
        ("doc read file:///tmp/x.txt lines=1-20", Bytes),
        ("@s-a doc read doc=3 lines=40-", Bytes),
        ("doc apply 3 base=7 len=12", Lines),
        ("doc log 3 since=2", Lines),
        ("doc save 3", Status),
        ("@s-a doc save file:///tmp/x.txt", Status),
    ] {
        assert_eq!(framing_of("doc", request), framing, "{request}");
    }
    assert_eq!(framing_of("result", "@s-a result"), Lines);
    assert_eq!(framing_of("result", "result full"), Lines);
    assert_eq!(framing_of("cursor", "cursor --json"), Lines);
    assert_eq!(framing_of("dims", "dims json"), Lines);
    assert_eq!(framing_of("cursor", "@s-a cursor --json"), Lines);
    assert_eq!(framing_of("edges", "edges --json"), Lines);
    assert_eq!(framing_of("dims", "dims"), Status);
    assert_eq!(framing_of("meta", "meta"), Status);
    assert_eq!(framing_of("meta", "meta set title build agent"), Status);
    assert_eq!(framing_of("meta", "@s-a meta unset icon"), Status);
    assert_eq!(framing_of("timeline", "timeline"), Lines);
    assert_eq!(framing_of("occupancy", "occupancy"), Lines);
    assert_eq!(framing_of("occupancy", "occupancy why"), Lines);
    assert_eq!(
        framing_of("occupancy", "@s-a occupancy check --replay 24h"),
        Lines
    );
    assert_eq!(framing_of("timeline", "@s-a timeline 10 since=3"), Lines);
    assert_eq!(framing_of("resizes", "resizes"), Lines);
    assert_eq!(framing_of("resizes", "@s-a resizes 20 since=4"), Lines);
    assert_eq!(framing_of("trail", "trail"), Lines);
    assert_eq!(framing_of("trail", "@s-a trail 5"), Lines);
    assert_eq!(framing_of("status", "status"), Status);
    assert_eq!(framing_of("status", "@s-a status"), Status);
    assert_eq!(framing_of("wake", "wake label=built when=in:5m"), Status);
    assert_eq!(
        framing_of(
            "wake",
            "@self wake label=b when=pid:42 exited need=slots>=1"
        ),
        Status
    );
    assert_eq!(framing_of("wake", "@s-a wake rm #1"), Status);
    assert_eq!(framing_of("wake", "wake taken #2"), Status);
    assert_eq!(framing_of("waits", "waits"), Lines);
    assert_eq!(framing_of("waits", "waits why"), Lines);
    assert_eq!(framing_of("waits", "@s-a waits why #2"), Lines);
    assert_eq!(framing_of("await", "await wake"), Status);
    assert_eq!(framing_of("await", "@self await wake timeout=5000"), Status);
}

/// A view freezes the person's pane, so only a write grant opens, steps, searches or closes one; reading it stays a read.
#[test]
fn rewind_needs_write_to_move_a_view_and_read_to_read_one() {
    let op = |rest: &str| spec_for("rewind", rest).expect("a table verb").op;
    for rest in [
        "open",
        "open ago=30",
        "step -1",
        "step -1 by=cmd",
        "changed 3,4",
        "close",
    ] {
        assert_eq!(op(rest), Write, "{rest}");
    }
    for rest in ["", "status", "text", "text trim", "export"] {
        assert_eq!(op(rest), Read, "{rest}");
    }
}

#[test]
fn artifact_ack_classification_covers_every_guarded_subform() {
    for (verb, request) in [
        ("image", "image"),
        ("image", "image shot.png"),
        ("window", "window prefs shot.png"),
        ("video", "video 3 full"),
        ("video", "@s-a video frames count=4"),
    ] {
        assert!(
            artifact_reply_requires_ack(verb, request),
            "{request} must acknowledge exact artifact retention"
        );
    }
    assert!(!artifact_reply_requires_ack("video", "video status"));
    assert!(!artifact_reply_requires_ack("video", "video stop"));
    assert!(!artifact_reply_requires_ack("image", "@s-a image read"));
    assert!(!artifact_reply_requires_ack("image", "image --bytes"));
    assert!(!artifact_reply_requires_ack(
        "image",
        "@s-a image --meta --bytes"
    ));
    assert!(!artifact_reply_requires_ack("image", "@s-a image bytes"));
    assert!(!artifact_reply_requires_ack("text", "text"));
    assert!(valid_artifact_ack_nonce("00112233445566778899aabbccddeeff"));
    assert!(!valid_artifact_ack_nonce("artifact"));
}

#[test]
fn entry_lines_wrap_every_verb_deterministically_and_losslessly() {
    let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
    let text_width = ENTRY_WRAP_COLUMNS - CATALOG_TEXT_COLUMN;
    // A verb whose entry is a page lays it out in blocks (`verb_page`'s tests hold those).
    for s in VERBS.iter().filter(|s| s.page.is_none()) {
        let lines = s.entry_lines();
        assert!(!lines.is_empty());
        let head = format!("{:<28} ", s.name);
        assert!(
            lines[0].starts_with(&head),
            "{}: first row leads with the name",
            s.name
        );
        let mut texts = vec![lines[0][head.len()..].to_string()];
        for l in &lines[1..] {
            assert!(
                l.starts_with(&gutter),
                "{}: continuation row not indented",
                s.name
            );
            assert!(
                !l[gutter.len()..].starts_with(' '),
                "{}: doubled indent",
                s.name
            );
            texts.push(l[gutter.len()..].to_string());
        }
        for l in &lines {
            let chars = l.chars().count();
            let one_word = !l.trim_start().contains(' ');
            assert!(
                chars <= ENTRY_WRAP_COLUMNS || one_word,
                "{}: row of {chars} chars breaks the {ENTRY_WRAP_COLUMNS}-column wrap: {l:?}",
                s.name
            );
        }
        for t in &texts {
            assert!(
                !t.is_empty() && t.trim() == t,
                "{}: ragged wrap chunk {t:?}",
                s.name
            );
            assert!(t.chars().count() <= text_width || !t.contains(' '));
        }
        assert_eq!(texts.join(" "), s.help_line(), "{}: wrap lost text", s.name);
        assert_eq!(
            s.entry_lines(),
            lines,
            "{}: wrap is not deterministic",
            s.name
        );
    }
    let one = spec("lines").unwrap();
    assert_eq!(
        one.entry_lines(),
        vec![catalog_row("lines", &one.help_line())]
    );
    assert!(spec("image").unwrap().entry_lines().len() > 5);
}

#[test]
fn fabric_verbs_carry_their_designed_classes() {
    for (name, op, framing, target, access) in [
        ("inbox", Read, Lines, Session, Scoped),
        ("inbox get", Read, Bytes, Session, Scoped),
        ("inbox seen", Write, Status, Session, Scoped),
        ("topic", Owner, Lines, Session, OwnerOnly),
        ("post", Write, Status, Session, Scoped),
        ("deliver", Write, Status, Meta, BridgeOnly),
        ("link", Write, Status, Meta, BridgeOnly),
        ("hello", Write, Status, Meta, BridgeOnly),
        ("hold", Write, Status, Meta, OwnerOnly),
        ("outbox", Read, Bytes, Meta, BridgeOnly),
        ("outbox sent", Write, Status, Meta, BridgeOnly),
        ("log-take", Read, Bytes, Meta, BridgeOnly),
        ("log-landed", Write, Status, Meta, BridgeOnly),
    ] {
        let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
        assert_eq!(s.op, op, "{name} op-class");
        assert_eq!(s.framing, framing, "{name} framing");
        assert_eq!(s.target, target, "{name} target");
        assert_eq!(s.access, access, "{name} access");
    }
    assert_eq!(framing_of("inbox", "inbox"), Lines);
    assert_eq!(framing_of("inbox", "inbox 20 since=4 --meta"), Lines);
    assert_eq!(framing_of("inbox", "inbox get 7"), Bytes);
    assert_eq!(framing_of("inbox", "@s-a inbox get 7"), Bytes);
    assert_eq!(framing_of("inbox", "inbox seen 7 handled"), Status);
    assert_eq!(framing_of("inbox", "@s-a inbox seen 7"), Status);
    assert_eq!(
        framing_of("deliver", "deliver s-a off=9 from=h-x kind=task"),
        Status
    );
    assert_eq!(framing_of("hold", "hold s-a on reason=x"), Status);
    assert_eq!(framing_of("post", "post to=@s-b kind=ask hello"), Status);
    assert_eq!(framing_of("log-take", "log-take n=32 tier=wire"), Bytes);
    assert_eq!(framing_of("log-landed", "log-landed s-a 1:4:2"), Status);
    assert_eq!(framing_of("outbox", "outbox"), Bytes);
    assert_eq!(framing_of("outbox", "outbox 8"), Bytes);
    assert_eq!(framing_of("outbox", "outbox sent s-a 3 off=91"), Status);
}

#[test]
fn copy_and_settings_have_their_own_op_class() {
    assert_eq!(spec("copy").unwrap().op, OpClass::ClipboardWrite);
    assert_eq!(spec("settings").unwrap().op, OpClass::ConfigWrite);
    assert_eq!(spec("scroll").unwrap().op, OpClass::Read);
    assert_eq!(spec("select").unwrap().op, OpClass::Read);
}

#[test]
fn privacy_is_a_headless_answerable_meta_read() {
    let s = spec("privacy").expect("privacy is in the table");
    assert_eq!(
        s.op, Read,
        "privacy is a Read verb (json-capable verbs must be)"
    );
    assert_eq!(s.framing, Lines, "privacy replies `OK <n>` + n lines");
    assert_eq!(
        s.target, Meta,
        "privacy is self-scoped; a selector is rejected"
    );
    assert_eq!(
        s.access, AnyScopeMeta,
        "privacy must be AnyScopeMeta or a headless instance cannot answer it"
    );
    assert_eq!(
        s.json,
        Json::Emits,
        "privacy --json is part of the contract"
    );
    assert_eq!(framing_of("privacy", "privacy --json"), Lines);
    assert_eq!(framing_of("privacy", "privacy"), Lines);
}

#[test]
fn theme_rows_carry_their_classes_and_only_list_is_lines() {
    for (name, op) in [
        ("theme", Read),
        ("theme preview", Write),
        ("theme set", ConfigWrite),
    ] {
        let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
        assert_eq!(s.op, op, "{name} op-class");
        assert_eq!(s.framing, Status, "{name} row framing");
        assert_eq!(s.target, App, "{name} target");
    }
    let session = spec("theme session").expect("theme session is in the table");
    assert_eq!((session.op, session.target), (Read, App));
    let op = |rest: &str| spec_for("theme", rest).map(|r| r.op);
    assert_eq!(op("session"), Some(Read));
    assert_eq!(op("session status"), Some(Read));
    assert_eq!(op("session set Hearth"), Some(Write));
    assert_eq!(op("session clear"), Some(Write));
    assert_eq!(op("palette session"), Some(Read));
    assert_eq!(framing_of("theme", "theme list"), Lines);
    assert_eq!(framing_of("theme", "@s-1 theme list"), Lines);
    assert_eq!(framing_of("theme", "theme palette"), Lines);
    assert_eq!(framing_of("theme", "@3 theme palette session"), Lines);
    for request in [
        "theme",
        "theme status",
        "theme session",
        "theme session set Hearth",
        "theme session clear",
        "theme preview Nord ttl=5",
        "theme revert",
        "theme commit",
        "theme set Nord",
    ] {
        assert_eq!(framing_of("theme", request), Status, "{request}");
    }
}

#[test]
fn spec_for_prefers_the_listed_sub_form_row() {
    let r = |verb: &str, rest: &str| spec_for(verb, rest).expect("a table verb");
    let seen = r("inbox", "seen 7");
    assert_eq!((seen.row.name, seen.op), ("inbox seen", Write));
    assert_eq!(r("inbox", "").row.name, "inbox");
    assert_eq!(r("inbox", "20 since=4").op, Read);
    let upper = r("theme", "SET Nord");
    assert_eq!((upper.row.name, upper.op), ("theme", ConfigWrite));
    assert_eq!(r("theme", "set Nord").row.name, "theme set");
    assert_eq!(r("theme", "preview Nord").op, Write);
    assert_eq!(r("theme", "revert").op, Write);
    assert_eq!(r("theme", "status").op, Read);
    assert_eq!(r("theme", "").op, Read);
    assert_eq!(r("inbox", "get @5").lane, Wait);
    assert_eq!(r("inbox", "get 5").lane, Rpc);
    assert_eq!(r("mainscreen", "").lane, Wait);
    assert_eq!(r("await", "task @s-a").framing, Lines);
    assert_eq!(r("cast", "drift rows=4").lane, Wait);
    assert_eq!(r("cast", "frames").lane, Rpc);
    assert_eq!(r("paste", "hi").lane, WaitWhenAimed);
    assert_eq!(r("video", "3 full").lane, Wait);
    assert_eq!(r("video", "status").lane, Rpc);
    assert_eq!(r("text", "").lane, Rpc);
    assert_eq!(r("edit", "file:///tmp/x.txt line=3").lane, Wait);
    assert_eq!(r("await", "edit ticket=1").lane, Wait);
    assert_eq!(r("doc", "ls").op, Read);
    assert_eq!(r("doc", "read 3 lines=1-20").op, Read);
    assert_eq!(r("doc", "log 3").op, Read);
    assert_eq!(r("doc", "apply 3 len=12").op, Write);
    assert_eq!(r("doc", "save 3").op, Write);
    assert_eq!(r("doc", "read 3").lane, Rpc);
    assert!(r("update", "check").owner_only && r("update", "apply").owner_only);
    assert_eq!(r("update", "check").lane, Wait);
    assert!(!r("update", "status").owner_only && !r("update", "").owner_only);
    assert!(r("family", "s-1").owner_only && !r("family", "").owner_only);
    assert!(r("pointer", "click").owner_only && !r("pointer", "move 3 4").owner_only);
    assert!(r("pointer", "mods link").owner_only);
    assert!(r("pointer", "unread=ok click").owner_only);
    assert!(r("pointer", "unread=ok mods link").owner_only);
    assert!(!r("pointer", "unread=ok move 3 4").owner_only);
    assert!(!r("update", "unread=ok check").owner_only);
    assert!(r("window", "overview").owner_only && !r("window", "prefs").owner_only);
    assert!(r("window", "connections").owner_only);
    assert!(r("controls", "overview").owner_only && !r("controls", "").owner_only);
    assert!(r("controls", "connections").owner_only);
    assert!(r("task", "mark done").owner_only && !r("task", "").owner_only);
    let wake = r("wake", "label=b when=in:5m");
    assert_eq!((wake.op, wake.lane, wake.owner_only), (Write, Rpc, false));
    assert_eq!(
        wake.row.input_class(),
        Input::NONE,
        "a wake writes no session input"
    );
    assert_eq!(r("wake", "rm #1").op, Write);
    assert_eq!((r("waits", "why").op, r("waits", "").lane), (Read, Rpc));
    assert_eq!(r("await", "wake").lane, Wait);
    assert_eq!(r("meta", "unset icon").op, Write);
    assert_eq!(r("meta", "").op, Read);
    assert_eq!(r("log", "take").row.name, "log");
    assert!(spec_for("bogus", "").is_none());
}

/// `end` signals processes, so it is a Signal verb a fleet halt refuses, and it lists one
/// line per token on a wait lane (each end can take a grace).
#[test]
fn end_is_a_signal_verb_a_halt_refuses() {
    let end = spec("end").expect("end ships");
    assert_eq!(end.op, Signal);
    assert_eq!(end.target, Session);
    assert_eq!(end.access, Scoped);
    assert_eq!(end.framing, Lines);
    assert_eq!(end.lane, Wait);
    assert!(!end.fleet, "a fleet peer never ends a process here");
    assert!(
        input_of("end").contains(Input::PTY),
        "a halt refuses end: {:?}",
        end.input
    );
    let help = end.help_line();
    for word in [
        "no `all`",
        "no wildcard",
        "no pid without its token",
        "why=<stale",
    ] {
        assert!(help.contains(word), "{word}: {help}");
    }
}

#[test]
fn the_hold_help_enumerates_exactly_the_halt_set() {
    const MARKER: &str = "from ANY scope — `";
    let halts = |verb: &str| input_of(verb).contains(Input::PTY);
    let hold = spec("hold").expect("hold ships").help_line();
    let listed: Vec<&str> = hold
        .split_once(MARKER)
        .expect("the `hold` detail enumerates the halted set after `from ANY scope — `")
        .1
        .split_once('`')
        .expect("the enumeration is one back-ticked run")
        .0
        .split_whitespace()
        .collect();

    for verb in &listed {
        assert!(
            halts(verb),
            "`help hold` names `{verb}` as refused under a halt and the halt \
             does not refuse it"
        );
    }
    for verb in VERBS.iter().map(|s| s.name).filter(|n| halts(n)) {
        assert!(
            listed.contains(&verb),
            "a halt refuses `{verb}` and `help hold` — the only shipped \
             statement of what a halt refuses — does not name it"
        );
    }
    let refused = VERBS.iter().filter(|s| halts(s.name)).count();
    assert_eq!(
        listed.len(),
        refused,
        "the `hold` detail lists {} verbs for a set of {refused}: a repeat \
         is a second literal in the same sentence",
        listed.len(),
    );
}
