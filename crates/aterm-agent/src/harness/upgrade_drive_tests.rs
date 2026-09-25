// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// `@s-b5cf… text --json trim tail=3`, measured 2026-09-23 on a busy Claude
/// Code 2.1.280 tab: the cursor row is ABSOLUTE (57) and `first` is where the
/// reply's rows start, so the caret sits on reply row 0, column 2.
const SCREEN: &str = r#"{"rows":["❯","────────","  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents"],"cursor":{"row":57,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":60,"cols":144},"seq":232803,"trimmed":0,"first":57}"#;

/// `@s-b5cf… status`, measured the same minute.
const STATUS: &str = "OK schema=1 sid=0 subject=%E2%97%90%20Git%20commit%20and%20push \
    subject_source=osc observed=true phase=running since_ms=4431314 outcome=none exit_code=- \
    signal=- detail=- confidence=strong reasons=fg_job,content_activity attribution=adopted \
    fs_consent=unknown conflict=false revision=171 enabled=true hold=0 fabric=absent \
    fabric_rtt_ms=- fabric_link_age_ms=- identity=- hand=- level=quiet story=0 seq=232803 \
    hash=7adef2d5e8547a9e";

#[test]
fn a_screen_reply_is_read_with_the_cursor_made_relative_to_its_rows() {
    let s = parse_screen(SCREEN).expect("the measured reply parses");
    assert_eq!(s.rows.len(), 3);
    assert_eq!(s.cursor, Some((0, 2)));
    assert_eq!(s.seq, 232_803);
    assert_eq!(s.first, 57, "the screen row `cell` addresses rows[0] by");
    assert!(upgrade::composer_is_empty(&s.rows, s.cursor, false));
    // A cursor ABOVE the reply's first row is no cursor, not row 0.
    let above = SCREEN.replace(r#""row":57"#, r#""row":12"#);
    assert_eq!(parse_screen(&above).expect("parses").cursor, None);
    assert!(parse_screen("not json").is_none());
}

#[test]
fn a_tab_is_held_by_a_halt_a_hand_or_an_unreadable_status() {
    assert!(!held_by_status(STATUS));
    assert!(held_by_status(&STATUS.replace("hold=0", "hold=1")));
    assert!(held_by_status(&STATUS.replace("hand=-", "hand=turn:7")));
    assert!(held_by_status("ERR no such session"));
    assert!(held_by_status(""));
}

#[test]
fn who_roster_exposes_a_unique_foreground_group_without_a_window_hop() {
    let body = "0 s-a driving=- watchers=0 turns=0 alive nonce=0123456789abcdef0123456789abcdef fgpgid=101\n\
                1 s-b driving=- watchers=1 turns=3 alive nonce=abcdef0123456789abcdef0123456789 fgpgid=-\n";
    let tabs = parse_host_roster("OK 2", body).expect("who roster");
    assert_eq!(tabs[0].fgpgid, Some(101));
    assert_eq!(tabs[1].fgpgid, None);
    assert_eq!(unique_tab_for_group(&tabs, 101), Some("s-a"));
    assert_eq!(parse_host_roster("OK 0", ""), Some(Vec::new()));
    assert_eq!(parse_host_roster("ERR busy", body), None);
    assert_eq!(
        parse_host_roster("OK 2", &body.replace("s-b", "s-a")),
        None,
        "duplicate tab ids are not an ownership roster"
    );
    let ambiguous = [
        tabs[0].clone(),
        LiveTab {
            sid: "s-c".to_string(),
            fgpgid: Some(101),
        },
    ];
    assert_eq!(unique_tab_for_group(&ambiguous, 101), None);
}

#[test]
fn every_phase_survives_the_state_file() {
    let to = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse("2.1.281").expect("version"),
        source: Source::Native,
    };
    let from = Version::parse("2.1.280").expect("version");
    for phase in [
        Phase::Pending,
        Phase::Announced { at_s: 7, asks: 2 },
        Phase::Exiting { at_s: 8 },
        Phase::Relaunched { at_s: 9 },
        Phase::Done,
        Phase::Failed("argv:--print".to_string()),
    ] {
        let st = St {
            phase,
            marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
            pid: 9162,
            notice_pid: 9162,
            notice_start: "Mon Sep 21 12:00:00 2026".to_string(),
            shell: 9001,
            tab: "s-b5cf2faabac5ce5127bd".to_string(),
            line: " . '/h/.aterm/shell.d/00-atpkg.zsh'; rehash; '/x/claude' '--resume' 'id'"
                .to_string(),
            last_seq: 232_803,
            noted: "terminal:tmux".to_string(),
            model_before: "claude-opus-5-5".to_string(),
            mark: 45_092_357,
            launch_model: "claude-sonnet-5".to_string(),
            confirm_by: 1_790_000_120,
            resumed_on: "2.1.282".to_string(),
            resumed_pid: 9163,
            ..St::fresh(&from, &to, 1_790_000_000)
        };
        assert_eq!(St::from_json(&st.to_json()), Some(st));
    }
    assert_eq!(St::from_json(r#"{"phase":"sideways"}"#), None);
}

#[test]
fn background_work_is_the_shells_under_the_agent_at_any_depth() {
    let t: Vec<(u32, u32, String)> = [
        (100, 1, "claude"),
        (200, 100, "zsh"),   // a Bash tool
        (201, 200, "targo"), // what it runs: its shell already counts
        (300, 100, "node"),  // an MCP server: a resume restarts it
        (301, 300, "bash"),  // ...but a shell under it is still work
        (400, 1, "zsh"),     // another tab
        (500, 100, "caffeinate"),
    ]
    .iter()
    .map(|(p, pp, n)| (*p, *pp, (*n).to_string()))
    .collect();
    let mut found = background(100, &t);
    found.sort();
    assert_eq!(found, vec!["bash", "caffeinate", "zsh"]);
    assert!(background(400, &t).is_empty());
}

#[test]
fn a_sweep_never_signals_its_own_ancestry() {
    let t = table();
    assert!(is_our_ancestor(std::process::id(), &t));
    if let Some((parent, _, _)) = ids(std::process::id()) {
        assert!(is_our_ancestor(parent, &t));
    }
    let synthetic = vec![(10, 1, "a".to_string()), (11, 10, "b".to_string())];
    assert!(!is_our_ancestor(11, &synthetic));
}

#[test]
fn host_filters_foreign_groups_before_expensive_process_reads() {
    let file = |pid: u32, kind: &str| SessionFile {
        pid,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: "1.0.0".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: kind.to_string(),
        entrypoint: "cli".to_string(),
    };
    let files = [
        file(1, "interactive"),
        file(2, "interactive"),
        file(3, "print"),
    ];
    let ours = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(101),
    }];
    let mut args_read = Vec::new();
    let mut groups_read = Vec::new();
    let found = host_candidates(
        &files,
        &ours,
        |_| true,
        |pid| {
            args_read.push(pid);
            Some(atpkg::caller_shell::ProcArgs::default()) // macOS omits env.
        },
        |pid| {
            groups_read.push(pid);
            Some(if pid == 1 { 101 } else { 202 })
        },
        |_| Some((10, 101, 101)),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.pid, 1);
    assert_eq!(found[0].2.tab, "s-mine");
    assert_eq!(args_read, [1]);
    assert_eq!(groups_read, [1, 2]);

    let found = host_candidates(
        &files[..1],
        &ours,
        |_| true,
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 202)),
    );
    assert!(found.is_empty(), "a background job does not own the tab");
    let found = host_candidates(
        &files[..1],
        &ours,
        |_| true,
        |_| {
            Some(atpkg::caller_shell::ProcArgs {
                env: vec!["ATERM_PARENT_SESSION_ID=s-other".to_string()],
                ..atpkg::caller_shell::ProcArgs::default()
            })
        },
        |_| Some(101),
        |_| Some((10, 101, 101)),
    );
    assert!(
        found.is_empty(),
        "a readable conflicting env vetoes the claim"
    );
}

/// A normal shell tab should not make the minute host parse every old Claude
/// session. A matching process, an unfamiliar filename, or an unfinished
/// restart still takes the complete scan so the fast path cannot authorize an
/// act or strand a relaunch.
#[test]
fn host_skips_quiet_session_history_but_preserves_candidate_and_orphan_scans() {
    let dir = scratch("host-quiet-history");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let mut child = parked().spawn().expect("session process");
    wait_exec(child.id());
    register(&opts.home, child.id(), SESSION);
    let mut tabs = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(i64::MAX),
    }];
    SESSION_FILE_SCANS.with(|count| count.set(0));
    assert!(sweep_with_roster(&opts, Some(&tabs)).is_empty());
    assert_eq!(SESSION_FILE_SCANS.with(std::cell::Cell::get), 0);

    tabs[0].fgpgid = process_group(child.id());
    assert!(host_may_have_work(&opts, &tabs));
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let _ = sweep_with_roster(&opts, Some(&tabs));
    assert_eq!(SESSION_FILE_SCANS.with(std::cell::Cell::get), 1);

    tabs[0].fgpgid = None;
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let _ = sweep_with_roster(&opts, Some(&tabs));
    assert_eq!(
        SESSION_FILE_SCANS.with(std::cell::Cell::get),
        1,
        "an unknown foreground group cannot prove this tab is quiet"
    );

    tabs[0].fgpgid = Some(i64::MAX);
    let unknown = opts.home.join(".claude/sessions/unknown.json");
    std::fs::write(&unknown, "not a session").expect("unknown filename");
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let reports = sweep_with_roster(&opts, Some(&tabs));
    assert_eq!(SESSION_FILE_SCANS.with(std::cell::Cell::get), 1);
    assert_eq!(reports[0].step, "wait:session-files-unreadable");
    std::fs::remove_file(unknown).expect("remove malformed test entry");

    let state = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: u32::MAX,
        shell: std::process::id(),
        tab: tabs[0].sid.clone(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state directory");
    // The live file above still owns SESSION; an orphan must be a distinct
    // conversation with no current session file.
    let orphan_session = format!("{SESSION}-orphan");
    std::fs::write(state_path(&opts, &orphan_session), state.to_json()).expect("in-flight state");
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let reports = sweep_with_roster(&opts, Some(&tabs));
    assert_eq!(SESSION_FILE_SCANS.with(std::cell::Cell::get), 1);
    assert_eq!(
        reports.len(),
        1,
        "the orphan restart must reach its handler"
    );
    assert_eq!(reports[0].session, orphan_session);
    assert_eq!(reports[0].step, "would-resume:exiting");

    child.kill().expect("stop child");
    child.wait().expect("reap child");
    let _ = std::fs::remove_dir_all(dir);
}

/// The one-minute host tick should pay no `ps` or argv cost for a current
/// Claude, yet a newer native/managed build or an interrupted restart must
/// still reach the ownership proofs before any possible act.
#[test]
fn current_claude_skips_process_inspection_but_new_and_in_flight_do_not() {
    let mut file = SessionFile {
        pid: 101,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: "2.1.281".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
    };
    let target = |version: &str, source| Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse(version).expect("version"),
        source,
    };
    let mut targets = Targets {
        managed: Some(target("2.1.281", Source::Managed)),
        native: Some(target("2.1.280", Source::Native)),
    };
    let ours = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(101),
    }];
    assert!(!needs_upgrade_inspection(&file, &targets, None));
    let current = host_candidates(
        std::slice::from_ref(&file),
        &ours,
        |sf| needs_upgrade_inspection(sf, &targets, None),
        |_| panic!("current build must not read argv"),
        |_| Some(101), // one cheap group lookup identifies this tab
        |_| panic!("current build must not run ps for process ids"),
    );
    assert!(current.is_empty());

    targets.native = Some(target("2.1.282", Source::Native));
    assert!(
        needs_upgrade_inspection(&file, &targets, None),
        "native may be eligible until the executable is checked"
    );
    let newer = host_candidates(
        std::slice::from_ref(&file),
        &ours,
        |sf| needs_upgrade_inspection(sf, &targets, None),
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 101)),
    );
    assert_eq!(newer.len(), 1, "new build keeps the ownership path");

    targets.native = None;
    let restarting = St {
        phase: Phase::Exiting { at_s: 1 },
        ..St::default()
    };
    assert!(needs_upgrade_inspection(&file, &targets, Some(&restarting)));
    file.version = "unknown".to_string();
    assert!(needs_upgrade_inspection(&file, &targets, None));
}

#[test]
fn a_restart_left_between_exit_and_relaunch_is_resumed_by_the_next_sweep() {
    let dir = std::env::temp_dir().join(format!("aterm-upgrade-orphan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("home");
    // A pid that WAS a process and is not one now.
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    let opts = Opts {
        home,
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: true,
    };
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    // Signalled a moment ago, and the shell it ran under still there.
    let at_s = now_s();
    let st = St {
        phase: Phase::Exiting { at_s },
        pid: dead,
        shell: std::process::id(),
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        to: "2.1.281".to_string(),
        source: "native".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].session, session);
    assert_eq!(got[0].step, "would-resume:exiting");
    // A dry run wrote nothing: the phase is where it was.
    assert_eq!(
        load(&opts, session).map(|s| s.phase),
        Some(Phase::Exiting { at_s })
    );
    // Another tab's restart is not this sweep's when it is told one tab.
    let only = Opts {
        only_sid: Some("s-0".to_string()),
        ..opts.clone()
    };
    assert!(sweep(&only).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-upgrade-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

#[test]
fn startup_cadence_keeps_five_short_checks_after_an_early_roster() {
    let model = aterm_spec::derive::harness_upgrade_startup_cadence_model();
    let mut state = model.init_state();
    let mut cadence = HostCadence::new();
    for tick in 1..=8 {
        let delay = cadence.next_delay();
        assert!(model.fire("Step", &mut state));
        assert_eq!(state["tick"], i64::from(cadence.tick));
        assert_eq!(
            state["short"],
            i64::from(delay == HOST_STARTUP_EVERY),
            "shipping cadence and derived model differ at tick {tick}"
        );
        if tick == 1 {
            assert!(model.fire("Ready", &mut state));
        }
    }
    assert_eq!(cadence.next_delay(), HOST_EVERY);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut premature = buggy.init_state();
    assert!(buggy.fire("Step", &mut premature));
    assert!(buggy.fire("Ready", &mut premature));
    assert!(buggy.fire("Step", &mut premature));
    assert!(!buggy.check_invariant("ShortUntilBudget", &premature));
}

#[test]
fn the_host_runs_by_default_and_either_switch_stops_it() {
    let dir = scratch("switch");
    let toml = dir.join("aterm.toml");
    // No aterm.toml: a fresh machine. ON.
    assert!(host_enabled(Some(&toml)));
    assert!(host_enabled(None));
    // The master switch.
    std::fs::write(&toml, "[harness]\nenabled = false\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    std::fs::write(&toml, "[harness]\nenabled = true\n").expect("write");
    assert!(host_enabled(Some(&toml)));
    // The sweep's own switch, in the same table (it left the deleted harness
    // config.toml for aterm.toml's [harness]), in both TOML spellings.
    std::fs::write(&toml, "[harness]\nupgrade = false\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    std::fs::write(&toml, "harness.upgrade = false\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    // Negative control: another table's `upgrade` is not this switch.
    std::fs::write(&toml, "[packages]\nupgrade = false\n").expect("write");
    assert!(host_enabled(Some(&toml)));
    // A value the switch cannot read is not consent (fail closed), whichever
    // switch carries it.
    std::fs::write(&toml, "[harness]\nupgrade = \"no\"\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    std::fs::write(&toml, "[harness]\nenabled = 0\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    // Read with the parser the window loads aterm.toml with: an inline table
    // is the switch, as the window reads it (a line reader read it ON).
    std::fs::write(&toml, "harness = { upgrade = false }\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    // A file that parser refuses starts the window escalate-only, whose sweep
    // is off: so is this reading.
    std::fs::write(&toml, "font_px = \n[harness]\nupgrade = true\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    // A `false` written below another table's header still stops it: a kill
    // switch does not depend on where in the file it was written.
    std::fs::write(&toml, "[theme]\nharness.enabled = false\n").expect("write");
    assert!(!host_enabled(Some(&toml)));
    std::fs::write(&toml, "[theme]\nharness.enabled = true\n").expect("write");
    assert!(host_enabled(Some(&toml)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_sweeper_does_nothing_while_the_first_holds_the_lock() {
    let dir = scratch("lock");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
    };
    let first = sweep_lock(&opts).expect("free").expect("a real run locks");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].step, "busy:another-sweep");
    assert!(!got[0].is_act());
    // Released by `LOCK_UN` (the guard's drop), so the very next sweep runs. A
    // child another test thread spawns at this instant shares the lock's open file
    // description until its exec, and these tests spawn `ps`, `lsof` and stand-in
    // agents throughout: released by the close alone, the lock read held after the
    // drop in 2 runs of 20 before the stand-in tests and most runs after (measured
    // 2026-09-23). `LOCK_UN` strips it from the description itself, copies and all.
    drop(first);
    let after = sweep(&opts);
    assert!(
        after.is_empty(),
        "no sessions, nothing to report: {after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The window's host SAYS, once per change, that its sweeps cannot run —
/// a state directory that cannot hold the lock is not a wait, and until
/// 2026-09-24 the host dropped it as one every minute (audit finding 12).
/// Driven through real sweeps of an unwritable and an unopenable state.
#[test]
fn the_host_says_once_that_its_sweeps_cannot_run() {
    let dir = scratch("host-fault");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    std::fs::create_dir_all(dir.join("state")).expect("state");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
    };
    // `upgrade` is a FILE: the state directory cannot be made.
    std::fs::write(state_dir(&opts), "").expect("a file where the state goes");
    let unwritable = sweep(&opts);
    assert_eq!(unwritable.len(), 1, "{unwritable:?}");
    assert_eq!(unwritable[0].step, "busy:state-unwritable");
    let mut fault = None;
    let said = |swept: Option<&[Report]>, fault: &mut Option<String>| -> Vec<String> {
        host_notes(swept, fault)
            .into_iter()
            .map(|r| r.step.clone())
            .collect()
    };
    assert_eq!(
        said(Some(&unwritable), &mut fault),
        ["busy:state-unwritable"]
    );
    assert!(said(Some(&unwritable), &mut fault).is_empty(), "said once");
    // A tick that swept nothing (the switch off) forgets it: said again.
    assert!(said(None, &mut fault).is_empty());
    assert_eq!(
        said(Some(&unwritable), &mut fault),
        ["busy:state-unwritable"]
    );
    // Another sweeper holding the lock is the lock working: quiet, and a
    // change, so the fault that comes back is said again.
    let contended = [Report {
        step: "busy:another-sweep".to_string(),
        ..unwritable[0].clone()
    }];
    assert!(said(Some(&contended), &mut fault).is_empty());
    assert_eq!(
        said(Some(&unwritable), &mut fault),
        ["busy:state-unwritable"]
    );
    // A DIFFERENT fault is a change: `sweep.lock` is a directory.
    std::fs::remove_file(state_dir(&opts)).expect("rm");
    std::fs::create_dir_all(state_dir(&opts).join("sweep.lock")).expect("a directory lock");
    let unopenable = sweep(&opts);
    assert_eq!(unopenable[0].step, "busy:lock-unopenable");
    assert_eq!(
        said(Some(&unopenable), &mut fault),
        ["busy:lock-unopenable"]
    );
    // CONTROL: acts always pass, waits never, and a sweep that ran clears it.
    let r = |step: &str| Report {
        step: step.to_string(),
        ..unwritable[0].clone()
    };
    let ran = [r("announced:1"), r("wait:not-idle"), r("current")];
    assert_eq!(said(Some(&ran), &mut fault), ["announced:1"]);
    assert_eq!(
        said(Some(&unopenable), &mut fault),
        ["busy:lock-unopenable"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_that_cannot_be_carried_is_refused_before_the_agent_is_asked_to_wind_down() {
    let dir = scratch("preflight");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
    };
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let launch = |extra: &[&str]| -> Vec<String> {
        ["claude", "--dangerously-skip-permissions"]
            .iter()
            .chain(extra)
            .map(|w| (*w).to_string())
            .collect()
    };
    // The plan `visit` hands the announcement, from the same builder `restart`
    // uses (`plan` reads the shell, the heal and the directory; this is the
    // rest of it).
    let planned = |argv: &[String]| {
        line_for(Dialect::Zsh, Some(hook), None, exe, argv, session)
            .map(|line| Plan { shell: 1, line })
    };
    let report = || Report {
        pid: 9162,
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        session: session.to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: String::new(),
    };
    let mut typed: Vec<u32> = Vec::new();
    let mut send = |asks: u32| -> Result<String, String> {
        typed.push(asks);
        Ok("ATERM-UPGRADE-READY-0badf00d".to_string())
    };

    // A launch whose relaunch line the tty could cut (a long inline system
    // prompt): refused at the announcement, the notice never typed. It used
    // to be refused only at the signal — after the agent had been told to
    // wind down and had answered READY.
    let prompt = "p".repeat(upgrade::MAX_LINE);
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--append-system-prompt", &prompt])),
        100,
        &mut send,
    );
    assert_eq!(got.step, "refused:line");
    assert!(got.is_act(), "a refusal is reported, not a wait");
    assert!(
        matches!(&st.phase, Phase::Failed(w) if w.starts_with("line:")),
        "{:?}",
        st.phase
    );
    // ...and for good: a failed upgrade is never announced to this target.
    assert_eq!(
        upgrade::next_step(&st.phase, &Facts::default(), true, 200),
        Step::Wait("failed")
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        ledger.contains(r#""step":"refused:line""#) && ledger.contains("the line would be"),
        "{ledger}"
    );

    // A flag the rewrite cannot carry: the same, before the notice.
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--brand-new-flag"])),
        100,
        &mut send,
    );
    assert_eq!(got.step, "refused:unknown-flag --brand-new-flag");
    assert_eq!(
        st.phase,
        Phase::Failed("argv:unknown-flag --brand-new-flag".to_string())
    );

    // A fact this sweep could not read: a wait, nothing typed, nothing failed.
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        Err(NoPlan::Wait("shell-dialect")),
        100,
        &mut send,
    );
    assert_eq!(got.step, "wait:shell-dialect");
    assert_eq!(st.phase, Phase::Pending);

    // A dry run says what it would refuse, and records nothing.
    let dry = Opts {
        dry_run: true,
        state: dir.join("dry-state"),
        ..opts.clone()
    };
    let got = announce(
        &dry,
        report(),
        &mut st,
        planned(&launch(&["--append-system-prompt", &prompt])),
        100,
        &mut send,
    );
    assert_eq!(got.step, "would-refuse:line");
    assert!(!got.is_act());
    assert_eq!(st.phase, Phase::Pending);
    assert!(!state_dir(&dry).exists());

    // NEGATIVE CONTROL: an everyday launch IS announced, once, through the same
    // path — so the refusals above are the plan's, not a notice that never types.
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--model", "opus"])),
        100,
        &mut send,
    );
    assert_eq!(got.step, "announced:1");
    assert_eq!(st.phase, Phase::Announced { at_s: 100, asks: 1 });
    assert_eq!(st.marker, "ATERM-UPGRADE-READY-0badf00d");
    assert_eq!(
        typed,
        vec![1],
        "the notice was typed only for the everyday launch"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SWEEP LOCK IS FREE THE MOMENT ITS GUARD DROPS, whatever copies of its
/// descriptor live on. `copy` stands in for a child another thread is
/// mid-spawning, which holds every descriptor until its exec: were the lock
/// released by the close alone, the copy would keep it held and the next sweep
/// would read `busy:another-sweep` — the 2026-09-24 landing-gate failure, which a
/// 100 ms poll waited out only for a spawn faster than the poll. A regression of
/// [`SweepLock`] to a bare `File` fails the assertion after the release. Negative
/// control first: a lock a sweep holds refuses.
#[test]
fn a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch("lock-window");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
    };
    let holder = sweep_lock(&opts).expect("free").expect("a real run locks");
    assert_eq!(
        sweep_lock(&opts).err(),
        Some("another-sweep"),
        "a lock a sweep holds refuses the next"
    );
    let copy = holder.0.try_clone().expect("a copy of the descriptor");
    // Released by `LOCK_UN` (the guard's drop) while the copy is still open.
    drop(holder);
    assert!(
        matches!(sweep_lock(&opts), Ok(Some(_))),
        "a released sweep lock must be free while a copy of its descriptor lives"
    );
    drop(copy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_what_a_sweep_did_is_an_act() {
    let r = |step: &str| Report {
        pid: 1,
        tab: "s-1".to_string(),
        session: "s".to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: step.to_string(),
    };
    for quiet in [
        "current",
        "wait:not-idle",
        "skip:not-selected",
        "busy:another-sweep",
        "would-announce",
    ] {
        assert!(!r(quiet).is_act(), "{quiet}");
    }
    for act in [
        "announced:1",
        "done",
        "done:no-continue",
        "gave-up",
        "failed:no-resume",
        "refused:--print",
        "held-back:terminal:tmux",
    ] {
        assert!(r(act).is_act(), "{act}");
    }
    assert_eq!(
        r("done").line(),
        "upgrade pid=1 tab=s-1 session=s from=2.1.280 to=2.1.281(native) step=done"
    );
}

// ---------------------------------------------------------------- stand-ins

/// The tab every stand-in agent names: no instance has it, and every test here
/// points its sweep at a socket that does not exist, so nothing can be typed.
const TAB: &str = "s-feedfacefeedfacefeed";

/// A conversation id the stand-in agents hold.
const SESSION: &str = "0badf00d-1111-2222-3333-444455556666";

/// The stand-in agent's argv: THIS test binary, running only [`park_when_asked`].
const PARK: [&str; 2] = ["harness::upgrade_drive::tests::park_when_asked", "--exact"];

/// What tells [`park_when_asked`] to park.
const PARK_ENV: &str = "UPGRADE_DRIVE_TEST_PARK";

/// The stand-in agents' body: idle unless asked to park.
#[test]
fn park_when_asked() {
    if std::env::var_os(PARK_ENV).is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// A stand-in agent in [`TAB`]: this test binary, parked. Not `/bin/sleep`: a
/// platform binary's environment is hidden from `KERN_PROCARGS2`, a real
/// agent's is not (measured by the audit).
fn parked() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c
}

/// Wait until `pid` runs this test binary (a fork not yet exec'd still reads
/// as its parent).
fn wait_exec(pid: u32) {
    let me = std::env::current_exe().expect("exe");
    for _ in 0..500 {
        if atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| Path::new(&a.exec_path).file_name() == me.file_name())
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the stand-in agent {pid} never started");
}

/// Claude's session file for `pid`, with the kernel's start time.
fn register(home: &Path, pid: u32, session: &str) -> SessionFile {
    let start = kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("sessions");
    std::fs::write(
        home.join(format!(".claude/sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    session_file_of(home, pid).expect("the file parses")
}

/// The complete roster was already read to find candidates. A quiet sweep
/// with multiple live conversations must not re-scan it once per candidate.
#[test]
fn multiple_candidates_share_one_preliminary_session_scan() {
    let dir = scratch("one-precheck-scan");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let mut a = parked().spawn().expect("agent A");
    let mut b = parked().spawn().expect("agent B");
    wait_exec(a.id());
    wait_exec(b.id());
    register(&opts.home, a.id(), SESSION);
    register(&opts.home, b.id(), "0badf00d-1111-2222-3333-444455556667");
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let reports = sweep(&opts);
    let scans = SESSION_FILE_SCANS.with(std::cell::Cell::get);
    assert_eq!(
        reports.len(),
        2,
        "both conversations were visited: {reports:?}"
    );
    assert!(
        reports
            .iter()
            .all(|r| matches!(r.step.as_str(), "current" | "would-refuse:not-a-shell-job")),
        "both candidates passed the preliminary owner check: {reports:?}"
    );
    assert_eq!(scans, 1, "only the initial complete roster read is needed");
    for child in [&mut a, &mut b] {
        child.kill().expect("stop agent");
        child.wait().expect("reap agent");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ready_from_tab_a_never_announces_or_terminates_tab_b() {
    let dir = scratch("notice-owner");
    let opts = drive(&dir);
    let mut a = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("tab A");
    let mut b = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-b")
        .spawn()
        .expect("tab B");
    wait_exec(a.id());
    wait_exec(b.id());
    let sf_a = register(&opts.home, a.id(), SESSION);
    let sf_b = register(&opts.home, b.id(), SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#),
    )
    .expect("READY transcript");
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(
            &transcript(&opts.home, SESSION).expect("transcript"),
            TAIL_BYTES
        )
        .0,
        marker
    ));
    let st = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        tab: "s-a".to_string(),
        notice_pid: sf_a.pid,
        notice_start: squash(&sf_a.proc_start),
        ..St::default()
    };
    assert!(st.notice_belongs_to(&sf_a, "s-a"));
    let mut recycled = sf_a.clone();
    recycled.proc_start = "Thu Sep 24 00:00:00 2026".to_string();
    assert!(!st.notice_belongs_to(&recycled, "s-a"));
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    let args_b = atpkg::caller_shell::ProcArgs {
        env: vec!["ATERM_PARENT_SESSION_ID=s-b".to_string()],
        ..atpkg::caller_shell::ProcArgs::default()
    };
    let files = session_files(&opts.home).expect("complete session scan");
    let model = aterm_spec::derive::harness_upgrade_notice_owner_model();
    let mut duplicate = model.init_state();
    assert!(model.fire("Announce", &mut duplicate));
    assert!(model.fire("Ready", &mut duplicate));
    assert!(model.fire("DuplicateOwner", &mut duplicate));
    assert_eq!(
        model.action_enabled("Terminate", &duplicate),
        unique_live_owner(&files, &sf_a) && st.notice_belongs_to(&sf_a, "s-a")
    );
    let report = visit_with_claim(
        &opts,
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&args_b),
        None,
    );
    assert_eq!(report.step, "wait:conversation-owner-ambiguous");
    assert_eq!(load(&opts, SESSION), Some(st.clone()));

    a.kill().expect("stop A");
    a.wait().expect("reap A");
    let files = session_files(&opts.home).expect("complete session scan");
    assert!(unique_live_owner(&files, &sf_b));
    let mut foreign = model.init_state();
    assert!(model.fire("Announce", &mut foreign));
    assert!(model.fire("Ready", &mut foreign));
    assert!(model.fire("OtherTab", &mut foreign));
    assert_eq!(
        model.action_enabled("Terminate", &foreign),
        unique_live_owner(&files, &sf_b) && st.notice_belongs_to(&sf_b, "s-b"),
        "Tier-1: B cannot consume A's READY after A exits"
    );
    let report = visit_with_claim(
        &opts,
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&args_b),
        None,
    );
    assert_eq!(report.step, "wait:notice-owned-by-other-process");
    assert_eq!(load(&opts, SESSION), Some(st.clone()));
    assert!(alive(sf_b.pid), "B was not signalled");

    let no_env = atpkg::caller_shell::ProcArgs::default();
    let report = visit_with_claim(
        &opts,
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&no_env),
        None,
    );
    assert_eq!(report.step, "wait:no-aterm-tab");
    assert_eq!(load(&opts, SESSION), Some(st));

    assert_eq!(
        continuation_session(&opts.home, sf_b.pid, SESSION),
        Some(sf_b.clone())
    );
    let other = "0badf00d-1111-2222-3333-444455556667";
    register(&opts.home, sf_b.pid, other);
    assert!(continuation_session(&opts.home, sf_b.pid, SESSION).is_none());
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(dir);
}

/// A second session JSON can be half-written exactly when a sweep reads it.
/// The valid owner's file does not make a partial directory an exhaustive
/// conversation roster, so even an announced READY cannot be used to act.
#[test]
fn malformed_or_unreadable_sibling_vetoes_a_valid_owner() {
    let dir = scratch("incomplete-roster");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#),
    )
    .expect("READY transcript");
    let st = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    let sibling_pid = dead_pid();
    assert_ne!(sibling_pid, sf.pid);
    let sibling = opts
        .home
        .join(format!(".claude/sessions/{sibling_pid}.json"));
    let args = atpkg::caller_shell::ProcArgs {
        env: vec![format!("ATERM_PARENT_SESSION_ID={TAB}")],
        ..atpkg::caller_shell::ProcArgs::default()
    };
    for unreadable in [false, true] {
        if unreadable {
            std::fs::create_dir(&sibling).expect("unreadable JSON path");
        } else {
            std::fs::write(&sibling, "{").expect("half-written JSON");
        }
        assert!(session_files(&opts.home).is_none());
        assert_eq!(
            require_unique_owner(&opts.home, &sf),
            Err("session-files-unreadable")
        );
        let model = aterm_spec::derive::harness_upgrade_notice_owner_model();
        let mut partial = model.init_state();
        assert!(model.fire("Announce", &mut partial));
        assert!(model.fire("Ready", &mut partial));
        assert!(model.fire("IncompleteScan", &mut partial));
        assert_eq!(
            model.action_enabled("Terminate", &partial),
            require_unique_owner(&opts.home, &sf).is_ok() && st.notice_belongs_to(&sf, TAB),
            "Tier-1: incomplete scan cannot use a valid owner's READY"
        );
        let sweep = sweep(&opts);
        assert_eq!(sweep.len(), 1);
        assert_eq!(sweep[0].step, "wait:session-files-unreadable");
        let report = visit_with_claim(&opts, &sf, None, &[], &newer(), &Live, Some(&args), None);
        assert_eq!(report.step, "wait:session-files-unreadable");
        assert_eq!(load(&opts, SESSION), Some(st.clone()));
        assert!(alive(sf.pid), "the valid owner was never signalled");
        if unreadable {
            std::fs::remove_dir(&sibling).expect("remove directory");
        } else {
            std::fs::remove_file(&sibling).expect("remove partial JSON");
        }
    }
    std::fs::copy(
        opts.home.join(format!(".claude/sessions/{}.json", sf.pid)),
        &sibling,
    )
    .expect("valid JSON under the wrong PID filename");
    assert!(
        session_files(&opts.home).is_none(),
        "a numeric filename cannot attest another process's record"
    );
    std::fs::remove_file(&sibling).expect("remove mismatched record");
    assert!(session_files(&opts.home).is_some());
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// A newer build to move to, so a sweep has something to do.
fn newer() -> Targets {
    Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("9.9.9").expect("version"),
            source: Source::Managed,
        }),
        native: None,
    }
}

/// A real sweep's options over `dir`, dialling a socket that does not exist:
/// a sweep that got past every proof reports `wait:no-socket`.
fn drive(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(dir.join("no.sock").to_string_lossy().into_owned()),
        only_sid: None,
        dry_run: false,
    }
}

/// A pid that WAS a process and is not one now.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    dead
}

fn ledger_lines(opts: &Opts) -> usize {
    std::fs::read_to_string(state_dir(opts).join("ledger.jsonl")).map_or(0, |t| t.lines().count())
}

// ---------------------------------------------------------------- the transcript

/// The READY answer is read whatever byte the tail window starts on — also on
/// the SECOND byte of `é` (C3 A9), where a strict decode refused the whole
/// window, left it empty, and the answer read as silence.
#[test]
fn a_ready_answer_is_read_when_the_tail_window_starts_inside_a_character() {
    let dir = scratch("tail");
    let path = dir.join("t.jsonl");
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let ready = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    ) + "\n";
    // An earlier turn whose `é` straddles the first byte of a 256-byte window.
    let mut body = String::from(r#"{"type":"user","message":"é"#);
    body.push_str(&"b".repeat(252 - ready.len()));
    body.push_str("\"}\n");
    body.push_str(&ready);
    std::fs::write(&path, &body).expect("write");
    let bytes = body.as_bytes();
    assert_eq!(
        bytes[bytes.len() - 256],
        0xa9,
        "the window starts mid-character"
    );
    assert!(std::str::from_utf8(&bytes[bytes.len() - 256..]).is_err());
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 256).0,
        marker
    ));
    // Controls: the window on the character's lead byte, on ASCII, and whole.
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 257).0,
        marker
    ));
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 255).0,
        marker
    ));
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 1 << 20).0,
        marker
    ));
    // The mark is where the bytes read end, whatever the window.
    assert_eq!(tail_to_end(&path, 256).1, bytes.len() as u64);
    assert_eq!(tail_to_end(&path, 1 << 20).1, bytes.len() as u64);
    assert_eq!(
        tail_to_end(&dir.join("absent.jsonl"), 256),
        (String::new(), 0)
    );
    // What is read past a mark: the rest, and nothing from a file that shrank.
    let end = bytes.len() as u64;
    assert_eq!(since(&path, end - 4, 1 << 20), "]}}\n");
    assert_eq!(since(&path, end + 1, 1 << 20), "");
    assert_eq!(since(&path, 0, 3), r#"{"t"#);
    // Lossy never invents an answer: another marker is still absent.
    assert!(!upgrade::transcript_has_ready(
        &tail_to_end(&path, 256).0,
        "ATERM-UPGRADE-READY-00000000"
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the composer

/// `cell` replies measured 2026-09-23 on s-b5cf… under Claude Code 2.1.281,
/// read only: the placeholder's first letter at the caret row's column 2, the
/// caret glyph, the no-break space after it, a blank past the text.
#[test]
fn a_cell_reply_is_dim_only_by_its_attrs() {
    assert!(cell_is_dim("OK p 999999 111318 dim"));
    assert!(!cell_is_dim("OK %E2%9D%AF d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %C2%A0 d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %20 d0d0d0 111318 none"));
    // The attrs are a comma list, a link trails them, and a never-written
    // cell's grapheme is an EMPTY token that must not shift the fields.
    assert!(cell_is_dim(
        "OK x 999999 111318 bold,dim link=https%3A%2F%2Fa"
    ));
    assert!(cell_is_dim("OK  999999 111318 dim,wide_cont"));
    assert!(!cell_is_dim("OK  999999 111318 none"));
    assert!(!cell_is_dim("ERR out of range"));
    assert!(!cell_is_dim("OK x 999999 111318"), "no attrs, no proof");
    assert!(!cell_is_dim(""));
}

/// A control connection whose server answers each request with the next of
/// `replies`, and hands back every request it read once the client is gone.
#[cfg(unix)]
fn fake_tab(replies: Vec<&'static str>) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    use std::io::BufRead as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let server = std::thread::spawn(move || {
        let mut out = theirs.try_clone().expect("clone");
        let mut asked = Vec::new();
        let mut replies = replies.into_iter();
        for line in std::io::BufReader::new(theirs).lines() {
            let Ok(line) = line else { break };
            asked.push(line);
            let reply = replies.next().unwrap_or("ERR no reply scripted");
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
        asked
    });
    (RelayClient::new(ours), server)
}

#[cfg(unix)]
const TAIL_DRAFT: &str = r#"OK {"rows":["────────────","❯ draft","────────────"],"cursor":{"row":57,"col":7},"first":56}"#;
#[cfg(unix)]
const TAIL_EMPTY: &str =
    r#"OK {"rows":["────────────","❯ ","────────────"],"cursor":{"row":57,"col":2},"first":56}"#;
#[cfg(unix)]
const FULL_DRAFT: &str =
    r#"OK {"rows":["────────────","❯ draft","────────────"],"cursor":{"row":1,"col":7},"first":0}"#;
#[cfg(unix)]
const FULL_EMPTY: &str =
    r#"OK {"rows":["────────────","❯ ","────────────"],"cursor":{"row":1,"col":2},"first":0}"#;

#[cfg(unix)]
#[test]
fn continuation_poll_uses_only_the_tail_while_a_draft_remains() {
    let (mut c, server) = fake_tab(vec![TAIL_DRAFT, TAIL_DRAFT]);
    let mut tail_available = true;
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(tail_available);
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x text --json tail=20"; 2],
        "a draft never needs a full-grid read or a cell probe"
    );
}

#[cfg(unix)]
#[test]
fn continuation_poll_confirms_an_empty_tail_with_the_full_screen() {
    let (mut c, server) = fake_tab(vec![
        TAIL_EMPTY,
        "OK %20 d0d0d0 111318 none",
        FULL_DRAFT,
        TAIL_EMPTY,
        "OK %20 d0d0d0 111318 none",
        FULL_EMPTY,
        "OK %20 d0d0d0 111318 none",
    ]);
    let mut tail_available = true;
    assert!(
        !continuation_composer_empty(&mut c, "s-x", &mut tail_available),
        "a blank tail alone cannot authorize continuation"
    );
    assert!(continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec![
            "@s-x text --json tail=20",
            "@s-x cell 57 2",
            "@s-x text --json",
            "@s-x text --json tail=20",
            "@s-x cell 57 2",
            "@s-x text --json",
            "@s-x cell 1 2",
        ],
        "tail `first` and full-screen cell coordinates stay distinct"
    );
}

#[cfg(unix)]
#[test]
fn continuation_poll_falls_back_on_an_unreadable_or_inconclusive_tail() {
    const NO_CARET: &str =
        r#"OK {"rows":["continuation of a long draft"],"cursor":{"row":58,"col":2},"first":56}"#;
    let (mut c, server) = fake_tab(vec![
        NO_CARET,
        FULL_DRAFT,
        "ERR usage: text [--json] [tail=<n>]",
        FULL_DRAFT,
        FULL_DRAFT,
    ]);
    let mut tail_available = true;
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(tail_available, "a truncated slice may resolve next poll");
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(!tail_available, "an unsupported tail is probed only once");
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec![
            "@s-x text --json tail=20",
            "@s-x text --json",
            "@s-x text --json tail=20",
            "@s-x text --json",
            "@s-x text --json",
        ]
    );
}

/// THE COMPOSER CHECK trusts only an `OK … dim` at the caret row's column 2.
/// The placeholder is empty; the SAME rows and cursor with the text typed and
/// the caret moved home (←, Home, ctrl-a — measured 2026-09-23: the cursor back
/// at column 2, the cell `none`) are a draft, which the cursor alone read as
/// empty and the notice was pasted in front of; an unreadable cell is a draft.
#[cfg(unix)]
#[test]
fn a_homed_draft_is_never_read_as_the_placeholder() {
    // The live composer's shape (`❯`, a no-break space, the text), on screen
    // rows 53-55 of the tab.
    let screen = |text: &str, col: usize| Screen {
        rows: vec!["─".repeat(20), format!("❯\u{a0}{text}"), "─".repeat(20)],
        cursor: Some((1, col)),
        seq: 1,
        first: 53,
    };
    let text = "push it as soon as the gates pass";
    let (mut c, server) = fake_tab(vec![
        "OK p 999999 111318 dim",
        "OK p d0d0d0 111318 none",
        "ERR out of range",
        "OK %20 d0d0d0 111318 none",
    ]);
    assert!(
        composer_empty(&mut c, "s-x", &screen(text, 2)),
        "the placeholder"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 2)),
        "typed, the caret moved home"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 2)),
        "unreadable"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 35)),
        "typed, the caret at the end: nothing asked"
    );
    assert!(composer_empty(&mut c, "s-x", &screen("", 2)), "blank");
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x cell 54 2"; 4],
        "the caret row's column 2, in the screen rows `cell` takes"
    );
    // The rule itself: the rows and cursor of the two are identical, and only
    // the dim bit tells them apart.
    let homed = screen(text, 2);
    assert!(upgrade::composer_is_empty(&homed.rows, homed.cursor, true));
    assert!(!upgrade::composer_is_empty(
        &homed.rows,
        homed.cursor,
        false
    ));
}

// ---------------------------------------------------------------- the terminal

fn tty_rows(rows: &[(u32, u32, &str, &str)]) -> Vec<(u32, u32, String, String)> {
    rows.iter()
        .map(|(p, pp, tty, name)| (*p, *pp, (*tty).to_string(), (*name).to_string()))
        .collect()
}

/// THE TERMINAL PROOF over process tables: the program that opened the agent's
/// terminal is an aterm, or gone (the tab handed on) — never a multiplexer,
/// `script`, or nothing.
#[test]
fn a_terminal_is_the_tabs_only_when_aterm_opened_it() {
    // Measured 2026-09-23, read only: this machine's live agent, its `-zsh`, and
    // launchd — the aterm that opened the tab handed it on and exited.
    let live = tty_rows(&[
        (4205, 1916, "ttys000", "2.1.281"),
        (1916, 1, "ttys000", "zsh"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(4205, &live), Some((1, "launchd")));
    assert!(terminal_owner(4205, &live).is_some_and(owned_by_aterm));
    // A tab of an instance still running: the owner is that aterm (its argv0
    // aliases too).
    for name in ["aterm", "aterm-gui"] {
        let fresh = tty_rows(&[
            (700, 600, "ttys004", "claude"),
            (600, 500, "ttys004", "zsh"),
            (500, 1, "??", name),
            (1, 0, "??", "launchd"),
        ]);
        assert!(
            terminal_owner(700, &fresh).is_some_and(owned_by_aterm),
            "{name}"
        );
    }
    // Measured the same day: a program under `script`, on the pty it opened.
    let script = tty_rows(&[
        (99234, 99225, "ttys002", "sleep"),
        (99225, 1, "??", "script"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(99234, &script), Some((99225, "script")));
    assert!(!terminal_owner(99234, &script).is_some_and(owned_by_aterm));
    // GNU screen started IN a tab: the window's agent is on the pty screen's
    // server opened; the tab's own shell is on the tab's.
    let screen = tty_rows(&[
        (29687, 28893, "ttys005", "2.1.0"),
        (28893, 28890, "ttys005", "zsh"),
        (28890, 28889, "??", "screen"),
        (28889, 22415, "ttys006", "screen"),
        (22415, 1, "ttys006", "zsh"),
        (1, 0, "??", "launchd"),
    ]);
    assert!(!terminal_owner(29687, &screen).is_some_and(owned_by_aterm));
    assert!(terminal_owner(22415, &screen).is_some_and(owned_by_aterm));
    // A tmux pane (Linux spellings): the server, a daemon, opened it.
    let tmux = tty_rows(&[
        (800, 801, "pts/7", "claude"),
        (801, 802, "pts/7", "bash"),
        (802, 1, "?", "tmux"),
        (1, 0, "?", "systemd"),
    ]);
    assert!(!terminal_owner(800, &tmux).is_some_and(owned_by_aterm));
    assert_eq!(owner_word(terminal_owner(800, &tmux)), "tmux");
    // No terminal at all, or a walk that cannot finish: no proof.
    let bare = tty_rows(&[(900, 1, "??", "claude"), (1, 0, "??", "launchd")]);
    assert_eq!(terminal_owner(900, &bare), None);
    let cut = tty_rows(&[(901, 555, "ttys001", "claude")]);
    assert_eq!(terminal_owner(901, &cut), None);
    assert_eq!(owner_word(None), "none");
    assert_eq!(owner_word(Some((5, "a b/c"))), "abc");
}

/// END TO END: an agent on a pty the tab does not own — `script`'s here, the
/// shape of every tmux or screen pane, whose environment still names the tab —
/// is never driven through that tab. The sweep stops before any socket is
/// dialled (unguarded, it reached the socket, which does not exist here), and
/// says why ONCE — in the ledger and as an act the window's host logs — then
/// waits in silence.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_on_a_pty_the_tab_does_not_own_is_never_driven_through_the_tab() {
    let dir = scratch("pty");
    let opts = drive(&dir);
    let exe = std::env::current_exe().expect("exe");
    let mut script = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null"])
        .arg(&exe)
        .args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("script");
    let owner = script.id();
    let agent = (0..250)
        .find_map(|_| {
            let hit = table()
                .into_iter()
                .find(|(_, pp, _)| *pp == owner)
                .map(|(p, _, _)| p);
            if hit.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            hit
        })
        .expect("the agent under script");
    wait_exec(agent);
    let sf = register(&opts.home, agent, SESSION);
    let got = visit(&opts, &sf, &table(), &newer(), &Live);
    let again = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = Command::new("kill")
        .args(["-9", &agent.to_string()])
        .status();
    let _ = script.kill();
    let _ = script.wait();
    assert_eq!(got.step, "held-back:terminal:script", "{got:?}");
    assert_eq!(got.tab, TAB);
    assert!(got.is_act(), "said, where the window's host logs it");
    assert_eq!(again.step, "wait:terminal:script", "{again:?}");
    assert!(!again.is_act(), "said ONCE");
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the job

/// [`Job`] from `ps`'s numbers, measured shapes first, then real processes.
#[cfg(unix)]
#[test]
fn only_its_shells_foreground_job_is_ever_announced_to_or_ended() {
    use std::os::unix::process::CommandExt as _;
    // Measured 2026-09-23 (`ps -o pid,ppid,pgid,tpgid`): the live agent and
    // its `-zsh`.
    assert_eq!(job(4205, 4205, 4205, 1916), Job::Foreground);
    // The same agent suspended or backgrounded: the shell holds the terminal.
    assert_eq!(job(4205, 4205, 1916, 1916), Job::Background);
    // Measured by the audit on a pty: a bash launcher (`cc.sh`, no `exec`) and
    // its agent share one group, and it is the terminal's.
    assert_eq!(job(30771, 30770, 30770, 30770), Job::NoJobControl);
    // zsh with `unsetopt monitor`: the agent in the shell's own group.
    assert_eq!(job(24965, 24932, 24932, 24932), Job::NoJobControl);
    // Real processes: a child left in its parent's group (what `sh -c`, `make`
    // or an IDE task does) is no job; one given a group of its own is a job,
    // and not the terminal's foreground.
    let me = std::process::id();
    let mut plain = parked().spawn().expect("spawn");
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let (a, b) = (job_of(plain.id()), job_of(own.id()));
    for c in [&mut plain, &mut own] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(a, Some((Job::NoJobControl, me)));
    assert_eq!(b, Some((Job::Background, me)));
}

/// END TO END: an agent no job-control shell started (a launcher script that
/// runs `claude` without `exec`, `sh -c`, `make`, an IDE task) is refused BEFORE
/// anything is typed, once, in the ledger — never announced to, never ended
/// with nothing to relaunch it (unguarded, the sweep went on toward the tab).
#[cfg(unix)]
#[test]
fn an_agent_no_job_control_shell_started_is_refused_before_anything_is_typed() {
    use std::os::unix::process::CommandExt as _;
    let dir = scratch("job");
    let opts = drive(&dir);
    // In THIS process's group: what a launcher script gives its child.
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let first = visit(&opts, &sf, &table(), &newer(), &Live);
    let again = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(first.step, "refused:not-a-shell-job", "{first:?}");
    assert!(first.is_act(), "said, where the window's host notes it");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("not-a-shell-job".to_string()))
    );
    // Said ONCE: the next sweep finds the upgrade failed and asks nothing.
    assert_eq!(again.step, "wait:no-socket", "{again:?}");
    assert_eq!(ledger_lines(&opts), 1);
    // A dry run says what it would do and records nothing.
    let other = "0badf00d-1111-2222-3333-444455556667";
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), other);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let got = visit(&dry, &sf, &table(), &newer(), &Live);
    // Control: the same stand-in as a job of its own passes this proof (and,
    // on no terminal at all, is no foreground job: it waits).
    let mut job = parked().process_group(0).spawn().expect("spawn");
    wait_exec(job.id());
    let third = "0badf00d-1111-2222-3333-444455556668";
    let sf = register(&opts.home, job.id(), third);
    let control = visit(&opts, &sf, &table(), &newer(), &Live);
    for c in [&mut agent, &mut job] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(got.step, "would-refuse:not-a-shell-job");
    assert_eq!(load(&opts, other), None);
    assert_eq!(control.step, "wait:not-foreground", "{control:?}");
    assert!(!control.is_act(), "{control:?}");
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One job read, as every look before an announce or a SIGTERM takes it: only
/// its shell's foreground job goes on, with that shell.
#[test]
fn a_job_read_lets_through_only_its_shells_foreground_job() {
    assert_eq!(foreground_shell(Some((Job::Foreground, 1916))), Ok(1916));
    assert_eq!(
        foreground_shell(Some((Job::Background, 1916))),
        Err("not-foreground")
    );
    assert_eq!(
        foreground_shell(Some((Job::NoJobControl, 1916))),
        Err("not-a-shell-job")
    );
    assert_eq!(foreground_shell(None), Err("ids"));
}

/// A [`Kernel`] that answers as scripted: the agent is `shell`'s foreground
/// job for its first `foreground` job reads and a background job after — a
/// ctrl-z landing between two looks, which no real process does on cue — its
/// terminal the tab's (the tab handed on, as measured live), and its parent
/// `parent`.
struct Script {
    shell: u32,
    foreground: usize,
    reads: std::cell::Cell<usize>,
    parent: Option<u32>,
}

impl Script {
    fn new(shell: u32, foreground: usize, parent: Option<u32>) -> Script {
        Script {
            shell,
            foreground,
            reads: std::cell::Cell::new(0),
            parent,
        }
    }
}

impl Kernel for Script {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        let n = self.reads.get();
        self.reads.set(n + 1);
        let job = if n < self.foreground {
            Job::Foreground
        } else {
            Job::Background
        };
        Some((job, self.shell))
    }

    fn parent(&self, _: u32) -> Option<u32> {
        self.parent
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        Some((1, "launchd".to_string()))
    }
}

/// The screen [`instance`] shows: an idle Claude's empty composer, the caret at
/// column 2 of its row.
fn idle_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0}}"#
    )
}

/// The token [`instance`] writes beside its socket.
const TOKEN: &str = "0badf00d0badf00d";

/// A control socket at `<dir>/t.sock`, its token beside it, standing in for
/// the aterm instance of [`TAB`] with an idle Claude in it: `sessions` names the
/// tab, `text --json` is [`idle_screen`], a `cell` is a plain blank, `status`
/// holds nothing, and a `turn` is answered OK — recorded here, typed nowhere.
/// Returns the socket and every request it read, in order. The accept loop
/// ends with the test process.
#[cfg(unix)]
fn instance(dir: &Path) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::BufRead as _;
    let sock = dir.join("t.sock");
    std::fs::write(dir.join("t.sock.token"), format!("{TOKEN}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let Ok(mut out) = conn.try_clone() else {
                continue;
            };
            let mut lines = std::io::BufReader::new(conn).lines();
            // `AUTH <token>` is acknowledged with nothing, as the server does.
            let auth = format!("AUTH {TOKEN}");
            if !lines.next().is_some_and(|l| l.is_ok_and(|l| l == auth)) {
                continue;
            }
            for line in lines {
                let Ok(line) = line else { break };
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("");
                let reply = match verb {
                    "sessions" => format!("OK 1\nlocal {TAB} 0 idle claude"),
                    "text" => format!("OK {}", idle_screen()),
                    "cell" => "OK %20 d0d0d0 111318 none".to_string(),
                    "status" => "OK schema=1 hold=0 hand=-".to_string(),
                    "turn" => "OK status=done".to_string(),
                    _ => "ERR unscripted".to_string(),
                };
                if let Ok(mut log) = log.lock() {
                    log.push(line);
                }
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    (sock.to_string_lossy().into_owned(), asked)
}

/// The requests of `asked` that TYPE into the tab.
#[cfg(unix)]
fn turns(asked: &std::sync::Mutex<Vec<String>>) -> usize {
    asked
        .lock()
        .map_or(0, |a| a.iter().filter(|l| l.contains(" turn ")).count())
}

/// The initial roster is only a preliminary filter. If another live process
/// claims the conversation before the announce, the action's fresh read must
/// refuse the turn even though the earlier snapshot still names one owner.
#[cfg(unix)]
#[test]
fn stale_preliminary_roster_cannot_authorize_an_announcement() {
    let dir = scratch("stale-precheck-roster");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut a = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("agent A");
    wait_exec(a.id());
    let sf_a = register(&opts.home, a.id(), SESSION);
    let initial_files = session_files(&opts.home).expect("initial complete roster");
    assert!(unique_live_owner(&initial_files, &sf_a));

    let mut b = parked().spawn().expect("agent B");
    wait_exec(b.id());
    register(&opts.home, b.id(), SESSION);
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now_s() - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".to_string())];
    let args = atpkg::caller_shell::process_args(a.id()).expect("agent argv");
    let run = || {
        visit_with_claim(
            &opts,
            &sf_a,
            Some(&initial_files),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let blocked = run();
    let scans = SESSION_FILE_SCANS.with(std::cell::Cell::get);
    assert_eq!(blocked.step, "wait:conversation-owner-ambiguous");
    assert_eq!(scans, 1, "the act re-read ownership after the snapshot");
    assert_eq!(turns(&asked), 0, "the stale roster typed nothing");
    assert_eq!(load(&opts, SESSION), Some(st.clone()));

    b.kill().expect("stop agent B");
    b.wait().expect("reap agent B");
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{}.json", b.id())))
        .expect("remove B's session file");
    let allowed = run();
    assert_eq!(allowed.step, "announced:1", "the action path is live");
    assert_eq!(turns(&asked), 1);
    a.kill().expect("stop agent A");
    a.wait().expect("reap agent A");
    let _ = std::fs::remove_dir_all(dir);
}

/// The first PTY claim can be true and the tab can switch foreground groups
/// while the continuation reads its latest session file. The final claim must
/// veto typing, leaving the restart in flight for another sweep.
#[cfg(unix)]
#[test]
fn carry_on_rechecks_tab_ownership_at_the_turn() {
    let dir = scratch("carry-on-tab-race");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let mut reads = 0;
    let report = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, pid, tab| {
            assert_eq!((pid, tab), (sf.pid, TAB));
            reads += 1;
            reads == 1
        },
    );
    assert_eq!(
        reads, 2,
        "the second ownership read is immediately before turn"
    );
    assert_eq!(report.step, "wait:tab-ownership-changed");
    assert_eq!(turns(&asked), 0, "nothing typed into the changed tab");
    assert!(matches!(st.phase, Phase::Relaunched { .. }));
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// A shell can leave the tab's foreground group during the complete session
/// scan. The second PTY claim is the one directly guarding the relaunch turn.
#[cfg(unix)]
#[test]
fn relaunch_rechecks_shell_tab_after_the_session_scan() {
    let dir = scratch("relaunch-tab-race");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let mut reads = 0;
    let result = type_relaunch_line(
        &opts,
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        |_, pid, tab| {
            assert_eq!((pid, tab), (shell, TAB));
            reads += 1;
            reads == 1
        },
    );
    assert_eq!(reads, 2, "the shell's tab was checked again after the scan");
    assert_eq!(
        result,
        Err(RelaunchLineError::Wait("tab-ownership-changed"))
    );
    assert_eq!(turns(&asked), 0, "the moved shell received no relaunch");

    type_relaunch_line(
        &opts,
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        |_, _, _| true,
    )
    .expect("unchanged tab can receive a relaunch");
    assert_eq!(turns(&asked), 1, "positive control reaches the turn");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// EVERY LOOK AT THE JOB before an act is the one standing between a ctrl-z and
/// that act. The agent is its shell's foreground job at the first look and a
/// background job from the Nth read on; each N stops the sweep at exactly one
/// look, and nothing is typed or signalled: the re-read before the announce
/// (N = 1, on a pending upgrade), the same re-read before the SIGTERM (N = 1),
/// restart's own gate (N = 2), and the last look before the signal (N = 3).
/// Dropped, each lets the act through — the announce is typed, or the next
/// look answers another word. The control proves the path is live: with every
/// read foreground, the announce IS typed.
#[cfg(unix)]
#[test]
fn every_look_at_the_job_stands_between_a_ctrl_z_and_the_act() {
    let dir = scratch("jobs");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // An argv a resume carries (the filter alone: a positional, dropped).
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    // The READY answer to the announcement already typed.
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
    // The shell: a pid no process holds now, so its name comes from the table
    // the sweep is handed.
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".to_string())];
    let now = now_s();
    let put = |phase: Phase| {
        let st = St {
            phase,
            marker: marker.to_string(),
            tab: TAB.to_string(),
            notice_pid: sf.pid,
            notice_start: squash(&sf.proc_start),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            last_seq: 77,
            seq_since_s: now - 3600,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    };
    let run = |phase: Phase, foreground: usize| {
        put(phase);
        let k = Script::new(shell, foreground, None);
        let r = visit(&opts, &sf, &t, &newer(), &k);
        (r.step, k.reads.get())
    };
    let announced = Phase::Announced {
        at_s: now - 60,
        asks: 1,
    };
    let before_announce = run(Phase::Pending, 1);
    let typed_before = turns(&asked);
    let control = run(Phase::Pending, usize::MAX);
    let typed_control = turns(&asked);
    let before_signal = run(announced.clone(), 1);
    let restart_gate = run(announced.clone(), 2);
    let last_look = run(announced, 3);
    let typed_after = turns(&asked);
    let still_running = alive(pid);
    let phase = load(&opts, SESSION).map(|s| s.phase);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(before_announce, ("wait:not-foreground".to_string(), 2));
    assert_eq!(
        typed_before, 0,
        "nothing typed at a suspended agent's shell"
    );
    assert_eq!(control, ("announced:1".to_string(), 2), "the path is live");
    assert_eq!(typed_control, 1);
    assert_eq!(before_signal, ("wait:not-foreground".to_string(), 2));
    assert_eq!(restart_gate, ("wait:not-foreground".to_string(), 3));
    assert_eq!(last_look, ("wait:changed".to_string(), 4));
    assert_eq!(typed_after, 1, "nothing typed after the control");
    assert!(still_running, "never signalled");
    assert!(
        matches!(phase, Some(Phase::Announced { .. })),
        "never exiting: {phase:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the relaunch

/// Only the relaunch itself — the SAME conversation, a child of the shell the
/// line was typed at — is adopted and told it was upgraded.
#[cfg(unix)]
#[test]
fn only_the_relaunch_itself_is_adopted_and_told_it_was_upgraded() {
    let dir = scratch("adopt");
    let opts = drive(&dir);
    // Its parent is THIS process, standing in for the shell.
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me, dead) = (agent.id(), std::process::id(), dead_pid());
    wait_exec(pid);
    let s = |session: &str| SessionFile {
        pid,
        session_id: session.to_string(),
        cwd: "/".to_string(),
        version: "9.9.9".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 1,
        proc_start: String::new(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
    };
    let fresh = "11111111-2222-3333-4444-555555555555";
    let adopted = [
        relaunch_of(&Live, &s(SESSION), SESSION, dead, me),
        // Measured 2026-09-23: a person's fresh `claude` at that prompt after a
        // relaunch that failed — the shell's child, another conversation.
        relaunch_of(&Live, &s(fresh), SESSION, dead, me),
        // The conversation resumed by hand under another shell.
        relaunch_of(&Live, &s(SESSION), SESSION, dead, 1),
        // The process that was ended is never its own relaunch.
        relaunch_of(&Live, &s(SESSION), SESSION, pid, me),
    ];
    // END TO END, the in-flight branch of a sweep: a process the recorded
    // shell did not start holds the conversation.
    let sf = register(&opts.home, pid, SESSION);
    let put = |shell: u32| {
        let st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: dead,
            shell,
            tab: TAB.to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    };
    put(1);
    let elsewhere = visit(&opts, &sf, &table(), &newer(), &Live);
    let failed = load(&opts, SESSION).map(|s| s.phase);
    // Control: the recorded shell IS its parent — adopted, on to the tab.
    put(me);
    let ours = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(
        adopted,
        [
            Relaunch::Ours,
            Relaunch::NotOurs,
            Relaunch::NotOurs,
            Relaunch::NotOurs
        ]
    );
    assert_eq!(elsewhere.step, "failed:resumed-elsewhere", "{elsewhere:?}");
    assert_eq!(failed, Some(Phase::Failed("resumed-elsewhere".to_string())));
    assert_eq!(ours.step, "wait:no-socket", "{ours:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch is judged from a parent READ: a parent that cannot be read (the
/// process ended as it was asked, or `ps` could not run) is no verdict at all.
#[test]
fn a_parent_that_cannot_be_read_is_no_verdict_on_the_relaunch() {
    assert_eq!(relaunch_by_parent(Some(1916), 1916), Relaunch::Ours);
    assert_eq!(relaunch_by_parent(Some(1), 1916), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 1916), Relaunch::Unread);
    // No shell recorded: nothing can be the relaunch, read or not.
    assert_eq!(relaunch_by_parent(Some(0), 0), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 0), Relaunch::NotOurs);
    // The kernel's reader: a process that is gone has no parent to read.
    assert_eq!(Live.parent(dead_pid()), None);
    let me = std::process::id();
    assert_eq!(Live.parent(me), ids(me).map(|(ppid, _, _)| ppid));
}

/// END TO END, the in-flight branch: the new process holding the conversation
/// fails the upgrade for good ONLY from a parent read as another than the
/// recorded shell. Unread, it is a wait — the phase kept, nothing in the
/// ledger, the next sweep asks again — where it was recorded as
/// `resumed-elsewhere` with a ledger line saying a process this upgrade did not
/// relaunch held the conversation, and the real relaunch was never told to
/// carry on.
#[cfg(unix)]
#[test]
fn a_relaunch_whose_parent_cannot_be_read_is_waited_for_never_failed() {
    let dir = scratch("unread");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let (dead, shell, at_s) = (dead_pid(), 4242, now_s());
    let st = St {
        phase: Phase::Relaunched { at_s },
        pid: dead,
        shell,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let parent = |p: Option<u32>| Script::new(shell, usize::MAX, p);
    let unread = visit(&opts, &sf, &table(), &newer(), &parent(None));
    let kept = load(&opts, SESSION).map(|s| s.phase);
    let unread_lines = ledger_lines(&opts);
    // Control: read as the recorded shell, it is the relaunch — on to the tab.
    let ours = visit(&opts, &sf, &table(), &newer(), &parent(Some(shell)));
    // Read as another: the one permanent verdict.
    let other = visit(&opts, &sf, &table(), &newer(), &parent(Some(shell + 1)));
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(unread.step, "wait:ids", "{unread:?}");
    assert!(!unread.is_act());
    assert_eq!(kept, Some(Phase::Relaunched { at_s }));
    assert_eq!(unread_lines, 0);
    assert_eq!(ours.step, "wait:no-socket", "{ours:?}");
    assert_eq!(other.step, "failed:resumed-elsewhere", "{other:?}");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("resumed-elsewhere".to_string()))
    );
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart left in flight acts on its tab only while it is fresh and its
/// shell is there: an exit older than [`STALE_S`] is never relaunched (the
/// person may be typing at that prompt by now), an exit whose shell is gone has
/// no prompt to relaunch at, and a relaunch older than [`STALE_S`] is waited
/// for no longer — each said once, before any socket is dialled.
#[test]
fn a_restart_in_flight_too_long_or_without_its_shell_never_acts_on_the_tab() {
    let dir = scratch("stale");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let old = now - STALE_S - 1;
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000001",
            Phase::Exiting { at_s: old },
            me,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000002",
            Phase::Exiting { at_s: now },
            dead,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000003",
            Phase::Relaunched { at_s: old },
            me,
        ),
        // Control: fresh, its shell alive — on to the tab.
        (
            "aaaaaaaa-0000-0000-0000-000000000004",
            Phase::Exiting { at_s: now },
            me,
        ),
    ];
    for (session, phase, shell) in &cases {
        let st = St {
            phase: phase.clone(),
            pid: dead,
            shell: *shell,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    }
    let steps = |o: &Opts| {
        let mut got: Vec<(String, String)> = orphans(o, &[], None)
            .into_iter()
            .map(|r| (r.session, r.step))
            .collect();
        got.sort();
        got.into_iter().map(|(_, step)| step).collect::<Vec<_>>()
    };
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        steps(&dry),
        [
            "would-fail:stale-exit",
            "would-fail:shell-gone",
            "would-fail:no-resume",
            "would-resume:exiting"
        ]
    );
    assert_eq!(ledger_lines(&opts), 0, "a dry run records nothing");
    assert_eq!(
        steps(&opts),
        [
            "failed:stale-exit",
            "failed:shell-gone",
            "failed:no-resume",
            "wait:no-socket"
        ]
    );
    assert_eq!(ledger_lines(&opts), 3);
    assert_eq!(
        load(&opts, cases[0].0).map(|s| s.phase),
        Some(Phase::Failed("stale-exit".to_string()))
    );
    assert_eq!(
        load(&opts, cases[3].0).map(|s| s.phase),
        Some(Phase::Exiting { at_s: now })
    );
    // Said once: a failed restart is no longer in flight.
    assert_eq!(steps(&opts), ["wait:no-socket"]);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the model

/// An assistant row naming `model`, as Claude Code 2.1.282 writes one
/// (measured 2026-09-24 in the owner's transcript), saying `text`.
fn turn_by(model: &str, text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}},"version":"2.1.282"}}"#
    )
}

/// A user row saying `text`.
fn user_row(text: &str) -> String {
    format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#)
}

/// A restart relaunched on 2.1.282 from 2.1.281 over a stand-in agent in
/// [`TAB`], at the moment its new process holds the conversation: the state as
/// [`relaunch`] leaves it, ON DISK, with what the restart recorded of the
/// transcript — through the sweep's own read, [`tail_to_end`] and
/// [`model_and_mark`] — when it was `before`. Dropped, it stops the agent and
/// removes its directory.
#[cfg(unix)]
struct Rig {
    dir: PathBuf,
    opts: Opts,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    agent: std::process::Child,
    sf: SessionFile,
    path: PathBuf,
    st: St,
}

#[cfg(unix)]
impl Rig {
    fn new(name: &str, before: &[String]) -> Rig {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let file = opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.id()));
        register(&opts.home, agent.id(), SESSION);
        let relaunched = std::fs::read_to_string(&file)
            .expect("session file")
            .replace(r#""version":"1.0.0""#, r#""version":"2.1.282""#);
        std::fs::write(&file, relaunched).expect("relaunched version");
        let sf = session_file_of(&opts.home, agent.id()).expect("parses");
        let project = opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        let path = project.join(format!("{SESSION}.jsonl"));
        std::fs::write(&path, before.join("\n") + "\n").expect("transcript");
        let (model_before, mark) = model_and_mark(Some(&tail_to_end(&path, TAIL_BYTES)));
        let st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            model_before,
            mark,
            ..St::default()
        };
        save(&opts, SESSION, &st);
        Rig {
            dir,
            opts,
            asked,
            agent,
            sf,
            path,
            st,
        }
    }

    /// The transcript gains `rows`.
    fn append(&self, rows: &[String]) {
        let mut more = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("append");
        for row in rows {
            writeln!(more, "{row}").expect("row");
        }
    }

    /// Carry `st` on, the resumed answer waited for at most `wait`.
    fn carry_on_from(&self, st: &mut St, wait: Duration) -> Report {
        let mut c = connect(&self.opts, TAB).expect("control connection");
        carry_on_with_tab_probe(
            &self.opts,
            blank(&self.sf),
            st,
            &mut c,
            SESSION,
            &self.sf,
            wait,
            |_, _, _| true,
        )
    }

    /// Every line typed into the tab.
    fn typed(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default()
    }

    /// The details of the ledger's rows whose step is `step`.
    fn details(&self, step: &str) -> Vec<String> {
        std::fs::read_to_string(state_dir(&self.opts).join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
            .filter_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }
}

#[cfg(unix)]
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.agent.kill();
        let _ = self.agent.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What one carried-on restart left: the report, every line typed into the
/// tab, the ledger's detail for the report's step and the state.
struct Carried {
    report: Report,
    typed: Vec<String>,
    detail: String,
    st: St,
}

/// A restart carried on END TO END ([`Rig`]): its transcript is `before` when
/// the restart records what the agent ran, and gains `after` since, and the
/// resumed answer is waited for at most `wait`.
#[cfg(unix)]
fn carried_on(name: &str, before: &[String], after: &[String], wait: Duration) -> Carried {
    let rig = Rig::new(name, before);
    rig.append(after);
    let mut st = rig.st.clone();
    let report = rig.carry_on_from(&mut st, wait);
    let detail = rig.details(&report.step);
    assert_eq!(detail.len(), 1, "the outcome is recorded once: {detail:?}");
    Carried {
        typed: rig.typed(),
        report,
        detail: detail[0].clone(),
        st,
    }
}

/// THE MODEL, BEFORE AND AFTER: the continuation names the model the agent's
/// READY answer ran, and the resumed session's first answer — past the
/// restart's mark, Claude's own `<synthetic>` row skipped (measured just after
/// a 2026-09-23 restart) — is the model after. A change is words only: the
/// step is `done`.
#[cfg(unix)]
#[test]
fn a_restart_says_the_model_before_and_confirms_the_one_after() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5", "Earlier work."),
        user_row("[aterm harness] Claude Code 2.1.282 (managed) is installed"),
        turn_by("claude-opus-5", &format!("Saved.\\n{marker}")),
    ];
    let after = [
        user_row("[aterm harness] Upgraded: carry on"),
        r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"No response requested."}]}}"#.to_string(),
        turn_by("claude-opus-5-5", "Resumed."),
        turn_by("claude-fable-5-1", "A later turn."),
    ];
    let changed = carried_on("model-changed", &before, &after, MODEL_WAIT);
    assert_eq!(changed.report.step, "done", "{:?}", changed.report);
    assert_eq!(changed.st.phase, Phase::Done);
    assert_eq!(
        changed.typed.len(),
        1,
        "one continuation: {:?}",
        changed.typed
    );
    assert!(
        changed.typed[0].contains(
            "restarted on Claude Code 2.1.282 (from 2.1.281); it ran claude-opus-5 before the \
             restart and was resumed."
        ),
        "{:?}",
        changed.typed
    );
    assert_eq!(
        changed.detail,
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (a session \
         launched without --model takes the current default; /model changes it)"
    );
    // The same model on both sides: the model, and nothing about a change.
    let before_same = [before[0].clone(), turn_by("claude-opus-5-5", marker)];
    let same = carried_on(
        "model-same",
        &before_same,
        &[turn_by("claude-opus-5-5", "Resumed.")],
        MODEL_WAIT,
    );
    assert_eq!(same.report.step, "done");
    assert!(same.typed[0].contains("it ran claude-opus-5-5 before the restart"));
    assert_eq!(
        same.detail,
        "claude restarted on 2.1.282 · model claude-opus-5-5"
    );
}

/// UNCONFIRMED: a resumed session that has not answered by the first sweep
/// past the bound is said to be, and it is the one outcome whose step says so.
/// The turns BEFORE the mark name a model: read from the start, they would have
/// "confirmed" it — the mark is what keeps the old process's model from
/// standing in for the new one's.
#[cfg(unix)]
#[test]
fn a_resumed_session_that_has_not_answered_is_unconfirmed() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "ATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-unconfirmed", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued");
    assert!(rig.typed()[0].contains("it ran claude-opus-5-5 before the restart"));
    // Within the bound, a sweep reads again and says nothing.
    assert_eq!(confirmations(&rig.opts), []);
    assert!(load(&rig.opts, SESSION).is_some_and(|st| st.confirming()));
    // Past it, still no answer: unconfirmed, once.
    let late = St {
        confirm_by: now_s() - 1,
        ..load(&rig.opts, SESSION).expect("state")
    };
    save(&rig.opts, SESSION, &late);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done:model-unconfirmed"]
    );
    assert!(said[0].is_act());
    assert_eq!(
        (said[0].pid, said[0].to.as_str()),
        (rig.sf.pid, "2.1.282(managed)")
    );
    assert_eq!(
        rig.details("done:model-unconfirmed"),
        [
            "claude restarted on 2.1.282 · model unconfirmed (it ran claude-opus-5-5 before the \
          restart)"
        ]
    );
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "said once, never re-asked");
    assert!(!saved.confirming());
    assert_eq!(confirmations(&rig.opts), []);
    assert_eq!(rig.typed().len(), 1);
}

/// A restart an older aterm began took no mark: nothing past it can be told to
/// be the resumed session's, so the model is unconfirmed at once — never read
/// from the start of the conversation, and never left pending.
#[cfg(unix)]
#[test]
fn a_restart_without_a_mark_is_unconfirmed_at_once() {
    let before = [turn_by("claude-opus-5-5", "ATERM-UPGRADE-READY-0badf00d")];
    let rig = Rig::new("model-no-mark", &before);
    rig.append(&[turn_by("claude-opus-5-5", "Resumed.")]);
    let mut st = St {
        mark: 0,
        ..rig.st.clone()
    };
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:model-unconfirmed");
    assert!(!st.confirming());
    assert_eq!(rig.typed().len(), 1);
}

/// THE MODEL BEFORE is read from the tail that proved the READY answer, at
/// the restart itself: a sweep that reaches the restart records the model the
/// answer ran and where that read ended, even when its last look then waits
/// (the next restart takes both again). The READY answer is not the last row
/// here — Claude's own `<synthetic>` row follows it and names no model.
#[cfg(unix)]
#[test]
fn a_restart_records_the_model_its_ready_answer_ran_and_where_that_read_ended() {
    let dir = scratch("model-before");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // An argv a resume carries (the filter alone: a positional, dropped).
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let path = project.join(format!("{SESSION}.jsonl"));
    let body = [
        turn_by("claude-fable-5-1", "Earlier."),
        turn_by("claude-opus-5-5", &format!("Saved.\\n{marker}")),
        r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#.to_string(),
    ]
    .join("\n")
        + "\n";
    std::fs::write(&path, &body).expect("transcript");
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".to_string())];
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    // The launch named a model: the relaunch keeps it, and the restart says so.
    let mut args = atpkg::caller_shell::process_args(pid).expect("agent argv");
    args.argv
        .extend(["--model".to_string(), "claude-sonnet-5".to_string()]);
    // Foreground for the first three job reads — the visit's two and the
    // restart's own — then suspended: the restart's last look waits.
    let files = session_files(&opts.home);
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &t,
        &newer(),
        &Script::new(shell, 3, None),
        Some(&args),
        None,
    );
    let saved = load(&opts, SESSION).expect("state");
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(r.step, "wait:changed", "the restart was reached: {r:?}");
    assert_eq!(saved.model_before, "claude-opus-5-5");
    assert_eq!(saved.mark, body.len() as u64, "the read ended at the end");
    assert_eq!(saved.launch_model, "claude-sonnet-5", "the kept --model");
    assert!(
        matches!(saved.phase, Phase::Announced { .. }),
        "never signalled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// TYPED ONCE: the sweep that types the continuation can die before the
/// resumed session has answered — the window quits or hands itself over, a
/// hand-run `aterm harness upgrade` is interrupted — and what it leaves on
/// disk must not read as a restart still in flight, or the next sweep carries
/// on again and the agent is told twice. Read here the moment the continuation
/// has been typed, while the resumed session has not answered.
#[cfg(unix)]
#[test]
fn a_sweep_that_dies_after_the_continuation_never_types_it_again() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("continued-once", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    // What the disk holds 300 ms after the continuation was typed.
    let snapshot = {
        let (asked, opts) = (std::sync::Arc::clone(&rig.asked), rig.opts.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while turns(&asked) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(300));
            load(&opts, SESSION).expect("state")
        })
    };
    let mut st = rig.st.clone();
    let _ = rig.carry_on_from(&mut st, Duration::from_secs(2));
    let on_disk = snapshot.join().expect("snapshot");
    assert_eq!(rig.typed().len(), 1, "one continuation: {:?}", rig.typed());
    // A sweep resumed from that disk: a restart in flight is carried on.
    let mut resumed = on_disk.clone();
    if resumed.in_flight() {
        let _ = rig.carry_on_from(&mut resumed, Duration::from_millis(300));
    }
    assert_eq!(
        rig.typed().len(),
        1,
        "state on disk once the continuation was typed is {:?}; a sweep resumed from it typed \
         {} more continuation(s)",
        on_disk.phase,
        rig.typed().len() - 1
    );
    // What that sweep still owed — the model — the next one says.
    rig.append(&[turn_by("claude-opus-5-5", "Resumed.")]);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done"]
    );
    assert_eq!(
        rig.details("done"),
        ["claude restarted on 2.1.282 · model claude-opus-5-5"]
    );
}

/// THE VISIT DOES NOT WAIT OUT THE ANSWER: a sweep runs its visits one after
/// another under one lock, and the orphan pass that relaunches a restart left
/// exiting runs after all of them, so every second one visit blocks ages that
/// restart toward [`STALE_S`] — past it, the agent is never relaunched and sits
/// dead at a shell prompt. The resumed session's first answer can take the
/// whole bound (a long first thought, retries, a usage limit that writes only
/// `<synthetic>` rows); the visit that types the continuation must not wait
/// for it.
#[cfg(unix)]
#[test]
fn the_sweep_that_types_the_continuation_does_not_wait_for_the_answer() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("continued-no-wait", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let started = Instant::now();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    let blocked = started.elapsed();
    assert!(
        blocked < Duration::from_secs(20),
        "the visit blocked {blocked:?} for an answer that had not come (STALE_S {STALE_S}s)"
    );
    assert_eq!(rig.typed().len(), 1);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(r.is_act(), "the continuation typed is said");
    assert_eq!(rig.details("continued").len(), 1);
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "never in flight again");
    assert!(saved.confirming());
    // The next sweep, before the answer: nothing to say, still owed.
    assert_eq!(confirmations(&rig.opts), []);
    // The answer is written — past the old build's one more row, which is not
    // it — and the sweep after says the model.
    let old_build = turn_by("claude-opus-5", "One more.").replace("2.1.282", "2.1.281");
    rig.append(&[old_build, turn_by("claude-opus-5-5", "Resumed.")]);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done"]
    );
    assert_eq!(
        rig.details("done"),
        ["claude restarted on 2.1.282 · model claude-opus-5-5"]
    );
    assert!(!load(&rig.opts, SESSION).expect("state").confirming());
    assert_eq!(confirmations(&rig.opts), [], "said once");
    // A sweep for another tab leaves it alone.
    let mut other = load(&rig.opts, SESSION).expect("state");
    other.confirm_by = now_s() + 60;
    save(&rig.opts, SESSION, &other);
    let scoped = Opts {
        only_sid: Some("s-0000000000000000beef".to_string()),
        ..rig.opts.clone()
    };
    assert_eq!(confirmations(&scoped), []);
    assert_eq!(rig.typed().len(), 1);
}

/// THE REASON, END TO END: a session launched with `--model claude-sonnet-5`
/// and moved to opus with `/model` comes back on sonnet — the kept flag did it,
/// and the outcome says so rather than blaming the default.
#[cfg(unix)]
#[test]
fn a_kept_model_flag_is_the_reason_a_launch_with_one_changed_model() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-flag", &before);
    rig.append(&[turn_by("claude-sonnet-5", "Resumed.")]);
    let mut st = St {
        launch_model: "claude-sonnet-5".to_string(),
        ..rig.st.clone()
    };
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done", "{r:?}");
    assert_eq!(
        rig.details("done"),
        [
            "claude restarted on 2.1.282 · model claude-opus-5-5 -> claude-sonnet-5 (the relaunch \
          kept the launch's --model claude-sonnet-5; /model changes it)"
        ]
    );
}

/// ONE OUTCOME PER RESTART: a newer build landing while the last restart's
/// model is still owed waits for that `done` row — a new upgrade starts from a
/// fresh state, and would lose it.
#[cfg(unix)]
#[test]
fn a_new_upgrade_waits_for_the_last_restarts_model() {
    let dir = scratch("confirm-before-next");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let pending = St {
        phase: Phase::Done,
        from: "0.9.0".to_string(),
        to: "1.0.0".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        mark: 1,
        confirm_by: now_s() + 60,
        resumed_on: "1.0.0".to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &pending);
    let r = visit(&opts, &sf, &[], &newer(), &Script::new(1, usize::MAX, None));
    assert_eq!(r.step, "wait:confirming", "{r:?}");
    assert_eq!(
        load(&opts, SESSION),
        Some(pending),
        "the owed outcome is kept"
    );
    // Once it is said, the next build is upgraded to as ever.
    let said = St {
        confirm_by: 0,
        ..load(&opts, SESSION).expect("state")
    };
    save(&opts, SESSION, &said);
    let r = visit(&opts, &sf, &[], &newer(), &Script::new(1, usize::MAX, None));
    assert_ne!(r.step, "wait:confirming", "{r:?}");
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}
