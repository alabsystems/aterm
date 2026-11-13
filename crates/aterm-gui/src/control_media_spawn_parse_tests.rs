// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::spawn_cwd_refusal;
use super::{AgentSpec, SpawnForm, parse_spawn_args, spawn_retired_refusal, split_quoted_tokens};
use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};

#[cfg(unix)]
#[test]
fn a_spawn_whose_cwd_names_no_folder_is_refused_by_name() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-spawn-cwd")
        .tempdir()
        .expect("scratch dir");
    let here = scratch.path().to_str().expect("utf-8 scratch path");
    let gone = format!("{here}/gone");
    let refused = |rest: &str| spawn_cwd_refusal(&parse_spawn_args(rest).expect("parses"));
    assert_eq!(
        refused(&format!("cwd={gone}")),
        Some(format!("ERR no such folder {gone}\n"))
    );
    assert_eq!(
        refused(&format!("connected=controlled place=tab of=s-a cwd={gone}")),
        Some(format!("ERR no such folder {gone}\n")),
        "the connected form too"
    );
    assert_eq!(
        refused(r#"cwd="""#),
        Some("ERR no such folder \"\"\n".to_string()),
        "an empty folder is named as empty, never read as no cwd="
    );
    assert_eq!(
        refused(&format!("cwd={here} split=v")),
        None,
        "a folder that is there"
    );
    assert_eq!(
        refused("split=h"),
        None,
        "no cwd=: the focused pane's folder"
    );
    use std::os::unix::fs::PermissionsExt as _;
    let shut = format!("{here}/shut");
    std::fs::create_dir(&shut).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o600)).unwrap();
    let verdict = refused(&format!("cwd={shut}"));
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    // SAFETY: getuid has no failure mode and takes no arguments.
    if unsafe { libc::getuid() } != 0 {
        assert_eq!(verdict, Some(format!("ERR no access to folder {shut}\n")));
    }
    assert_eq!(
        refused(&format!("cwd={shut}")),
        None,
        "the same folder, opened"
    );
}

#[test]
fn the_summarys_raise_letters_parse_like_splits_do() {
    let raise_of = |rest: &str| match parse_spawn_args(rest) {
        Ok(SpawnForm::Plain { raise, .. }) => raise,
        other => panic!("{rest:?}: {other:?}"),
    };
    for (rest, want) in [
        ("raise=t", Some(true)),
        ("raise=true", Some(true)),
        ("raise=f", Some(false)),
        ("raise=false", Some(false)),
        ("split=v raise=t", Some(true)),
        ("cwd=/tmp raise=f", Some(false)),
        ("", None),
    ] {
        assert_eq!(raise_of(rest), want, "{rest:?}");
    }
    assert!(parse_spawn_args("split=v").is_ok());
    for bad in [
        "raise=",
        "raise=yes",
        "raise=T",
        "raise=True",
        "raise=1",
        "raise=tr",
        "raise=maybe",
        "window=",
        "window=abc",
        "window=-1",
        "window=0x1",
        "window=1 window=x",
        "window=1 raise=maybe",
    ] {
        assert!(parse_spawn_args(bad).is_err(), "{bad:?} is usage");
    }
}

#[test]
fn unquoted_requests_tokenize_exactly_as_whitespace_splitting_did() {
    for rest in [
        "",
        "   ",
        "cwd=/tmp/x",
        "  cwd=/tmp/x  ",
        "cwd=/tmp/x split=v",
        r#"cwd=/tmp/a"b"#,
        r#"cwd=/tmp/a"b"c"#,
        r#"cwd=/tmp/say"hi split=h"#,
    ] {
        let expected: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        assert_eq!(split_quoted_tokens(rest), Some(expected), "{rest:?}");
    }
}

#[test]
fn a_worker_spawn_parses_agent_model_and_its_frame() {
    let agent_of = |rest: &str| match parse_spawn_args(rest) {
        Ok(SpawnForm::Plain { agent, .. }) => agent,
        other => panic!("{rest:?}: {other:?}"),
    };
    assert_eq!(agent_of(""), None);
    assert_eq!(
        agent_of("agent=claude len=12"),
        Some(AgentSpec::Worker {
            agent: aterm_phase::Program::Claude,
            model: None,
            autopilot: None,
            from: None
        })
    );
    assert_eq!(
        agent_of("cwd=/w raise=false agent=claude model=claude-opus-4.1 from=s-ab12 len=3"),
        Some(AgentSpec::Worker {
            agent: aterm_phase::Program::Claude,
            model: Some("claude-opus-4.1".to_string()),
            autopilot: None,
            from: Some(aterm_session::SessionId::new("s-ab12")),
        })
    );
    assert_eq!(
        agent_of("agent=agy model=gemini-2.5-pro- from=s-ab12 len=3"),
        Some(AgentSpec::Worker {
            agent: aterm_phase::Program::Agy,
            model: Some("gemini-2.5-pro-".to_string()),
            autopilot: None,
            from: Some(aterm_session::SessionId::new("s-ab12")),
        })
    );
    let longest = "M".repeat(64);
    assert_eq!(
        agent_of(&format!("model={longest} agent=claude len=1 split=v")),
        Some(AgentSpec::Worker {
            agent: aterm_phase::Program::Claude,
            model: Some(longest.clone()),
            autopilot: None,
            from: None,
        })
    );
    for (word, autopilot) in [
        ("land", aterm_agent::supervise::Autopilot::Land),
        ("cruise", aterm_agent::supervise::Autopilot::Cruise),
    ] {
        assert_eq!(
            agent_of(&format!("agent=claude autopilot={word} len=1")),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Claude,
                model: None,
                autopilot: Some(autopilot),
                from: None,
            })
        );
    }
    for program in [
        aterm_phase::Program::Codex,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
        aterm_phase::Program::Grok,
    ] {
        assert_eq!(
            agent_of(&format!(
                "agent={} model=m-1 autopilot=cruise from=s-ab12 len=1",
                program.name()
            )),
            Some(AgentSpec::Worker {
                agent: program,
                model: Some("m-1".to_string()),
                autopilot: Some(aterm_agent::supervise::Autopilot::Cruise),
                from: Some(aterm_session::SessionId::new("s-ab12")),
            }),
            "a worker of {program:?}"
        );
    }
    let too_long = format!("agent=claude model={longest}x len=1");
    for bad in [
        "agent=",
        "agent=generic",
        "agent=Claude",
        "model=opus",
        "from=s-ab12",
        "len=4",
        "agent=claude model=",
        "agent=claude from=",
        "agent=claude from=ctl",
        "agent=claude from=s-xyz",
        "agent=claude model=a/b",
        "agent=claude model=a$b",
        "agent=claude model=a\u{e9}",
        "agent=claude autopilot=off len=1",
        "agent=claude autopilot= len=1",
        "agent=claude autopilot=Land len=1",
        "agent=claude autopilot=land autopilot=cruise len=1",
        "autopilot=land",
        too_long.as_str(),
        "agent=claude connected=controlled place=tab of=s-a",
        "agent=gemini connected=controlled place=tab of=s-a",
        "agent=Gemini len=1",
        "agent=gemini len=1",
        "agent=gemini-cli len=1",
        "agent=codex-old len=1",
        "agent=muse model=a/b len=1",
        "agent=gemini model=--yolo len=1",
        "agent=muse model=-x len=1",
        "agent=claude model=- len=1",
        "agent=Grok len=1",
        "agent=grok model=--always-approve len=1",
        "agent=kimi len=1",
        "connected=controlled place=tab of=s-a len=3",
    ] {
        assert!(parse_spawn_args(bad).is_err(), "{bad}");
    }
    let usage = super::spawn_usage();
    assert!(
        usage.contains(
            "agent=<claude|codex|agy|muse> [model=<m>] [autopilot=<land|cruise>] \
             [from=<sid>] len=<n>"
        ) || aterm_phase::Program::Grok.supervisable(),
        "{usage}"
    );
}

/// `agent=grok` IS A WORKER WORD: `spawn agent=grok` parses to a Grok Build worker with its
/// model, Autopilot and starter, as every other row's word does (it was `ERR usage` while
/// `WORKERS` had no Grok row); the usage line names it only once a spawn starts it.
#[test]
fn agent_grok_parses_as_a_grok_build_worker() {
    assert_eq!(
        parse_spawn_args("agent=grok model=grok-code-fast-1 from=s-ab12 len=9"),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: None,
            split: None,
            theme: None,
            agent: Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Grok,
                model: Some("grok-code-fast-1".to_string()),
                autopilot: None,
                from: Some(aterm_session::SessionId::new("s-ab12")),
            }),
            why: None,
            req: None,
        })
    );
    assert_eq!(
        crate::task_launch::worker_word(Some("grok")),
        "grok",
        "the frame's replies name grok, not the claude fallback"
    );
    let usage = super::spawn_usage();
    assert_eq!(
        usage.contains("|grok>"),
        aterm_phase::Program::Grok.supervisable(),
        "{usage}"
    );
}

/// `program=` IS A PROGRAM WORKER'S FORM: its program and each `arg=` percent-decoded, one
/// word each whatever it holds, with `from=`. NEGATIVE CONTROLS: a spawn naming no program
/// (none, an empty one, `arg=` alone) is the usage line, as are a program spelled as an option,
/// one starting `~`, a control character in a word, a second `program=`, and a program beside
/// `agent=`, `model=` or `autopilot=`; a program a harness here concludes is refused by name,
/// with its agent form named, and Grok Build and Kimi Code, which none concludes, run.
#[test]
fn program_parses_as_a_program_worker_and_a_spawn_naming_none_is_refused() {
    use crate::program_worker::ProgramRun;
    let program = |rest: &str| match parse_spawn_args(rest) {
        Ok(SpawnForm::Plain {
            agent: Some(AgentSpec::Program { run, from }),
            ..
        }) => Some((run, from)),
        _ => None,
    };
    assert_eq!(
        program("program=/bin/sh arg=-c arg=echo%20it%27s%20done from=s-ab12 len=9"),
        Some((
            ProgramRun {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "echo it's done".to_string()],
            },
            Some(aterm_session::SessionId::new("s-ab12")),
        ))
    );
    assert_eq!(
        program("cwd=/w program=/opt/my%20tools/run arg= len=1"),
        Some((
            ProgramRun {
                program: "/opt/my tools/run".to_string(),
                args: vec![String::new()],
            },
            None,
        )),
        "an empty argument is one"
    );
    for bad in [
        "len=1",
        "program= len=1",
        "arg=x len=1",
        "program=-x len=1",
        "program=~/bin/x len=1",
        "program=a%0Ab len=1",
        "program=sh arg=a%07 len=1",
        "program=sh program=sh len=1",
        "agent=claude program=sh len=1",
        "program=sh agent=claude len=1",
        "program=sh model=m len=1",
        "program=sh autopilot=land len=1",
        "agent=claude arg=x len=1",
    ] {
        assert!(parse_spawn_args(bad).is_err(), "{bad}");
        let reply = super::spawn_form(bad).expect_err(bad);
        assert!(reply.starts_with("ERR usage: spawn"), "{bad}: {reply}");
    }
    for (word, agent) in [
        ("claude", "claude"),
        ("/usr/local/bin/codex", "codex"),
        ("agy", "agy"),
        ("muse", "muse"),
    ] {
        let rest = format!("program={word} len=1");
        let reply = super::spawn_form(&rest).expect_err(&rest);
        assert!(
            reply.starts_with(&format!("ERR spawn program={word}: "))
                && reply.ends_with(&format!(
                    "has a harness here; spawn it as `agent={agent}`\n"
                )),
            "{reply}"
        );
    }
    for free in [
        "program=kimi len=1",
        "program=grok len=1",
        "program=/bin/sh len=1",
    ] {
        assert!(super::spawn_form(free).is_ok(), "{free}");
    }
    assert!(
        super::spawn_usage().contains("| program=<pct> [arg=<pct>]... [from=<sid>] len=<n>]"),
        "the usage line names the form"
    );
}

/// A SPAWN NAMING THE RETIRED GEMINI CLI IS REFUSED BY NAME, with agy, which
/// replaces it, named; never the usage line and never another worker.
#[test]
fn a_spawn_of_the_retired_gemini_cli_is_refused_with_agy_named() {
    for (rest, word) in [
        ("agent=gemini len=3", "gemini"),
        ("cwd=/w agent=Gemini model=m-1 len=1", "Gemini"),
        ("agent=gemini-cli", "gemini-cli"),
    ] {
        assert_eq!(
            spawn_retired_refusal(rest).as_deref(),
            Some(
                format!(
                    "ERR spawn agent={word}: Gemini CLI is no longer supported: agy \
                     replaces it (agent=agy)\n"
                )
                .as_str()
            ),
            "{rest}"
        );
        assert!(
            parse_spawn_args(rest).is_err(),
            "{rest}: no worker of any agent"
        );
    }
    for rest in [
        "agent=agy len=1",
        "agent=geminix len=1",
        "agent=/bin/gemini len=1",
        "",
    ] {
        assert_eq!(spawn_retired_refusal(rest), None, "the control: {rest}");
    }
}

/// A SPAWN'S REFUSAL NAMES THE WORD IT REFUSES: the bare usage line left its caller, a fleet
/// drive's lead above all, to guess which option was refused. The reply stays the usage line
/// (clients read `ERR usage: spawn` as such) and names the word, one line of its first 32
/// characters; a line no one word breaks gets the usage line alone. Every one names
/// `agent=`, so the client never reads it as an instance older than workers
/// (`aterm_ctl::usage_predates_workers`) and tells the person to update a current aterm.
#[test]
fn a_spawn_refusal_names_the_word_it_refuses() {
    let usage = super::spawn_usage();
    assert!(!aterm_ctl::usage_predates_workers(&usage), "{usage:?}");
    let long = format!("identity={}", "y".repeat(400));
    for (rest, word) in [
        (
            "agent=claude autopilot=land autopilot=cruise len=1",
            "autopilot=cruise",
        ),
        ("theme=a theme=b", "theme=b"),
        ("agent=claude model=a/b len=1", "model=a/b"),
        ("agent=claude from=ctl len=1", "from=ctl"),
        ("agent=bogus len=1", "agent=bogus"),
        ("window=abc", "window=abc"),
        ("cwd=/w raise=maybe", "raise=maybe"),
        ("split=v identity=worker", "identity=worker"),
        (long.as_str(), "identity=yyyyyyyyyyyyyyyyyyyyyyy…"),
    ] {
        let reply = super::spawn_form(rest).expect_err(rest);
        assert!(
            reply.starts_with("ERR usage: spawn")
                && reply.contains(&format!("`{word}`"))
                && reply.lines().count() == 1
                && !aterm_ctl::usage_predates_workers(&reply),
            "{rest}: {reply:?}"
        );
    }
    for rest in ["model=opus", "connected=controlled", r#"cwd="/w"#] {
        assert_eq!(super::spawn_form(rest), Err(usage.clone()), "{rest}");
    }
    assert!(super::spawn_form("split=v raise=f").is_ok(), "the control");
}

#[test]
fn plain_spawn_parses_with_and_without_cwd() {
    assert_eq!(
        parse_spawn_args(""),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: None,
            split: None,
            theme: None,
            agent: None,
            why: None,
            req: None,
        })
    );
    assert_eq!(
        parse_spawn_args("cwd=/tmp/x"),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: Some("/tmp/x".to_string()),
            split: None,
            theme: None,
            agent: None,
            why: None,
            req: None,
        })
    );
    assert_eq!(
        parse_spawn_args("cwd=/tmp/x split=v"),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: Some("/tmp/x".to_string()),
            split: Some(crate::pane::SplitDir::Vertical),
            theme: None,
            agent: None,
            why: None,
            req: None,
        })
    );
    assert_eq!(
        parse_spawn_args(r#"cwd="C:\Program Files\Git""#),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: Some(r"C:\Program Files\Git".to_string()),
            split: None,
            theme: None,
            agent: None,
            why: None,
            req: None,
        })
    );
    assert_eq!(
        parse_spawn_args(r#"cwd=/tmp/x why="rebuild the cache""#),
        Ok(SpawnForm::Plain {
            window: None,
            raise: None,
            cwd: Some("/tmp/x".to_string()),
            split: None,
            theme: None,
            agent: None,
            why: Some("rebuild the cache".to_string()),
            req: None,
        }),
        "a request's one line, which its tab's open keeps"
    );
    for bad in [
        "bogus",
        "split=diagonal",
        "identity=worker",
        "identity=-",
        "why=",
    ] {
        assert_eq!(parse_spawn_args(bad), Err(Some(bad.to_owned())), "{bad}");
    }
    assert_eq!(
        parse_spawn_args("why=a why=b"),
        Err(Some("why=b".to_owned())),
        "a second why= is the word refused"
    );
    assert_eq!(parse_spawn_args(r#"cwd="C:\Program Files"#), Err(None));
}

#[test]
fn connected_spawn_requires_the_full_explicit_form() {
    let full = parse_spawn_args("connected=controlled place=tab of=s-abc cwd=/w");
    assert_eq!(
        full,
        Ok(SpawnForm::Connected {
            kind: ConnectedSpawnKind::Controlled,
            place: ConnectedSpawnPlace::Tab,
            origin: aterm_session::SessionId::new("s-abc"),
            cwd: Some("/w".to_string()),
            theme: None,
        })
    );
    assert!(matches!(
        parse_spawn_args("connected=controller place=window of=s-abc"),
        Ok(SpawnForm::Connected {
            kind: ConnectedSpawnKind::Controller,
            place: ConnectedSpawnPlace::Window,
            ..
        })
    ));
    for incomplete in [
        "connected=controlled place=tab",
        "connected=controlled of=s-abc",
        "connected=controlled",
        "place=tab",
        "of=s-abc",
    ] {
        assert_eq!(parse_spawn_args(incomplete), Err(None), "{incomplete}");
    }
    for (rest, word) in [
        ("connected=owner place=tab of=s-abc", "connected=owner"),
        ("connected=controlled place=pane of=s-abc", "place=pane"),
        ("connected=controlled place=tab of=", "of="),
        ("connected=controlled place=tab of=@s-abc", "of=@s-abc"),
    ] {
        assert_eq!(parse_spawn_args(rest), Err(Some(word.to_owned())), "{rest}");
    }
    for knob in [
        "window=1",
        "raise=false",
        "split=v",
        "identity=worker",
        "identity=-",
    ] {
        assert!(
            parse_spawn_args(&format!("connected=controlled place=tab of=s-abc {knob}")).is_err(),
            "{knob}"
        );
    }
}

#[test]
fn spawn_aim_prefers_the_explicit_window_over_the_selector() {
    use super::SpawnAim;
    assert_eq!(SpawnAim::from_request(None, None), SpawnAim::Front);
    assert_eq!(SpawnAim::from_request(None, Some(7)), SpawnAim::Session(7));
    assert_eq!(SpawnAim::from_request(Some(2), None), SpawnAim::Window(2));
    assert_eq!(
        SpawnAim::from_request(Some(2), Some(7)),
        SpawnAim::Window(2)
    );
    assert!(!SpawnAim::Front.is_aimed());
    assert!(SpawnAim::Window(0).is_aimed());
    assert!(SpawnAim::Session(0).is_aimed());
}

#[test]
fn raise_after_spawn_all_four_cells() {
    use super::raise_after_spawn;
    assert!(
        raise_after_spawn(false, None),
        "un-aimed, no verdict: raise (attach)"
    );
    assert!(
        !raise_after_spawn(true, None),
        "aimed, no verdict: do NOT raise"
    );
    assert!(
        raise_after_spawn(true, Some(true)),
        "aimed, raise=true: insist"
    );
    assert!(
        !raise_after_spawn(false, Some(false)),
        "un-aimed, raise=false: hold"
    );
}

/// `req=` JOINS A REQUEST: one id of the shape aterm mints, on a plain or worker spawn. NEGATIVE
/// CONTROLS: a second `req=`, an id aterm never mints, and `req=` on the connected form are each
/// refused, the first two naming the word.
#[test]
fn spawn_takes_one_req_and_refuses_it_on_the_connected_form() {
    let id = "r-20261109-1411-87be5228-01";
    match parse_spawn_args(&format!("cwd=/tmp/x req={id}")) {
        Ok(SpawnForm::Plain { req, .. }) => assert_eq!(req.as_deref(), Some(id)),
        other => panic!("{other:?}"),
    }
    match parse_spawn_args(&format!("agent=claude req={id} len=3")) {
        Ok(SpawnForm::Plain {
            req,
            agent: Some(AgentSpec::Worker { .. }),
            ..
        }) => assert_eq!(req.as_deref(), Some(id)),
        other => panic!("{other:?}"),
    }
    let again = format!("req={id}");
    assert_eq!(
        parse_spawn_args(&format!("req={id} {again}")),
        Err(Some(again))
    );
    for never in ["req=", "req=r-1", "req=r-20261109-1411-87be5228-1"] {
        assert_eq!(
            parse_spawn_args(&format!("cwd=/tmp/x {never}")),
            Err(Some(never.to_owned())),
            "{never}"
        );
    }
    assert_eq!(
        parse_spawn_args(&format!(
            "connected=controlled place=tab of=s-ab12 req={id}"
        )),
        Err(None)
    );
    assert!(super::spawn_usage().contains("[req=<id>]"));
}
