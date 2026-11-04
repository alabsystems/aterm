// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::rewrite;

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

fn rewritten(line: &str) -> Option<String> {
    rewrite(&words(line)).map(|parts| parts.join(" "))
}

/// Every read the old pasted list named but `who` (a meta verb: below), a few it did not (the
/// table names them), each with its own words, in both selector shapes.
#[test]
fn a_read_takes_its_session_after_the_verb_with_its_words_kept() {
    for (typed, sent) in [
        ("status @s-ab12", "@s-ab12 status"),
        ("status s-ab12", "@s-ab12 status"),
        ("text @s-ab12 tail=3 trim", "@s-ab12 text tail=3 trim"),
        ("text s-ab12 --json", "@s-ab12 text --json"),
        ("screen @s-ab12 since=00ff", "@s-ab12 screen since=00ff"),
        ("cursor s-ab12", "@s-ab12 cursor"),
        ("modes @s-ab12", "@s-ab12 modes"),
        ("selection s-ab12", "@s-ab12 selection"),
        ("blocks @s-ab12 5", "@s-ab12 blocks 5"),
        ("chrome s-ab12", "@s-ab12 chrome"),
        ("panes @s-ab12", "@s-ab12 panes"),
        (
            "offscreen s-ab12 tail=2 screen=1",
            "@s-ab12 offscreen tail=2 screen=1",
        ),
        ("search @s-ab12 needle case", "@s-ab12 search needle case"),
        ("inbox s-ab12 3 --peek", "@s-ab12 inbox 3 --peek"),
        ("inbox @s-ab12 get 7", "@s-ab12 inbox get 7"),
        (
            "log @s-ab12 class=turn since=4 ep=2",
            "@s-ab12 log class=turn since=4 ep=2",
        ),
        ("result s-ab12 full", "@s-ab12 result full"),
        ("history @s-ab12 5", "@s-ab12 history 5"),
        ("line s-ab12 40", "@s-ab12 line 40"),
        ("doc @s-ab12 read d1", "@s-ab12 doc read d1"),
        ("image s-ab12 read", "@s-ab12 image read"),
        ("text @self", "@self text"),
        ("text @env", "@env text"),
        ("status @.", "@. status"),
        ("status @3", "@3 status"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
}

#[test]
fn read_reads_as_text_on_either_side_of_a_selector() {
    for (typed, sent) in [
        ("read @s-ab12", "@s-ab12 text"),
        ("read s-ab12 tail=3", "@s-ab12 text tail=3"),
        ("@s-ab12 read", "@s-ab12 text"),
        ("@s-ab12 read trim tail=2", "@s-ab12 text trim tail=2"),
        ("@self read", "@self text"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
    // `read` as a verb's own second word is that verb's, not a session's `text`.
    for alone in [
        "read",
        "read trim",
        "read tail=3",
        "@s-ab12 text",
        "text",
        "doc read d1",
        "doc read file:///tmp/x lines=1-2",
        "image read",
        "@s-ab12 doc read d1",
    ] {
        assert_eq!(rewritten(alone), None, "{alone}");
    }
}

#[test]
fn close_takes_its_one_session_after_the_verb_and_nothing_else() {
    for (typed, sent) in [
        ("close @s-ab12", "@s-ab12 close"),
        ("close s-ab12", "@s-ab12 close"),
        ("close @self", "@self close"),
        ("close @3", "@3 close"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
    for alone in [
        "close",
        "close s-foo",
        "close --help",
        "close s-ab12 now",
        "@s-ab12 close",
        "@s-ab12 close s-cd34",
    ] {
        assert_eq!(rewritten(alone), None, "{alone}");
    }
}

/// A verb that is no read answered at once keeps its words where they are: the client verbs, a
/// meta verb (some take a bare sid of their own), a wait, `subscribe` (its targets already
/// follow it) and a write, whose one spelling stays the protocol's.
#[test]
fn a_verb_that_takes_no_session_after_it_is_left_alone() {
    for typed in [
        "ls @s-ab12",
        "survey @s-ab12",
        "who @s-ab12",
        "hold s-ab12 on",
        "raise s-ab12",
        "help status",
        "spawn @s-ab12 agent=claude -",
        "await task @s-ab12,@s-cd34 since=0",
        "await @s-ab12 agent idle",
        "ready @s-ab12",
        "wait s-ab12",
        "sys @s-ab12",
        "subscribe @s-ab12 events",
        "send @s-ab12 hello",
        "turn s-ab12 hello",
        "meta @s-ab12 set title x",
        "inbox @s-ab12 get @5",
    ] {
        assert_eq!(rewritten(typed), None, "{typed}");
    }
}

/// A word that names no session stays the verb's own: after `search` a bare id or an `@` that
/// names nothing is part of the pattern, and nowhere is `s-` plus non-hex a session.
#[test]
fn a_word_that_names_no_session_is_left_alone() {
    for typed in [
        "search s-ab12",
        "search s-ab12 needle",
        "search s-foo",
        "search @foo",
        "search @ needle",
        "search @s-ab12:3 x",
        "status s-foo",
        "status S-AB12",
        "status s-",
        "text tail=3",
        "blocks 5",
        "log t3",
        "nosuchverb @s-ab12",
    ] {
        assert_eq!(rewritten(typed), None, "{typed}");
    }
    for (typed, sent) in [
        ("search @self needle", "@self search needle"),
        ("search @env needle", "@env search needle"),
        ("search @. needle", "@. search needle"),
        ("search @3 needle", "@3 search needle"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
}

/// An `@` after a read that names nothing (`text @$W` with `$W` empty) moves in front, where the
/// client refuses it as it refuses `@ text`: it is never read as the active tab.
#[test]
fn an_at_that_names_nothing_after_a_read_meets_the_empty_selector_refusal() {
    for typed in ["text @", "blocks @ 5"] {
        let parts = rewrite(&words(typed)).unwrap_or_else(|| panic!("{typed}"));
        assert_eq!(parts.first().map(String::as_str), Some("@"), "{typed}");
        let refusal = crate::refuse_empty_selectors(&parts).expect_err(typed);
        assert_eq!(
            refusal.to_string(),
            aterm_types::control_verbs::EMPTY_SELECTOR
        );
    }
}

/// `@self` is expanded, and the multiplexer boundary judged, on the rewritten form: from a pane
/// inside screen/tmux, `text @self` drives the outer terminal and is refused like `@self text`,
/// while a sid named after the verb is an explicit target and is not.
#[test]
fn self_and_the_multiplexer_boundary_read_the_rewritten_form() {
    let mut parts = rewrite(&words("text @self tail=2")).expect("rewritten");
    assert!(crate::targets_own_session_implicitly(&parts, None, None));
    crate::expand_self_selector(&mut parts, Some("s-ab12".to_owned())).expect("expanded");
    assert_eq!(parts, words("@s-ab12 text tail=2"));
    let named = rewrite(&words("status s-ab12")).expect("rewritten");
    assert!(!crate::targets_own_session_implicitly(&named, None, None));
    assert!(crate::targets_own_session_implicitly(
        &words("text"),
        None,
        None
    ));
}

/// Unpinned, a session named after a read's verb is followed to the instance hosting it, as
/// `@<sid> <verb>` is: the request line the client resolves its socket from is the rewritten
/// one, whose leading selector names the session's graph entry. The line as typed names none
/// there and would fall back to the rendezvous dir's alias.
#[cfg(unix)]
#[test]
fn a_session_after_its_verb_is_followed_to_the_instance_hosting_it() {
    let scratch = crate::test_scratch_dir("vf-host");
    let dir = scratch.path().join("rv");
    std::fs::create_dir_all(dir.join("graph")).expect("graph dir");
    let sock = dir.join("aterm-4004.sock");
    let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let sock = sock.to_str().expect("utf8 path").to_owned();
    std::fs::write(dir.join("graph").join("s-c8c3"), format!("sock {sock}\n")).expect("entry");
    let hosting = |line: &str| {
        let named = crate::redial::target_sid(line);
        let targets = crate::targets_in(Some(dir.clone()), None, None, None, named);
        let targets = targets.expect("targets");
        targets
            .hosting
            .into_iter()
            .map(|dial| dial.path)
            .collect::<Vec<_>>()
    };
    for typed in [
        "text s-c8c3 tail=3",
        "status @s-c8c3",
        "read s-c8c3",
        "@s-c8c3 read trim",
    ] {
        let sent = rewrite(&words(typed)).unwrap_or_else(|| panic!("{typed}"));
        assert_eq!(
            hosting(&sent.join(" ")),
            std::slice::from_ref(&sock),
            "{typed}"
        );
        if !typed.starts_with('@') {
            assert!(
                hosting(typed).is_empty(),
                "{typed} as typed names no session"
            );
        }
    }
}

/// The whole run: what reaches the socket for each form, and that an empty `@` never does.
#[cfg(unix)]
#[test]
fn the_socket_hears_the_protocols_form_and_a_word_that_names_no_session_as_typed() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;

    let scratch = crate::test_scratch_dir("verbfirst");
    let sock = scratch.path().join("s.sock");
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    let cases: [(&[&str], &str); 7] = [
        (&["status", "@s-ab12"], "@s-ab12 status"),
        (&["text", "s-ab12", "tail=3"], "@s-ab12 text tail=3"),
        (&["read", "@s-ab12"], "@s-ab12 text"),
        (&["@s-ab12", "read", "trim"], "@s-ab12 text trim"),
        (&["search", "@s-ab12", "needle"], "@s-ab12 search needle"),
        (&["search", "s-ab12"], "search s-ab12"),
        (&["blocks", "5"], "blocks 5"),
    ];
    let n = cases.len();
    let srv = std::thread::spawn(move || {
        let mut heard = Vec::new();
        for _ in 0..n {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("request");
            let reply: &[u8] = if line.contains("status") {
                b"OK schema=1 sid=s-ab12\n"
            } else {
                b"OK 0\n"
            };
            let mut w = conn;
            w.write_all(reply).expect("reply");
            w.flush().expect("flush");
            heard.push(line.trim_end().to_owned());
        }
        heard
    });
    let path = sock.to_str().expect("utf8 path").to_owned();
    let run = |argv: &[&str]| {
        let mut full = vec!["--sock", path.as_str()];
        full.extend_from_slice(argv);
        crate::real_main(
            full.iter().map(|a| std::ffi::OsString::from(*a)).collect(),
            crate::LocalVerbs::NONE,
        )
    };
    let refusal = run(&["text", "@"]).expect_err("an empty @ is refused before any dial");
    assert_eq!(
        refusal.to_string(),
        aterm_types::control_verbs::EMPTY_SELECTOR
    );
    for (argv, _) in &cases {
        let code = run(argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::SUCCESS),
            "{argv:?}"
        );
    }
    let heard = srv.join().expect("server");
    let want: Vec<&str> = cases.iter().map(|(_, sent)| *sent).collect();
    assert_eq!(heard, want);
}

/// `--help` after a verb-first read is that read's entry, answered offline: `read` is `text`
/// there too, on either side of the selector.
#[test]
fn help_after_a_verb_first_read_is_its_entry_with_no_dial() {
    let scratch = crate::test_scratch_dir("vf-help");
    let nowhere = scratch.path().join("none.sock");
    let nowhere = nowhere.to_str().expect("utf8 path").to_owned();
    for typed in [
        "read @s-ab12 --help",
        "@s-ab12 read --help",
        "status s-ab12 --help",
    ] {
        let argv = ["--sock", nowhere.as_str()]
            .into_iter()
            .chain(typed.split_whitespace())
            .map(std::ffi::OsString::from)
            .collect();
        let code = crate::real_main(argv, crate::LocalVerbs::NONE)
            .unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::SUCCESS),
            "{typed}"
        );
    }
}
