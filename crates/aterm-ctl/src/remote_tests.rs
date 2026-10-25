// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Cursor, Read};
use std::sync::Mutex;

use super::*;

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

#[test]
fn a_turn_over_a_drive_waits_inside_its_connection() {
    let mut bare = words("@s-a1 turn fix the test");
    turn_wait(&mut bare).unwrap();
    assert_eq!(bare, words("@s-a1 turn timeout=20000 fix the test"));
    let mut named = words("@s-a1 turn settle=submitted timeout=15000 go");
    turn_wait(&mut named).unwrap();
    assert_eq!(named, words("@s-a1 turn settle=submitted timeout=15000 go"));
    let mut in_text = words("@s-a1 turn set timeout=90000 in the config");
    turn_wait(&mut in_text).unwrap();
    assert_eq!(in_text[2], "timeout=20000", "{in_text:?}");
    for long in ["timeout=60000", "timeout=soon"] {
        let refusal = turn_wait(&mut words(&format!("@s-a1 turn {long} go")))
            .expect_err(long)
            .to_string();
        assert!(refusal.contains("settle=submitted"), "{long}: {refusal}");
    }
}

#[test]
fn a_turns_leading_options_keep_their_place_over_a_drive() {
    for (sent, wire) in [
        (
            "@s-a1 turn id=7f:3:41 run the tests",
            "@s-a1 turn id=7f:3:41 timeout=20000 run the tests",
        ),
        (
            "@s-a1 turn if-supervisor=h-9 idle=600 go",
            "@s-a1 turn if-supervisor=h-9 timeout=20000 idle=600 go",
        ),
        (
            "@s-a1 turn cause=@12 id=k:1:2 go",
            "@s-a1 turn cause=@12 id=k:1:2 timeout=20000 go",
        ),
        (
            "@s-a1 turn id=k:1:2 settle=submitted timeout=9000 go",
            "@s-a1 turn id=k:1:2 settle=submitted timeout=9000 go",
        ),
        (
            "@s-a1 turn -- id=k literal",
            "@s-a1 turn -- timeout=20000 id=k literal",
        ),
    ] {
        let mut parts = words(sent);
        turn_wait(&mut parts).unwrap();
        assert_eq!(parts, words(wire), "{sent}");
    }
    for long in [
        "@s-a1 turn id=k:1:2 timeout=60000 go",
        "@s-a1 turn if-supervisor=h-9 id=k timeout=60000 go",
    ] {
        let refusal = turn_wait(&mut words(long)).expect_err(long).to_string();
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
fn a_hold_off_after_a_leave_names_what_lifts_the_hold() {
    fn left(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Err(DriveRefused {
            left: Some("496e9154e57ae1d4bb20b01cf5d624cf".to_owned()),
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
    assert_eq!(
        off,
        "this machine is in no fleet now, so it reaches lab-mini no more to hand s-a1 back: a \
         hold it took there before it left stands until that Mac's person takes the tab (Take \
         Over) or runs `aterm fleet peers rm 496e9154` there; if this machine left a fleet it \
         joined, `aterm ctl --host lab-mini hold s-a1 off` hands it back once it joins that \
         fleet again (a fleet it issued went with its leave)"
    );
    for other in ["hold s-a1 on", "@s-a1 status"] {
        let said = run("lab-mini", &words(other), None, &local)
            .expect_err(other)
            .to_string();
        assert_eq!(said, "this machine is in no fleet", "{other}");
    }
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
    let ran = |line: &str, verb: &str| exchange(link(answer, &SEEN), line, verb).unwrap();
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
        spawn_exchange(link(go, &SEEN), &head, task).unwrap(),
        ExitCode::SUCCESS
    );
    assert_eq!(
        spawn_exchange(link(deny, &SEEN), &head, task).unwrap(),
        ExitCode::FAILURE
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
    let turned = exchange(cut(), "@s-a1 turn timeout=20000 go\n", "turn")
        .expect_err("cut")
        .to_string();
    assert!(
        turned.starts_with(
            "lab-mini cut the drive connection before it answered (its person took back"
        ) && turned.contains(
            "what this `turn` sent may have landed: `aterm ctl --host lab-mini @s-a1 status`"
        ) && !turned.contains("docs.rs"),
        "{turned}"
    );
    let held = exchange(cut(), "hold s-a1 on\n", "hold")
        .expect_err("cut")
        .to_string();
    assert!(
        held.contains("`aterm ctl --host lab-mini @s-a1 status`"),
        "{held}"
    );
    let read = exchange(cut(), "@s-a1 text\n", "text")
        .expect_err("cut")
        .to_string();
    assert!(
        read.contains("`aterm ctl --host lab-mini @s-a1 text …` again says which")
            && !read.contains("landed"),
        "{read}"
    );
    let surveyed = exchange(cut(), "survey\n", "survey")
        .expect_err("cut")
        .to_string();
    assert!(
        surveyed.contains("`aterm ctl --host lab-mini survey …` again says which"),
        "{surveyed}"
    );
    let spawned = spawn_exchange(cut(), "spawn agent=claude len=2", b"go")
        .expect_err("cut")
        .to_string();
    assert!(spawned.contains("the worker may have started"), "{spawned}");
}
