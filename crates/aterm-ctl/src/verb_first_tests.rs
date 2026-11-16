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
        ("status @tab12xxxxxx", "@tab12xxxxxx status"),
        ("status tab12xxxxxx", "@tab12xxxxxx status"),
        (
            "text @tab12xxxxxx tail=3 trim",
            "@tab12xxxxxx text tail=3 trim",
        ),
        ("text tab12xxxxxx --json", "@tab12xxxxxx text --json"),
        (
            "screen @tab12xxxxxx since=00ff",
            "@tab12xxxxxx screen since=00ff",
        ),
        ("cursor tab12xxxxxx", "@tab12xxxxxx cursor"),
        ("modes @tab12xxxxxx", "@tab12xxxxxx modes"),
        ("selection tab12xxxxxx", "@tab12xxxxxx selection"),
        ("blocks @tab12xxxxxx 5", "@tab12xxxxxx blocks 5"),
        ("chrome tab12xxxxxx", "@tab12xxxxxx chrome"),
        ("panes @tab12xxxxxx", "@tab12xxxxxx panes"),
        (
            "offscreen tab12xxxxxx tail=2 screen=1",
            "@tab12xxxxxx offscreen tail=2 screen=1",
        ),
        (
            "search @tab12xxxxxx needle case",
            "@tab12xxxxxx search needle case",
        ),
        ("inbox tab12xxxxxx 3 --peek", "@tab12xxxxxx inbox 3 --peek"),
        ("inbox @tab12xxxxxx get 7", "@tab12xxxxxx inbox get 7"),
        (
            "log @tab12xxxxxx class=turn since=4 ep=2",
            "@tab12xxxxxx log class=turn since=4 ep=2",
        ),
        ("result tab12xxxxxx full", "@tab12xxxxxx result full"),
        ("history @tab12xxxxxx 5", "@tab12xxxxxx history 5"),
        ("line tab12xxxxxx 40", "@tab12xxxxxx line 40"),
        ("doc @tab12xxxxxx read d1", "@tab12xxxxxx doc read d1"),
        ("image tab12xxxxxx read", "@tab12xxxxxx image read"),
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
        ("read @tab12xxxxxx", "@tab12xxxxxx text"),
        ("read tab12xxxxxx tail=3", "@tab12xxxxxx text tail=3"),
        ("@tab12xxxxxx read", "@tab12xxxxxx text"),
        (
            "@tab12xxxxxx read trim tail=2",
            "@tab12xxxxxx text trim tail=2",
        ),
        ("@self read", "@self text"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
    // `read` as a verb's own second word is that verb's, not a session's `text`.
    for alone in [
        "read",
        "read trim",
        "read tail=3",
        "@tab12xxxxxx text",
        "text",
        "doc read d1",
        "doc read file:///tmp/x lines=1-2",
        "image read",
        "@tab12xxxxxx doc read d1",
    ] {
        assert_eq!(rewritten(alone), None, "{alone}");
    }
}

#[test]
fn close_takes_its_one_session_after_the_verb_and_nothing_else() {
    for (typed, sent) in [
        ("close @tab12xxxxxx", "@tab12xxxxxx close"),
        ("close tab12xxxxxx", "@tab12xxxxxx close"),
        ("close @self", "@self close"),
        ("close @3", "@3 close"),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(sent), "{typed}");
    }
    for alone in [
        "close",
        "close s-foo",
        "close --help",
        "close tab12xxxxxx now",
        "@tab12xxxxxx close",
        "@tab12xxxxxx close tcd34xxxxxx",
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
        "ls @tab12xxxxxx",
        "survey @tab12xxxxxx",
        "who @tab12xxxxxx",
        "hold tab12xxxxxx on",
        "raise tab12xxxxxx",
        "help status",
        "spawn @tab12xxxxxx agent=claude -",
        "await task @tab12xxxxxx,@tcd34xxxxxx since=0",
        "await @tab12xxxxxx agent idle",
        "ready @tab12xxxxxx",
        "wait tab12xxxxxx",
        "sys @tab12xxxxxx",
        "subscribe @tab12xxxxxx events",
        "send @tab12xxxxxx hello",
        "turn tab12xxxxxx hello",
        "meta @tab12xxxxxx set title x",
        "inbox @tab12xxxxxx get @5",
    ] {
        assert_eq!(rewritten(typed), None, "{typed}");
    }
}

/// A word that names no session stays the verb's own: after `search` a bare id or an `@` that
/// names nothing is part of the pattern, and nowhere is `s-` plus non-hex a session.
#[test]
fn a_word_that_names_no_session_is_left_alone() {
    for typed in [
        "search tab12xxxxxx",
        "search tab12xxxxxx needle",
        "search s-foo",
        "search @foo",
        "search @ needle",
        "search @tab12xxxxxx:3 x",
        "status s-foo",
        "status S-AB12",
        "status s-",
        "text tail=3",
        "blocks 5",
        "log t3",
        "nosuchverb @tab12xxxxxx",
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
    crate::expand_self_selector(&mut parts, Some("tab12xxxxxx".to_owned())).expect("expanded");
    assert_eq!(parts, words("@tab12xxxxxx text tail=2"));
    let named = rewrite(&words("status tab12xxxxxx")).expect("rewritten");
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
    std::fs::write(
        dir.join("graph").join("tc8c3xxxxxx"),
        format!("sock {sock}\n"),
    )
    .expect("entry");
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
        "text tc8c3xxxxxx tail=3",
        "status @tc8c3xxxxxx",
        "read tc8c3xxxxxx",
        "@tc8c3xxxxxx read trim",
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
        (&["status", "@tab12xxxxxx"], "@tab12xxxxxx status"),
        (
            &["text", "tab12xxxxxx", "tail=3"],
            "@tab12xxxxxx text tail=3",
        ),
        (&["read", "@tab12xxxxxx"], "@tab12xxxxxx text"),
        (&["@tab12xxxxxx", "read", "trim"], "@tab12xxxxxx text trim"),
        (
            &["search", "@tab12xxxxxx", "needle"],
            "@tab12xxxxxx search needle",
        ),
        (&["search", "tab12xxxxxx"], "search tab12xxxxxx"),
        (&["blocks", "5"], "blocks 5"),
    ];
    let n = cases.len();
    let srv = std::thread::spawn(move || {
        let mut heard = Vec::new();
        for _ in 0..n {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("request");
            let reply: &[u8] = if line.contains("status") {
                b"OK schema=1 sid=tab12xxxxxx\n"
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
        "read @tab12xxxxxx --help",
        "@tab12xxxxxx read --help",
        "status tab12xxxxxx --help",
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
