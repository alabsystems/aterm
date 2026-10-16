// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn protocol_success_requires_a_complete_ok_token() {
    for header in ["OK", "OK 0", "OK version=1", "OK subscribe 2"] {
        assert!(is_ok_reply(header), "{header}");
    }
    for header in ["", "OKAY", "OK\tversion=1", "ERR busy"] {
        assert!(!is_ok_reply(header), "{header}");
    }
}

/// How long a wait for something that must happen (a reply, a thread's return)
/// may take before it is read as a hang: a hang detector, never a latency budget.
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(unix)]
static PROBE_SOCKET_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Leave at `path` what a crashed instance leaves (a socket file nobody
/// accepts on: bound, then dropped) and wait, bounded, until it refuses. A
/// child another test is spawning can hold the listener until it execs.
fn abandon_socket(path: &Path) {
    drop(aterm_uds::CtlListener::bind(path).expect("bind then abandon"));
    // The refusal is what the unix tests judge; elsewhere nothing waits for it.
    if cfg!(not(unix)) {
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let answer = CtlStream::connect(path).map(drop).map_err(|e| e.kind());
        if answer == Err(io::ErrorKind::ConnectionRefused) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the abandoned socket {} still answers {answer:?} after 60 s",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn operator_proposal_limit_matches_the_server_protocol_bound() {
    let limit = aterm_types::control_verbs::MAX_OPERATOR_PROPOSAL_BYTES;
    assert!(validate_operator_proposal_size(limit).is_ok());
    let error = validate_operator_proposal_size(limit + 1).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("65536-byte limit"));
}

/// `cast`/`temporal` are byte-framed (`OK <nbytes>` + body), so their bodies
/// stream to stdout; everything else keeps its framing.
#[test]
fn bytes_payload_gates_cast_and_temporal() {
    assert!(bytes_payload("cast", "cast"));
    assert!(bytes_payload("temporal", "temporal"));
    // `cast frames` is line-framed, not byte-framed.
    assert!(!bytes_payload("cast", "cast frames"));
    assert!(streams_payload("cast", "cast frames"));
    for verb in [
        "text",
        "screen",
        "subscribe",
        "image",
        "send",
        "lines",
        "copy",
    ] {
        assert!(!bytes_payload(verb, verb), "{verb} is not byte-framed");
    }
}

/// `byte_count` takes the first token, rejects garbage, and caps the
/// peer-supplied length fail-closed at `MAX_BODY_BYTES`.
#[test]
fn byte_count_first_token_and_cap() {
    assert_eq!(byte_count("1234"), Some(1234));
    assert_eq!(byte_count("0"), Some(0));
    assert_eq!(byte_count("77 incomplete"), Some(77));
    assert_eq!(byte_count(""), None);
    assert_eq!(byte_count("x"), None);
    assert_eq!(
        byte_count(&MAX_BODY_BYTES.to_string()),
        Some(MAX_BODY_BYTES)
    );
    assert_eq!(
        byte_count(&(MAX_BODY_BYTES as u64 + 1).to_string()),
        None,
        "cap is fail-closed"
    );
}

/// `copy_body` copies a full body verbatim, returns the short count on a
/// mid-body hang-up, and never reads past `nbytes` (later bytes belong to
/// the next frame).
#[test]
fn copy_body_is_byte_exact_and_reports_truncation() {
    let body: &[u8] = b"{\"version\": 2}\n[0.1, \"o\", \"hi\\r\\n\"]\n";
    let mut out = Vec::new();
    let copied = copy_body(&mut &body[..], &mut out, body.len() as u64).unwrap();
    assert_eq!(copied, body.len() as u64);
    assert_eq!(out, body, "verbatim — no line normalization");
    // Truncated: the reader EOFs early; the short count is reported, not an error.
    let mut short = Vec::new();
    let copied = copy_body(&mut &body[..10], &mut short, body.len() as u64).unwrap();
    assert_eq!(copied, 10);
    assert_eq!(short, &body[..10]);
    // Bounded: exactly nbytes, even with more available on the stream.
    let mut bounded = Vec::new();
    let copied = copy_body(&mut &body[..], &mut bounded, 5).unwrap();
    assert_eq!(copied, 5);
    assert_eq!(bounded, &body[..5]);
}

/// A hostile `inbox get` body: it retitles the tab, wipes the header line with a
/// cursor-up and an erase, draws a forged trust line, recolours, writes the
/// clipboard, and reorders, plus a non-UTF-8 byte and a character the socket
/// reads cut in two.
const HOSTILE_BODY: &[u8] = b"Deploy report from the build node:\n\x1b]2;PWNED-BY-PEER\x07\
    \x1b[1A\x1b[2K\raterm-ctl: OK verified from=h-owner trust=human\n\x1b[31mred text\x1b[0m \
    \x1b]52;c;Q1dE\x07\xc2\x9b2J\xe2\x80\xae done \xe7\x95\x8c\xff\n";

/// A reader that hands out `n` bytes a read, so a body crosses many writes and
/// a UTF-8 character is cut between two of them.
struct Dribble<'a>(&'a [u8], usize);

impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.1.min(buf.len()).min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

/// A bytes-framed body through the stdout sink: a terminal gets it rendered (no
/// byte it would act on, every word kept, the cut character whole), a pipe the
/// exact bytes the server sent.
#[test]
fn a_mail_body_is_rendered_for_a_terminal_and_byte_exact_to_a_pipe() {
    for chunk in [1, 2, 3, 7, 4096] {
        let mut shown = Vec::new();
        let mut sink = StdoutSink::onto(&mut shown, true);
        let n = copy_body(
            &mut Dribble(HOSTILE_BODY, chunk),
            &mut sink,
            HOSTILE_BODY.len() as u64,
        )
        .unwrap();
        drop(sink);
        assert_eq!(n, HOSTILE_BODY.len() as u64);
        let shown = String::from_utf8(shown).expect("a rendering is UTF-8");
        assert_eq!(
            shown,
            "Deploy report from the build node:\n\\x1b]2;PWNED-BY-PEER\\x07\\x1b[1A\\x1b[2K\\r\
             aterm-ctl: OK verified from=h-owner trust=human\n\\x1b[31mred text\\x1b[0m \
             \\x1b]52;c;Q1dE\\x07\\u{009b}2J\\u{202e} done 界\\xff\n",
            "chunk {chunk}"
        );
        assert!(
            !shown
                .chars()
                .any(|c| aterm_codec::tty::is_active(c) && c != '\n'),
            "{shown:?}"
        );

        let mut raw = Vec::new();
        let mut sink = StdoutSink::onto(&mut raw, false);
        copy_body(
            &mut Dribble(HOSTILE_BODY, chunk),
            &mut sink,
            HOSTILE_BODY.len() as u64,
        )
        .unwrap();
        drop(sink);
        assert_eq!(raw, HOSTILE_BODY, "a pipe gets the bytes, chunk {chunk}");
    }
    // A body that ends inside a character: its held bytes are shown when the
    // sink is done with, never lost.
    let mut shown = Vec::new();
    let mut sink = StdoutSink::onto(&mut shown, true);
    sink.write_all(b"cut \xe2\x80").unwrap();
    drop(sink);
    assert_eq!(shown, b"cut \\xe2\\x80");
}

/// Rows and status lines through the same sink (a `dial`ed remote's `screen`
/// rows and `OK` lines): rendered for a terminal, as they came for a pipe, and
/// an honest server's already-encoded row unchanged either way.
#[test]
fn rows_through_the_sink_are_rendered_for_a_terminal_only() {
    let honest = "row 0 cwd=/tmp/CWD%1B]52%C2%9B title=\"build \u{754C}\" {\"t\":\"\\u001b\"}\n";
    let hostile = "\u{1b}]52;c;ZWNobyBwd25lZAo=\u{7}fake \u{9b}2J row\n";
    for (terminal, want_hostile) in [
        (
            true,
            "\\x1b]52;c;ZWNobyBwd25lZAo=\\x07fake \\u{009b}2J row\n",
        ),
        (false, hostile),
    ] {
        let mut out = Vec::new();
        let mut sink = StdoutSink::onto(io::BufWriter::new(&mut out), terminal);
        sink.write_all(honest.as_bytes()).unwrap();
        sink.write_all(hostile.as_bytes()).unwrap();
        sink.flush().unwrap();
        drop(sink);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{honest}{want_hostile}"),
            "terminal={terminal}"
        );
    }
}

/// Every stderr line (the server's `ERR` or `OK` words, a remote's through
/// `dial`) is rendered for a terminal, so a CR and erase cannot wipe the
/// `aterm-ctl:` prefix nor an OSC 52 reach the clipboard, and goes raw to a pipe.
#[test]
fn a_stderr_line_is_rendered_for_a_terminal_and_raw_to_a_pipe() {
    let err = "ERR \u{1b}]52;c;ZWNobyBwd25lZAo=\u{7}\u{1b}]0;PWNED\u{7}\u{1b}[2K\rfake error";
    assert_eq!(
        prefixed_line("aterm-ctl: ", err, true),
        b"aterm-ctl: ERR \\x1b]52;c;ZWNobyBwd25lZAo=\\x07\\x1b]0;PWNED\\x07\\x1b[2K\\rfake error\n"
    );
    assert_eq!(
        prefixed_line("aterm-ctl: ", err, false),
        format!("aterm-ctl: {err}\n").into_bytes()
    );
    // The local server's own words are already printable: unchanged.
    let ok = "OK 6 truncated=1 len=100 from=s-1@n-2 trust=agent";
    assert_eq!(
        prefixed_line("aterm-ctl: ", ok, true),
        prefixed_line("aterm-ctl: ", ok, false)
    );
}

/// A refusal's continuation lines take the same rendering: on a terminal no
/// line can erase the one above it or plant a link.
#[test]
fn a_refusal_tail_is_rendered_for_a_terminal_and_raw_to_a_pipe() {
    let reply = b"ERR usage\n\x1b[1A\x1b[2K\rforged-err\n  \x1b]8;;http://evil.example\x07click\n";
    for (terminal, want) in [
        (
            true,
            &b"\\x1b[1A\\x1b[2K\\rforged-err\n  \\x1b]8;;http://evil.example\\x07click\n"[..],
        ),
        (
            false,
            &b"\x1b[1A\x1b[2K\rforged-err\n  \x1b]8;;http://evil.example\x07click\n"[..],
        ),
    ] {
        let mut reader = BufReader::new(io::Cursor::new(reply.to_vec()));
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let mut err = Vec::new();
        drain_refusal_tail_into(&mut reader, &mut err, terminal).unwrap();
        assert_eq!(err, want, "terminal={terminal}");
    }
}

/// `<verb> --help` becomes a catalog query and never runs the verb.
#[test]
fn help_after_a_verb_becomes_a_catalog_query_not_an_action() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    let h = |s: &str| help_request_rewrite(&p(s));
    for req in [
        "turn --help",
        "turn -h",
        "window --help",
        "text --help",
        "settings --help",
        "grants -h",
        // A selector and the `dial` form resolve through the same rule.
        "@s-abc text --help",
        "dial box screen --help",
        "dial box @s-remote screen -h",
        // Past other arguments, not just adjacent to the verb.
        "search needle --json --help",
        // `help --help` asks about `help` itself.
        "help --help",
        "help -h",
    ] {
        let verb = forwarded_verb(&p(req)).expect("probe has a verb");
        assert_eq!(
            h(req),
            Some(vec![String::from("help"), verb.clone()]),
            "`{req}` must ask the catalog about `{verb}`, not run it"
        );
    }
    // A `dial` that names no remote verb asks about `dial` itself.
    for req in ["dial --help", "dial -h", "dial box --help"] {
        assert_eq!(
            h(req),
            Some(vec![String::from("help"), String::from("dial")]),
            "`{req}` asks about dial"
        );
    }
}

/// The rewrite does not fire on a request that is already a help query, on
/// a verb with no help token, or on a token that merely looks like one.
#[test]
fn help_rewrite_leaves_everything_else_alone() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    for req in [
        "help",
        "help text",
        "help --full",
        "text",
        "send hello",
        // Not the exact token: neither is a request for help.
        "send --help=1",
        "send x--help",
        "send help",
        // The verb itself is never scanned, only what follows it.
        "-h",
    ] {
        assert_eq!(
            help_request_rewrite(&p(req)),
            None,
            "`{req}` must reach the server unchanged"
        );
    }
    assert_eq!(help_request_rewrite(&[]), None);
}

/// Every client-answered discovery verb has a local help entry, so `ls
/// --help` never goes to a server that does not know `ls`.
#[test]
fn rewritten_discovery_help_is_answerable_without_the_server() {
    for verb in DISCOVERY_VERBS {
        let req = vec![String::from(verb), String::from("--help")];
        let rewritten = help_request_rewrite(&req).expect("discovery verb rewrites");
        assert!(
            client_help_reply(&rewritten, HelpAsked::ByFlag).is_some(),
            "`aterm ctl {verb} --help` rewrites to `{rewritten:?}`, which no \
             client entry answers -- it would dial the socket and be told the \
             verb does not exist"
        );
    }
}

#[test]
fn forwarded_verb_skips_proxy_selector() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    assert_eq!(forwarded_verb(&p("screen")).as_deref(), Some("screen"));
    assert_eq!(
        forwarded_verb(&p("@s-abc screen")).as_deref(),
        Some("screen")
    );
    assert_eq!(forwarded_verb(&p("@. text")).as_deref(), Some("text"));
    assert_eq!(
        forwarded_verb(&p("@s-abc image read")).as_deref(),
        Some("image")
    );
    assert_eq!(forwarded_verb(&p("send hi there")).as_deref(), Some("send"));
    assert_eq!(forwarded_verb(&[]), None);
    // Degenerate bare selector: falls back to it (the server then errors).
    assert_eq!(forwarded_verb(&p("@s-abc")).as_deref(), Some("@s-abc"));
}

/// `dial <name> <verb...>` frames the reply by the forwarded verb
/// (`parts[2]`), including positional sub-forms (`image read`, `cast
/// frames`) and bare `cast`.
#[test]
fn dial_frames_response_by_forwarded_verb_not_dial() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    assert_eq!(
        forwarded_verb(&p("dial myhost text")).as_deref(),
        Some("text")
    );
    assert_eq!(
        forwarded_verb(&p("dial myhost cursor")).as_deref(),
        Some("cursor")
    );
    assert_eq!(
        forwarded_verb(&p("dial myhost image read")).as_deref(),
        Some("image")
    );
    // `dial <name> @<sid> <verb>`: the framing verb skips the remote
    // selector.
    assert_eq!(
        forwarded_verb(&p("dial myhost @s-remote text")).as_deref(),
        Some("text"),
        "dial + remote selector frames on the forwarded verb, not the selector"
    );
    assert_eq!(
        forwarded_verb(&p("dial myhost @s-remote image read")).as_deref(),
        Some("image")
    );
    // `frame_request` (what `real_main` passes for a dial = `request_parts[2..]`)
    // decides Lines/Bytes/Status exactly as if the verb had been run locally.
    assert!(streams_payload("text", "text"), "dial ... text => Lines");
    assert!(
        !streams_payload("cursor", "cursor") && !bytes_payload("cursor", "cursor"),
        "dial ... cursor => Status"
    );
    assert!(
        streams_payload("image", "image read"),
        "dial ... image read => Lines"
    );
    assert!(
        streams_payload("cast", "cast frames"),
        "dial ... cast frames => Lines"
    );
    assert!(bytes_payload("cast", "cast"), "dial ... cast => Bytes");
}

/// A bare `dial <name>` (or `dial` alone) is rejected before any bytes are
/// sent, with a nonzero, non-124 exit.
#[test]
fn bare_dial_is_rejected_before_send() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    assert!(dial_missing_verb(&p("dial")), "`dial` alone has no verb");
    assert!(
        dial_missing_verb(&p("dial myhost")),
        "`dial <name>` has no verb"
    );
    assert!(
        !dial_missing_verb(&p("dial myhost text")),
        "`dial <name> <verb>` is complete"
    );
    assert!(
        !dial_missing_verb(&p("text")),
        "non-dial verbs are unaffected"
    );
    assert!(!dial_missing_verb(&[]), "empty request is not a dial");

    let e = dial_needs_verb_error();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert!(
        !is_timeout_error(&e),
        "bare dial is a plain failure, not a 124 timeout"
    );
    assert_eq!(
        e.to_string(),
        "ERR dial: give a verb to run on the remote (dial <name> <verb...>)"
    );
}

/// End to end over a mock socket: `dial myhost text` sends the dial line
/// verbatim and reads back the remote's line-framed `text` reply without
/// blocking. Unix only: the mock instance is a `UnixListener`.
#[cfg(unix)]
#[test]
fn dial_with_verb_reads_lines_reply_without_hanging() {
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    // A private dir holding only the socket (no token file), so no AUTH line
    // is sent and the mock reads `dial myhost text` first.
    let dir = std::env::temp_dir().join(format!("aterm-ctl-dialtest-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("mock.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");

    // Mock local instance: accept, read the `dial` line, send the line-framed
    // reply, then drain until the client closes.
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut first = String::new();
        r.read_line(&mut first).expect("read request line");
        let mut w = conn;
        w.write_all(b"OK 2\nhello\nworld\n").expect("write reply");
        w.flush().expect("flush");
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
        first
    });

    // Drive `exchange` exactly as `real_main` does for `dial myhost text`: the
    // line sent is the full dial line; framing follows the forwarded verb `text`.
    let sockpath = sock.to_str().expect("utf8 path").to_string();
    let (tx, rx) = mpsc::channel();
    let cli = std::thread::spawn(move || {
        let res = exchange(
            &sockpath,
            TargetOrigin::Pinned,
            "dial myhost text\n",
            "text",
            "text",
            Some(HANG_DETECTOR),
            false,
            &redial::Redial::none(),
        );
        let _ = tx.send(res.is_ok());
    });

    // The exchange finishes inside its own deadline: a client waiting for the
    // mock to close would run [`HANG_DETECTOR`] out and fail. The recv outlasts it.
    let ok = rx
        .recv_timeout(2 * HANG_DETECTOR)
        .expect("dial exchange must not block indefinitely");
    assert!(
        ok,
        "dial myhost text should complete the exchange (OK reply)"
    );
    cli.join().expect("client thread");
    let relayed = srv.join().expect("server thread");
    assert_eq!(
        relayed, "dial myhost text\n",
        "client must relay the dial line verbatim"
    );
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// `help <verb>` against an older server: a verb it calls unknown is still
/// answered from this client's entry (exit 0), `help cast` without `cast
/// drift` gets its own entry, and other refusals stay refusals.
#[cfg(unix)]
#[test]
fn help_fills_what_an_older_server_cannot_say() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("aterm-ctl-oldhelp-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("old.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    let srv = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..3 {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("request");
            let reply = match line.trim_end() {
                "help resizes" => "ERR unknown verb 'resizes' (help lists them)\n".to_string(),
                "help cast" => {
                    "OK 1\ncast  asciicast v2 recording (compact, sendable);\n".to_string()
                }
                _ => "ERR usage: help [<verb> | --full | --json]\n".to_string(),
            };
            let mut w = conn;
            w.write_all(reply.as_bytes()).expect("reply");
            w.flush().expect("flush");
            seen.push(line.trim_end().to_string());
        }
        seen
    });
    let path = sock.to_str().expect("utf8 path").to_string();
    let deadline = Some(std::time::Duration::from_secs(60));
    let ask = |name: &str| {
        help_with_fallback(
            &path,
            TargetOrigin::Pinned,
            name,
            deadline,
            &LocalVerbs::NONE,
        )
        .expect("an exchange")
    };
    assert_eq!(ask("resizes"), ExitCode::SUCCESS, "this client's entry");
    assert_eq!(ask("cast"), ExitCode::SUCCESS, "the server's entry");
    assert_eq!(ask("status"), ExitCode::FAILURE, "a refusal stays one");
    assert_eq!(
        srv.join().expect("server"),
        ["help resizes", "help cast", "help status"]
    );
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// The `exchange` backstop: `dial <name> cast drift` against a remote that
/// predates the verb answers the bare cast (`OK <nbytes>` + body, no
/// `verdict=`). The client reads the body off and refuses by name,
/// promptly, with nothing on stdout.
#[cfg(unix)]
#[test]
fn dial_cast_drift_refuses_an_older_remotes_bare_cast() {
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    const CAST: &str = "{\"version\": 2, \"width\": 20, \"height\": 8}\n\
                        [0.1, \"o\", \"hi\"]\n[0.2, \"r\", \"20x7\"]\n";
    let dir = std::env::temp_dir().join(format!("aterm-ctl-dialdrift-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("old.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    // The relay keeps the connection open after its reply, as a live one
    // does: a client reading lines of a byte body would wait forever.
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut first = String::new();
        r.read_line(&mut first).expect("read request line");
        let mut w = conn;
        w.write_all(format!("OK {}\n{CAST}", CAST.len()).as_bytes())
            .expect("write reply");
        w.flush().expect("flush");
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
        first
    });
    let sockpath = sock.to_str().expect("utf8 path").to_string();
    let (tx, rx) = mpsc::channel();
    let cli = std::thread::spawn(move || {
        let res = exchange(
            &sockpath,
            TargetOrigin::Pinned,
            "dial far cast drift\n",
            "cast",
            "cast drift",
            // Longer than the recv below, so a client reading lines of the
            // byte body is caught by the recv.
            Some(std::time::Duration::from_secs(120)),
            false,
            &redial::Redial::none(),
        );
        let _ = tx.send(res.map_err(|e| e.to_string()));
    });
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the refusal must not wait for the connection to close");
    let err = res.expect_err("an older remote's bare cast is refused");
    assert!(err.contains("predates `cast drift`"), "{err}");
    cli.join().expect("client thread");
    assert_eq!(srv.join().expect("server thread"), "dial far cast drift\n");
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// `search`'s header is `OK <n> incomplete[ capped][ rewrapping][ long-line][ older]`
/// when the list is not exhaustive; the count parser reads only the first
/// token, so the matches are kept, and stderr names every cause.
#[test]
fn stream_count_ignores_incomplete_marker() {
    // `OK 3 incomplete` -> tail "3 incomplete" -> count 3.
    assert_eq!(stream_count("3 incomplete"), Some(3));
    assert_eq!(stream_count("100000 incomplete capped"), Some(100_000));
    let note = search_incomplete_note;
    let depth = "older history not searched (search_history_lines)";
    let limit = "results stop at 100000 matches";
    let long_line =
        "a long line was not searched to its end: the regex ran out of scan budget there";
    assert_eq!(note("3 incomplete").as_deref(), Some(depth));
    assert_eq!(note("100000 incomplete capped").as_deref(), Some(limit));
    assert_eq!(
        note("0 incomplete rewrapping").as_deref(),
        Some("screen only: the history is rewrapping")
    );
    assert_eq!(note("7 incomplete long-line").as_deref(), Some(long_line));
    assert_eq!(note("100000 incomplete").as_deref(), Some(limit));
    assert_eq!(
        note("3 incomplete changed screen_first=21").as_deref(),
        Some("the terminal changed while the search ran")
    );
    assert_eq!(
        note("3 incomplete future-cause screen_first=21").as_deref(),
        Some("the server reported an unrecognized search completeness cause")
    );
    assert_eq!(note("3 incomplete screen_first=21").as_deref(), Some(depth));
    assert_eq!(note("3 screen_first=21"), None);
    // Every cause the server names is said, not only the first.
    assert_eq!(
        note("1 incomplete long-line older").as_deref(),
        Some(format!("{long_line}; {depth}").as_str())
    );
    assert_eq!(
        note("100000 incomplete capped older").as_deref(),
        Some(format!("{limit}; {depth}").as_str())
    );
    assert_eq!(note("3"), None);
    // Clean `OK <n>` headers (every other streaming verb) still parse.
    assert_eq!(stream_count("42"), Some(42));
    assert_eq!(stream_count("0"), Some(0));
    // A non-numeric first token is genuinely malformed.
    assert_eq!(stream_count(""), None);
    assert_eq!(stream_count("incomplete"), None);
    // A peer-supplied count past the bound is rejected like a malformed
    // header — it must never size an allocation.
    assert_eq!(
        stream_count(&MAX_STREAM_LINES.to_string()),
        Some(MAX_STREAM_LINES)
    );
    assert_eq!(stream_count(&(MAX_STREAM_LINES + 1).to_string()), None);
    assert_eq!(stream_count("18446744073709551615"), None);
}

/// `malformed_header_error` renders the status line by hand, byte for byte
/// equal to `format!("... {status_line:?}")`, including `Debug`-for-`str`
/// corners: literal single quotes, escaped double quotes/backslashes,
/// control chars and grapheme-extended chars.
#[test]
fn malformed_header_error_matches_debug_formatting() {
    for s in [
        "OK x",
        "",
        "it's fine",
        "say \"hi\"",
        "back\\slash",
        "tab\tnl\nnul\0",
        "caf\u{e9} + combining e\u{301}",
        "emoji \u{1f980}",
    ] {
        let e = malformed_header_error(s);
        assert_eq!(
            e.to_string(),
            format!("malformed response header: {s:?}"),
            "drifted for input {s:?}"
        );
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}

/// `connect_error` frames `connect {path}: {cause}` and preserves the error
/// kind. `NotFound` and `ConnectionRefused` also carry the not-running
/// remedy. Pinned on the pure rendering with an empty fleet, since
/// `connect_error` itself looks the fleet up.
#[test]
fn connect_error_matches_format_and_keeps_kind() {
    let path = "/tmp/aterm-test/aterm.sock";
    // A kind that does not mean "not running" keeps the bare cause: a
    // permission error must never claim aterm is down.
    let cause = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");
    for origin in [TargetOrigin::Flagless, TargetOrigin::Pinned] {
        let e = connect_error_naming(path, &cause, origin, None, &[]);
        assert_eq!(e.to_string(), format!("connect {path}: {cause}"));
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }

    // NotFound / ConnectionRefused say what the failure means and what to do,
    // framed on the path tried and kind-preserving.
    for (kind, oserr) in [
        (
            io::ErrorKind::NotFound,
            "No such file or directory (os error 2)",
        ),
        (
            io::ErrorKind::ConnectionRefused,
            "Connection refused (os error 61)",
        ),
    ] {
        let cause = io::Error::new(kind, oserr);
        let e = connect_error_naming(path, &cause, TargetOrigin::Flagless, None, &[]);
        // With nothing live there is nothing to name and nothing to count,
        // but a pin is not where a new instance listens: retrying it
        // cannot work, so its remedy names where the new pid is listed.
        let pinned =
            connect_error_naming(path, &cause, TargetOrigin::Pinned, None, &[]).to_string();
        assert!(
            pinned.contains("aterm isn't running")
                && pinned.contains("then `aterm ctl instances` names its pid")
                && !pinned.contains("and retry"),
            "{pinned}"
        );
        assert!(e.to_string().ends_with(" and retry"), "{e}");
        let msg = e.to_string();
        assert!(
            msg.starts_with(&format!("connect {path}: {oserr}")),
            "the raw cause stays first: {msg}"
        );
        assert!(msg.contains("aterm isn't running"), "{msg}");
        // The launch remedy must be runnable where printed: `open -a` is
        // macOS only.
        #[cfg(not(target_os = "macos"))]
        assert!(
            !msg.contains("open -a aterm") && !msg.contains("aterm.app"),
            "a non-macOS build must not name a macOS-only remedy: {msg}"
        );
        assert!(
            msg.contains(LAUNCH_REMEDY),
            "the remedy is named, and is the one THIS platform can run: {msg}"
        );
        assert_eq!(e.kind(), kind);
    }
}

/// A dead socket beside a live instance does not claim "aterm isn't running".
/// Flagless, the error names the flag that reaches each live instance (`--pid`
/// only for the default dir's own `aterm-<pid>.sock`); pinned, only the count
/// and `aterm ctl instances`.
#[test]
fn connect_error_names_the_instances_that_are_alive() {
    let dir = Path::new("/tmp/aterm-test");
    let path = "/tmp/aterm-test/aterm-10274.sock";
    // The dir's own socket as `instance_socks_in` yields it (`dir.join`, with
    // the platform separator); expectations use the same join.
    let own = dir
        .join(control_socket::instance_sock_name(99878))
        .to_string_lossy()
        .into_owned();
    let cause = io::Error::new(
        io::ErrorKind::ConnectionRefused,
        "Connection refused (os error 61)",
    );
    let live = vec![
        // A legacy graph entry with no `pid` line: the `0` placeholder.
        (0, "/tmp/private/a.sock".to_string()),
        // An explicit socket whose graph entry carries the hosting pid.
        (4242, "/tmp/private/b.sock".to_string()),
        // A socket that looks per-instance but lives outside the default
        // dir: `--pid 7` would dial `/tmp/aterm-test/aterm-7.sock`, not it.
        (7, "/tmp/elsewhere/aterm-7.sock".to_string()),
        // The default dir's own per-instance socket: `--pid` reaches it.
        (99878, own.clone()),
    ];
    let e = connect_error_naming(path, &cause, TargetOrigin::Flagless, Some(dir), &live);
    let msg = e.to_string();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
    assert!(
        msg.starts_with(
            "connect /tmp/aterm-test/aterm-10274.sock: Connection refused (os error 61)"
        ),
        "the raw cause stays first: {msg}"
    );
    assert!(
        msg.contains(&format!(
            "aterm IS running: --sock /tmp/private/a.sock, \
             --sock /tmp/private/b.sock (pid 4242), \
             --sock /tmp/elsewhere/aterm-7.sock (pid 7), \
             --pid 99878 ({own})"
        )),
        "{msg}"
    );
    assert!(
        msg.ends_with(
            "; target one with that flag (`aterm ctl instances` lists every live instance)"
        ),
        "the flagless form hands the choice back: {msg}"
    );
    // The same socket spelled differently (a doubled separator, `/` against
    // `\` on Windows) is still the dir's own; neither spelling exists on
    // disk, so only the component-wise comparison can tell.
    assert!(reached_by_pid(
        Some(dir),
        99878,
        "/tmp/aterm-test//aterm-99878.sock"
    ));
    assert!(reached_by_pid(
        Some(dir),
        99878,
        "/tmp/aterm-test/aterm-99878.sock"
    ));
    assert!(!reached_by_pid(Some(dir), 7, "/tmp/elsewhere/aterm-7.sock"));
    assert!(
        !msg.contains("--pid 4242") && !msg.contains("--pid 7 "),
        "`--pid` is a remedy only for the default dir's own socket: {msg}"
    );
    assert!(
        !msg.contains("isn't running") && !msg.contains(LAUNCH_REMEDY),
        "a live instance refutes both the claim and the launch remedy: {msg}"
    );
    // No resolvable default dir: `--pid` can dial nothing, so every
    // instance — the per-instance-looking one included — is a `--sock`.
    let msg = connect_error_naming(path, &cause, TargetOrigin::Flagless, None, &live).to_string();
    assert!(
        msg.contains(&format!("--sock {own} (pid 99878)")) && !msg.contains("--pid"),
        "{msg}"
    );
    // Pinned: the caller named the socket, so the error names no instance
    // and hands out no flag: the fact, the count, and where the list is.
    let pinned = connect_error_naming(path, &cause, TargetOrigin::Pinned, Some(dir), &live);
    assert_eq!(pinned.kind(), io::ErrorKind::ConnectionRefused);
    let msg = pinned.to_string();
    assert_eq!(
        msg,
        "connect /tmp/aterm-test/aterm-10274.sock: Connection refused (os error 61) \
         — nothing is serving this control socket, but aterm IS running \
         (4 other instances; `aterm ctl instances` lists them)"
    );
    assert!(
        !msg.contains("--pid") && !msg.contains("--sock") && !msg.contains("target one"),
        "a pinned caller is handed no other instance to drive: {msg}"
    );
    assert!(
        live.iter().all(|(_, sock)| !msg.contains(sock.as_str())),
        "no live socket is named: {msg}"
    );
    // One other instance reads in the singular, and is not named either.
    let one = connect_error_naming(path, &cause, TargetOrigin::Pinned, Some(dir), &live[3..]);
    assert!(
        one.to_string()
            .ends_with("aterm IS running (1 other instance; `aterm ctl instances` lists it)"),
        "{one}"
    );
    assert!(!one.to_string().contains("99878"), "{one}");
    // A kind that does not mean "no listener" names nobody, live or not,
    // whichever way the socket was chosen.
    let denied = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");
    for origin in [TargetOrigin::Flagless, TargetOrigin::Pinned] {
        assert_eq!(
            connect_error_naming(path, &denied, origin, Some(dir), &live).to_string(),
            format!("connect {path}: {denied}")
        );
    }
}

/// `streams_payload`: the streaming-verb set and the selector-aware `image read`
/// detection.
#[test]
fn streams_payload_gates_by_verb_and_image_read() {
    // Streaming verbs frame `OK <n>` + n lines.
    for verb in [
        "text",
        "screen",
        "search",
        "modes",
        "selection",
        "blocks",
        "blocktext",
        "chrome",
        "panes",
        "sessions",
        "family",
        "who",
        "edges",
        "grants",
        "controls",
        // `offscreen` is `OK <n> first=… …` + n rows, unlike its status-line
        // neighbour `lines`.
        "offscreen",
    ] {
        assert!(streams_payload(verb, verb), "{verb} should stream");
    }
    assert!(streams_payload(
        "offscreen",
        "@s-a offscreen since=3 screen=1"
    ));
    // Single-line verbs echo their status line (even integer-tailed ones).
    for verb in ["cursor", "copy", "lines", "feed", "signal", "image"] {
        assert!(!streams_payload(verb, verb), "{verb} should not stream");
    }
    // `image read` streams; detection skips a leading proxy selector.
    assert!(streams_payload("image", "image read\n"));
    assert!(streams_payload("image", "@s-abc image read\n"));
    assert!(!streams_payload("image", "image shot.png\n"));
    assert!(!streams_payload("image", "@s-abc image shot.png\n"));
}

/// A refusal's continuation lines are consumed, printed and bounded. The
/// reader is primed as in `exchange`, which has already read the status
/// line through the same `BufReader`.
#[test]
fn a_multi_line_err_is_drained_to_the_end_and_bounded() {
    let bytes = |tail: &str| {
        let mut v = b"ERR usage: subscribe\n".to_vec();
        v.extend_from_slice(tail.as_bytes());
        v
    };
    let primed = |v: Vec<u8>| {
        let mut r = BufReader::new(io::Cursor::new(v));
        let mut status = String::new();
        r.read_line(&mut status).unwrap();
        r
    };

    let mut reader = primed(bytes(
        "  <streams>: a subset of screen,mail and must name one\n",
    ));
    drain_refusal_tail(&mut reader).unwrap();
    let mut rest = String::new();
    reader.read_line(&mut rest).unwrap();
    assert_eq!(rest, "", "the second line was consumed");

    // A refusal with no tail at all.
    let mut reader = primed(bytes(""));
    drain_refusal_tail(&mut reader).unwrap();

    let many: String = (0..REFUSAL_TAIL_MAX_LINES + 4)
        .map(|i| format!("line {i}\n"))
        .collect();
    let mut reader = primed(bytes(&many));
    drain_refusal_tail(&mut reader).unwrap();
    let mut rest = String::new();
    reader.read_line(&mut rest).unwrap();
    assert_eq!(rest, format!("line {REFUSAL_TAIL_MAX_LINES}\n"), "bounded");
}

/// The drain never touches the socket: a control connection stays open
/// after a refusal, so any read would block until the deadline. This reader
/// panics if read.
#[test]
fn the_drain_never_reads_from_the_socket() {
    struct Detonate;
    impl Read for Detonate {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("drain_refusal_tail issued a read — that is the hang");
        }
    }

    // Empty buffer: nothing in hand, and nothing may be fetched.
    let mut reader = BufReader::new(Detonate);
    drain_refusal_tail(&mut reader).unwrap();

    // A buffered tail is still printed without a read, and a trailing partial
    // line (no newline) is left alone rather than completed.
    let mut reader = BufReader::new(io::Cursor::new(
        b"ERR usage: subscribe\n  one\n  partial-no-newline".to_vec(),
    ));
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    drain_refusal_tail(&mut reader).unwrap();
    assert_eq!(
        reader.buffer(),
        b"  partial-no-newline",
        "the incomplete line is left where it is"
    );
}

#[test]
fn guarded_file_artifact_echoes_only_the_server_nonce() {
    let (client, mut server) = CtlStream::pair().unwrap();
    server
        .write_all(b"ACK-CHALLENGE 00112233445566778899aabbccddeeff\n")
        .unwrap();
    server.flush().unwrap();
    let peer = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(&server).read_line(&mut line).unwrap();
        line
    });
    let mut reader = BufReader::new(&client);
    acknowledge_artifact_reply(&client, &mut reader, "image", "image shot.png").unwrap();
    assert_eq!(
        peer.join().unwrap(),
        "ACK 00112233445566778899aabbccddeeff\n"
    );
}

#[test]
fn in_memory_image_never_probes_for_an_optional_ack_challenge() {
    let (client, mut server) = CtlStream::pair().unwrap();
    server.write_all(b"not-an-ack-challenge\n").unwrap();
    server.flush().unwrap();
    let mut reader = BufReader::new(&client);
    acknowledge_artifact_reply(&client, &mut reader, "image", "image --bytes").unwrap();

    let mut untouched = String::new();
    reader.read_line(&mut untouched).unwrap();
    assert_eq!(untouched, "not-an-ack-challenge\n");
}

#[test]
fn guarded_artifact_reply_accepts_a_legacy_server_without_hanging() {
    let (client, _legacy_server) = CtlStream::pair().unwrap();
    let mut reader = BufReader::new(&client);
    acknowledge_artifact_reply(&client, &mut reader, "image", "image shot.png")
        .expect("an absent optional challenge is a bounded legacy fallback");
}

#[test]
fn guarded_payload_ack_precedes_the_user_output_handoff() {
    let (client, mut server) = CtlStream::pair().unwrap();
    let (acked_tx, acked_rx) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        server
            .write_all(
                b"OK 1\nframe n=1 /private/frame.png\n\
                  ACK-CHALLENGE 00112233445566778899aabbccddeeff\n",
            )
            .unwrap();
        server.flush().unwrap();
        let mut ack = String::new();
        BufReader::new(&server).read_line(&mut ack).unwrap();
        assert_eq!(ack, "ACK 00112233445566778899aabbccddeeff\n");
        acked_tx.send(()).unwrap();
    });

    let mut reader = BufReader::new(&client);
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let status = status.trim_end();
    let output = receive_guarded_artifact_reply(
        &client,
        &mut reader,
        "video",
        "video frames count=1",
        status,
        "1",
    )
    .unwrap();
    acked_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("wire ACK must complete before the caller can block on stdout");
    match output {
        GuardedArtifactOutput::Lines(lines) => {
            assert_eq!(lines, ["frame n=1 /private/frame.png"]);
        }
        GuardedArtifactOutput::Status => panic!("video frames must retain its payload"),
    }
    peer.join().unwrap();
}

/// The socket is line-delimited, so an argument with an embedded `\n` would
/// frame a second authenticated verb; framing rejects any line terminator.
#[test]
fn validate_request_parts_rejects_embedded_line_terminators() {
    let injected = vec!["send".to_string(), "hi\nAUTH deadbeef".to_string()];
    let err = validate_request_parts(&injected).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    // A bare CR is rejected too (CRLF-splitting servers).
    assert!(validate_request_parts(&["a\rb".to_string()]).is_err());
    // Clean free-form arguments (spaces included) still frame fine.
    let clean = vec!["send".to_string(), "hello world".to_string()];
    assert!(validate_request_parts(&clean).is_ok());
}

/// The exact line the client prints for a pattern element the join will
/// split — `aterm-ctl: ` is [`stderr_line`]'s prefix, the rest is this.
fn split_note(arg: &str) -> String {
    format!(
        "note: '{arg}' contains whitespace and is split on the wire; \
         write the pattern as ONE token (e.g. esc.to.interrupt)"
    )
}

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// `await match|gone <re>`: a regex element with whitespace is named
/// wherever the timeout sits (the server peels `timeout=<ms>` and `timeout
/// <ms>` anywhere); quiet for a one-token regex, other predicates and a
/// missing pattern.
#[test]
fn split_pattern_note_names_an_await_regex_with_whitespace() {
    assert_eq!(
        split_pattern_note(&argv(&["await", "gone", "esc to interrupt"])),
        Some(split_note("esc to interrupt"))
    );
    assert_eq!(
        split_pattern_note(&argv(&[
            "await",
            "match",
            "BUILD SUCCESSFUL",
            "rows",
            "0",
            "5"
        ])),
        Some(split_note("BUILD SUCCESSFUL"))
    );
    // The timeout peel is mirrored: both spellings, before or after the regex.
    assert_eq!(
        split_pattern_note(&argv(&["await", "timeout=5000", "gone", "a b"])),
        Some(split_note("a b"))
    );
    assert_eq!(
        split_pattern_note(&argv(&["await", "gone", "a b", "timeout", "5000"])),
        Some(split_note("a b"))
    );
    // A tab is whitespace on the wire exactly like a space.
    assert_eq!(
        split_pattern_note(&argv(&["await", "match", "a\tb"])),
        Some(split_note("a\tb"))
    );
    // One token: the documented spelling earns no note.
    assert_eq!(
        split_pattern_note(&argv(&[
            "await",
            "gone",
            "esc.to.interrupt",
            "timeout=600000"
        ])),
        None
    );
    for quiet in [
        &["await", "idle", "500"][..],
        &["await", "seq"][..],
        &["await", "block"][..],
        &["await", "inbox", "since=0"][..],
        &["await", "match"][..],
        &["await"][..],
    ] {
        assert_eq!(split_pattern_note(&argv(quiet)), None, "{quiet:?}");
    }
}

/// `turn settle=match:<re>|gone:<re>`: the server's option parser stops at
/// the first token without `=`, so a split pattern's tail would be typed.
/// Only a leading option counts; the turn's text and anything after `--`
/// are text.
#[test]
fn split_pattern_note_names_a_turn_settle_pattern_with_whitespace() {
    assert_eq!(
        split_pattern_note(&argv(&["turn", "settle=match:BUILD SUCCESSFUL", "make"])),
        Some(split_note("settle=match:BUILD SUCCESSFUL"))
    );
    assert_eq!(
        split_pattern_note(&argv(&[
            "turn",
            "settle=gone:esc to interrupt",
            "timeout=600000",
            "fix the test",
        ])),
        Some(split_note("settle=gone:esc to interrupt"))
    );
    // Still leading after the exactly-once key and other options.
    assert_eq!(
        split_pattern_note(&argv(&[
            "turn",
            "id=1:me:7",
            "idle=500",
            "settle=gone:a b",
            "hello",
        ])),
        Some(split_note("settle=gone:a b"))
    );
    // The text is the payload; whitespace there is the point of a turn.
    assert_eq!(
        split_pattern_note(&argv(&[
            "turn",
            "settle=match:BUILD.SUCCESSFUL",
            "hello world"
        ])),
        None
    );
    // Not leading: after the text, or after `--`, it is text.
    assert_eq!(
        split_pattern_note(&argv(&["turn", "fix it", "settle=match:a b"])),
        None
    );
    assert_eq!(
        split_pattern_note(&argv(&["turn", "--", "settle=match:a b"])),
        None
    );
    // Another option with whitespace is not a pattern.
    assert_eq!(
        split_pattern_note(&argv(&["turn", "submit=none x", "hi"])),
        None
    );
}

/// `key`/`send` with a leading `if=<re>` guard: the same rule and note; a
/// guard after the payload is payload, and `send`'s free-form text keeps
/// its whitespace unremarked.
#[test]
fn split_pattern_note_names_a_key_or_send_if_guard_with_whitespace() {
    assert_eq!(
        split_pattern_note(&argv(&["key", "if=esc to interrupt", "enter"])),
        Some(split_note("if=esc to interrupt"))
    );
    assert_eq!(
        split_pattern_note(&argv(&["send", "id=1:me:2", "if=a b", "hi"])),
        Some(split_note("if=a b"))
    );
    assert_eq!(
        split_pattern_note(&argv(&["key", "if=quit.now", "enter"])),
        None
    );
    assert_eq!(split_pattern_note(&argv(&["send", "hello world"])), None);
    assert_eq!(split_pattern_note(&argv(&["send", "hi", "if=a b"])), None);
    assert_eq!(split_pattern_note(&argv(&["key", "enter"])), None);
}

/// The verb is found where [`forwarded_verb`] finds it (behind a proxy
/// selector and behind `dial <name> [@<sid>]`); every other verb, and an
/// empty request, gets no note.
#[test]
fn split_pattern_note_follows_the_forwarded_verb_and_is_quiet_elsewhere() {
    assert_eq!(
        split_pattern_note(&argv(&["@s-1a2b", "await", "gone", "a b"])),
        Some(split_note("a b"))
    );
    assert_eq!(
        split_pattern_note(&argv(&["dial", "host", "await", "gone", "a b"])),
        Some(split_note("a b"))
    );
    assert_eq!(
        split_pattern_note(&argv(&[
            "dial",
            "host",
            "@s-r",
            "turn",
            "settle=gone:a b",
            "x"
        ])),
        Some(split_note("settle=gone:a b"))
    );
    for quiet in [
        &["search", "a b"][..],
        &["text", "trim"][..],
        &["help", "await"][..],
        &["@s-1"][..],
        &["dial", "host"][..],
        &[][..],
    ] {
        assert_eq!(split_pattern_note(&argv(quiet)), None, "{quiet:?}");
    }
}

#[test]
fn resolve_refuses_both_sock_and_pid() {
    let err = resolve_path(Some("/tmp/a.sock".into()), Some(7), None).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn resolve_pid_targets_that_instance_socket() {
    let path = resolve_path(None, Some(42), None).expect("per-user dir");
    // Platform-native separator: '/' on Unix, '\' on Windows.
    let want = format!("{}aterm-42.sock", std::path::MAIN_SEPARATOR);
    assert!(path.ends_with(&want), "got {path}");
}

#[test]
fn resolve_sock_flag_passes_straight_through() {
    let path = resolve_path(Some("/tmp/a.sock".into()), None, None).unwrap();
    assert_eq!(path, "/tmp/a.sock");
}

/// Every flag pins the socket, so a dead one is never answered with another
/// instance to drive. (The flagless arm is not exercised: it would dial the
/// developer's real `latest` alias.)
#[test]
fn resolve_target_pins_every_named_socket() {
    let (path, origin) = resolve_target(None, Some(42), None).expect("per-user dir");
    let want = format!("{}aterm-42.sock", std::path::MAIN_SEPARATOR);
    assert!(path.ends_with(&want), "got {path}");
    assert_eq!(origin, TargetOrigin::Pinned);
    assert_eq!(
        resolve_target(Some("/tmp/a.sock".into()), None, None).unwrap(),
        ("/tmp/a.sock".to_string(), TargetOrigin::Pinned)
    );
}

/// In-session self-location: with several instances running, a flagless
/// call from inside a session reaches the instance hosting that session
/// through its graph entry, not the `latest` owner. Malformed sids and
/// dangling entries fall back.
#[test]
fn self_location_prefers_own_instance_graph_entry() {
    // The graph parse is the shared engine helper (one on-disk format).
    assert_eq!(
        control_socket::graph_entry_sock("sock /d/aterm-7.sock\nnonce ab\n").as_deref(),
        Some("/d/aterm-7.sock")
    );
    assert_eq!(control_socket::graph_entry_sock("nonce ab\n"), None);
    assert_eq!(control_socket::graph_entry_sock("sock \n"), None);

    // self_instance_sock: sid-shape validation fails closed (no traversal).
    assert_eq!(self_instance_sock(None), None);
    assert_eq!(self_instance_sock(Some("nope")), None);
    assert_eq!(self_instance_sock(Some("s-")), None);
    assert_eq!(self_instance_sock(Some("s-../../etc/passwd")), None);
    assert_eq!(self_instance_sock(Some("s-XYZ")), None);
}

/// Every verb in the protocol table (`catalog_lines`) appears in `--help`,
/// and the hand-written prose sections survive.
#[test]
fn help_text_lists_every_verb_and_the_prose() {
    let help = help_text();
    // The synopsis is shared between --help and the no-verb usage error, so
    // both must surface the exact invocation shape.
    assert!(help.contains(SYNOPSIS), "help should embed the synopsis");
    // Every verb in the table appears — the generated catalog cannot drift.
    for spec in aterm_types::control_verbs::VERBS {
        assert!(
            help.contains(spec.name),
            "help must document the `{}` verb",
            spec.name
        );
    }
    // Every generated catalog line appears verbatim: the section is the
    // table's own rendering.
    assert!(
        aterm_types::control_verbs::catalog_lines().all(|line| help.contains(&line)),
        "every generated catalog line must appear in --help"
    );
    // The hand-written prose sections survive.
    assert!(
        !help.contains("$ATERM_CONTROL_SOCK"),
        "no environment selects a socket"
    );
    assert!(help.contains("aterm-<pid>.sock"));
    // The options and exit-codes prose.
    assert!(help.contains("--timeout"));
    assert!(help.contains("124"));
    // The stdin-payload forms are documented.
    assert!(help.contains("send --stdin"));
}

/// `read_bounded_line`'s success path is byte-identical to
/// `BufRead::read_line` (headers, CRLF body rows, the EOF-cut partial line,
/// std's invalid-UTF-8 error).
#[test]
fn read_bounded_line_matches_read_line_on_wellformed_input() {
    let data: &[u8] = b"OK 2 incomplete\r\nalpha\n\nbeta"; // header, body, blank, EOF-cut tail
    let mut bounded = io::Cursor::new(data);
    let mut plain = io::Cursor::new(data);
    loop {
        let (mut a, mut b) = (String::new(), String::new());
        let na = read_bounded_line(&mut bounded, &mut a).unwrap();
        let nb = plain.read_line(&mut b).unwrap();
        assert_eq!((na, &a), (nb, &b));
        if na == 0 {
            break;
        }
    }
    // Invalid UTF-8: same kind, same message, `line` left untouched.
    let mut out = String::new();
    let e = read_bounded_line(&mut io::Cursor::new(&b"\xffbad\n"[..]), &mut out).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert_eq!(e.to_string(), "stream did not contain valid UTF-8");
    assert!(out.is_empty());
}

/// The accumulation cap bounds memory against a server streaming bytes
/// without a newline (a read deadline never fires while bytes arrive).
#[test]
fn read_bounded_line_caps_newlineless_floods() {
    // A newline right at the cap still parses: content == MAX_LINE_BYTES.
    let mut ok_line = vec![b'x'; MAX_LINE_BYTES];
    ok_line.push(b'\n');
    let mut out = String::new();
    let n = read_bounded_line(&mut io::Cursor::new(&ok_line), &mut out).unwrap();
    assert_eq!(n, MAX_LINE_BYTES + 1);
    assert_eq!(out.len(), MAX_LINE_BYTES + 1);
    // One more byte with no newline is oversized, even when the cap slices a
    // multi-byte char.
    let ascii_flood = vec![b'x'; MAX_LINE_BYTES + 1];
    let multibyte_flood = "é".repeat(MAX_LINE_BYTES).into_bytes();
    for flood in [ascii_flood, multibyte_flood] {
        let mut out = String::new();
        let err = read_bounded_line(&mut io::Cursor::new(&flood), &mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("oversized reply"), "got {err}");
        assert!(out.is_empty(), "must not leak a partial line");
    }
}

/// Short, individually valid rows still consume one shared body allowance.
/// A small injected allowance exercises the boundary without allocating a
/// 256 MiB reply, and the next reply stays unread after an overrun.
#[test]
fn collected_reply_lines_bound_the_whole_body_and_preserve_framing() {
    let mut exact = io::Cursor::new(b"a\r\nb\nOK next\n");
    let rows = collect_reply_lines(2, 5, |line| read_bounded_line(&mut exact, line))
        .expect("the exact wire-byte allowance");
    assert_eq!(rows, ["a", "b"]);
    let mut next = String::new();
    read_bounded_line(&mut exact, &mut next).expect("the next reply");
    assert_eq!(next, "OK next\n");

    let mut over = io::Cursor::new(b"a\r\nb\nOK next\n");
    let error = collect_reply_lines(2, 4, |line| read_bounded_line(&mut over, line))
        .expect_err("two valid rows exceed the aggregate allowance");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("aggregate bound"));
    let mut next = String::new();
    read_bounded_line(&mut over, &mut next).expect("the next reply");
    assert_eq!(next, "OK next\n");

    let error = collect_reply_lines(MAX_STREAM_LINES + 1, usize::MAX, |_| {
        panic!("an oversized count must be refused before reading")
    })
    .expect_err("the collector also checks its count");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        read_reply_lines(&mut io::Cursor::new(b""), 0)
            .expect("an empty reply")
            .is_empty()
    );
    assert_eq!(
        read_reply_lines(&mut io::Cursor::new(b"only one\n"), 2)
            .expect_err("a truncated counted body")
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

/// The `exchange` deadline clears the blocking verbs' server-side 600 000 ms
/// clamp with enough margin for `update check`'s floor (30 s API + 600 s
/// download `--max-time`); its size-derived ceiling needs `--timeout` (see
/// [`EXCHANGE_DEADLINE`]).
#[test]
fn exchange_deadline_clears_the_server_blocking_clamp() {
    assert!(EXCHANGE_DEADLINE > std::time::Duration::from_millis(600_000));
    // update check's floor: 30 s + 600 s of curl, plus real margin for staging.
    assert!(EXCHANGE_DEADLINE >= std::time::Duration::from_secs(650 + 60));
}

/// `--timeout` parses seconds: `0` disables (`None`), a positive integer is
/// that many seconds, `900` equals [`EXCHANGE_DEADLINE`]; negative or
/// non-numeric is a usage error.
#[test]
fn parse_timeout_zero_disables_positive_is_seconds() {
    assert_eq!(parse_timeout("0").unwrap(), None);
    assert_eq!(
        parse_timeout("5").unwrap(),
        Some(std::time::Duration::from_secs(5))
    );
    assert_eq!(parse_timeout("900").unwrap(), Some(EXCHANGE_DEADLINE));
    for bad in ["", "-1", "abc", "1.5", "  "] {
        let e = parse_timeout(bad).unwrap_err();
        assert_eq!(
            e.kind(),
            io::ErrorKind::InvalidInput,
            "{bad:?} should error"
        );
    }
}

/// Exit 124 is reserved for a timeout: nonzero, but distinct from the
/// generic failure 1.
#[test]
fn timeout_exit_code_is_124_and_client_kinds_map_to_it() {
    assert_eq!(EXIT_TIMEOUT, 124);
    assert!(is_timeout_error(&io::Error::from(
        io::ErrorKind::WouldBlock
    )));
    assert!(is_timeout_error(&io::Error::from(io::ErrorKind::TimedOut)));
    assert!(!is_timeout_error(&io::Error::from(
        io::ErrorKind::ConnectionRefused
    )));
    assert!(!is_timeout_error(&io::Error::from(io::ErrorKind::NotFound)));
}

/// A deadline that fires once connected names the socket and the wait, keeps
/// its kind (exit 124) and says `timed out` (aterm-agent's `LOST` reads it):
/// before any answer ([`Unanswered`]) the server did not answer, after it the
/// reply stopped part-way. A successor's carried line, other kinds, and errors
/// with no deadline pass unchanged.
#[test]
fn a_deadline_once_connected_names_the_socket_and_the_wait() {
    let path = "/d/aterm-7.sock";
    let deadline = Some(std::time::Duration::from_secs(900));
    let want = "no reply from /d/aterm-7.sock within 900s (connected; the server did not \
                answer) — the request timed out";
    let part_way = "reply from /d/aterm-7.sock stopped part-way: nothing more within 900s \
                    (connected; the server began its answer, then went quiet) — the \
                    request timed out";
    for line in [want, part_way] {
        assert!(
            line.contains("timed out"),
            "the supervisor's LOST list reads it"
        );
    }
    for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut] {
        let marked = unanswered(io::Error::from(kind));
        assert_eq!(marked.kind(), kind, "the mark keeps the kind");
        assert_eq!(
            unanswered(marked).to_string(),
            io::Error::from(kind).to_string(),
            "marked once, with the OS's words until it is worded"
        );
        let e = no_reply_error(unanswered(io::Error::from(kind)), path, deadline);
        assert_eq!(e.to_string(), want, "{kind:?}");
        assert_eq!(e.kind(), kind, "the kind is kept");
        assert_eq!(exit_for(&e), ExitCode::from(EXIT_TIMEOUT));
        let again = no_reply_error(e, "/d/aterm-8.sock", deadline);
        assert_eq!(again.to_string(), want, "worded once");
        assert_eq!(
            worded(again).to_string(),
            want,
            "marking it again changes nothing"
        );
        let cut = no_reply_error(io::Error::from(kind), path, deadline);
        assert_eq!(cut.to_string(), part_way, "after the status line: {kind:?}");
        assert_eq!(cut.kind(), kind);
        assert_eq!(exit_for(&cut), ExitCode::from(EXIT_TIMEOUT));
        assert_eq!(
            no_reply_error(cut, "/d/aterm-8.sock", deadline).to_string(),
            part_way,
            "worded once"
        );
    }
    let hung_up = unanswered(io::Error::from(io::ErrorKind::ConnectionReset));
    assert!(!is_unanswered(&hung_up), "only a deadline is marked");
    let reset = io::Error::from(io::ErrorKind::ConnectionReset);
    let raw = reset.to_string();
    let passed = no_reply_error(reset, path, deadline);
    assert_eq!(
        passed.to_string(),
        raw,
        "a reset is the server's hang-up, not silence"
    );
    assert_eq!(passed.kind(), io::ErrorKind::ConnectionReset);
    let raw = io::Error::from(io::ErrorKind::WouldBlock).to_string();
    let unbounded = no_reply_error(io::Error::from(io::ErrorKind::WouldBlock), path, None);
    assert_eq!(unbounded.to_string(), raw, "no deadline set, none fired");
}

/// A full non-blocking stdout is not the server's silence: its would-block
/// is [`Worded`], keeping the OS's words (and 124) through
/// [`no_reply_error`].
#[test]
fn a_stdout_that_would_block_keeps_the_os_words() {
    struct Blocks;
    impl Write for Blocks {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let raw = io::Error::from(io::ErrorKind::WouldBlock).to_string();
    // A terminal's rendering marks the failure the same way as the pipe's
    // pass-through.
    for terminal in [false, true] {
        let blocked = StdoutSink::onto(Blocks, terminal)
            .write_all(b"x")
            .expect_err("the sink fails");
        assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(blocked.to_string(), raw);
        let passed = no_reply_error(
            blocked,
            "/d/aterm-7.sock",
            Some(std::time::Duration::from_secs(900)),
        );
        assert_eq!(passed.to_string(), raw, "never the server's silence");
        assert_eq!(exit_for(&passed), ExitCode::from(EXIT_TIMEOUT));
    }
}

/// The wait reads as whole seconds, a remainder to a tenth, or milliseconds
/// under a second (the post-hang-up retry runs on what is left, 1 ms floor).
#[test]
fn a_deadline_reads_in_seconds_or_what_was_left() {
    use std::time::Duration;
    assert_eq!(deadline_words(Duration::from_secs(900)), "900s");
    assert_eq!(deadline_words(Duration::from_secs(1)), "1s");
    assert_eq!(deadline_words(Duration::from_millis(12_540)), "12.5s");
    assert_eq!(deadline_words(Duration::from_millis(250)), "250ms");
    assert_eq!(deadline_words(Duration::from_micros(10)), "1ms");
}

/// End to end over a socket that takes the connection and never answers: a
/// verb line and a stdin payload each end in the named wait with exit 124.
/// Unix only: the stand-in instance is a `UnixListener`.
#[cfg(unix)]
#[test]
fn a_server_that_never_answers_is_a_named_wait_not_eagain() {
    use std::io::{BufRead, Read};
    use std::os::unix::net::UnixListener;

    // A private dir holding only the socket, so no token is read and the
    // request line is the first line the mock sees.
    let dir = std::env::temp_dir().join(format!("aterm-ctl-silent-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("silent.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    // Take each connection, read what it sends, answer nothing: the
    // connection ends when the client gives up and hangs up.
    let srv = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn);
            let mut first = String::new();
            r.read_line(&mut first).expect("request line");
            let mut rest = Vec::new();
            let _ = r.read_to_end(&mut rest);
            seen.push(first);
        }
        seen
    });
    let path = sock.to_str().expect("utf8 path").to_string();
    let deadline = Some(std::time::Duration::from_secs(1));
    let want = format!(
        "no reply from {path} within 1s (connected; the server did not answer) — the \
         request timed out"
    );
    let e = exchange(
        &path,
        TargetOrigin::Pinned,
        "text\n",
        "text",
        "text\n",
        deadline,
        false,
        &redial::Redial::none(),
    )
    .expect_err("nothing answers the verb line");
    assert_eq!(e.to_string(), want);
    assert_eq!(exit_for(&e), ExitCode::from(EXIT_TIMEOUT));
    let e = feed_bin_exchange(&path, TargetOrigin::Pinned, "feed-bin", b"x", deadline)
        .expect_err("nothing answers the payload");
    assert_eq!(e.to_string(), want);
    assert_eq!(exit_for(&e), ExitCode::from(EXIT_TIMEOUT));
    assert_eq!(srv.join().expect("server"), ["text\n", "feed-bin 1\n"]);
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// Behind `dial <name>` the local server only relays, so a deadline names
/// both the remote and the relay socket. Still exit 124.
#[cfg(unix)]
#[test]
fn a_silent_dial_names_the_remote_and_the_relay() {
    use std::io::{BufRead, Read};
    use std::os::unix::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("aterm-ctl-remote-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("relay.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn);
        let mut first = String::new();
        r.read_line(&mut first).expect("request line");
        let mut rest = Vec::new();
        let _ = r.read_to_end(&mut rest);
        first
    });
    let path = sock.to_str().expect("utf8 path").to_string();
    let e = exchange(
        &path,
        TargetOrigin::Pinned,
        "dial work text\n",
        "text",
        "text",
        Some(std::time::Duration::from_secs(1)),
        false,
        &redial::Redial::none(),
    )
    .expect_err("the remote never answers");
    assert_eq!(
        e.to_string(),
        format!(
            "no reply from the remote 'work' (through {path}) within 1s — the request timed out"
        )
    );
    assert_eq!(exit_for(&e), ExitCode::from(EXIT_TIMEOUT));
    assert_eq!(srv.join().expect("server"), "dial work text\n");
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// End to end over a socket that answers `OK 2` and one row, then goes
/// quiet: the deadline says the reply stopped part-way. Still exit 124.
/// `help <verb>` reads its whole body before printing, so nothing prints.
/// Unix only.
#[cfg(unix)]
#[test]
fn a_reply_that_stops_part_way_says_so() {
    use std::io::{BufRead, Read};
    use std::os::unix::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("aterm-ctl-partway-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("partway.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    // Read the request, answer its status line and the first of its two
    // rows, then say nothing more until the client gives up and hangs up.
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn);
        let mut first = String::new();
        r.read_line(&mut first).expect("request line");
        r.get_mut()
            .write_all(b"OK 2\nrow one\n")
            .expect("the partial reply");
        r.get_mut().flush().expect("flush");
        let mut rest = Vec::new();
        let _ = r.read_to_end(&mut rest);
        first
    });
    let path = sock.to_str().expect("utf8 path").to_string();
    let deadline = Some(std::time::Duration::from_secs(1));
    let e = help_with_fallback(
        &path,
        TargetOrigin::Pinned,
        "text",
        deadline,
        &LocalVerbs::NONE,
    )
    .expect_err("the second row never comes");
    assert_eq!(
        e.to_string(),
        format!(
            "reply from {path} stopped part-way: nothing more within 1s (connected; the \
             server began its answer, then went quiet) — the request timed out"
        )
    );
    assert_eq!(exit_for(&e), ExitCode::from(EXIT_TIMEOUT));
    assert_eq!(srv.join().expect("server"), "help text\n");
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&dir);
}

/// A server-reported timeout maps to 124: a bare `OK timeout` and a `turn`
/// verdict with `status=timeout`. A settled turn, a normal reply and any
/// `ERR` line are not timeouts.
#[test]
fn reply_is_timeout_detects_await_and_turn_forms() {
    assert!(reply_is_timeout("OK timeout"));
    assert!(reply_is_timeout(
        "OK 24 turn submitted=1 status=timeout seq=9"
    ));
    assert!(!reply_is_timeout(
        "OK 24 turn submitted=1 status=settled seq=9"
    ));
    assert!(!reply_is_timeout("OK ready prompt"));
    assert!(!reply_is_timeout("OK 12 bytes"));
    assert!(!reply_is_timeout("OK"));
    // An ERR/other status is never a timeout (stays generic-failure exit 1).
    assert!(!reply_is_timeout("ERR timeout"));
    assert!(!reply_is_timeout("timeout"));
}

/// `split_selector` peels a leading `@<selector>` off the request parts (so a
/// stdin frame can route to a peer), and leaves a flagless verb untouched.
#[test]
fn split_selector_peels_leading_at() {
    let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    let parts = p("@s-abc feed-bin");
    let (sel, rest) = split_selector(&parts);
    assert_eq!(sel, Some("@s-abc"));
    assert_eq!(rest, &["feed-bin".to_string()]);
    let parts = p("send --stdin");
    let (sel, rest) = split_selector(&parts);
    assert_eq!(sel, None);
    assert_eq!(rest, &["send".to_string(), "--stdin".to_string()]);
    assert_eq!(split_selector(&[]), (None, &[][..]));
}

/// The binary-frame head: bare `<verb>`, or `@<sel> <verb>`. `send
/// --stdin`/`feed-bin` use `feed-bin`; `paste --stdin` uses `paste-bin`.
/// The caller appends ` <len>\n`.
#[test]
fn binary_frame_prefix_prepends_selector_and_carries_verb() {
    assert_eq!(binary_frame_prefix(None, "feed-bin"), "feed-bin");
    assert_eq!(
        binary_frame_prefix(Some("@s-abc"), "feed-bin"),
        "@s-abc feed-bin"
    );
    assert_eq!(binary_frame_prefix(Some("@."), "feed-bin"), "@. feed-bin");
    // `paste --stdin` routes through the paste seam, not the raw feed.
    assert_eq!(binary_frame_prefix(None, "paste-bin"), "paste-bin");
    assert_eq!(
        binary_frame_prefix(Some("@s-abc"), "paste-bin"),
        "@s-abc paste-bin"
    );
}

/// `@self` / `@env` expand to `@<sid>` from `$ATERM_PARENT_SESSION_ID`
/// before framing; a missing or empty variable is an error (parts left
/// as-is); other selectors and flagless verbs pass through.
#[test]
fn expand_self_selector_expands_env_and_errors_when_unset() {
    let mut p = vec!["@self".to_string(), "text".to_string()];
    expand_self_selector(&mut p, Some("s-abc123".into())).unwrap();
    assert_eq!(p[0], "@s-abc123");
    // The `@env` alias behaves identically.
    let mut p = vec!["@env".to_string(), "send".to_string(), "hi".to_string()];
    expand_self_selector(&mut p, Some("s-ff".into())).unwrap();
    assert_eq!(p[0], "@s-ff");
    // Unset OR empty: a clear error naming the env var, parts untouched.
    for env in [None, Some(String::new())] {
        let mut p = vec!["@self".to_string(), "text".to_string()];
        let e = expand_self_selector(&mut p, env).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(
            e.to_string().contains("$ATERM_PARENT_SESSION_ID unset"),
            "clear message: {e}"
        );
        assert_eq!(p[0], "@self", "left untouched on error");
    }
    // `@<sid>`, `@.`, and a flagless verb are all untouched.
    for sel in ["@s-xyz", "@.", "text"] {
        let mut p = vec![sel.to_string(), "text".to_string()];
        expand_self_selector(&mut p, Some("s-abc".into())).unwrap();
        assert_eq!(p[0], sel);
    }
    // Empty request is a no-op (no panic on `first()`).
    expand_self_selector(&mut [], None).unwrap();

    // Selector second: `subscribe @self <streams>` expands parts[1].
    let mut p = vec![
        "subscribe".to_string(),
        "@self".to_string(),
        "screen,events".to_string(),
    ];
    expand_self_selector(&mut p, Some("s-live".into())).unwrap();
    assert_eq!(p[1], "@s-live", "subscribe selector-second is expanded");
    assert_eq!(p[2], "screen,events", "streams untouched");
    // A comma list expands each @self/@env element, leaving sids alone.
    let mut p = vec![
        "subscribe".to_string(),
        "@self,@1,@env".to_string(),
        "screen".to_string(),
    ];
    expand_self_selector(&mut p, Some("s-x".into())).unwrap();
    assert_eq!(
        p[1], "@s-x,@1,@s-x",
        "each self element in the list expands"
    );
    // `subscribe` with a concrete selector is untouched; unset env still errors
    // when the subscribe selector needs it.
    let mut p = vec![
        "subscribe".to_string(),
        "@self".to_string(),
        "screen".to_string(),
    ];
    assert_eq!(
        expand_self_selector(&mut p, None).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// The stdin-payload cap mirrors the server's 256 KiB `feed-bin` limit and
/// is enforced client-side.
#[test]
fn feed_bin_cap_matches_server_and_errors_are_static() {
    assert_eq!(MAX_FEED_BIN, 256 * 1024);
    assert_eq!(
        oversized_feed_bin_error().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(feed_bin_inline_error().kind(), io::ErrorKind::InvalidInput);
}

/// The hidden `--completions <shell>` generator: every supported shell
/// yields a non-empty script with its shell wiring, every protocol verb and
/// the client-only discovery verbs; an unknown shell is an error.
#[test]
fn completions_cover_every_verb_per_shell_and_reject_unknown() {
    for (shell, wiring) in [
        ("bash", "complete -F _aterm_ctl aterm-ctl"),
        ("zsh", "#compdef aterm-ctl"),
        ("fish", "complete -c aterm-ctl"),
    ] {
        let script = completion_script(shell).expect("known shell yields a script");
        assert!(!script.is_empty(), "{shell} script is non-empty");
        assert!(
            script.contains(wiring),
            "{shell} script has its shell wiring"
        );
        // Representative verbs.
        for verb in ["text", "turn", "subscribe"] {
            assert!(script.contains(verb), "{shell} completes `{verb}`");
        }
        // Exhaustive: every protocol verb appears — the completion list is the
        // table's own rendering (`completion_verb_list`), so it cannot drift.
        for spec in aterm_types::control_verbs::VERBS {
            assert!(
                script.contains(spec.name),
                "{shell} must complete the `{}` verb",
                spec.name
            );
        }
        // The client-answered verbs (absent from the protocol table), appended
        // together by `completion_verb_list`.
        assert!(
            script.contains("ls instances windows mux"),
            "{shell} completes the client verbs `ls`/`instances`/`windows`/`mux`"
        );
    }
    // An unrecognized shell name has no script and maps to a clear error.
    assert!(completion_script("powershell").is_none());
    let e = unknown_shell_error();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert!(e.to_string().contains("unknown shell"), "got {e}");
}

/// The front-door generator completes `aterm`, and its `aterm ctl` arm
/// renders the same ctl verb set as the sibling script (both from
/// `completion_verb_list`). The front-door verb/flag tables are pinned by
/// the `aterm` crate's tests.
#[test]
fn front_door_completions_complete_aterm_and_carry_the_ctl_surface() {
    let verbs = ["help", "ctl", "pkg", "fleet", "drive"];
    let flags = [
        ("--window", "open the GPU window"),
        ("--version", "print the version"),
    ];
    for (shell, wiring) in [
        ("bash", "complete -F _aterm aterm\n"),
        ("zsh", "#compdef aterm\n"),
        ("fish", "complete -c aterm -f\n"),
    ] {
        let script = front_door_completion_script(shell, &verbs, &flags)
            .expect("known shell yields a script");
        assert!(script.contains(wiring), "{shell} wires `aterm`: {script}");
        for verb in verbs {
            assert!(script.contains(verb), "{shell} completes `{verb}`");
        }
        for (flag, _) in flags {
            // fish names long flags dash-less (`-l window`).
            let probe = if shell == "fish" {
                flag.trim_start_matches('-')
            } else {
                flag
            };
            assert!(script.contains(probe), "{shell} completes `{flag}`");
        }
        // The `aterm ctl` arm renders the ctl surface: every protocol verb,
        // the client-only discovery verbs, and the ctl flags.
        for spec in aterm_types::control_verbs::VERBS {
            assert!(
                script.contains(spec.name),
                "{shell} must complete `aterm ctl {}`",
                spec.name
            );
        }
        assert!(
            script.contains("ls instances"),
            "{shell} has the client verbs"
        );
        let sock = if shell == "fish" { "-l sock" } else { "--sock" };
        assert!(script.contains(sock), "{shell} has the ctl flags");
    }
    // Contract (install.sh): the zsh script's first line is `#compdef aterm`.
    let zsh = front_door_completion_script("zsh", &verbs, &flags).expect("zsh");
    assert_eq!(zsh.lines().next(), Some("#compdef aterm"));
    // An unknown shell has no script, exactly like the sibling generator —
    // and the front door says so without the protocol's `ERR` token.
    assert!(front_door_completion_script("powershell", &verbs, &flags).is_none());
    let e = front_door_completions_result(Some("tcsh"), &verbs, &flags)
        .expect_err("an unknown shell is refused");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        e.to_string(),
        "--completions: unknown shell (bash, zsh, or fish)"
    );
}

/// `aterm-uds` keeps a std-only mirror of the token-filename rule
/// (`aterm-agent`'s `--dial` path reads tokens through it without depending
/// on `aterm-types`). This crate depends on both, so it pins their
/// agreement for the name the server writes and the ordered list a client
/// reads.
#[test]
fn uds_token_name_mirror_matches_aterm_types() {
    let cases = [
        // per-instance sockets -> per-instance tokens
        "aterm-1.sock",
        "aterm-42.sock",
        "aterm-4294967295.sock",
        // the `latest` alias and every explicit shape -> a token named
        // after that socket alone
        "aterm.sock",
        "c.sock",
        "control.sock",
        "aterm-.sock",
        "aterm-abc.sock",
        "aterm-1x.sock",
        // pid round-trips through u32: leading zeros normalize, and a pid past
        // u32::MAX falls through to the explicit form.
        "aterm-01.sock",
        "aterm-007.sock",
        "aterm-99999999999.sock",
        "aterm-+1.sock",
        "aterm-1.socket",
        "aterm-1",
        "",
        // Names that would append onto a reserved token name, and the `.token`
        // spelling; both mirrors key on `.sock` alone.
        "aterm",
        "aterm-4242",
        "aterm-42.token",
        "aterm.token",
        "ctl",
        "a.b.c.sock",
        "\u{65e5}\u{672c}.sock",
    ];
    for name in cases {
        assert_eq!(
            aterm_uds::latest::token_name_for_sock(name),
            control_socket::token_name_for_sock(name),
            "token-name mirror drifted for {name:?}"
        );
        assert_eq!(
            aterm_uds::latest::token_names_for_sock(name),
            control_socket::token_names_for_sock(name),
            "token-name READ ORDER mirror drifted for {name:?}"
        );
        // Neither mirror hands two sockets one token file: explicit names stay
        // out of the reserved `aterm.token` / `aterm-<pid>.token` space.
        let token = control_socket::token_name_for_sock(name);
        if control_socket::instance_pid(name).is_none() || !name.ends_with(".sock") {
            assert_ne!(
                token,
                control_socket::SIBLING_TOKEN_FILE,
                "{name:?} must not derive the legacy shared token"
            );
            assert_eq!(
                control_socket::instance_pid(&token),
                None,
                "{name:?} must not derive a per-instance token name"
            );
        }
    }
}

/// Two explicit sockets in one directory read two different tokens, so a
/// second instance cannot lock out the first one's clients.
#[test]
fn two_explicit_sockets_in_one_directory_read_their_own_tokens() {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-tok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test dir");
    let a = dir.join("a.sock").to_string_lossy().into_owned();
    let b = dir.join("b.sock").to_string_lossy().into_owned();
    std::fs::write(dir.join("a.sock.token"), "aaaa\n").expect("token a");
    std::fs::write(dir.join("b.sock.token"), "bbbb\n").expect("token b");
    assert_eq!(read_token_for(&a).as_deref(), Some("aaaa"));
    assert_eq!(read_token_for(&b).as_deref(), Some("bbbb"));

    // A stale shared `aterm.token` from an older build cannot displace
    // either: the per-socket file exists, so the fallback is never reached.
    std::fs::write(dir.join("aterm.token"), "cccc\n").expect("legacy token");
    assert_eq!(read_token_for(&a).as_deref(), Some("aaaa"));
    assert_eq!(read_token_for(&b).as_deref(), Some("bbbb"));

    // Compatibility: an instance that wrote only the shared name is still
    // reachable. The fallback fires exactly when the per-socket file is
    // absent, resolving to the same path as the dependency-free mirror.
    let c = dir.join("c.sock").to_string_lossy().into_owned();
    assert_eq!(read_token_for(&c).as_deref(), Some("cccc"));
    assert_eq!(
        aterm_uds::latest::token_path_for_sock(&c).expect("resolves"),
        dir.join("aterm.token")
    );

    // A per-instance socket never falls back onto the shared file.
    let inst = dir.join("aterm-4242.sock").to_string_lossy().into_owned();
    let (miss, err) = read_token_at(&inst).expect_err("no per-instance token");
    assert_eq!(miss, dir.join("aterm-4242.token"));
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(read_token_for(&inst).is_none());

    // An empty per-socket token fails right there: it belongs to the
    // instance being dialed, and the shared file may be anyone's.
    std::fs::write(dir.join("a.sock.token"), "   \n").expect("empty token");
    let (path, err) = read_token_at(&a).expect_err("an empty token is a miss");
    assert_eq!(path, dir.join("a.sock.token"));
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `--sock` scopes the client-answered `ls`/`instances` to exactly one
/// target, so a caller with an isolated instance never sees the user's
/// terminals.
#[test]
fn discovery_scopes_to_an_explicit_socket() {
    let t = discovery_targets(Some("/tmp/private-run/c.sock"), None)
        .expect("an explicit socket is always a valid target");
    assert_eq!(
        t,
        vec![(0, "/tmp/private-run/c.sock".to_string())],
        "exactly the addressed socket, and NEVER the ambient fleet"
    );
}

/// An explicit socket whose filename encodes a pid reports it; a custom name
/// falls back to the `0` placeholder.
#[test]
fn discovery_reads_the_pid_from_an_instance_socket_name() {
    let named = discovery_targets(Some("/tmp/run/aterm-4242.sock"), None).expect("valid");
    assert_eq!(named, vec![(4242, "/tmp/run/aterm-4242.sock".to_string())]);

    let custom = discovery_targets(Some("/tmp/run/weird.sock"), None).expect("valid");
    assert_eq!(custom[0].0, 0, "no pid in the name -> 0 placeholder");

    // Leading zeros normalize through u32, matching `instance_pid`.
    let zeros = discovery_targets(Some("/tmp/run/aterm-007.sock"), None).expect("valid");
    assert_eq!(zeros[0].0, 7);
}

/// `--pid` for an instance that is not live is an error, never a widening to
/// the fleet.
#[test]
fn discovery_pid_miss_is_an_error_not_a_fleet_listing() {
    // Not 0: a graph entry with no `pid` line enumerates as pid 0. Where the
    // socket directory is missing or unreadable (a scratch HOME), the error
    // names the directory instead; either way, no widening.
    let err = discovery_targets(None, Some(DEAD_PID));
    match err {
        Err((msg, _)) => assert!(
            msg.contains(&format!("no live aterm instance with pid {DEAD_PID}"))
                || msg.starts_with("control-socket directory")
                || msg.starts_with("cannot resolve the control-socket directory"),
            "actionable message, got {msg:?}"
        ),
        Ok(list) => assert!(
            list.is_empty(),
            "a --pid miss must never return other instances, got {list:?}"
        ),
    }
}

/// `--pid` under a directory that could not be read names the directory's
/// cause, never "no live aterm instance with pid": it did not look. The
/// control: a readable directory without that pid keeps the pid claim, and
/// one holding it returns it.
#[test]
fn discovery_pid_under_an_unreadable_directory_names_the_directory() {
    let unreadable = DirOutcome::Unreadable {
        path: PathBuf::from(FLEET),
        err: eperm(),
    };
    let (msg, code) = pid_targets(4242, (unreadable, Vec::new())).expect_err("could not look");
    assert_eq!(code, EXIT_UNREACHABLE, "the code unscoped discovery gives");
    assert!(
        msg.starts_with(&format!(
            "control-socket directory {FLEET} exists but cannot be read"
        )),
        "{msg}"
    );
    assert!(!msg.contains("no live aterm instance"), "{msg}");
    let missing = DirOutcome::Missing {
        path: PathBuf::from(FLEET),
        why: "resolved from $HOME".to_string(),
    };
    let (msg, _) = pid_targets(4242, (missing, Vec::new())).expect_err("could not look");
    assert!(msg.contains("does not exist"), "{msg}");
    let (msg, _) = pid_targets(4242, (DirOutcome::Unresolvable, Vec::new())).expect_err("no dir");
    assert!(
        msg.starts_with("cannot resolve the control-socket directory"),
        "{msg}"
    );

    let (msg, code) = pid_targets(4242, (DirOutcome::Empty(PathBuf::from(FLEET)), Vec::new()))
        .expect_err("readable, no such pid");
    assert_eq!(code, 1);
    assert!(
        msg.starts_with("no live aterm instance with pid 4242"),
        "{msg}"
    );
    let sock = fleet_sock(4242);
    let found = DirOutcome::Found {
        path: PathBuf::from(FLEET),
        n: 2,
    };
    let listed = vec![
        Target::named(7, fleet_sock(7)),
        Target::named(4242, sock.clone()),
    ];
    assert_eq!(pid_targets(4242, (found, listed)), Ok(vec![(4242, sock)]));
}

// Discovery never says "empty" when it means "could not look": each row below
// is a condition under which a live instance could not be listed.

/// A pid no host hands out (above every pid_max), so `pid_alive` is false.
const DEAD_PID: u32 = 0x7fff_fff0;

/// The fleet-wide claim, licensed by exactly one condition.
const FLEET_CLAIM: &str = "no live aterm instances found";

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// An option this client does not have, typed before the verb, is refused by
/// the client rather than forwarded as the verb.
#[test]
fn an_unknown_option_before_the_verb_is_refused_by_the_client() {
    for argv in [&["--frob", "status"][..], &["-x", "status"][..]] {
        let err = real_main(
            strings(argv).into_iter().map(Into::into).collect(),
            LocalVerbs::NONE,
        )
        .expect_err("an unknown option is a usage error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let want = format!("unknown option {} (aterm ctl --help lists them)", argv[0]);
        assert_eq!(err.to_string(), want);
    }
}

/// `--pid 0` names no process: refused at parse time for every verb, never
/// dialed as `aterm-0.sock` nor matched against an entry with no pid.
#[test]
fn pid_zero_is_refused_before_any_verb_runs() {
    for argv in [&["--pid", "0", "ls"][..], &["--pid=0", "status"][..]] {
        let err = real_main(
            strings(argv).into_iter().map(Into::into).collect(),
            LocalVerbs::NONE,
        )
        .expect_err("--pid 0 is a usage error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            "--pid 0 names no process; address that instance with --sock <path> (`aterm ctl \
             instances` lists the sockets)"
        );
    }
    assert_eq!(parse_pid("42").expect("a pid"), 42);
}

/// `aterm ctl help --full | head`: the reader closing stdout is not our
/// failure: no line, status 0. Every other write failure, and a broken pipe on
/// the socket, keep their line and their exit.
#[test]
fn a_reader_closing_stdout_ends_the_client_quietly_with_status_0() {
    let closed = stdout_closed(io::Error::from(io::ErrorKind::BrokenPipe));
    assert!(is_stdout_closed(&closed));
    assert_eq!(
        format!("{:?}", exit_for(&closed)),
        format!("{:?}", ExitCode::SUCCESS)
    );
    let full = stdout_closed(io::Error::from(io::ErrorKind::StorageFull));
    assert!(!is_stdout_closed(&full));
    assert_eq!(
        format!("{:?}", exit_for(&full)),
        format!("{:?}", ExitCode::FAILURE)
    );
    // A socket-side broken pipe is never marked: the server hung up, which is news.
    let socket = io::Error::from(io::ErrorKind::BrokenPipe);
    assert!(!is_stdout_closed(&socket));
    // Through the sink, a broken pipe comes back marked; other errors come back as they were.
    struct Fails(io::ErrorKind);
    impl Write for Fails {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for terminal in [false, true] {
        let err = StdoutSink::onto(Fails(io::ErrorKind::BrokenPipe), terminal)
            .write_all(b"x")
            .expect_err("the sink fails");
        assert!(is_stdout_closed(&err));
        let err = StdoutSink::onto(Fails(io::ErrorKind::StorageFull), terminal)
            .write_all(b"x")
            .expect_err("the sink fails");
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    }
}

/// Every stdout writer goes through [`StdoutSink`], and every entry that runs
/// one ends a closed stdout with status 0 (a bare `stdout.lock()` would print
/// `Broken pipe` and exit 1).
#[test]
fn every_stdout_writer_goes_through_the_sink() {
    let shipped = |src: &'static str| {
        src.split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("the shipped half")
    };
    let files = [
        ("lib.rs", shipped(include_str!("lib.rs"))),
        ("conn.rs", shipped(include_str!("conn.rs"))),
        ("drift.rs", shipped(include_str!("drift.rs"))),
    ];
    let mut writers = 0;
    for (file, src) in files {
        for (n, line) in src.lines().enumerate() {
            if line.contains("stdout.lock()") || line.contains("stdout().lock()") {
                assert!(
                    line.contains("StdoutSink::new("),
                    "{file}:{}: a stdout writer outside the sink: {line}",
                    n + 1
                );
                writers += 1;
            }
        }
    }
    assert!(writers >= 11, "the scan found {writers} stdout writers");
    for (file, entry) in [
        // `main_entry` is `main_entry_with` with no local verbs; the latter
        // owns the error handling.
        (files[0].1, "pub fn main_entry_with("),
        (files[0].1, "pub fn front_door_completions_entry("),
        (files[1].1, "pub fn conn_main_entry("),
    ] {
        let body = file
            .split(entry)
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap_or_else(|| panic!("{entry} is gone"));
        assert!(
            body.contains("is_stdout_closed(&e)"),
            "{entry} reports a closed stdout as a failure"
        );
    }
}

/// `help <client verb>` is answered from the CLIENT VERBS block, framed like
/// the server's own `help <verb>`: `OK <n>` then exactly the entry's rows —
/// the verb line and its continuation lines, nothing from the neighbouring
/// entry or the prose paragraph, each row present verbatim in `--help`.
#[test]
fn help_for_a_client_verb_is_answered_from_the_client_block() {
    let help = help_text();
    for verb in CLIENT_HELP_VERBS {
        let reply = client_help_reply(&strings(&["help", verb]), HelpAsked::ByVerb)
            .unwrap_or_else(|| panic!("help {verb} is client-answered"));
        assert_eq!(
            client_help_reply(&strings(&["help", verb]), HelpAsked::ByFlag).as_ref(),
            Some(&reply),
            "{verb}: `{verb} --help` and `help {verb}` read the same block"
        );
        let mut lines = reply.lines();
        let header = lines.next().expect("framed");
        let rows: Vec<&str> = lines.collect();
        assert_eq!(header, format!("OK {}", rows.len()), "{verb}: {reply}");
        assert!(rows.len() > 1, "{verb}: an entry has continuation rows");
        assert!(
            rows[0].starts_with(&format!("{verb} ")),
            "{verb}: the first row is the verb line: {}",
            rows[0]
        );
        for row in &rows[1..] {
            assert!(
                row.starts_with("              "),
                "{verb}: continuation rows keep the gutter: {row}"
            );
            assert!(
                !row.trim_start().starts_with("None of the three"),
                "{verb}: the prose paragraph is not part of an entry"
            );
        }
        for row in &rows {
            assert!(
                help.contains(&format!("    {row}\n")),
                "{verb}: every row is a `--help` line verbatim: {row}"
            );
        }
    }
    // The `windows` entry names its row shape and what it could not say.
    let windows = client_help_reply(&strings(&["help", "windows"]), HelpAsked::ByVerb).unwrap();
    assert!(
        windows.contains("<pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]"),
        "{windows}"
    );
    assert!(windows.contains("could not say"), "{windows}");
    let ls = client_help_reply(&strings(&["help", "ls"]), HelpAsked::ByVerb).unwrap();
    assert!(ls.contains("detail=<pct|->"), "{ls}");
    // The identity column sits before `path=`; the agent columns, the
    // supervisor's and Autopilot's, the owner's and the harness's last two
    // follow, up to the self mark — the order the roster prints them.
    assert!(
        ls.contains("identity=<name|-> path=<frozen|live> program=<name|->"),
        "{ls}"
    );
    let order = [
        "supervisor=<holder|->",
        "autopilot=<off|cruise|land|->",
        "path_evidence=",
        "upgrade=<-|<state>/<to>/<why>/<age>>",
        "land_at=<unix-s|-> task=<open|landed|done|spent|theirs|->[ *]",
    ];
    let at: Vec<Option<usize>> = order.iter().map(|field| ls.find(field)).collect();
    assert!(
        at.iter().all(Option::is_some) && at.windows(2).all(|pair| pair[0] < pair[1]),
        "{order:?} at {at:?}: {ls}"
    );
    assert!(ls.contains("`spawn identity=<name>`"), "{ls}");
    assert!(ls.contains("frozen = a foreign copy (a"), "{ls}");
    assert!(!ls.contains("instances     one line per"), "{ls}");
}

/// Everything but `help <client verb>` and `<verb> --help` reaches the
/// server: an explicit `help <protocol verb>` (its `cmd_help` describes its
/// own build), a bare `help`, `help --full`, `help --json`, an unknown name,
/// a trailing argument (`ERR usage`), and a selector form.
#[test]
fn help_for_an_unknown_or_catalog_form_still_reaches_the_server() {
    for parts in [
        vec!["help", "text"],
        vec!["help", "sessions"],
        vec!["help"],
        vec!["help", "--full"],
        vec!["help", "--json"],
        vec!["help", "nonesuch"],
        vec!["help", "ls", "extra"],
        vec!["@s-abc", "help", "ls"],
        vec!["ls"],
        vec!["text", "ls"],
    ] {
        assert_eq!(
            client_help_reply(&strings(&parts), HelpAsked::ByVerb),
            None,
            "{parts:?} is the server's to answer"
        );
    }
    // Asked by the flag, an unknown name still goes to the server.
    assert_eq!(
        client_help_reply(&strings(&["help", "nonesuch"]), HelpAsked::ByFlag),
        None
    );
    // Every client verb the block documents is one the table lacks, while
    // `help` itself is a table verb framed as lines, the shape the client
    // reply copies.
    for verb in CLIENT_HELP_VERBS {
        assert!(
            aterm_types::control_verbs::spec(verb).is_none(),
            "{verb} must stay out of the protocol table"
        );
    }
    assert_eq!(
        aterm_types::control_verbs::framing_of("help", "help ls"),
        aterm_types::control_verbs::Framing::Lines
    );
}

/// `<verb> --help` for every protocol verb is answered from the linked table
/// with no socket, framed as `aterm-gui`'s `cmd_help` frames `help <verb>`
/// (`OK <n>` + the entry's rows).
#[test]
fn help_for_every_protocol_verb_is_answered_from_the_shared_table() {
    for verb in aterm_types::control_verbs::VERBS {
        let reply = client_help_reply(&strings(&["help", verb.name]), HelpAsked::ByFlag)
            .unwrap_or_else(|| panic!("help {} is client-answered", verb.name));
        let rows = verb.entry_lines();
        let mut expected = format!("OK {}\n", rows.len());
        for row in &rows {
            expected.push_str(row);
            expected.push('\n');
        }
        assert_eq!(
            reply, expected,
            "{}: framed exactly as the server's",
            verb.name
        );
        assert!(
            reply
                .lines()
                .nth(1)
                .is_some_and(|r| r.starts_with(verb.name)),
            "{}: the first row names the verb",
            verb.name
        );
    }
}

/// An undocumented verb has no entry, and an entry is a verb line plus
/// gutter rows only; prose at the 4-column indent is never taken for a verb.
#[test]
fn client_verb_entry_reads_only_documented_verbs() {
    assert_eq!(client_verb_entry("text"), None);
    assert_eq!(client_verb_entry("None"), None);
    assert_eq!(client_verb_entry("l"), None, "a prefix is not a verb");
    let ls = client_verb_entry("ls").expect("documented");
    assert!(
        ls[0].starts_with("ls            every session"),
        "{}",
        ls[0]
    );
}

/// A denied connect is named as the sandbox's refusal and nothing more: no
/// directory, no flag, no escalation — the sandbox is not worked around.
#[test]
fn a_denied_connect_is_named_and_not_worked_around() {
    let probes = vec![
        (1, fleet_sock(1), Probe::Denied(eperm())),
        (
            3,
            "/private/tmp/agent/aterm-3.sock".to_string(),
            Probe::Denied(eperm()),
        ),
        (
            4,
            "/elsewhere/aterm-4.sock".to_string(),
            Probe::Refused {
                pid_running: Some(false),
            },
        ),
    ];
    let hint = dominant_hint(&probes, None).expect("denied dominates");
    assert_eq!(
        hint,
        "the control socket is refused by this sandbox; aterm drives such a session from \
         outside and it takes no part in messaging — nothing to configure"
    );
    for forbidden in [
        "--allow-unix-socket",
        "escalat",
        "/private/tmp/agent",
        FLEET,
    ] {
        assert!(!hint.contains(forbidden), "{forbidden}: {hint}");
    }
    let scoped = vec![(
        6,
        "/tmp/run/aterm-6.sock".to_string(),
        Probe::Denied(eacces()),
    )];
    assert_eq!(dominant_hint(&scoped, Some(0)), Some(hint));
}

/// The foreign hint speaks of the `foreign` rows only: dominant over a stale
/// socket listed beside them, it never calls that row a private instance.
#[test]
fn the_foreign_hint_names_only_the_foreign_rows() {
    let foreign = |n: u32| {
        (
            n,
            format!("/private/x/{n}.sock"),
            Probe::Foreign {
                token: PathBuf::from(format!("/private/x/{n}.sock.token")),
                facts: ForeignFacts::Unknown,
            },
        )
    };
    let probes = vec![
        foreign(1),
        foreign(2),
        (
            3,
            "/elsewhere/aterm-3.sock".to_string(),
            Probe::Refused {
                pid_running: Some(false),
            },
        ),
    ];
    assert_eq!(
        dominant_hint(&probes, None).expect("foreign dominates"),
        "only whoever started a `foreign` instance can drive it"
    );
    // An orphan — launchd its parent, nobody left holding its token — is
    // offered the one action that ends it, named by its pid.
    let mut with_orphan = probes;
    with_orphan[1].2 = Probe::Foreign {
        token: PathBuf::from("/private/x/2.sock.token"),
        facts: ForeignFacts::Writer {
            orphaned: true,
            binary_deleted: false,
            up: None,
        },
    };
    assert_eq!(
        dominant_hint(&with_orphan, None).expect("foreign dominates"),
        "only whoever started a `foreign` instance can drive it; `kill 2` ends it once \
         nothing in it is yours to keep"
    );
}

/// The host's own "denied" errors, built from raw OS codes so each host runs
/// the real `from_raw_os_error` decode. Unix: `EPERM` (1, the macOS seatbelt's
/// answer) and `EACCES` (13). Windows: `ERROR_ACCESS_DENIED` (5) and
/// `WSAEACCES` (10013). Expectations quote the error's text, since strerror
/// wording varies.
fn eperm() -> io::Error {
    io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 1 })
}

fn eacces() -> io::Error {
    io::Error::from_raw_os_error(if cfg!(windows) { 10013 } else { 13 })
}

/// A directory read that is no denial: `ENOTDIR` (20; Windows
/// `ERROR_DIRECTORY`, 267), a file where the directory should be.
fn enotdir() -> io::Error {
    io::Error::from_raw_os_error(if cfg!(windows) { 267 } else { 20 })
}

/// `EMFILE` (24; Windows `ERROR_TOO_MANY_OPEN_FILES`, 4): the process is
/// out of descriptors.
fn emfile() -> io::Error {
    io::Error::from_raw_os_error(if cfg!(windows) { 4 } else { 24 })
}

/// The verdict `probe_cause` attaches to a `Denied` probe carrying
/// [`eperm`]: on Unix raw 1 is the seatbelt's `EPERM` and names the sandbox;
/// on Windows no denial is raw 1, so the wording leaves another user's socket
/// open.
const EPERM_VERDICT: &str = if cfg!(windows) {
    "another user's socket, or a sandbox refusing AF_UNIX connect()"
} else {
    "a sandbox is refusing AF_UNIX connect()"
};

/// One row: the directory and probes, what the report must say, what it must
/// never say, and the exit code. `says` borrows so a row can quote the
/// host's strerror (`&*format!(..)` in an array literal, whose temporaries
/// live to the end of the block).
struct Row<'a> {
    name: &'static str,
    dir: DirOutcome,
    probes: Vec<(u32, String, Probe)>,
    code: u8,
    says: &'a [&'a str],
    never_says: &'a [&'a str],
}

fn check_row(row: Row<'_>) {
    let with_probes = !row.probes.is_empty();
    let (msg, code) = discovery_report(row.dir, &row.probes);
    assert_eq!(code, row.code, "[{}] exit code; report:\n{msg}", row.name);
    for s in row.says {
        assert!(
            msg.contains(s),
            "[{}] must say {s:?}; report:\n{msg}",
            row.name
        );
    }
    for s in row.never_says {
        assert!(
            !msg.contains(s),
            "[{}] must never say {s:?}; report:\n{msg}",
            row.name
        );
    }
    // Exactly one hint block whenever sockets were dialled, none otherwise;
    // a busy majority has none (its rows already quote the server's retry).
    let busy = dominant_cause(&row.probes) == Some(Cause::Busy);
    let hints = msg.matches("hint:").count();
    assert_eq!(
        hints,
        usize::from(with_probes && !busy),
        "[{}] hint blocks; report:\n{msg}",
        row.name
    );
}

const FLEET: &str = "/Users//someone/Library/Application Support/aterm";

fn fleet_sock(pid: u32) -> String {
    format!("{FLEET}/aterm-{pid}.sock")
}

/// Each condition names its cause, and "no live aterm instances found"
/// appears only for a readable, empty directory; plus the `--sock` control.
#[test]
fn discovery_report_names_the_cause_for_every_row_of_the_f8_table() {
    let rows = [
        Row {
            name: "XDG_RUNTIME_DIR set, $XDG_RUNTIME_DIR/aterm does not exist",
            dir: DirOutcome::Missing {
                path: PathBuf::from("/nonexistent/aterm"),
                why: "resolved from $XDG_RUNTIME_DIR, which is set — so \
                      ~/Library/Application Support/aterm was NOT consulted"
                    .to_string(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[
                "control-socket directory /nonexistent/aterm does not exist",
                "$XDG_RUNTIME_DIR",
                "~/Library/Application Support/aterm was NOT consulted",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "HOME pointed elsewhere",
            dir: DirOutcome::Missing {
                path: PathBuf::from("/elsewhere/Library/Application Support/aterm"),
                why: "resolved from $HOME; no aterm instance has published a socket there"
                    .to_string(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[
                "control-socket directory /elsewhere/Library/Application Support/aterm does not exist",
                "$HOME",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "env -i (no HOME, no XDG_RUNTIME_DIR)",
            dir: DirOutcome::Unresolvable,
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &["cannot resolve the control-socket directory", DIR_ENV_UNSET],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "sandbox denies connect() (EPERM) — the Codex case, beside a stale socket",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            probes: vec![
                (10718, fleet_sock(10718), Probe::Denied(eperm())),
                (
                    10274,
                    fleet_sock(10274),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                ),
            ],
            code: EXIT_UNREACHABLE,
            says: &[
                "found 2 control sockets in /Users//someone/Library/Application Support/aterm but could not reach any:",
                &*format!(
                    "\n  aterm-10718.sock  connect: {} — {EPERM_VERDICT}",
                    eperm()
                ),
                "\n  aterm-10274.sock  connect: Connection refused — stale, pid 10274 is not running",
                "hint: the control socket is refused by this sandbox; aterm drives such a session from outside and it takes no part in messaging — nothing to configure",
            ],
            never_says: &[
                FLEET_CLAIM,
                "aterm isn't running",
                "--allow-unix-socket",
                "escalated",
            ],
        },
        Row {
            name: "token file unreadable → the server answers ERR auth",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            probes: vec![(
                10718,
                fleet_sock(10718),
                Probe::NoToken(
                    PathBuf::from(format!("{FLEET}/aterm-10718.token")),
                    eacces(),
                ),
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "found 1 control socket in",
                &*format!(
                    "\n  aterm-10718.sock  token {FLEET}/aterm-10718.token unreadable: {}",
                    eacces()
                ),
                "hint: the token beside a socket must be readable by THIS user",
            ],
            never_says: &[FLEET_CLAIM, "sockets in"],
        },
        Row {
            name: "(control) stale socket of a dead pid, --sock form",
            dir: DirOutcome::Scoped { live_elsewhere: 0 },
            probes: vec![(
                10274,
                "/tmp/aterm-10274.sock".to_string(),
                Probe::Refused {
                    pid_running: Some(false),
                },
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "the addressed instance could not be reached:",
                "\n  /tmp/aterm-10274.sock  connect: Connection refused — stale, pid 10274 is not running",
                LAUNCH_REMEDY,
                "then `aterm ctl instances` names its pid",
            ],
            never_says: &[
                FLEET_CLAIM,
                "found 1 control socket",
                "and retry",
                "did not answer",
            ],
        },
        Row {
            name: "stale socket of a dead pid, --pid form, another instance serving",
            dir: DirOutcome::Scoped { live_elsewhere: 1 },
            probes: vec![(
                10274,
                fleet_sock(10274),
                Probe::Refused {
                    pid_running: Some(false),
                },
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "the addressed instance could not be reached:",
                "stale, pid 10274 is not running",
                "\n  hint: nothing is serving this control socket, but aterm IS running \
                 (1 other instance; `aterm ctl instances` lists it)",
            ],
            never_says: &[FLEET_CLAIM, LAUNCH_REMEDY, "isn't running", "names its pid"],
        },
    ];
    for row in rows {
        check_row(row);
    }
}

/// A scoped run dials the rest of the fleet exactly when its report would
/// otherwise end in the launch remedy: nothing answered, refusals dominate
/// and no refused pid is running. Each case is checked against the report
/// itself, so the predicate cannot drift from the hint it guards.
#[test]
fn only_a_dead_scoped_target_dials_the_rest_of_the_fleet() {
    let refused =
        |n: u32, pid_running: Option<bool>| (n, fleet_sock(n), Probe::Refused { pid_running });
    let cases = [
        (vec![refused(1, Some(false))], true),
        (vec![refused(0, None)], true),
        (vec![refused(1, Some(false)), refused(2, None)], true),
        (
            vec![
                refused(1, Some(false)),
                (2, fleet_sock(2), Probe::Other(io::Error::other("x"))),
            ],
            true,
        ),
        (vec![refused(1, Some(true))], false),
        (vec![refused(1, Some(false)), refused(2, Some(true))], false),
        (
            vec![
                refused(1, Some(false)),
                (2, fleet_sock(2), Probe::Answered(vec![])),
            ],
            false,
        ),
        (vec![(1, fleet_sock(1), Probe::Denied(eperm()))], false),
        (vec![(1, fleet_sock(1), Probe::Timeout)], false),
        (vec![], false),
    ];
    for (probes, due) in cases {
        assert_eq!(dead_target_remedy_due(&probes), due, "{probes:?}");
        let (report, _) = discovery_report(DirOutcome::Scoped { live_elsewhere: 0 }, &probes);
        assert_eq!(report.contains(LAUNCH_REMEDY), due, "{probes:?}: {report}");
    }
}

/// The remaining causes, the exit-code ladder and the report's shape: only
/// `Empty` makes the claim (naming where it looked), all-timeout is 124, one
/// timeout beside a refusal is 2, an out-of-dir socket keeps its full path,
/// hint ties go to the actionable cause, and an answered fleet reports
/// nothing.
#[test]
fn discovery_report_covers_stale_running_auth_timeout_and_empty() {
    let rows = [
        Row {
            name: "readable, empty directory — the one honest fleet claim",
            dir: DirOutcome::Empty(PathBuf::from(FLEET)),
            probes: vec![],
            code: 1,
            says: &[
                "no live aterm instances found (looked in /Users//someone/Library/Application Support/aterm)",
            ],
            never_says: &["could not"],
        },
        Row {
            name: "directory exists but readdir is refused",
            dir: DirOutcome::Unreadable {
                path: PathBuf::from(FLEET),
                err: eperm(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[
                &*format!(
                    "control-socket directory {FLEET} exists but cannot be read: {}",
                    eperm()
                ),
                "a sandbox is refusing it",
            ],
            never_says: &[FLEET_CLAIM, "another user", "readdir()"],
        },
        Row {
            name: "readdir denied by mode bits (EACCES): another user's, or a sandbox",
            dir: DirOutcome::Unreadable {
                path: PathBuf::from(FLEET),
                err: eacces(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[&*format!(
                "control-socket directory {FLEET} exists but cannot be read: {} — another \
                 user's directory, or a sandbox refusing it",
                eacces()
            )],
            never_says: &[FLEET_CLAIM, "a sandbox is refusing it"],
        },
        Row {
            name: "a file where the directory should be (ENOTDIR) is no ownership claim",
            dir: DirOutcome::Unreadable {
                path: PathBuf::from(FLEET),
                err: enotdir(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[&*format!(
                "control-socket directory {FLEET} exists but cannot be read: {} — that \
                 path, or one above it, is not a directory",
                enotdir()
            )],
            never_says: &[FLEET_CLAIM, "another user's", "sandbox"],
        },
        Row {
            name: "out of descriptors (EMFILE): the error alone, no cause attached",
            dir: DirOutcome::Unreadable {
                path: PathBuf::from(FLEET),
                err: emfile(),
            },
            probes: vec![],
            code: EXIT_UNREACHABLE,
            says: &[&*format!(
                "control-socket directory {FLEET} exists but cannot be read: {}",
                emfile()
            )],
            never_says: &[FLEET_CLAIM, "another user's", "sandbox", " — "],
        },
        Row {
            name: "refused, but the pid IS running (still booting)",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            probes: vec![(
                4242,
                fleet_sock(4242),
                Probe::Refused {
                    pid_running: Some(true),
                },
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "\n  aterm-4242.sock  connect: Connection refused — pid 4242 is running but nothing is serving this socket",
                "hint: a pid that is running but not serving is still starting up",
            ],
            never_says: &[FLEET_CLAIM, "stale"],
        },
        Row {
            name: "refused, pid unknown (a --sock name that encodes none)",
            dir: DirOutcome::Scoped { live_elsewhere: 0 },
            probes: vec![(
                0,
                "/tmp/run/c.sock".to_string(),
                Probe::Refused { pid_running: None },
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "\n  /tmp/run/c.sock  connect: Connection refused — nothing is serving this socket (it names no pid)",
            ],
            never_says: &[FLEET_CLAIM, "pid 0"],
        },
        Row {
            name: "the token on disk is not the one the server holds",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            probes: vec![(10718, fleet_sock(10718), Probe::AuthRejected)],
            code: EXIT_UNREACHABLE,
            says: &[
                "\n  aterm-10718.sock  server rejected the token (ERR auth) — a restarted instance rewrites its token",
                "hint: re-run once",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "every socket timed out → 124",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            probes: vec![
                (1, fleet_sock(1), Probe::Timeout),
                (2, fleet_sock(2), Probe::Timeout),
            ],
            code: EXIT_TIMEOUT,
            says: &[
                "\n  aterm-1.sock  timed out after 2s",
                "\n  aterm-2.sock  timed out after 2s",
                "hint: `aterm ctl --sock <path> --timeout 30 sessions`",
                "--timeout 30 sessions",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "one timeout beside a refusal → 2, not 124",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            probes: vec![
                (1, fleet_sock(1), Probe::Timeout),
                (
                    2,
                    fleet_sock(2),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                ),
            ],
            code: EXIT_UNREACHABLE,
            says: &[
                "timed out after 2s",
                "stale, pid 2 is not running",
                "hint: `aterm ctl --sock <path> --timeout 30 sessions`",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "an explicit-socket instance keeps its out-of-dir path; Other passes the cause through",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            probes: vec![(
                77,
                "/private/tmp/agent/x.sock".to_string(),
                Probe::Other(io::Error::new(
                    io::ErrorKind::NotFound,
                    "connect: No such file or directory (os error 2)",
                )),
            )],
            code: EXIT_UNREACHABLE,
            says: &[
                "\n  /private/tmp/agent/x.sock  connect: No such file or directory (os error 2)",
                "hint: `aterm ctl --sock <path> sessions`",
            ],
            never_says: &[FLEET_CLAIM],
        },
        Row {
            name: "tie for the hint: one denied, one stale → the sandbox refusal wins",
            dir: DirOutcome::Scoped { live_elsewhere: 0 },
            probes: vec![
                (
                    5,
                    "/tmp/run/aterm-5.sock".to_string(),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                ),
                (
                    6,
                    "/tmp/run/aterm-6.sock".to_string(),
                    Probe::Denied(eacces()),
                ),
            ],
            code: EXIT_UNREACHABLE,
            says: &[
                &*format!(
                    "\n  /tmp/run/aterm-6.sock  connect: {} — another user's socket, or a sandbox refusing AF_UNIX connect()",
                    eacces()
                ),
                "hint: the control socket is refused by this sandbox;",
            ],
            never_says: &[FLEET_CLAIM, LAUNCH_REMEDY, "--allow-unix-socket"],
        },
    ];
    for row in rows {
        check_row(row);
    }

    // An answered probe is the success path: nothing to report, exit 0 —
    // and a stale neighbour is not mentioned (no noise on success).
    let (msg, code) = discovery_report(
        DirOutcome::Found {
            path: PathBuf::from(FLEET),
            n: 2,
        },
        &[
            (
                10718,
                fleet_sock(10718),
                Probe::Answered(vec!["r s-1".to_string()]),
            ),
            (
                10274,
                fleet_sock(10274),
                Probe::Refused {
                    pid_running: Some(false),
                },
            ),
        ],
    );
    assert_eq!((msg.as_str(), code), ("", 0));
}

/// The per-socket label is aligned into a column so the causes line up.
#[test]
fn discovery_report_aligns_the_cause_column() {
    let (msg, _) = discovery_report(
        DirOutcome::Found {
            path: PathBuf::from(FLEET),
            n: 2,
        },
        &[
            (7, fleet_sock(7), Probe::Timeout),
            (10718, fleet_sock(10718), Probe::Timeout),
        ],
    );
    assert!(msg.contains("\n  aterm-7.sock      timed out"), "{msg}");
    assert!(msg.contains("\n  aterm-10718.sock  timed out"), "{msg}");
}

/// `connect()` failures classify by kind: a permission error is Denied, a
/// refusal is Refused (no liveness verdict for a pid-less socket), a
/// deadline is Timeout, the rest keep the raw cause under `connect:`.
#[test]
fn connect_failures_classify_by_kind_not_by_guess() {
    assert!(matches!(
        classify_connect_error(eperm(), 0),
        Probe::Denied(_)
    ));
    assert!(matches!(
        classify_connect_error(eacces(), 0),
        Probe::Denied(_)
    ));
    assert!(matches!(
        classify_connect_error(io::Error::new(io::ErrorKind::NotConnected, "x"), 0),
        Probe::Denied(_)
    ));
    assert!(matches!(
        classify_connect_error(io::Error::new(io::ErrorKind::ConnectionRefused, "x"), 0),
        Probe::Refused { pid_running: None }
    ));
    assert!(matches!(
        classify_connect_error(
            io::Error::new(io::ErrorKind::ConnectionRefused, "x"),
            DEAD_PID
        ),
        Probe::Refused {
            pid_running: Some(false)
        }
    ));
    assert!(matches!(
        classify_connect_error(io::Error::new(io::ErrorKind::WouldBlock, "x"), 0),
        Probe::Timeout
    ));
    match classify_connect_error(io::Error::new(io::ErrorKind::NotFound, "gone"), 0) {
        Probe::Other(e) => {
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
            assert_eq!(e.to_string(), "connect: gone");
        }
        other => panic!("a vanished socket is Other, got {other:?}"),
    }
    assert!(matches!(
        io_probe(io::Error::new(io::ErrorKind::WouldBlock, "x")),
        Probe::Timeout
    ));
    assert!(matches!(
        io_probe(io::Error::new(io::ErrorKind::TimedOut, "x")),
        Probe::Timeout
    ));
    assert!(matches!(
        io_probe(io::Error::new(io::ErrorKind::BrokenPipe, "x")),
        Probe::Other(_)
    ));
}

/// A connect that never returns consumes one worker only: the first caller
/// leaves at its own deadline, later callers fail closed without queueing
/// behind it, and once released the late stream is closed and the worker
/// joins. Judged by structure, not stopwatches: each call runs on its own
/// thread and reads its end off its own deadline.
#[test]
fn connector_deadline_bounds_one_stuck_worker_and_late_success() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_calls = std::sync::Arc::clone(&calls);
    let (entered, worker_entered) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_sender, blocked) = std::sync::mpsc::sync_channel::<()>(0);
    let connector = std::sync::Arc::new(
        ProbeConnector::spawn(move |_path| {
            worker_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            entered.send(()).expect("announce stalled connector");
            blocked.recv().expect("release stalled connector");
            let (result, peer) = CtlStream::pair()?;
            drop(peer);
            Ok(result)
        })
        .expect("spawn injected connector"),
    );
    // Bound after the connector, so a failing assertion drops it first and
    // the stuck connect errors out instead of a join hanging.
    let release = release_sender;
    // One connect on its own thread: its error, and whether its deadline
    // still had time left the moment it returned.
    let call = |path: &'static str, budget: std::time::Duration| {
        let connector = std::sync::Arc::clone(&connector);
        let (done, outcome) = std::sync::mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let deadline = ProbeDeadline::after(budget);
            let result = connector.connect(path, deadline).map(drop);
            let ran = started.elapsed();
            let _ = done.send((result, deadline.remaining().ok(), ran));
        });
        let (result, left, ran) = outcome
            .recv_timeout(HANG_DETECTOR)
            .unwrap_or_else(|_| panic!("{path}: the caller blocked behind the stuck connect"));
        caller.join().expect("caller thread");
        let error = result.expect_err("a connect behind the stuck worker cannot succeed");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{path}: {error}");
        (left, ran)
    };

    // The first caller claims the worker; `busy` stays set while the connect
    // is stuck. If the budget ran out before submission (the thread was
    // descheduled), retry with a larger budget.
    let claimed = [40, 160, 640, 2560].into_iter().any(|budget_ms| {
        let budget = std::time::Duration::from_millis(budget_ms);
        let (left, ran) = call("forever-stalled", budget);
        assert!(
            left.is_none(),
            "the first caller gave up with {left:?} of its own budget left"
        );
        assert!(
            ran.saturating_sub(budget) < std::time::Duration::from_secs(1),
            "the blocking connect escaped its caller's {budget:?} budget: it ran {ran:?}"
        );
        connector.busy.load(std::sync::atomic::Ordering::Acquire)
    });
    assert!(claimed, "no first caller ever reached the worker");
    worker_entered
        .recv_timeout(HANG_DETECTOR)
        .expect("the sole worker must enter the injected connect");

    for _ in 0..3 {
        let (left, _) = call("must-not-queue", std::time::Duration::from_secs(10));
        assert!(
            left.is_some_and(|left| left >= std::time::Duration::from_millis(8_500)),
            "a later caller waited behind the stuck connect: {left:?} of its 10 s left"
        );
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "later callers must not start or queue another connect"
    );

    release.send(()).expect("release worker");
    // A queued connect would now run and fail on the closed release rather
    // than hang the join below.
    drop(release);
    // Every caller thread is joined, so this is the last handle: dropping it
    // closes the request channel and joins the sole worker.
    drop(std::sync::Arc::into_inner(connector).expect("the callers have exited"));
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a later caller queued a connect the worker ran once released"
    );
}

/// A token miss names the file it looked for (resolved in the socket's own
/// directory); the `Option` wrapper is unchanged.
#[test]
fn a_token_miss_names_the_file_it_looked_for() {
    let (path, err) =
        read_token_at("/nonexistent-aterm-dir/aterm-77.sock").expect_err("no such token");
    assert_eq!(path, Path::new("/nonexistent-aterm-dir/aterm-77.token"));
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(read_token_for("/nonexistent-aterm-dir/aterm-77.sock").is_none());
}

#[cfg(unix)]
#[test]
fn token_reader_rejects_oversized_symlink_and_fifo_without_parking() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-ctl-token-bound-{}-{}",
        std::process::id(),
        aterm_time::since_epoch()
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("token fixture directory");
    let socket = dir.join("mock.sock");
    let token = dir.join("aterm.token");
    let socket_text = socket.to_str().expect("utf8 socket path");

    std::fs::write(
        &token,
        vec![b'x'; MAX_CONTROL_TOKEN_FILE_BYTES as usize + 1],
    )
    .expect("oversized token");
    let (_, error) = read_token_at(socket_text).expect_err("oversized token must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    std::fs::remove_file(&token).expect("remove oversized token");
    std::fs::write(&token, "deadbeef\nsend injected\n").expect("multiline token");
    let (_, error) = read_token_at(socket_text).expect_err("multiline token must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    std::fs::remove_file(&token).expect("remove multiline token");
    let target = dir.join("real.token");
    std::fs::write(&target, "deadbeef\n").expect("real token");
    std::os::unix::fs::symlink(&target, &token).expect("token symlink");
    assert!(
        read_token_at(socket_text).is_err(),
        "the final token component must not be followed"
    );

    std::fs::remove_file(&token).expect("remove token symlink");
    let status = std::process::Command::new("mkfifo")
        .arg(&token)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "create token FIFO");
    let started = std::time::Instant::now();
    let (_, error) = read_token_at(socket_text).expect_err("FIFO token must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        // A parked open of a FIFO with no writer never returns at all; this
        // bound catches only a finite stall: a hang detector, never a latency
        // budget.
        started.elapsed() < HANG_DETECTOR,
        "the token FIFO parked discovery"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A symlink at an explicit socket's own token name ends the search: the
/// directory-shared legacy `aterm.token` beside it is never presented
/// instead.
#[cfg(unix)]
#[test]
fn a_symlinked_socket_token_is_not_passed_over_for_the_shared_one() {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-toklink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let elsewhere = dir.join("elsewhere");
    std::fs::write(&elsewhere, "feed1234\n").expect("a token-shaped file");
    std::os::unix::fs::symlink(&elsewhere, dir.join("a.sock.token")).expect("plant");
    std::fs::write(dir.join("aterm.token"), "cafef00d\n").expect("the shared file");
    let sock = dir.join("a.sock").to_string_lossy().into_owned();
    let read = read_token_beside(&sock);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(read.is_err(), "read {read:?} for a symlinked socket token");
}

/// The server's words before its hang-up, not the write's error: a listener
/// that cannot admit a connection writes its busy line and drops it, often
/// before the client writes, so the write fails (`EPIPE`) while the line waits
/// in the receive buffer. The discovery probe reads it, as the one-shot verbs
/// do ([`pending_status_line`]); a hang-up with nothing said keeps its error.
#[cfg(unix)]
#[test]
fn a_refusal_written_before_the_hang_up_is_what_the_probe_reports() {
    use aterm_types::control_verbs::{CONTROL_BUSY_LINE, CONTROL_BUSY_REPLY};
    use std::io::Write as _;

    // A connection the listener refused: its line written, then dropped,
    // before the client writes anything.
    let refused = |said: &[u8]| {
        let (client, server) = CtlStream::pair().expect("socket pair");
        (&server).write_all(said).expect("the listener's line");
        drop(server);
        client
    };
    let probe = |client: &CtlStream| {
        probe_exchange(
            client,
            Some("tok"),
            None,
            "sessions",
            ProbeDeadline::after(PROBE_DEADLINE),
        )
    };

    match probe(&refused(CONTROL_BUSY_LINE.as_bytes())) {
        Probe::Busy(said) => assert_eq!(said, CONTROL_BUSY_REPLY),
        other => panic!("the busy line before the hang-up is Busy, got {other:?}"),
    }
    // The same holds for the other line a server says before it hangs up.
    assert!(matches!(
        probe(&refused(b"ERR auth\n")),
        Probe::AuthRejected
    ));

    // Negative control: a hang-up that said nothing keeps the write's error.
    match probe(&refused(b"")) {
        Probe::Other(error) => assert!(redial::is_hangup(&error), "{error}"),
        other => panic!("a silent hang-up keeps its error, got {other:?}"),
    }
}

/// A socket that answered only to say it is busy is reported in its own words,
/// under a heading that says it answered, never "did not answer"; busy beside a
/// stale socket keeps the "could not reach" heading.
#[test]
fn discovery_report_quotes_a_busy_server() {
    let busy = || Probe::Busy(aterm_types::control_verbs::CONTROL_BUSY_REPLY.to_string());
    let rows = vec![
        Row {
            name: "--sock at a saturated instance",
            dir: DirOutcome::Scoped { live_elsewhere: 0 },
            probes: vec![(0, "/tmp/at.x/a.sock".to_string(), busy())],
            code: EXIT_UNREACHABLE,
            says: &[
                "the addressed instance answered busy:",
                "\n  /tmp/at.x/a.sock  ERR control server busy; retry — the instance is up",
            ],
            never_says: &["did not answer", "not running", "hint:", FLEET_CLAIM],
        },
        Row {
            name: "every socket in the directory busy",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            probes: vec![(4242, fleet_sock(4242), busy())],
            code: EXIT_UNREACHABLE,
            says: &[
                "found 1 control socket in /Users//someone/Library/Application Support/aterm; each answered busy:",
                "\n  aterm-4242.sock  ERR control server busy; retry",
            ],
            never_says: &["could not reach", FLEET_CLAIM],
        },
        Row {
            name: "(control) busy beside a stale socket",
            dir: DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            probes: vec![
                (4242, fleet_sock(4242), busy()),
                (
                    10274,
                    fleet_sock(10274),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                ),
            ],
            code: EXIT_UNREACHABLE,
            says: &[
                "but could not reach any:",
                "\n  aterm-4242.sock   ERR control server busy; retry",
            ],
            never_says: &["each answered busy", "hint: retry", FLEET_CLAIM],
        },
    ];
    for row in rows {
        check_row(row);
    }
}

/// A mock instance at `dir/<name>` that accepts one connection, reads request
/// lines past any `AUTH` line, answers `reply` (an empty reply hangs up
/// instead), then drains until the client closes. Returns the socket path and
/// the thread, whose value is the request line it saw.
#[cfg(unix)]
fn mock_instance(
    dir: &Path,
    name: &str,
    reply: &'static [u8],
) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{BufRead, Read, Write};
    let sock = dir.join(name);
    let _ = std::fs::remove_file(&sock);
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind mock instance");
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut line = String::new();
        loop {
            line.clear();
            let n = r.read_line(&mut line).expect("read request line");
            if n == 0 || !line.starts_with("AUTH ") {
                break;
            }
        }
        if reply.is_empty() {
            return line; // hang up without a word
        }
        let mut w = conn;
        w.write_all(reply).expect("write reply");
        w.flush().expect("flush");
        // The probe may already have read the whole reply and closed: then the
        // half-close is ENOTCONN, and there is nothing left to finish.
        let _ = w.shutdown(std::net::Shutdown::Write);
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
        line
    });
    (sock.to_str().expect("utf8 path").to_string(), srv)
}

/// The probe against real sockets: an abandoned socket file is Refused
/// (labelled by pid liveness), a missing file is Other, `ERR auth` is
/// NoToken with nothing on disk and AuthRejected when a token was sent, a
/// complete `OK n` is Answered, a truncated `OK n` or headerless hang-up is
/// Other(UnexpectedEof), and a non-auth `ERR` keeps its words. Unix only.
#[cfg(unix)]
#[test]
fn probe_lines_classifies_real_sockets() {
    let _serial = PROBE_SOCKET_TEST_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let dir = std::env::temp_dir().join(format!("aterm-ctl-f8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private dir");

    // Refused: a socket file with nobody accepting — bind, then drop the
    // listener; the file stays behind exactly like a crashed instance's.
    let stale = dir.join(control_socket::instance_sock_name(DEAD_PID));
    abandon_socket(&stale);
    let stale_s = stale.to_str().expect("utf8").to_string();
    assert!(matches!(
        probe_lines(&stale_s, "sessions", DEAD_PID),
        Probe::Refused {
            pid_running: Some(false)
        }
    ));
    assert!(matches!(
        probe_lines(&stale_s, "sessions", 0),
        Probe::Refused { pid_running: None }
    ));
    assert!(matches!(
        probe_lines(&stale_s, "sessions", std::process::id()),
        Probe::Refused {
            pid_running: Some(true)
        }
    ));

    // Vanished: no such file at all.
    let gone = dir.join("aterm-1.sock");
    match probe_lines(gone.to_str().expect("utf8"), "sessions", 1) {
        Probe::Other(e) => {
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
            assert!(e.to_string().starts_with("connect: "), "{e}");
        }
        other => panic!("a missing socket file is Other(NotFound), got {other:?}"),
    }

    // NoToken: a live mock with no token beside it answers ERR auth, and the
    // request went out with no AUTH line.
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR auth\n");
    match probe_lines(&sock, "sessions", 0) {
        // The miss names the per-socket file, not the legacy shared one.
        Probe::NoToken(path, _) => assert_eq!(path, dir.join("mock.sock.token")),
        other => panic!("no token on disk + ERR auth is NoToken, got {other:?}"),
    }
    assert_eq!(srv.join().expect("mock"), "sessions\n");

    // AuthRejected: the token IS on disk, was sent, and still ERR auth.
    std::fs::write(dir.join("mock.sock.token"), "deadbeef\n").expect("token");
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR auth\n");
    assert!(matches!(
        probe_lines(&sock, "sessions", 0),
        Probe::AuthRejected
    ));
    assert_eq!(srv.join().expect("mock"), "sessions\n");

    // Answered: the happy path, body rows trimmed of CR/LF.
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"OK 2\nalpha\r\nbeta\n");
    match probe_lines(&sock, "sessions", 0) {
        Probe::Answered(lines) => assert_eq!(lines, vec!["alpha", "beta"]),
        other => panic!("OK 2 + two rows is Answered, got {other:?}"),
    }
    srv.join().expect("mock");

    // Other: the header promised two rows but the peer supplied only one.
    // The complete two-row arm immediately above is the negative control.
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"OK 2\nalpha\n");
    match probe_lines(&sock, "sessions", 0) {
        Probe::Other(e) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
        other => panic!("a truncated OK body is UnexpectedEof, got {other:?}"),
    }
    srv.join().expect("mock");

    // Other: a non-auth ERR keeps the server's words.
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR usage: sessions\n");
    match probe_lines(&sock, "sessions", 0) {
        Probe::Other(e) => assert_eq!(e.to_string(), "server replied: ERR usage: sessions"),
        other => panic!("a non-auth ERR is Other, got {other:?}"),
    }
    srv.join().expect("mock");

    // Other: hung up without a reply.
    let (sock, srv) = mock_instance(&dir, "mock.sock", b"");
    match probe_lines(&sock, "sessions", 0) {
        Probe::Other(e) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
        other => panic!("a hang-up is Other(UnexpectedEof), got {other:?}"),
    }
    srv.join().expect("mock");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Discovery dials only what it can reach. A fake rendezvous dir holding
/// three graph entries:
///
/// * a dead host: skipped without a connect, its entry pruned;
/// * a foreign one (live listener outside the dir, no token): listed but
///   answered `Probe::Foreign` without a connect;
/// * a reachable one outside the dir with its token: dialled and answered.
///
/// Control: the same foreign socket named explicitly (as `--sock` does) is
/// dialled, and the server refuses it for lack of a token.
#[cfg(unix)]
#[test]
fn discovery_skips_dead_and_foreign_graph_entries_and_dials_the_reachable() {
    use std::io::Write;
    let _serial = PROBE_SOCKET_TEST_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Under `/tmp` for sun_path; removed on drop, so a failing assertion
    // leaves nothing behind.
    let scratch = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch dir");
    let root = scratch.path();
    let (rv, private, public) = (root.join("rv"), root.join("priv"), root.join("pub"));
    for d in [&rv.join("graph"), &private, &public] {
        std::fs::create_dir_all(d).expect("scratch dirs");
    }
    let me = std::process::id();
    let entry = |sid: &str, sock: &Path, pid: u32| {
        let body = format!("sock {}\nnonce 00\npid {pid}\n", sock.display());
        std::fs::write(rv.join("graph").join(sid), body).expect("graph entry");
    };
    // Dead: its host pid is not running.
    entry("s-dead", &root.join("gone").join("a.sock"), DEAD_PID);
    // Foreign: a live listener, no token anywhere beside it.
    let foreign_sock = private.join("a.sock");
    let foreign_listener =
        std::os::unix::net::UnixListener::bind(&foreign_sock).expect("bind foreign");
    foreign_listener.set_nonblocking(true).expect("nonblocking");
    entry("s-foreign", &foreign_sock, me);
    // Reachable: outside the dir too, but its own token sits beside it.
    std::fs::write(public.join("b.sock.token"), "feedbeef\n").expect("token");
    let (reach_sock, srv) = mock_instance(&public, "b.sock", b"OK 1\nlocal s-reach row\n");
    entry("s-reach", Path::new(&reach_sock), me);

    let targets = enumerate_instances(&rv, std::fs::read_dir(&rv).expect("readdir"));
    let foreign_s = foreign_sock.to_str().expect("utf8").to_string();
    assert!(
        targets.iter().all(|t| t.written.is_some()),
        "each entry carries its write time, for the foreign facts: {targets:?}"
    );
    assert_eq!(
        targets
            .iter()
            .cloned()
            .map(Target::listed)
            .collect::<Vec<_>>(),
        {
            let mut want = vec![(me, foreign_s.clone()), (me, reach_sock.clone())];
            want.sort_unstable();
            want
        },
        "the dead entry is skipped; the foreign one is still PUBLISHED"
    );
    assert!(
        !rv.join("graph").join("s-dead").exists(),
        "the dead entry is pruned, so the next call does not read it"
    );
    assert!(
        rv.join("graph").join("s-foreign").exists(),
        "only the dead one"
    );

    let probes: Vec<(u32, String, Probe)> = probe_sessions(targets, Some(rv.clone())).collect();
    for (_, sock, probe) in &probes {
        if *sock == foreign_s {
            match probe {
                Probe::Foreign { token, .. } => {
                    assert_eq!(token, &private.join("a.sock.token"));
                }
                other => panic!("an unreachable private socket is Foreign, got {other:?}"),
            }
        } else {
            match probe {
                Probe::Answered(rows) => assert_eq!(rows, &["local s-reach row"]),
                other => panic!("the reachable instance must be answered, got {other:?}"),
            }
        }
    }
    assert_eq!(srv.join().expect("mock"), "sessions\n");
    assert_eq!(
        foreign_listener.accept().map(drop).map_err(|e| e.kind()),
        Err(io::ErrorKind::WouldBlock),
        "the foreign instance was never connected to"
    );
    assert_eq!(foreign_note("ls", &probes), None, "ls says nothing more");
    // The entry's pid is this test process, which is not aterm: the entry's
    // writer is gone, so `instances` counts no private instance for it.
    assert_eq!(foreign_note("instances", &probes), None);
    // Nothing else answering: the report names it, and says it was not dialed.
    let only_foreign: Vec<_> = probes
        .into_iter()
        .filter(|(_, sock, _)| *sock == foreign_s)
        .collect();
    let (report, code) = discovery_report(
        DirOutcome::Found {
            path: rv.clone(),
            n: 1,
        },
        &only_foreign,
    );
    assert_eq!(code, EXIT_UNREACHABLE);
    // The row names the entry it left, never an instance being there.
    assert!(
        report.contains(&format!(
            "foreign — pid {me}, the entry a private instance left"
        )),
        "the row names the entry, pid and socket: {report}"
    );
    assert!(report.contains(&foreign_s), "{report}");
    assert!(report.contains("not dialed"), "{report}");
    // The writer is gone, so there is no instance to drive: no hint, the
    // row says what the entry is.
    assert!(!report.contains("hint:"), "{report}");
    // Its one action is one this reader can take: never a dial it says is refused.
    assert!(!report.contains("--sock <path>"), "{report}");
    assert!(!report.contains("doctor"), "{report}");

    // Control: named explicitly (no rendezvous), the same socket is dialed.
    foreign_listener.set_nonblocking(false).expect("blocking");
    let refuse = std::thread::spawn(move || {
        let (mut conn, _) = foreign_listener.accept().expect("the explicit dial");
        let mut first = String::new();
        let _ = std::io::BufRead::read_line(
            &mut std::io::BufReader::new(conn.try_clone().expect("clone")),
            &mut first,
        );
        conn.write_all(b"ERR auth\n").expect("refuse");
        first
    });
    let explicit: Vec<_> =
        probe_sessions(vec![Target::named(me, foreign_s.clone())], None).collect();
    assert!(
        matches!(explicit[0].2, Probe::NoToken(..)),
        "{:?}",
        explicit[0].2
    );
    assert_eq!(
        refuse.join().expect("server"),
        "sessions\n",
        "no AUTH line: the request a real instance logs as DENIED"
    );
}

/// An entry naming a live pid whose process was born after the entry was last
/// written (a SIGKILLed instance's pid, reused) is skipped like a dead host's,
/// but kept on disk: the judgement rests on the wall clock, and only a dead
/// pid's entry is removed. Control: the same pid in an entry written now is
/// listed. macOS only: elsewhere no wall-clock birth is kept.
#[cfg(target_vendor = "apple")]
#[test]
fn discovery_skips_and_keeps_an_entry_whose_pid_now_names_a_younger_process() {
    let scratch = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch dir");
    let rv = scratch.path().join("rv");
    std::fs::create_dir_all(rv.join("graph")).expect("graph dir");
    let me = std::process::id();
    let entry = |sid: &str, sock: &str| {
        let path = rv.join("graph").join(sid);
        std::fs::write(&path, format!("sock {sock}\nnonce 00\npid {me}\n")).expect("entry");
        path
    };
    let reused = entry("s-reused", "/tmp/aterm-gone-x/a.sock");
    std::fs::File::options()
        .write(true)
        .open(&reused)
        .and_then(|f| f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000)))
        .expect("back-date the entry");
    let fresh = entry("s-fresh", "/tmp/aterm-live-x/b.sock");

    let targets = enumerate_instances(&rv, std::fs::read_dir(&rv).expect("readdir"));
    assert_eq!(
        targets
            .iter()
            .map(|t| (t.pid, t.sock.as_str()))
            .collect::<Vec<_>>(),
        vec![(me, "/tmp/aterm-live-x/b.sock")],
        "only the entry its process can have written"
    );
    assert!(targets[0].written.is_some());
    assert!(
        reused.exists(),
        "a live pid's entry is skipped, never removed"
    );
    assert!(fresh.exists(), "the control is kept");
}

/// A reply the peer sent before it hung up is read, not lost: macOS refuses the
/// probe's per-read timeout (`EINVAL`) on a socket whose peer has closed.
#[cfg(unix)]
#[test]
fn a_reply_sent_before_the_peer_hung_up_is_read() {
    use std::io::{BufRead, Write};
    let _serial = PROBE_SOCKET_TEST_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let dir = std::env::temp_dir().join(format!("aterm-ctl-hungup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private dir");
    let sock = dir.join("mock.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind mock instance");
    let srv = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        conn.write_all(b"ERR auth\n").expect("write reply");
    });
    let stream = CtlStream::connect(&sock).expect("connect");
    srv.join().expect("the peer wrote and hung up");
    let mut reader = BufReader::new(ProbeIo {
        stream: &stream,
        deadline: ProbeDeadline::after(HANG_DETECTOR),
    });
    let mut line = String::new();
    assert_eq!(reader.read_line(&mut line).expect("the buffered reply"), 9);
    assert_eq!(line, "ERR auth\n");
    line.clear();
    assert_eq!(reader.read_line(&mut line).expect("then the end"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every byte arrives sooner than the socket timeout, so only the one total
/// probe deadline cuts the response off mid-line. The status line alone
/// trickles for three minutes, so a deadline re-armed per read, or checked only
/// between lines, overruns the minute's hang detector; the trickle is paid only
/// on failure (the server stops at the first refused write).
#[cfg(unix)]
#[test]
fn probe_deadline_is_total_against_a_trickling_peer() {
    use std::io::{BufRead, Write};

    const GAP: std::time::Duration = std::time::Duration::from_millis(60);
    let pad = 3 * HANG_DETECTOR.as_millis() / GAP.as_millis();
    let status = format!(
        "OK 1{}\n",
        " ".repeat(usize::try_from(pad).expect("a short pad"))
    );
    let status_trickle = GAP * u32::try_from(status.len()).expect("short status line");
    let reply = format!("{status}alpha\n");

    let _serial = PROBE_SOCKET_TEST_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let dir = std::env::temp_dir().join(format!("aterm-ctl-trickle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private trickle dir");
    let sock = dir.join("mock.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind trickle mock");
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept trickle client");
        let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone client"));
        let mut request = String::new();
        reader.read_line(&mut request).expect("read probe request");
        for byte in reply.bytes() {
            std::thread::sleep(GAP);
            if conn.write_all(&[byte]).and_then(|()| conn.flush()).is_err() {
                break; // the total-deadline client closed as intended
            }
        }
    });

    let started = std::time::Instant::now();
    let probe = probe_lines_with_deadline(
        sock.to_str().expect("utf8 socket"),
        "sessions",
        0,
        std::time::Duration::from_millis(140),
    );
    assert!(matches!(probe, Probe::Timeout), "got {probe:?}");
    assert!(
        started.elapsed() < HANG_DETECTOR,
        "took {:?} of the status line's {status_trickle:?} trickle: the deadline \
         waited for the whole line instead of sharing one budget across its reads",
        started.elapsed()
    );

    server.join().expect("trickle server");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The token mirror resolves in the socket's own directory: `/tmp/c.sock`
/// pairs with `/tmp/c.sock.token`, not `/tmp/c.token` or the directory-wide
/// `/tmp/aterm.token`.
#[test]
fn uds_token_path_resolves_in_the_socket_dir() {
    let p = aterm_uds::latest::token_path_for_sock("/tmp/run/c.sock")
        .expect("a socket path with a parent resolves");
    assert_eq!(p, Path::new("/tmp/run/c.sock.token"));

    let inst = aterm_uds::latest::token_path_for_sock("/tmp/run/aterm-77.sock")
        .expect("instance socket resolves");
    assert_eq!(inst, Path::new("/tmp/run/aterm-77.token"));
}

/// The `attach` lane's foreground handoff names the receiving instance from
/// a socket path: a per-instance socket names its pid, and the flagless
/// alias is a pointer file naming it. Anything else stays `None`, rather than
/// granting the foreground to a pid guessed from a filename.
#[cfg(windows)]
#[test]
fn the_forward_target_pid_comes_from_the_socket_name_or_its_alias() {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-fgpid-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let named = dir.join("aterm-77.sock");
    assert_eq!(
        instance_pid_of(named.to_str().expect("utf8 path")),
        Some(77),
        "a per-instance socket names its own pid"
    );

    let alias = dir.join(control_socket::LATEST_SOCK_FILE);
    std::fs::write(&alias, "aterm-4242.sock\n").expect("write the latest pointer file");
    assert_eq!(
        instance_pid_of(alias.to_str().expect("utf8 path")),
        Some(4242),
        "the flagless alias resolves to the instance it points at"
    );

    // An explicit `--control-sock` path names no instance.
    assert_eq!(
        instance_pid_of(dir.join("c.sock").to_str().expect("utf8")),
        None
    );
    // A pointer file that does not name an instance socket resolves to
    // nothing rather than to the alias's own (pid-less) name.
    std::fs::write(&alias, "..\\..\\somewhere-else.sock").expect("rewrite the pointer file");
    assert_eq!(instance_pid_of(alias.to_str().expect("utf8 path")), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The front door's liveness probe dials the instance the flagless alias
/// points at. On Windows the alias is a pointer file that cannot be
/// connected to, and this probe decides `route_launch`'s
/// `instance_reachable`. Uses a real listener at the instance path and a
/// real pointer file.
#[test]
fn the_front_door_probe_dials_through_the_latest_alias() {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private probe dir");
    let instance = dir.join(control_socket::instance_sock_name(std::process::id()));
    let alias = dir.join(control_socket::LATEST_SOCK_FILE);
    let alias_str = alias.to_str().expect("utf8 path").to_string();

    // Nothing bound yet: the alias exists but the instance is a corpse.
    aterm_uds::latest::publish(&alias, instance.to_str().expect("utf8 path"));
    assert_eq!(
        front_door_probe(&alias_str),
        None,
        "a dangling alias must not route a tab into a corpse"
    );

    // A live listener at the instance path, found through the alias. On
    // Windows the probe answers the resolved instance path (the forward and
    // the foreground grant must name the same instance); on Unix the kernel
    // follows the symlink, `latest::resolve` is the identity, and the probe
    // answers the alias.
    let listener = aterm_uds::CtlListener::bind(&instance).expect("bind the mock instance");
    #[cfg(windows)]
    let expected = instance.to_str().expect("utf8 path");
    #[cfg(not(windows))]
    let expected = alias_str.as_str();
    assert_eq!(
        front_door_probe(&alias_str).as_deref(),
        Some(expected),
        "the probe must dial the live instance through the alias"
    );
    drop(listener);
    let _ = std::fs::remove_file(&instance);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A front-door forward that times out after the request was written is
/// `front_door_delivered` (the running aterm may still open the tab, so
/// the launcher must not open a second window), as is an unreadable
/// reply; a connect failure or a hang-up with no reply is not. The
/// request follows the connect phase's `AUTH` and probe, and a refusal of
/// the connect phase is the reply, with nothing sent after it.
#[cfg(unix)]
#[test]
fn a_front_door_failure_after_the_send_is_delivered() {
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixListener;

    let dir = tokened_socket_dir("fdsend");
    let sock = dir.join("mock.sock");
    let sock_str = sock.to_str().expect("utf8 path").to_string();
    // Every phase the mock answers gets the hang detector (`open_accepted`
    // caps the probe itself at ACCEPT_DEADLINE); only the unanswered reply,
    // the phase under test, has a short deadline.
    let send = |reply| front_door_send_within(&sock_str, "spawn\n", HANG_DETECTOR, reply);

    let refused = send(HANG_DETECTOR).expect_err("nothing listens");
    assert!(!front_door_delivered(&refused), "{refused}");

    // A mock instance: answer the probe, read the request, write `reply`,
    // then with `hold` keep the connection until the client hangs up.
    let instance = |reply: &'static [u8], hold: bool| {
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind mock socket");
        std::thread::spawn(move || {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut w = conn;
            let mut lines = Vec::new();
            for answer in [&b""[..], b"OK version=test\n", reply] {
                let mut line = String::new();
                r.read_line(&mut line).expect("a request line");
                lines.push(line);
                w.write_all(answer).expect("answer");
            }
            if hold {
                let _ = r.read_to_end(&mut Vec::new());
            }
            lines
        })
    };
    let served = ["AUTH deadbeef\n", "version\n", "spawn\n"];

    // Read the request, hang up without a reply.
    let srv = instance(b"", false);
    let hung_up = send(HANG_DETECTOR).expect_err("no reply");
    assert_eq!(srv.join().expect("mock server"), served);
    assert!(!front_door_delivered(&hung_up), "{hung_up}");

    // Read the request, answer nothing: it was taken, so it is delivered.
    let srv = instance(b"", true);
    let unanswered = send(std::time::Duration::from_millis(700)).expect_err("no reply");
    assert_eq!(srv.join().expect("mock server"), served);
    assert!(front_door_delivered(&unanswered), "{unanswered}");
    assert!(is_timeout_error(&unanswered), "{unanswered}");

    // Read the request, answer with bytes that are not a reply: it was
    // taken, so it is delivered.
    let srv = instance(b"\xff\xfe\n", false);
    let garbled = send(HANG_DETECTOR).expect_err("junk reply");
    assert_eq!(srv.join().expect("mock server"), served);
    assert!(front_door_delivered(&garbled), "{garbled}");
    assert_eq!(garbled.kind(), io::ErrorKind::InvalidData, "{garbled}");

    // Refuse the token: the refusal is the reply, and the request never
    // follows the probe.
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    let srv = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut hello = [0u8; 14];
        conn.read_exact(&mut hello).expect("auth line");
        conn.write_all(b"ERR auth\n").expect("refusal");
        let mut rest = String::new();
        let _ = conn.read_to_string(&mut rest);
        rest
    });
    let reply = send(HANG_DETECTOR).expect("a refusal");
    assert_eq!(srv.join().expect("mock server"), "version\n");
    assert!(reply.starts_with("ERR auth"), "{reply}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A listener that never accepts passes `front_door_instance`'s connect, yet a
/// forward to it fails in the connect phase, not delivered, so the launcher
/// opens its own window: a request written into the queue would be read after
/// the launcher gave up, and refused unrun. The queue holds only the probe.
#[cfg(unix)]
#[test]
fn a_front_door_forward_nobody_accepts_is_never_sent() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    let dir = tokened_socket_dir("fdsend-deadq");
    let sock = dir.join("mock.sock");
    let listener = UnixListener::bind(&sock).expect("bind, accept only later");
    let path = sock.to_str().expect("utf8 path").to_string();
    let e = front_door_send_within(
        &path,
        "spawn\n",
        std::time::Duration::from_millis(700),
        HANG_DETECTOR,
    )
    .expect_err("nothing accepts");
    assert!(!front_door_delivered(&e), "{e}");
    assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");

    // The server's late accept finds the client gone and reads what it
    // queued. Nonblocking: the connection is already queued, so a block
    // here would be a hang, not a wait.
    listener.set_nonblocking(true).expect("nonblocking");
    let (mut conn, _) = listener.accept().expect("the queued connection");
    conn.set_nonblocking(false).expect("blocking");
    let mut queued = String::new();
    conn.read_to_string(&mut queued).expect("the queued bytes");
    assert_eq!(
        queued, "AUTH deadbeef\nversion\n",
        "the request was never sent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The dead-`latest` fallback, through both ways an alias dies: a crashed
/// instance's socket refuses (`ConnectionRefused`), and a graceful exit or a
/// swept corpse leaves no alias or a dangling one (`NotFound`). A refused alias
/// resolves to the live instance beside it, never to a live non-instance
/// socket; a live alias, or nothing live, is the alias itself.
#[test]
fn flagless_target_falls_back_to_the_newest_live_instance_when_latest_is_dead() {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-latest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private dir");
    let alias = dir.join(control_socket::LATEST_SOCK_FILE);
    let alias_s = alias.to_str().expect("utf8").to_string();
    let corpse = dir.join(control_socket::instance_sock_name(DEAD_PID));
    let live = dir.join(control_socket::instance_sock_name(std::process::id()));
    let live_s = live.to_str().expect("utf8").to_string();

    // Nothing bound at all — the alias dangles, the swept-but-unpublished
    // state: the alias comes back so the error names it.
    aterm_uds::latest::publish(&alias, corpse.to_str().expect("utf8"));
    #[cfg(unix)]
    assert_eq!(
        CtlStream::connect(&alias).map(drop).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound),
        "a dangling alias is ENOENT"
    );
    assert_eq!(flagless_target(&dir, alias_s.clone()), alias_s);
    assert_eq!(newest_live_instance_in(&dir), None);

    // The crash state: the corpse bound then abandoned, so the alias names a
    // socket that refuses. Beside it, a live instance and a live
    // explicit-name socket that must never be chosen.
    abandon_socket(&corpse);
    #[cfg(unix)]
    assert_eq!(
        CtlStream::connect(&alias).map(drop).map_err(|e| e.kind()),
        Err(io::ErrorKind::ConnectionRefused),
        "a crashed instance's alias is ECONNREFUSED, not ENOENT"
    );
    let listener = aterm_uds::CtlListener::bind(&live).expect("bind the live instance");
    let stranger =
        aterm_uds::CtlListener::bind(dir.join("private.sock")).expect("bind the explicit one");
    assert_eq!(
        flagless_target(&dir, alias_s.clone()),
        live_s,
        "a dead alias resolves to the live instance beside it"
    );
    assert_eq!(
        newest_live_instance_in(&dir).as_deref(),
        Some(live_s.as_str())
    );

    // The alias repointed at the live instance: returned as-is, so the
    // caller dials the alias.
    aterm_uds::latest::publish(&alias, &live_s);
    assert_eq!(
        flagless_target(&dir, alias_s.clone()),
        alias_s,
        "a live alias is dialed as the alias"
    );

    // A graceful exit removes the socket and the alias. Only the non-instance
    // socket is left: back to the alias.
    drop(listener);
    let _ = std::fs::remove_file(&live);
    let _ = std::fs::remove_file(&alias);
    #[cfg(unix)]
    assert_eq!(
        CtlStream::connect(&alias).map(drop).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound),
        "a removed alias is ENOENT"
    );
    assert_eq!(flagless_target(&dir, alias_s.clone()), alias_s);
    assert_eq!(newest_live_instance_in(&dir), None);
    drop(stranger);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Newest" is the bind instant, not the pid: pids wrap, so the youngest
/// instance can carry the lowest pid. Ties and an unreadable mtime fall back
/// to pid order.
#[test]
fn newest_live_instance_is_by_bind_time_not_pid() {
    use std::time::{Duration, UNIX_EPOCH};
    let t = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(s));
    let sock = |pid: u32| format!("/run/aterm-{pid}.sock");
    // The wrapped pid 12 bound last: it wins over the higher pid 99878.
    assert_eq!(
        newest(vec![(t(100), 99878, sock(99878)), (t(200), 12, sock(12))]).as_deref(),
        Some("/run/aterm-12.sock")
    );
    // Equal bind instants: pid order, as `enumerate_instances` sorts.
    assert_eq!(
        newest(vec![(t(100), 12, sock(12)), (t(100), 99878, sock(99878))]).as_deref(),
        Some("/run/aterm-99878.sock")
    );
    // An unreadable mtime loses to any readable one.
    assert_eq!(
        newest(vec![(None, 99878, sock(99878)), (t(1), 12, sock(12))]).as_deref(),
        Some("/run/aterm-12.sock")
    );
    assert_eq!(newest(Vec::new()), None);
}

// The multiplexer boundary. Inside a `screen` window a flagless `aterm ctl
// status` answers from the outer session. tmux's `$TMUX` marker is checked
// first because its default `TERM` is `screen-256color`: the kind comes from
// the marker and only the family from `TERM`.

/// Tier 1: the integration's `$ATERM_MUX` marker decides, and its disable
/// spellings turn the guard off (for a stale marker outside any pane).
#[test]
fn mux_marker_decides_and_its_disable_spellings_turn_the_guard_off() {
    let n = detect_mux_nesting(&MuxEnv {
        mux: Some("screen"),
        outer_sid: Some("s-outer"),
        ..MuxEnv::default()
    })
    .expect("a marked boundary is nested");
    assert_eq!(n.kind, "screen");
    assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
    assert_eq!(
        n.detected_by, "shell-integration",
        "the in-pane marker is the top tier"
    );
    // The marker wins even when nothing else in the environment agrees —
    // including a session base that would otherwise answer "no crossing".
    let n = detect_mux_nesting(&MuxEnv {
        mux: Some("tmux"),
        base: Some("|"),
        term: Some("xterm-256color"),
        self_sid: Some("s-env"),
        ..MuxEnv::default()
    })
    .expect("marked");
    assert_eq!(n.kind, "tmux");
    assert_eq!(
        n.outer_sid.as_deref(),
        Some("s-env"),
        "$ATERM_PARENT_SESSION_ID stands in when the marker carries no outer sid"
    );
    for off in ["0", "off", "no", "none", "false"] {
        assert_eq!(
            detect_mux_nesting(&MuxEnv {
                mux: Some(off),
                base: Some("|"),
                outer_sid: Some("s-outer"),
                tmux: Some("/tmp/tmux-1000/default,9,0"),
                term: Some("screen-256color"),
                self_sid: Some("s-outer"),
                ..MuxEnv::default()
            }),
            None,
            "ATERM_MUX={off} must disable the guard outright"
        );
    }
}

/// Tier 2, the tier that answers in a real pane: aterm injects bash through
/// `--rcfile`, which a shell started by screen never receives, so
/// `$ATERM_MUX` is empty there and the session shell's `$ATERM_MUX_BASE`
/// decides.
#[test]
fn mux_session_base_decides_a_pane_without_reading_term() {
    // A session shell born outside any multiplexer stamps "|"; the pane's own
    // $STY no longer matches, which alone is the crossing.
    let n = detect_mux_nesting(&MuxEnv {
        base: Some("|"),
        sty: Some("4242.pts-3.host"),
        term: Some("screen.xterm-256color"),
        self_sid: Some("s-outer"),
        ..MuxEnv::default()
    })
    .expect("STY differing from the session base is a pane");
    assert_eq!(n.kind, "screen");
    assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
    assert_eq!(n.detected_by, "session-base");
    // tmux with `default-terminal "xterm-256color"` looks like an aterm
    // window to TERM; the base never reads TERM, so it is a pane.
    let n = detect_mux_nesting(&MuxEnv {
        base: Some("|"),
        tmux: Some("/tmp/tmux-1000/default,9,0"),
        term: Some("xterm-256color"),
        self_sid: Some("s-outer"),
        ..MuxEnv::default()
    })
    .expect("a tmux pane with aterm's own TERM is still a pane");
    assert_eq!(n.kind, "tmux", "the marker names the program, not TERM");
    assert_eq!(n.detected_by, "session-base");
    // An aterm window launched from a pane re-stamps the base as its own, so
    // its inherited $TMUX matches and it is not refused.
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            base: Some("/tmp/tmux-1000/default,9,0|"),
            tmux: Some("/tmp/tmux-1000/default,9,0"),
            term: Some("xterm-256color"),
            self_sid: Some("s-fresh"),
            ..MuxEnv::default()
        }),
        None,
        "a base equal to this environment means nothing was entered since"
    );
    // A nested multiplexer inside that window is a crossing again.
    let n = detect_mux_nesting(&MuxEnv {
        base: Some("/tmp/tmux-1000/default,9,0|"),
        tmux: Some("/tmp/tmux-1000/default,9,7"),
        term: Some("screen-256color"),
        self_sid: Some("s-fresh"),
        ..MuxEnv::default()
    })
    .expect("a DIFFERENT tmux than the session's own is a pane");
    assert_eq!(n.kind, "tmux");
    // Different base, but no marker names a multiplexer (unset by hand):
    // nothing to report, and no kind is invented from TERM.
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            base: Some("/tmp/tmux-1000/default,9,0|"),
            term: Some("screen-256color"),
            self_sid: Some("s-fresh"),
            ..MuxEnv::default()
        }),
        None,
        "a base mismatch with no live marker names no multiplexer"
    );
}

/// Tier 3, for a shell with neither mark: it needs both an aterm identity to
/// mis-target and a multiplexer-written `TERM`. `TERM` (forced to
/// `xterm-256color` by aterm) is what keeps an aterm window launched from a
/// pane, which inherits `$TMUX`/`$STY`, out of the refusal.
#[test]
fn mux_fallback_needs_an_aterm_identity_and_a_multiplexer_term() {
    let n = detect_mux_nesting(&MuxEnv {
        sty: Some("4242.pts-3.host"),
        term: Some("screen.xterm-256color"),
        self_sid: Some("s-outer"),
        ..MuxEnv::default()
    })
    .expect("STY + a screen TERM inside an aterm session is a pane");
    assert_eq!(n.kind, "screen");
    assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
    assert_eq!(n.detected_by, "environment", "no mark was present");
    // tmux's default TERM is screen-256color, so the marker, not TERM, names
    // the program.
    let n = detect_mux_nesting(&MuxEnv {
        tmux: Some("/tmp/tmux-1000/default,9,0"),
        term: Some("screen-256color"),
        self_sid: Some("s-outer"),
        ..MuxEnv::default()
    })
    .expect("tmux pane");
    assert_eq!(n.kind, "tmux");
    // An aterm window launched from a pane: markers inherited, TERM aterm's.
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            tmux: Some("/tmp/tmux-1000/default,9,0"),
            sty: Some("4242.pts-3.host"),
            term: Some("xterm-256color"),
            self_sid: Some("s-fresh"),
            ..MuxEnv::default()
        }),
        None,
        "a stale $TMUX/$STY with aterm's own TERM is not a pane"
    );
    // A screen inside a non-aterm terminal has no aterm identity to
    // mis-target; flagless calls there are not refused.
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            sty: Some("1.pts-0.h"),
            term: Some("screen"),
            ..MuxEnv::default()
        }),
        None,
        "no aterm session identity in scope means nothing to mis-target"
    );
    // Empty is unset everywhere, including the base (whose real spelling is
    // never shorter than "|").
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            mux: Some(""),
            base: Some(""),
            outer_sid: Some(""),
            tmux: Some(""),
            sty: Some(""),
            term: Some(""),
            self_sid: Some(""),
            ..MuxEnv::default()
        }),
        None
    );
}

/// Tier 3's second corroboration: tmux 3.2+ stamps `TERM_PROGRAM=tmux` even
/// when `TERM` looks like aterm's. Negative controls: an aterm window
/// launched from the pane re-stamps `TERM_PROGRAM=aterm`, and
/// `TERM_PROGRAM=tmux` without a `$TMUX` marker names nothing.
#[test]
fn mux_fallback_takes_tmuxs_term_program_when_term_is_xterm() {
    let n = detect_mux_nesting(&MuxEnv {
        tmux: Some("/tmp/tmux-1000/default,9,0"),
        term: Some("xterm-256color"),
        self_sid: Some("s-outer"),
        term_program: Some("tmux"),
        ..MuxEnv::default()
    })
    .expect("a tmux 3.2+ pane with an xterm TERM is still a pane");
    assert_eq!(n.kind, "tmux");
    assert_eq!(n.detected_by, "environment");
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            tmux: Some("/tmp/tmux-1000/default,9,0"),
            term: Some("xterm-256color"),
            self_sid: Some("s-fresh"),
            term_program: Some("aterm"),
            ..MuxEnv::default()
        }),
        None,
        "an aterm window launched from a pane re-stamps TERM_PROGRAM"
    );
    assert_eq!(
        detect_mux_nesting(&MuxEnv {
            term: Some("xterm-256color"),
            self_sid: Some("s-outer"),
            term_program: Some("tmux"),
            ..MuxEnv::default()
        }),
        None,
        "TERM_PROGRAM alone, with no $TMUX, names no pane"
    );
}

/// Which invocation shapes name their target only implicitly — the ones a
/// multiplexer silently redirects to the outer terminal.
#[test]
fn implicit_self_targeting_is_exactly_the_flagless_at_self_and_at_dot_shapes() {
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
    // Flagless with nothing pinned.
    assert!(targets_own_session_implicitly(
        &parts(&["text"]),
        None,
        None
    ));
    // Any explicit instance pin is a deliberate choice, not silence.
    assert!(!targets_own_session_implicitly(
        &parts(&["text"]),
        Some("/tmp/x.sock"),
        None
    ));
    assert!(!targets_own_session_implicitly(
        &parts(&["text"]),
        None,
        Some(42)
    ));
    // `@self`/`@env` claim this session by name — false in a pane however
    // the socket was chosen, so a pin does not rescue them.
    for sel in ["@self", "@env"] {
        assert!(targets_own_session_implicitly(
            &parts(&[sel, "send", "hi"]),
            Some("/tmp/x.sock"),
            Some(42)
        ));
    }
    // …including inside `subscribe`'s selector-second position and a list.
    assert!(targets_own_session_implicitly(
        &parts(&["subscribe", "@self,@1", "screen"]),
        None,
        None
    ));
    // `@.` follows the active tab — as implicit as flagless, and pinned the
    // same way.
    assert!(targets_own_session_implicitly(
        &parts(&["@.", "text"]),
        None,
        None
    ));
    assert!(!targets_own_session_implicitly(
        &parts(&["@.", "text"]),
        None,
        Some(7)
    ));
    // A concrete sid names its target; nothing implicit is left.
    assert!(!targets_own_session_implicitly(
        &parts(&["@s-abc", "text"]),
        None,
        None
    ));
    // The client-answered verbs address no session at all.
    for verb in MUX_EXEMPT_VERBS {
        assert!(
            !targets_own_session_implicitly(&parts(&[verb]), None, None),
            "{verb} addresses no session"
        );
    }
    // Nor anything the shared table calls Target::Meta (a selector there is
    // `ERR denied`). Driven from the table, so a new Meta verb is exempt
    // automatically.
    let meta: Vec<&str> = aterm_types::control_verbs::VERBS
        .iter()
        .filter(|s| matches!(s.target, aterm_types::control_verbs::Target::Meta))
        .map(|s| s.name)
        .collect();
    for verb in &meta {
        assert!(
            !targets_own_session_implicitly(&parts(&[verb]), None, None),
            "{verb} addresses no session and cannot be mis-targeted by a multiplexer"
        );
        for selector in ["@self", "@env", "@.", "@s-abc"] {
            assert!(
                !targets_own_session_implicitly(&parts(&[selector, verb]), None, None),
                "the server validates {selector} {verb}; it has no session to mis-target"
            );
        }
    }
    // The exemption is exactly the session-less set — a session or app verb
    // is still refused, because those really can land in the wrong terminal.
    for verb in ["text", "send", "key", "blocks", "window", "tab", "spawn"] {
        assert!(
            targets_own_session_implicitly(&parts(&[verb]), None, None),
            "{verb} names a real target and stays guarded"
        );
    }
    // Empty argv is the usage error's business, not this guard's.
    assert!(!targets_own_session_implicitly(&[], None, None));
}

/// The refusal is actionable: it names the session a flagless call would
/// have driven, the explicit form, the two instance pins, and how to switch
/// the guard off.
#[test]
fn nested_self_drive_error_names_the_outer_session_and_every_way_forward() {
    let n = MuxNesting {
        kind: "tmux",
        outer_sid: Some("s-1a2b".to_string()),
        detected_by: "session-base",
    };
    let e = nested_self_drive_error(&n);
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    let msg = e.to_string();
    for needle in [
        "inside tmux",
        "s-1a2b",
        "aterm ctl @s-1a2b <verb>",
        "--pid",
        "--sock",
        "aterm ctl instances",
        "aterm ctl mux",
        "ATERM_MUX=0",
        "OSC 133",
    ] {
        assert!(msg.contains(needle), "refusal must name {needle:?}: {msg}");
    }
    // Every remedy it names is accepted by the reachable set: `@<sid>` is
    // offered only to the Session and App verbs that reach the guard. If a
    // Meta verb ever became refusable, this fails.
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
    for spec in aterm_types::control_verbs::VERBS {
        if matches!(spec.target, aterm_types::control_verbs::Target::Meta) {
            assert!(
                !targets_own_session_implicitly(&parts(&[spec.name]), None, None),
                "{} is session-less, so `@<sid> {}` must never be prescribed \
                 — the server rejects a selector there",
                spec.name,
                spec.name
            );
        }
    }
}

/// The block-model caveat rides only on verbs whose answer the boundary
/// shapes, and only when the request addresses the session hosting the
/// multiplexer.
#[test]
fn mux_degradation_note_marks_the_block_reading_verbs_only() {
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
    let n = MuxNesting {
        kind: "screen",
        outer_sid: Some("s-1a2b".to_string()),
        detected_by: "environment",
    };
    for verb in ["blocks", "status"] {
        let note = mux_degradation_note(&n, &parts(&[verb]))
            .unwrap_or_else(|| panic!("{verb} reads the block model"));
        assert!(note.contains("inside screen"), "{note}");
        assert!(note.contains("s-1a2b"), "{note}");
        assert!(note.contains("ABSENT"), "absent, not empty: {note}");
        // Its silencer is the notice's, never the guard's off switch.
        assert!(note.contains("ATERM_MUX_NOTICE=0"), "{note}");
        assert!(!note.contains("ATERM_MUX=0"), "{note}");
        // The same verb aimed at the hosting session explicitly still earns
        // the note: that is the session the multiplexer is starving.
        let sel = mux_degradation_note(&n, &parts(&["@s-1a2b", verb]));
        assert!(sel.is_some(), "{verb} at the outer sid still applies");
    }
    // A different session is not the one the pane is starving.
    assert_eq!(
        mux_degradation_note(&n, &parts(&["@s-other", "blocks"])),
        None
    );
    // Verbs whose answer the boundary does not shape stay quiet.
    for verb in ["text", "cursor", "send", "meta"] {
        assert_eq!(
            mux_degradation_note(&n, &parts(&[verb])),
            None,
            "{verb} does not read the block model"
        );
    }
    // No aterm identity, no note to give.
    let anon = MuxNesting {
        kind: "screen",
        outer_sid: None,
        detected_by: "shell-integration",
    };
    assert_eq!(mux_degradation_note(&anon, &parts(&["blocks"])), None);
}

/// `aterm ctl mux` with no multiplexer says what a flagless call reaches:
/// the active tab inside a session, the newest instance outside any, and
/// that the check is off when ATERM_MUX turned it off.
#[test]
fn mux_none_explanation_names_what_a_flagless_call_reaches() {
    let inside = mux_none_explanation(None, true);
    assert!(
        inside.contains("drives the tab you are looking at"),
        "{inside}"
    );
    assert!(inside.contains("@self drives this shell"), "{inside}");
    let outside = mux_none_explanation(None, false);
    assert!(!outside.contains("@<sid>"), "{outside}");
    for off in ["0", "off"] {
        let line = mux_none_explanation(Some(off), true);
        assert_eq!(line, format!("multiplexer check off (ATERM_MUX={off})"));
    }
    assert!(mux_none_explanation(Some("tmux"), true).starts_with("no multiplexer"));
    assert!(
        outside.starts_with("not inside an aterm session"),
        "{outside}"
    );
    assert!(outside.contains("newest aterm instance"), "{outside}");
    assert!(!outside.contains("no multiplexer"), "{outside}");
}

/// `mux` is a client verb, so it is discoverable where the other client verbs
/// are: the `--help` client block and the completion verb list.
#[test]
fn the_mux_client_verb_is_documented_and_completable() {
    let help = help_text();
    assert!(
        help.contains("    mux "),
        "the mux verb is in --help: {help}"
    );
    assert!(
        help.contains("ATERM_MUX=0"),
        "--help names the escape hatch"
    );
    let verbs = completion_verb_list();
    assert!(
        verbs.split_whitespace().any(|v| v == "mux"),
        "mux completes like ls/instances"
    );
}

/// The notice stamp matches the shell integration's exactly, since both
/// halves share it: `${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/aterm/mux-notice/<kind>-<id>`
/// with `id=${TMUX:-${STY:-$TERM}}` and `${id//[!A-Za-z0-9._-]/_}`.
#[test]
fn the_mux_notice_stamp_matches_the_shell_integrations_path_exactly() {
    // tmux's marker wins the id, and every byte outside the shell's class
    // (`/`, `,`) becomes `_` — one substitution per character, not per run.
    assert_eq!(
        mux_notice_stamp(
            "tmux",
            Some("/tmp/tmux-1000/default,9,0"),
            Some("4242.pts-3.h"),
            Some("screen-256color"),
            Some("/run/user/1000"),
            None,
        ),
        Path::new("/run/user/1000/aterm/mux-notice/tmux-_tmp_tmux-1000_default_9_0"),
    );
    // $STY when there is no $TMUX; dots and dashes survive untouched, which
    // is what keeps one screen's panes on one stamp.
    assert_eq!(
        mux_notice_stamp(
            "screen",
            None,
            Some("4242.pts-3.host"),
            Some("screen.xterm-256color"),
            Some("/run/user/1000"),
            Some("/tmp"),
        ),
        Path::new("/run/user/1000/aterm/mux-notice/screen-4242.pts-3.host"),
    );
    // The root falls through XDG_RUNTIME_DIR -> TMPDIR -> /tmp, and TERM is the
    // id of last resort, as in the shell's `${TMUX:-${STY:-$TERM}}`.
    assert_eq!(
        mux_notice_stamp(
            "screen",
            Some(""),
            None,
            Some("screen"),
            Some(""),
            Some("/x")
        ),
        Path::new("/x/aterm/mux-notice/screen-screen"),
    );
    assert_eq!(
        mux_notice_stamp("screen", None, None, None, None, None),
        Path::new("/tmp/aterm/mux-notice/screen-"),
    );
}

/// The notice belongs to a boundary that cost something: a screen inside
/// another terminal never had an aterm block model to lose, so neither half
/// announces there. The shell integration draws the line at
/// `$ATERM_PARENT_SESSION_ID`, and this draws it in the same place. Driven on
/// a real path, with a positive control that the claim does write.
#[test]
fn the_boundary_is_announced_only_where_it_cost_a_session_something() {
    let root = std::env::temp_dir().join(format!("aterm-ctl-muxnotice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("private scratch root");
    // The path the shell and `aterm ctl` both compute, resolved by the
    // shipping resolver off this test's root.
    let stamp = mux_notice_stamp(
        "screen",
        None,
        Some("77.pts-1.h"),
        None,
        root.to_str(),
        None,
    );
    assert_eq!(
        stamp,
        root.join("aterm")
            .join("mux-notice")
            .join("screen-77.pts-1.h"),
        "fixture: the stamp under test is the shell's own path, resolved from this root"
    );
    // The shipping mechanism (`create_new` on the resolved stamp), aimed at
    // this test's root instead of the process-wide `$XDG_RUNTIME_DIR`.
    let claim = |_: &MuxNesting| claim_mux_notice_at(&stamp);

    // Anonymous: no aterm session outside, so the gate returns before the
    // claim runs at all — nothing written, nothing spent.
    let anon = MuxNesting {
        kind: "screen",
        outer_sid: None,
        detected_by: "shell-integration",
    };
    announce_mux_boundary_with(&anon, false, claim);
    assert!(
        !stamp.exists(),
        "an anonymous multiplexer must leave no stamp behind"
    );

    // Ours: the same call and path with an aterm session outside.
    // `already_said = true` claims without printing.
    let ours = MuxNesting {
        kind: "screen",
        outer_sid: Some("s1".to_string()),
        detected_by: "shell-integration",
    };
    announce_mux_boundary_with(&ours, true, claim);
    assert!(
        stamp.exists(),
        "a boundary that cost an aterm session its block model claims the stamp"
    );

    // Once: the second pane to reach `create_new` is refused.
    assert!(
        !claim_mux_notice_at(&stamp),
        "a claimed stamp must refuse the next claimant"
    );

    // …and the `None` nesting is a no-op, not a panic.
    announce_mux_boundary(None, false);
    let _ = std::fs::remove_dir_all(&root);
}

/// A `sessions` row, as a real server frames one.
fn session_rows(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|r| (*r).to_string()).collect()
}

/// An in-process caller gets the rows `ls` prints: the server's line
/// verbatim, the hosting pid, the addressed columns, and ` *` as a bit.
#[test]
fn fleet_sessions_carry_the_ls_row_verbatim_and_the_self_marker_as_data() {
    let rows = session_rows(&[
        "0 s-a - alive sh meta=0 window=1 active=1 wfocus=1 detail=claude",
        "1 s-b s-a alive sh meta=1 window=1 active=0 wfocus=1 detail=-",
    ]);
    let listed = instance_sessions(4242, &rows, Some("s-b"));

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].pid, 4242);
    assert_eq!(listed[0].row, rows[0], "the server's row, byte for byte");
    assert_eq!(listed[0].sid(), Some("s-a"));
    assert!(!listed[0].is_self);
    // Exactly the row `ls` would mark ` *`: its sid is the calling terminal's.
    assert!(listed[1].is_self, "the caller's own session is flagged");

    // No `$ATERM_PARENT_SESSION_ID` marks nothing — a row whose sid column is
    // absent must never match a missing self sid by both being `None`.
    let outside = instance_sessions(1, &session_rows(&["0"]), None);
    assert_eq!(outside[0].sid(), None, "a row too short names no session");
    assert!(!outside[0].is_self);
}

/// A sid two live instances both answer to shows up as a duplicate. One
/// instance listing a sid once is not two claimants, and a row too short
/// for a sid is skipped.
#[test]
fn a_sid_two_instances_both_serve_is_reported_as_ambiguous() {
    let rows = |pid, sids: &[&str]| {
        instance_sessions(
            pid,
            &session_rows(
                &sids
                    .iter()
                    .enumerate()
                    .map(|(i, sid)| format!("{i} {sid} - alive sh meta=0"))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
            None,
        )
    };

    // A healthy fleet: two instances, distinct ids, nothing to report.
    let mut healthy = rows(10, &["s-a", "s-b"]);
    healthy.extend(rows(11, &["s-c"]));
    assert!(duplicate_sids(&healthy).is_empty());

    // A clash: pid 10 and pid 11 both answer to `s-a`.
    let mut clash = rows(10, &["s-a", "s-b"]);
    clash.extend(rows(11, &["s-a"]));
    assert_eq!(
        duplicate_sids(&clash),
        vec![("s-a".to_string(), vec![10, 11])],
        "the duplicated id and BOTH pids claiming it"
    );

    // One instance, one id, listed twice (it cannot happen through a store
    // keyed by sid, but a listing must not invent a collision out of it).
    let mut repeated = rows(10, &["s-a"]);
    repeated.extend(rows(10, &["s-a"]));
    assert!(
        duplicate_sids(&repeated).is_empty(),
        "one instance is one claimant, however many rows it sends"
    );

    // A row with no sid column is skipped, exactly as the printer skips it.
    let short = instance_sessions(10, &session_rows(&["0"]), None);
    assert!(duplicate_sids(&short).is_empty());
}

/// An in-process listing reports an empty fleet, an unreachable one and a
/// wedged one as three different things, the same three `ls` exits on.
#[test]
fn a_fleet_listing_failure_is_never_an_empty_listing() {
    let sock = fleet_sock(4242);
    let answered = |rows: &[&str]| vec![(4242, sock.clone(), Probe::Answered(session_rows(rows)))];

    // Reachable: the sessions, however many — Ok is the only "empty fleet".
    let rows = instance_sessions(4242, &session_rows(&["0 s-a - alive sh meta=0"]), None);
    let dir = DirOutcome::Found {
        path: PathBuf::from(FLEET),
        n: 1,
    };
    let ok = fleet_listing(
        rows,
        discovery_report(dir, &answered(&["0 s-a - alive sh meta=0"])),
    )
    .expect("an instance answered");
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].sid(), Some("s-a"));

    // Genuinely empty: the one condition that licenses the fleet claim.
    let empty = fleet_listing(
        Vec::new(),
        discovery_report(DirOutcome::Empty(PathBuf::from(FLEET)), &[]),
    )
    .expect_err("no instance answered");
    assert!(empty.is_empty_fleet(), "a readable, empty directory");
    assert_eq!(empty.code, EXIT_EMPTY_FLEET);
    assert!(empty.reason.contains(FLEET_CLAIM));

    // Found but unreachable (the seatbelt case): not empty, and it says so.
    let denied = vec![(4242, sock.clone(), Probe::Denied(eperm()))];
    let blind = fleet_listing(
        Vec::new(),
        discovery_report(
            DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            &denied,
        ),
    )
    .expect_err("nothing answered");
    assert!(!blind.is_empty_fleet(), "sockets were found, not absent");
    assert_eq!(blind.code, EXIT_UNREACHABLE);
    assert!(blind.reason.contains(EPERM_VERDICT), "{}", blind.reason);
    assert!(!blind.reason.contains(FLEET_CLAIM));

    // Wedged: every probe timed out, which is its own status (124).
    let wedged = fleet_listing(
        Vec::new(),
        discovery_report(
            DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 1,
            },
            &[(4242, sock, Probe::Timeout)],
        ),
    )
    .expect_err("nothing answered");
    assert_eq!(wedged.code, EXIT_TIMEOUT);
    assert!(!wedged.is_empty_fleet());
}

/// The `windows` fold over real `sessions` lines: windows in id order, the
/// front bit from `wfocus=`, active-tab sids joined, the detached count
/// apart from the could-not-say count; a server without `window=` folds
/// into `window=-`.
#[test]
fn windows_rows_fold_sessions_per_window_and_keep_unknown_apart_from_none() {
    let lines: Vec<String> = [
        // A server with the identity column (session identities): one more
        // key=value after detail=, read by key like the rest.
        "0 s-a - alive sh meta=0 window=1 active=1 wfocus=1 detail=claude identity=worker",
        "1 s-b s-a alive sh meta=1 window=1 active=0 wfocus=1 detail=-",
        "2 s-c - alive sh meta=0 window=0 active=1 wfocus=0 detail=codex",
        "3 s-d - alive sh meta=0 window=none active=0 wfocus=0 detail=-",
        "4 s-e - alive sh meta=0 window=- active=- wfocus=- detail=-",
        "5 s-f - alive sh meta=0",
        "6 s-g - alive sh meta=0 window=1 active=1 wfocus=1 detail=-",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        window_rows_from_sessions(42, &lines),
        vec![
            "42 window=0 focused=0 sessions=1 active=s-c".to_string(),
            "42 window=1 focused=1 sessions=3 active=s-a,s-g".to_string(),
            "42 window=none sessions=1".to_string(),
            "42 window=- sessions=2".to_string(),
        ]
    );
    // A window with nothing on its active tab says so.
    let lines = vec!["0 s-a - alive sh meta=0 window=2 active=0 wfocus=0 detail=-".to_string()];
    assert_eq!(
        window_rows_from_sessions(7, &lines),
        vec!["7 window=2 focused=0 sessions=1 active=-".to_string()]
    );
    // No sessions: no rows (the instance is still listed by `instances`).
    assert!(window_rows_from_sessions(7, &[]).is_empty());
}

/// The title is the fifth field and any program can set it (`pct_encode`
/// leaves `=` alone), so titles like `window=none`, `window=7`, `active=1`,
/// `wfocus=1` or `meta=1` must not affect the fold: only the server's tokens
/// after `meta=` count. `ls` prints the line verbatim and is unaffected.
#[test]
fn windows_rows_never_read_the_title_as_a_token() {
    let lines: Vec<String> = [
        "0 s-a - alive window=none meta=0 window=3 active=0 wfocus=0 detail=-",
        "1 s-b - alive active=1 meta=0 window=3 active=0 wfocus=0 detail=-",
        "2 s-c - alive wfocus=1 meta=0 window=3 active=1 wfocus=0 detail=-",
        "3 s-d - alive window=7 meta=0 window=none active=0 wfocus=0 detail=-",
        "4 s-e - alive window=7 meta=1 window=5 active=1 wfocus=1 detail=-",
        "5 s-f - alive meta=1 meta=0 window=5 active=0 wfocus=1 detail=-",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        window_rows_from_sessions(9, &lines),
        vec![
            "9 window=3 focused=0 sessions=3 active=s-c".to_string(),
            "9 window=5 focused=1 sessions=2 active=s-e".to_string(),
            "9 window=none sessions=1".to_string(),
        ]
    );
    // A pre-`window=` server whose title happens to be `window=1` (or
    // `wfocus=1`, or `detail=x`): still "could not say", never window 1.
    for title in ["window=1", "wfocus=1", "detail=x"] {
        let lines = vec![format!("0 s-a - alive {title} meta=0")];
        assert_eq!(
            window_rows_from_sessions(9, &lines),
            vec!["9 window=- sessions=1".to_string()],
            "{title}"
        );
    }
    // No `meta=` anchor at all (a pre-roster server): no tail is
    // recognised, whatever `=`-bearing words the title carries.
    let lines = vec!["0 s-a - alive window=1".to_string()];
    assert_eq!(
        window_rows_from_sessions(9, &lines),
        vec!["9 window=- sessions=1".to_string()]
    );
}

/// An empty title is two spaces on the wire, collapsed by a whitespace
/// split; reading from the end still lands the keys, including a column a
/// later server appends after `detail=`.
#[test]
fn windows_rows_tolerate_an_empty_title_and_trailing_new_columns() {
    let lines: Vec<String> = [
        "0 s-a - alive  meta=0 window=2 active=1 wfocus=1 detail=-",
        "1 s-b - alive  meta=0",
        "2 s-c - alive sh meta=0 window=2 active=0 wfocus=1 detail=- extra=1 more=x",
        "3 s-d - alive  meta=0 window=none active=0 wfocus=0 detail=- extra=1",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        window_rows_from_sessions(9, &lines),
        vec![
            "9 window=2 focused=1 sessions=2 active=s-a".to_string(),
            "9 window=none sessions=1".to_string(),
            "9 window=- sessions=1".to_string(),
        ]
    );
}

/// `roster_tail`: the `key=value` run behind `meta=` in end-first order,
/// nothing without the anchor, and the walk stops at the first `=`-less word.
#[test]
fn roster_tail_reads_back_to_the_meta_anchor_or_nothing() {
    let f = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let full = f("0 s-a - alive window=7 meta=0 window=1 active=1 wfocus=0 detail=-");
    let fields: Vec<&str> = full.iter().map(String::as_str).collect();
    assert_eq!(
        roster_tail(&fields),
        vec!["detail=-", "wfocus=0", "active=1", "window=1"]
    );
    let bare = f("0 s-a - alive x=1 meta=0");
    let fields: Vec<&str> = bare.iter().map(String::as_str).collect();
    assert!(roster_tail(&fields).is_empty(), "nothing after the anchor");
    let old = f("0 s-a - alive window=1 active=1");
    let fields: Vec<&str> = old.iter().map(String::as_str).collect();
    assert!(roster_tail(&fields).is_empty(), "no anchor, no tail");
    assert!(roster_tail(&[]).is_empty());
}

/// `windows` takes no argument: `windows 1` is `ERR usage: windows` (exit
/// FAILURE); a bare `windows` is fine; a non-discovery request is not this
/// check's concern.
#[test]
fn windows_rejects_trailing_arguments_with_the_usage_shape() {
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        discovery_given_arguments(&parts(&["windows", "1"])),
        Some("windows")
    );
    assert_eq!(
        discovery_given_arguments(&parts(&["windows", "--all"])),
        Some("windows")
    );
    assert_eq!(discovery_given_arguments(&parts(&["windows"])), None);
    assert_eq!(discovery_given_arguments(&parts(&["sessions", "1"])), None);
    assert_eq!(discovery_given_arguments(&parts(&[])), None);
    let e = discovery_usage_error("windows");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(e.to_string(), "ERR usage: windows");
    assert!(
        !is_timeout_error(&e),
        "a usage error exits FAILURE, not 124"
    );
}

/// `ls 1` is `ERR usage: ls` (exit FAILURE), as for `windows`; a bare `ls` is
/// fine.
#[test]
fn ls_rejects_trailing_arguments_with_the_usage_shape() {
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(discovery_given_arguments(&parts(&["ls", "1"])), Some("ls"));
    assert_eq!(
        discovery_given_arguments(&parts(&["ls", "--json"])),
        Some("ls")
    );
    assert_eq!(discovery_given_arguments(&parts(&["ls"])), None);
    let e = discovery_usage_error("ls");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(e.to_string(), "ERR usage: ls");
    assert!(
        !is_timeout_error(&e),
        "a usage error exits FAILURE, not 124"
    );
}

/// The same for `instances`: `instances 1` is `ERR usage: instances`, not
/// the fleet; a bare `instances` is fine.
#[test]
fn instances_rejects_trailing_arguments_with_the_usage_shape() {
    let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        discovery_given_arguments(&parts(&["instances", "1"])),
        Some("instances")
    );
    assert_eq!(
        discovery_given_arguments(&parts(&["instances", "self"])),
        Some("instances")
    );
    assert_eq!(discovery_given_arguments(&parts(&["instances"])), None);
    let e = discovery_usage_error("instances");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(e.to_string(), "ERR usage: instances");
    assert!(
        !is_timeout_error(&e),
        "a usage error exits FAILURE, not 124"
    );
}

/// A private dir holding the token file for `mock.sock`: the token is what
/// makes the client authenticate, and so probe.
#[cfg(unix)]
fn tokened_socket_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-ctl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private dir");
    std::fs::write(dir.join("mock.sock.token"), "deadbeef\n").expect("token");
    dir
}

/// A forward whose host went away under the request (`ERR gone pid=<host>
/// sock=<its socket>`) is the host's hang-up, never a refusal: an anchorless
/// read is asked once of the host's successor (found through `graph/<sid>`
/// beside its socket), a `turn` ends 75, and any other verb fails as a
/// connection closed without a reply.
#[cfg(unix)]
#[test]
fn a_forward_whose_host_went_away_is_a_replacement_not_a_refusal() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("aterm-ctl-fwd-gone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("graph")).expect("graph dir");
    let relay_sock = dir.join("a.sock");
    let host_sock = dir.join("b.sock");
    let successor_sock = dir.join("b2.sock");
    let gone = {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    // Answer `version` OK, stop on `stop`, and anything else with `reply`.
    fn serve(listener: UnixListener, reply: String) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut line = String::new();
                if std::io::BufReader::new(&conn)
                    .read_line(&mut line)
                    .unwrap_or(0)
                    == 0
                {
                    continue;
                }
                if line.starts_with("stop") {
                    return;
                }
                let answer = if line.starts_with("version") {
                    "OK aterm test\n"
                } else {
                    reply.as_str()
                };
                let _ = conn.write_all(answer.as_bytes());
            }
        })
    }
    let relay = serve(
        UnixListener::bind(&relay_sock).expect("bind relay"),
        format!("ERR gone pid={gone} sock={}\n", host_sock.display()),
    );
    let successor = serve(
        UnixListener::bind(&successor_sock).expect("bind successor"),
        "OK 1\nfrom the successor\n".to_string(),
    );
    std::fs::write(
        dir.join("graph").join("s-abc"),
        format!("sock {}\n", successor_sock.display()),
    )
    .expect("the successor's graph entry");
    let path = relay_sock.to_str().expect("utf8").to_string();
    let redial = redial::Redial::new(redial::Follow::Path(path.clone())).with_bound(HANG_DETECTOR);
    let ask = |request: &str, verb: &str| {
        exchange(
            &path,
            TargetOrigin::Pinned,
            request,
            verb,
            request,
            Some(std::time::Duration::from_secs(60)),
            false,
            &redial,
        )
    };
    assert_eq!(
        ask("@s-abc text\n", "text").expect("asked of the host's successor"),
        ExitCode::SUCCESS,
        "an anchorless read is asked once of the host's successor"
    );
    assert_eq!(
        ask("@s-abc turn hi\n", "turn").expect("a reported outcome"),
        ExitCode::from(redial::EXIT_REPLACED),
        "a `turn` is never asked again: 75"
    );
    let err = ask("@s-abc key enter\n", "key").expect_err("a connection that closed");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
    assert!(
        err.to_string()
            .contains("server closed the connection without responding"),
        "the words a supervisor rides out: {err}"
    );
    for sock in [&relay_sock, &successor_sock] {
        if let Ok(mut conn) = std::os::unix::net::UnixStream::connect(sock) {
            let _ = conn.write_all(b"stop\n");
        }
    }
    relay.join().expect("relay");
    successor.join().expect("successor");
    let _ = std::fs::remove_dir_all(dir);
}

/// A listener that never accepts: the client's connect succeeds into the kernel
/// queue and nothing ever answers. The connect phase names the failure within
/// its own bound, not the verb's whole deadline (900 s by default), and exits
/// [`EXIT_UNREACHABLE`], distinct from a timeout (124) or an `ERR` (1).
#[cfg(unix)]
#[test]
fn a_dead_listen_queue_fails_fast_and_named() {
    let dir = tokened_socket_dir("accept-deadq");
    let sock = dir.join("mock.sock");
    let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind, never accept");
    let path = sock.to_str().expect("utf8").to_string();
    let started = std::time::Instant::now();
    let outcome = exchange(
        &path,
        TargetOrigin::Pinned,
        "text\n",
        "text",
        "text\n",
        Some(std::time::Duration::from_millis(700)),
        false,
        &redial::Redial::none(),
    )
    .expect("a dead queue is a reported outcome, not an I/O error");
    let waited = started.elapsed();
    assert_eq!(outcome, ExitCode::from(EXIT_UNREACHABLE));
    assert!(waited < HANG_DETECTOR, "the connect phase took {waited:?}");
    // The caller's shorter deadline governs the connect phase, not
    // ACCEPT_DEADLINE: read off the bound itself, not a stopwatch.
    let short = std::time::Duration::from_millis(700);
    assert!(
        matches!(
            open_accepted(&path, TargetOrigin::Pinned, Some(short)),
            Ok(Acceptance::NotAccepted(d)) if d == short
        ),
        "the connect phase waits the caller's {short:?}, not {ACCEPT_DEADLINE:?}"
    );
    let report = not_accepted_message(&path, std::time::Duration::from_secs(10));
    assert!(
        report.contains("the instance took no request"),
        "the report names the failure: {report}"
    );
    assert!(
        report.contains("the process is not running"),
        "the report names both causes the client cannot tell apart: {report}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// With a token, an instance that takes the connection and never answers
/// ends within the connect bound as the not-accepted report (exit
/// [`EXIT_UNREACHABLE`], indistinguishable from a dead queue), and the
/// request is never sent. (Without a token nothing is probed:
/// `a_server_that_never_answers_is_a_named_wait_not_eagain`.)
#[cfg(unix)]
#[test]
fn a_tokened_server_that_takes_the_connection_and_never_answers_gets_no_request() {
    use std::io::{BufRead, Read};
    use std::os::unix::net::UnixListener;

    let dir = tokened_socket_dir("accept-silent");
    let sock = dir.join("mock.sock");
    let listener = UnixListener::bind(&sock).expect("bind mock socket");
    // Take the connection, read everything the client sends, answer
    // nothing: the connection ends when the client gives up.
    let srv = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn);
        let mut first = String::new();
        r.read_line(&mut first).expect("first line");
        let mut rest = Vec::new();
        let _ = r.read_to_end(&mut rest);
        (first, String::from_utf8_lossy(&rest).into_owned())
    });
    let path = sock.to_str().expect("utf8").to_string();
    let started = std::time::Instant::now();
    let outcome = exchange(
        &path,
        TargetOrigin::Pinned,
        "text\n",
        "text",
        "text\n",
        Some(std::time::Duration::from_millis(700)),
        false,
        &redial::Redial::none(),
    )
    .expect("a silent instance is a reported outcome, not an I/O error");
    let waited = started.elapsed();
    assert_eq!(outcome, ExitCode::from(EXIT_UNREACHABLE));
    assert!(waited < HANG_DETECTOR, "the connect phase took {waited:?}");
    let (first, rest) = srv.join().expect("server");
    assert_eq!(first, "AUTH deadbeef\n", "the client authenticates first");
    assert_eq!(
        rest.as_bytes(),
        ACCEPT_PROBE,
        "only the probe went out, never the request"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The dead queue through the two exchanges that open their own connection:
/// `help <verb>` and `cast drift`. Both use the connect phase, so both name
/// the failure within its bound.
#[cfg(unix)]
#[test]
fn help_and_cast_drift_fail_fast_on_a_dead_queue_too() {
    let dir = tokened_socket_dir("accept-deadq-own");
    let sock = dir.join("mock.sock");
    let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind, never accept");
    let path = sock.to_str().expect("utf8").to_string();
    let deadline = Some(std::time::Duration::from_millis(700));
    let started = std::time::Instant::now();
    let help = help_with_fallback(
        &path,
        TargetOrigin::Pinned,
        "status",
        deadline,
        &LocalVerbs::NONE,
    )
    .expect("a dead queue is a reported outcome, not an I/O error");
    let drift = drift::live(
        &path,
        TargetOrigin::Pinned,
        drift::Route::default(),
        &[],
        deadline,
        &LocalVerbs::NONE,
    )
    .expect("a dead queue is a reported outcome, not an I/O error");
    let waited = started.elapsed();
    assert_eq!(help, ExitCode::from(EXIT_UNREACHABLE), "help <verb>");
    assert_eq!(drift, ExitCode::from(EXIT_UNREACHABLE), "cast drift");
    assert!(
        waited < HANG_DETECTOR,
        "the two connect phases took {waited:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// An update's predecessor drops the connection at its Commit, and the one
/// reconnect lands on the successor with its own token. The listener is
/// replaced between the first connect and the retry (token rewritten before
/// the new listener binds, the successor's order), and the retry must use
/// the new token.
#[cfg(unix)]
#[test]
fn the_retry_after_a_replaced_listener_sends_the_new_token() {
    use std::io::{BufRead, Write};
    let dir = tokened_socket_dir("accept-replaced");
    let sock = dir.join("mock.sock");
    let token_file = dir.join("mock.sock.token");
    let predecessor = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let server = {
        let sock = sock.clone();
        std::thread::spawn(move || {
            let (held, _) = predecessor.accept().expect("accept the first attempt");
            let mut first = Vec::new();
            let mut held_lines = std::io::BufReader::new(&held);
            for _ in 0..2 {
                let mut line = String::new();
                held_lines
                    .read_line(&mut line)
                    .expect("a first-attempt line");
                first.push(line);
            }
            std::fs::remove_file(&token_file).expect("unlink the old token");
            std::fs::write(&token_file, "cafef00d\n").expect("the new token");
            std::fs::remove_file(&sock).expect("unlink the old socket");
            let successor = aterm_uds::CtlListener::bind(&sock).expect("rebind");
            // The predecessor's Commit: what it held closes unanswered.
            drop(held_lines);
            drop(held);
            drop(predecessor);
            let (conn, _) = successor.accept().expect("accept the retry");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut w = conn;
            let mut lines = Vec::new();
            for reply in [&b""[..], b"OK version=test\n", b"OK done\n"] {
                let mut line = String::new();
                r.read_line(&mut line).expect("a request line");
                let refused = lines.is_empty() && line != "AUTH cafef00d\n";
                lines.push(line);
                if refused {
                    w.write_all(b"ERR auth\n").expect("refusal");
                    break;
                }
                w.write_all(reply).expect("reply");
            }
            (first, lines)
        })
    };
    let path = sock.to_str().expect("utf8").to_string();
    let outcome = exchange(
        &path,
        TargetOrigin::Pinned,
        "status\n",
        "status",
        "status\n",
        Some(std::time::Duration::from_secs(60)),
        false,
        &redial::Redial::none(),
    )
    .expect("exchange");
    let (first, lines) = server.join().expect("server");
    assert_eq!(first, ["AUTH deadbeef\n", "version\n"]);
    assert_eq!(lines, ["AUTH cafef00d\n", "version\n", "status\n"]);
    assert_eq!(outcome, ExitCode::SUCCESS);
    let _ = std::fs::remove_dir_all(dir);
}

/// A client that authenticates its own connection
/// ([`connect_then_read_token`]: the supervisor's relay, the live upgrade's
/// drivers, `aterm link`, the window's fleet menu) gets the token of the
/// listener that took its connection, even when the instance is replaced
/// at the moment of the read.
#[cfg(unix)]
#[test]
fn the_token_read_for_a_connection_is_its_listeners() {
    let dir = tokened_socket_dir("connect-then-read");
    let sock = dir.join("mock.sock");
    let token_file = dir.join("mock.sock.token");
    let predecessor = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let mut successor = None;
    let path = sock.to_str().expect("utf8").to_string();
    let (_stream, token) = connect_then_read(&path, |at| {
        let token = read_token_beside(at);
        std::fs::remove_file(&token_file).expect("unlink the old token");
        std::fs::write(&token_file, "cafef00d\n").expect("the new token");
        std::fs::remove_file(&sock).expect("unlink the old socket");
        successor = Some(aterm_uds::CtlListener::bind(&sock).expect("rebind"));
        token
    })
    .expect("connect");
    let successor = successor.expect("the read ran");
    predecessor.set_nonblocking(true).expect("nonblocking");
    successor.set_nonblocking(true).expect("nonblocking");
    let holds = |l: &aterm_uds::CtlListener| l.accept().is_ok();
    let holders_token = match (holds(&predecessor), holds(&successor)) {
        (true, false) => "deadbeef",
        (false, true) => "cafef00d",
        other => panic!("exactly one listener holds the connection: {other:?}"),
    };
    assert_eq!(token.expect("a token"), holders_token);
    let _ = std::fs::remove_dir_all(dir);
}

/// Through the `latest` alias, the token read for a connection is the one
/// beside the instance the connect reached, even when another live instance
/// republishes the alias in between.
#[cfg(unix)]
#[test]
fn a_token_read_through_the_alias_is_the_connected_instances() {
    let dir = tokened_socket_dir("alias-flip");
    let (a, b) = (dir.join("aterm-1001.sock"), dir.join("aterm-1002.sock"));
    std::fs::write(dir.join("aterm-1001.token"), "aaaa\n").expect("token a");
    std::fs::write(dir.join("aterm-1002.token"), "bbbb\n").expect("token b");
    let first = aterm_uds::CtlListener::bind(&a).expect("bind a");
    let second = aterm_uds::CtlListener::bind(&b).expect("bind b");
    let alias = dir.join(SOCK_FILE);
    aterm_uds::latest::publish(&alias, a.to_str().expect("utf8"));
    let path = alias.to_str().expect("utf8").to_string();
    let (_stream, token) = connect_then_read(&path, |at| {
        aterm_uds::latest::publish(&alias, b.to_str().expect("utf8"));
        read_token_beside(at)
    })
    .expect("connect");
    first.set_nonblocking(true).expect("nonblocking");
    second.set_nonblocking(true).expect("nonblocking");
    let holds = |l: &aterm_uds::CtlListener| l.accept().is_ok();
    let holders_token = match (holds(&first), holds(&second)) {
        (true, false) => "aaaa",
        (false, true) => "bbbb",
        other => panic!("exactly one listener holds the connection: {other:?}"),
    };
    assert_eq!(token.expect("a token"), holders_token);
    let _ = std::fs::remove_dir_all(dir);
}

/// The connect phase proves acceptance before the request: the server sees
/// `AUTH`, the `version` probe, then the request, and the probe's reply is
/// not printed. A refusal of the probe ends the exchange with that line.
#[cfg(unix)]
#[test]
fn the_connect_probe_precedes_the_request() {
    use std::io::{BufRead, Write};
    let dir = tokened_socket_dir("accept-probe");
    let sock = dir.join("mock.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let server = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut w = conn;
        let mut lines = Vec::new();
        for reply in [&b""[..], b"OK version=test\n", b"OK done\n"] {
            let mut line = String::new();
            r.read_line(&mut line).expect("a request line");
            lines.push(line);
            w.write_all(reply).expect("reply");
        }
        lines
    });
    let path = sock.to_str().expect("utf8").to_string();
    let outcome = exchange(
        &path,
        TargetOrigin::Pinned,
        "status\n",
        "status",
        "status\n",
        Some(HANG_DETECTOR),
        false,
        &redial::Redial::none(),
    )
    .expect("exchange");
    assert_eq!(outcome, ExitCode::SUCCESS);
    let lines = server.join().expect("server");
    assert_eq!(lines, ["AUTH deadbeef\n", "version\n", "status\n"]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A busy listener writes `ERR control server busy; retry` and closes
/// without reading; the client reports that line (not the failed write) and
/// exits 1.
#[cfg(unix)]
#[test]
fn a_busy_reply_is_reported_as_busy() {
    use std::io::Write;
    const BUSY: &str = "ERR control server busy; retry";
    // The exchange: the busy line arrives as the connect-phase answer.
    let dir = tokened_socket_dir("accept-busy");
    let sock = dir.join("mock.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        conn.write_all(b"ERR control server busy; retry\n")
            .expect("busy");
    });
    let path = sock.to_str().expect("utf8").to_string();
    let outcome = open_accepted(&path, TargetOrigin::Pinned, Some(HANG_DETECTOR)).expect("open");
    server.join().expect("server");
    match outcome {
        Acceptance::Refused(line) => assert_eq!(line, BUSY),
        Acceptance::Served(_) => panic!("a busy server was read as serving"),
        Acceptance::NotAccepted(w) => panic!("a busy server read as not accepting ({w:?})"),
        Acceptance::Dropped => panic!("the busy line was lost"),
    }

    // The failed write: the server already answered and closed, so the
    // client's send is what fails — and the line it left behind is read.
    let (client, server_end) = CtlStream::pair().expect("pair");
    (&server_end)
        .write_all(b"ERR control server busy; retry\n")
        .expect("busy");
    drop(server_end);
    let big = vec![b'x'; 1 << 20];
    let sent = send_served(&client, &big).expect("a hang-up with a reason is not an error");
    assert_eq!(sent, Some(ExitCode::FAILURE));
    let (client, server_end) = CtlStream::pair().expect("pair");
    drop(server_end);
    assert!(
        send_served(&client, &big).is_err(),
        "negative control: a hang-up with no reason stays the I/O error"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A refused token names its file: the server answers a bare `ERR auth`, and
/// the client says which file it sent after `; `, the shape a supervisor keys on.
#[cfg(unix)]
#[test]
fn an_auth_refusal_names_the_token_file() {
    use std::io::{Read, Write};
    let dir = tokened_socket_dir("accept-auth");
    let sock = dir.join("mock.sock");
    let token = dir.join("mock.sock.token");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut hello = [0u8; 14];
        conn.read_exact(&mut hello).expect("auth line");
        conn.write_all(b"ERR auth\n").expect("refusal");
        listener
    });
    let path = sock.to_str().expect("utf8").to_string();
    let outcome = open_accepted(&path, TargetOrigin::Pinned, Some(HANG_DETECTOR)).expect("open");
    let listener = server.join().expect("server");
    match outcome {
        Acceptance::Refused(line) => assert_eq!(
            line,
            format!(
                "ERR auth; the server refused the token in {}; re-run",
                token.display()
            )
        ),
        _ => panic!("a refused token was not reported as refused"),
    }
    drop(listener);
    assert_eq!(
        auth_refusal("ERR control server busy; retry", Some(token.as_path())),
        "ERR control server busy; retry",
        "only `ERR auth` gains the file"
    );
    assert_eq!(auth_refusal("ERR auth", None), "ERR auth", "no token sent");
    let _ = std::fs::remove_dir_all(dir);
}

/// A busy listener can accept, answer and close before the client sets its
/// connect-phase timeouts, which Darwin then refuses with EINVAL; the connect
/// phase still reads the busy line it left, never "Invalid argument".
#[cfg(unix)]
#[test]
fn a_busy_answer_that_beat_the_timeouts_is_still_read() {
    use std::io::Write;
    const BUSY: &str = "ERR control server busy; retry";
    let (client, server_end) = CtlStream::pair().expect("pair");
    (&server_end)
        .write_all(b"ERR control server busy; retry\n")
        .expect("busy");
    drop(server_end);
    let set = client.set_write_timeout(Some(std::time::Duration::from_secs(1)));
    if let Err(e) = &set {
        assert!(peer_already_closed(e), "an unexpected refusal: {e:?}");
    }
    #[cfg(target_os = "macos")]
    assert!(
        set.is_err(),
        "Darwin set a timeout on a socket whose peer had closed"
    );
    match answer_left_behind(&client, Some(Path::new("/d/mock.sock.token"))) {
        Acceptance::Refused(line) => assert_eq!(line, BUSY),
        Acceptance::Served(_) => panic!("a busy server was read as serving"),
        Acceptance::NotAccepted(w) => panic!("a busy server read as not accepting ({w:?})"),
        Acceptance::Dropped => panic!("the busy line was lost"),
    }
    assert!(
        !peer_already_closed(&io::Error::new(io::ErrorKind::InvalidInput, "zero")),
        "std's own zero-timeout refusal carries no errno and is not a hang-up"
    );
}
