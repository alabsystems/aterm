// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// The program a relaunch runs is what the process RUNS, not the word it was
/// typed as: a `#!` script (the managed twin, an npm install, a stand-in)
/// shows its interpreter as the image and itself as `argv[1]`; a binary is
/// the kernel's image. NEGATIVE CONTROLS: a wrapper that runs Claude as a
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
    // NEGATIVE CONTROLS.
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

/// THE DECISION, whose exit it was: a person's (a keystroke within the
/// grace), a holder's (`hold=1`, a lease, a named driver's turn), else the
/// owner's limit, else nobody's — relaunched. A person or a holder is asked
/// about FIRST, so their own exit is never said to them as a limit.
/// NEGATIVE CONTROLS: a keystroke older than the grace, `-`, a server that
/// does not say, an unreadable status, an unnamed turn (this host's own
/// loop types so) and a `driving:` hand are nobody.
#[test]
fn an_exit_is_relaunched_unless_someone_owns_it_or_it_is_limited() {
    let st = |tail: &str| format!("OK schema=1 {tail}");
    let nobody = st("hold=0 hand=- human_ms=-");
    assert_eq!(on_exit(true, Some(&nobody), 120, false), OnExit::Relaunch);
    assert_eq!(
        on_exit(true, None, 120, false),
        OnExit::Relaunch,
        "unreadable"
    );
    let typed = |ms: u64| st(&format!("hold=0 hand=- human_ms={ms}"));
    assert_eq!(
        on_exit(true, Some(&typed(119_999)), 120, false),
        OnExit::PersonAsked
    );
    assert_eq!(
        on_exit(true, Some(&typed(120_000)), 120, false),
        OnExit::Relaunch
    );
    assert_eq!(
        on_exit(true, Some(&typed(0)), 0, false),
        OnExit::Relaunch,
        "no grace"
    );
    for held in [
        "hold=1 hand=- human_ms=-",
        "hold=0 hand=lease:orchestrator human_ms=-",
        "hold=0 hand=turn:41:s-9f00 human_ms=-",
    ] {
        assert_eq!(
            on_exit(true, Some(&st(held)), 120, false),
            OnExit::Held,
            "{held}"
        );
        assert_eq!(
            on_exit(false, Some(&st(held)), 120, false),
            OnExit::Held,
            "{held}"
        );
    }
    for free in [
        "hold=0 hand=turn:41 human_ms=-",
        "hold=0 hand=driving:s-2 human_ms=-",
    ] {
        assert_eq!(
            on_exit(true, Some(&st(free)), 120, false),
            OnExit::Relaunch,
            "{free}"
        );
    }
    assert_eq!(on_exit(false, Some(&nobody), 120, false), OnExit::Limited);
    assert_eq!(on_exit(false, None, 120, false), OnExit::Limited);
    assert_eq!(
        on_exit(false, Some(&typed(1)), 120, false),
        OnExit::PersonAsked,
        "a person's exit is theirs, never said as a limit"
    );
    // U1: an agent that had stopped reading its input — its stall held —
    // exited by the stall's remedy: a keystroke just before was read by
    // nothing, so it is no person's exit. A holder still owns it, and the
    // owner's limit still holds.
    assert_eq!(on_exit(true, Some(&typed(1)), 120, true), OnExit::Relaunch);
    assert_eq!(
        on_exit(true, Some(&st("hold=1 hand=- human_ms=1")), 120, true),
        OnExit::Held
    );
    assert_eq!(on_exit(false, Some(&typed(1)), 120, true), OnExit::Limited);
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

/// One step's word, read for the host: relaunched; LEFT — the launch's own
/// end (a one-shot run, a launch that never registered a conversation) or
/// no exit at all — journaled and never said; never (an interactive
/// conversation that cannot come back) — said. NEGATIVE CONTROL: every wait
/// and every failure that a later attempt can get past is NOT the
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

/// ANOTHER ACTOR ON THE LOCK is no miss: an upgrade step in another tab can
/// hold it for minutes, and a crash here meanwhile must not read as a
/// relaunch that keeps failing. However many busy looks in a row, the
/// back-off, the misses and the attention stay where they were.
/// NEGATIVE CONTROL: the same number of real misses is said.
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

/// A ONE-SHOT RUN is read wherever its flag stands — after a prompt, after a
/// flag that takes a value, after a flag the table does not know — and its
/// exit is its own end. NEGATIVE CONTROLS: an interactive launch; a `-p`
/// that is another flag's VALUE (`--append-system-prompt -p`); and one after
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

/// FOLLOWING the snapshot's agent: Claude's own record of the SAME process
/// (pid and kernel start) moves its conversation, build and directory — an
/// in-app `/clear` names a new conversation under the same pid. The tab's
/// published foreground group says whether the snapshot still describes the
/// tab. NEGATIVE CONTROLS: a record of another start (a reused pid) and a
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
    // NEGATIVE CONTROLS.
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

/// THE RELAUNCH'S BUILD obeys `[harness] upgrade`: on, a crashed session
/// comes back on the newest build it may move to; OFF, on the program it
/// ran, whatever is installed — configuration only takes power away.
/// NEGATIVE CONTROLS: nothing newer, and a running version that cannot be
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
        relaunch_target(on, running, c, ran, Some("2.1.281"))
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

/// THE BACK-OFF grows 1 s, 10 s, 60 s, then ten minutes for as long as the
/// agent keeps exiting — never a give-up — and starts over once a relaunched
/// agent ran [`HEALTHY`]ly. Misses in a row are said ONCE after
/// [`BADGE_AFTER`] and cleared by a relaunch that lands. NEGATIVE CONTROLS: a
/// crash soon after a relaunch does NOT start the back-off over, and a
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
    // Misses: said ONCE after BADGE_AFTER in a row, cleared by a landing.
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
/// action of `HarnessRelaunchOnExit` and check, after each, that the state
/// they produce is the model's after the same action, every invariant
/// holding, and every action of the model is driven. NEGATIVE CONTROLS: the
/// model's buggy host relaunches after a person's exit, drops a relaunch it
/// cannot make without a word and keeps a limit quiet, and each is caught
/// by an invariant, so the check is not vacuous.
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
        ("Limited", "exit"),
        ("PersonRuns", ""),
        ("Crash", "exit"),
        ("Limited", "pause"),
        ("PersonRuns", ""),
    ];
    for (action, input) in schedule {
        match (action, input) {
            ("Crash", _) => {
                running = false;
                assert!(
                    r.exited(on_exit(true, Some(&nobody), 120, false), now)
                        .is_ok()
                );
            }
            ("PersonExit", who) => {
                running = false;
                let who = if who == "person" { &person } else { &halted };
                assert_eq!(
                    r.exited(on_exit(true, Some(who), 120, false), now),
                    Err(Say::Nothing)
                );
            }
            ("PersonReturns", who) => {
                let who = if who == "person" { &person } else { &leased };
                assert_eq!(
                    r.decide(on_exit(true, Some(who), 120, false)),
                    Some(Say::Nothing)
                );
            }
            ("Limited", "exit") => {
                running = false;
                assert_eq!(
                    r.exited(on_exit(false, Some(&nobody), 120, false), now),
                    Err(Say::Limited)
                );
            }
            ("Limited", _) => {
                assert_eq!(
                    r.decide(on_exit(false, Some(&nobody), 120, false)),
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
    }
    // Every action of the model was driven.
    for a in &model.actions {
        assert!(
            schedule.iter().any(|(name, _)| *name == a.name),
            "{} is never driven",
            a.name
        );
    }
    // NEGATIVE CONTROLS: the buggy host's defects, each caught.
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
}

/// The continuation a relaunched agent is typed is one line, whatever the
/// version text holds, and it never invites the agent to stop and wait for
/// a person: it decides for itself and keeps going, as the upgrade's does.
/// It says why it was relaunched: an exit nobody asked for, or the host's
/// restart for the memory banner (D3).
#[test]
fn the_resumed_prompt_is_one_line_and_keeps_going() {
    for (cause, head) in [
        (CAUSE_EXIT, "[aterm harness] Relaunched: Claude Code exited"),
        (
            CAUSE_MEMORY,
            "[aterm harness] Restarted: Claude Code reported its memory critical",
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

/// THE LOOK AT AN EXIT (D2 of the 2026-09-26 live test), over scripted looks
/// and no clock: a record that survives the settle is a crash, kept as read;
/// one the exit removes a moment after it is seen — a graceful `/exit`
/// removes its own in its last moments — is removed, though the first look
/// saw it. NEGATIVE CONTROLS: a record already gone decides at once; a
/// process that still runs proves nothing and is left to the attempt after
/// [`EXIT_GONE`]; a look cut short or one that reads nothing decides
/// nothing.
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

/// A relaunch never moves a session INTO bypass, and resumes it in the mode
/// its person left it in, ALWAYS named — the default included, so neither a
/// `defaultMode` setting nor a build's auto-mode default picks another. The
/// case measured on this repo's own session (2026-09-26): launched with
/// --dangerously-skip-permissions, left in auto, relaunched — auto, with
/// bypass still in the cycle.
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

/// The whole line, from the RAW launch argv: the mode rides the flags
/// `rewrite_argv` parsed, so neither a launch prompt behind a many-valued
/// flag nor a `--` can swallow or drop it (review 2026-09-27: the raw-argv
/// rewrite turned `"fix the parser"` into a second `--add-dir`, and lost the
/// mode behind `--`).
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
        )
        .unwrap_or_else(|_| panic!("a line for {argv:?}"))
    };
    let want = format!(
        " '/opt/claude' '--add-dir' '../lib' '--allow-dangerously-skip-permissions' \
         '--permission-mode' 'auto' '--resume' '{id}'"
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
