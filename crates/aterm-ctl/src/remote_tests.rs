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
        node: None,
        asked_by: None,
        unnamed: None,
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

/// `--host` sends a read's session named after its verb in front of it, as the drive's peer
/// reads it, and leaves a meta verb's bare sid where it stands.
#[test]
fn a_drive_hears_a_reads_session_in_front_of_its_verb() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn worker(sent: &str) -> String {
        if sent.contains(" text") {
            "OK 0\n".to_owned()
        } else {
            "OK schema=1 sid=s-a1\n".to_owned()
        }
    }
    fn drive(host: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        assert_eq!(host, "lab-mini");
        Ok(link(worker, &SEEN))
    }
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        ..LocalVerbs::NONE
    };
    for typed in [
        "status @s-a1",
        "text s-a1 tail=3",
        "read @s-a1",
        "@s-a1 read trim",
        "hold s-a1 on",
        "close s-a1",
        "close @s-a1",
        "@s-a1 close",
    ] {
        let argv = ["--host", "lab-mini"]
            .into_iter()
            .chain(typed.split_whitespace())
            .map(std::ffi::OsString::from)
            .collect();
        let code = crate::real_main(argv, local).unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!(code, ExitCode::SUCCESS, "{typed}");
    }
    let refusal = crate::real_main(
        ["--host", "lab-mini", "text", "@3"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect(),
        local,
    )
    .expect_err("a local number");
    assert!(refusal.to_string().contains("by its sid"), "{refusal}");
    let seen = SEEN.lock().unwrap();
    assert_eq!(
        *seen,
        [
            "@s-a1 status",
            "@s-a1 text tail=3",
            "@s-a1 text",
            "@s-a1 text trim",
            "hold s-a1 on",
            "@s-a1 close",
            "@s-a1 close",
            "@s-a1 close"
        ]
    );
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
        ("agent=claude cwd=~bob/proj -", "another account's home"),
        ("agent=claude cwd=~/../bob -", "may leave the home"),
    ] {
        let refusal = spawn_head(&words(bad), None).expect_err(bad).to_string();
        assert!(refusal.contains(why), "{bad}: {refusal}");
    }
    // A folder of that Mac's home crosses as written: that Mac resolves it against its own.
    assert_eq!(
        spawn_head(
            &[
                "agent=claude".to_owned(),
                "cwd=$HOME/trust/My Crate".to_owned(),
                "-".to_owned()
            ],
            None
        )
        .unwrap(),
        "spawn agent=claude cwd=\"$HOME/trust/My Crate\""
    );
    assert_eq!(
        spawn_head(&words("agent=claude cwd=~ -"), None).unwrap(),
        "spawn agent=claude cwd=~"
    );
}

#[test]
fn a_plain_fleet_shell_has_one_explicit_folder_and_no_task_channel() {
    let owned = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        shell_spawn_head(&owned(&["cwd=/private/tmp", "why=look"])).unwrap(),
        "spawn cwd=/private/tmp why=look"
    );
    assert_eq!(
        shell_spawn_head(&owned(&[
            "cwd=/a folder",
            "why=read its notes",
            "raise=false"
        ]))
        .unwrap(),
        "spawn cwd=\"/a folder\" why=\"read its notes\" raise=false"
    );
    assert_eq!(
        shell_spawn_head(&owned(&["cwd=~/My Project", "why=look"])).unwrap(),
        "spawn cwd=\"~/My Project\" why=look",
        "a folder of that Mac's home, which it resolves"
    );
    assert_eq!(
        shell_spawn_head(&words("cwd=~ why=look raise=false")).unwrap(),
        "spawn cwd=~ why=look raise=false"
    );
    // The one form with no brief says what it is for (the requests design §5): none, nothing, a
    // second line or 201 characters is refused here, before any Mac is dialed.
    let missing = shell_spawn_head(&words("cwd=/private/tmp")).expect_err("no why=");
    assert!(missing.to_string().contains("names no why="), "{missing}");
    for bad in [
        owned(&["cwd=/tmp", "why="]),
        owned(&["cwd=/tmp", "why=a\nb"]),
        vec!["cwd=/tmp".to_owned(), format!("why={}", "w".repeat(201))],
        owned(&["cwd=/tmp", "why=a", "why=b"]),
    ] {
        assert!(shell_spawn_head(&bad).is_err(), "{bad:?}");
    }
    for (bad, why) in [
        ("cwd=~bob/x", "another account's home"),
        ("cwd=~/../x", "may leave the home"),
        ("cwd=~/a/../../x", "may leave the home"),
    ] {
        let refusal = shell_spawn_head(&words(bad)).expect_err(bad).to_string();
        assert!(refusal.contains(why), "{bad}: {refusal}");
        assert!(refusal.contains(&format!("`{bad}`")), "{bad}: {refusal}");
    }
    for bad in [
        "",
        "cwd=relative",
        "cwd=/tmp -",
        "cwd=/tmp len=3",
        "cwd=/tmp command=sh",
        "cwd=/tmp -e sh",
        "cwd=/tmp from=s-x",
        "cwd=/tmp model=x",
        "cwd=/tmp agent=",
        "cwd=/tmp raise=true",
        "cwd=/tmp cwd=/",
        "cwd=/tmp raise=false raise=false",
        "cwd=/tmp split=v",
        "cwd=/tmp window=0",
    ] {
        assert!(shell_spawn_head(&words(bad)).is_err(), "{bad}");
    }
    // Each refusal names the word it refuses, so the driver need not guess which.
    for (bad, word) in [
        ("cwd=relative", "cwd=relative"),
        ("cwd=/tmp command=sh", "command=sh"),
        ("cwd=/tmp -e sh", "-e"),
        ("cwd=/tmp cwd=/", "cwd=/"),
        ("cwd=/tmp raise=false raise=false", "raise=false"),
        ("cwd=/tmp window=0", "window=0"),
    ] {
        let refusal = shell_spawn_head(&words(bad)).expect_err(bad).to_string();
        assert!(refusal.contains(&format!("`{word}`")), "{bad}: {refusal}");
    }
}

/// A plain fleet shell joins a request the driven Mac named (`req=<id>`): one id of the shape it
/// mints. NEGATIVE CONTROLS: an id it never mints and a second `req=` are refused here, naming
/// the word, before any Mac is dialed.
#[test]
fn a_plain_fleet_shell_takes_one_req_of_request_id_shape() {
    let id = "r-20261109-1411-87be5228-01";
    assert_eq!(
        shell_spawn_head(&words(&format!("cwd=/tmp why=look req={id}"))).unwrap(),
        format!("spawn cwd=/tmp why=look req={id}")
    );
    for (bad, word) in [
        ("cwd=/tmp why=look req=r-1".to_owned(), "req=r-1".to_owned()),
        (
            format!("cwd=/tmp why=look req={id} req={id}"),
            format!("req={id}"),
        ),
    ] {
        let refusal = shell_spawn_head(&words(&bad)).expect_err(&bad).to_string();
        assert!(refusal.contains(&format!("`{word}`")), "{bad}: {refusal}");
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
fn a_plain_shell_spawn_retries_only_a_complete_predispatch_busy_reply() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static CASE: AtomicUsize = AtomicUsize::new(0);
    fn busy(_: &str) -> String {
        aterm_types::control_verbs::CONTROL_BUSY_LINE.to_owned()
    }
    fn ready(_: &str) -> String {
        "OK s-00000000000000aa\n".to_owned()
    }
    fn eof(_: &str) -> String {
        String::new()
    }
    fn partial_busy(_: &str) -> String {
        aterm_types::control_verbs::CONTROL_BUSY_REPLY.to_owned()
    }
    fn surplus(_: &str) -> String {
        format!(
            "{}unexpected body\n",
            aterm_types::control_verbs::CONTROL_BUSY_LINE
        )
    }
    fn drive(host: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        assert_eq!(host, "lab-mini");
        let call = CALLS.fetch_add(1, Ordering::SeqCst);
        let answer = match CASE.load(Ordering::SeqCst) {
            0 if call == 0 => busy,
            0 => ready,
            1 => eof,
            2 => partial_busy,
            3 => surplus,
            _ => unreachable!(),
        };
        Ok(link(answer, &SEEN))
    }
    let local = LocalVerbs {
        fleet_drive: Some(drive),
        ..LocalVerbs::NONE
    };
    for case in 0..4 {
        CASE.store(case, Ordering::SeqCst);
        CALLS.store(0, Ordering::SeqCst);
        SEEN.lock().unwrap().clear();
        let result = run("lab-mini", &words("spawn cwd=/tmp why=look"), None, &local);
        let expected = if case == 0 { 2 } else { 1 };
        assert_eq!(CALLS.load(Ordering::SeqCst), expected, "case {case}");
        assert_eq!(
            SEEN.lock().unwrap().as_slice(),
            vec!["spawn cwd=/tmp why=look"; expected],
            "case {case}: only the complete Busy response permits another spawn"
        );
        assert_eq!(
            matches!(result, Ok(code) if code == ExitCode::SUCCESS),
            case == 0,
            "case {case}: {result:?}"
        );
    }
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
                node: None,
                asked_by: None,
                unnamed: None,
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
    fn refused_after_go(sent: &str) -> String {
        let len = sent
            .split_whitespace()
            .find_map(|w| w.strip_prefix("len="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        format!(
            "{}ERR spawn: no window takes a worker\n",
            aterm_types::control_verbs::task_frame_go(len)
        )
    }
    let task = b"read the README\nand say one line";
    let head = format!("spawn agent=claude len={}", task.len());
    // What the exchange answered, and every line it printed on stdout and on stderr: none, for
    // its caller prints the answer once it has recorded the worker (`spawn_worker`).
    let exchanged = |answer: fn(&str) -> String| {
        let ((answered, told), printed) = crate::stdout_lines_of(false, || {
            crate::stderr_lines_of(|| spawn_exchange(link(answer, &SEEN), &head, task, OWN))
        });
        (answered.unwrap(), printed, told)
    };
    assert_eq!(
        exchanged(go),
        (("OK s-w1 task_rev=1".to_owned(), None), vec![], vec![]),
        "`w1` is no sid"
    );
    assert_eq!(
        exchanged(deny),
        (
            (
                "ERR denied: a drive's worker opens raise=false".to_owned(),
                None
            ),
            vec![],
            vec![]
        ),
        "a refusal before the task"
    );
    assert_eq!(
        exchanged(refused_after_go),
        (
            ("ERR spawn: no window takes a worker".to_owned(), None),
            vec![],
            vec![]
        ),
        "a refusal of the task"
    );
    let seen = SEEN.lock().unwrap();
    assert_eq!(*seen, [head.clone(), head.clone(), head]);
}

/// A worker `agent=claude cwd=/w -` sent to `host`, its task `go on`, by `s-5e1`.
fn fleet_worker<'a>(host: &str, args: &'a [String]) -> Worker<'a> {
    Worker {
        host: host.to_owned(),
        placed: crate::board::Placed::default(),
        args,
        label: Some("dx".to_owned()),
        starter: Some("s-5e1"),
        head: "spawn agent=claude cwd=/w - from=s-5e1",
        task: b"go on",
    }
}

/// The `kind=spawn` rows of the log in `log`.
fn spawn_rows(log: &std::path::Path) -> Vec<aterm_telemetry::Row> {
    aterm_telemetry::log::read(log, 4, None).map_or_else(
        |_| Vec::new(),
        |read| {
            read.rows
                .into_iter()
                .filter(|r| r.kind == aterm_telemetry::Kind::Spawn)
                .collect()
        },
    )
}

/// A FLEET WORKER IS RECORDED FROM ITS ANSWER, NOT FROM ITS PRINT (Continuum TM-1): a spawn
/// whose answer cannot be printed (stdout closed: `… spawn … - | head -0`, here this thread's
/// stdout seam) started its worker all the same, so the sender's `kind=spawn` row says `ok` and
/// the worker's sid on the Mac its dial proved, and the board holds the worker; the closed stdout
/// is the call's own error alone, which exits 0 (`exit_for`). The answer is printed once, after
/// both: as it prints, the log holds the row and the board the worker.
/// NEGATIVE CONTROL: the same spawn with stdout open writes the same row and board row, and
/// prints `OK` once.
#[test]
fn a_fleet_spawn_whose_answer_cannot_be_printed_still_records_its_worker() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn go(_: &str) -> String {
        format!(
            "{}OK s-0e1 task_rev=1\n",
            aterm_types::control_verbs::task_frame_go(b"go on".len())
        )
    }
    fn drive(host: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        assert_eq!(host, "lab-mini");
        Ok(DriveLink {
            node: Some("0a".repeat(16)),
            ..link(go, &SEEN)
        })
    }
    let args = words("agent=claude cwd=/w -");
    for closed in [true, false] {
        let (tmp, _here, board) = scratch_board("unprinted");
        let log = aterm_telemetry::log::dir_in(tmp.path());
        let at_print = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let watch = {
            let (at_print, log, board) = (at_print.clone(), log.clone(), board.dir().to_owned());
            move |line: &str| {
                at_print.borrow_mut().push((
                    line.to_owned(),
                    spawn_rows(&log).len(),
                    crate::board::Board::at(&board).row("s-0e1").is_some(),
                ));
            }
        };
        let (ended, printed) = crate::stdout_lines_watched(closed, watch, || {
            spawn_worker(fleet_worker("lab-mini", &args), OWN, drive)
        });
        assert_eq!(
            *at_print.borrow(),
            [("OK s-0e1 task_rev=1".to_owned(), 1, true)],
            "printed after the row and the board's record (closed stdout: {closed})"
        );
        if closed {
            let e = ended.expect_err("stdout closed");
            assert!(crate::is_stdout_closed(&e), "{e}");
            assert_eq!(crate::exit_for(&e), ExitCode::SUCCESS, "a closed stdout");
        } else {
            assert_eq!(ended.expect("printed"), ExitCode::SUCCESS);
        }
        assert_eq!(
            printed,
            ["OK s-0e1 task_rev=1"],
            "the answer, printed once (closed stdout: {closed})"
        );
        let rows = spawn_rows(&log);
        assert_eq!(
            rows.iter()
                .map(|r| (r.outcome, r.sid.as_deref(), r.on.as_deref()))
                .collect::<Vec<_>>(),
            [(
                aterm_telemetry::Outcome::Ok,
                Some("s-0e1"),
                Some("0a".repeat(16).as_str())
            )],
            "one sender's row naming the worker (closed stdout: {closed})"
        );
        let row = board
            .row("s-0e1")
            .unwrap_or_else(|| panic!("the board holds the worker (closed stdout: {closed})"));
        assert_eq!(
            (
                row.node,
                row.host.as_deref(),
                row.label.as_deref(),
                row.cwd.as_deref(),
                row.from.as_deref()
            ),
            (
                Some("0a".repeat(16)),
                Some("lab-mini"),
                Some("dx"),
                Some("/w"),
                Some("s-5e1")
            )
        );
    }
}

/// A FLEET SPAWN WHOSE DIAL FAILED WRITES ITS SENDER'S ROW (Continuum TM-1): no grant, no route,
/// so nothing was sent: the row says `refused`, names no worker and names the Mac it was for as
/// the spawn named it, since no dial proved its node. The board records nothing, nothing is
/// printed to stdout or told through `stderr_line`, and the call is the dial's refusal.
#[test]
fn a_fleet_spawn_whose_dial_failed_writes_a_refused_row_for_the_mac_it_named() {
    fn ungranted(host: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        assert_eq!(host, "lab-mini");
        Err(DriveRefused {
            ungranted: true,
            ..DriveRefused::once("lab-mini serves no drive to this Mac".to_owned())
        })
    }
    let (tmp, _here, board) = scratch_board("undialled");
    let log = aterm_telemetry::log::dir_in(tmp.path());
    let args = words("agent=claude cwd=/w -");
    let ((ended, printed), told) = crate::stderr_lines_of(|| {
        crate::stdout_lines_of(false, || {
            spawn_worker(fleet_worker("lab-mini", &args), OWN, ungranted)
        })
    });
    let refusal = ended.expect_err("the dial failed");
    assert!(refusal.to_string().contains("serves no drive"), "{refusal}");
    assert!(printed.is_empty(), "no answer came: {printed:?}");
    assert!(
        told.is_empty(),
        "the refusal is the call's, told once by its caller: {told:?}"
    );
    let rows = spawn_rows(&log);
    assert_eq!(
        rows.iter()
            .map(|r| (
                r.outcome,
                r.sid.as_deref(),
                r.on.as_deref(),
                r.label.as_str()
            ))
            .collect::<Vec<_>>(),
        [(
            aterm_telemetry::Outcome::Refused,
            None,
            Some("lab-mini"),
            "claude"
        )],
        "one refused row, for the Mac the spawn named"
    );
    assert!(
        board.scan().rows.is_empty(),
        "no worker started, so the board holds none"
    );
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
        node: None,
        asked_by: None,
        unnamed: None,
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
        node: None,
        asked_by: None,
        unnamed: None,
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
            node: None,
            asked_by: None,
            unnamed: None,
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
            node: None,
            asked_by: None,
            unnamed: None,
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

/// A cut names the deadline it ran out at, a part of a second included: a test world's seam, an
/// `await task` leg's `timeout=` and a `--timeout` can each make one.
/// NEGATIVE CONTROL: whole seconds alone say a tenth of a second as `0 s`.
#[test]
fn a_cut_names_a_deadline_under_a_second_as_it_is() {
    let cut = |within| {
        closed(
            "lab-mini",
            "@s-a1 status\n",
            within,
            io::ErrorKind::WouldBlock.into(),
        )
        .to_string()
    };
    let own = cut(Within::Drive(Duration::from_millis(100)));
    assert!(own.contains("did not answer within 0.1 s"), "{own}");
    let flag = cut(Within::Timeout(
        Duration::from_millis(1500),
        Longer::Drive(Duration::from_millis(9500)),
    ));
    assert!(
        flag.contains("--timeout of 1.5 s") && flag.contains("own 9.5 s"),
        "{flag}"
    );
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
                node: None,
                asked_by: None,
                unnamed: None,
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
        authenticated: None,
        reason: None,
        label: format!("m{n}.local"),
        answer: Ok(vec![
            format!(
                "instance pid=9 host=m{n}.local version=0.146.0 sessions=0 level=facts \
                 at_ms=1793130000000 accounts=1{}",
                if drive { " drive=1" } else { "" }
            ),
            format!(
                "account pool=claude id=main condition=usable enrolled=1 pct5=1 pct7={week} \
                 week_reset=- walled_until=- last_used=- read_at=1793129900"
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
    let (node, placed) =
        any_host_node(&words("agent=claude -"), &[], None, &local).expect("a pick");
    assert_eq!(node, "03".repeat(16), "m3, never m2, which grants no drive");
    assert_eq!(
        placed,
        crate::board::Placed {
            node: Some("03".repeat(16)),
            host: Some("m3.local".to_owned()),
            account: Some("main".to_owned()),
        },
        "the board records the pick: its node, its host and the account row it was chosen by"
    );
    let none = any_host_node(&words("agent=codex -"), &[], None, &local)
        .expect_err("no codex row")
        .to_string();
    assert!(
        none.starts_with("--host any: no Mac can take agent=codex: 3 members answered, 2 of them"),
        "{none}"
    );
}

/// The survey of [`any_fleet`] with each member's home: m1 (50%) holds a trust checkout its read
/// of 10 min ago found and 200 GiB free, m2 (no drive, 0%) the same, and m3 (20%) runs an aterm
/// too old to say anything of its home.
fn any_fleet_homes(ask: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
    let mut read = any_fleet(ask)?;
    for peer in &mut read.peers[..2] {
        if let Ok(lines) = &mut peer.answer {
            let host = lines[0]
                .split_whitespace()
                .find_map(|word| word.strip_prefix("host="))
                .map(str::to_owned)
                .expect("a host");
            lines[0].push_str(&format!(
                " home=/Users/{host} home_free_mb={} repos=1 repos_at={} repos_cut=0",
                200 * 1024,
                1_793_130_000_000_u64 - 600_000
            ));
            lines.push(format!(
                "repo name=trust path=/Users/{host}/trust \
                 head=0123456789abcdef0123456789abcdef01234567 branch=main main=origin/main \
                 behind=4 fetched_at=1793120000000 dirty=0"
            ));
        }
    }
    Ok(read)
}

/// `need=` picks the Mac by its own facts and never crosses to it; a `cwd=~/…` needs a Mac that
/// names its home. The least used Mac (m3) says nothing of its home, so it is never chosen for
/// either, and a need no Mac meets is refused with the counts.
#[test]
fn host_any_places_a_worker_only_where_its_needs_are_met() {
    fn never(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        panic!("dialled")
    }
    let local = LocalVerbs {
        fleet_survey: Some(any_fleet_homes),
        fleet_drive: Some(never),
        ..LocalVerbs::NONE
    };
    assert_eq!(
        any_host_node(&words("agent=claude -"), &[], None, &local)
            .expect("a pick")
            .0,
        "03".repeat(16),
        "with no needs, the least used week"
    );
    let m1 = "01".repeat(16);
    assert_eq!(
        any_host_node(
            &words("agent=claude -"),
            &words("need=repo:trust,free:150g"),
            None,
            &local
        )
        .expect("a pick")
        .0,
        m1
    );
    assert_eq!(
        any_host_node(&words("agent=claude cwd=$HOME/trust -"), &[], None, &local)
            .expect("a pick")
            .0,
        m1,
        "a home that resolves $HOME/trust"
    );
    let none = any_host_node(
        &words("agent=claude -"),
        &words("need=repo:trust,free:250g"),
        None,
        &local,
    )
    .expect_err("none has 250 GiB")
    .to_string();
    assert_eq!(
        none,
        "--host any: no Mac can take agent=claude need=repo:trust,free:250g: 3 members answered, \
         2 of them can be driven from this Mac (of the others, 1 with no grant: `aterm fleet \
         allow drive --from <this Mac>` there); of those, 1 have less free disk than it needs \
         (free:250g), 1 cannot say whether they meet its needs (1 saying nothing of their home, \
         checkouts or disk, from an aterm too old to, no read of its checkouts yet or a reply cut \
         short: update it, or ask again), so none meets them"
    );
}

/// `need=` is this client's to judge from the survey `--host any` reads: with one Mac named it
/// is refused before any dial, never sent.
#[test]
fn a_need_names_no_mac_of_its_own() {
    fn never(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        panic!("dialled")
    }
    let local = LocalVerbs {
        fleet_drive: Some(never),
        ..LocalVerbs::NONE
    };
    let refusal = run(
        "lab-mini",
        &words("spawn agent=claude need=repo:trust -"),
        None,
        &local,
    )
    .expect_err("one Mac named");
    assert_eq!(refusal.kind(), io::ErrorKind::InvalidInput, "{refusal}");
    assert!(
        refusal
            .to_string()
            .contains("need= picks the Mac under --host any"),
        "{refusal}"
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
    for line in [
        "survey",
        "@s-a1 spawn agent=claude -",
        "spawn agent=agy -",
        "spawn -",
        "spawn cwd=/tmp",
        "spawn agent=claude",
        "spawn agent=claude cwd=rel -",
        "spawn agent=claude cwd=~bob -",
        "spawn agent=claude need=gpu:1 -",
        "spawn agent=claude need= -",
        "spawn agent=claude need=repo:trust,free:150 -",
    ] {
        let refusal = run(ANY_TEST, &words(line), None, &local).expect_err(line);
        assert_eq!(
            refusal.kind(),
            io::ErrorKind::InvalidInput,
            "{line}: {refusal}"
        );
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

/// This thread's board, in a scratch folder, while the guard lives.
fn scratch_board(
    tag: &str,
) -> (
    aterm_tempfile::TempDir,
    crate::board::HereGuard,
    crate::board::Board,
) {
    let tmp = crate::test_scratch_dir(tag);
    let dir = tmp.path().join("board");
    let guard = crate::board::HereGuard::at(&dir);
    (tmp, guard, crate::board::Board::at(&dir))
}

/// A row of a worker this Mac started on `node` (`None`: this Mac).
fn started_on(sid: &str, node: Option<&str>) -> crate::board::Row {
    crate::board::Row {
        node: node.map(str::to_owned),
        ..crate::board::Row::started(sid, crate::board::now_ms())
    }
}

/// A worker `--host any` spawned is followed to its Mac by its sid: from the board row the
/// spawn recorded, else from the one live, proven member whose survey lists it.
#[test]
fn host_any_follows_a_spawned_workers_sid_to_its_mac() {
    static DIALLED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let (_tmp, _guard, board) = scratch_board("any-follow");

    fn fleet_survey(_: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
        Ok(survey::FleetRead {
            peers: vec![
                // An unproven member's word for where a sid runs is never taken.
                survey::PeerSurvey {
                    authenticated: None,
                    reason: None,
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
                    authenticated: None,
                    reason: None,
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
                    authenticated: None,
                    reason: None,
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
            node: None,
            asked_by: None,
            unnamed: None,
        })
    }

    // The board places s-c25a on m3: `await task` and `@s-c25a result` follow it there, while
    // `@s-f00d`, which the board does not hold, falls back to `fleet_survey` and routes to `m4`.
    board
        .record(&started_on("s-c25a", Some("m3")))
        .expect("record");
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
}

/// NEGATIVE CONTROL: `--host any` follows a sid by the board's row before any survey, and a
/// survey can neither override it nor write a binding down (the board holds only workers this Mac
/// started); a sid more than one proven peer lists is refused rather than picked; a sid the board
/// places on this Mac is refused (`--host any` drives other Macs); and a row's file open to
/// others is not read.
#[test]
fn host_any_follows_the_board_first_and_never_records_a_survey() {
    let (_tmp, _guard, board) = scratch_board("any-harden");
    board
        .record(&started_on("s-c25a", Some("m3")))
        .expect("record");
    board.record(&started_on("s-0ca1", None)).expect("record");

    fn conflicting_survey(
        _: &survey::FleetAsk<'_>,
    ) -> Result<survey::FleetRead, survey::FleetRefused> {
        let huge_sid = format!("s-{}", "f".repeat(128));
        Ok(survey::FleetRead {
            peers: vec![
                survey::PeerSurvey {
                    authenticated: None,
                    reason: None,
                    label: "m-a".into(),
                    answer: Ok(vec![
                        "instance host=m-a drive=1 sessions=3".into(),
                        "session sid=s-c25a agent=idle".into(),
                        "session sid=s-a1b2 agent=idle".into(),
                        format!("session sid={huge_sid} agent=idle"),
                    ]),
                    fault: survey::PeerFault::Other,
                    unproven: false,
                    node: Some("node-a".into()),
                    addr: None,
                },
                survey::PeerSurvey {
                    authenticated: None,
                    reason: None,
                    label: "m-b".into(),
                    answer: Ok(vec![
                        "instance host=m-b drive=1 sessions=2".into(),
                        "session sid=s-a1b2 agent=idle".into(),
                        "session sid=s-b001 agent=idle".into(),
                    ]),
                    fault: survey::PeerFault::Other,
                    unproven: false,
                    node: Some("node-b".into()),
                    addr: None,
                },
            ],
            note: None,
            alone: None,
        })
    }
    fn unasked(_: &survey::FleetAsk<'_>) -> Result<survey::FleetRead, survey::FleetRefused> {
        panic!("surveyed")
    }

    assert_eq!(
        resolve_any_sid_hosts(&["s-c25a"], None, Some(unasked)).unwrap(),
        vec!["m3".to_owned()],
        "a sid the board holds needs no survey"
    );
    assert_eq!(
        resolve_any_sid_hosts(&["s-c25a", "s-b001"], None, Some(conflicting_survey)).unwrap(),
        vec!["m3".to_owned(), "node-b".to_owned()],
        "m-a listing s-c25a does not move it off the board's m3"
    );
    assert!(
        board.row("s-b001").is_none(),
        "a survey's finding is not written down"
    );
    let ambig =
        resolve_any_sid_hosts(&["s-a1b2"], None, Some(conflicting_survey)).expect_err("ambiguous");
    assert!(
        ambig.to_string().contains("multiple fleet peers")
            && ambig.to_string().contains("node-a")
            && ambig.to_string().contains("node-b"),
        "{ambig}"
    );
    let here = resolve_any_sid_hosts(&["s-0ca1"], None, Some(unasked)).expect_err("this Mac");
    assert_eq!(here.kind(), io::ErrorKind::InvalidInput, "{here}");
    assert!(here.to_string().contains("runs on this Mac"), "{here}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let file = board.dir().join("s-c25a");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let loose = resolve_any_sid_hosts(&["s-c25a"], None, Some(conflicting_survey)).unwrap();
        assert_eq!(
            loose,
            vec!["node-a".to_owned()],
            "a row open to others is not read: the survey places the sid"
        );
    }
}

const ACTIVITY_NODE: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
const ACTIVITY_ASKER: &str = "s-0123456789abcdef0123";

/// The lock's refusal of an act of `kind` as a driven Mac at Read Only answers it.
fn locked(kind: aterm_types::fleet_access::DriveKind) -> String {
    let refusal = aterm_types::fleet_access::FleetAccess::ReadOnly
        .refusal(kind)
        .expect("Read Only refuses it");
    format!("{refusal}; not `{}`\n", kind.word())
}

/// What the requests since the last call left for the fleet activity record: each row's type,
/// whether the other Mac's lock blocked it, and the session that asked.
fn activity() -> Vec<(aterm_types::fleet_activity::Plain, bool, Option<String>)> {
    crate::fleet_activity::NOTED
        .with(|noted| std::mem::take(&mut *noted.borrow_mut()))
        .into_iter()
        .map(|row| {
            assert_eq!(
                (row.way, row.node.as_str(), row.host.as_deref(), row.n),
                (
                    aterm_types::fleet_activity::Way::Out,
                    ACTIVITY_NODE,
                    Some("lab-mini"),
                    1
                )
            );
            (row.what, row.blocked(), row.by)
        })
        .collect()
}

/// A REQUEST `aterm ctl --host` SENT IS ONE OUTGOING ROW, AS THE OTHER MAC'S LOCK ANSWERED IT:
/// admitted where its answer is `OK`, blocked where it is that Mac's fleet access refusing it,
/// and none where the answer is the gate's own refusal, which no lock decided. A read is a row
/// only where a session asked for it (the drive's requester line named one); what a drive does
/// to a session is a row whoever asked. A link whose dial proved no node names no Mac to count.
#[test]
fn a_request_sent_over_a_drive_is_one_row_as_the_other_macs_lock_answered_it() {
    use aterm_types::fleet_access::DriveKind;
    use aterm_types::fleet_activity::Plain;
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn answer(sent: &str) -> String {
        match sent.split_whitespace().nth(1) {
            Some("close") => "ERR denied: a drive closes only a worker it spawned\n".to_owned(),
            Some("turn") => "OK 1 turn submitted=1 status=done seq=4\n❯ go\n".to_owned(),
            Some("send") => locked(DriveKind::Send),
            _ => "OK 2\nrow one\nrow two\n".to_owned(),
        }
    }
    let run = |node: Option<&str>, by: Option<&str>, line: &str, verb: &str| {
        let link = DriveLink {
            node: node.map(str::to_owned),
            asked_by: by.map(str::to_owned),
            unnamed: None,
            ..link(answer, &SEEN)
        };
        let ((ended, _printed), _told) = crate::stderr_lines_of(|| {
            crate::stdout_lines_of(false, || exchange(link, line, verb, OWN))
        });
        assert!(ended.is_ok(), "{line}");
        activity()
    };
    drop(activity());
    let (node, asker) = (Some(ACTIVITY_NODE), Some(ACTIVITY_ASKER));
    let by = Some(ACTIVITY_ASKER.to_owned());
    assert_eq!(
        run(node, asker, "@s-a1 turn timeout=20000 go\n", "turn"),
        [(Plain::Typed, false, by.clone())]
    );
    assert_eq!(
        run(node, asker, "@s-a1 text\n", "text"),
        [(Plain::Read, false, by.clone())]
    );
    assert_eq!(
        run(node, asker, "@s-a1 send hi\n", "send"),
        [(Plain::Typed, true, by)],
        "the lock's refusal"
    );
    assert_eq!(
        run(node, asker, "@s-a1 close\n", "close"),
        [],
        "the gate's refusal"
    );
    // No session asked: a read is aterm's own poll, a typed line still counts.
    assert_eq!(run(node, None, "@s-a1 text\n", "text"), []);
    assert_eq!(
        run(node, None, "@s-a1 turn timeout=20000 go\n", "turn"),
        [(Plain::Typed, false, None)]
    );
    assert_eq!(
        run(None, asker, "@s-a1 turn timeout=20000 go\n", "turn"),
        [],
        "no node proved"
    );
}

/// A WORKER STARTED ON ANOTHER MAC IS AN OUTGOING NEW JOB, and one that Mac's fleet access
/// refused is a blocked one; a spawn the dial never carried has no row (nothing was asked).
#[test]
fn a_spawn_sent_to_another_mac_is_a_new_job_row_admitted_or_blocked() {
    use aterm_types::fleet_activity::Plain;
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn go(_: &str) -> String {
        format!(
            "{}OK s-0e1 task_rev=1\n",
            aterm_types::control_verbs::task_frame_go(b"go on".len())
        )
    }
    fn refuse(_: &str) -> String {
        locked(aterm_types::fleet_access::DriveKind::Spawn)
    }
    fn dialled(answer: fn(&str) -> String) -> DriveLink {
        DriveLink {
            node: Some(ACTIVITY_NODE.to_owned()),
            asked_by: Some(ACTIVITY_ASKER.to_owned()),
            unnamed: None,
            ..link(answer, &SEEN)
        }
    }
    fn started(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Ok(dialled(go))
    }
    fn blocked(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Ok(dialled(refuse))
    }
    fn undialled(_: &str, _: Duration) -> Result<DriveLink, DriveRefused> {
        Err(DriveRefused::once(
            "lab-mini serves no drive to this Mac".to_owned(),
        ))
    }
    let args = words("agent=claude cwd=/w -");
    let by = Some(ACTIVITY_ASKER.to_owned());
    let (_tmp, _here, _board) = scratch_board("activity");
    let spawn = |drive: FleetDriveFn| {
        drop(activity());
        let _ = crate::stderr_lines_of(|| {
            crate::stdout_lines_of(false, || {
                spawn_worker(fleet_worker("lab-mini", &args), OWN, drive)
            })
        });
        activity()
    };
    assert_eq!(spawn(started), [(Plain::NewJob, false, by.clone())]);
    assert_eq!(spawn(blocked), [(Plain::NewJob, true, by)]);
    assert_eq!(spawn(undialled), []);
}
