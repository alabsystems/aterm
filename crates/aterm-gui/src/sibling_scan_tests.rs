// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::parse_session_row;
use aterm_types::needs::NeedWord;

#[cfg(unix)]
#[test]
fn queued_survey_lines_survive_the_siblings_close() {
    use super::*;
    let (stream, mut peer) = aterm_uds::CtlStream::pair().unwrap();
    peer.write_all(b"OK 1\nsession sid=s-peer needs=-\n")
        .unwrap();
    drop(peer);
    aterm_spec::wait::until_peer_closed(&stream);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut reader = BufReader::new(PeerRead {
        stream: &stream,
        deadline,
    });
    assert_eq!(
        read_bounded_line(&mut reader, deadline).as_deref(),
        Some("OK 1\n")
    );
    assert_eq!(
        read_bounded_line(&mut reader, deadline).as_deref(),
        Some("session sid=s-peer needs=-\n")
    );
    assert_eq!(read_bounded_line(&mut reader, deadline), None);
}

/// A `survey facts` row is read by key: its published need, its role
/// (else its program) as the label, and the age of the fact it stands on
/// (none for an ask, whose row carries only the agent publication's);
/// `needs=-` is a counted session needing nothing; an exited session and
/// a line that is no session row are not counted.
#[test]
fn survey_facts_rows_carry_the_published_need() {
    let (need, published) = parse_session_row(
        "session local=3 sid=s-abc state=alive program=claude program_kind=agent \
         agent=wall:unresponsive agent_ms=4000 task=- task_ms=- role=review \
         attention=1 needs=frozen hold_by=-\n",
    )
    .expect("a live row");
    assert!(published);
    let need = need.expect("its need");
    assert_eq!(
        (
            need.word,
            need.sid.as_str(),
            need.label.as_str(),
            need.age_ms
        ),
        (NeedWord::Frozen, "s-abc", "review", Some(4000))
    );
    let (need, _) = parse_session_row(
        "session local=4 sid=s-def state=alive program=claude task=stalled task_ms=9000 \
         agent_ms=1 role=- needs=stalled hold_by=-",
    )
    .expect("a live row");
    let need = need.expect("its need");
    assert_eq!((need.label.as_str(), need.age_ms), ("claude", Some(9000)));
    let (need, _) = parse_session_row(
        "session local=9 sid=s-ask state=alive program=claude agent=idle \
         agent_ms=10800000 needs=ask hold_by=-",
    )
    .expect("a live row");
    assert_eq!(need.expect("its need").age_ms, None, "an ask has no age");
    let (need, _) = parse_session_row(
        "session local=10 sid=s-sleep state=alive program=sleep program_kind=program \
         agent=- agent_ms=240912 needs=frozen hold_by=-",
    )
    .expect("a live row");
    assert_eq!(need.expect("its need").age_ms, None, "a shell's freeze");
    assert_eq!(
        parse_session_row("session local=5 sid=s-x state=alive needs=- hold_by=-"),
        Some((None, true))
    );
    assert_eq!(
        parse_session_row("session local=6 sid=s-y state=exited needs=ask"),
        None
    );
    assert_eq!(parse_session_row("instance pid=1 host=h"), None);
    assert_eq!(
        parse_session_row("session local=7 sid=s-z state=alive attention=1"),
        Some((None, false)),
        "an older aterm's row"
    );
    assert_eq!(
        parse_session_row("session local=8 sid=s-w state=alive needs=someday"),
        Some((None, true)),
        "a newer word is never guessed"
    );
}

/// A need the row says is the program's own report (`needs_by=program`) keeps the words
/// that say so: an approval, a sign-in (the survey's `agent=wall:auth`) and a question each
/// in their own; CONTROL: the same need with no `needs_by=` (a box a reader saw, or an older
/// aterm's row) has none.
#[test]
fn a_row_the_programs_own_report_raised_says_so() {
    use crate::needs::{CODEX_APPROVAL_WORDS, CODEX_QUESTION_WORDS, CODEX_SIGN_IN_WORDS};
    let claim = |needs: &str, agent: &str, by: &str| {
        parse_session_row(&format!(
            "session local=3 sid=s-abc state=alive program=codex program_kind=agent \
             agent={agent} agent_ms=4000 needs={needs} needs_by={by} hold_by=-"
        ))
        .and_then(|(need, _)| need)
        .expect("its need")
        .claim
    };
    assert_eq!(
        claim("approval", "prompt", "program"),
        Some(CODEX_APPROVAL_WORDS)
    );
    assert_eq!(
        claim("approval", "wall:auth", "program"),
        Some(CODEX_SIGN_IN_WORDS)
    );
    assert_eq!(
        claim("question", "question", "program"),
        Some(CODEX_QUESTION_WORDS)
    );
    assert_eq!(claim("approval", "prompt", "-"), None, "CONTROL");
}

/// A need the row says is a harness's `NEEDS YOU:` report (`needs_by=harness`) carries on,
/// so Needs you lists it after the stopped work (§14.7); CONTROL: the same ask with
/// `needs_by=-` (another owner's direct ask, or an older aterm's row) and a program's own
/// report have stopped their agents.
#[test]
fn a_row_a_harness_reported_carries_on() {
    let carries_on = |by: &str| {
        parse_session_row(&format!(
            "session local=3 sid=s-abc state=alive program=claude program_kind=agent \
             agent=busy agent_ms=4000 needs=ask needs_by={by} hold_by=-"
        ))
        .and_then(|(need, _)| need)
        .expect("its need")
        .carries_on
    };
    assert!(carries_on("harness"));
    assert!(!carries_on("-"), "CONTROL: a direct ask");
    assert!(!carries_on("program"), "CONTROL: the program's own report");
}

/// A scripted sibling over a real Unix socket serving AUTH and `survey
/// facts` — the whole dial in ONE read: its live sessions are counted,
/// its published needs travel, an exited row is not counted.
#[cfg(unix)]
#[test]
fn summarize_peer_reads_a_scripted_sibling() {
    use std::io::{BufRead, BufReader, Write};
    for (needs, published) in [(" needs=ask", true), ("", false)] {
        let scratch = crate::test_scratch::short_dir("the peer's socket");
        let dir = scratch.path();
        let sock = dir.join("peer.sock");
        std::fs::write(dir.join("aterm.token"), "feedbeef\n").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let accounts: String = (0..8192)
            .map(|i| format!("account cli=codex label=account-{i} condition=usable\n"))
            .collect();
        let reply = format!(
            "OK {}\ninstance pid=9 host=h version=1 sessions=3 level=facts cut=0\n\
             session local=0 sid=s-op state=alive program=zsh role=- attention=1{needs}\n\
             session local=1 sid=s-w state=alive program=zsh role=- attention=-{}\n\
             session local=2 sid=s-x state=exited program=zsh role=- attention=-{needs}\n{accounts}",
            4 + 8192,
            if published { " needs=-" } else { "" },
        );
        let server = std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH feedbeef\n");
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "survey facts\n");
            let mut w = conn.try_clone().unwrap();
            w.write_all(reply.as_bytes()).unwrap();
            line.clear();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0, "{line:?}");
        });
        let row = super::summarize_peer_within(
            4242,
            sock.to_str().unwrap(),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("summarized");
        server.join().unwrap();
        assert_eq!(row.pid, 4242);
        assert_eq!(row.sessions, 2, "exited row excluded");
        assert_eq!(row.needs.len(), usize::from(published), "{needs:?}");
    }
}
