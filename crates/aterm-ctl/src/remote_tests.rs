// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Cursor, Read};
use std::sync::Mutex;

use super::*;

const OWN: Within = Within::Drive(DRIVE_WITHIN);

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

struct Scripted {
    sent: Vec<u8>,
    reply: Option<Cursor<Vec<u8>>>,
    answer: fn(&str) -> String,
    seen: &'static Mutex<Vec<String>>,
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.reply.is_none() {
            let sent = String::from_utf8_lossy(&self.sent).into_owned();
            self.seen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(sent.trim_end().to_owned());
            self.reply = Some(Cursor::new((self.answer)(&sent).into_bytes()));
        }
        self.reply.as_mut().map_or(Ok(0), |reply| reply.read(buf))
    }
}

impl Write for Scripted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sent.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn link(answer: fn(&str) -> String, seen: &'static Mutex<Vec<String>>) -> DriveLink {
    DriveLink {
        stream: Box::new(Scripted {
            sent: Vec::new(),
            reply: None,
            answer,
            seen,
        }),
        label: "lab-mini".to_owned(),
    }
}

#[test]
fn a_drive_names_each_session_by_its_sid() {
    assert!(refuse_local_selector(&words("@s-a1 status")).is_ok());
    assert!(refuse_local_selector(&words("hold s-a1 on")).is_ok());
    for local in ["@self", "@env", "@.", "@3", "@s-a1,@s-b2", "@s-", "@s-zz"] {
        let refusal = refuse_local_selector(&words(&format!("{local} status")))
            .expect_err(local)
            .to_string();
        assert!(refusal.contains("by its sid"), "{local}: {refusal}");
    }
}

fn turn_sent(parts: &[&str]) -> io::Result<String> {
    let parts: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    let (selector, rest) = split_selector(&parts);
    turn_wait(&rest[1..], selector, "lab-mini")
}

#[test]
fn a_turn_over_a_drive_waits_inside_its_connection() {
    let sent = |line: &str| turn_sent(&line.split_whitespace().collect::<Vec<_>>()).unwrap();
    assert_eq!(
        sent("@s-a1 turn fix the test"),
        "timeout=20000 fix the test"
    );
    assert_eq!(
        sent("@s-a1 turn settle=submitted timeout=15000 go"),
        "settle=submitted timeout=15000 go"
    );
    assert_eq!(
        sent("@s-a1 turn set timeout=90000 in the config"),
        "timeout=20000 set timeout=90000 in the config"
    );
    let refusal = turn_sent(&["@s-a1", "turn", "timeout=60000", "go"])
        .expect_err("past the cap")
        .to_string();
    assert!(
        refusal.contains("give it a shorter timeout=, or say settle=submitted")
            && refusal.contains("`aterm ctl --host lab-mini await task @s-a1:<task_rev>`")
            && refusal.contains("`aterm ctl --host lab-mini @s-a1 status` (or `text`)"),
        "{refusal}"
    );
}

#[test]
fn a_turns_timeout_over_a_drive_is_the_one_the_driven_mac_reads() {
    for (parts, wire) in [
        (
            &["@s-a1", "turn", "--", "timeout=5000 echo T2"][..],
            "-- timeout=5000 echo T2",
        ),
        (
            &["@s-a1", "turn", "timeout=1 x", "timeout=99999"][..],
            "timeout=1 x timeout=99999",
        ),
        (
            &["@s-a1", "turn", "timeout=99999", "timeout=5000", "go"][..],
            "timeout=99999 timeout=5000 go",
        ),
        (
            &["@s-a1", "turn", "timeout=abc", "go"][..],
            "timeout=abc go",
        ),
        (
            &["@s-a1", "turn", "--", "--", "timeout=99999"][..],
            "-- timeout=20000 -- timeout=99999",
        ),
        (
            &["@s-a1", "turn", "idle=5", "--", "timeout=99999"][..],
            "timeout=20000 idle=5 -- timeout=99999",
        ),
    ] {
        assert_eq!(turn_sent(parts).unwrap(), wire, "{parts:?}");
    }
    let refusal = turn_sent(&["@s-a1", "turn", "timeout=5000", "timeout=60000", "go"])
        .expect_err("the last timeout= is the one read")
        .to_string();
    assert!(refusal.contains("at most timeout=22000"), "{refusal}");
}

#[test]
fn a_turns_leading_options_keep_their_place_over_a_drive() {
    for (sent, wire) in [
        (
            "@s-a1 turn id=7f:3:41 run the tests",
            "id=7f:3:41 timeout=20000 run the tests",
        ),
        (
            "@s-a1 turn if-supervisor=h-9 idle=600 go",
            "if-supervisor=h-9 timeout=20000 idle=600 go",
        ),
        (
            "@s-a1 turn cause=@12 id=k:1:2 go",
            "cause=@12 id=k:1:2 timeout=20000 go",
        ),
        (
            "@s-a1 turn id=k:1:2 settle=submitted timeout=9000 go",
            "id=k:1:2 settle=submitted timeout=9000 go",
        ),
        (
            "@s-a1 turn -- id=k literal",
            "-- timeout=20000 id=k literal",
        ),
        ("@s-a1 turn id=k", "id=k timeout=20000"),
    ] {
        let parts: Vec<&str> = sent.split_whitespace().collect();
        assert_eq!(turn_sent(&parts).unwrap(), wire, "{sent}");
    }
    for long in [
        "@s-a1 turn id=k:1:2 timeout=60000 go",
        "@s-a1 turn if-supervisor=h-9 id=k timeout=60000 go",
    ] {
        let refusal = turn_sent(&long.split_whitespace().collect::<Vec<_>>())
            .expect_err(long)
            .to_string();
        assert!(
            refusal.contains("at most timeout=22000"),
            "{long}: {refusal}"
        );
    }
}

#[test]
fn a_spawn_over_a_drive_names_no_folder_of_this_mac() {
    assert_eq!(
        spawn_head(&words("agent=claude model=haiku -"), Some("s-m1")).unwrap(),
        "spawn agent=claude model=haiku from=s-m1"
    );
    assert_eq!(
        spawn_head(
            &[
                "agent=claude".to_owned(),
                "cwd=/Users//a/My Project".to_owned(),
                "from=s-x9".to_owned(),
                "-".to_owned()
            ],
            Some("s-m1")
        )
        .unwrap(),
        "spawn agent=claude cwd=\"/Users//a/My Project\" from=s-x9"
    );
    for (bad, why) in [
        ("agent=claude", "end the request with -"),
        ("model=haiku -", "a worker alone"),
        ("", "a worker alone"),
        ("-e /bin/sh -c true", "a worker alone"),
        ("agent=claude len=4 -", "len="),
        ("agent=claude cwd=proj -", "absolute path"),
    ] {
        let refusal = spawn_head(&words(bad), None).expect_err(bad).to_string();
        assert!(refusal.contains(why), "{bad}: {refusal}");
    }
}

#[test]
fn what_never_crosses_the_fleet_is_refused_here() {
    let none = run("lab-mini", &words("@s-a1 status"), None, &LocalVerbs::NONE)
        .expect_err("no fleet client")
        .to_string();
    assert!(none.contains("does not link"), "{none}");
    fn unreachable(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        panic!("dialled")
    }
    let local = LocalVerbs {
        fleet_drive: Some(unreachable),
        ..LocalVerbs::NONE
    };
    for frame in [
        "@s-a1 send --stdin",
        "@s-a1 send -",
        "@s-a1 paste -",
        "@s-a1 feed-bin",
    ] {
        let refusal = run("lab-mini", &words(frame), None, &local)
            .expect_err(frame)
            .to_string();
        assert!(refusal.contains("inline text alone"), "{frame}: {refusal}");
    }
    let aimed = run(
        "lab-mini",
        &words("@s-a1 spawn agent=claude -"),
        None,
        &local,
    )
    .expect_err("aimed spawn")
    .to_string();
    assert!(aimed.contains("name no @<sel>"), "{aimed}");
}

#[test]
fn a_survey_over_a_host_that_names_its_own_machines_is_refused() {
    for (line, word) in [
        ("survey --fleet", "--fleet"),
        ("survey --host c93a8940", "--host c93a8940"),
        ("survey --json --host=c93a8940", "--host=c93a8940"),
    ] {
        let refusal = run("93bfb599", &words(line), None, &LocalVerbs::NONE)
            .expect_err(line)
            .to_string();
        assert_eq!(
            refusal,
            format!(
                "--host 93bfb599 and survey's own `{word}` each name the machines a survey reads: \
                 give one (`aterm ctl survey --host <h>` reads one peer, `aterm ctl survey \
                 --fleet` every one)"
            ),
            "{line}"
        );
    }
}

#[test]
fn a_hold_off_after_a_leave_names_what_lifts_the_hold() {
    fn left(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Err(DriveRefused {
            left: Some(Left::Fleet("496e9154e57ae1d4bb20b01cf5d624cf".to_owned())),
            ..DriveRefused::once("this machine is in no fleet".to_owned())
        })
    }
    let local = LocalVerbs {
        fleet_drive: Some(left),
        ..LocalVerbs::NONE
    };
    let off = run("lab-mini", &words("hold s-a1 off"), None, &local)
        .expect_err("no fleet")
        .to_string();
    assert!(
        off.starts_with("this machine is in no fleet; if this machine took s-a1 on lab-mini, ")
            && off.ends_with(
                "`aterm ctl --host lab-mini hold s-a1 off` from here works once this machine is \
                 in that fleet again (a fleet it issued went with its leave)"
            ),
        "{off}"
    );
    assert!(!off.contains("stands until"), "{off}");
    let at = run("lab-mini", &words("hold @s-a1 off"), None, &local)
        .expect_err("no fleet")
        .to_string();
    assert_eq!(at, off, "the remedy names the bare sid a hold takes");
    for other in ["hold s-a1 on", "@s-a1 status"] {
        let said = run("lab-mini", &words(other), None, &local)
            .expect_err(other)
            .to_string();
        assert_eq!(said, "this machine is in no fleet", "{other}");
    }
    fn unbooked(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Err(DriveRefused {
            left: Some(Left::Book("496e9154e57ae1d4bb20b01cf5d624cf".to_owned())),
            ..DriveRefused::once("`lab-mnii` is no Mac this machine's book records".to_owned())
        })
    }
    let local = LocalVerbs {
        fleet_drive: Some(unbooked),
        ..LocalVerbs::NONE
    };
    let off = run("lab-mnii", &words("hold s-a1 off"), None, &local)
        .expect_err("no such Mac in the book")
        .to_string();
    assert!(
        off.starts_with(
            "`lab-mnii` is no Mac this machine's book records; if this machine took s-a1 on \
             lab-mnii, "
        ) && off.ends_with(
            "`aterm ctl --host lab-mnii hold s-a1 off` from here works once this machine's book \
             records that Mac again"
        ),
        "{off}"
    );
    let on = run("lab-mnii", &words("hold s-a1 on"), None, &local)
        .expect_err("no such Mac in the book")
        .to_string();
    assert_eq!(on, "`lab-mnii` is no Mac this machine's book records");
}

#[test]
fn an_exchange_is_framed_as_on_one_mac() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn answer(sent: &str) -> String {
        match sent.split_whitespace().nth(1) {
            Some("close") => "ERR denied: a drive closes only a worker it spawned\n".to_owned(),
            Some("turn") => "OK 1 turn submitted=1 status=timeout seq=4\n❯ go\n".to_owned(),
            _ => "OK 2\nrow one\nrow two\n".to_owned(),
        }
    }
    let ran = |line: &str, verb: &str| match exchange(link(answer, &SEEN), line, verb, OWN).unwrap()
    {
        Exchange::Completed(code) => code,
        Exchange::Busy => panic!("the fixture is not busy"),
    };
    assert_eq!(ran("@s-a1 close\n", "close"), ExitCode::FAILURE);
    assert_eq!(
        ran("@s-a1 turn timeout=20000 go\n", "turn"),
        ExitCode::from(EXIT_TIMEOUT)
    );
    assert_eq!(ran("@s-a1 text\n", "text"), ExitCode::SUCCESS);
    assert_eq!(
        *SEEN.lock().unwrap(),
        ["@s-a1 close", "@s-a1 turn timeout=20000 go", "@s-a1 text"]
    );
}

#[test]
fn a_busy_fleet_handoff_retries_under_one_deadline() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn busy(_: &str) -> String {
        aterm_types::control_verbs::CONTROL_BUSY_LINE.to_owned()
    }
    fn ready(_: &str) -> String {
        "OK delivered=1\n".to_owned()
    }
    SEEN.lock().unwrap().clear();
    let budget = Duration::from_secs(60);
    let mut attempts = Vec::new();
    let code = exchange_with_retry(
        "@s-a1 post key=k kind=note -- once\n",
        "post",
        Within::Drive(budget),
        |remaining| {
            attempts.push(remaining);
            Ok(link(if attempts.len() == 1 { busy } else { ready }, &SEEN))
        },
    )
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], budget);
    assert!(
        attempts[1] < attempts[0],
        "the redial keeps the original deadline"
    );
    assert_eq!(
        *SEEN.lock().unwrap(),
        [
            "@s-a1 post key=k kind=note -- once",
            "@s-a1 post key=k kind=note -- once"
        ]
    );
}

#[test]
fn a_fleet_retry_requires_the_exact_complete_predispatch_busy_reply() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn denied(_: &str) -> String {
        "ERR denied\n".into()
    }
    fn other_busy(_: &str) -> String {
        "ERR control server busy; retry later\n".into()
    }
    fn truncated(_: &str) -> String {
        aterm_types::control_verbs::CONTROL_BUSY_REPLY.into()
    }
    fn surplus(_: &str) -> String {
        format!(
            "{}unexpected body\n",
            aterm_types::control_verbs::CONTROL_BUSY_LINE
        )
    }
    fn eof(_: &str) -> String {
        String::new()
    }
    for answer in [
        denied as fn(&str) -> String,
        other_busy,
        truncated,
        surplus,
        eof,
    ] {
        SEEN.lock().unwrap().clear();
        let mut calls = 0;
        let result =
            exchange_with_retry("@s-a1 post key=k kind=note -- once\n", "post", OWN, |_| {
                calls += 1;
                Ok(link(answer, &SEEN))
            });
        assert_eq!(
            calls, 1,
            "only a full exact Busy refusal may repeat the request"
        );
        assert!(!matches!(result, Ok(code) if code == ExitCode::SUCCESS));
    }
}

#[test]
fn busy_does_not_extend_an_expired_fleet_deadline() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn busy(_: &str) -> String {
        aterm_types::control_verbs::CONTROL_BUSY_LINE.into()
    }
    let mut calls = 0;
    let error = exchange_with_retry(
        "@s-a1 inbox\n",
        "inbox",
        Within::Drive(Duration::ZERO),
        |_| {
            calls += 1;
            Ok(link(busy, &SEEN))
        },
    )
    .unwrap_err();
    assert_eq!(calls, 1);
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("request was not dispatched"));
}

#[test]
fn a_partial_fleet_send_is_never_repeated() {
    struct PartialSend(bool);
    impl Write for PartialSend {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.0, false) {
                Ok(1)
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Read for PartialSend {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("a failed send reaches no reply")
        }
    }
    let mut calls = 0;
    assert!(
        exchange_with_retry("@s-a1 send once\n", "send", OWN, |_| {
            calls += 1;
            Ok(DriveLink {
                stream: Box::new(PartialSend(true)),
                label: "lab-mini".into(),
            })
        })
        .is_err()
    );
    assert_eq!(calls, 1);
}

#[test]
fn a_workers_task_crosses_only_after_go() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn go(sent: &str) -> String {
        let len = sent
            .split_whitespace()
            .find_map(|w| w.strip_prefix("len="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        format!(
            "{}OK s-w1 task_rev=1\n",
            aterm_types::control_verbs::task_frame_go(len)
        )
    }
    fn deny(_: &str) -> String {
        "ERR denied: a drive's worker opens raise=false\n".to_owned()
    }
    let task = b"read the README\nand say one line";
    let head = format!("spawn agent=claude len={}", task.len());
    assert_eq!(
        spawn_exchange(link(go, &SEEN), &head, task, OWN).unwrap(),
        (ExitCode::SUCCESS, None)
    );
    assert_eq!(
        spawn_exchange(link(deny, &SEEN), &head, task, OWN).unwrap(),
        (ExitCode::FAILURE, None)
    );
    let seen = SEEN.lock().unwrap();
    assert_eq!(*seen, [head.clone(), head]);
}

#[test]
fn an_await_over_a_drive_waits_in_short_legs() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn worker(sent: &str) -> String {
        if sent.contains("timeout=0 ") || sent.trim_end().ends_with("timeout=0") {
            "OK 1 status=timeout\n@s-a1 rev=3 state=working agent=busy attention=- \
             result_bytes=0\n"
                .to_owned()
        } else {
            "OK 1\n@s-a1 rev=4 state=done agent=idle attention=- result_bytes=12\n".to_owned()
        }
    }
    fn drive(host: &str, within: Duration) -> Result<DriveLink, DriveRefused> {
        assert_eq!(host, "lab-mini");
        assert!(within <= Duration::from_secs(28), "{within:?}");
        Ok(link(worker, &SEEN))
    }
    let refusal = await_task::run_driven(&words("await task @3"), None, "lab-mini", drive)
        .expect_err("a local number")
        .to_string();
    assert!(refusal.contains("by its sid"), "{refusal}");
    let code = await_task::run_driven(
        &words("await task @s-a1 timeout=120000"),
        None,
        "lab-mini",
        drive,
    )
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    let seen = SEEN.lock().unwrap();
    assert_eq!(
        *seen,
        [
            "await task @s-a1 any timeout=0",
            "await task @s-a1:3 any timeout=20000"
        ]
    );
    drop(seen);
    SEEN.lock().unwrap().clear();
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        ..LocalVerbs::default()
    };
    let code = run(
        "lab-mini",
        &words("@s-a1:0 await task timeout=18446744073709551615"),
        Some(Some(Duration::from_secs(u64::MAX))),
        &local,
    )
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        *SEEN.lock().unwrap(),
        [
            "await task @s-a1:0 any timeout=0",
            "await task @s-a1:0 any timeout=20000"
        ]
    );
}

struct Cut;

impl Read for Cut {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed connection without sending TLS close_notify: https://docs.rs/rustls",
        ))
    }
}

impl Write for Cut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_drive_cut_before_its_answer_says_what_cuts_one_and_what_may_have_landed() {
    let cut = || DriveLink {
        stream: Box::new(Cut),
        label: "lab-mini".to_owned(),
    };
    let turned = exchange(cut(), "@s-a1 turn timeout=20000 go\n", "turn", OWN)
        .expect_err("cut")
        .to_string();
    assert!(
        turned.starts_with(
            "lab-mini cut the drive connection before it answered (its person took back"
        ) && turned.contains("lowered its fleet level, its fleet key or listen place changed, or")
            && turned.contains(
                "what this `turn` sent may have landed: `aterm ctl --host lab-mini @s-a1 status`"
            )
            && !turned.contains("docs.rs"),
        "{turned}"
    );
    let held = exchange(cut(), "hold s-a1 on\n", "hold", OWN)
        .expect_err("cut")
        .to_string();
    assert!(
        held.contains("`aterm ctl --host lab-mini @s-a1 status`"),
        "{held}"
    );
    let read = exchange(cut(), "@s-a1 text\n", "text", OWN)
        .expect_err("cut")
        .to_string();
    assert!(
        read.contains("`aterm ctl --host lab-mini @s-a1 text …` again says which")
            && !read.contains("landed"),
        "{read}"
    );
    let surveyed = exchange(cut(), "survey\n", "survey", OWN)
        .expect_err("cut")
        .to_string();
    assert!(
        surveyed.contains("`aterm ctl --host lab-mini survey …` again says which"),
        "{surveyed}"
    );
    let spawned = spawn_exchange(cut(), "spawn agent=claude len=2", b"go", OWN)
        .expect_err("cut")
        .to_string();
    assert!(spawned.contains("the worker may have started"), "{spawned}");
}

struct Frozen;

impl Read for Frozen {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
}

impl Write for Frozen {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_drive_whose_answer_never_comes_says_which_peer_and_what_may_have_landed() {
    let frozen = || DriveLink {
        stream: Box::new(Frozen),
        label: "lab-mini".to_owned(),
    };
    let turned =
        exchange(frozen(), "@s-a1 turn timeout=20000 go\n", "turn", OWN).expect_err("unanswered");
    assert_eq!(turned.kind(), io::ErrorKind::WouldBlock, "exit 124 stays");
    let turned = turned.to_string();
    assert!(
        turned.starts_with("lab-mini did not answer within 28 s (its aterm is stopped, frozen")
            && turned.contains(
                "what this `turn` sent may have landed: `aterm ctl --host lab-mini @s-a1 status`"
            )
            && !turned.contains("os error"),
        "{turned}"
    );
    let read = exchange(frozen(), "@s-a1 text\n", "text", OWN)
        .expect_err("unanswered")
        .to_string();
    assert!(
        read.contains("`aterm ctl --host lab-mini @s-a1 text …` again is safe")
            && !read.contains("landed"),
        "{read}"
    );
    let spawned = spawn_exchange(frozen(), "spawn agent=claude len=2", b"go", OWN)
        .expect_err("unanswered")
        .to_string();
    assert!(
        spawned.starts_with("lab-mini did not answer") && spawned.contains("may have started"),
        "{spawned}"
    );
    let reader_gone = io::Error::new(io::ErrorKind::BrokenPipe, crate::StdoutClosed);
    assert!(
        crate::is_stdout_closed(&closed("lab-mini", "@s-a1 text\n", OWN, reader_gone)),
        "a closed stdout is no cut of the drive"
    );
    fn drive(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Ok(DriveLink {
            stream: Box::new(Frozen),
            label: "lab-mini".to_owned(),
        })
    }
    let awaited = await_task::run_driven(&words("await task @s-a1"), None, "lab-mini", drive)
        .expect_err("unanswered");
    assert_eq!(awaited.kind(), io::ErrorKind::WouldBlock, "exit 124 stays");
    let awaited = awaited.to_string();
    assert!(
        awaited.starts_with("lab-mini did not answer within 8 s")
            && awaited.contains("`aterm ctl --host lab-mini await …` again is safe"),
        "{awaited}"
    );
}

#[test]
fn a_reply_cut_at_a_shorter_timeout_is_said_as_that_timeout_not_the_peers() {
    fn drive(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Ok(DriveLink {
            stream: Box::new(Frozen),
            label: "lab-mini".to_owned(),
        })
    }
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        ..LocalVerbs::NONE
    };
    let five = Some(Some(Duration::from_secs(5)));
    let turned = run("lab-mini", &words("@s-a1 turn go"), five, &local).expect_err("unanswered");
    assert_eq!(turned.kind(), io::ErrorKind::WouldBlock, "exit 124 stays");
    let turned = turned.to_string();
    assert!(
        turned.starts_with(
            "no answer came from lab-mini within this call's --timeout of 5 s (a `turn` there \
             answers once it settles or its timeout= runs out, 20000 ms where it names none; a \
             longer --timeout, or 0, waits up to the drive's own 28 s), so what this `turn` sent \
             may have landed"
        ) && !turned.contains("frozen"),
        "{turned}"
    );
    let read = run("lab-mini", &words("@s-a1 status"), five, &local)
        .expect_err("unanswered")
        .to_string();
    assert!(
        read.starts_with(
            "no answer came from lab-mini within this call's --timeout of 5 s (a longer"
        ) && read.contains("`aterm ctl --host lab-mini @s-a1 status …` again is safe"),
        "{read}"
    );
    let spawned = spawn_exchange(
        drive("lab-mini", DRIVE_WITHIN).expect("a link"),
        "spawn agent=claude len=2",
        b"go",
        Within::Timeout(Duration::from_secs(5), Longer::Drive(DRIVE_WITHIN)),
    )
    .expect_err("unanswered")
    .to_string();
    assert!(
        spawned.starts_with("no answer came from lab-mini within this call's --timeout of 5 s")
            && spawned.contains("may have started"),
        "{spawned}"
    );
    for total in [None, Some(None), Some(Some(Duration::from_secs(60)))] {
        let own = run("lab-mini", &words("@s-a1 turn go"), total, &local)
            .expect_err("unanswered")
            .to_string();
        assert!(
            own.starts_with("lab-mini did not answer within 28 s (its aterm is stopped"),
            "{total:?}: {own}"
        );
    }
}

#[test]
fn a_handshake_that_never_comes_waits_the_callers_deadline_and_exits_2() {
    static ASKED: Mutex<Vec<Duration>> = Mutex::new(Vec::new());
    fn frozen(_: &str, within: Duration) -> Result<DriveLink, DriveRefused> {
        ASKED.lock().unwrap_or_else(|p| p.into_inner()).push(within);
        Err(DriveRefused {
            unserved: true,
            ..DriveRefused::once(format!(
                "127.0.0.1:7100: connected, but no fleet handshake within {:.1}s",
                within.as_secs_f64()
            ))
        })
    }
    let local = LocalVerbs {
        fleet_drive: Some(frozen),
        ..LocalVerbs::NONE
    };
    let five = Duration::from_secs(5);
    for (total, within) in [
        (Some(Some(five)), five),
        (None, DRIVE_WITHIN),
        (Some(None), DRIVE_WITHIN),
        (Some(Some(Duration::from_secs(60))), DRIVE_WITHIN),
    ] {
        let refused =
            run("lab-mini", &words("@s-a1 status"), total, &local).expect_err("never served");
        assert_eq!(crate::exit_for(&refused), ExitCode::from(2), "{refused}");
        assert_eq!(
            ASKED.lock().unwrap().pop(),
            Some(within),
            "{total:?}: {refused}"
        );
    }
    for (total, most) in [
        (Some(Some(five)), five),
        (None, Duration::from_secs(8)),
        (Some(Some(Duration::from_secs(60))), Duration::from_secs(8)),
    ] {
        let awaited = run(
            "lab-mini",
            &words("await task @s-a1 timeout=2000"),
            total,
            &local,
        )
        .expect_err("never served");
        assert_eq!(crate::exit_for(&awaited), ExitCode::from(2), "{awaited}");
        let asked = ASKED.lock().unwrap().pop().expect("a dial");
        assert!(asked <= most, "{total:?}: {asked:?}");
        if total == Some(Some(five)) {
            assert!(asked < Duration::from_secs(8), "{asked:?}");
        }
    }
    fn cut(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Err(DriveRefused::once(
            "127.0.0.1:7100: no connection within 5.0s".to_owned(),
        ))
    }
    let local = LocalVerbs {
        fleet_drive: Some(cut),
        ..LocalVerbs::NONE
    };
    let refused = run("lab-mini", &words("@s-a1 status"), None, &local).expect_err("refused");
    assert_eq!(crate::exit_for(&refused), ExitCode::FAILURE, "{refused}");
}

#[test]
fn an_await_over_a_drive_ends_within_ctls_own_timeout() {
    static ASKED: Mutex<Vec<Duration>> = Mutex::new(Vec::new());
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn baseline(_: &str) -> String {
        "OK 1 status=timeout\n@s-a1 rev=3 state=working agent=busy attention=- result_bytes=0\n"
            .to_owned()
    }
    fn drive(_: &str, within: Duration) -> Result<DriveLink, DriveRefused> {
        let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
        asked.push(within);
        Ok(if asked.len() == 1 {
            link(baseline, &SEEN)
        } else {
            DriveLink {
                stream: Box::new(Frozen),
                label: "lab-mini".to_owned(),
            }
        })
    }
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        ..LocalVerbs::NONE
    };
    let five = Duration::from_secs(5);
    let code = run(
        "lab-mini",
        &words("await task @s-a1 timeout=600000"),
        Some(Some(five)),
        &local,
    )
    .expect("the rows read");
    assert_eq!(
        code,
        ExitCode::from(crate::EXIT_REPLACED),
        "the leg cut was never served"
    );
    let asked = ASKED.lock().unwrap();
    assert_eq!(asked.len(), 2, "a baseline and one leg: {asked:?}");
    assert!(asked.iter().all(|within| *within <= five), "{asked:?}");
    let seen = SEEN.lock().unwrap();
    assert_eq!(*seen, ["await task @s-a1 any timeout=0"]);
}

/// The survey `--host any` reads in these tests: m1 lets this Mac drive it and has a weekly
/// figure of 50%, m2 has the fleet's freshest account but grants no drive, and m3 lets this Mac
/// drive it with 20%.
fn any_fleet(ask: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
    assert_eq!(ask.host, None, "every member is read");
    let peer = |n: u8, drive: bool, week: u8| survey::PeerSurvey {
        label: format!("m{n}.local"),
        answer: Ok(vec![
            format!(
                "instance pid=9 host=m{n}.local version=0.146.0 sessions=0 level=facts \
                 at_ms=1793130000000 accounts=1{}",
                if drive { " drive=1" } else { "" }
            ),
            format!(
                "account pool=claude id=main condition=usable enrolled=1 pct5=1 pct7={week} \
                 week_reset=- walled_until=- last_used=-"
            ),
        ]),
        fault: survey::PeerFault::Other,
        unproven: false,
        node: Some(format!("{n:02x}").repeat(16)),
        addr: Some(format!("192.168.1.{n}")),
    };
    Ok(survey::FleetRead {
        peers: vec![peer(1, true, 50), peer(2, false, 0), peer(3, true, 20)],
        note: None,
        alone: None,
    })
}

#[test]
fn host_any_spawns_on_the_best_member_that_lets_this_mac_drive_it() {
    fn never(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        panic!("dialled")
    }
    let local = LocalVerbs {
        fleet_survey: Some(any_fleet),
        fleet_drive: Some(never),
        ..LocalVerbs::NONE
    };
    assert_eq!(
        any_host_node(&words("agent=claude -"), None, &local).expect("a pick"),
        "03".repeat(16),
        "m3, never m2, which grants no drive"
    );
    let none = any_host_node(&words("agent=codex -"), None, &local)
        .expect_err("no codex row")
        .to_string();
    assert!(
        none.starts_with("--host any: no Mac can take agent=codex: 3 members answered, 2 of them"),
        "{none}"
    );
}

/// A `--host any` spawn is one of a Claude Code or Codex worker, refused before any survey, and
/// a survey is never sent to whichever Mac.
#[test]
fn host_any_spawns_a_claude_or_codex_worker_alone() {
    fn never(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        panic!("dialled")
    }
    fn unasked(_: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
        panic!("surveyed")
    }
    let local = LocalVerbs {
        fleet_survey: Some(unasked),
        fleet_drive: Some(never),
        ..LocalVerbs::NONE
    };
    for (line, why) in [
        ("survey", "run `aterm ctl survey --fleet`"),
        ("@s-a1 spawn agent=claude -", "name no @<sel>"),
        ("spawn agent=agy -", "a Claude Code or Codex worker"),
        ("spawn -", "spawns a worker alone"),
        ("spawn agent=claude", "end the request with -"),
        ("spawn agent=claude cwd=rel -", "absolute path"),
    ] {
        let refusal = run(ANY_TEST, &words(line), None, &local)
            .expect_err(line)
            .to_string();
        assert!(refusal.contains(why), "{line}: {refusal}");
    }
    let unlinked = run(
        ANY_TEST,
        &words("spawn agent=claude -"),
        None,
        &LocalVerbs::NONE,
    )
    .expect_err("no fleet client");
    assert_eq!(unlinked.kind(), io::ErrorKind::Unsupported, "{unlinked}");
}

const ANY_TEST: &str = any_host::ANY;

/// A worker `--host any` spawned is followed to its Mac by its sid: from the table the spawn
/// recorded, else from the one live, proven member whose survey lists it.
#[test]
fn host_any_follows_a_spawned_workers_sid_to_its_mac() {
    static DIALLED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let tmp = aterm_tempfile::tempdir().unwrap();
    let any_table = tmp.path().join("driven-any-hosts.tsv");
    ANY_HOSTS_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(any_table.clone()));

    fn fleet_survey(_: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
        Ok(survey::FleetRead {
            peers: vec![
                // An unproven member's word for where a sid runs is never taken.
                survey::PeerSurvey {
                    label: "m-unproven".into(),
                    answer: Ok(vec![
                        "instance host=m-unproven drive=1 sessions=1".into(),
                        "session sid=s-f00d agent=idle".into(),
                    ]),
                    fault: survey::PeerFault::Other,
                    unproven: true,
                    node: None,
                    addr: None,
                },
                // An entry with no node (a peer named by hand) cannot be driven.
                survey::PeerSurvey {
                    label: "m-by-hand at 192.168.1.9".into(),
                    answer: Ok(vec![
                        "instance host=m-by-hand drive=1 sessions=1".into(),
                        "session sid=s-f00d agent=idle".into(),
                    ]),
                    fault: survey::PeerFault::Other,
                    unproven: false,
                    node: None,
                    addr: Some("192.168.1.9".into()),
                },
                survey::PeerSurvey {
                    label: "m4.local at 192.168.1.4".into(),
                    answer: Ok(vec![
                        "instance host=m4 drive=1 sessions=1".into(),
                        "session sid=s-f00d agent=idle".into(),
                    ]),
                    fault: survey::PeerFault::Other,
                    unproven: false,
                    node: Some("node-m4".into()),
                    addr: Some("192.168.1.4".into()),
                },
            ],
            note: None,
            alone: None,
        })
    }

    fn drive(host: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        let peer = host.to_owned();
        struct PeerReply {
            peer: String,
            sent: Vec<u8>,
            reply: Option<Cursor<Vec<u8>>>,
        }
        impl Read for PeerReply {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.reply.is_none() {
                    let req = String::from_utf8_lossy(&self.sent).trim_end().to_owned();
                    DIALLED
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push((self.peer.clone(), req.clone()));
                    let text = if req.starts_with("await task") {
                        "OK 1\n@s-c25a rev=2 state=done agent=idle attention=- result_bytes=5\n"
                            .to_owned()
                    } else if req.ends_with(" result") {
                        "OK 1\nNothing left: verified.\n".to_owned()
                    } else {
                        "OK status\n".to_owned()
                    };
                    self.reply = Some(Cursor::new(text.into_bytes()));
                }
                self.reply.as_mut().map_or(Ok(0), |r| r.read(buf))
            }
        }
        impl Write for PeerReply {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.sent.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        Ok(DriveLink {
            stream: Box::new(PeerReply {
                peer,
                sent: Vec::new(),
                reply: None,
            }),
            label: host.to_owned(),
        })
    }

    // Record spawned sid -> m3 and verify `await task` and `@s-c25a result` follow m3, while an
    // unrecorded sid `@s-f00d` falls back to `fleet_survey` and routes to `m4`.
    record_any_sid_host("s-c25a", "m3");
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        fleet_survey: Some(fleet_survey),
        ..LocalVerbs::NONE
    };
    assert_eq!(
        run("any", &words("await task @s-c25a since=0"), None, &local).unwrap(),
        ExitCode::SUCCESS
    );
    assert_eq!(
        run("any", &words("@s-c25a result"), None, &local).unwrap(),
        ExitCode::SUCCESS
    );
    assert_eq!(
        run("any", &words("@s-f00d status"), None, &local).unwrap(),
        ExitCode::SUCCESS
    );

    let dialled = DIALLED.lock().unwrap().clone();
    assert_eq!(
        dialled,
        vec![
            (
                "m3".to_owned(),
                "await task @s-c25a:0 any timeout=0".to_owned()
            ),
            ("m3".to_owned(), "@s-c25a result".to_owned()),
            ("node-m4".to_owned(), "@s-f00d status".to_owned()),
        ]
    );

    let no_survey = resolve_any_sid_hosts(&["s-dead"], None, None).expect_err("no survey_fn");
    assert_eq!(no_survey.kind(), io::ErrorKind::Unsupported);

    fn unserved_survey(
        _: &survey::FleetAsk<'_>,
    ) -> Result<survey::FleetRead, survey::FleetRefused> {
        Err(survey::FleetRefused {
            why: "issuer connected, but no fleet handshake within 3.0s".into(),
            unserved: true,
        })
    }
    let unserved_err =
        resolve_any_sid_hosts(&["s-dead"], None, Some(unserved_survey)).expect_err("unserved");
    assert_eq!(crate::exit_for(&unserved_err), ExitCode::from(2));

    ANY_HOSTS_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
}
