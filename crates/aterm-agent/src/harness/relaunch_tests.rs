// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_cold_restore_yields_between_session_checks_but_live_relaunch_keeps_its_wait() {
    assert_eq!(held_wait(CAUSE_HOST), Duration::from_secs(7));
    assert!(held_wait(CAUSE_HOST) > ENDED_AT_ONCE);
    assert_eq!(held_wait(CAUSE_EXIT), HELD_WAIT);
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// The program a relaunch runs is what the process RUNS, not the word it was
/// typed as: a `#!` script (the managed twin, an npm install, a stand-in)
/// shows its interpreter as the image and itself as `argv[1]`; a binary is
/// the kernel's image. Negative controls: a wrapper that runs Claude as a
/// child (`caffeinate claude`), a relative `argv[1]`, and an image that is
/// neither named claude, native, nor registered are no program to relaunch.
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
    // Negative controls.
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

/// The decision, whose exit it was: a person's (a keystroke within the
/// grace), a holder's (`hold=1`, a lease, a named driver's turn), else the
/// owner's limit, else nobody's — relaunched. A person or a holder is asked
/// about first, so their own exit is never said to them as a limit.
/// Negative controls: a keystroke older than the grace, `-`, a server that
/// does not say, an unreadable status, an unnamed turn (this host's own
/// loop types so) and a `driving:` hand are nobody.
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
    // An agent that had stopped reading its input, exited by the stall's
    // remedy: a keystroke just before was read by nothing, so it is no
    // person's exit. A holder still owns it, and the owner's limit still holds.
    assert_eq!(
        on_exit(true, Some(&typed(1)), 120, true, false),
        OnExit::Relaunch
    );
    assert_eq!(
        on_exit(
            true,
            Some(&st("hold=1 hand=- human_ms=1")),
            120,
            true,
            false
        ),
        OnExit::Held
    );
    assert_eq!(
        on_exit(false, Some(&typed(1)), 120, true, false),
        OnExit::Limited
    );
    // The harness's own restart ended the agent: its exit is that restart's,
    // whoever is at the tab, a hold, a lease or the owner's switch (the
    // relaunch line's own look still waits on them before anything is typed).
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

/// One step's word, read for the host: relaunched; left — the launch's own
/// end (a one-shot run, a launch that never registered a conversation) or
/// no exit at all — journaled and never said; never (an interactive
/// conversation that cannot come back) — said. Negative control: every wait
/// and every failure that a later attempt can get past is not the
/// irreducible case.
#[test]
fn a_step_is_relaunched_left_not_yet_or_never() {
    assert_eq!(
        outcome("adopted"),
        Outcome::Relaunched,
        "its loop carries on"
    );
    assert_eq!(outcome("done:fresh"), Outcome::Relaunched);
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
    // An exit a standing stamp names, its process stopped away from its tab:
    // owed, looked at again uncounted (ruling 13 of 2026-10-08) — never the
    // bare `not-exited` above, which is left.
    assert_eq!(outcome(WAIT_ACT_ENDS), Outcome::Busy);
    for step in [
        "wait:shell-prompt",
        "wait:resume",
        "failed:no-resume",
        "failed:relaunch:ERR-busy",
        "wait:tab-ownership-changed",
    ] {
        assert_eq!(outcome(step), Outcome::NotYet(step.to_string()), "{step}");
    }
}

/// The relaunched agent reads as ended only once every look for
/// [`ENDED_AT_ONCE`] found its shell holding the tab, looks taken
/// [`SHELL_LOOK`] apart. Negative controls: the agent still starting starts
/// the count over; an unreadable look is no verdict; a look before
/// [`SHELL_LOOK`] has passed is not taken.
#[test]
fn a_shell_that_has_the_tab_back_reads_as_ended_only_after_a_while() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut back = ShellBack::default();
    assert!(back.due(at(0)));
    assert!(!back.due(at(400)), "not at the roster's pace");
    assert!(back.due(at(1_000)));
    assert!(!back.saw(Some(true), at(0)), "held just now");
    assert!(!back.saw(None, at(3_000)), "an unread look is no verdict");
    assert!(!back.saw(Some(false), at(4_000)), "the agent holds it");
    assert!(!back.saw(Some(true), at(5_000)), "the count started over");
    assert!(!back.saw(None, at(9_000)));
    assert!(!back.saw(Some(true), at(9_999)));
    assert!(
        back.saw(Some(true), at(10_000)),
        "held five seconds in a row"
    );
    let mut never = ShellBack::default();
    for s in 0..60 {
        assert!(!never.saw(None, at(s * 1_000)), "never read, never ended");
    }
}

/// Another actor on the lock is no miss: an upgrade step in another tab can
/// hold it for minutes, and a crash here meanwhile must not read as a
/// relaunch that keeps failing. However many busy looks in a row, the
/// back-off, the misses and the attention stay where they were.
/// Negative control: the same number of real misses is said.
#[test]
fn another_actor_on_the_lock_is_never_a_miss() {
    let t0 = Instant::now();
    let mut r = Relaunches::default();
    assert!(r.exited(OnExit::Relaunch, t0).is_ok());
    let before = r.clone();
    for _ in 0..BADGE_AFTER * 4 {
        assert_eq!(
            r.attempted(&outcome("busy:another-sweep"), t0),
            Say::Nothing
        );
    }
    assert_eq!(r, before, "a stutter");
    assert!(r.pending(), "still due");
    let said: Vec<Say> = (0..BADGE_AFTER)
        .map(|_| r.attempted(&outcome("wait:shell-prompt"), t0))
        .collect();
    assert!(said.contains(&Say::Failing), "{said:?}");
}

/// A one-shot run is read wherever its flag stands — after a prompt, after a
/// flag that takes a value, after a flag the table does not know — and its
/// exit is its own end. Negative controls: an interactive launch; a `-p`
/// that is another flag's value (`--append-system-prompt -p`); and one after
/// `--`, where Claude's parser reads no flag.
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

/// Whether an exit would be relaunched on its conversation, from the snapshot
/// alone ([`resumes_on_exit`]): the frozen remedy says "aterm relaunches it"
/// only where the relaunch's plan would be made, and names
/// `claude --resume <id>` elsewhere. Negative controls, each a launch the
/// relaunch refuses or waits on: an unknown flag (`--permission-prompt-tool`:
/// the relaunch says `argv:unknown-flag`, while the person's own command still
/// resumes the conversation bare), a `--worktree` launch
/// (`argv:not-resumable`), a shell the line is not written for (nushell), no
/// conversation registered, and a one-shot `-p` run.
#[test]
fn an_exit_is_relaunched_only_where_the_relaunch_would_plan_it() {
    const ID: &str = "0badf00d-1111-2222-3333-444455556666";
    let snap = |argv: &[&str]| Snapshot {
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(argv),
        session: Some(ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.283".to_string()),
        codex: None,
        dialect: Some(upgrade::Dialect::Zsh),
        login: None,
        home: None,
    };
    let known = snap(&["/opt/claude", "--model", "opus"]);
    assert!(resumes_on_exit(&known), "a launch the relaunch carries");
    for dialect in [upgrade::Dialect::Bash, upgrade::Dialect::Fish] {
        let other = Snapshot {
            dialect: Some(dialect),
            ..known.clone()
        };
        assert!(resumes_on_exit(&other), "{dialect:?}");
    }

    let unknown = ["/opt/claude", "--permission-prompt-tool", "mcp__x"];
    assert!(
        upgrade::rewrite_argv(&words(&unknown), ID).is_err(),
        "the premise: the relaunch refuses this argv"
    );
    assert!(!resumes_on_exit(&snap(&unknown)), "argv:unknown-flag");
    assert_eq!(
        super::super::resume::command(&words(&unknown), ID).as_deref(),
        Some("claude --resume 0badf00d-1111-2222-3333-444455556666"),
        "the person's command still resumes the conversation"
    );
    assert!(
        !resumes_on_exit(&snap(&["/opt/claude", "--worktree"])),
        "argv:not-resumable"
    );
    assert!(
        !resumes_on_exit(&snap(&["/opt/claude", "-p", "x"])),
        "one-shot"
    );
    let nushell = Snapshot {
        dialect: None,
        ..known.clone()
    };
    assert!(
        !resumes_on_exit(&nushell),
        "no dialect: plan waits for ever"
    );
    let unregistered = Snapshot {
        session: None,
        ..known.clone()
    };
    assert!(!resumes_on_exit(&unregistered), "no conversation to resume");
    let control = Snapshot {
        cwd: "/w\nx".to_string(),
        ..known
    };
    assert!(
        !resumes_on_exit(&control),
        "no line with a control character"
    );
}

/// Following the snapshot's agent: Claude's own record of the same process
/// (pid and kernel start) moves its conversation, build and directory — an
/// in-app `/clear` names a new conversation under the same pid. The tab's
/// published foreground group says whether the snapshot still describes the
/// tab. Negative controls: a record of another start (a reused pid) and a
/// conversation that is no session id move nothing.
#[test]
fn a_snapshot_follows_its_own_process_and_nothing_else() {
    let home = std::env::temp_dir().join(format!("aterm-relaunch-follow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("home");
    let start = "Thu Sep 24 01:02:03 2026";
    let snap = Snapshot {
        tab: "s-1".to_string(),
        pid: 4242,
        start: start.to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude"]),
        session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.281".to_string()),
        codex: None,
        dialect: Some(upgrade::Dialect::Zsh),
        login: None,
        home: None,
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
    // Negative controls.
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
    // Where the tab's foreground stands.
    assert_eq!(foreground(&snap, 4242), Foreground::Agent);
    assert_eq!(foreground(&snap, 4241), Foreground::Shell);
    assert_eq!(foreground(&snap, 5000), Foreground::Other);
    let _ = std::fs::remove_dir_all(&home);
}

/// The relaunch's build obeys `[harness] upgrade`: on, a crashed session
/// comes back on the newest build it may move to; off, on the program it
/// ran, whatever is installed — configuration only takes power away.
/// Negative controls: nothing newer, and a running version that cannot be
/// read, relaunch the same program with the switch on too.
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
        relaunch_target(on, running, c, None, ran, Some("2.1.281"), 0)
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

/// The back-off grows 1 s, 10 s, 60 s, then ten minutes for as long as the
/// agent keeps exiting — never a give-up — and starts over once a relaunched
/// agent ran [`HEALTHY`]ly. Misses in a row are said once after
/// [`BADGE_AFTER`] and cleared by a relaunch that lands. Negative controls: a
/// crash soon after a relaunch does not start the back-off over, and a
/// person's exit schedules nothing.
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
    // A crash soon after the last relaunch: still the long pause.
    assert_eq!(
        r.exited(OnExit::Relaunch, t0 + Duration::from_secs(60)),
        Ok(Duration::from_secs(600))
    );
    // One that ran healthily first starts over.
    assert_eq!(
        r.exited(OnExit::Relaunch, t0 + Duration::from_secs(5) + HEALTHY),
        Ok(Duration::from_secs(1))
    );
    // A person's exit: nothing due, the tab theirs, nothing said.
    assert_eq!(r.exited(OnExit::PersonAsked, t0), Err(Say::Nothing));
    assert!(!r.pending());
    assert!(r.counts().0, "left to the person");
    // A limited one: nothing due, and said.
    assert_eq!(r.exited(OnExit::Limited, t0), Err(Say::Limited));
    assert!(!r.pending() && r.counts().1, "badged");
    assert_eq!(r.running(), Say::Clear, "an agent back clears it");
    // The launch's own end: nothing due, the tab left, nothing said.
    let _ = r.exited(OnExit::Relaunch, t0);
    assert_eq!(
        r.attempted(&Outcome::Left("one-shot".to_string()), t0),
        Say::Nothing
    );
    assert!(!r.pending() && r.counts().0 && !r.counts().1);
    // Misses: said once after BADGE_AFTER in a row, cleared by a landing.
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
    // Cannot: said, and nothing more is due.
    let _ = m.exited(OnExit::Relaunch, t0);
    assert_eq!(
        m.attempted(&Outcome::Cannot("shell-gone".to_string()), t0),
        Say::Cannot
    );
    assert!(!m.pending());
    assert_eq!(m.running(), Say::Clear, "a person's agent clears the word");
}

/// Drive the real [`Relaunches`], [`on_exit`] and [`outcome`] through every
/// action of `HarnessRelaunchOnExit` and check after each that the state is
/// the model's after the same action, every invariant holds, and every model
/// action is driven, including the harness's own restart's exit (`Restarted`),
/// due though a person typed, a halt stands or the owner switched relaunch off.
/// Negative controls: the model's buggy host relaunches after a person's exit,
/// silently drops a relaunch it cannot make, keeps a limit quiet, and leaves
/// its own restart to a person or reads it as the launch's end; each is caught
/// by an invariant.
#[test]
fn the_real_relaunch_decisions_conform_to_the_model() {
    let model = aterm_spec::derive::harness_relaunch_on_exit_model();
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
    // The one fact the type does not hold: whether an agent runs in the
    // tab, which the host reads off the session's program.
    let mut running = true;
    let mut r = Relaunches::default();
    let mut now = Instant::now();
    let project = |r: &Relaunches, running: bool| {
        let (left, badged, _, misses) = r.counts();
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
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<&'static str, i64>>()
    };
    let mut expect = model.init_state();
    let status = |tail: &str| format!("OK schema=1 {tail}");
    let nobody = status("hold=0 hand=- human_ms=-");
    let person = status("hold=0 hand=- human_ms=2000");
    let halted = status("hold=1 hand=- human_ms=-");
    let leased = status("hold=0 hand=lease:orchestrator human_ms=-");
    // Every action, each real input that produces it at least once (a
    // person and a holder for `PersonExit`/`PersonReturns`, the owner's
    // switch before an exit and during a pause for `Limited`, a one-shot
    // run and an agent that never ended for `Ended`).
    let schedule = [
        ("Crash", "exit"),
        ("Land", "adopted"),
        ("Crash", "exit"),
        ("Miss", "wait:shell-prompt"),
        // Another actor on the lock: no model action at all — a stutter.
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
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Ended", "ended:one-shot"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Ended", "wait:not-exited"),
        ("PersonRuns", ""),
        // The harness's own restart ended it: due though a person typed, a
        // halt stands or the owner switched relaunch off; re-asked during its
        // pause, still due.
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
                    r.exited(on_exit(allowed, Some(who), 120, false, true), now)
                        .is_ok()
                );
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
            }
            ("PersonExit", who) => {
                running = false;
                let who = if who == "person" { &person } else { &halted };
                assert_eq!(
                    r.exited(on_exit(true, Some(who), 120, false, false), now),
                    Err(Say::Nothing)
                );
            }
            ("PersonReturns", who) => {
                let who = if who == "person" { &person } else { &leased };
                assert_eq!(
                    r.decide(on_exit(true, Some(who), 120, false, false)),
                    Some(Say::Nothing)
                );
            }
            ("Limited", "exit") => {
                running = false;
                assert_eq!(
                    r.exited(on_exit(false, Some(&nobody), 120, false, false), now),
                    Err(Say::Limited)
                );
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
                let _ = r.running();
            }
            ("Healthy", _) => {
                now += HEALTHY;
                r.forgive(now);
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
        let seen = project(&r, running);
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &seen),
                "{action}: {} broken by {seen:?}",
                inv.name
            );
        }
        assert_eq!(seen, expect, "{action} ({input}): observed vs model");
        // The restart is nothing else's (the model's guards on the harness's
        // own restart): no person, limit or launch's own end takes it. A person
        // or a limit is decided as the restart ([`on_exit`], above), and an end
        // its carry could never answer (an `ended:*` word, an agent that never
        // exited) is said as a `Cannot`, never a silent leave.
        if expect["restarted"] == 1 {
            for off in ["PersonReturns", "Ended", "Limited"] {
                assert!(!model.action_enabled(off, &expect), "{off} after {action}");
            }
        }
        if expect["restarted"] == 1 && expect["pending"] == 1 {
            for word in ["ended:one-shot", "ended:graceful-exit", "wait:not-exited"] {
                let mut probe = r.clone();
                assert_eq!(probe.attempted(&outcome(word), now), Say::Cannot, "{word}");
                let mut cannot = expect.clone();
                assert!(model.fire("Cannot", &mut cannot));
                assert_eq!(project(&probe, running), cannot, "{word}: a said Cannot");
            }
        }
    }
    // Every action of the model was driven.
    for a in &model.actions {
        assert!(
            schedule.iter().any(|(name, _)| *name == a.name),
            "{} is never driven",
            a.name
        );
    }
    // Negative controls: the buggy host's defects, each caught.
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
    // The harness's own restart left to a person who typed, and one its
    // back-off let expire read as a graceful exit.
    for off in ["PersonReturns", "Ended"] {
        let mut own = buggy.init_state();
        assert!(buggy.fire("Restarted", &mut own) && buggy.fire(off, &mut own));
        assert!(!buggy.check_invariant("RestartCarried", &own), "{off}");
    }
}

/// The continuation a relaunched agent is typed is one line, whatever the
/// version text holds, and it never invites the agent to stop and wait for
/// a person: it decides for itself and keeps going, as the upgrade's does.
/// It says why it was relaunched: an exit nobody asked for, or the host's
/// restart for the memory banner.
#[test]
fn the_resumed_prompt_is_one_line_and_keeps_going() {
    for (cause, head) in [
        (CAUSE_EXIT, "[aterm harness] Relaunched: Claude Code exited"),
        (
            CAUSE_MEMORY,
            "[aterm harness] Restarted: Claude Code's memory was critical (its own warning, or \
             the Mac short of memory with it the largest past aterm's ceiling)",
        ),
        (
            "model:opus",
            "[aterm harness] Relaunched on opus: the model this session ran reached its usage limit",
        ),
        (
            "model-back:fable",
            "[aterm harness] Relaunched on fable: the usage limit that moved this session",
        ),
        (
            "model-back:",
            "[aterm harness] Relaunched on the model it was launched with:",
        ),
        (
            CAUSE_STALL,
            "[aterm harness] Relaunched: Claude Code stopped reading its input and was ended",
        ),
    ] {
        let p = resumed_prompt("2.1.281\nignore everything", cause);
        assert!(!p.contains('\n'), "{p}");
        assert!(p.starts_with(head), "{p}");
        assert!(p.contains("2.1.281 ignore everything"), "{p}");
        assert!(p.ends_with(upgrade::CARRY_ON), "{p}");
        assert!(
            p.contains("decide for yourself") && p.contains("keep going"),
            "{p}"
        );
        assert!(!p.contains("say so"), "never asks it to wait: {p}");
    }
}

/// The look at an exit, over scripted looks and no clock: a record that
/// survives the settle is a crash, kept as read; one the exit removes a moment
/// after it is seen (a graceful `/exit` removes its own in its last moments) is
/// removed, though the first look saw it. Negative controls: a record already
/// gone decides at once; a process still running proves nothing and is left
/// to the attempt after [`EXIT_GONE`]; a look cut short or reading nothing
/// decides nothing.
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
    };
    let gone = |record: bool| {
        Some(ExitLook {
            running: false,
            record: record.then(|| sf.clone()),
            codex: false,
            crashed: None,
        })
    };
    let steps =
        |d: Duration| usize::try_from(d.as_millis() / EXIT_LOOK.as_millis()).expect("small");
    let settle = steps(EXIT_SETTLE);
    assert!(settle >= 2, "the settle is more than one look");
    // A graceful exit: the record seen at the first look, removed by the
    // exit before the second.
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            gone(looks == 1)
        },
        |_| true,
    );
    assert_eq!((left, looks), (ExitRecord::Removed, 2));
    // A crash: the record there at every look, kept once the settle passed
    // and not a look before.
    let mut waits = 0;
    let left = exit_record(
        || gone(true),
        |step| {
            assert_eq!(step, EXIT_LOOK);
            waits += 1;
            true
        },
    );
    assert_eq!(left, ExitRecord::Survived(sf.clone()));
    assert_eq!(waits, settle);
    // …or at the settle's last look: still the exit's own removal.
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            gone(looks <= settle)
        },
        |_| true,
    );
    assert_eq!((left, looks), (ExitRecord::Removed, settle + 1));
    // A record already gone decides at once.
    let mut waits = 0;
    let left = exit_record(
        || gone(false),
        |_| {
            waits += 1;
            true
        },
    );
    assert_eq!((left, waits), (ExitRecord::Removed, 0));
    // The process still runs (a job stopped, a zombie not yet reaped): its
    // record proves nothing yet, and the attempt reads it.
    let mut waits = 0;
    let left = exit_record(
        || {
            Some(ExitLook {
                running: true,
                record: Some(sf.clone()),
                codex: false,
                crashed: None,
            })
        },
        |_| {
            waits += 1;
            true
        },
    );
    assert_eq!((left, waits), (ExitRecord::Unread, steps(EXIT_GONE)));
    // A process that ends during the look: the settle counts from the
    // first look, not from its end.
    let mut looks = 0;
    let left = exit_record(
        || {
            looks += 1;
            Some(ExitLook {
                running: looks <= 3,
                record: Some(sf.clone()),
                codex: false,
                crashed: None,
            })
        },
        |_| true,
    );
    assert_eq!(
        (left, looks),
        (ExitRecord::Survived(sf.clone()), settle + 1)
    );
    // Cut short, or nothing readable: nothing decided.
    assert_eq!(exit_record(|| gone(true), |_| false), ExitRecord::Unread);
    assert_eq!(exit_record(|| None, |_| true), ExitRecord::Unread);
    assert_eq!(
        [
            ExitRecord::Unread.word(),
            ExitRecord::Survived(sf).word(),
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
    // Negative control: the launch's flags as they were (today's relaunch).
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
    // A launch without bypass never gains it; a named mode is replaced.
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

/// A Codex look is decided on its shell's word: a crash is
/// [`ExitRecord::Crashed`], its own exit or someone's signal
/// [`ExitRecord::Removed`], at once; with no word yet it is looked at again up
/// to [`EXIT_GONE`] (the shell writes it as it takes the terminal back), then
/// left [`ExitRecord::Unread`], never read as a removal. Negative control: the
/// same silence from a Claude Code look (no record) is a removal at once.
#[test]
fn a_codex_exit_is_decided_on_its_shells_word() {
    let look = |crashed: Option<bool>| {
        Some(ExitLook {
            running: false,
            record: None,
            codex: true,
            crashed,
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
    // The word comes on the third look.
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
    // Negative control: a Claude Code look with no record is a removal.
    let claude = || {
        Some(ExitLook {
            running: false,
            record: None,
            codex: false,
            crashed: None,
        })
    };
    assert_eq!(exit_record(claude, |_| true), ExitRecord::Removed);
}

/// A quiet restart's outcome names what is known, never "unconfirmed" (nothing
/// is asked of the resumed session, so no answer names the model after): the
/// build, that it was idle and nothing was typed, the model it ran before and,
/// under the model rule, the model the relaunch asked for. Negative control:
/// the ask flow's line, where a confirm step runs, still says `unconfirmed`
/// before its answer.
#[test]
fn a_quiet_restarts_outcome_names_the_model_it_ran_never_unconfirmed() {
    let st = |before: &str, list: &str| St {
        model_before: before.to_string(),
        model_list: list.to_string(),
        ..St::default()
    };
    assert_eq!(
        quiet_outcome(&st("claude-opus-5-5", ""), "2.1.282"),
        "claude restarted on 2.1.282 · resumed while idle, nothing typed; it ran \
         claude-opus-5-5 before the restart"
    );
    assert_eq!(
        quiet_outcome(&st("claude-opus-5-5", "claude-opus-5-6"), "2.1.282"),
        "claude restarted on 2.1.282 · resumed while idle, nothing typed; it ran \
         claude-opus-5-5 before, and the relaunch asked for claude-opus-5-6"
    );
    assert_eq!(
        quiet_outcome(&st("", ""), "2.1.282"),
        "claude restarted on 2.1.282 · resumed while idle, nothing typed"
    );
    for (before, list) in [("claude-opus-5-5", ""), ("", "claude-opus-5-6"), ("", "")] {
        let line = quiet_outcome(&st(before, list), "2.1.282");
        assert!(!line.contains("unconfirmed"), "{line}");
    }
    assert!(outcome_line(&st("claude-opus-5-5", ""), "2.1.282", None).contains("unconfirmed"));
}

// ------------------------------------------------------------------------
// The login a relaunch line carries
// (`docs/DESIGN-claude-accounts-2026-10-01.md` §6, "The move"). Labels are
// the design's (`main`, `work-2`, `pool-a`); directories are invented.
// ------------------------------------------------------------------------

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

/// THE LOGIN IS ON THE LINE, FIRST, QUOTED AS THE SHELL NEEDS IT — in front
/// of the survey switch and the program, inside the subshell's `cd`, behind
/// fish's `env` — and the EMPTY value (the default login, named) is written
/// `''`, never dropped: unset and empty are two relaunch lines. NEGATIVE
/// CONTROL: nothing required, the line is byte for byte what it was.
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
    // The default login, NAMED: set and empty.
    assert_eq!(login_env(Some("")), [(accounts::LOGIN_ENV, "")]);
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(Some(""))).as_deref(),
        Ok(format!(" CLAUDE_SECURESTORAGE_CONFIG_DIR='' {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &login_env(Some(""))).as_deref(),
        Ok(format!(" env CLAUDE_SECURESTORAGE_CONFIG_DIR='' {survey} {tail}").as_str())
    );
    // NEGATIVE CONTROL: the process had no such variable — nothing is added.
    assert!(login_env(None).is_empty());
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(None)).as_deref(),
        Ok(format!(" {survey} {tail}").as_str())
    );
    // A control character in the value refuses the line, never a cut value.
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login_env(Some("/tmp/a\nb"))),
        Err("line".to_string())
    );
}

/// THE CLAUDE HOME IS ON THE LINE TOO ([`required_env`]): a process started
/// with `CLAUDE_CONFIG_DIR` (on its own command line, or by a wrapper: the
/// tab's shell never had it) is relaunched with exactly that value, ahead of
/// the login and the survey switch, in every shell's form — dropped, the new
/// process reads another `.claude.json` (measured 2026-10-09 in the
/// real-claude lab: the first-run theme picker, the conversation never held)
/// and, with no login variable, signs in by another keychain item. A home no
/// line can hold refuses the line, and no relaunch is promised for it.
/// NEGATIVE CONTROL: no home, the line is byte for byte what it was.
#[test]
fn the_relaunch_line_carries_the_claude_home_its_agent_ran_under() {
    const HOME: &str = "/tmp/lab home/.claude";
    let tail = format!("'/opt/claude' '--verbose' '--resume' '{LOGIN_ID}'");
    let survey = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    let argv = ["claude", "--verbose"];
    let home = r"CLAUDE_CONFIG_DIR='/tmp/lab home/.claude'";
    assert_eq!(
        required_env(Some(HOME), None),
        [(accounts::HOME_ENV, HOME)],
        "the home alone"
    );
    for dialect in [Dialect::Zsh, Dialect::Bash] {
        assert_eq!(
            login_line(dialect, None, &argv, &required_env(Some(HOME), None)).as_deref(),
            Ok(format!(" {home} {survey} {tail}").as_str()),
            "{dialect:?}"
        );
    }
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &required_env(Some(HOME), None)).as_deref(),
        Ok(format!(" env {home} {survey} {tail}").as_str())
    );
    // The home first, then the login: both carried.
    let posix = r"CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner'\''s work-2'";
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(Some(HOME), Some(LOGIN_DIR))
        )
        .as_deref(),
        Ok(format!(" {home} {posix} {survey} {tail}").as_str())
    );
    // NEGATIVE CONTROL: no home — the login's line, and with neither the
    // line as it always was.
    assert_eq!(
        required_env(None, Some(LOGIN_DIR)),
        login_env(Some(LOGIN_DIR))
    );
    assert!(required_env(None, None).is_empty());
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &required_env(None, None)).as_deref(),
        Ok(format!(" {survey} {tail}").as_str())
    );
    // Whether an exit is promised a relaunch asks the same line, home and all.
    let snap = |home: Option<String>| Snapshot {
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--verbose"]),
        session: Some(LOGIN_ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.295".to_string()),
        codex: None,
        dialect: Some(Dialect::Zsh),
        login: None,
        home,
    };
    assert!(resumes_on_exit(&snap(None)));
    assert!(resumes_on_exit(&snap(Some(HOME.to_string()))));
    assert!(!resumes_on_exit(&snap(Some(format!(
        "/tmp/{}",
        "d".repeat(5000)
    )))));
    // A control character in the value refuses the line, never a cut value.
    assert_eq!(
        login_line(
            Dialect::Zsh,
            None,
            &argv,
            &required_env(Some("/tmp/a\nb"), None)
        ),
        Err("line".to_string())
    );
}

/// THE SURVEY SWITCH IS DROPPED BEFORE THE LOGIN IS, AND THE LOGIN NEVER: a
/// line that fits only without the switch is typed with the login alone, and
/// one that cannot hold the login is REFUSED (`refused:line`) — never typed
/// without it, which would resume the conversation under whatever login the
/// shell has. NEGATIVE CONTROL: the same two launches with nothing required
/// are the line and its fallback as they always were — the second is typed
/// with no environment at all.
#[test]
fn a_line_that_cannot_hold_the_login_is_refused_never_typed_without_it() {
    let login = login_env(Some(LOGIN_DIR));
    // A launch whose `--model` value pads the line to a chosen length.
    let padded = |pad: usize| vec!["claude".to_string(), "--model".to_string(), "m".repeat(pad)];
    let with = |pad: usize, required: &[(&str, &str)]| {
        let argv = padded(pad);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        login_line(Dialect::Zsh, None, &argv, required)
    };
    let has = |line: &str, name: &str| line.contains(&format!("{name}="));
    let (login_var, survey_var) = (accounts::LOGIN_ENV, SURVEY_SWITCH_VAR);
    // The longest launch whose line still holds the login AND the switch,
    // found against this kernel's own bound.
    let both = (1..5000)
        .take_while(|&pad| with(pad, &login).is_ok_and(|l| has(&l, survey_var)))
        .last()
        .expect("a short line holds both");
    assert!(with(both, &login).is_ok_and(|l| has(&l, login_var) && has(&l, survey_var)));
    // One byte longer: the switch goes, the login stays.
    let line = with(both + 1, &login).expect("the login alone still fits");
    assert!(has(&line, login_var) && !has(&line, survey_var), "{line}");
    // Longer still, the login no longer fits: refused, not typed without it.
    let lost = (both + 1..5000)
        .find(|&pad| with(pad, &login).is_err())
        .expect("a line too long for the login");
    assert_eq!(with(lost, &login), Err("line".to_string()));
    // NEGATIVE CONTROL: with nothing required that very launch is typed, as
    // it always was — which is exactly what must not happen to a login.
    let plain = with(lost, &[]).expect("the launch itself fits");
    assert!(!has(&plain, login_var), "{plain}");
    // …and the launch whose switch was dropped is typed with no environment.
    let line = with(lost + 60, &[]).expect("fits with no environment");
    assert!(!has(&line, survey_var), "{line}");
}

/// WHETHER AN EXIT WOULD BE RELAUNCHED is asked of the line the relaunch
/// would really type — with the login the process runs under: a snapshot
/// whose login no line can carry promises no relaunch. NEGATIVE CONTROL: the
/// same launch under a login that fits, and under none, is relaunched.
#[test]
fn an_exit_is_promised_a_relaunch_only_where_the_line_can_hold_its_login() {
    let snap = |login: Option<String>| Snapshot {
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude", "--verbose"]),
        session: Some(LOGIN_ID.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.286".to_string()),
        codex: None,
        dialect: Some(Dialect::Zsh),
        login,
        home: None,
    };
    assert!(resumes_on_exit(&snap(None)));
    assert!(resumes_on_exit(&snap(Some(String::new()))));
    assert!(resumes_on_exit(&snap(Some(LOGIN_DIR.to_string()))));
    assert!(!resumes_on_exit(&snap(Some(format!(
        "/tmp/{}",
        "d".repeat(5000)
    )))));
}

/// THE ROSTER NAMES THE LOGIN, AND THE PROCESS'S OWN ENVIRONMENT SAYS WHICH
/// ACCOUNT IT RUNS UNDER: a label's login is its row's directory verbatim —
/// the empty value for the row with none — and a label the roster does not
/// list has none. A process is on an account when its own variable names
/// that row, byte for byte: unset and empty are both the row with no
/// directory. NEGATIVE CONTROLS: another row's directory, a directory with a
/// trailing slash (another login to the vendor), and a roster with no such
/// row.
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
        tab: "s-1".to_string(),
        pid: 4242,
        start: String::new(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(&["/opt/claude"]),
        session: None,
        cwd: String::new(),
        version: None,
        codex: None,
        dialect: None,
        login: login.map(str::to_string),
        home: None,
    };
    assert!(runs_under(&on(None), &roster, "main"));
    assert!(runs_under(&on(Some("")), &roster, "main"));
    assert!(runs_under(&on(Some(LOGIN_DIR)), &roster, "work-2"));
    // NEGATIVE CONTROLS.
    assert!(!runs_under(&on(Some(LOGIN_DIR)), &roster, "main"));
    assert!(!runs_under(&on(None), &roster, "work-2"));
    assert!(!runs_under(
        &on(Some(&format!("{LOGIN_DIR}/"))),
        &roster,
        "work-2"
    ));
    assert!(!runs_under(&on(None), &accounts::Roster::default(), "main"));
}

/// AN ACCOUNT MOVE'S RECORD: its cause is `account:<label>` — in BOTH
/// allowlists, so the harness's own SIGTERM is never read as a graceful exit
/// and the continuation is a relaunch's, never the upgrade's — and its model
/// rides as the memory banner's does (what rides, else a hand choice), or,
/// where a bucket's fallback stands, is the way back, the origin spent: the
/// fallback answered the OLD account's bucket. NEGATIVE CONTROLS: a cause
/// that only begins like it, and the upgrade's own (empty) cause.
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
    assert!(!starts_afresh(&plain.cause) && !ends_at_once(&plain.cause));
    // A hand choice, and a model that rides, are asked for as at a memory
    // restart.
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
    // A fallback stands: the move is the way back.
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
    // The memory banner's restart of the same record keeps the fallback: the
    // difference is the account move's alone.
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
    // NEGATIVE CONTROLS.
    for cause in ["account", "accounts", "", CAUSE_FRESH] {
        assert!(!relaunch_cause(cause), "{cause:?}");
    }
    assert!(!ends_its_agent("account"));
    assert!(
        !ends_its_agent(CAUSE_EXIT),
        "an exit's relaunch ended nothing"
    );
}

/// WHAT A SESSION MOVED TO ANOTHER ACCOUNT IS TOLD: that it was relaunched
/// under another Claude account — the LABEL, nothing else of it — and to
/// carry on. Nothing about limits (it cannot know them, and the move may
/// have been made ahead of one or on the owner's word), no directory, one
/// line whatever the label holds.
#[test]
fn a_session_moved_to_another_account_is_told_the_label_and_to_carry_on() {
    let p = resumed_prompt("2.1.286", "account:work-2");
    assert_eq!(
        p,
        format!(
            "{} Relaunched under another Claude account (work-2): this session was ended and \
             restarted on Claude Code 2.1.286 and resumed. {}",
            upgrade::HARNESS_MARK,
            upgrade::CARRY_ON
        )
    );
    for word in [
        "limit",
        "usage",
        "reset",
        "/tmp",
        LOGIN_DIR,
        accounts::LOGIN_ENV,
    ] {
        assert!(
            !p.to_lowercase().contains(&word.to_lowercase()),
            "{word}: {p}"
        );
    }
    assert!(!resumed_prompt("2.1.286", "account:work\n2").contains('\n'));
    // NEGATIVE CONTROL: without the cause in the allowlist's branch it would
    // read as an exit nobody asked for.
    assert!(resumed_prompt("2.1.286", CAUSE_EXIT).contains("exited without anyone asking"));
    assert!(!p.contains("exited without anyone asking"));
}

/// WHAT A SHELL WOULD EXPAND IN A LOGIN DIRECTORY IS TYPED LITERALLY, AND A
/// LINE THAT CANNOT BE WRITTEN WITH THE LOGIN IS NEVER WRITTEN WITHOUT IT
/// (the review of 2026-10-01). The vendor hashes the directory's bytes into
/// its keychain item's name, so `$HOME` expanded by the shell, a backtick
/// run, or a `;` that ended the word would each be another login — or a
/// command. Every such value rides inside single quotes, under each shell.
/// And fish has no subshell form for an agent whose directory differs from
/// its shell's: with a login required that line is REFUSED on both of
/// [`line_for`]'s tries. NEGATIVE CONTROLS: a value of plain path characters
/// needs no quotes; the same fish launch with no `cd` holds the login; and
/// no control character is ever on a line, a tab and a carriage return
/// included.
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
    // A backslash is literal inside a POSIX shell's quotes, and doubled
    // inside fish's.
    let login = login_env(Some(r"/tmp/a\b"));
    assert_eq!(
        login_line(Dialect::Zsh, None, &argv, &login).as_deref(),
        Ok(format!(r" {name}='/tmp/a\b' {survey} {tail}").as_str())
    );
    assert_eq!(
        login_line(Dialect::Fish, None, &argv, &login).as_deref(),
        Ok(format!(r" env {name}='/tmp/a\\b' {survey} {tail}").as_str())
    );
    // NEGATIVE CONTROL: plain path characters need no quotes.
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
    // Fish, the agent's directory not its shell's: refused, with the login
    // or with none — never typed bare because the login could not ride.
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
    // No control character is ever on the line.
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

/// AN ACCOUNT MOVE ASKED OVER A RESTART OF ANOTHER KIND IN FLIGHT WAS NOT
/// TRIED ([`carried_word`], [`IN_FLIGHT_OTHER`]): the conversation's record
/// was carried on its own line, under the login its process had, so every
/// word of that carry that is no failure — `adopted` included — is answered
/// `wait:restart-in-flight`: a wait, asked again, with nothing held against
/// the account. So is a move to ANOTHER label than the one in flight.
/// NEGATIVE CONTROLS: the move's OWN record carried answers the carry's
/// word; a restart of another kind answers the carry's word whatever the
/// record's cause; and a carry that FAILED is said as it failed.
#[test]
fn an_account_move_over_another_restart_in_flight_is_not_tried() {
    let to = |label: &str| Restart::Account {
        to: label.to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Wall,
    };
    assert_eq!(IN_FLIGHT_OTHER, "wait:restart-in-flight");
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
        // The carry's own failure is the tab's word.
        assert_eq!(
            word(&to("work-2"), cause, "failed:relaunch:ERR"),
            "failed:relaunch:ERR",
            "{cause:?}"
        );
    }
    // NEGATIVE CONTROLS: the move's own record, and every other restart.
    for step in ["adopted", "wait:resume", "failed:no-resume"] {
        assert_eq!(word(&to("work-2"), "account:work-2", step), step);
        for cause in ["", CAUSE_MEMORY, "account:work-2"] {
            assert_eq!(word(&Restart::Memory, cause, step), step, "{cause:?}");
        }
    }
    // A label that only begins like the one in flight is another label.
    assert_eq!(
        word(&to("work"), "account:work-2", "adopted"),
        IN_FLIGHT_OTHER
    );
}

/// A RELAUNCH WITH NO PROCESS TO READ CARRIES THE MACHINE'S ACTIVE ACCOUNT
/// ([`cold_login`]): where two or more accounts are enrolled and `active` is
/// one of them and PROVEN, the login its roster row names — the empty value
/// for the row with no directory, the default login named. NEGATIVE
/// CONTROLS, each of which adds NOTHING (the relaunch as it was before
/// accounts existed): no roster; one account listed; two listed and one
/// enrolled; no `active`; an `active` not proven; one the roster no longer
/// lists, or not enrolled here; one known signed out; and a leaf whose
/// Claude home is not the default.
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
    // The row with no directory is enrolled whatever the set says.
    let only_dirs: BTreeSet<String> = ["work-2".to_string()].into();
    assert_eq!(
        cold_login(&roster, &on(Some("main"), true), &only_dirs, true).as_deref(),
        Some("")
    );

    // NEGATIVE CONTROLS.
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
    // (The word ended nothing here: `active` and its proof are set by hand.)
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
