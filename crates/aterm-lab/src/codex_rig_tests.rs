// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn no_ip_policy_parses_and_denies_the_live_loopback_exception() {
    fn run(policy: &str, program: &str, args: &[&str]) -> std::process::Output {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        command
            .args(["-p", policy, program])
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
            .expect("bounded sandbox policy control")
    }

    let denied = no_ip_jail_policy();
    let parsed = run(&denied, "/usr/bin/true", &[]);
    assert!(parsed.status.success(), "sandbox must parse: {parsed:?}");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let port_text = port.to_string();
    let args = ["-z", "-G", "1", "127.0.0.1", port_text.as_str()];
    let allowed = run(&jail_policy(port), "/usr/bin/nc", &args);
    assert!(
        allowed.status.success(),
        "explicit loopback port: {allowed:?}"
    );
    drop(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the allowed process really connected"),
    );

    let blocked = run(&denied, "/usr/bin/nc", &args);
    assert!(!blocked.status.success(), "no IP exception: {blocked:?}");
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

/// A record on the terminal numbered 3, started at 1000.
fn record() -> kproc::Record {
    kproc::Record {
        pid: 4242,
        ppid: 1,
        pgid: 4240,
        tpgid: 4240,
        tty: Some(3),
        uid: 501,
        comm: "zsh".to_string(),
        start_s: 1_000,
        start_us: 7,
        zombie: false,
    }
}

/// The line `-p <pid> -o <o>` chooses and prints of `r`, read by `read`, `etime` at 1065.
fn line_of(o: &str, r: &kproc::Record, read: &Readers<'_>) -> Vec<u8> {
    let pid = r.pid.to_string();
    let args = ["-p", pid.as_str(), "-o", o].map(str::to_string);
    let (fields, rows) =
        ps_rows(&args, || None, |pid| (pid == r.pid).then(|| r.clone())).expect("parses");
    assert_eq!(rows.len(), 1, "{o}");
    ps_line(&rows[0], &fields, 1_065, read)
}

/// A `ps -o` line is the kernel record's fields in the order asked, each keyword its own
/// column: `tt` the terminal's name less its head and `tty` all of it, `etime` from its
/// start, `comm` and `args` (and `command`) from their readers, `ucomm` the kernel's short
/// name, `stat` `Z` for a zombie and `-` else, a keyword with no reader `-`, and a process
/// with no terminal at `??`. A `comm` or `command` another column follows is cut to 16 bytes
/// and an `args` to 64, even inside a character; the last column is whole.
#[test]
fn a_ps_line_reads_the_kernel_record() {
    let tty = |dev: Option<i32>| match dev {
        Some(3) => "ttys003".to_string(),
        None => "??".to_string(),
        Some(other) => panic!("no terminal {other}"),
    };
    let read = Readers {
        tty: &tty,
        comm: &|_: &kproc::Record| "-zsh".to_string(),
        args: &|_: &kproc::Record| "-zsh -l".to_string(),
    };
    let r = record();
    let every = "pid=,ppid=,pgid=,tpgid=,tt=,tty=,lstart=,etime=,comm=,ucomm=,args=,command=,\
                 stat=,rss=";
    assert_eq!(
        String::from_utf8(line_of(every, &r, &read)).expect("UTF-8"),
        format!(
            "4242 1 4240 4240 s003 ttys003 {} 01:05 -zsh zsh -zsh -l -zsh -l - -",
            lstart(1_000)
        )
    );
    let dead = kproc::Record {
        zombie: true,
        tty: None,
        ..record()
    };
    assert_eq!(line_of("stat=,tt=,tty=", &dead, &read), b"Z ?? ??");
    let comm = "/ééééééééé";
    let args = format!("/{} -l", "a".repeat(70));
    let long = Readers {
        tty: &tty,
        comm: &|_: &kproc::Record| comm.to_string(),
        args: &|_: &kproc::Record| args.clone(),
    };
    let cut: [&[u8]; 7] = [
        "/ééééééé".as_bytes(),
        &[0xc3],
        b" ",
        &args.as_bytes()[..64],
        b" ",
        &args.as_bytes()[..16],
        b" 4242",
    ];
    assert_eq!(
        line_of("comm=,args=,command=,pid=", &r, &long),
        cut.concat()
    );
    assert_eq!(
        line_of("pid=,comm=", &r, &long),
        format!("4242 {comm}").into_bytes()
    );
    assert_eq!(
        line_of("pid=,args=", &r, &long),
        format!("4242 {args}").into_bytes()
    );
    assert_eq!(short_tty("cuau0"), "u0");
    assert_eq!(short_tty("console"), "console");
}

/// A `/bin/sleep 600` its `argv[0]` names, killed and reaped once dropped.
struct Sleeping(i32);

impl Sleeping {
    fn named(arg0: &str) -> Sleeping {
        Sleeping(
            crate::procs::spawn_kept(
                aterm_uds::spawnfd::command("/bin/sleep")
                    .arg0(arg0)
                    .arg("600"),
            )
            .expect("spawn sleep"),
        )
    }

    fn record(&self) -> kproc::Record {
        kproc::record(u32::try_from(self.0).expect("a pid")).expect("its record")
    }
}

impl Drop for Sleeping {
    fn drop(&mut self) {
        let _ = crate::sys::kill(self.0, libc::SIGKILL);
        if !crate::procs::wait_gone(self.0, Duration::from_secs(60)) {
            eprintln!("sleep {} was not reaped", self.0);
        }
    }
}

/// What `/bin/ps -o <o> -p <pid>` prints with `env` its whole environment (and this test's
/// `TZ`), waited for as a one-minute hang detector.
fn ps_says(o: &str, pid: i32, env: &[(&str, &str)]) -> Vec<u8> {
    let pid = pid.to_string();
    let mut cmd = aterm_uds::spawnfd::command("/bin/ps");
    cmd.args(["-o", o, "-p", pid.as_str()]).env_clear();
    if let Ok(tz) = std::env::var("TZ") {
        cmd.env("TZ", tz);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(60))
        .expect("/bin/ps answers within a minute");
    assert!(out.status.success(), "{out:?}");
    out.stdout
}

/// The stand-in's columns of a live process, each keyword read through its own table, are
/// the ones `/bin/ps` prints: `comm` its `argv[0]`, `args` and `command` its whole command
/// line, each cut where another column follows, `ucomm` the kernel's short name. The terminal
/// columns are left to `a_ps_line_reads_the_kernel_record`: two `devname` scans of a live
/// terminal can disagree under pty churn.
#[test]
fn a_live_process_reads_as_ps_prints_it() {
    let sleep = Sleeping::named(&format!("/lab/{}-sleep", "a".repeat(70)));
    let pid = sleep.record().pid.to_string();
    let read = Readers {
        tty: &crate::sys::tty_name,
        comm: &|r: &kproc::Record| kproc::ps_comm(r, kproc::Locale::C),
        args: &|r: &kproc::Record| kproc::ps_args(r, kproc::Locale::C),
    };
    let words = |line: &[u8]| {
        String::from_utf8_lossy(line)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    for o in [
        "pid=,ppid=,pgid=,tpgid=,comm=,ucomm=,command=,lstart=,args=",
        "args=,comm=,pid=",
    ] {
        let args = ["-p", pid.as_str(), "-o", o].map(str::to_string);
        let (fields, rows) = ps_rows(&args, || None, kproc::record).expect("parses");
        assert_eq!(rows.len(), 1, "{o}");
        let ours = ps_line(&rows[0], &fields, 0, &read);
        let theirs = ps_says(o, sleep.0, &[("LC_ALL", "C")]);
        assert_eq!(words(&ours), words(&theirs), "{o}");
    }
}

/// `ps` renders a name outside ASCII in the locale its `setlocale(LC_ALL, "")` makes, and so
/// does the stand-in, reading this Mac's locales: C with nothing set, with a name no locale
/// has, with `UTF-8` (a `LC_CTYPE` alone) or with any one category refused, UTF-8 in a UTF-8
/// locale and C in a US-ASCII one. In ISO8859-1, which neither rendering is, the stand-in
/// prints such a name `-` and an ASCII one as `ps` does.
#[test]
fn a_name_outside_ascii_reads_as_ps_prints_it_in_its_locale() {
    let odd = Sleeping::named("/lab/é-sleep");
    let plain = Sleeping::named("/lab/plain-sleep");
    let comm = |sleep: &Sleeping, env: &[(&str, &str)]| {
        let r = sleep.record();
        let locale = ps_locale(
            |name| {
                env.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_string())
            },
            crate::sys::locale_codeset,
        );
        format!("{}\n", in_locale(locale, |l| kproc::ps_comm(&r, l))).into_bytes()
    };
    let envs: [&[(&str, &str)]; 8] = [
        &[],
        &[("LANG", "en_US.UTF-8")],
        &[("LC_ALL", "C.UTF-8")],
        &[("LANG", "UTF-8")],
        &[("LANG", "bogus")],
        &[("LC_CTYPE", "en_US.UTF-8"), ("LANG", "bogus")],
        &[("LC_ALL", ""), ("LC_CTYPE", "en_US.UTF-8")],
        &[("LANG", "en_US.US-ASCII")],
    ];
    for env in envs {
        for sleep in [&odd, &plain] {
            assert_eq!(comm(sleep, env), ps_says("comm=", sleep.0, env), "{env:?}");
        }
    }
    let latin: &[(&str, &str)] = &[("LANG", "en_US.ISO8859-1")];
    assert_eq!(comm(&odd, latin), b"-\n");
    assert_eq!(comm(&plain, latin), ps_says("comm=", plain.0, latin));
}

/// `ps` prints in the locale `setlocale(LC_ALL, "")` makes: each category the first of
/// `LC_ALL`, its own variable and `LANG` set to a non-empty value, else C; C again when any
/// category's name does not load (an unknown name, or `UTF-8`, which only `LC_CTYPE` loads);
/// else UTF-8 or C by `LC_CTYPE`'s codeset, and no rendering for another codeset. This Mac's
/// loader answers the codesets the stand-in reads.
#[test]
fn the_locale_is_the_one_ps_runs_in() {
    let codeset = |category: &str, name: &str| {
        match name {
            "C" | "POSIX" => Some("US-ASCII"),
            "en_US.UTF-8" | "C.UTF-8" => Some("UTF-8"),
            "UTF-8" if category == "LC_CTYPE" => Some("UTF-8"),
            "en_US.ISO8859-1" => Some("ISO8859-1"),
            _ => None,
        }
        .map(str::to_string)
    };
    let locale = |set: &[(&str, &str)]| {
        ps_locale(
            |name| {
                set.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_string())
            },
            codeset,
        )
    };
    let (c, utf8) = (Some(kproc::Locale::C), Some(kproc::Locale::Utf8));
    assert_eq!(locale(&[]), c);
    assert_eq!(locale(&[("LANG", "en_US.UTF-8")]), utf8);
    assert_eq!(locale(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]), c);
    assert_eq!(
        locale(&[
            ("LC_ALL", ""),
            ("LC_CTYPE", "POSIX"),
            ("LANG", "en_US.UTF-8")
        ]),
        c
    );
    assert_eq!(locale(&[("LC_CTYPE", "C.UTF-8"), ("LANG", "C")]), utf8);
    assert_eq!(locale(&[("LC_CTYPE", "UTF-8"), ("LANG", "C")]), utf8);
    assert_eq!(locale(&[("LANG", "bogus")]), c, "a name no locale has");
    assert_eq!(locale(&[("LANG", "UTF-8")]), c, "a name LC_COLLATE refuses");
    assert_eq!(
        locale(&[("LC_CTYPE", "en_US.UTF-8"), ("LANG", "bogus")]),
        c,
        "one category refused"
    );
    assert_eq!(locale(&[("LANG", "en_US.ISO8859-1")]), None);
    let mac = crate::sys::locale_codeset;
    assert_eq!(mac("LC_CTYPE", "en_US.UTF-8").as_deref(), Some("UTF-8"));
    assert_eq!(mac("LC_CTYPE", "UTF-8").as_deref(), Some("UTF-8"));
    assert_eq!(mac("LC_COLLATE", "UTF-8"), None);
    assert_eq!(mac("LC_CTYPE", "C").as_deref(), Some("US-ASCII"));
    assert_eq!(
        mac("LC_CTYPE", "en_US.ISO8859-1").as_deref(),
        Some("ISO8859-1")
    );
    assert_eq!(mac("LC_CTYPE", "bogus"), None);
    assert_eq!(mac("LC_ALL", "C"), None, "no one category");
}

/// `-p` chooses exactly the pids it lists, as `ps` does: pid 0 (`kernel_task`, which the
/// kernel's table does hold) is never a row, and a list that names no pid is refused rather
/// than read as no `-p` at all. Without `-p`, or with `-A`, every process is chosen. An `-o`
/// list splits on commas and spaces alike, each keyword's header dropped.
#[test]
fn the_pid_list_chooses_exactly_its_pids() {
    let record = |pid: u32| kproc::Record {
        pid,
        ppid: 1,
        pgid: 0,
        tpgid: 0,
        tty: None,
        uid: 501,
        comm: format!("p{pid}"),
        start_s: 1,
        start_us: 0,
        zombie: false,
    };
    let every = || Some(vec![record(0), record(1), record(7), record(8)]);
    let chosen = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        ps_rows(&args, every, |pid| Some(record(pid)))
            .map(|(_, rows)| rows.iter().map(|r| r.pid).collect::<Vec<_>>())
    };
    assert_eq!(chosen(&["-p", "7,8", "-o", "pid="]), Ok(vec![7, 8]));
    assert_eq!(chosen(&["-p", "7  8"]), Ok(vec![7, 8]), "spaces split too");
    assert_eq!(chosen(&["-p", "7", "-p", "8"]), Ok(vec![7, 8]));
    assert_eq!(chosen(&["-p", "0"]), Ok(vec![]), "pid 0 is no row");
    assert_eq!(
        chosen(&["-o", "pid="]),
        Ok(vec![1, 7, 8]),
        "no -p: every process"
    );
    assert_eq!(chosen(&["-A", "-p", "7"]), Ok(vec![1, 7, 8]));
    for refused in ["", "-1", "abc", "7,,8"] {
        assert!(chosen(&["-p", refused]).is_err(), "-p {refused:?}");
    }
    assert!(chosen(&["-p"]).is_err(), "-p with no list");
    let fields =
        |o: &str| ps_rows(&["-o".to_string(), o.to_string()], every, |_| None).map(|(f, _)| f);
    assert_eq!(
        fields("pid=,ppid=PARENT,tt,command="),
        Ok(vec![Field::Pid, Field::Ppid, Field::Tt, Field::Command])
    );
    assert_eq!(
        fields("stat=,pid=,rss="),
        Ok(vec![Field::Stat, Field::Pid, Field::Unread])
    );
    assert_eq!(
        fields("pid, ppid"),
        Ok(vec![Field::Pid, Field::Ppid]),
        "spaces split -o too"
    );
    assert_eq!(
        fields("pid  ppid,,comm\targs"),
        Ok(vec![Field::Pid, Field::Ppid, Field::Comm, Field::Args])
    );
}

#[test]
fn a_request_reads_its_last_user_text_and_a_trailing_tool_output() {
    let body = J::parse(
        r#"{"input":[{"role":"user","content":"first"},{"role":"assistant","content":"x"},
                {"role":"user","content":[{"type":"input_text","text":"a"},{"type":"input_text","text":"b"}]},
                {"type":"function_call_output","output":"done"}]}"#,
    )
    .unwrap();
    let (user, tool) = request_view(&body);
    assert_eq!(user, "b");
    assert_eq!(tool, Some(J::s("done")));
    assert_eq!(
        request_view(&J::parse("{}").unwrap()),
        (String::new(), None)
    );
    assert_eq!(request_marks(&body), (4, "-".to_string()));
    let turned = J::parse(
        r#"{"input":[{"role":"user","content":"x"}],
                "client_metadata":{"turn_id":"t-2","thread_id":"th"}}"#,
    )
    .unwrap();
    assert_eq!(request_marks(&turned), (1, "t-2".to_string()));
    assert_eq!(
        request_marks(&J::parse("{}").unwrap()),
        (0, "-".to_string())
    );
}

#[test]
fn a_reply_s_text_is_its_messages_and_nothing_else() {
    let body = sse(vec![
        message_item("Nothing left: all of it.\nNEEDS YOU: nothing"),
        call_item("exec_command", &J::obj([("cmd", J::s("true"))])),
    ]);
    assert_eq!(
        said_of(&body),
        "Nothing left: all of it.\nNEEDS YOU: nothing"
    );
    let call = sse(vec![call_item("exec_command", &J::obj([]))]);
    assert_eq!(said_of(&call), "");
    assert_eq!(said_of(r#"{"error":{"type":"usage_limit_reached"}}"#), "");
}

#[test]
fn a_stream_carries_each_item_once_and_completes_with_all_of_them() {
    let body = sse(vec![
        message_item("hi"),
        call_item("exec_command", &J::obj([("cmd", J::s("true"))])),
    ]);
    let data: Vec<J> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .collect();
    let kinds: Vec<&str> = data
        .iter()
        .filter_map(|d| d.get("type").and_then(J::as_str))
        .collect();
    assert_eq!(
        kinds,
        [
            "response.created",
            "response.output_item.done",
            "response.output_item.done",
            "response.completed"
        ]
    );
    let output = data[3]
        .get("response")
        .and_then(|r| r.get("output"))
        .and_then(J::as_arr)
        .map_or(0, <[J]>::len);
    assert_eq!(output, 2);
    assert!(jail_policy(4242).contains("localhost:4242"));

    struct Broken {
        left: usize,
        fail_flush: bool,
    }
    impl std::io::Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Err(std::io::Error::other("injected write failure"));
            }
            let n = self.left.min(bytes.len());
            self.left -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush {
                Err(std::io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }
    let dir = aterm_tempfile::tempdir().expect("exclusive response journal");
    let mut log = ApiLog::new(dir.path().join("api.log"), Instant::now());
    let answer = Answered::now((200, "text/event-stream", body));
    let mut bytes = Vec::new();
    let first = log
        .request(vec![
            ("tag".into(), J::s("same")),
            ("status".into(), J::Int(200)),
        ])
        .unwrap();
    write_response(&mut bytes, &answer, || {
        log.response_written(first, answer.status)
    })
    .unwrap();
    assert!(bytes.ends_with(answer.body.as_bytes()));
    for (left, fail_flush) in [(0, false), (bytes.len() - 1, false), (usize::MAX, true)] {
        let id = log
            .request(vec![
                ("tag".into(), J::s("same")),
                ("status".into(), J::Int(200)),
            ])
            .unwrap();
        assert_ne!(id, first, "repeated tags have distinct identities");
        let mut broken = Broken { left, fail_flush };
        assert!(
            write_response(&mut broken, &answer, || log
                .response_written(id, answer.status))
            .is_err()
        );
    }
    let (requests, witnesses) = log.snapshot().unwrap();
    assert_eq!(requests.len(), 4, "the legacy log remains request-only");
    assert_eq!(witnesses.len(), 1);
    assert_eq!(witnesses[0].get("request_id"), Some(&J::Int(first)));
    assert_eq!(
        witnesses[0].get("event").and_then(J::as_str),
        Some("response_written")
    );
    assert_eq!(witnesses[0].get("status"), Some(&J::Int(200)));
    assert!(
        witnesses[0]
            .get("mono_ms")
            .and_then(J::as_i64)
            .is_some_and(|t| t >= 0)
    );
    std::fs::write(&log.path, "torn request\n").unwrap();
    assert!(
        log.snapshot().is_err(),
        "bad evidence must not vanish from a quiet verdict"
    );
}

#[test]
fn ps_times_read_as_ps_prints_them() {
    assert_eq!(etime(59), "00:59");
    assert_eq!(etime(3 * 3600 + 61), "03:01:01");
    assert_eq!(etime(2 * 86_400 + 5), "02-00:00:05");
    assert_eq!(weekday(2026, 10, 15), 4, "a Thursday");
    assert_eq!(weekday(2000, 1, 1), 6, "a Saturday");
}

/// `mcp-standin echo` serves its one tool, `lab_echo`, and answers a call
/// of it with the text back; plain `mcp-standin` (Codex's) still lists
/// none and answers a call with `{}`.
#[test]
fn the_mcp_standin_echoes_only_when_asked_to() {
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lab_echo","arguments":{"text":"hi"}}}"#;
    let parse = |s: Option<String>| J::parse(&s.expect("an answer")).expect("JSON");
    let tools = parse(mcp_reply(list, true));
    let tool = tools
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| match t {
            J::Arr(a) => a.first().cloned(),
            _ => None,
        })
        .expect("one tool");
    assert_eq!(tool.get("name").and_then(J::as_str), Some(MCP_ECHO_TOOL));
    let echoed = parse(mcp_reply(call, true)).py();
    assert!(echoed.contains("echo: hi"), "{echoed}");
    assert!(parse(mcp_reply(list, false)).py().contains("\"tools\": []"));
    let plain = parse(mcp_reply(call, false)).py();
    assert!(!plain.contains("echo:"), "the control: {plain}");
    assert_eq!(
        mcp_reply(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            true
        ),
        None
    );
    assert_eq!(mcp_reply("not json", true), None);
}
