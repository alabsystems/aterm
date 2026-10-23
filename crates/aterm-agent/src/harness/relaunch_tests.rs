// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// The program a relaunch runs is what the process runs, not the word it was typed as: a `#!`
#[cfg(unix)]
#[test]
fn the_program_is_the_script_or_the_image_that_is_claude() {
    let native = Path::new("/h/.local/share/claude/versions");
    let twin = words(&["/bin/sh", "/p/agents/claude", "--model", "opus"]);
    assert_eq!(
        launched(&twin, Some(Path::new("/bin/sh")), native, false),
        Some((
            PathBuf::from("/p/agents/claude"),
            words(&["/p/agents/claude", "--model", "opus"])
        ))
    );
    let npm = words(&["node", "/usr/local/bin/claude", "-c"]);
    assert_eq!(
        launched(&npm, Some(Path::new("/opt/node/bin/node")), native, false).map(|(p, _)| p),
        Some(PathBuf::from("/usr/local/bin/claude"))
    );
    let binary = words(&["claude", "--verbose"]);
    let image = native.join("2.1.281");
    assert_eq!(
        launched(&binary, Some(&image), native, false),
        Some((
            image.clone(),
            vec![
                image.to_string_lossy().into_owned(),
                "--verbose".to_string()
            ]
        ))
    );
    assert!(launched(&binary, Some(Path::new("/s/claude")), native, false).is_some());
    assert!(
        launched(&binary, Some(Path::new("/s/cc-2.1")), native, true).is_some(),
        "Claude registered this very process"
    );
    let wrapper = words(&["caffeinate", "claude"]);
    assert_eq!(
        launched(
            &wrapper,
            Some(Path::new("/usr/bin/caffeinate")),
            native,
            false
        ),
        None
    );
    let relative = words(&["/bin/sh", "agents/claude"]);
    assert_eq!(
        launched(&relative, Some(Path::new("/bin/sh")), native, false),
        None
    );
    assert_eq!(launched(&binary, None, native, true), None, "no image");
    assert_eq!(
        launched(&binary, Some(Path::new("relative/claude")), native, true),
        None
    );
}

/// The decision, whose exit it was: a person's (a keystroke within the grace), a holder's
/// (`hold=1`, a lease, a named driver's turn), else the owner's limit, else nobody's: relaunched.
#[test]
fn an_exit_is_relaunched_unless_someone_owns_it_or_it_is_limited() {
    let st = |tail: &str| format!("OK schema=1 {tail}");
    let nobody = st("hold=0 hand=- human_ms=-");
    assert_eq!(
        on_exit(true, Some(&nobody), 120, false, false),
        OnExit::Relaunch
    );
    assert_eq!(
        on_exit(true, None, 120, false, false),
        OnExit::Relaunch,
        "unreadable"
    );
    let typed = |ms: u64| st(&format!("hold=0 hand=- human_ms={ms}"));
    assert_eq!(
        on_exit(true, Some(&typed(119_999)), 120, false, false),
        OnExit::PersonAsked
    );
    assert_eq!(
        on_exit(true, Some(&typed(120_000)), 120, false, false),
        OnExit::Relaunch
    );
    assert_eq!(
        on_exit(true, Some(&typed(0)), 0, false, false),
        OnExit::Relaunch,
        "no grace"
    );
    for held in [
        "hold=1 hand=- human_ms=-",
        "hold=0 hand=lease:orchestrator human_ms=-",
        "hold=0 hand=turn:41:s-9f00 human_ms=-",
    ] {
        assert_eq!(
            on_exit(true, Some(&st(held)), 120, false, false),
            OnExit::Held,
            "{held}"
        );
        assert_eq!(
            on_exit(false, Some(&st(held)), 120, false, false),
            OnExit::Held,
            "{held}"
        );
    }
    for free in [
        "hold=0 hand=turn:41 human_ms=-",
        "hold=0 hand=driving:s-2 human_ms=-",
    ] {
        assert_eq!(
            on_exit(true, Some(&st(free)), 120, false, false),
            OnExit::Relaunch,
            "{free}"
        );
    }
    assert_eq!(
        on_exit(false, Some(&nobody), 120, false, false),
        OnExit::Limited
    );
    assert_eq!(on_exit(false, None, 120, false, false), OnExit::Limited);
    assert_eq!(
        on_exit(false, Some(&typed(1)), 120, false, false),
        OnExit::PersonAsked,
        "a person's exit is theirs, never said as a limit"
    );
    assert_eq!(
        on_exit(true, Some(&typed(1)), 120, true, false),
        OnExit::Asked
    );
    for (held, holder) in [
        ("hold=1 hand=- human_ms=1", true),
        ("hold=0 hand=lease:orchestrator human_ms=-", true),
        ("hold=0 hand=turn:41:s-9f00 human_ms=-", true),
        ("hold=0 hand=turn:41 human_ms=-", false),
        ("hold=0 hand=driving:s-9f00 human_ms=-", false),
    ] {
        assert_eq!(
            on_exit(true, Some(&st(held)), 120, true, false),
            OnExit::Asked,
            "{held}"
        );
        assert!(hand_on_tab(&st(held)), "{held}: what the host waits out");
        assert_eq!(
            held_by_someone(&st(held)),
            holder,
            "{held}: an unasked exit is left to it"
        );
    }
    let ours = format!(
        "hold=0 hand=lease:{} human_ms=-",
        super::super::upgrade_drive::hand_name()
    );
    for free in [
        "hold=0 hand=- human_ms=1",
        "hold=0 hand=- human_ms=- input=3",
        ours.as_str(),
    ] {
        assert!(!hand_on_tab(&st(free)), "{free}: no hand that waits");
    }
    assert_eq!(
        on_exit(false, Some(&typed(1)), 120, true, false),
        OnExit::Limited
    );
    for status in [
        Some(nobody.as_str()),
        Some(typed(1).as_str()),
        Some("OK schema=1 hold=1 hand=- human_ms=-"),
        Some("OK schema=1 hold=0 hand=lease:orchestrator human_ms=-"),
        None,
    ] {
        for allowed in [true, false] {
            assert_eq!(
                on_exit(allowed, status, 120, false, true),
                OnExit::Restarted,
                "{status:?} allowed={allowed}"
            );
        }
    }
    let ok = "OK schema=1 hold=0 hand=- human_ms=5000 level=quiet";
    assert!(person_present(ok, 120));
    assert!(!person_present(ok, 5));
    assert!(!person_present(&ok.replace("5000", "-"), 120), "never");
    assert!(
        !person_present("OK schema=1 hold=0", 120),
        "an older server"
    );
    assert!(!person_present("ERR no such session human_ms=1", 120));
}

/// One step's word, read for the host: relaunched; left (the launch's own end, or no exit at all),
/// journaled and never said; never (an interactive conversation that cannot come back), said.
#[test]
fn a_step_is_relaunched_left_not_yet_or_never() {
    assert_eq!(
        outcome("adopted"),
        Outcome::Relaunched,
        "its loop carries on"
    );
    assert_eq!(outcome("done:fresh"), Outcome::Relaunched);
    assert_eq!(
        outcome("done:resumed"),
        Outcome::Relaunched,
        "the crash rule's relaunch of a self-claimed codex-alab landed: its TUI leads the tab"
    );
    assert_eq!(
        outcome("done:no-continue"),
        Outcome::NotYet("done:no-continue".to_string()),
        "a carry-on that typed nothing is no relaunch"
    );
    assert_eq!(
        outcome("wait:not-exited"),
        Outcome::Left("not-exited".to_string())
    );
    assert_eq!(
        outcome("ended:one-shot"),
        Outcome::Left("one-shot".to_string())
    );
    assert_eq!(
        outcome("ended:no-conversation"),
        Outcome::Left("no-conversation".to_string())
    );
    assert_eq!(
        outcome("refused:not-resumable --worktree"),
        Outcome::Cannot("not-resumable --worktree".to_string())
    );
    assert_eq!(
        outcome("refused:shell-gone"),
        Outcome::Cannot("shell-gone".to_string()),
        "the word after_exit gives a gone shell"
    );
    assert_eq!(
        outcome("busy:state-unwritable"),
        Outcome::Cannot("state-unwritable".to_string())
    );
    assert_eq!(outcome("busy:another-sweep"), Outcome::Busy);
    assert_eq!(outcome(WAIT_ACT_ENDS), Outcome::Busy);
    assert_eq!(outcome(WAIT_HAND_LIFTS), Outcome::Busy);
    for step in [
        "wait:held",
        "wait:shell-prompt",
        "wait:resume",
        "failed:no-resume",
        "failed:relaunch:ERR-busy",
        "wait:tab-ownership-changed",
    ] {
        assert_eq!(outcome(step), Outcome::NotYet(step.to_string()), "{step}");
    }
}

/// The relaunched agent reads as ended only once its line's command was seen to run and every look
/// for `ended_at_once` since found its shell holding the tab, looks `shell_look` apart (the
/// production clock's 5 s and 1 s, [`super::super::Timings::PRODUCTION`]): by the
/// kernel's witness (a look found the shell not holding its terminal) or, with the line's block,
/// by that block being over ([`line_over`]).
#[test]
fn a_shell_that_has_the_tab_back_reads_as_ended_only_once_its_line_ran() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut back = ShellBack::after(None, &super::super::Timings::PRODUCTION);
    assert!(back.due(at(0)));
    assert!(!back.due(at(400)), "not at the roster's pace");
    assert!(back.due(at(1_000)));
    for s in 0..60 {
        assert!(
            !back.saw(Some(true), None, at(s * 1_000)),
            "held throughout: its line never seen to run"
        );
    }
    let t = 60_000;
    assert!(!back.saw(Some(false), None, at(t)), "the agent holds it");
    assert!(!back.saw(Some(true), None, at(t + 1_000)), "held just now");
    assert!(
        !back.saw(None, None, at(t + 3_000)),
        "an unread look is no verdict"
    );
    assert!(
        !back.saw(Some(false), None, at(t + 4_000)),
        "the agent holds it again"
    );
    assert!(
        !back.saw(Some(true), None, at(t + 5_000)),
        "the count started over"
    );
    assert!(!back.saw(None, None, at(t + 9_000)));
    assert!(!back.saw(Some(true), None, at(t + 9_999)));
    assert!(
        back.saw(Some(true), None, at(t + 10_000)),
        "held five seconds in a row"
    );
    let mut never = ShellBack::after(None, &super::super::Timings::PRODUCTION);
    for s in 0..60 {
        assert!(
            !never.saw(None, None, at(s * 1_000)),
            "never read, never ended"
        );
    }
    let mut marked = ShellBack::after(Some(7), &super::super::Timings::PRODUCTION);
    assert!(
        !marked.saw(Some(false), Some(false), at(0)),
        "the agent runs"
    );
    for s in 1..60 {
        assert!(
            !marked.saw(Some(true), Some(false), at(s * 1_000)),
            "held, its block not over: the agent left it no end"
        );
    }
    let t = 60_000;
    assert!(
        !marked.saw(Some(true), None, at(t)),
        "an unread block is no verdict"
    );
    assert!(
        !marked.saw(Some(true), Some(true), at(t + 1_000)),
        "over just now"
    );
    assert!(
        !marked.saw(Some(true), None, at(t + 3_000)),
        "unread: the count stands"
    );
    assert!(
        !marked.saw(Some(false), Some(true), at(t + 4_000)),
        "something else holds the terminal"
    );
    assert!(!marked.saw(Some(true), Some(true), at(t + 5_000)));
    assert!(
        marked.saw(Some(true), Some(true), at(t + 10_000)),
        "over, and held five seconds since"
    );
}

/// A one-shot run is read wherever its flag stands — after a prompt, after a
/// flag that takes a value, after a flag the table does not know — and its
/// exit is its own end.
#[test]
fn a_one_shot_run_is_read_wherever_its_flag_stands() {
    for argv in [
        &["claude", "-p", "fix the bug"][..],
        &["claude", "fix the bug", "--print"],
        &[
            "claude",
            "--model",
            "opus",
            "--output-format",
            "json",
            "-p",
            "x",
        ],
        &["claude", "--from-a-newer-build", "-p", "x"],
        &["claude", "--version"],
        &["claude", "-h"],
    ] {
        assert!(upgrade::one_shot(&words(argv)), "{argv:?}");
    }
    for argv in [
        &["claude", "--model", "opus"][..],
        &["claude", "--append-system-prompt", "-p"],
        &["claude", "--", "-p"],
        &["claude", "mcp", "list"],
    ] {
        assert!(!upgrade::one_shot(&words(argv)), "{argv:?}");
    }
}

/// Whether an exit would be relaunched on its conversation, from the snapshot alone
/// ([`resumes_on_exit`]): only where the relaunch's plan would be made.
#[test]
fn an_exit_is_relaunched_only_where_the_relaunch_would_plan_it() {
    const ID: &str = "0badf00d-1111-2222-3333-444455556666";
    let snap = |argv: &[&str]| Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(argv),
        session: Some(ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.283".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(upgrade::Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let known = snap(&["/opt/claude", "--model", "opus"]);
    assert!(
        resumes_on_exit(Path::new("/Users//someone"), &known),
        "a launch the relaunch carries"
    );
    for dialect in [upgrade::Dialect::Bash, upgrade::Dialect::Fish] {
        let other = Snapshot {
            dialect: Some(dialect),
            ..known.clone()
        };
        assert!(
            resumes_on_exit(Path::new("/Users//someone"), &other),
            "{dialect:?}"
        );
    }

    let unknown = ["/opt/claude", "--permission-prompt-tool", "mcp__x"];
    assert!(
        upgrade::rewrite_argv(&words(&unknown), ID).is_err(),
        "the premise: the relaunch refuses this argv"
    );
    assert!(
        !resumes_on_exit(Path::new("/Users//someone"), &snap(&unknown)),
        "argv:unknown-flag"
    );
    assert_eq!(
        super::super::resume::command(&words(&unknown), ID).as_deref(),
        Some("claude --resume 0badf00d-1111-2222-3333-444455556666"),
        "the person's command still resumes the conversation"
    );
    assert!(
        !resumes_on_exit(
            Path::new("/Users//someone"),
            &snap(&["/opt/claude", "--worktree"])
        ),
        "argv:not-resumable"
    );
    assert!(
        !resumes_on_exit(
            Path::new("/Users//someone"),
            &snap(&["/opt/claude", "-p", "x"])
        ),
        "one-shot"
    );
    let nushell = Snapshot {
        dialect: None,
        ..known.clone()
    };
    assert!(
        !resumes_on_exit(Path::new("/Users//someone"), &nushell),
        "no dialect: plan waits for ever"
    );
    let unregistered = Snapshot {
        session: None,
        ..known.clone()
    };
    assert!(
        !resumes_on_exit(Path::new("/Users//someone"), &unregistered),
        "no conversation to resume"
    );
    let control = Snapshot {
        cwd: "/w\nx".to_string(),
        ..known
    };
    assert!(
        !resumes_on_exit(Path::new("/Users//someone"), &control),
        "no line with a control character"
    );
}

/// Following the snapshot's agent: Claude's own record of the same process (pid and kernel start)
/// moves its conversation, build and directory (`/clear` names a new conversation under the same
/// pid), and the tab's published foreground group says whether the snapshot still describes the
/// tab.
#[test]
fn a_snapshot_follows_its_own_process_and_nothing_else() {
    let home = std::env::temp_dir().join(format!("aterm-relaunch-follow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("home");
    let start = "Thu Sep 24 01:02:03 2026";
    let snap = Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: start.to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude"]),
        session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.281".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(upgrade::Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let record = |session: &str, at: &str| {
        std::fs::write(
            home.join(".claude/sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":"/w2","version":"2.1.282","status":"idle","statusUpdatedAt":1,"procStart":"{at}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("record");
    };
    let cleared = "0badf00d-7777-2222-3333-444455556666";
    record(cleared, start);
    let mut followed = snap.clone();
    assert!(follow(&home, &mut followed));
    assert_eq!(followed.session.as_deref(), Some(cleared));
    assert_eq!(
        (followed.version.as_deref(), followed.cwd.as_str()),
        (Some("2.1.282"), "/w2")
    );
    assert!(!follow(&home, &mut followed), "nothing moved since");
    record(cleared, "Mon Jan  1 00:00:00 2024");
    let mut other = snap.clone();
    assert!(
        !follow(&home, &mut other),
        "another start is another process"
    );
    assert_eq!(other, snap);
    record("not-a-session", start);
    let mut bad = snap.clone();
    let _ = follow(&home, &mut bad);
    assert_eq!(bad.session, snap.session, "no session id: kept");
    assert_eq!(foreground(&snap, 4242), Foreground::Agent);
    assert_eq!(foreground(&snap, 4241), Foreground::Shell);
    assert_eq!(foreground(&snap, 5000), Foreground::Other);
    let _ = std::fs::remove_dir_all(&home);
}

/// The relaunch's build obeys `[harness] upgrade`: on, a crashed session comes back on the newest
/// build it may move to; off, on the program it ran.
#[test]
fn a_relaunch_moves_to_a_newer_build_only_while_the_upgrade_is_on() {
    let v = |s: &str| Version::parse(s).expect("version");
    let managed = upgrade::Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: v("2.1.282"),
        source: upgrade::Source::Managed,
    };
    let ran = Path::new("/opt/claude");
    let running = v("2.1.281");
    let pick = |on: bool, running: Option<&Version>, c: &[upgrade::Candidate]| {
        relaunch_target(on, running, c, |_| false, ran, Some("2.1.281"))
    };
    let same = (ran.to_path_buf(), "2.1.281".to_string(), "same".to_string());
    assert_eq!(
        pick(true, Some(&running), std::slice::from_ref(&managed)),
        (
            managed.exe.clone(),
            "2.1.282".to_string(),
            "managed".to_string()
        )
    );
    assert_eq!(
        pick(false, Some(&running), std::slice::from_ref(&managed)),
        same,
        "upgrade = false: never onto another build"
    );
    assert_eq!(pick(true, Some(&running), &[]), same, "nothing newer");
    assert_eq!(
        pick(true, None, std::slice::from_ref(&managed)),
        same,
        "no running version to compare"
    );
}

/// The back-off grows 1 s, 10 s, 60 s, then ten minutes for as long as the agent keeps exiting, and
/// starts over once a relaunched agent ran [`HEALTHY`]ly.
#[test]
fn the_back_off_grows_caps_and_forgives_a_healthy_run() {
    let t0 = Instant::now();
    let mut r = Relaunches::default();
    let mut pauses = Vec::new();
    for n in 0..6u64 {
        let now = t0 + Duration::from_secs(n);
        pauses.push(r.exited(OnExit::Relaunch, now).map(|p| p.as_secs()));
        assert!(r.pending());
        assert_eq!(r.attempted(&Outcome::Relaunched, now), Say::Nothing);
    }
    assert_eq!(pauses, [1, 10, 60, 600, 600, 600].map(Ok));
    assert_eq!(
        r.exited(OnExit::Relaunch, t0 + Duration::from_secs(60)),
        Ok(Duration::from_secs(600))
    );
    assert_eq!(
        r.exited(OnExit::Relaunch, t0 + Duration::from_secs(5) + HEALTHY),
        Ok(Duration::from_secs(1))
    );
    assert_eq!(r.exited(OnExit::PersonAsked, t0), Err(Say::Nothing));
    assert!(!r.pending());
    assert!(r.counts().0, "left to the person");
    assert_eq!(r.exited(OnExit::Limited, t0), Err(Say::Limited));
    assert!(!r.pending() && r.counts().1, "badged");
    assert_eq!(r.running(t0), Say::Clear, "an agent back clears it");
    let _ = r.exited(OnExit::Relaunch, t0);
    assert_eq!(
        r.attempted(&Outcome::Left("one-shot".to_string()), t0),
        Say::Nothing
    );
    assert!(!r.pending() && r.counts().0 && !r.counts().1);
    let mut m = Relaunches::default();
    let _ = m.exited(OnExit::Relaunch, t0);
    let miss = Outcome::NotYet("wait:shell-prompt".to_string());
    let said: Vec<Say> = (0..BADGE_AFTER + 1)
        .map(|_| m.attempted(&miss, t0))
        .collect();
    assert_eq!(said.last(), Some(&Say::Nothing), "said once: {said:?}");
    assert_eq!(
        said.iter().filter(|s| **s == Say::Failing).count(),
        1,
        "{said:?}"
    );
    assert_eq!(
        said[usize::try_from(BADGE_AFTER - 1).unwrap()],
        Say::Failing
    );
    assert!(m.pending(), "still tried");
    assert_eq!(m.attempted(&Outcome::Relaunched, t0), Say::Clear);
    let _ = m.exited(OnExit::Relaunch, t0);
    assert_eq!(
        m.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
        Say::Cannot
    );
    assert!(!m.pending());
    assert_eq!(
        m.running(t0),
        Say::Clear,
        "a person's agent clears the word"
    );
}

/// THE CRASH BACK-OFF COUNTS CRASHES (LC-6 of the 2026-10-15 sweep): an exit
/// someone asked for — a person's `restart` (a keystroke just before it or
/// none), the harness's own restart — lands without climbing the crash
/// back-off or restarting the healthy run that forgives it, and its own
/// misses are paced from the first step, whatever crashes came before.
#[test]
fn an_exit_someone_asked_for_is_invisible_to_the_crash_back_off() {
    let st = |tail: &str| format!("OK schema=1 {tail}");
    let nobody = st("hold=0 hand=- human_ms=-");
    let typed = st("hold=0 hand=- human_ms=1500");
    let crash = on_exit(true, Some(&nobody), 120, false, false);
    let asked = [
        on_exit(true, Some(&typed), 120, true, false),
        on_exit(true, Some(&nobody), 120, true, false),
        on_exit(true, Some(&nobody), 120, false, true),
    ];
    let land = |r: &mut Relaunches, decision: OnExit, at: Instant| {
        assert!(r.exited(decision, at).is_ok(), "{decision:?} due");
        assert_eq!(r.attempted(&Outcome::Relaunched, at), Say::Nothing);
    };
    let secs = Duration::from_secs;
    let t0 = Instant::now();
    for history in 0..=4u64 {
        let mut plain = Relaunches::default();
        let mut last = t0;
        for n in 0..history {
            last = t0 + secs(30 * n);
            land(&mut plain, crash, last);
        }
        let mut with = plain.clone();
        let mut at = last;
        for decision in asked {
            at += secs(15);
            land(&mut with, decision, at);
        }
        at += secs(15);
        assert!(with.exited(asked[1], at).is_ok());
        assert_eq!(
            with.attempted(&outcome("wait:shell-prompt"), at),
            Say::Nothing
        );
        assert_eq!(with.pause(), BACKOFF[1], "history {history}");
        assert_eq!(with.attempted(&Outcome::Relaunched, at), Say::Nothing);
        for judged in [last + secs(60), last + HEALTHY + secs(1)] {
            assert_eq!(
                with.clone().exited(crash, judged),
                plain.clone().exited(crash, judged),
                "history {history}, judged {:?} after the last crash",
                judged - last
            );
        }
        if history < 3 {
            let mut more = plain.clone();
            land(&mut more, crash, last + secs(15));
            assert_ne!(
                more.exited(crash, last + secs(60)),
                plain.clone().exited(crash, last + secs(60)),
                "history {history}: a crash is a step"
            );
        }
    }
    let hour = secs(3600);
    let day = 24 * hour;
    let missed = || {
        let mut r = Relaunches::default();
        assert!(r.exited(crash, t0).is_ok());
        for _ in 0..3 {
            let _ = r.attempted(&outcome("wait:shell-prompt"), t0);
        }
        assert_eq!(r.counts().2, 3, "three crash steps, no landing");
        r
    };
    let asked_lands = |r: &mut Relaunches, at: Instant| {
        assert!(r.exited(asked[1], at).is_ok());
        let _ = r.attempted(&Outcome::Relaunched, at);
        assert!(!r.pending(), "landed");
    };
    let climbed = Ok(BACKOFF[3]);
    let forgiven = Ok(BACKOFF[0]);
    let mut resumed = missed();
    let _ = resumed.running(t0 + hour);
    assert_eq!(resumed.clone().exited(crash, t0 + hour + secs(60)), climbed);
    asked_lands(&mut resumed, t0 + day);
    assert_eq!(resumed.clone().exited(crash, t0 + 2 * day), forgiven);
    let mut landed = missed();
    asked_lands(&mut landed, t0 + day);
    assert_eq!(landed.clone().exited(crash, t0 + day + secs(60)), climbed);
    assert_eq!(landed.clone().exited(crash, t0 + 2 * day), forgiven);
    let mut early = missed();
    let _ = early.running(t0 + hour);
    asked_lands(&mut early, t0 + hour + secs(60));
    assert_eq!(early.exited(crash, t0 + hour + HEALTHY + secs(1)), forgiven);
}

/// THE WORD OF A RELAUNCH THAT KEEPS FAILING ENDS WITH ITS TRYING (the review
/// of the r20 relaunch lane, 2026-10-16): the word said once a relaunch has
/// missed [`BADGE_AFTER`] in a row — the tab's mark, "aterm still tries",
/// naming the conversation it asks a person to resume ([`Relaunches::ask`])
/// — is that relaunch's, and goes when it is tried no more: the launch's own
/// end, a person or a holder back at the tab, another agent running there
/// ([`Say::Clear`]); a limit or a refusal says its own word in its place,
/// asking for nothing until the host names it again.
#[test]
fn a_failing_relaunchs_word_ends_with_its_trying() {
    const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";
    let t0 = Instant::now();
    let miss = Outcome::NotYet("wait:shell-prompt".to_string());
    let failing = || {
        let mut r = Relaunches::default();
        assert!(r.exited(OnExit::Relaunch, t0).is_ok());
        let said: Vec<Say> = (0..BADGE_AFTER).map(|_| r.attempted(&miss, t0)).collect();
        assert_eq!(said.last(), Some(&Say::Failing), "{said:?}");
        r.ask(CONVERSATION);
        assert_eq!(r.asks(), Some(CONVERSATION));
        r
    };
    let word = |r: &Relaunches| (r.asks().map(str::to_string), r.counts().1);
    let mut left = failing();
    assert_eq!(
        left.attempted(&Outcome::Left("one-shot".to_string()), t0),
        Say::Clear
    );
    assert_eq!(word(&left), (None, false), "the launch's own end");
    for decision in [OnExit::PersonAsked, OnExit::Held] {
        let mut r = failing();
        assert_eq!(r.decide(decision), Some(Say::Clear), "{decision:?}");
        assert_eq!(word(&r), (None, false), "{decision:?}");
    }
    let mut runs = failing();
    assert_eq!(runs.running(t0), Say::Clear);
    assert_eq!(word(&runs), (None, false), "another agent runs");
    let mut limited = failing();
    assert_eq!(limited.decide(OnExit::Limited), Some(Say::Limited));
    assert_eq!(word(&limited), (None, true), "a limit");
    let mut cannot = failing();
    assert_eq!(
        cannot.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
        Say::Cannot
    );
    assert_eq!(word(&cannot), (None, true), "a refusal");
    let mut gone = Relaunches::default();
    assert!(gone.exited(OnExit::Relaunch, t0).is_ok());
    assert_eq!(
        gone.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
        Say::Cannot
    );
    gone.ask(CONVERSATION);
    assert_eq!(gone.running(t0), Say::Nothing);
    assert!(gone.exited(OnExit::Relaunch, t0).is_ok());
    assert_eq!(
        gone.attempted(&Outcome::Left("one-shot".to_string()), t0),
        Say::Nothing
    );
    assert_eq!(word(&gone), (Some(CONVERSATION.to_string()), true));
}

/// A RELAUNCH THAT KEEPS FAILING BESIDE A WORD THAT ASKS FOR A CONVERSATION
/// ENDS WITH ITS TRYING TOO (the review of the harness landing's eighth
/// merge): the word of an agent before it that cannot be brought back keeps
/// the attention, so this relaunch's failing is said beside it, once (the
/// digest's line, "aterm still tries").
#[test]
fn a_failing_said_beside_a_word_that_asks_ends_with_its_trying() {
    const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";
    let t0 = Instant::now();
    let miss = Outcome::NotYet("wait:shell-prompt".to_string());
    let beside = || {
        let mut r = Relaunches::default();
        assert!(r.exited(OnExit::Relaunch, t0).is_ok());
        assert_eq!(
            r.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
            Say::Cannot
        );
        r.ask(CONVERSATION);
        assert_eq!(
            r.running(t0),
            Say::Nothing,
            "another agent: the word stands"
        );
        assert!(r.exited(OnExit::Relaunch, t0).is_ok());
        let said: Vec<Say> = (0..BADGE_AFTER).map(|_| r.attempted(&miss, t0)).collect();
        assert_eq!(said.last(), Some(&Say::Failing), "{said:?}");
        assert!(r.failing_said() && !r.failing(), "said beside the word");
        r
    };
    let word = |r: &Relaunches| (r.asks().map(str::to_string), r.counts().1, r.failing_said());
    let standing = (Some(CONVERSATION.to_string()), true, false);
    let mut left = beside();
    assert_eq!(
        left.attempted(&Outcome::Left("one-shot".to_string()), t0),
        Say::Clear
    );
    assert_eq!(word(&left), standing, "the launch's own end");
    for decision in [OnExit::PersonAsked, OnExit::Held] {
        let mut r = beside();
        assert_eq!(r.decide(decision), Some(Say::Clear), "{decision:?}");
        assert_eq!(word(&r), standing, "{decision:?}");
    }
    let mut runs = beside();
    assert_eq!(runs.running(t0), Say::Clear);
    assert_eq!(word(&runs), standing, "another agent runs");
    assert_eq!(runs.running(t0), Say::Nothing, "ended once");
    let mut cannot = beside();
    assert_eq!(
        cannot.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
        Say::Cannot
    );
    assert_eq!(word(&cannot), standing, "a refusal writes over it");
    let mut lands = beside();
    assert_eq!(lands.attempted(&Outcome::Relaunched, t0), Say::Clear);
    assert_eq!(word(&lands), (None, false, false), "a landing ends both");
}

/// Drive the real [`Relaunches`], [`on_exit`] and [`outcome`] through every action of
/// `HarnessRelaunchOnExit`: after each, the state is the model's and every invariant holds,
/// including for the harness's own restart (`Restarted`) and the explicit restart's
/// (`Asked`), which the crash back-off neither paces nor counts until its stamp lapses
/// (`Lapse`).
#[test]
fn the_real_relaunch_decisions_conform_to_the_model() {
    let model = aterm_models_harness::harness_relaunch_on_exit_model();
    let konst = |name: &str| {
        model
            .consts
            .iter()
            .find(|c| c.0 == name)
            .map(|c| c.1)
            .expect(name)
    };
    assert_eq!(konst("Badge"), i64::from(BADGE_AFTER), "the model's badge");
    assert_eq!(
        konst("Steps"),
        i64::try_from(BACKOFF.len() - 1).expect("small"),
        "the model's back-off steps"
    );
    let badge = u32::try_from(konst("Badge")).expect("small");
    let mut running = true;
    let mut base = 0i64;
    let mut r = Relaunches::default();
    let mut now = Instant::now();
    const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";
    let steps = i64::try_from(BACKOFF.len() - 1).expect("small");
    let project = |r: &Relaunches, running: bool, base: i64| {
        let (left, badged, streak, misses) = r.counts();
        let step = BACKOFF
            .iter()
            .position(|p| *p == r.pause())
            .expect("a back-off step");
        [
            ("running", i64::from(running)),
            ("asked", i64::from(left)),
            ("pending", i64::from(r.pending())),
            ("badged", i64::from(badged)),
            ("step", i64::try_from(step).expect("small")),
            ("misses", i64::from(misses.min(badge))),
            ("restarted", i64::from(r.restarted())),
            ("crash", i64::from(r.crash())),
            ("streak", i64::from(streak).min(steps)),
            ("base", base),
            ("asks", i64::from(r.asks().is_some())),
            ("failing", i64::from(r.failing())),
            ("said", i64::from(r.failing_said())),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<&'static str, i64>>()
    };
    let streak_of = |r: &Relaunches| i64::from(r.counts().2).min(steps);
    let mut expect = model.init_state();
    let status = |tail: &str| format!("OK schema=1 {tail}");
    let nobody = status("hold=0 hand=- human_ms=-");
    let person = status("hold=0 hand=- human_ms=2000");
    let halted = status("hold=1 hand=- human_ms=-");
    let leased = status("hold=0 hand=lease:orchestrator human_ms=-");
    let schedule = [
        ("Asked", "person"),
        ("Miss", "wait:shell-prompt"),
        ("Land", "adopted"),
        ("Asked", "halted"),
        ("Land", "adopted"),
        ("Restarted", "limited"),
        ("Land", "done:fresh"),
        ("Asked", "nobody"),
        ("PersonReturns", "claim"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Cannot", "refused:shell-gone"),
        ("PersonRuns", ""),
        ("Healthy", ""),
        ("Asked", "nobody"),
        ("Miss", "wait:shell-prompt"),
        ("Lapse", ""),
        ("Miss", "wait:shell-prompt"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Stutter", "busy:another-sweep"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("PersonReturns", "person"),
        ("PersonRuns", ""),
        ("Healthy", ""),
        ("PersonExit", "person"),
        ("PersonRuns", ""),
        ("PersonExit", "halted"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("PersonReturns", "leased"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Cannot", "refused:not-resumable --worktree"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("AskedRuns", ""),
        ("Asked", "person"),
        ("Miss", "wait:shell-prompt"),
        ("Land", "adopted"),
        ("Asked", "halted"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ask", ""),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Ended", "ended:one-shot"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Ended", "wait:not-exited"),
        ("PersonRuns", ""),
        ("Restarted", "person"),
        ("Miss", "wait:held"),
        ("Land", "adopted"),
        ("Restarted", "halted"),
        ("Cannot", "refused:stale-exit"),
        ("PersonRuns", ""),
        ("Restarted", "limited"),
        ("Land", "done:fresh"),
        ("Limited", "exit"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Limited", "pause"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ask", ""),
        ("Ended", "ended:one-shot"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ask", ""),
        ("PersonReturns", "leased"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ask", ""),
        ("Limited", "pause"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ask", ""),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("AskedRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Ended", "ended:one-shot"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("PersonReturns", "person"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("PersonReturns", "claim"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Limited", "pause"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Cannot", "refused:shell-gone"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Cannot", "refused:shell-gone"),
        ("Ask", ""),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Miss", "wait:shell-prompt"),
        ("Land", "adopted"),
    ];
    for (action, input) in schedule {
        match (action, input) {
            ("Restarted", who) => {
                running = false;
                let (allowed, who) = match who {
                    "person" => (true, &person),
                    "halted" => (true, &halted),
                    _ => (false, &nobody),
                };
                assert!(
                    r.exited_as(on_exit(allowed, Some(who), 120, false, true), false, now)
                        .is_ok()
                );
                base = streak_of(&r);
                assert_eq!(
                    r.decide(on_exit(allowed, Some(who), 120, false, true)),
                    None,
                    "still due"
                );
            }
            ("Crash", _) => {
                running = false;
                assert!(
                    r.exited(on_exit(true, Some(&nobody), 120, false, false), now)
                        .is_ok()
                );
                base = streak_of(&r);
            }
            ("Asked", who) => {
                running = false;
                let who = match who {
                    "person" => &person,
                    "halted" => &halted,
                    _ => &nobody,
                };
                let decision = on_exit(true, Some(who), 120, true, false);
                assert_eq!(decision, OnExit::Asked);
                assert!(r.exited(decision, now).is_ok());
                base = streak_of(&r);
                assert_eq!(r.decide(decision), None, "still due");
            }
            ("Lapse", _) => {
                assert_eq!(
                    r.decide(on_exit(true, Some(&nobody), 120, false, false)),
                    None
                );
            }
            ("PersonReturns", "claim") => {
                let said = if expect["failing"] == 1 || expect["said"] == 1 {
                    Say::Clear
                } else {
                    Say::Nothing
                };
                assert_eq!(r.decide(OnExit::Held), Some(said));
            }
            ("PersonExit", who) => {
                running = false;
                let who = if who == "person" { &person } else { &halted };
                assert_eq!(
                    r.exited(on_exit(true, Some(who), 120, false, false), now),
                    Err(Say::Nothing)
                );
                base = streak_of(&r);
            }
            ("Ask", _) => r.ask(CONVERSATION),
            ("AskedRuns", _) => {
                assert_eq!(r.conversation_runs(CONVERSATION), Say::Clear);
            }
            ("PersonReturns", who) => {
                let who = if who == "person" { &person } else { &leased };
                let said = if expect["failing"] == 1 || expect["said"] == 1 {
                    Say::Clear
                } else {
                    Say::Nothing
                };
                assert_eq!(
                    r.decide(on_exit(true, Some(who), 120, false, false)),
                    Some(said)
                );
            }
            ("Limited", "exit") => {
                running = false;
                assert_eq!(
                    r.exited(on_exit(false, Some(&nobody), 120, false, false), now),
                    Err(Say::Limited)
                );
                base = streak_of(&r);
            }
            ("Limited", _) => {
                assert_eq!(
                    r.decide(on_exit(false, Some(&nobody), 120, false, false)),
                    Some(Say::Limited)
                );
            }
            ("Land", step) => {
                running = true;
                let _ = r.attempted(&outcome(step), now);
            }
            ("Miss" | "Cannot" | "Ended", step) => {
                let _ = r.attempted(&outcome(step), now);
            }
            ("PersonRuns", _) => {
                running = true;
                let _ = r.running(now);
            }
            ("Healthy", _) => {
                now += HEALTHY;
                r.forgive(now);
                base = 0;
            }
            ("Stutter", step) => {
                assert_eq!(r.attempted(&outcome(step), now), Say::Nothing);
            }
            other => unreachable!("{other:?}"),
        }
        assert!(
            action == "Stutter" || model.fire(action, &mut expect),
            "{action} disabled at {expect:?}"
        );
        let seen = project(&r, running, base);
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &seen),
                "{action}: {} broken by {seen:?}",
                inv.name
            );
        }
        assert_eq!(seen, expect, "{action} ({input}): observed vs model");
        if expect["restarted"] == 1 {
            for off in ["PersonReturns", "Ended", "Limited"] {
                assert!(!model.action_enabled(off, &expect), "{off} after {action}");
            }
        }
        if (expect["failing"] == 1 || expect["said"] == 1) && expect["restarted"] == 0 {
            let mut probe = r.clone();
            assert_eq!(probe.running(now), Say::Clear, "after {action}");
            let mut runs = expect.clone();
            assert!(model.fire("PersonReturns", &mut runs) && model.fire("PersonRuns", &mut runs));
            assert_eq!(
                project(&probe, true, base),
                runs,
                "another agent runs while it keeps failing"
            );
        }
        if expect["restarted"] == 1 && expect["pending"] == 1 {
            for word in ["ended:one-shot", "ended:graceful-exit", "wait:not-exited"] {
                let mut probe = r.clone();
                assert_eq!(probe.attempted(&outcome(word), now), Say::Cannot, "{word}");
                let mut cannot = expect.clone();
                assert!(model.fire("Cannot", &mut cannot));
                assert_eq!(
                    project(&probe, running, base),
                    cannot,
                    "{word}: a said Cannot"
                );
            }
        }
    }
    for a in &model.actions {
        assert!(
            schedule.iter().any(|(name, _)| *name == a.name),
            "{} is never driven",
            a.name
        );
    }
    let mut own = Relaunches::default();
    for _ in 0..3 {
        let _ = own.exited_as(OnExit::Relaunch, false, now);
        let _ = own.attempted(&outcome("wait:shell-prompt"), now);
        let _ = own.attempted(&outcome("adopted"), now);
    }
    assert_eq!(own.exited(OnExit::Relaunch, now), Ok(BACKOFF[0]));
    let mut asking = Relaunches::default();
    let _ = asking.exited(OnExit::Relaunch, now);
    assert_eq!(asking.attempted(&outcome("refused:home"), now), Say::Cannot);
    asking.ask(CONVERSATION);
    assert_eq!(
        asking.running(now),
        Say::Nothing,
        "another agent: it stands"
    );
    assert_eq!(
        asking.conversation_runs("0badf00d-9999-2222-3333-444455556666"),
        Say::Nothing
    );
    assert_eq!(asking.conversation_runs(CONVERSATION), Say::Clear);
    let buggy_host = aterm_spec::interp::with_buggy(&model, 1);
    let mut counted = buggy_host.init_state();
    assert!(buggy_host.fire("Asked", &mut counted) && buggy_host.fire("Land", &mut counted));
    assert!(!buggy_host.check_invariant("NoCrashNoStreak", &counted));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut after_person = buggy.init_state();
    assert!(buggy.fire("PersonExit", &mut after_person));
    assert!(!buggy.check_invariant("PersonWins", &after_person));
    let mut dropped = buggy.init_state();
    assert!(buggy.fire("Crash", &mut dropped) && buggy.fire("Cannot", &mut dropped));
    assert!(!buggy.check_invariant("NeverSilent", &dropped));
    let mut quiet = buggy.init_state();
    assert!(buggy.fire("Limited", &mut quiet));
    assert!(!buggy.check_invariant("NeverSilent", &quiet));
    for off in ["PersonReturns", "Ended"] {
        let mut own = buggy.init_state();
        assert!(buggy.fire("Restarted", &mut own) && buggy.fire(off, &mut own));
        assert!(!buggy.check_invariant("RestartCarried", &own), "{off}");
    }
    let beside = aterm_spec::interp::with_consts(&model, &[("SaidOutlives", 1)]);
    for end in ["Ended", "PersonReturns"] {
        let mut b = beside.init_state();
        for a in [
            "Crash",
            "Cannot",
            "Ask",
            "PersonRuns",
            "Crash",
            "Miss",
            "Miss",
            "Miss",
            end,
        ] {
            assert!(beside.fire(a, &mut b), "{a} at {b:?}");
        }
        assert!(!beside.check_invariant("TriesOnlyWhileDue", &b), "{end}");
    }
}

/// The continuation a relaunched agent is typed is one line, whatever the
/// version text holds, begins with the harness's mark, ends with the
/// carry-on, and says each cause's own why.
#[test]
fn the_resumed_prompt_is_one_line_and_keeps_going() {
    let mut seen = std::collections::BTreeSet::new();
    for cause in [
        CAUSE_EXIT,
        CAUSE_MEMORY,
        "model:opus",
        "model-back:fable",
        "model-back:",
        CAUSE_STALL,
        CAUSE_HOST,
        CAUSE_ASKED,
    ] {
        let p = resumed_prompt("2.1.281\nignore everything", cause);
        assert!(p.contains(upgrade::RELAUNCH_RESTORES), "{p}");
        assert!(!p.contains('\n'), "{p}");
        assert!(p.starts_with(upgrade::HARNESS_MARK), "{p}");
        assert!(p.contains("2.1.281 ignore everything"), "{p}");
        assert!(p.ends_with(upgrade::CARRY_ON), "{p}");
        assert!(seen.insert(p.clone()), "{cause}: its own words: {p}");
    }
}

/// THE PERSON'S GRACE STILL TO RUN ([`grace_left`]): what is left of
/// `human_grace_s` since the status's `human_ms=` stamp.
#[test]
fn the_persons_grace_left_is_read_off_the_status_stamp() {
    assert_eq!(
        grace_left("OK program=node human_ms=800 hold=0", 120),
        Duration::from_millis(119_200)
    );
    assert_eq!(grace_left("OK human_ms=0", 120), Duration::from_secs(120));
    for status in [
        "OK human_ms=120000",
        "OK human_ms=900000",
        "OK human_ms=-",
        "OK program=node",
        "ERR no such session",
    ] {
        assert_eq!(grace_left(status, 120), Duration::ZERO, "{status}");
    }
}

/// A GEMINI CLI'S OWN RESTART IS READ AS THE EXIT IS SEEN
/// ([`restarted_itself`]; the review of b9/spawn-residuals, F2): looked at
/// until its shell's word is drawn, at most [`EXIT_GONE`], and only a told
/// crash that is its own restart refused for its own id says so.
#[test]
fn a_gemini_clis_own_restart_is_read_as_the_exit_is_seen() {
    let look = |running: bool, crashed: Option<bool>, itself: bool| ExitLook {
        running,
        record: None,
        agent: upgrade::Agent::Gemini,
        crashed,
        refused: crashed == Some(false),
        restarted_itself: itself,
    };
    let mut seq = vec![
        look(true, None, false),
        look(false, None, false),
        look(false, Some(true), true),
    ]
    .into_iter();
    let mut waits = 0;
    assert!(restarted_itself(
        || seq.next(),
        |_| {
            waits += 1;
            true
        }
    ));
    assert_eq!(waits, 2);
    assert!(!restarted_itself(
        || Some(look(false, Some(true), false)),
        |_| true
    ));
    assert!(!restarted_itself(
        || Some(look(false, Some(false), false)),
        |_| true
    ));
    assert!(!restarted_itself(|| None, |_| true));
    let mut looks = 0;
    assert!(!restarted_itself(
        || {
            looks += 1;
            Some(look(true, None, false))
        },
        |_| true
    ));
    assert_eq!(
        looks,
        usize::try_from(EXIT_GONE.as_millis() / EXIT_LOOK.as_millis()).unwrap() + 1
    );
    assert!(!restarted_itself(
        || Some(look(false, None, true)),
        |_| true
    ));
    assert!(!restarted_itself(
        || Some(look(false, None, false)),
        |_| false
    ));
    let kept = exit_record(|| Some(look(false, Some(true), true)), |_| true);
    assert_eq!(kept, ExitRecord::Restarted);
    assert!(kept.crashed());
    assert_eq!(kept.word(), "restarted");
    assert_eq!(
        exit_record(|| Some(look(false, Some(true), false)), |_| true),
        ExitRecord::Crashed
    );
}

/// AN EXIT NO LOOK DECIDES IS READ BY NOTHING: a Codex's end is aterm's copy of codex-alab's
/// own record's to decide (the Codex screen lane's retirement, 2026-10-16).
#[test]
fn an_exit_of_an_agent_with_no_lane_is_read_by_nothing() {
    let look = |agent| ExitLook {
        running: false,
        record: None,
        agent,
        crashed: Some(true),
        refused: false,
        restarted_itself: false,
    };
    {
        let agent = upgrade::Agent::Codex;
        let mut looks = 0;
        let left = exit_record(
            || {
                looks += 1;
                Some(look(agent))
            },
            |_| true,
        );
        assert_eq!((left, looks), (ExitRecord::Unread, 1), "{agent:?}");
    }
    assert_eq!(
        exit_record(|| Some(look(upgrade::Agent::Claude)), |_| true),
        ExitRecord::Removed
    );
    for lane in [
        upgrade::Agent::Muse,
        upgrade::Agent::Agy,
        upgrade::Agent::Gemini,
    ] {
        let shell = |crashed: Option<bool>, refused: bool| ExitLook {
            crashed,
            refused,
            ..look(lane)
        };
        for (crashed, refused, want) in [
            (Some(true), false, ExitRecord::Crashed),
            (Some(false), true, ExitRecord::Refused),
            (Some(false), false, ExitRecord::Removed),
        ] {
            assert_eq!(
                exit_record(|| Some(shell(crashed, refused)), |_| true),
                want,
                "{lane:?}"
            );
        }
        let mut looks = 0;
        let left = exit_record(
            || {
                looks += 1;
                Some(shell(None, false))
            },
            |_| true,
        );
        assert_eq!(left, ExitRecord::Unread, "{lane:?}");
        assert!(looks > 1, "{lane:?}: waited for the shell's word: {looks}");
    }
    assert!(
        !ExitRecord::Refused.crashed(),
        "a refused start is no crash"
    );
}

/// The look at an exit, over scripted looks and no clock: a record that survives the settle is a
/// crash, kept as read; one removed a moment after the exit is seen is removed.
#[test]
fn the_look_at_an_exit_waits_out_its_own_removal_and_no_longer() {
    let sf = SessionFile {
        pid: 4242,
        session_id: "0badf00d-1111-2222-3333-444455556666".to_string(),
        cwd: "/".to_string(),
        version: "2.1.283".to_string(),
        status: "busy".to_string(),
        status_updated_at_ms: 1,
        proc_start: "Thu Sep 24 01:02:03 2026".to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let gone = |record: bool| {
        Some(ExitLook {
            running: false,
            record: record.then(|| sf.clone()),
            agent: upgrade::Agent::Claude,
            crashed: None,
            refused: false,
            restarted_itself: false,
        })
    };
    let steps =
        |d: Duration| usize::try_from(d.as_millis() / EXIT_LOOK.as_millis()).expect("small");
    let settle = steps(EXIT_SETTLE);
    assert!(settle >= 2, "the settle is more than one look");
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            gone(looks == 1)
        },
        |_| true,
    );
    assert_eq!((left, looks), (ExitRecord::Removed, 2));
    let mut waits = 0;
    let left = exit_record(
        || gone(true),
        |step| {
            assert_eq!(step, EXIT_LOOK);
            waits += 1;
            true
        },
    );
    assert_eq!(left, ExitRecord::Survived(Box::new(sf.clone())));
    assert_eq!(waits, settle);
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            gone(looks <= settle)
        },
        |_| true,
    );
    assert_eq!((left, looks), (ExitRecord::Removed, settle + 1));
    let mut waits = 0;
    let left = exit_record(
        || gone(false),
        |_| {
            waits += 1;
            true
        },
    );
    assert_eq!((left, waits), (ExitRecord::Removed, 0));
    let mut waits = 0;
    let left = exit_record(
        || {
            Some(ExitLook {
                running: true,
                record: Some(sf.clone()),
                agent: upgrade::Agent::Claude,
                crashed: None,
                refused: false,
                restarted_itself: false,
            })
        },
        |_| {
            waits += 1;
            true
        },
    );
    assert_eq!((left, waits), (ExitRecord::Unread, steps(EXIT_GONE)));
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            Some(ExitLook {
                running: looks <= 3,
                record: Some(sf.clone()),
                agent: upgrade::Agent::Claude,
                crashed: None,
                refused: false,
                restarted_itself: false,
            })
        },
        |_| true,
    );
    assert_eq!(
        (left, looks),
        (ExitRecord::Survived(Box::new(sf.clone())), settle + 1)
    );
    assert_eq!(exit_record(|| gone(true), |_| false), ExitRecord::Unread);
    assert_eq!(exit_record(|| None, |_| true), ExitRecord::Unread);
    assert_eq!(
        [
            ExitRecord::Unread.word(),
            ExitRecord::Survived(Box::new(sf)).word(),
            ExitRecord::Removed.word()
        ],
        ["unread", "survived", "removed"]
    );
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The newest `permission-mode` row decides — the structure Claude 2.1.283
/// writes, a mode changed mid-turn included — and an unknown mode is not
/// carried.
#[test]
fn the_conversations_last_permission_mode_is_read_from_its_transcript() {
    let tail = concat!(
        r#"{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"x"}"#,
        "\n",
        r#"{"type":"user","permissionMode":"bypassPermissions","timestamp":"2026-09-24T21:47:16.939Z"}"#,
        "\n",
        r#"{"type":"mode","mode":"normal","sessionId":"x"}"#,
        "\n",
        r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"x"}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"\"permission-mode\""}]}}"#,
        "\n",
    );
    assert_eq!(permission_mode_of(tail).as_deref(), Some("auto"));
    assert_eq!(permission_mode_of(""), None);
    assert_eq!(
        permission_mode_of(r#"{"type":"permission-mode","permissionMode":"yolo; rm"}"#),
        None,
        "a mode this module does not know is not carried"
    );
}

/// A relaunch never moves a session into bypass, and resumes it in the mode
/// its person left it in, always named (the default included), so neither a
/// `defaultMode` setting nor a build's auto-mode default picks another: a
/// session launched with --dangerously-skip-permissions and left in auto is
/// relaunched in auto, with bypass still in the cycle.
#[test]
fn a_relaunch_resumes_the_mode_the_conversation_ran_in() {
    let carried = args(&[
        "--dangerously-skip-permissions",
        "--model",
        "opus",
        "--resume",
        "ID",
    ]);
    assert_eq!(
        with_permission_mode(carried.clone(), Some("auto")),
        args(&[
            "--model",
            "opus",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "auto",
            "--resume",
            "ID"
        ])
    );
    assert_eq!(with_permission_mode(carried.clone(), None), carried);
    assert_eq!(
        with_permission_mode(carried.clone(), Some("bypassPermissions")),
        carried,
        "still in bypass: as launched"
    );
    for default in ["default", "manual"] {
        assert_eq!(
            with_permission_mode(carried.clone(), Some(default)),
            args(&[
                "--model",
                "opus",
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "default",
                "--resume",
                "ID"
            ]),
            "the default is named too ({default})"
        );
    }
    assert_eq!(
        with_permission_mode(
            args(&[
                "--permission-mode=plan",
                "--add-dir",
                "/w",
                "--resume",
                "ID"
            ]),
            Some("acceptEdits")
        ),
        args(&[
            "--add-dir",
            "/w",
            "--permission-mode",
            "acceptEdits",
            "--resume",
            "ID"
        ])
    );
    assert_eq!(
        with_permission_mode(args(&["--resume", "ID"]), Some("bypassPermissions")),
        args(&["--resume", "ID"]),
        "never INTO bypass"
    );
    assert_eq!(
        with_permission_mode(
            args(&["--permission-mode", "bypassPermissions", "--resume", "ID"]),
            Some("plan")
        ),
        args(&[
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan",
            "--resume",
            "ID"
        ])
    );
}

/// The whole line, from the raw launch argv: the mode rides the flags
/// `rewrite_argv` parsed, so neither a launch prompt behind a many-valued flag
/// nor a `--` can swallow or drop it.
#[test]
fn the_relaunch_line_carries_the_mode() {
    let exe = Path::new("/opt/claude");
    let id = "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b";
    let line = |argv: &[&str]| {
        line_for(
            Dialect::Bash,
            None,
            None,
            exe,
            &args(argv),
            Some(id),
            None,
            Some("auto"),
            &[],
        )
        .unwrap_or_else(|_| panic!("a line for {argv:?}"))
    };
    let want = format!(
        " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--add-dir' '../lib' \
         '--allow-dangerously-skip-permissions' '--permission-mode' 'auto' '--resume' '{id}'"
    );
    assert_eq!(
        line(&[
            "claude",
            "--add-dir",
            "../lib",
            "--dangerously-skip-permissions",
            "fix the parser"
        ]),
        want
    );
    assert_eq!(
        line(&[
            "claude",
            "--add-dir",
            "../lib",
            "--dangerously-skip-permissions",
            "--",
            "fix it"
        ]),
        want
    );
}

/// The live pill names the mode flag Claude takes; manual is `default`.
#[test]
fn a_pill_names_its_mode_flag() {
    use super::super::lights::Mode;
    assert_eq!(mode_flag(Mode::Manual), "default");
    assert_eq!(mode_flag(Mode::Auto), "auto");
    assert_eq!(mode_flag(Mode::Bypass), "bypassPermissions");
    assert_eq!(mode_flag(Mode::AcceptEdits), "acceptEdits");
    assert_eq!(mode_flag(Mode::DontAsk), "dontAsk");
    assert_eq!(mode_flag(Mode::Plan), "plan");
}

/// A Muse Code look is decided on its shell's word, by its own table: a crash
/// is [`ExitRecord::Crashed`], its own exit or someone's signal
/// [`ExitRecord::Removed`], at once; with no word yet it is looked at again up
/// to [`EXIT_GONE`] (the shell writes it as it takes the terminal back), then
/// left [`ExitRecord::Unread`], never read as a removal.
#[test]
fn a_muse_exit_is_decided_on_its_shells_word() {
    let look = |crashed: Option<bool>| {
        Some(ExitLook {
            running: false,
            record: None,
            agent: upgrade::Agent::Muse,
            crashed,
            refused: false,
            restarted_itself: false,
        })
    };
    assert_eq!(
        exit_record(|| look(Some(true)), |_| true),
        ExitRecord::Crashed
    );
    assert_eq!(
        exit_record(|| look(Some(false)), |_| true),
        ExitRecord::Removed
    );
    let steps =
        |d: Duration| usize::try_from(d.as_millis() / EXIT_LOOK.as_millis()).expect("small");
    let mut waits = 0;
    let left = exit_record(
        || look(None),
        |_| {
            waits += 1;
            true
        },
    );
    assert_eq!((left, waits), (ExitRecord::Unread, steps(EXIT_GONE)));
    let mut n = 0;
    let left = exit_record(
        || {
            n += 1;
            look((n >= 3).then_some(true))
        },
        |_| true,
    );
    assert_eq!((left, n), (ExitRecord::Crashed, 3));
    assert_eq!(ExitRecord::Crashed.word(), "crashed");
    let claude = || {
        Some(ExitLook {
            running: false,
            record: None,
            agent: upgrade::Agent::Claude,
            crashed: None,
            refused: false,
            restarted_itself: false,
        })
    };
    assert_eq!(exit_record(claude, |_| true), ExitRecord::Removed);
}

/// A login directory with a space and an apostrophe in it: the two
/// characters a shell line is most easily broken by.
const LOGIN_DIR: &str = "/tmp/aterm state/logins/owner's work-2";

/// The conversation the line tests resume.
const LOGIN_ID: &str = "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b";

/// The relaunch line for `argv` under `dialect`, `required` on it.
fn login_line(
    dialect: Dialect,
    cd: Option<&str>,
    argv: &[&str],
    required: &[(&str, &str)],
) -> Result<String, String> {
    line_for(
        dialect,
        None,
        cd,
        Path::new("/opt/claude"),
        &args(argv),
        Some(LOGIN_ID),
        None,
        None,
        required,
    )
    .map_err(|no| match no {
        NoPlan::Refused { what, .. } => what,
        NoPlan::Wait(why) => why.to_string(),
    })
}

/// The login is on the line, first, quoted as the shell needs it: in front of the survey switch
/// and the program, inside the subshell's `cd`, behind fish's `env`.
#[test]
fn the_relaunch_line_carries_the_login_in_every_shells_form() {
    let login = login_env(Some(LOGIN_DIR));
    let tail = format!("'/opt/claude' '--verbose' '--resume' '{LOGIN_ID}'");
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    let argv = ["claude", "--verbose"];
    let posix = r"CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner'\''s work-2'";
    for dialect in [Dialect::Zsh, Dialect::Bash] {
        assert_eq!(
            login_line(dialect, None, &argv, &login).as_deref(),
            Ok(format!(" {posix} {survey} {tail}").as_str()),
            "{dialect:?}"
        );
        assert_eq!(
            login_line(dialect, Some("/w"), &argv, &login).as_deref(),
            Ok(format!(" ( cd -- '/w' && {posix} {survey} exec {tail} )").as_str()),
            "{dialect:?}"
        );
    }
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &login).as_deref(),
        Ok(format!(
            r" env CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner\'s work-2' {survey} {tail}"
        )
        .as_str())
    );
    assert_eq!(login_env(Some("")), [(accounts::LOGIN_ENV, "")]);
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(Some(""))).as_deref(),
        Ok(format!(" CLAUDE_SECURESTORAGE_CONFIG_DIR='' {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &login_env(Some(""))).as_deref(),
        Ok(format!(" env CLAUDE_SECURESTORAGE_CONFIG_DIR='' {survey} {tail}").as_str())
    );
    assert!(login_env(None).is_empty());
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(None)).as_deref(),
        Ok(format!(" {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(Some("/tmp/a\nb"))),
        Err("line".to_string())
    );
}

/// The Claude home is on the line too ([`required_env`]): a process started with
/// `CLAUDE_CONFIG_DIR` is relaunched with exactly that value, ahead of the login and the survey
/// switch, in every shell's form.
#[cfg(unix)]
#[test]
fn the_relaunch_line_carries_the_claude_home_its_agent_ran_under() {
    const HOME: &str = "/tmp/lab home/.claude";
    let tail = format!("'/opt/claude' '--verbose' '--resume' '{LOGIN_ID}'");
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    let argv = ["claude", "--verbose"];
    let home = r"CLAUDE_CONFIG_DIR='/tmp/lab home/.claude'";
    assert_eq!(
        required_env(Some(HOME), None, None),
        [(accounts::HOME_ENV, HOME)],
        "the home alone"
    );
    for dialect in [Dialect::Zsh, Dialect::Bash] {
        assert_eq!(
            login_line(dialect, None, &argv, &required_env(Some(HOME), None, None)).as_deref(),
            Ok(format!(" {home} {survey} {tail}").as_str()),
            "{dialect:?}"
        );
    }
    assert_eq!(
        login_line(
            Dialect::Fish,
            None,
            &argv,
            &required_env(Some(HOME), None, None)
        )
        .as_deref(),
        Ok(format!(" env {home} {survey} {tail}").as_str())
    );
    let posix = r"CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner'\''s work-2'";
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(Some(HOME), None, Some(LOGIN_DIR))
        )
        .as_deref(),
        Ok(format!(" {home} {posix} {survey} {tail}").as_str())
    );
    assert_eq!(
        required_env(None, None, Some(LOGIN_DIR)),
        login_env(Some(LOGIN_DIR))
    );
    assert!(required_env(None, None, None).is_empty());
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &required_env(None, None, None)).as_deref(),
        Ok(format!(" {survey} {tail}").as_str())
    );
    assert_eq!(
        required_env(None, Some("/other"), Some(LOGIN_DIR)),
        [("HOME", "/other")]
            .into_iter()
            .chain(login_env(Some(LOGIN_DIR)))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(None, Some("/other"), None)
        )
        .as_deref(),
        Ok(format!(" HOME=/other {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(None, Some("/tmp/lab home"), None)
        )
        .as_deref(),
        Ok(format!(" HOME='/tmp/lab home' {survey} {tail}").as_str())
    );
    let snap = |home: Option<String>| Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--verbose"]),
        session: Some(LOGIN_ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.295".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home,
        claude_dir: None,
    };
    assert!(resumes_on_exit(Path::new("/Users//someone"), &snap(None)));
    assert!(resumes_on_exit(
        Path::new("/Users//someone"),
        &snap(Some(HOME.to_string()))
    ));
    assert!(!resumes_on_exit(
        Path::new("/Users//someone"),
        &snap(Some(format!("/tmp/{}", "d".repeat(5000))))
    ));
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(Some("/tmp/a\nb"), None, None)
        ),
        Err("line".to_string())
    );
}

/// Whether an exit would be relaunched is asked of the line the relaunch would really type, with
/// the login the process runs under: a snapshot whose login no line can carry promises no
/// relaunch.
#[test]
fn an_exit_is_promised_a_relaunch_only_where_the_line_can_hold_its_login() {
    let snap = |login: Option<String>| Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--verbose"]),
        session: Some(LOGIN_ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.286".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login,
        home: None,
        claude_dir: None,
    };
    assert!(resumes_on_exit(Path::new("/Users//someone"), &snap(None)));
    assert!(resumes_on_exit(
        Path::new("/Users//someone"),
        &snap(Some(String::new()))
    ));
    assert!(resumes_on_exit(
        Path::new("/Users//someone"),
        &snap(Some(LOGIN_DIR.to_string()))
    ));
    assert!(!resumes_on_exit(
        Path::new("/Users//someone"),
        &snap(Some(format!("/tmp/{}", "d".repeat(5000))))
    ));
}

/// The roster names the login, and the process's own environment says which account it runs under:
/// a label's login is its row's directory verbatim (the empty value for the row with none), and a
/// label the roster does not list has none.
#[cfg(unix)]
#[test]
fn a_label_is_a_login_only_through_the_roster() {
    let roster = accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n"
    ))
    .expect("a roster");
    assert_eq!(account_login(&roster, "main").as_deref(), Some(""));
    assert_eq!(account_login(&roster, "work-2").as_deref(), Some(LOGIN_DIR));
    assert_eq!(account_login(&roster, "pool-a"), None);
    assert_eq!(account_login(&accounts::Roster::default(), "main"), None);
    let on = |login: Option<&str>| Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: String::new(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude"]),
        session: None,
        cwd: String::new(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: login.map(str::to_string),
        home: None,
        claude_dir: None,
    };
    assert!(runs_under(&on(None), &roster, "main"));
    assert!(runs_under(&on(Some("")), &roster, "main"));
    assert!(runs_under(&on(Some(LOGIN_DIR)), &roster, "work-2"));
    assert!(!runs_under(&on(Some(LOGIN_DIR)), &roster, "main"));
    assert!(!runs_under(&on(None), &roster, "work-2"));
    assert!(!runs_under(
        &on(Some(&format!("{LOGIN_DIR}/"))),
        &roster,
        "work-2"
    ));
    assert!(!runs_under(&on(None), &accounts::Roster::default(), "main"));
}

/// An account move's record: its cause is `account:<label>`, in both allowlists, so the harness's
/// own SIGTERM is never read as a graceful exit and the record is a relaunch's, carried on with
/// nothing typed ([`carried_quietly`], quiet or not).
#[test]
fn an_account_moves_record_is_a_relaunch_that_ended_its_own_agent() {
    let to = |label: &str| Restart::Account {
        to: label.to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Wall,
    };
    let plain = model_restart(
        &to("work-2"),
        None,
        "claude-fable-5-1".to_string(),
        None,
        None,
    );
    assert_eq!(
        plain,
        ModelRestart {
            model: None,
            cause: "account:work-2".to_string(),
            launch_model: "claude-fable-5-1".to_string(),
            fallback_from: None,
        }
    );
    assert!(relaunch_cause(&plain.cause) && ends_its_agent(&plain.cause));
    assert!(carried_quietly(&plain.cause, false) && carried_quietly(&plain.cause, true));
    assert!(!starts_afresh(&plain.cause));
    assert!(ends_at_once(&plain.cause));
    let hand = model_restart(
        &to("work-2"),
        None,
        "claude-fable-5-1".to_string(),
        None,
        Some("claude-opus-5-5".to_string()),
    );
    assert_eq!(hand.model.as_deref(), Some("claude-opus-5-5"));
    let riding = model_restart(
        &to("work-2"),
        None,
        "claude-fable-5-1".to_string(),
        Some("claude-fable-5-2".to_string()),
        Some("claude-opus-5-5".to_string()),
    );
    assert_eq!(riding.model.as_deref(), Some("claude-fable-5-2"));
    let fell = St {
        cause: "model:claude-sonnet-5-5".to_string(),
        fallback_from: Some("claude-fable-5-1".to_string()),
        ..St::default()
    };
    let back = model_restart(
        &to("work-2"),
        Some(&fell),
        "claude-sonnet-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(
        (back.model.as_deref(), back.fallback_from),
        (Some("claude-fable-5-1"), None)
    );
    let memory = model_restart(
        &Restart::Memory,
        Some(&fell),
        "claude-sonnet-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(
        (memory.model, memory.fallback_from.as_deref()),
        (None, Some("claude-fable-5-1"))
    );
    for cause in ["account", "accounts", "", CAUSE_FRESH] {
        assert!(!relaunch_cause(cause), "{cause:?}");
        assert!(!carried_quietly(cause, false), "{cause:?}");
    }
    assert!(
        !carried_quietly(CAUSE_MEMORY, true),
        "another relaunch says why it came back"
    );
    assert!(!ends_its_agent("account"));
    assert!(
        !ends_its_agent(CAUSE_EXIT),
        "an exit's relaunch ended nothing"
    );
}

/// What a shell would expand in a login directory is typed literally, and a line that cannot be
/// written with the login is never written without it: the vendor hashes the directory's bytes into
/// its keychain item's name, so every value rides inside single quotes, under each shell.
#[test]
fn a_login_directory_is_typed_literally_or_the_line_is_refused() {
    let argv = ["claude"];
    let tail = format!("'/opt/claude' '--resume' '{LOGIN_ID}'");
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    let name = accounts::LOGIN_ENV;
    for dir in [
        "/tmp/$HOME/work-2",
        "/tmp/`id`/work-2",
        "/tmp/a;b/work-2",
        "/tmp/a&b|c/work-2",
        "/tmp/a\"b/work-2",
        "/tmp/*/work-2",
        "/tmp/~/work-2",
        "/tmp/a!b/work-2",
        "/tmp/a(b)/work-2",
        "/tmp/a#b/work-2",
    ] {
        let login = login_env(Some(dir));
        for dialect in [Dialect::Zsh, Dialect::Bash] {
            assert_eq!(
                login_line(dialect, None, &argv, &login).as_deref(),
                Ok(format!(" {name}='{dir}' {survey} {tail}").as_str()),
                "{dialect:?}: {dir}"
            );
        }
        assert_eq!(
            login_line(Dialect::Fish, None, &argv, &login).as_deref(),
            Ok(format!(" env {name}='{dir}' {survey} {tail}").as_str()),
            "fish: {dir}"
        );
    }
    let login = login_env(Some(r"/tmp/a\b"));
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login).as_deref(),
        Ok(format!(r" {name}='/tmp/a\b' {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &login).as_deref(),
        Ok(format!(r" env {name}='/tmp/a\\b' {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &login_env(Some("/tmp/logins/work-2"))
        )
        .as_deref(),
        Ok(format!(" {name}=/tmp/logins/work-2 {survey} {tail}").as_str())
    );
    let login = login_env(Some(LOGIN_DIR));
    assert_eq!(
        login_line(Dialect::Fish, Some("/w"), &argv, &login),
        Err("line".to_string())
    );
    assert_eq!(
        login_line(Dialect::Fish, Some("/w"), &argv, &[]),
        Err("line".to_string())
    );
    assert!(login_line(Dialect::Fish, None, &argv, &login).is_ok());
    for bad in ["/tmp/a\tb", "/tmp/a\rb", "/tmp/a\u{1b}[2Jb", "/tmp/a\0b"] {
        for dialect in [Dialect::Zsh, Dialect::Bash, Dialect::Fish] {
            assert_eq!(
                login_line(dialect, None, &argv, &login_env(Some(bad))),
                Err("line".to_string()),
                "{dialect:?}: {bad:?}"
            );
        }
    }
}

/// An account move asked over a restart of another kind in flight was not tried ([`carried_word`],
/// [`IN_FLIGHT_OTHER`]): every word of that carry but a failure (`adopted` included) answers
/// `wait:restart-in-flight`, as does a move to another label than the one in flight.
#[test]
fn an_account_move_over_another_restart_in_flight_is_not_tried() {
    let to = |label: &str| Restart::Account {
        to: label.to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Wall,
    };
    let word = |why: &Restart, cause: &str, step: &str| carried_word(why, cause, step.to_string());
    for cause in [
        "",
        CAUSE_MEMORY,
        "model:claude-sonnet-5-5",
        "account:pool-a",
    ] {
        for step in [
            "adopted",
            "relaunched",
            "continued",
            "wait:resume",
            "wait:held",
        ] {
            assert_eq!(
                word(&to("work-2"), cause, step),
                IN_FLIGHT_OTHER,
                "{cause:?} {step}"
            );
        }
        assert_eq!(
            word(&to("work-2"), cause, "failed:relaunch:ERR"),
            "failed:relaunch:ERR",
            "{cause:?}"
        );
    }
    for step in ["adopted", "wait:resume", "failed:no-resume"] {
        assert_eq!(word(&to("work-2"), "account:work-2", step), step);
        for cause in ["", CAUSE_MEMORY, "account:work-2"] {
            assert_eq!(word(&Restart::Memory, cause, step), step, "{cause:?}");
        }
    }
    assert_eq!(
        word(&to("work"), "account:work-2", "adopted"),
        IN_FLIGHT_OTHER
    );
}

/// A relaunch with no process to read carries the machine's active account ([`cold_login`]): where
/// two or more accounts are enrolled and `active` is one of them and proven, the login its roster
/// row names (the empty value, the default login named, for the row with no directory).
#[cfg(unix)]
#[test]
fn a_cold_relaunch_carries_the_proven_active_account_and_nothing_else() {
    use std::collections::BTreeSet;
    let roster = accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n"
    ))
    .expect("a roster");
    let both: BTreeSet<String> = ["main", "work-2"].map(String::from).into();
    let on = |active: Option<&str>, proven: bool| accounts::State {
        active: active.map(str::to_string),
        proven,
        ..accounts::State::default()
    };
    assert_eq!(
        cold_login(&roster, &on(Some("work-2"), true), &both, true).as_deref(),
        Some(LOGIN_DIR),
        "the active account's directory, verbatim"
    );
    assert_eq!(
        cold_login(&roster, &on(Some("main"), true), &both, true).as_deref(),
        Some(""),
        "the default login, named"
    );
    let only_dirs: BTreeSet<String> = ["work-2".to_string()].into();
    assert_eq!(
        cold_login(&roster, &on(Some("main"), true), &only_dirs, true).as_deref(),
        Some("")
    );

    let none = BTreeSet::new();
    let proven = on(Some("work-2"), true);
    assert_eq!(
        cold_login(&accounts::Roster::default(), &proven, &both, true),
        None,
        "no roster"
    );
    let one = accounts::Roster::parse("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n")
        .expect("a roster of one");
    assert_eq!(
        cold_login(&one, &on(Some("main"), true), &both, true),
        None,
        "one account listed"
    );
    assert_eq!(
        cold_login(&roster, &on(Some("main"), true), &none, true),
        None,
        "two listed, one enrolled"
    );
    assert_eq!(cold_login(&roster, &on(None, true), &both, true), None);
    assert_eq!(
        cold_login(&roster, &on(Some("work-2"), false), &both, true),
        None,
        "an active account nobody has been served by"
    );
    assert_eq!(
        cold_login(&roster, &on(Some("pool-a"), true), &both, true),
        None,
        "no row of the roster"
    );
    let three = accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n\n\
         [[account]]\nlabel = \"pool-a\"\nclass = \"shared\"\ndir = \"/tmp/logins/pool-a\"\n"
    ))
    .expect("a roster of three");
    assert_eq!(
        cold_login(&three, &on(Some("pool-a"), true), &both, true),
        None,
        "listed, and not enrolled here"
    );
    let mut out = on(Some("work-2"), true);
    accounts::note_auth(&mut out, "work-2", accounts::Auth::SignedOut, 1);
    out.active = Some("work-2".to_string());
    out.proven = true;
    assert_eq!(
        cold_login(&roster, &out, &both, true),
        None,
        "known signed out"
    );
    assert_eq!(
        cold_login(&roster, &proven, &both, false),
        None,
        "a Claude home that is not the default"
    );
}

/// A RELAUNCH LINE TYPED IS ON ITS RECORD BEFORE ANYTHING AFTER IT RUNS
/// (host-5): a step that panics after the line is typed —
/// in the ledger's line, or anything later — leaves the record `Relaunched`
/// with the line's block and no adopted process, so the next step awaits
/// that line's agent and never types it again.
#[cfg(unix)]
#[test]
fn a_typed_relaunch_line_is_recorded_before_anything_after_it() {
    const ID: &str = "0badf00d-1111-2222-3333-77778888aaaa";
    let dir = crate::supervise::ScratchDir::new("relaunch", "typed-recorded");
    let o = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        usage_tap: None,
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let mut st = St {
        phase: upgrade::Phase::Exiting {
            at_s: 1_791_477_331,
        },
        cause: CAUSE_HOST.to_string(),
        tab: "s-restored".to_string(),
        line: "claude --resume 0badf00d".to_string(),
        relaunched_pid: 4242,
        ..St::default()
    };
    save(&o, ID, &st);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        typed_recorded(&o, &mut st, ID, Some(7), |seen| {
            let saved = load(&o, ID).expect("the record");
            assert_eq!(saved.phase, seen.phase, "saved before what runs after it");
            panic!("the ledger's line panics (a test's)");
        });
    }));
    assert!(panicked.is_err());
    let saved = load(&o, ID).expect("the record");
    assert!(
        matches!(saved.phase, upgrade::Phase::Relaunched { .. }),
        "{:?}",
        saved.phase
    );
    assert_eq!(saved.line_block, Some(7));
    assert_eq!(
        saved.relaunched_pid, 0,
        "no process of the new line adopted"
    );
    assert!(
        in_flight_here(&o, "s-restored", ID),
        "the next step reads it in flight: awaited, never typed"
    );
}

/// WHETHER THE AGENT A RELAUNCH ADOPTED IS GONE, by the conversation's
/// record (the residual review's R5): in flight, with the adopted process
/// on it, `Some(true)` for one that is no process now (a reaped child),
/// `Some(false)` for one that lives (this test's own).
#[cfg(unix)]
#[test]
fn whether_the_adopted_agent_is_gone_is_the_records_to_say() {
    const ID: &str = "0badf00d-1111-2222-3333-777788889999";
    let dir = crate::supervise::ScratchDir::new("relaunch", "adopted-gone");
    let o = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: Some("s-restored".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        usage_tap: None,
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let reaped = {
        let mut child = aterm_uds::spawnfd::command("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let ours = std::process::id();
    let gone_in = |tab: &str, phase: upgrade::Phase, pid: u32| {
        save(
            &o,
            ID,
            &St {
                phase,
                cause: CAUSE_HOST.to_string(),
                tab: tab.to_string(),
                relaunched_pid: pid,
                ..St::default()
            },
        );
        adopted_gone(&o, "s-restored", ID)
    };
    let gone = |phase: upgrade::Phase, pid: u32| gone_in("s-restored", phase, pid);
    let here_in = |tab: &str, phase: upgrade::Phase| {
        let _ = gone_in(tab, phase, 0);
        in_flight_here(&o, "s-restored", ID)
    };
    assert!(here_in(
        "s-restored",
        upgrade::Phase::Exiting {
            at_s: 1_791_477_331
        }
    ));
    assert!(!here_in(
        "s-before-aterm-ended",
        upgrade::Phase::Exiting {
            at_s: 1_791_477_331
        }
    ));
    assert!(!here_in("s-restored", upgrade::Phase::Done));
    let flight = || upgrade::Phase::Relaunched {
        at_s: 1_791_477_331,
    };
    assert_eq!(
        gone(flight(), reaped),
        Some(true),
        "the adopted agent is gone"
    );
    assert_eq!(gone(flight(), ours), Some(false), "the adopted agent lives");
    assert_eq!(gone(flight(), 0), None, "none adopted");
    assert_eq!(gone(upgrade::Phase::Done, reaped), None, "not in flight");
    assert_eq!(
        gone_in("s-before-aterm-ended", flight(), ours),
        None,
        "another tab's record"
    );
    assert_eq!(
        adopted_gone(&o, "s-restored", "0badf00d-0000-0000-0000-000000000000"),
        None
    );
}

/// A RELAUNCH-ONLY LANE'S WORKER CARRIES ITS OWN AGENT'S RECORD ALONE
/// ([`owed_of`], [`resume_as`]'s `only`; the review of the Muse Code lane's
/// rehome, 2026-10-17; agy's lane, the same): a codex-alab's relaunch left
/// owed in the tab is the sweep's (`lanes::sweep_carries_relaunch`), so
/// neither a Muse Code worker's host nor an agy worker's parks for it or
/// carries it — `current`, the record still owed as it lies — and one
/// relaunch-only lane's record is never the other's worker's.
#[test]
fn a_relaunch_only_lanes_worker_is_owed_its_own_agents_record_alone() {
    const CODEX: &str = "019a0c3e-7d1f-7b52-9e41-5a6b7c8d9e0f";
    const MUSE: &str = "01a148f8-9301-7752-96db-fef456f88074";
    const AGY: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
    let dir = crate::supervise::ScratchDir::new("relaunch", "owed-of");
    let o = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: Some("s-owed".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        usage_tap: None,
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let codex = St {
        phase: upgrade::Phase::Relaunched { at_s: now_s() },
        tab: "s-owed".to_string(),
        agent: upgrade::Agent::Codex,
        ..St::default()
    };
    save(&o, CODEX, &codex);
    for lane in [upgrade::Agent::Muse, upgrade::Agent::Agy] {
        assert!(
            !owed_of(&o, Some(lane)),
            "the sweep's: not the {lane:?} worker's"
        );
        assert_eq!(
            resume_as(&o, false, Some(lane)).step,
            "current",
            "nothing carried for the {lane:?} worker"
        );
        assert_eq!(
            load(&o, CODEX).map(|st| st.phase),
            Some(codex.phase.clone()),
            "its record as it lies"
        );
    }
    assert!(owed_of(&o, None), "any agent's: owed");
    assert!(owed(&o), "any agent's: owed");
    assert!(owed_of(&o, Some(upgrade::Agent::Codex)), "its own agent's");
    let muse = St {
        agent: upgrade::Agent::Muse,
        ..codex.clone()
    };
    save(&o, MUSE, &muse);
    assert!(owed_of(&o, Some(upgrade::Agent::Muse)), "its own agent's");
    assert!(
        !owed_of(&o, Some(upgrade::Agent::Agy)),
        "a Muse Code's: not the agy worker's"
    );
    assert_eq!(
        resume_as(&o, false, Some(upgrade::Agent::Agy)).step,
        "current",
        "nothing carried for the agy worker"
    );
    assert_eq!(
        load(&o, MUSE).map(|st| st.phase),
        Some(muse.phase.clone()),
        "the Muse Code's record as it lies"
    );
    let agy = St {
        agent: upgrade::Agent::Agy,
        ..codex.clone()
    };
    save(&o, AGY, &agy);
    assert!(owed_of(&o, Some(upgrade::Agent::Agy)), "its own agent's");
    let agy_tab = Opts {
        only_sid: Some("s-agy".to_string()),
        ..o.clone()
    };
    save(
        &agy_tab,
        "5c1a7e2b-3d4f-4a6b-8c9d-0e1f2a3b4c5d",
        &St {
            tab: "s-agy".to_string(),
            ..agy.clone()
        },
    );
    assert!(owed_of(&agy_tab, Some(upgrade::Agent::Agy)), "its own");
    assert!(
        !owed_of(&agy_tab, Some(upgrade::Agent::Muse)),
        "an agy's: not the Muse worker's"
    );
    let other = Opts {
        only_sid: Some("s-other".to_string()),
        ..o.clone()
    };
    assert!(!owed_of(&other, None), "another tab owes nothing");
}

/// A LIMIT ROW THE HARNESS MOVED THE CONVERSATION OFF IS NO STANDING LIMIT
/// (LIM-1; owner decision 3, the accounts design §6): the supervisor's loop
/// reads the row the agent's transcript stands at as the wall where the
/// screen draws none, and a move's carry-on must not be held behind the
/// bucket or the account it left — on a cold restore too, whose new tab
/// remembers nothing but the conversation's record.
#[test]
fn a_limit_row_a_move_left_is_no_standing_limit() {
    const ID: &str = "0badf00d-1111-2222-3333-444455556666";
    const WRITTEN: u64 = 1_791_477_331;
    let tail = include_str!("../supervise/fixtures/claude-2.1.294-transcript-weekly-limit.jsonl");
    let sonnet = tail
        .replace(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            "You've hit your Sonnet limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
        )
        .replace(
            "\"rateLimitType\":\"seven_day\"",
            "\"rateLimitType\":\"seven_day_sonnet\"",
        );
    let dir = crate::supervise::ScratchDir::new("relaunch", "standing-limit");
    let o = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: Some("s-restored".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let snap = Snapshot {
        tab: "s-restored".to_string(),
        pid: 4242,
        start: "Thu Oct 8 18:04:23 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--model", "opus"]),
        session: Some(ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.294".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(upgrade::Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    };
    let standing = |cause: Option<&str>, relaunched_at: u64, tail: &str| {
        let _ = std::fs::remove_file(state_dir(&o).join(format!("{ID}.json")));
        if let Some(cause) = cause {
            let st = St {
                cause: cause.to_string(),
                tab: "s-crashed".to_string(),
                relaunched_at,
                ..St::default()
            };
            save(&o, ID, &st);
        }
        standing_limit(&o, &snap, tail)
    };
    let after = WRITTEN + 600;
    let weekly = standing(None, 0, tail).expect("the weekly row stands");
    assert_eq!(
        (weekly.until, weekly.window.as_deref(), weekly.at),
        (Some(1_791_736_527), Some("seven_day"), Some(WRITTEN))
    );
    assert!(weekly.text.starts_with("You've hit your weekly limit"));
    assert_eq!(
        weekly.row.as_deref(),
        Some("a65a7337-1491-48c9-983b-5dd498b525a0")
    );
    for (cause, tail) in [
        ("account:work-2", tail),
        ("model:opus", sonnet.as_str()),
        ("model-back:claude-fable-5-1", tail),
    ] {
        assert_eq!(standing(Some(cause), after, tail), None, "{cause}");
        assert_eq!(
            standing(Some(cause), 0, tail),
            None,
            "{cause}: an older record with no instant"
        );
    }
    for cause in [CAUSE_EXIT, CAUSE_HOST, CAUSE_STALL, CAUSE_MEMORY, ""] {
        assert!(standing(Some(cause), after, tail).is_some(), "{cause:?}");
    }
    for cause in ["account:work-2", "model:opus"] {
        assert!(
            standing(Some(cause), WRITTEN - 600, tail).is_some(),
            "{cause}: the row was written after the move"
        );
    }
    let kept = |moved_at: u64| {
        let _ = std::fs::remove_file(state_dir(&o).join(format!("{ID}.json")));
        save(
            &o,
            ID,
            &St {
                cause: CAUSE_HOST.to_string(),
                tab: "s-restored".to_string(),
                relaunched_at: after + 60,
                moved_at,
                ..St::default()
            },
        );
        standing_limit(&o, &snap, tail)
    };
    assert_eq!(kept(after), None, "the move a restore's record keeps");
    assert!(kept(WRITTEN - 600).is_some(), "a row after the kept move");
    assert!(kept(0).is_some(), "no move kept");
    let unnamed = Snapshot {
        session: None,
        ..snap.clone()
    };
    save(
        &o,
        ID,
        &St {
            cause: "account:work-2".to_string(),
            relaunched_at: after,
            ..St::default()
        },
    );
    assert!(standing_limit(&o, &unnamed, tail).is_some());
    let under = |st: St, live: &Snapshot, tail: &str| {
        let _ = std::fs::remove_file(state_dir(&o).join(format!("{ID}.json")));
        save(&o, ID, &st);
        standing_limit(&o, live, tail)
    };
    let live = |login: Option<&str>, argv: &[&str]| Snapshot {
        login: login.map(str::to_owned),
        argv: words(argv),
        ..snap.clone()
    };
    let bare = ["/opt/claude"];
    let account = St {
        cause: "account:work-2".to_string(),
        tab: "s-crashed".to_string(),
        relaunched_at: after,
        login: Some("/accts/work-2".to_string()),
        ..St::default()
    };
    assert!(
        under(account.clone(), &live(None, &bare), tail).is_some(),
        "an unread login: the limit stands"
    );
    assert!(
        under(account.clone(), &live(Some("/accts/work-1"), &bare), tail).is_some(),
        "back under the login the move left"
    );
    assert_eq!(
        under(account, &live(Some("/accts/work-2"), &bare), tail),
        None,
        "under the login moved to"
    );
    let kept = St {
        cause: CAUSE_HOST.to_string(),
        tab: "s-restored".to_string(),
        relaunched_at: after + 60,
        moved_at: after,
        moved_login: Some("work-2".to_string()),
        ..St::default()
    };
    assert!(under(kept.clone(), &live(None, &bare), tail).is_some());
    assert_eq!(under(kept, &live(Some("work-2"), &bare), tail), None);
    let model = St {
        cause: "model:opus".to_string(),
        tab: "s-crashed".to_string(),
        relaunched_at: after,
        fallback_from: Some("claude-sonnet-x".to_string()),
        ..St::default()
    };
    assert!(
        under(
            model.clone(),
            &live(None, &["/opt/claude", "--model", "claude-sonnet-x"]),
            &sonnet
        )
        .is_some(),
        "back on the model the fallback left"
    );
    assert!(
        under(
            model.clone(),
            &live(None, &["/opt/claude", "--model=claude-sonnet-x"]),
            &sonnet
        )
        .is_some(),
        "its `--model=` spelling"
    );
    assert_eq!(
        under(
            model,
            &live(None, &["/opt/claude", "--model", "opus"]),
            &sonnet
        ),
        None,
        "under the fallback"
    );
    let older = St {
        cause: "account:work-2".to_string(),
        tab: "s-crashed".to_string(),
        relaunched_at: after,
        ..St::default()
    };
    assert_eq!(under(older, &live(None, &bare), tail), None);
}

/// A MOVE TO THE DEFAULT LOGIN IS MET BY AN UNSET VARIABLE UNDER THE DEFAULT
/// HOME (the review's RR3-R4-1).
#[test]
fn a_move_to_the_default_login_is_met_by_an_unset_variable_under_the_default_home() {
    const ID: &str = "0badf00d-1111-2222-3333-444455556666";
    const WRITTEN: u64 = 1_791_477_331;
    let tail = include_str!("../supervise/fixtures/claude-2.1.294-transcript-weekly-limit.jsonl");
    let dir = crate::supervise::ScratchDir::new("relaunch", "default-login-move");
    let o = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: Some("s-restored".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let live = |login: Option<&str>, home: Option<&str>| Snapshot {
        tab: "s-restored".to_string(),
        pid: 4242,
        start: "Thu Oct 8 18:04:23 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude"]),
        session: Some(ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.294".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(upgrade::Dialect::Zsh),
        login: login.map(str::to_owned),
        home: home.map(str::to_owned),
        claude_dir: None,
        stored_login: false,
        profile: None,
    };
    let stands = |target: &str, live: &Snapshot| {
        let mut read = Vec::new();
        for st in [
            St {
                cause: "account:personal".to_string(),
                tab: "s-restored".to_string(),
                relaunched_at: WRITTEN + 600,
                login: Some(target.to_string()),
                ..St::default()
            },
            St {
                cause: CAUSE_HOST.to_string(),
                tab: "s-restored".to_string(),
                relaunched_at: WRITTEN + 700,
                moved_at: WRITTEN + 600,
                moved_login: Some(target.to_string()),
                ..St::default()
            },
        ] {
            let _ = std::fs::remove_file(state_dir(&o).join(format!("{ID}.json")));
            save(&o, ID, &st);
            read.push(standing_limit(&o, live, tail).is_some());
        }
        assert_eq!(read[0], read[1], "the move its own and kept read alike");
        read[0]
    };
    assert!(!stands("", &live(Some(""), None)), "the default named");
    assert!(!stands("", &live(None, None)), "unset, the default home");
    assert!(
        !stands("", &live(None, Some(""))),
        "unset, the home set and empty"
    );
    assert!(
        !stands("", &live(Some(""), Some("/alt"))),
        "the default named under another home"
    );
    assert!(!stands("/accts/work-2", &live(Some("/accts/work-2"), None)));
    assert!(
        !stands("/alt", &live(None, Some("/alt"))),
        "unset under the home the move's login names"
    );
    assert!(
        stands("", &live(None, Some("/alt"))),
        "CONTROL: unset under another home is that home's login"
    );
    assert!(
        stands("", &live(Some("/accts/work-1"), None)),
        "CONTROL: another login named"
    );
    assert!(
        stands("/accts/work-2", &live(None, None)),
        "CONTROL: unset against another account's login"
    );
    assert!(
        stands("/accts/work-2", &live(Some(""), None)),
        "CONTROL: the default against another account's login"
    );
    assert!(
        stands("/alt", &live(Some(""), Some("/alt"))),
        "CONTROL: the default named under that home is not its login"
    );
    assert!(
        stands("/alt/", &live(None, Some("/alt"))),
        "CONTROL: the string is hashed as given, a trailing slash another login"
    );
}

/// THE COMMAND A PERSON IS TOLD CARRIES THE HOME ITS RELAUNCH CARRIES
/// ([`told_command`] LC-4): an agent under another `HOME`
/// is told `HOME=<it> claude --resume <id>`, one under an absolute
/// `CLAUDE_CONFIG_DIR` that variable, as the relaunch line carries them;
/// told bare, run at its tab's shell under another home, the line found no
/// transcript, or met the first-run screen.
#[cfg(unix)]
#[test]
fn the_command_a_person_is_told_carries_the_home_its_relaunch_carries() {
    let user = Path::new("/u/me");
    let snap = |home: Option<&str>, claude_dir: Option<&str>, cwd: &str| Snapshot {
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--verbose"]),
        session: Some(LOGIN_ID.to_string()),
        cwd: cwd.to_string(),
        version: Some("2.1.300".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: Some(LOGIN_DIR.to_string()),
        home: home.map(str::to_string),
        claude_dir: claude_dir.map(PathBuf::from),
        stored_login: false,
        profile: None,
    };
    let bare = format!("claude --verbose --resume {LOGIN_ID}");
    assert_eq!(
        told_command(user, None, &snap(None, Some("/u/other/.claude"), "/w")),
        Some(format!("HOME=/u/other {bare}")),
        "another HOME"
    );
    assert_eq!(
        told_command(user, None, &snap(Some("/abs/.claude"), None, "/w")),
        Some(format!("CLAUDE_CONFIG_DIR=/abs/.claude {bare}")),
        "an absolute CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        told_command(
            user,
            None,
            &snap(Some("rel/.claude"), Some("/work/b/rel/.claude"), "/work/c")
        ),
        None,
        "a relative home the line would move"
    );
    let own = told_command(user, None, &snap(None, None, "/w"));
    assert_eq!(
        own,
        super::super::resume::command(&words(&["/opt/claude", "--verbose"]), LOGIN_ID)
    );
    assert_eq!(own.as_deref(), Some(bare.as_str()));
    let codex = Snapshot {
        run: crate::harness::relaunch::AgentRun::Codex(CodexRun {
            home: PathBuf::from("/c"),
            home_env: None,
            self_claimed: None,
        }),
        ..snap(None, None, "/w")
    };
    assert_eq!(told_command(user, None, &codex), None, "a Codex");
}

/// The tap's value for the installed app's path.
#[cfg(unix)]
fn tap_value() -> String {
    super::super::tap::settings_value(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"))
        .expect("an admissible path")
}

/// The tap rides before `--resume`, quoted as each shell needs it, and last on a fresh start; with
/// none, [`line_tapped`]'s and [`line_for`]'s line is each shell's golden line, byte for byte.
#[cfg(unix)]
#[test]
fn the_tap_rides_before_the_resume_in_every_shells_form() {
    let value = tap_value();
    let tail = format!("'/opt/claude' '--verbose' '--settings' '{value}' '--resume' '{LOGIN_ID}'");
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    let line = |dialect: Dialect, session: Option<&str>, tap: Option<&str>| {
        line_tapped(
            dialect,
            None,
            None,
            Path::new("/opt/claude"),
            &args(&["claude", "--verbose"]),
            session,
            None,
            None,
            &[],
            tap,
        )
        .unwrap_or_else(|_| panic!("a line"))
    };
    for dialect in [Dialect::Zsh, Dialect::Bash] {
        assert_eq!(
            line(dialect, Some(LOGIN_ID), Some(&value)),
            format!(" {survey} {tail}")
        );
    }
    assert_eq!(
        line(Dialect::Fish, Some(LOGIN_ID), Some(&value)),
        format!(" env {survey} {tail}")
    );
    assert_eq!(
        line(Dialect::Zsh, None, Some(&value)),
        format!(" {survey} '/opt/claude' '--verbose' '--settings' '{value}'")
    );
    let untapped = format!("{survey} '/opt/claude' '--verbose' '--resume' '{LOGIN_ID}'");
    for (dialect, golden) in [
        (Dialect::Zsh, format!(" {untapped}")),
        (Dialect::Bash, format!(" {untapped}")),
        (Dialect::Fish, format!(" env {untapped}")),
    ] {
        assert_eq!(line(dialect, Some(LOGIN_ID), None), golden, "{dialect:?}");
        let plain = line_for(
            dialect,
            None,
            None,
            Path::new("/opt/claude"),
            &args(&["claude", "--verbose"]),
            Some(LOGIN_ID),
            None,
            None,
            &[],
        );
        assert_eq!(plain.ok(), Some(golden), "{dialect:?}");
    }
    let flags = with_tap(args(&["--verbose", "--resume", "X"]), "v");
    assert_eq!(
        flags,
        args(&["--verbose", "--settings", "v", "--resume", "X"])
    );
    assert_eq!(flags.iter().filter(|f| *f == "--resume").count(), 1);
}

/// The tap is dropped before anything required: a launch whose line fits its bound only without
/// the tap is relaunched without it, the login still first on the line; one that fits only without
/// the tap and the survey switch keeps the login alone.
#[cfg(unix)]
#[test]
fn the_tap_is_dropped_before_the_required_login() {
    let value = tap_value();
    let login = login_env(Some("/logins/w"));
    let line = |pad: usize| {
        line_tapped(
            Dialect::Bash,
            None,
            None,
            Path::new("/opt/claude"),
            &args(&["claude", "--add-dir"])
                .into_iter()
                .chain([format!("/{}", "d".repeat(pad))])
                .collect::<Vec<_>>(),
            Some(LOGIN_ID),
            None,
            None,
            &login,
            Some(&value),
        )
    };
    let base = |pad: usize| {
        let argv = args(&["claude", "--add-dir"])
            .into_iter()
            .chain([format!("/{}", "d".repeat(pad))])
            .collect::<Vec<_>>();
        let flags = upgrade::rewrite_argv(&argv, LOGIN_ID).expect("flags");
        upgrade::relaunch_line_env(
            Dialect::Bash,
            None,
            None,
            &login,
            Path::new("/opt/claude"),
            &flags,
        )
        .expect("a line")
        .len()
    };
    let roomy = line(1).unwrap_or_else(|_| panic!("a roomy line"));
    assert!(
        roomy.contains("harness tap") && roomy.contains("FEEDBACK_SURVEY"),
        "{roomy}"
    );
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 ".len();
    let fits_switch = upgrade::MAX_LINE - survey - base(1) + 1;
    assert_eq!(base(fits_switch) + survey, upgrade::MAX_LINE);
    let line_at = line(fits_switch).unwrap_or_else(|_| panic!("relaunched without the tap"));
    assert!(!line_at.contains("harness tap"), "{}", line_at.len());
    assert!(line_at.contains("FEEDBACK_SURVEY"), "{line_at}");
    assert!(
        line_at.starts_with(" CLAUDE_SECURESTORAGE_CONFIG_DIR=/logins/w "),
        "{line_at}"
    );
    let fits_bare = upgrade::MAX_LINE - base(1) + 1;
    assert_eq!(base(fits_bare), upgrade::MAX_LINE);
    let line_at = line(fits_bare).unwrap_or_else(|_| panic!("relaunched on the login alone"));
    assert!(!line_at.contains("harness tap") && !line_at.contains("FEEDBACK_SURVEY"));
    assert!(
        line_at.starts_with(" CLAUDE_SECURESTORAGE_CONFIG_DIR=/logins/w "),
        "{line_at}"
    );
    assert!(line(fits_bare + 1).is_err());
}

/// A PROVEN SELF-CLAIMED CODEX-ALAB'S RELAUNCH AFTER ATERM ENDED IS THE CRASH
/// RULE'S (the self-claim contract, §4; [`CodexRun::self_claimed`]): a
/// snapshot that carries the line aterm's copy of its record validated — off
/// a crash journal's or a restart's leaf, or owed at a seamless update's
/// Commit — is relaunched by [`after_host_ended`] on that line, through the
/// crash rule's own relaunch, its once-per-witness ledger read: a witness
/// handled already is `ended:handled`, and one that is not would be typed
/// (`would-relaunch`, a dry run).
#[cfg(unix)]
#[test]
fn a_relaunch_after_aterm_ended_is_made_on_the_carried_line_as_the_crash_rule_makes_it() {
    const UUID: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let codex_home = tmp.path().join("codex");
    let day = codex_home.join("sessions/2026/10/16");
    std::fs::create_dir_all(&day).expect("sessions");
    std::fs::write(
        day.join(format!("rollout-2026-10-16T08-00-00-{UUID}.jsonl")),
        "{}\n",
    )
    .expect("rollout");
    let opts = super::super::upgrade_drive::Opts {
        home: tmp.path().join("home"),
        state: tmp.path().join("state"),
        sock: None,
        only_sid: Some("s-1".to_string()),
        dry_run: true,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    };
    let mut gone = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .expect("spawn true");
    let dead = gone.id();
    let _ = gone.wait();
    let launch = "/u/.aterm/agents/codex".to_string();
    let line = super::super::self_claimed::Resume {
        argv: vec![launch.clone(), "resume".to_string(), UUID.to_string()],
        session: UUID.to_string(),
        codex_home: None,
        cwd: None,
    };
    let snap = |self_claimed: Option<super::super::self_claimed::Resume>| Snapshot {
        stored_login: false,
        profile: None,
        tab: "s-1".to_string(),
        pid: dead,
        start: "Thu Oct 16 08:00:00 2026".to_string(),
        shell: std::process::id(),
        program: launch.clone().into(),
        argv: vec![launch.clone()],
        session: Some(UUID.to_string()),
        cwd: "/".to_string(),
        version: Some("0.163.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Codex(CodexRun {
            home: codex_home.clone(),
            home_env: None,
            self_claimed,
        }),
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    let carried = snap(Some(line.clone()));
    assert_eq!(self_claimed_line(&carried), Some(&line));
    for left in [ExitRecord::Removed, ExitRecord::Unread] {
        let step = after_exit(&opts, &carried, &left, true, None).step;
        assert_ne!(step, "would-relaunch", "{left:?}: never on the line");
        assert!(step.starts_with("ended:"), "{left:?}: {step}");
    }
    assert_eq!(
        after_host_ended(&opts, &carried, true).step,
        "would-relaunch",
        "the crash rule's relaunch, on the carried line"
    );
    super::super::self_claimed::Witnesses::under(&opts.state)
        .record(dead, &carried.start)
        .expect("the witness");
    assert_eq!(
        after_host_ended(&opts, &carried, true).step,
        "ended:handled",
        "relaunched once per witness, as the crash rule relaunches"
    );
    let lane = snap(None);
    assert_eq!(self_claimed_line(&lane), None);
    assert_eq!(after_host_ended(&opts, &lane, true).step, "ended:no-lane");
    assert_eq!(
        after_exit(
            &opts,
            &lane,
            &ExitRecord::Unread,
            true,
            Some(super::super::restart_stamp::Cause::Asked)
        )
        .step,
        "ended:no-lane"
    );
    assert_eq!(
        after_exit(&opts, &lane, &ExitRecord::Removed, true, None).step,
        "ended:no-lane"
    );
}

/// AN `adopted` ROW PROMISES WHAT THE LOOP'S FIRST IDLE POINT DOES (the live
/// lab of 2026-10-17: a restored tab under Land was told "its supervisor
/// types the continuation", and `done:landing` followed with nothing typed;
/// a cleared conversation under Cruise was told the same, and
/// `done:taskless` followed — the review's RV2-REL-1): a quiet restart is
/// resumed with nothing typed; a conversation its own records owe nothing
/// (`nothing_owed`: taskless, finished, left to its person, resumed by its
/// loop first) gets nothing, the row naming why; a tab under Land — the
/// pass's word, or the tab's own `status` read at the adoption — gets no
/// continuation, its loop deciding its point; and any other move owes the
/// continuation.
#[test]
fn an_adoption_promises_what_its_first_idle_point_does() {
    let quiet = St {
        quiet: true,
        ..St::default()
    };
    let owed = St::default();
    let why = NOBODY_ASKED;
    assert_eq!(
        Adopted::of(
            &quiet,
            || panic!("the records are not read for a quiet move"),
            || panic!("Land is not read for a quiet move")
        ),
        Adopted::Quiet
    );
    assert_eq!(
        Adopted::of(
            &owed,
            || Some(why),
            || panic!("Land is not read where nothing is owed")
        ),
        Adopted::NothingOwed(why)
    );
    assert_eq!(Adopted::of(&owed, || None, || true), Adopted::Land);
    assert_eq!(Adopted::of(&owed, || None, || false), Adopted::Owed);
    let [q, n, l, o] = [
        Adopted::Quiet,
        Adopted::NothingOwed(why),
        Adopted::Land,
        Adopted::Owed,
    ]
    .map(Adopted::detail);
    let rows = [&q, &n, &l, &o];
    for (i, a) in rows.iter().enumerate() {
        for b in &rows[i + 1..] {
            assert_ne!(a, b, "four promises, four rows");
        }
    }
    assert!(n.ends_with(why), "the row names why: {n}");
    assert!(land_by_status(
        "OK schema=1 hold=0 supervisor=aterm-harness@1 autopilot=land land_at=- task=open"
    ));
    for line in [
        "OK schema=1 hold=0 supervisor=- autopilot=cruise",
        "OK schema=1 autopilot=off",
        "OK schema=1 hold=0 hand=-",
        "ERR no such session autopilot=land",
        "OK subject=autopilot%3Dland autopilot=cruise",
        "OK autopilot=- autopilot=land",
    ] {
        assert!(!land_by_status(line), "{line}");
    }
}

/// A WORKER'S FIRST LINE HANDS ITS TASK OVER AS ONE ARGUMENT, BYTE FOR BYTE
/// (`spawn agent=claude`, [`task_launch_line`]): each shell runs the line it
/// is written for, and the program it starts — a stand-in that writes its argv
/// NUL-separated — gets `--model <m>`, `--`, then the task file's text with
/// its trailing newlines trimmed, whatever the text holds: lines, both
/// quotes, a `$(…)` and backticks the shell must not run, a leading `-`.
#[cfg(unix)]
#[test]
fn a_workers_first_line_hands_the_task_over_as_one_argument() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-task-line")
        .tempdir()
        .expect("scratch dir");
    let exe = tmp.path().join("claude");
    std::fs::write(
        &exe,
        "#!/bin/sh\n{ for a in \"$@\"; do printf '%s\\000' \"$a\"; done; \
         printf 'HOME_ENV=%s\\000' \"$CLAUDE_CONFIG_DIR\"; } > \"$ARGV_OUT\"\n",
    )
    .expect("write the stand-in");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let ran = tmp.path().join("ran");
    let task = format!(
        "-leading dash\nline two with 'single' and \"double\" quotes\n\
         $(touch {ran}) and `touch {ran}` stay text\n\n\n",
        ran = ran.display()
    );
    let file = tmp.path().join("task.txt");
    std::fs::write(&file, &task).expect("write the task");
    let mut shells = vec![(Dialect::Zsh, "/bin/zsh"), (Dialect::Bash, "/bin/bash")];
    if !cfg!(target_os = "macos") {
        shells.retain(|(_, sh)| Path::new(sh).exists());
    }
    aterm_spec::wait::first_exec(&exe, &[]);
    let run = |dialect: Dialect, shell: &str, prelude: &str, line: &str| {
        let argv_out = tmp.path().join(format!("argv-{}", dialect.hook_ext()));
        let _ = std::fs::remove_file(&argv_out);
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(shell)
                .args(["-f", "-c", &format!("{prelude}\n{line}")])
                .env("ATERM_TASK_FILE", &file)
                .env("ARGV_OUT", &argv_out),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap_or_else(|e| panic!("{shell} within the hang detector: {e}"));
        assert!(
            out.status.success(),
            "{shell}: {line}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read(&argv_out).expect("the stand-in ran")
    };
    for (dialect, shell) in shells {
        let line = task_launch_line(
            dialect,
            &exe,
            Some("opus-4.1"),
            Some("/homes/w"),
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{shell}: {e}"));
        assert!(line.ends_with(upgrade::TASK_OPERAND), "{line}");
        let aliased = match dialect {
            Dialect::Bash => "shopt -s expand_aliases; alias cat='echo ALIASED'",
            _ => "alias cat='echo ALIASED'",
        };
        for prelude in [":", aliased] {
            let argv = run(dialect, shell, prelude, &line);
            let argv: Vec<&[u8]> = argv.split(|b| *b == 0).collect();
            assert_eq!(
                argv,
                [
                    b"--model".as_slice(),
                    b"opus-4.1",
                    b"--",
                    task.trim_end_matches('\n').as_bytes(),
                    b"HOME_ENV=/homes/w",
                    b"",
                ],
                "{shell} after {prelude:?}: {line}"
            );
        }
        assert!(!ran.exists(), "{shell} ran the task's text");
        let bare = line.replace("command cat -- ", "cat ");
        assert_ne!(bare, line);
        let argv = run(dialect, shell, aliased, &bare);
        assert!(
            argv.split(|b| *b == 0).any(|a| a.starts_with(b"ALIASED ")),
            "{shell}: the control's alias took the bare cat"
        );
    }
    let evaluated = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/bin/sh")
            .args([
                "-c",
                r#"eval "$(sed -n 3p "$ATERM_TASK_FILE")" 2>/dev/null; true"#,
            ])
            .env("ATERM_TASK_FILE", &file),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sh within the hang detector: {e}"));
    assert!(
        evaluated.status.success() && ran.exists(),
        "the control ran the text"
    );
    let big = "x".repeat(60 * 1024);
    assert!(
        upgrade::relaunch_line_env(Dialect::Zsh, None, None, &[], &exe, &[big]).is_err(),
        "a task as a word of the line"
    );
}

/// The worker's line under fish is `env NAME=VALUE …` (fish has no prefix
/// assignment), its operand the same fixed text; a value the line cannot
/// carry refuses the line rather than starting the worker under the wrong
/// home.
#[test]
fn a_workers_first_line_is_written_per_dialect_and_refuses_what_it_cannot_carry() {
    let exe = Path::new("/opt/claude");
    let fish = task_launch_line(Dialect::Fish, exe, None, Some("/h w"), None, Some(""))
        .expect("a fish line");
    assert!(
        fish.starts_with(" env CLAUDE_CONFIG_DIR='/h w' CLAUDE_SECURESTORAGE_CONFIG_DIR='' "),
        "{fish}"
    );
    assert!(
        fish.ends_with(&format!("'/opt/claude' {}", upgrade::TASK_OPERAND)),
        "{fish}"
    );
    let bare = task_launch_line(Dialect::Zsh, exe, None, None, None, None).expect("a bare line");
    assert_eq!(
        bare,
        format!(
            " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' {}",
            upgrade::TASK_OPERAND
        )
    );
    assert!(task_launch_line(Dialect::Bash, exe, None, Some("/a\nb"), None, None).is_err());
    let long = format!("/{}", "h".repeat(upgrade::MAX_LINE));
    assert!(task_launch_line(Dialect::Bash, exe, None, Some(&long), None, None).is_err());
    let homed = task_launch_line(Dialect::Zsh, exe, None, None, Some("/homes/u"), None)
        .expect("a line carrying HOME");
    assert!(
        homed.starts_with(&format!(" {}=", accounts::HOMEDIR_ENV)),
        "{homed}"
    );
    assert!(homed.contains("/homes/u"), "{homed}");
    assert!(task_launch_line(Dialect::Zsh, exe, None, Some("other/.claude"), None, None).is_err());
    assert!(task_launch_line(Dialect::Zsh, exe, None, Some(""), None, None).is_err());
}

/// WHAT A WORKER CARRIES OF ITS MANAGER'S CLAUDE HOME ([`worker_homes`]):
/// an absolute `CLAUDE_CONFIG_DIR` as it is; a relative one (empty
/// included) as the absolute directory it named for the manager, raw only
/// where that was not read (which the line refuses); with none, the
/// manager's own `HOME` where its records are under it and it is not the
/// user's.
#[cfg(unix)]
#[test]
fn a_worker_carries_its_managers_claude_home_resolved_where_the_manager_runs() {
    use super::super::upgrade_drive::HomeVars;
    let user = Path::new("/Users//u");
    let vars = |config, home| HomeVars { config, home };
    assert_eq!(
        worker_homes(user, vars(Some("/h/abs"), None), Some(Path::new("/h/abs"))),
        (Some("/h/abs".to_string()), None)
    );
    assert_eq!(
        worker_homes(
            user,
            vars(Some("other/.claude"), None),
            Some(Path::new("/work/m/other/.claude"))
        ),
        (Some("/work/m/other/.claude".to_string()), None),
        "a relative home is placed where the manager runs"
    );
    assert_eq!(
        worker_homes(user, vars(Some(""), None), Some(Path::new("/work/m"))),
        (Some("/work/m".to_string()), None),
        "an empty home is the manager's own directory"
    );
    assert_eq!(
        worker_homes(user, vars(Some("rel"), None), None),
        (Some("rel".to_string()), None),
        "unread: left raw, for the line to refuse"
    );
    assert_eq!(
        worker_homes(user, vars(None, Some("/x")), Some(Path::new("/x/.claude"))),
        (None, Some("/x".to_string())),
        "a manager under another HOME has it carried"
    );
    assert_eq!(
        worker_homes(
            user,
            vars(None, Some("/Users//u")),
            Some(Path::new("/Users//u/.claude"))
        ),
        (None, None),
        "the user's own HOME: nothing carried"
    );
    assert_eq!(
        worker_homes(user, vars(None, None), Some(Path::new("/Users//u/.claude"))),
        (None, None)
    );
}

/// A model-shaped option value or payload does not change the model the
/// launch selected, so it cannot prove that a limit's model was left.
#[test]
fn a_model_move_reads_the_launchs_model_and_never_another_flags_value() {
    let live = |argv: &[&str]| Snapshot {
        tab: "s-cafe".to_string(),
        pid: 0,
        start: String::new(),
        shell: 0,
        program: PathBuf::from("/fixture/claude"),
        argv: words(argv),
        session: None,
        cwd: String::new(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    };
    for (argv, expected) in [
        (&["claude"][..], ""),
        (&["claude", "--model", "default"][..], "default"),
        (&["claude", "--model", "opus"][..], "opus"),
        (&["claude", "--model=sonnet"][..], "sonnet"),
        (
            &["claude", "--model", "opus", "--model=sonnet"][..],
            "sonnet",
        ),
        (
            &[
                "claude",
                "--model",
                "opus",
                "--append-system-prompt",
                "--",
                "--model=sonnet",
            ][..],
            "sonnet",
        ),
        (
            &[
                "claude",
                "--append-system-prompt=--model=sonnet",
                "--model",
                "opus",
            ][..],
            "opus",
        ),
    ] {
        let snap = live(argv);
        assert_eq!(launch_model_of(&snap.argv), expected, "{argv:?}");
        assert!(
            !runs_moved(&snap, None, Some(expected)),
            "the selected model still stands: {argv:?}"
        );
        assert!(
            runs_moved(&snap, None, Some("another-model")),
            "a different named model is left: {argv:?}"
        );
        assert!(
            runs_moved(&snap, None, None),
            "an older record names no model target"
        );
    }
    let opus = live(&["claude", "--model", "opus"]);
    assert!(
        runs_moved(&opus, Some(""), Some("sonnet")),
        "ordinary default-login control"
    );
    assert!(
        !runs_moved(&opus, Some("another-login"), Some("sonnet")),
        "a different login never proves the move"
    );

    let mut changed = Vec::new();
    for (argv, expected) in [
        (
            &[
                "claude",
                "--model",
                "opus",
                "--append-system-prompt",
                "--model=sonnet",
            ][..],
            "opus",
        ),
        (
            &[
                "claude",
                "--model",
                "opus",
                "--append-system-prompt",
                "--model",
                "sonnet",
            ][..],
            "opus",
        ),
        (
            &["claude", "--model", "opus", "--add-dir", "--model=sonnet"][..],
            "opus",
        ),
        (
            &["claude", "--model", "opus", "--", "--model=sonnet"][..],
            "opus",
        ),
        (
            &["claude", "--model", "opus", "--", "--model", "sonnet"][..],
            "opus",
        ),
        (
            &["claude", "--append-system-prompt", "--model=sonnet"][..],
            "",
        ),
        (&["claude", "--", "--model=sonnet"][..], ""),
    ] {
        let snap = live(argv);
        let read = launch_model_of(&snap.argv);
        let moved = runs_moved(&snap, None, Some(expected));
        if read != expected || moved {
            changed.push((words(argv), read.to_string(), moved));
        }
    }
    assert!(
        changed.is_empty(),
        "model selection must respect option values and -- boundaries: {changed:?}"
    );
}

/// A MODEL SWITCH ONTO A MODEL BILLED TO USAGE CREDITS (the owner,
/// 2026-10-19: *"for continuing on credits, that's an optional additional
/// feature that's off by default. use subscriptions."*): the fallback and the
/// return whose line asks for a model of a family the profile bills to
/// credits are refused (`refused:model-on-credits`), by id or by Claude
/// Code's alias.
#[test]
fn a_model_switch_onto_a_model_billed_to_usage_credits_is_refused() {
    let billed = ["fable".to_string()];
    let fallback = |to: &str| Restart::Model { to: to.to_string() };
    let back = Restart::ModelBack {
        to: Some("fable".to_string()),
    };
    assert!(moves_onto_credits(
        &fallback("fable"),
        Some("fable"),
        Some(&billed)
    ));
    assert!(moves_onto_credits(
        &fallback("claude-fable-5-1[1m]"),
        Some("claude-fable-5-1[1m]"),
        Some(&billed)
    ));
    assert!(moves_onto_credits(&back, Some("fable"), Some(&billed)));
    assert!(moves_onto_credits(&fallback("opus"), Some("opus"), None));
    assert!(moves_onto_credits(&back, Some("fable"), None));
    assert!(!moves_onto_credits(&back, None, None));
    assert!(!moves_onto_credits(&back, Some("fable"), Some(&[])));
    assert!(!moves_onto_credits(
        &fallback("opus"),
        Some("opus"),
        Some(&billed)
    ));
    assert!(!moves_onto_credits(&back, None, Some(&billed)));
    let account = Restart::Account {
        to: "work-2".to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Wall,
    };
    for rides in [Restart::Memory, account] {
        assert!(!moves_onto_credits(&rides, Some("opus"), None));
        assert!(!moves_onto_credits(
            &rides,
            Some("claude-fable-5-1"),
            Some(&billed)
        ));
    }

    use super::super::profile::Context;
    let home = Path::new("/captured/home");
    let records = home.join(".claude");
    let standard = vec![format!("{}=/captured/home", accounts::HOMEDIR_ENV)];
    let mut custom = standard.clone();
    custom.push("CLAUDE_CODE_CUSTOM_OAUTH_URL=https://auth.example.invalid".to_string());
    let standard_context = Context::of_exec_env(home, &records, &standard).expect("standard");
    let custom_context = Context::of_exec_env(home, &records, &custom).expect("custom");
    let target = standard_context
        .for_launch(home, "/captured/work", &required_env(None, None, None))
        .expect("explicit Standard target");
    assert_eq!(target.context, standard_context);
    assert_eq!(target.unset, [accounts::HOME_ENV, accounts::LOGIN_ENV]);
    assert_eq!(
        target.values,
        [
            (accounts::HOMEDIR_ENV, "/captured/home".to_string()),
            ("CLAUDE_CODE_CUSTOM_OAUTH_URL", String::new()),
        ]
    );
    assert!(billing_allows_launch(
        false,
        ModelBilling::Move(Some(&standard_context)),
        Some("opus"),
        Some(&target.context)
    ));
    assert!(
        custom_context
            .for_launch(home, "/captured/work", &[])
            .is_none()
    );
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(Some(&standard_context)),
        Some("opus"),
        Some(&custom_context)
    ));
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(Some(&custom_context)),
        Some("opus"),
        Some(&target.context)
    ));
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(Some(&custom_context)),
        Some("opus"),
        Some(&custom_context)
    ));
    assert!(billing_allows_launch(
        true,
        ModelBilling::Move(Some(&custom_context)),
        Some("opus"),
        None
    ));
    assert!(billing_allows_launch(
        true,
        ModelBilling::Move(None),
        None,
        None
    ));
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(None),
        Some("opus"),
        Some(&target.context)
    ));
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(Some(&standard_context)),
        Some("opus"),
        None
    ));
    for model in [None, Some("")] {
        assert!(!billing_allows_launch(
            false,
            ModelBilling::Move(Some(&standard_context)),
            model,
            Some(&target.context)
        ));
    }
    assert!(billing_allows_launch(false, ModelBilling::Keep, None, None));
    assert!(Context::of_exec_env(home, &records, &[]).is_none());
    let absent = Context::of_exec_env(home, &records, &["PATH=/captured/bin".to_string()])
        .expect("known absent profile variables");
    let absent_target = absent
        .for_launch(home, "/captured/work", &[])
        .expect("unset carrier");
    assert_eq!(absent_target.context, absent);
    assert_eq!(
        absent_target.unset,
        [
            accounts::HOMEDIR_ENV,
            accounts::HOME_ENV,
            accounts::LOGIN_ENV
        ]
    );
    let empty = Context::of_exec_env(
        home,
        Path::new("/captured/work"),
        &[
            format!("{}=", accounts::HOMEDIR_ENV),
            format!("{}=", accounts::HOME_ENV),
            format!("{}=", accounts::LOGIN_ENV),
        ],
    )
    .expect("set-empty context");
    let empty_target = empty
        .for_launch(home, "/captured/work", &[])
        .expect("empty carrier");
    assert!(empty_target.unset.is_empty());
    assert_eq!(empty_target.context, empty);
    assert_ne!(empty, absent);
    assert!(
        empty.for_launch(home, "", &[]).is_none(),
        "relative config needs known cwd"
    );
    let overlay = required_env(
        Some("/captured/config"),
        Some("/captured/other"),
        Some("named"),
    );
    let moved = vec![
        format!("{}=/captured/other", accounts::HOMEDIR_ENV),
        "CLAUDE_CONFIG_DIR=/captured/config".to_string(),
        format!("{}=named", accounts::LOGIN_ENV),
    ];
    let captured = Context::of_exec_env(home, Path::new("/captured/config"), &moved)
        .expect("captured overrides");
    let overlaid = captured
        .for_launch(home, "/captured/work", &overlay)
        .expect("matching required overlay");
    assert_eq!(captured, overlaid.context);
    assert!(billing_allows_launch(
        false,
        ModelBilling::Move(Some(&captured)),
        Some("opus"),
        Some(&overlaid.context)
    ));
    let changed = standard_context
        .for_launch(home, "/captured/work", &overlay)
        .expect("different required target");
    assert!(!billing_allows_launch(
        false,
        ModelBilling::Move(Some(&standard_context)),
        Some("opus"),
        Some(&changed.context)
    ));
    assert!(
        standard_context
            .for_launch(home, "/captured/work", &[("OTHER", "1")])
            .is_none()
    );

    for dialect in [Dialect::Zsh, Dialect::Bash, Dialect::Fish] {
        let line = upgrade::relaunch_line_env_clear(
            dialect,
            None,
            None,
            &[
                ("HOME", r"/captured/it's\home"),
                ("CLAUDE_CODE_CUSTOM_OAUTH_URL", ""),
            ],
            &[accounts::HOME_ENV, accounts::LOGIN_ENV],
            Path::new("/captured/claude"),
            &["--model".to_string(), "opus".to_string()],
        )
        .expect("quoted exact environment");
        let expected = match dialect {
            Dialect::Fish => concat!(
                " '/usr/bin/env' -u 'CLAUDE_CONFIG_DIR' -u 'CLAUDE_SECURESTORAGE_CONFIG_DIR'",
                " 'HOME=/captured/it\\'s\\\\home' 'CLAUDE_CODE_CUSTOM_OAUTH_URL='",
                " '/captured/claude' '--model' 'opus'"
            ),
            Dialect::Zsh | Dialect::Bash => concat!(
                " '/usr/bin/env' -u 'CLAUDE_CONFIG_DIR' -u 'CLAUDE_SECURESTORAGE_CONFIG_DIR'",
                " 'HOME=/captured/it'\\''s\\home' 'CLAUDE_CODE_CUSTOM_OAUTH_URL='",
                " '/captured/claude' '--model' 'opus'"
            ),
        };
        assert_eq!(line, expected, "{dialect:?}");
        assert!(
            upgrade::relaunch_line_env_clear(
                dialect,
                None,
                None,
                &[],
                &["BAD;NAME"],
                Path::new("/captured/claude"),
                &[]
            )
            .is_err()
        );
        assert!(
            upgrade::relaunch_line_env_clear(
                dialect,
                None,
                None,
                &[("HOME", "value")],
                &["HOME"],
                Path::new("/captured/claude"),
                &[]
            )
            .is_err()
        );
        assert!(
            upgrade::relaunch_line_env_clear(
                dialect,
                None,
                None,
                &[("HOME", "a\nb")],
                &[],
                Path::new("/captured/claude"),
                &[]
            )
            .is_err()
        );
        let values: Vec<_> = target
            .values
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let carried = line_tapped_clear(
            dialect,
            None,
            None,
            Path::new("/captured/claude"),
            &words(&["/captured/claude"]),
            None,
            Some("opus"),
            None,
            &values,
            Some(&target.unset),
            Some("{}"),
        )
        .unwrap_or_else(|_| panic!("required context on tapped line"));
        assert!(carried.contains("-u 'CLAUDE_CONFIG_DIR' -u 'CLAUDE_SECURESTORAGE_CONFIG_DIR'"));
        assert!(carried.contains("'CLAUDE_CODE_CUSTOM_OAUTH_URL='"));
        let too_long = "x".repeat(16_384);
        assert!(
            line_tapped_clear(
                dialect,
                None,
                None,
                Path::new("/captured/claude"),
                &words(&["/captured/claude"]),
                None,
                Some("opus"),
                None,
                &[("HOME", &too_long)],
                Some(&target.unset),
                Some("{}")
            )
            .is_err(),
            "no fallback drops required words"
        );
    }
}

/// EVERY LANE'S CARRY-ON ENDS ON THE CONTINUATION AND ITS NEEDS YOU
/// SENTENCE (Autopilot Cruise / Land): Claude Code's, Gemini CLI's,
/// agy's and Muse Code's relaunch, for every cause, and the upgrade's own,
/// end on [`upgrade::CARRY_ON`] — whose last sentence asks for a NEEDS YOU
/// line ([`crate::supervise::policy::needs::NEEDS_HINT`]) and which names no
/// retired token — so under Cruise [`upgrade::checking`] puts the Cruise
/// prompt in its place for a done claim.
#[test]
fn every_lanes_carry_on_ends_on_the_continuation_and_its_needs_sentence() {
    use crate::supervise::policy::needs::NEEDS_HINT;
    use crate::supervise::policy::turn_end::DONE_CHECK;
    assert!(upgrade::CARRY_ON.ends_with(NEEDS_HINT));
    assert!(!upgrade::CARRY_ON.contains("ATERM-TASK-DONE"));
    let agents = [
        "Claude Code",
        upgrade::Agent::Gemini.product(),
        upgrade::Agent::Agy.product(),
        "Muse Code",
    ];
    let causes = [
        CAUSE_EXIT,
        CAUSE_MEMORY,
        CAUSE_STALL,
        CAUSE_ASKED,
        CAUSE_HOST,
        "model:claude-opus-5-5",
        "model-back:",
    ];
    let mut texts: Vec<String> = agents
        .iter()
        .flat_map(|agent| {
            causes
                .iter()
                .map(move |cause| resumed_prompt_as(agent, "2.1.300", cause))
        })
        .collect();
    let (from, to) = (
        upgrade::Version::parse("2.1.299").expect("a version"),
        upgrade::Version::parse("2.1.300").expect("a version"),
    );
    texts.push(upgrade::continue_prompt(
        &from,
        &to,
        Some("claude-opus-5-5"),
    ));
    for text in &texts {
        assert!(text.ends_with(upgrade::CARRY_ON), "{text}");
        let checked = upgrade::checking(text, false);
        assert!(checked.ends_with(DONE_CHECK), "{checked}");
        assert!(!checked.contains(upgrade::CARRY_ON), "{checked}");
    }
    let other = format!("{} Upgraded.", upgrade::HARNESS_MARK);
    assert_eq!(upgrade::checking(&other, false), other);
}
