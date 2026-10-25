// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::relaunch::{
    Adopted, AtPrompt, CARRIED_DONE, CAUSE_EXIT, CAUSE_HOST, CONTINUE_FENCE_TRIES, EXIT_LOOK,
    ExitRecord, MODEL_WAIT, NOBODY_ASKED, RELAUNCH_FENCE_TRIES, RelaunchLineError, Restart,
    STALE_S, Snapshot, after_exit, after_host_ended, after_host_ended_from, await_new,
    carry_on_with_tab_probe, continuation_session, exit_fallback, exit_record, fallback_origin,
    follow, line_for, look_at_exit, model_restart, owed, relaunch_by_parent, restart_from, resume,
    resume_as, resumed_prompt, shell_dialect, type_relaunch_line, with_model,
};
use super::super::upgrade::Dialect;
use super::*;
use crate::supervise::ScratchDir;
use aterm_uds::spawnfd::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Where a test plants `session`'s record by hand: the record module keeps its
/// own path private, so the one writer is the only shipped way to it.
fn state_path(opts: &Opts, session: &str) -> std::path::PathBuf {
    state_dir(opts).join(format!("{session}.json"))
}

/// A `text --json trim tail=3` reply from a busy Claude Code tab: the cursor
/// row is absolute (57) and `first` is where the reply's rows start, so the
/// caret sits on reply row 0, column 2.
const SCREEN: &str = r#"{"rows":["❯","────────","  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents"],"cursor":{"row":57,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":60,"cols":144},"seq":232803,"trimmed":0,"first":57}"#;

/// The same tab's `status`, read the same minute.
const STATUS: &str = "OK schema=1 sid=0 subject=%E2%97%90%20Git%20commit%20and%20push \
    subject_source=osc observed=true phase=running since_ms=4431314 outcome=none exit_code=- \
    signal=- detail=- confidence=strong reasons=fg_job,content_activity attribution=adopted \
    fs_consent=unknown conflict=false revision=171 enabled=true hold=0 fabric=absent \
    fabric_rtt_ms=- fabric_link_age_ms=- hand=- level=quiet story=0 seq=232803 \
    hash=7adef2d5e8547a9e";

/// [`file_holders`] (the holder scan) names the holders of a lock held through descriptors: this
/// process and two children, one given the file through a symlink. The children are the ones
/// `lsof -t` names among them and a third, quiet child that holds no lock (the negative
/// control); this process, whose sockets lsof is never let read, is held to the known set.
#[cfg(target_os = "macos")]
#[test]
fn file_holders_are_lsof_s() {
    const NAME: &str = "harness::upgrade_drive::tests::file_holders_are_lsof_s";
    const ALONE: &str = "ATERM_AGENT_FILE_HOLDERS_ALONE";
    const DONE: &str = "ALONE holders compared";
    if std::env::var_os(ALONE).is_none() {
        let out = aterm_uds::deadline::output_until(
            Command::new(std::env::current_exe().expect("the test binary"))
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(ALONE, "1"),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("the test, alone, within the hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains(DONE),
            "{:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let dir = scratch("file-holders");
    let lock = dir.join("writer.lock");
    std::fs::write(&lock, b"").expect("write the lock");
    let link = dir.join("link.lock");
    std::os::unix::fs::symlink(&lock, &link).expect("link the lock");
    let mine = std::fs::File::open(&lock).expect("open the lock");
    let sleeper = |stdin: aterm_uds::spawnfd::Stdio| {
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(stdin)
            .spawn()
            .expect("spawn a sleeper")
    };
    let holder = |path: &std::path::Path| sleeper(std::fs::File::open(path).expect("open").into());
    let mut children = [holder(&lock), holder(&link)];
    let mut quiet = sleeper(aterm_uds::spawnfd::Stdio::null());
    let among = [children[0].id(), children[1].id(), quiet.id()];
    let mut by_children = vec![children[0].id(), children[1].id()];
    by_children.sort_unstable();
    let mut held = vec![std::process::id(), children[0].id(), children[1].id()];
    held.sort_unstable();
    let sorted = |h: Option<Vec<u32>>| {
        h.map(|mut h| {
            h.sort_unstable();
            h
        })
    };
    let scanned = |path: &std::path::Path| {
        let mut scan = aterm_uds::holders::scan_file_holders(path, &[]).expect("scan the file");
        scan.holders.sort_unstable();
        scan
    };
    let captured_scans = [scanned(&lock), scanned(&link)];
    let while_held = [
        sorted(lsof_holders(&lock, &among)),
        sorted(file_holders(&lock)),
        sorted(file_holders(&link)),
        Some(captured_scans[0].holders.clone()),
        Some(captured_scans[1].holders.clone()),
    ];
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(mine);
    let gone = dir.join("gone.lock");
    let still_owned = [quiet.id()];
    let lsof_after = [
        lsof_holders(&lock, &still_owned),
        lsof_holders(&gone, &still_owned),
    ];
    let _ = quiet.kill();
    let _ = quiet.wait();
    assert_eq!(
        while_held[0],
        Some(by_children),
        "lsof, the oracle, among the children"
    );
    let held = Some(held);
    assert_eq!(while_held[1], held, "the scan, by the file's path");
    assert_eq!(while_held[2], held, "the scan, by a link to it");
    assert_eq!(
        while_held[3], held,
        "the scan's own reads, by the file's path"
    );
    assert_eq!(while_held[4], held, "the scan's own reads, by a link to it");
    assert_eq!(
        file_holders(&lock),
        Some(Vec::new()),
        "the scan, once let go"
    );
    let check_complete = |scan: aterm_uds::holders::HolderScan, expected: &[u32], context| {
        assert_eq!(scan.holders, expected, "{context}: positive holders");
        let unreadable = scan.unreadable.clone();
        let answer = complete_file_holders(scan);
        if unreadable.is_empty() {
            assert_eq!(answer.as_deref(), Some(expected), "{context}: complete");
        } else {
            assert_eq!(answer, None, "{context}: unreadable is never free");
            println!("{context}: unknown tables {unreadable:?}");
        }
    };
    for (scan, context) in captured_scans
        .into_iter()
        .zip(["the file's path", "a link to it"])
    {
        check_complete(scan, held.as_deref().expect("held set"), context);
    }
    assert_eq!(lsof_after[0], Some(Vec::new()), "lsof, once let go");
    check_complete(scanned(&lock), &[], "once let go");
    assert_eq!(lsof_after[1], None, "lsof, no such file");
    assert_eq!(file_holders(&gone), None, "the scan, no such file");
    println!("{DONE}");
}

/// [`open_files`] (read in process) names, in descriptor order, the files
/// `lsof -p <pid> -d 0-1023 -Fn` names for a process's descriptors: a child holding one file as
/// its stdin, `/dev/null` as its stdout and another file as its stderr.
#[cfg(target_os = "macos")]
#[test]
fn open_files_are_lsof_s() {
    let lsof_names = |pid: u32| -> Vec<std::path::PathBuf> {
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("lsof"))
            .args([
                "-n",
                "-P",
                "-w",
                "-a",
                "-p",
                &pid.to_string(),
                "-d",
                "0-1023",
                "-Fn",
            ])
            .output()
            .expect("run lsof");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('n').map(std::path::PathBuf::from))
            .collect()
    };
    let dir = scratch("open-files");
    let (input, errors, other) = (dir.join("in"), dir.join("err"), dir.join("other"));
    for file in [&input, &errors, &other] {
        std::fs::write(file, b"").expect("write a file");
    }
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(std::fs::File::open(&input).expect("open"))
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(std::fs::File::create(&errors).expect("create"))
        .spawn()
        .expect("spawn a holder");
    let pid = child.id();
    let ours = open_files(pid).expect("its descriptors");
    let theirs = lsof_names(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(ours, theirs, "lsof, the oracle");
    assert_eq!(ours.len(), 3, "{ours:?}");
    let named = |path: &std::path::Path| {
        let canonical = path.canonicalize().expect("canonical");
        ours.contains(&canonical)
    };
    assert!(named(&input) && named(&errors), "{ours:?}");
    assert!(!named(&other), "a file it does not hold: {ours:?}");
    assert_eq!(open_files(pid), None, "exited");
}

/// Only a complete scan names the holders: an unreadable table leaves them unknown, with or
/// without a holder found.
#[test]
fn only_a_complete_holder_scan_can_establish_the_holder_set() {
    let scan = |holders, unreadable, sealed| aterm_uds::holders::HolderScan {
        holders,
        unreadable,
        sealed,
        scanned: 4,
    };
    assert_eq!(
        complete_file_holders(scan(vec![], vec![], vec![])),
        Some(vec![])
    );
    assert_eq!(
        complete_file_holders(scan(vec![42, 73], vec![], vec![])),
        Some(vec![42, 73])
    );
    assert_eq!(complete_file_holders(scan(vec![], vec![99], vec![])), None);
    assert_eq!(
        complete_file_holders(scan(vec![42], vec![99], vec![])),
        None
    );
    assert_eq!(
        complete_file_holders(scan(vec![42], vec![], vec![77])),
        Some(vec![42]),
        "a sealed table is no holder"
    );
    assert_eq!(
        complete_file_holders(scan(vec![], vec![99], vec![77])),
        None
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_lock_census_never_says_the_lock_is_free() {
    use std::os::unix::process::ExitStatusExt as _;
    let read = |status, stdout: &[u8], stderr: &[u8]| {
        lsof_holder_pids(&std::process::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        })
    };
    assert_eq!(read(0, b"42\n73\n", b""), Some(vec![42, 73]));
    assert_eq!(read(0, b"", b""), Some(Vec::new()));
    assert_eq!(read(1 << 8, b"", b""), Some(Vec::new()));
    let unreadable = [
        read(2 << 8, b"", b"lsof: cannot inspect the file\n"),
        read(2 << 8, b"", b""),
        read(libc::SIGKILL, b"", b""),
        read(1 << 8, b"", b"lsof: cannot inspect the file\n"),
        read(0, b"", b"lsof: incomplete scan\n"),
        read(0, b"42\n", b"lsof: incomplete scan\n"),
        read(0, b"42\nnot-a-pid\n", b""),
        read(0, b"\xff", b""),
        read(1 << 8, b"42\n", b""),
    ];
    assert!(
        unreadable.iter().all(Option::is_none),
        "LSOF_UNKNOWN_MUST_NOT_FREE_THE_LOCK: {unreadable:?}"
    );
}

#[test]
fn a_screen_reply_is_read_with_the_cursor_made_relative_to_its_rows() {
    let s = parse_screen(SCREEN).expect("the measured reply parses");
    assert_eq!(s.rows.len(), 3);
    assert_eq!(s.cursor, Some((0, 2)));
    assert_eq!(s.seq, 232_803);
    assert_eq!(s.first, 57, "the screen row `cell` addresses rows[0] by");
    assert!(upgrade::composer_is_empty(&s.rows, s.cursor, false));
    let above = SCREEN.replace(r#""row":57"#, r#""row":12"#);
    assert_eq!(parse_screen(&above).expect("parses").cursor, None);
    assert!(parse_screen("not json").is_none());
    assert_eq!(s.human, HumanInput::Unknown, "an older host sends none");
    let with = |h: &str| {
        parse_screen(&SCREEN.replace(
            r#""seq":232803"#,
            &format!(r#""seq":232803,"human_ms":{h}"#),
        ))
        .expect("parses")
        .human
    };
    assert_eq!(with("null"), HumanInput::Never);
    assert_eq!(
        with(r#"null,"unkeyed_ms":9000"#),
        HumanInput::NeverFor(9000),
        "a never that says since when"
    );
    assert_eq!(with("4200"), HumanInput::Ago(4200));
    assert_eq!(with("\"12\""), HumanInput::Unknown);
}

/// A proven self-claim (`supervisor_proven=1`) is its agent's session: no upgrade decision of
/// aterm's ([`due`], [`step`]).
#[test]
fn a_proven_self_claim_is_read_off_status_and_nothing_else_is() {
    let proven =
        "OK schema=1 sid=0 supervisor=codex-alab%40500 supervisor_proven=1 autopilot=cruise";
    assert!(self_claimed_by_status(proven));
    assert!(!self_claimed_by_status(
        &proven.replace("supervisor_proven=1", "supervisor_proven=0")
    ));
    assert!(!self_claimed_by_status(
        "OK schema=1 sid=0 supervisor=codex-alab%40500 autopilot=cruise"
    ));
    assert!(!self_claimed_by_status("ERR supervisor_proven=1"));
    assert!(!self_claimed_by_status("OKAY supervisor_proven=1"));
    assert!(!self_claimed_by_status("OKsuffix supervisor_proven=1"));
    assert!(!self_claimed_by_status("OK detail=supervisor_proven%3D1"));
    assert_eq!(SKIP_SELF_CLAIMED, "skip:self-claimed");
    assert!(
        !Report {
            pid: 0,
            tab: "s-x".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: SKIP_SELF_CLAIMED.to_string(),
        }
        .is_act()
    );
}

/// A person at the keyboard wins: a tab a person typed into within `[harness] human_grace_s` is
/// held like a hand on it.
#[test]
fn a_tab_is_held_by_a_halt_a_hand_a_person_or_an_unreadable_status() {
    assert!(!held_by_status(STATUS, 120));
    assert!(held_by_status(&STATUS.replace("hold=0", "hold=1"), 120));
    assert!(held_by_status(
        &STATUS.replace("hand=-", "hand=turn:7"),
        120
    ));
    assert!(held_by_status("ERR no such session", 120));
    assert!(held_by_status("", 120));
    let typed = |ms: &str| STATUS.replace("hand=-", &format!("hand=- human_ms={ms}"));
    assert!(held_by_status(&typed("4000"), 120));
    assert!(!held_by_status(&typed("121000"), 120));
    assert!(!held_by_status(&typed("-"), 120));
    assert!(held_by_status(&typed("119999"), 120));
    assert!(
        !held_by_status(&typed("4000"), 0),
        "a zero grace holds nobody"
    );
}

/// The harness's own hand (`hand=lease:aterm-harness@<this pid>`) holds nothing against itself.
#[test]
fn the_harness_own_hand_holds_nothing_against_it_and_unread_input_holds_the_tab() {
    let ours = STATUS.replace("hand=-", &format!("hand=lease:{}", hand_name()));
    assert!(our_hand(&format!("lease:{}", hand_name())));
    assert!(!held_by_status(&ours, 120), "{ours}");
    assert!(!crate::harness::relaunch::driver_hand(&ours));
    assert!(!crate::harness::relaunch::held_by_someone(&ours));
    for theirs in [
        "hand=lease:orchestrator".to_string(),
        "hand=lease:aterm-harness@1".to_string(),
        "hand=turn:7:manager".to_string(),
    ] {
        let line = STATUS.replace("hand=-", &theirs);
        assert!(held_by_status(&line, 120), "{line}");
        assert!(crate::harness::relaunch::driver_hand(&line), "{line}");
    }
    for input in ["pending", "typeahead", "stalled", "stopped"] {
        let line = STATUS.replace("hand=-", &format!("hand=- input={input}"));
        assert!(held_by_status(&line, 0), "{line}");
    }
    for input in ["clear", "-"] {
        let line = STATUS.replace("hand=-", &format!("hand=- input={input}"));
        assert!(!held_by_status(&line, 0), "{line}");
    }
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
    assert_eq!(parse_host_roster("OKAY", body), None);
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

/// The lanes past Claude Code's, as [`due_after_lanes`] asks them: Codex's answers `codex`,
/// every other lane none (the tab is not its).
fn codex_answers(codex: Option<Due>) -> impl Fn(upgrade::Agent) -> Option<Due> {
    move |agent| match agent {
        upgrade::Agent::Codex => codex,
        upgrade::Agent::Agy | upgrade::Agent::Muse | upgrade::Agent::Gemini => None,
        upgrade::Agent::Claude => panic!("Claude Code's half is not asked as another lane"),
    }
}

/// A tab another lane noted is that lane's alone ([`behind_after_lanes`]).
#[test]
fn a_tab_another_lane_noted_is_never_read_as_claude_codes() {
    let dir = scratch("behind-codex-skips-claude-roster");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let sessions = opts.home.join(".claude/sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    std::fs::write(
        sessions.join("4000001.json"),
        format!(
            r#"{{"pid":4000001,"sessionId":"{SESSION}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("Claude session file");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(4_000_002),
    }];
    let check = |codex: Option<Behind>| {
        SESSION_FILE_SCANS.with(|count| count.set(0));
        let lane_noted = move |agent| match agent {
            upgrade::Agent::Codex => codex,
            upgrade::Agent::Agy | upgrade::Agent::Muse | upgrade::Agent::Gemini => None,
            upgrade::Agent::Claude => panic!("Claude Code's half is not asked as another lane"),
        };
        let behind = behind_after_lanes(&opts, TAB, &tabs, now_s(), &lane_noted);
        (behind, SESSION_FILE_SCANS.with(std::cell::Cell::get))
    };
    for answer in [
        Behind::Noted,
        Behind::Nothing,
        Behind::Unread(UNREAD_PROCESS),
    ] {
        assert_eq!(check(Some(answer)), (answer, 0), "{answer:?}");
    }
    assert_eq!(
        check(None).1,
        1,
        "unknown or Claude foreground still reads Claude's roster"
    );
    assert!(load(&opts, SESSION).is_none(), "nothing minted");
    let _ = std::fs::remove_dir_all(&dir);
}
/// A tab whose agent's own switch is off is never due.
#[test]
fn a_tab_whose_agents_switch_is_off_is_never_due() {
    use aterm_phase::Program;
    use upgrade::Agent::{Claude, Codex};
    let dir = scratch("due-agent-off");
    let base = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let off = |program: Program| Opts {
        agents_off: vec![program],
        ..base.clone()
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(4_000_002),
    }];
    let check =
        |opts: &Opts, codex: Option<Due>| due_after_lanes(opts, &tabs, &codex_answers(codex));
    assert_eq!(check(&off(Program::Codex), Some(Due::Yes)), Due::No);
    assert_eq!(
        check(&off(Program::Claude), Some(Due::Yes)),
        Due::Yes,
        "control: Claude Code's switch is no Codex's"
    );
    assert_eq!(check(&off(Program::Claude), None), Due::No);
    assert_eq!(check(&off(Program::Codex), None), Due::No);

    std::fs::create_dir_all(state_dir(&base)).expect("state");
    for (agent, name, own, other) in [
        (Claude, SESSION.to_string(), Program::Claude, Program::Codex),
        (
            Codex,
            format!("codex-{TAB}"),
            Program::Codex,
            Program::Claude,
        ),
    ] {
        let st = St {
            agent,
            phase: Phase::Exiting { at_s: now_s() },
            pid: dead_pid(),
            shell: std::process::id(),
            tab: TAB.to_string(),
            ..St::default()
        };
        std::fs::write(state_path(&base, &name), st.to_json()).expect("write");
        assert!(in_flight(&off(own)).is_empty(), "{agent:?}");
        assert_eq!(
            due(&off(own)),
            Due::Unread(UNREAD_SOCKET),
            "{agent:?}: the restart is no answer; the roster decides"
        );
        assert_eq!(in_flight(&off(other)), [name.as_str()], "{agent:?}");
        assert_eq!(due(&off(other)), Due::Yes, "control: {agent:?}");
        assert_eq!(due(&base), Due::Yes, "control: {agent:?}");
        std::fs::remove_file(state_path(&base, &name)).expect("rm");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch-only lane's record is its lane's, never the upgrade's.
#[test]
fn a_relaunch_only_lanes_record_is_its_lanes_never_the_upgrades() {
    use super::super::relaunch::restarted;
    use upgrade::Agent::{Agy, Claude, Codex, Gemini, Muse};
    let dir = scratch("no-lane-record");
    let base = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&base)).expect("state");
    let pid = dead_pid();
    let record = |agent| St {
        agent,
        phase: Phase::Exiting { at_s: now_s() },
        pid,
        shell: std::process::id(),
        tab: TAB.to_string(),
        ..St::default()
    };
    for (agent, name) in [
        (Claude, SESSION.to_string()),
        (Codex, format!("codex-{TAB}")),
        (Muse, format!("muse-{TAB}")),
        (Agy, format!("agy-{TAB}")),
    ] {
        assert!(lane_on(&base, agent), "control: {agent:?}");
        let path = state_path(&base, &name);
        std::fs::write(&path, record(agent).to_json()).expect("write");
        assert_eq!(
            load(&base, &name).map(|st| st.agent),
            Some(agent),
            "control"
        );
        assert_eq!(in_flight(&base), [name.as_str()], "control: {agent:?}");
        assert_eq!(in_flight_agent(&base), Some(agent), "control: {agent:?}");
        assert_eq!(due(&base), Due::Yes, "control: {agent:?}");
        std::fs::remove_file(&path).expect("rm");
    }
    for lane in [Muse, Agy] {
        let name = format!("{}-{TAB}", lane.word());
        std::fs::write(state_path(&base, &name), record(lane).to_json()).expect("write");
        assert!(
            restarted(&base, lane, Some(pid)),
            "{lane:?}: the quiet restart's own exit"
        );
        assert!(
            !restarted(&base, Claude, Some(pid)),
            "{lane:?}: never read as Claude Code's"
        );
        let crash = St {
            cause: super::super::relaunch::CAUSE_EXIT.to_string(),
            ..record(lane)
        };
        std::fs::write(state_path(&base, &name), crash.to_json()).expect("write");
        assert!(
            !restarted(&base, lane, Some(pid)),
            "{lane:?}: a crash relaunch's record"
        );
        assert_eq!(
            in_flight(&base),
            [name.as_str()],
            "{lane:?}: still its lane's in flight"
        );
        std::fs::remove_file(state_path(&base, &name)).expect("rm");
    }
    let lane = Gemini;
    let name = format!("{}-{TAB}", lane.word());
    let path = state_path(&base, &name);
    std::fs::write(&path, record(lane).to_json()).expect("write");
    assert!(!lane_on(&base, lane), "{lane:?}: no upgrade lane");
    assert_eq!(load(&base, &name).map(|st| st.agent), Some(lane));
    assert!(
        in_flight(&base).is_empty(),
        "{lane:?}: the relaunch's, not the upgrade's"
    );
    assert_eq!(due(&base), Due::Unread(UNREAD_SOCKET), "{lane:?}");
    assert!(
        !restarted(&base, lane, Some(pid)) && !restarted(&base, Claude, Some(pid)),
        "{lane:?}"
    );
    std::fs::remove_file(&path).expect("rm");
    std::fs::write(state_path(&base, SESSION), record(Claude).to_json()).expect("write");
    assert!(
        restarted(&base, Claude, Some(pid)),
        "control: the restart's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build whose `--version` timed out decides nothing.
#[test]
fn a_build_whose_version_timed_out_is_unread_never_not_due() {
    let dir = scratch("due-unanswered");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(dir.join("agents")).expect("agents");
    std::fs::write(&twin, "#!/bin/sh\necho 1.0.0\n").expect("the twin");
    let probes = std::cell::Cell::new(0);
    let look = |files: &[SessionFile], group: i64, now: Instant, answer: Probed| {
        due_among(
            &opts,
            files,
            &the_tab(),
            || {
                Targets::read_with(Some(twin.clone()), &opts.home, |exe| {
                    cached_version_with(exe, now, |_| {
                        probes.set(probes.get() + 1);
                        answer.clone()
                    })
                })
            },
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(group),
            |_| Some((10, group, group)),
        )
    };
    let on_it = [record("1.0.0")];
    let t0 = Instant::now();
    assert_eq!(
        look(&on_it, 101, t0, Probed::TimedOut),
        Due::Unread(UNREAD_VERSION),
        "a build that did not answer decides nothing"
    );
    assert_eq!(probes.get(), 1);
    assert_eq!(
        look(&on_it, 202, t0, Probed::TimedOut),
        Due::Unread(UNREAD_RECORD),
        "another tab's agent's record is none of this tab's"
    );
    let answers = Probed::Version(Version::parse("1.0.0").expect("v"));
    assert_eq!(
        look(&on_it, 101, t0 + VERSION_RETRY_FIRST, answers.clone()),
        Due::No,
        "answered at the back-off, and the session is on it"
    );
    assert_eq!(probes.get(), 2, "asked again at the back-off");
    assert_eq!(
        look(&[record("0.9.0")], 101, t0 + VERSION_RETRY_FIRST, answers),
        Due::Yes,
        "answered, and the session is behind it"
    );
    let _ = std::fs::remove_dir_all(&dir);
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
            relaunched_at: 1_790_000_090,
            line_block: Some(12),
            hold_since_s: 1_790_000_060,
            hold_seen_s: 1_790_000_120,
            proc_start: "Mon Sep 21 11:59:00 2026".to_string(),
            own_work_at: 1_790_000_030,
            waits: vec![WaitNote {
                at: 1_790_000_030,
                word: "background".to_string(),
                note: "status=busy holders=3".to_string(),
            }],
            cause: CAUSE_EXIT.to_string(),
            ..St::fresh(&from, &to, Proc::default(), 1_790_000_000)
        };
        assert_eq!(St::from_json(&st.to_json()), Some(st));
    }
    assert_eq!(St::from_json(r#"{"phase":"sideways"}"#), None);
    let old = r#"{"phase":"relaunched","at":7}"#;
    assert_eq!(St::from_json(old).map(|s| s.cause), Some(String::new()));
    assert_eq!(St::from_json(old).map(|s| s.relaunched_at), Some(7));
    let done = r#"{"phase":"done","at":0}"#;
    assert_eq!(St::from_json(done).map(|s| s.relaunched_at), Some(0));
    let written = r#"{"phase":"relaunched","at":7,"relaunched_at":0}"#;
    assert_eq!(St::from_json(written).map(|s| s.relaunched_at), Some(0));
}

/// The record's agent word: none (an older record) or Claude's own name is Claude Code's.
#[test]
fn a_record_names_its_agent_or_is_no_record() {
    let agent =
        |word: &str| St::from_json(&format!(r#"{{"phase":"pending"{word}}}"#)).map(|st| st.agent);
    assert_eq!(agent(""), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"""#), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"claude""#), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"codex""#), Some(upgrade::Agent::Codex));
    assert_eq!(agent(r#","agent":"gemini""#), Some(upgrade::Agent::Gemini));
    assert_eq!(agent(r#","agent":"opencode""#), None);
    assert_eq!(agent(r#","agent":"Codex""#), None);
    assert_eq!(agent(r#","agent":7"#), None);
}

/// One reader for both writers.
#[test]
fn a_state_main_wrote_reads_with_the_owners_fields_at_their_defaults() {
    let main_wrote = r#"{"phase":"announced","why":"","from":"2.1.281","to":"2.1.282","source":"managed","marker":"ATERM-UPGRADE-READY-0badf00d","notice_start":"Mon Sep 21 12:00:00 2026","tab":"s-b5cf2faabac5ce5127bd","line":"","noted":"","model_before":"claude-opus-5","launch_model":"","model_list":"claude-opus-5-5","resumed_on":"","at":1790000060,"asks":1,"salt":1790000000,"last_seq":12,"seq_since":1790000000,"pid":9162,"notice_pid":9162,"shell":9001,"mark":0,"confirm_by":0,"resumed_pid":0}"#;
    let st = St::from_json(main_wrote).expect("main's state reads");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: 1_790_000_060,
            asks: 1,
        }
    );
    assert_eq!(st.model_list, "claude-opus-5-5");
    assert_eq!(st.model_before, "claude-opus-5");
    assert_eq!((st.from.as_str(), st.to.as_str()), ("2.1.281", "2.1.282"));
    assert_eq!(st.notice_pid, 9162);
    assert_eq!(st.prompt, None);
    assert_eq!(st.line_block, None);
    assert_eq!(st.request, Request::None);
    assert!(st.request_tab.is_empty() && st.wait.is_empty());
    assert_eq!(
        st.behind_since(),
        1_790_000_000,
        "the age falls back to salt"
    );
    assert_eq!(St::from_json(&st.to_json()), Some(st));
}

/// `(pid, ppid, comm)` rows as [`table`] reads them, basenames already taken.
fn ps_rows(rows: &[(u32, u32, &str)]) -> Vec<ProcRow> {
    rows.iter()
        .map(|(p, pp, n)| (*p, *pp, (*n).into()))
        .collect()
}

/// Claude Code's own keep-awake is not work.
#[test]
fn the_agents_own_keep_awake_does_not_hold_the_restart() {
    let idle = Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        quiet_s: 60,
        ..Facts::default()
    };
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    let restart = |t: &[ProcRow]| {
        let mut found = background(4163, t);
        found.sort();
        let f = Facts {
            background: found.clone(),
            ..idle.clone()
        };
        (
            found,
            upgrade::gate_restart(&f, true),
            upgrade::next_step(&asked, &f, true, 200),
        )
    };
    let agent = [(4100, 1, "zsh"), (4163, 4100, "2.1.281")];
    let with = |rows: &[(u32, u32, &str)]| ps_rows(&[&agent[..], rows].concat());

    let (found, gate, step) = restart(&with(&[(8931, 4163, "caffeinate")]));
    assert_eq!(
        (gate, step),
        (upgrade::Gate::Go, Step::Terminate),
        "the agent's own keep-awake held the restart as work: {found:?}"
    );
    assert!(found.is_empty(), "{found:?}");

    let (found, gate, step) = restart(&with(&[(4492, 4163, "zsh"), (4494, 4492, "caffeinate")]));
    assert_eq!(found, vec!["caffeinate", "zsh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
    assert_eq!(step, Step::Wait("background"));

    let (found, gate, step) = restart(&with(&[
        (4444, 4163, "caffeinate"),
        (4492, 4163, "zsh"),
        (4494, 4492, "Python"),
    ]));
    assert_eq!(found, vec!["zsh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
    assert_eq!(step, Step::Wait("background"));

    let (found, gate, _) = restart(&with(&[(4444, 4163, "caffeinate"), (4445, 4444, "sh")]));
    assert_eq!(found, vec!["sh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));

    let (found, gate, _) = restart(&with(&[(4300, 4163, "node"), (4301, 4300, "caffeinate")]));
    assert_eq!(found, vec!["caffeinate"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
}

/// The notice names what runs under the agent by its `eval` payload, without the Bash tool's
/// boilerplate.
#[test]
fn the_notice_names_what_runs_under_the_agent_by_its_command() {
    let measured = "/bin/zsh -c source /Users//u/.claude/shell-snapshots/snapshot-zsh-1790024225604-061jsy.sh \
        2>/dev/null || true && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && { \
        \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true \
        && eval 'D=/tmp/wf; until [ \"$(python3 -c \"\\012import json\\012print(sum(1 for l in \
        open('\"'\"'$D/journal.jsonl'\"'\"')))\")\" -ge 6 ]; do sleep 15; done; echo \"CLIPPY_PHASE_DONE\"' \
        < /dev/null && pwd -P >| /tmp/claude-f730-cwd";
    let head = command_head(measured);
    assert!(head.starts_with("D=/tmp/wf; until ["), "{head}");
    assert!(head.contains("-ge 6 ]; do sleep 15; done"), "{head}");
    assert!(
        head.contains("open('$D/journal.jsonl')"),
        "the quoting undone: {head}"
    );
    assert!(
        !head.contains("shell-snapshots") && !head.contains("pwd -P"),
        "{head}"
    );
    assert!(!head.contains("\\012"), "{head}");
    assert_eq!(
        command_head("/opt/bin/watcher --poll 5"),
        "/opt/bin/watcher --poll 5"
    );

    for (etime, secs) in [
        ("00:07", 7),
        ("12:34", 754),
        ("04:10:51", 15_051),
        ("05-04:10:51", 5 * 86_400 + 15_051),
    ] {
        assert_eq!(parse_etime(etime), Some(secs), "{etime}");
    }
    assert_eq!(parse_etime("soon"), None);
    assert_eq!(parse_etime("1:2:3:4"), None);
}

/// The restart's last look fails closed: an empty process table (`ps` gave none) reads as work,
/// never as "nothing runs".
#[test]
fn an_unreadable_process_table_is_never_read_as_nothing_running() {
    assert_eq!(
        background_or_unreadable(4163, &[]),
        vec![UNREADABLE_TABLE.to_string()]
    );
    assert!(background_or_unreadable(4163, &[(1, 0, "launchd".into())]).is_empty());
    assert!(!table().is_empty());
    assert!(!live_background(std::process::id()).contains(&UNREADABLE_TABLE.to_string()));
}

#[test]
fn a_sweep_never_signals_its_own_ancestry() {
    let t = table();
    assert!(is_our_ancestor(std::process::id(), &t));
    if let Some((parent, _, _)) = ids(std::process::id()) {
        assert!(is_our_ancestor(parent, &t));
    }
    let synthetic = vec![(10, 1, "a".into()), (11, 10, "b".into())];
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
        claude_dir: None,
        profile: None,
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
        |_, _| true,
        |pid| {
            args_read.push(pid);
            Some(atpkg::caller_shell::ProcArgs::default())
        },
        |pid| {
            groups_read.push(pid);
            Some(if pid == 1 { 101 } else { 202 })
        },
        |_| Some((10, 101, 101)),
    );
    assert_eq!(found.len(), 1);
    let (sf, _, claim) = found[0].as_ref().expect("claimed");
    assert_eq!(sf.pid, 1);
    assert_eq!(claim.tab, "s-mine");
    assert_eq!(args_read, [1]);
    assert_eq!(groups_read, [1, 2]);

    let found = host_candidates(
        &files[..1],
        &ours,
        |_, _| true,
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 202)),
    );
    let [Err(wait)] = found.as_slice() else {
        panic!("a background job does not own the tab: {found:?}");
    };
    assert_eq!(
        (wait.pid, wait.tab.as_str(), wait.step.as_str()),
        (1, "s-mine", "wait:no-process")
    );
    let found = host_candidates(
        &files[..1],
        &ours,
        |_, _| true,
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

/// A current Claude costs the host tick no `ps` or argv read.
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
        claude_dir: None,
        profile: None,
    };
    let target = |version: &str, source| Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse(version).expect("version"),
        source,
    };
    let mut targets = Targets {
        managed: Some(target("2.1.281", Source::Managed)),
        native: Some(target("2.1.280", Source::Native)),
        unanswered: false,
    };
    let ours = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(101),
    }];
    assert!(!needs_upgrade_inspection(&file, &targets, None));
    let current = host_candidates(
        std::slice::from_ref(&file),
        &ours,
        |sf, _| needs_upgrade_inspection(sf, &targets, None),
        |_| panic!("current build must not read argv"),
        |_| Some(101),
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
        |sf, _| needs_upgrade_inspection(sf, &targets, None),
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 101)),
    );
    assert!(
        matches!(newer.as_slice(), [Ok(_)]),
        "new build keeps the ownership path"
    );

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
    let dir = scratch("orphan");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    let dead = dead_pid();
    let opts = Opts {
        sock: None,
        dry_run: true,
        ..drive(&dir)
    };
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
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
    std::fs::write(
        dir.join(format!("home/.claude/sessions/{dead}.json")),
        format!(
            r#"{{"pid":{dead},"sessionId":"{session}","cwd":"/","version":"2.1.280","status":"idle","statusUpdatedAt":1,"procStart":"gone","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("the leftover session file");
    let orphaned = |o: &Opts| -> Vec<Report> {
        sweep(o)
            .into_iter()
            .filter(|r| r.step != "wait:stale-file")
            .collect()
    };
    let got = orphaned(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].session, session);
    assert_eq!(got[0].step, "would-resume:exiting");
    assert_eq!(
        load(&opts, session).map(|s| s.phase),
        Some(Phase::Exiting { at_s })
    );
    let only = Opts {
        only_sid: Some("s-0".to_string()),
        ..opts.clone()
    };
    let other = sweep(&only);
    assert!(
        other
            .iter()
            .all(|r| r.session != session || r.step == "wait:stale-file"),
        "{other:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sweep leaves out the lane of an agent whose own switch is off ([`Opts::agents_off`],
/// [`lanes`]).
#[test]
fn a_sweep_leaves_out_the_lane_of_an_agent_whose_switch_is_off() {
    use aterm_phase::Program;
    use upgrade::Agent::{Agy, Claude, Codex, Muse};
    let dir = scratch("lanes-off");
    let base = drive(&dir);
    let ran = std::cell::RefCell::new(Vec::new());
    let pass = |agent: upgrade::Agent| {
        ran.borrow_mut().push(agent);
        vec![Report {
            pid: 0,
            tab: "-".to_string(),
            session: agent.product().to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: "current".to_string(),
        }]
    };
    let swept = |agents_off: Vec<Program>| {
        ran.borrow_mut().clear();
        let opts = Opts {
            agents_off,
            ..base.clone()
        };
        let reports = lanes(&opts, None, &pass);
        let said: Vec<String> = reports.into_iter().map(|r| r.session).collect();
        (ran.borrow().clone(), said)
    };
    assert_eq!(
        swept(Vec::new()),
        (
            vec![Codex, Agy, Muse, Claude],
            vec![
                "Claude Code".into(),
                "Codex".into(),
                "Antigravity CLI".into(),
                "Muse Code".into()
            ]
        )
    );
    assert_eq!(
        swept(vec![Program::Claude]),
        (
            vec![Codex, Agy, Muse],
            vec!["Codex".into(), "Antigravity CLI".into(), "Muse Code".into()]
        )
    );
    assert_eq!(
        swept(vec![Program::Codex]),
        (
            vec![Agy, Muse, Claude],
            vec![
                "Claude Code".into(),
                "Antigravity CLI".into(),
                "Muse Code".into()
            ]
        )
    );
    assert_eq!(
        swept(vec![Program::Muse]),
        (
            vec![Codex, Agy, Claude],
            vec![
                "Claude Code".into(),
                "Codex".into(),
                "Antigravity CLI".into()
            ]
        )
    );
    assert_eq!(
        swept(vec![Program::Agy]),
        (
            vec![Codex, Muse, Claude],
            vec!["Claude Code".into(), "Codex".into(), "Muse Code".into()]
        )
    );
    assert_eq!(
        swept(vec![
            Program::Claude,
            Program::Codex,
            Program::Agy,
            Program::Muse
        ]),
        (Vec::new(), Vec::new())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The window's step for a tab whose foreground job is a Codex ([`lanes`]: `only_sid` and its own
/// roster, the Codex pass reporting that tab) runs the Codex lane alone.
#[test]
fn a_tab_another_lane_reports_is_that_lanes_alone() {
    use upgrade::Agent::{Agy, Claude, Codex, Muse};
    let dir = scratch("lanes-others-tab");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let roster = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(4_000_002),
    }];
    let report = |tab: &str, agent: upgrade::Agent| Report {
        pid: 0,
        tab: tab.to_string(),
        session: agent.product().to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "current".to_string(),
    };
    let ran = std::cell::RefCell::new(Vec::new());
    let swept = |codex_tab: &str, live_tabs: Option<&[LiveTab]>| {
        ran.borrow_mut().clear();
        let pass = |agent: upgrade::Agent| {
            ran.borrow_mut().push(agent);
            match agent {
                Codex => vec![report(codex_tab, Codex)],
                Claude => vec![report("-", Claude)],
                Muse | Agy => Vec::new(),
                upgrade::Agent::Gemini => panic!("{agent:?} has no upgrade lane"),
            }
        };
        let said: Vec<String> = lanes(&opts, live_tabs, &pass)
            .into_iter()
            .map(|r| r.session)
            .collect();
        (ran.borrow().clone(), said)
    };
    assert_eq!(
        swept(TAB, Some(&roster)),
        (vec![Codex, Agy, Muse], vec!["Codex".into()]),
        "the Codex lane's tab"
    );
    let both = (
        vec![Codex, Agy, Muse, Claude],
        vec!["Claude Code".into(), "Codex".into()],
    );
    assert_eq!(swept("s-other", Some(&roster)), both, "another tab's Codex");
    assert_eq!(swept(TAB, None), both, "the CLI's sweep");
    let _ = std::fs::remove_dir_all(&dir);
}

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("upgrade", name)
}

/// The home's model caches are not read while two or more accounts are listed
/// ([`Opts::several_logins`]).
#[test]
fn the_homes_model_caches_are_not_read_under_several_logins() {
    let dir = scratch("model-evidence-logins");
    let home = dir.join("home");
    let catalog = home.join(".claude/cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(
        catalog.join("0000-cc.json"),
        r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"}]}}}"#,
    )
    .expect("served");
    std::fs::write(
        home.join(".claude.json"),
        r#"{"modelAccessCache":[{"apiName":"claude-opus-5","entitled":false}],
            "additionalModelOptionsCache":[{"value":"claude-fable-5[1m]","disabled":true}],
            "cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-5-launch","text":"t","requiresModel":"claude-opus-5-5"}]}}"#,
    )
    .expect("the profile");
    let one = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&one);
    assert!(
        !one.several_logins,
        "control: the packaged state is one login"
    );
    let read = model_evidence(&one, &Records::of(&one.home, None));
    assert_eq!(read.denied, ["claude-opus-5"]);
    assert_eq!(read.disabled_options, ["claude-fable-5"]);
    assert_eq!(read.served.as_ref().map(Vec::len), Some(1));
    assert_eq!(read.announcements.len(), 1);
    assert!(
        !models::availability("claude-opus-5", "9.9.9", &read, None).ok(),
        "control: the cache's denial is read"
    );

    let several = Opts {
        several_logins: true,
        instances: Instances::Listed(Vec::new()),
        ..one.clone()
    };
    let unknown = model_evidence(&several, &Records::of(&several.home, None));
    assert_eq!(
        unknown,
        models::Evidence {
            credits_billed: vec!["fable".to_string()],
            ..models::Evidence::default()
        },
        "nothing of the home's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model the account bills to usage credits is offered to no upgrade under
/// `[harness] usage_credits = false`.
#[test]
fn a_model_billed_to_usage_credits_is_offered_to_no_upgrade_under_the_key_off() {
    let dir = scratch("model-evidence-credits");
    let home = dir.join("home");
    let catalog = home.join(".claude/cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(
        catalog.join("0000-cc.json"),
        r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},
            {"id":"claude-fable-5-1","section":"current"}]}}}"#,
    )
    .expect("served");
    let profile = |tiers: &str| {
        std::fs::write(
            home.join(".claude.json"),
            format!(
                r#"{{"oauthAccount":{{"organizationType":"claude_max","seatTier":"standard",
                    "userRateLimitTier":"default_claude_max_5x"}},
                    "cachedGrowthBookFeatures":{{"tengu_saffron_credits_only_tiers":[{tiers}],
                    "tengu_saffron_lattice":{{}}}}}}"#
            ),
        )
        .expect("the profile");
    };
    let off = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    assert!(!off.usage_credits, "the packaged state");
    let on = Opts {
        usage_credits: true,
        ..off.clone()
    };
    let list = models::Priority::seed(0);
    let offer = |opts: &Opts| {
        let ev = model_evidence(opts, &Records::of(&opts.home, None));
        models::offered(&list, "9.9.9", &ev, None, None)
    };
    profile(r#""max""#);
    let held = offer(&off);
    assert_eq!(held, ["claude-opus-5-5"]);
    assert_eq!(
        models::family_successor("claude-fable-5", false, &held),
        None
    );
    let today = offer(&on);
    assert_eq!(today, ["claude-opus-5-5", "claude-fable-5-1"]);
    assert_eq!(
        models::family_successor("claude-fable-5", false, &today).as_deref(),
        Some("claude-fable-5-1")
    );
    profile(r#""enterprise""#);
    assert_eq!(offer(&off), today);
    use super::super::profile::Selection;
    let records = Records::of(&off.home, None);
    let context = |selection| super::super::profile::Context {
        plain: home.join(".claude.json"),
        records: home.join(".claude"),
        selection,
        home: Some(home.to_string_lossy().into_owned()),
        config: None,
        login: None,
    };
    let legacy = home.join(".claude/.config.json");
    let custom = home.join(".claude-custom-oauth.json");
    let included = std::fs::read(home.join(".claude.json")).expect("included bytes");
    let mut cached = ModelCtx::none();
    cached.list = list.clone();
    cached.versions.0 = "9.9.9".to_string();
    cached.offer(&model_evidence(&off, &records));
    assert_eq!(cached.offered, today);
    profile(r#""max""#);
    let billed = std::fs::read(home.join(".claude.json")).expect("billed bytes");
    let known_billed = Some(vec!["fable".to_string()]);
    let known_included = Some(Vec::<String>::new());

    std::fs::write(home.join(".claude.json"), &included).expect("stale plain Included");
    std::fs::write(&legacy, &billed).expect("legacy billed account");
    assert_eq!(
        credits_billed_for(&off, Some(&context(Selection::Standard))),
        None
    );
    assert!(
        cached
            .for_profile(&off, &records, Some(&context(Selection::Standard)))
            .offered
            .is_empty()
    );
    std::fs::write(&legacy, &included).expect("legacy Included");
    std::fs::write(home.join(".claude.json"), &billed).expect("plain billed");
    assert_eq!(
        credits_billed_for(&off, Some(&context(Selection::Standard))),
        None
    );
    std::fs::write(&legacy, "").expect("empty legacy beside a plain profile");
    std::fs::write(home.join(".claude.json"), &included).expect("plain Included");
    assert_eq!(
        credits_billed_for(&off, Some(&context(Selection::Standard))),
        known_included
    );
    std::fs::remove_file(&legacy).expect("remove legacy");

    let standard = Selection::of_exec_env(&["HOME=/invented".to_string()]);
    let selected = Selection::of_exec_env(&[
        "HOME=/invented".to_string(),
        "CLAUDE_CODE_CUSTOM_OAUTH_URL=https://auth.example.invalid".to_string(),
    ]);
    assert_eq!(standard, Selection::Standard);
    assert_eq!(selected, Selection::CustomOauth);
    assert_eq!(Selection::of_exec_env(&[]), Selection::Unknown);
    assert_eq!(
        Selection::of_exec_env(&[
            "CLAUDE_CODE_CUSTOM_OAUTH_URL=".to_string(),
            "CLAUDE_CODE_CUSTOM_OAUTH_URL=https://auth.example.invalid".to_string(),
        ]),
        Selection::Unknown
    );
    std::fs::write(&custom, &billed).expect("custom billed");
    assert_eq!(
        credits_billed_for(&off, Some(&context(standard))),
        known_included
    );
    assert_eq!(
        credits_billed_for(&off, Some(&context(selected))),
        known_billed
    );
    assert_eq!(
        credits_billed_for(&off, Some(&context(Selection::Unknown))),
        None
    );
    assert!(
        cached
            .for_profile(&off, &records, Some(&context(Selection::Unknown)))
            .offered
            .is_empty()
    );
    assert_eq!(
        credits_billed_for(&on, Some(&context(Selection::Unknown))),
        known_included
    );

    assert_eq!(
        cached
            .for_profile(&off, &records, Some(&context(selected)))
            .offered,
        ["claude-opus-5-5"]
    );
    std::fs::write(&custom, &included).expect("custom Included");
    std::fs::write(home.join(".claude.json"), &billed).expect("plain billed again");
    assert_eq!(
        credits_billed_for(&off, Some(&context(selected))),
        known_included
    );
    assert_eq!(
        cached
            .for_profile(&off, &records, Some(&context(selected)))
            .offered,
        today
    );
    std::fs::remove_file(&custom).expect("remove custom");
    assert_eq!(credits_billed_for(&off, Some(&context(selected))), None);

    #[cfg(unix)]
    {
        std::fs::write(home.join(".claude.json"), &included).expect("plain Included again");
        std::os::unix::fs::symlink(".config.json", &legacy).expect("owned legacy cycle");
        assert_eq!(credits_billed_for(&off, Some(&context(standard))), None);
        assert_eq!(
            credits_billed_for(&on, Some(&context(standard))),
            known_included
        );
        std::fs::remove_file(&legacy).expect("remove owned cycle");
    }
    let other = home.join("other-home");
    let record_cwd = home.join("record-cwd");
    std::fs::create_dir_all(&other).expect("owned other HOME");
    std::fs::create_dir_all(&record_cwd).expect("owned record cwd");
    std::fs::write(other.join(".claude.json"), &billed).expect("own HOME billed");
    std::fs::write(home.join(".claude.json"), &included).expect("supervisor plain Included");
    let env = vec![
        format!("HOME={}", other.display()),
        "CLAUDE_CONFIG_DIR=".to_string(),
    ];
    let captured = super::super::profile::Context::of_exec_env(&home, &record_cwd, &env)
        .expect("captured context");
    assert_eq!(captured.plain, other.join(".claude.json"));
    assert_eq!(captured.records, record_cwd);
    let old_identity = Records::at(&home, HomeVars::config(Some("")), || {
        Some(record_cwd.clone())
    });
    assert_eq!(old_identity.profile(&home), Some(home.join(".claude.json")));
    assert_ne!(old_identity.profile(&home), Some(captured.plain.clone()));
    assert_eq!(credits_billed_for(&off, Some(&captured)), known_billed);
    assert_eq!(
        credits_billed_for(&off, None),
        None,
        "a restored/unread snapshot grants nothing"
    );
    assert_eq!(
        credits_billed_for(&on, None),
        known_included,
        "explicit opt-in control"
    );
    profile(r#""max""#);
    let several = Opts {
        several_logins: true,
        ..off.clone()
    };
    assert_eq!(credits_billed_for(&several, Some(&context(standard))), None);
    std::fs::remove_file(home.join(".claude.json")).expect("the profile gone");
    assert_eq!(credits_billed_for(&off, Some(&context(standard))), None);
    assert_eq!(
        credits_billed_for(&on, Some(&context(standard))),
        known_included
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_sweeper_does_nothing_while_the_first_holds_the_lock() {
    let dir = scratch("lock");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let first = sweep_lock(&opts).expect("free").expect("a real run locks");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].step, "busy:another-sweep");
    assert!(!got[0].is_act());
    let copy = first.0.try_clone().expect("a copy of the descriptor");
    drop(first);
    let after = sweep(&opts);
    assert!(
        after.is_empty(),
        "no sessions, nothing to report: {after:?}"
    );
    drop(copy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_that_cannot_be_carried_is_refused_before_the_agent_is_asked_to_wind_down() {
    let dir = scratch("preflight");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
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
    let planned = |argv: &[String]| {
        line_for(
            Dialect::Zsh,
            Some(hook),
            None,
            exe,
            argv,
            Some(session),
            None,
            None,
            &[],
        )
        .map(|line| Plan {
            shell: 1,
            dialect: Dialect::Zsh,
            line,
        })
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

    let prompt = "p".repeat(upgrade::MAX_LINE);
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--append-system-prompt", &prompt])),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "refused:line");
    assert!(got.is_act(), "a refusal is reported, not a wait");
    assert!(
        matches!(&st.phase, Phase::Failed(w) if w.starts_with("line:")),
        "{:?}",
        st.phase
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(ledger.contains(r#""step":"refused:line""#), "{ledger}");

    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        Err(NoPlan::Wait("shell-dialect")),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "wait:shell-dialect");
    assert_eq!(st.phase, Phase::Pending);

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
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "would-refuse:line");
    assert!(!got.is_act());
    assert_eq!(st.phase, Phase::Pending);
    assert!(!state_dir(&dry).exists());

    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--model", "opus"])),
        100,
        1,
        None,
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

/// The drain's bound over the real state.
#[test]
fn a_drain_a_person_holds_past_its_bound_voids_the_answer_and_asks_again() {
    let dir = scratch("drain-bound");
    let opts = drive(&dir);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let tail = format!(
        r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    );
    let mut st = St {
        phase: Phase::Announced { at_s: 100, asks: 1 },
        marker: marker.to_string(),
        ..St::default()
    };
    let report = || Report {
        pid: 9162,
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        session: "03396a15-856e-4f1b-8174-ae9a3e4b369f".to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: String::new(),
    };
    let boxed = Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        quiet_s: 60,
        approval_box: true,
        hold_s: upgrade::HOLD_S,
        ..Facts::default()
    };
    let expiry = 100 + upgrade::DRAIN_S;

    assert!(
        answered(&st, Some(&tail)),
        "the answer counts before the bound"
    );
    let step = upgrade::next_step(&st.phase, &boxed, answered(&st, Some(&tail)), expiry);
    assert_eq!(step, Step::Void("box"));

    let dry = Opts {
        dry_run: true,
        state: dir.join("dry-state"),
        ..opts.clone()
    };
    let got = drain_expired(&dry, report(), &mut st, "box", &boxed, expiry);
    assert_eq!(got.step, "would-void:box");
    assert!(!got.is_act());
    assert!(answered(&st, Some(&tail)), "a dry run forgets nothing");
    assert!(st.release.is_empty(), "and owes nothing");
    assert!(!state_dir(&dry).exists());

    let got = drain_expired(&opts, report(), &mut st, "box", &boxed, expiry);
    assert_eq!(got.step, "drain-expired:box");
    assert!(got.is_act(), "the host logs it");
    assert_eq!(after(&got.step, 0), After::NextIdle, "the release is owed");
    assert!(
        !answered(&st, Some(&tail)),
        "the voided answer can never end the agent later"
    );
    assert_eq!(st.release, "void");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: expiry,
            asks: 1
        }
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(ledger.contains(r#""step":"drain-expired:box""#), "{ledger}");
    let codex_dir = scratch("drain-bound-codex");
    let codex_opts = drive(&codex_dir);
    let mut codex = St {
        phase: Phase::Announced { at_s: 100, asks: 1 },
        marker: marker.to_string(),
        agent: upgrade::Agent::Codex,
        ..St::default()
    };
    drain_expired(&codex_opts, report(), &mut codex, "box", &boxed, expiry);
    assert!(codex.release.is_empty());
    let _ = std::fs::remove_dir_all(&codex_dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The tab every stand-in agent names: no instance has it, and every test here
/// points its sweep at a socket that does not exist, so nothing can be typed.
const TAB: &str = "s-feedfacefeedfacefeed";

/// A conversation id the stand-in agents hold.
const SESSION: &str = "0badf00d-1111-2222-3333-444455556666";

/// A person's prompt: it gives a test's transcript a task (one holding only the harness's own
/// turns is restarted afresh, never announced to, and carried on with nothing).
const ASKED: &str = r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#;

/// The stand-in agent's argv: this test binary, running only [`park_when_asked`].
const PARK: [&str; 2] = ["harness::upgrade_drive::tests::park_when_asked", "--exact"];

/// What tells [`park_when_asked`] to park.
const PARK_ENV: &str = "UPGRADE_DRIVE_TEST_PARK";

/// The stand-in agents' body.
#[test]
fn park_when_asked() {
    if std::env::var_os(PARK_ENV).is_none() {
        return;
    }
    let until = std::time::Instant::now() + Duration::from_secs(600);
    #[cfg(unix)]
    {
        // SAFETY: getppid takes nothing and cannot fail.
        let parent = unsafe { libc::getppid() };
        // SAFETY: as above.
        while unsafe { libc::getppid() } == parent && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(unix))]
    std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
}

/// A stand-in agent in [`TAB`]: this test binary, parked.
fn parked() -> Command {
    let mut c = parked_bare();
    c.arg(PARK[1]);
    c
}

/// [`parked`] without `--exact`, which no Claude Code launch carries: an argv
/// the relaunch plan can carry into a resume.
fn parked_bare() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .env_remove(crate::harness::accounts::LOGIN_ENV)
        .env_remove("HOME")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    c
}

/// Wait until `pid` runs this test binary (a fork not yet exec'd still reads
/// as its parent).
fn wait_exec(pid: u32) {
    let me = std::env::current_exe().expect("exe");
    for _ in 0..3_000 {
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
    register_in(&home.join(".claude"), pid, session)
}

/// [`register`] in the Claude Code directory `claude_dir` itself — where a
/// Claude Code started with `CLAUDE_CONFIG_DIR=<claude_dir>` files it.
fn register_in(claude_dir: &Path, pid: u32, session: &str) -> SessionFile {
    register_as(claude_dir, pid, session, "1.0.0")
}

/// [`register_in`], the process running build `version` (`unknown`: one
/// nobody can compare, which the host looks at as it does a behind one).
fn register_as(claude_dir: &Path, pid: u32, session: &str, version: &str) -> SessionFile {
    let start = kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(claude_dir.join("sessions")).expect("sessions");
    std::fs::write(
        claude_dir.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"{version}","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    session_file_in(claude_dir, pid).expect("the file parses")
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
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#
        ),
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
    let model = aterm_models_harness::harness_upgrade_notice_owner_model();
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
        continuation_session(&Records::of(&opts.home, None), sf_b.pid, SESSION),
        Some(sf_b.clone())
    );
    let other = "0badf00d-1111-2222-3333-444455556667";
    register(&opts.home, sf_b.pid, other);
    assert!(continuation_session(&Records::of(&opts.home, None), sf_b.pid, SESSION).is_none());
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(dir);
}

/// A notice whose process is gone is asked afresh of the live holder B (in a's tab, in another, or
/// under a's pid recycled).
#[cfg(unix)]
#[test]
fn a_notice_whose_process_is_gone_is_asked_afresh_of_the_live_holder() {
    const OTHER: &str = "0badf00d-1111-2222-3333-444455556667";
    let model = aterm_models_harness::harness_upgrade_notice_owner_model();
    let nine = Version::parse("9.9.9").expect("version");
    for (name, b_tab, recycled, holder) in [
        ("gone-same-tab", "s-a", false, "Resume"),
        ("gone-other-tab", "s-b", false, "OtherTab"),
        ("gone-recycled", "s-a", true, "Recycled"),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_of(&dir, b_tab);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let now = now_s();
        let salt = now - 7_200;
        let old = upgrade::ready_marker(SESSION, &nine, salt + 1);
        let project = opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!(
                r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{old}"}}]}}}}"#
            ),
        )
        .expect("A's READY");
        let mut b = parked_bare()
            .env("ATERM_PARENT_SESSION_ID", b_tab)
            .spawn()
            .expect("B");
        wait_exec(b.id());
        let sf_b = register(&opts.home, b.id(), SESSION);
        let mut a = (!recycled).then(|| {
            let a = parked()
                .env("ATERM_PARENT_SESSION_ID", "s-a")
                .spawn()
                .expect("A");
            wait_exec(a.id());
            a
        });
        let (notice_pid, notice_start) = match &a {
            Some(a) => (a.id(), register(&opts.home, a.id(), OTHER).proc_start),
            None => (b.id(), "Thu Sep 24 00:00:00 2026".to_string()),
        };
        let st = St {
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            marker: old.clone(),
            markers: vec![old.clone()],
            asked: vec![old.clone()],
            release: "void".to_string(),
            salt,
            pending_since: salt,
            last_seq: 77,
            seq_since_s: now - 3_600,
            tab: "s-a".to_string(),
            notice_pid,
            notice_start: squash(&notice_start),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
        let shell = dead_pid();
        let table = vec![(shell, 1, "zsh".into())];
        let args = atpkg::caller_shell::process_args(b.id()).expect("B's argv");
        let visit_as = |opts: &Opts| {
            let files = session_files(&opts.home).expect("complete session scan");
            visit_with_claim(
                opts,
                &sf_b,
                Some(&files),
                &table,
                &newer(),
                &Script::new(shell, usize::MAX, None),
                Some(&args),
                None,
            )
        };
        let ledger =
            || std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();

        if let Some(live) = a.as_mut() {
            let waits = visit_as(&opts);
            assert_eq!(waits.step, "wait:notice-owned-by-other-process", "{name}");
            assert_eq!(
                model.action_enabled("Reask", &run_model(&model, &["Announce", "Ready", holder])),
                waits.step.ends_with(":notice-process-gone"),
                "{name}: Tier-1, the notified process alive"
            );
            assert_eq!(load(&opts, SESSION), Some(st.clone()), "{name}");
            live.kill().expect("stop A");
            live.wait().expect("reap A");
        }
        let gone = run_model(&model, &["Announce", "Ready", "OwnerExits", holder]);

        let dry = visit_as(&Opts {
            dry_run: true,
            ..opts.clone()
        });
        assert_eq!(dry.step, "would-reopen:notice-process-gone", "{name}");
        assert_eq!(load(&opts, SESSION), Some(st.clone()), "{name}");
        assert!(ledger().is_empty(), "{name}: a dry run ledgers nothing");

        let report = visit_as(&opts);
        assert_eq!(report.step, "announced:1", "{name}");
        assert_eq!(
            model.action_enabled("Reask", &gone),
            ledger().contains(r#""step":"reopened:notice-process-gone""#),
            "{name}: Tier-1, the notified process gone"
        );
        let got = load(&opts, SESSION).expect("state");
        assert!(
            matches!(got.phase, Phase::Announced { asks: 1, .. }),
            "{name}: {:?}",
            got.phase
        );
        assert_ne!(got.marker, old, "{name}: a marker of B's own");
        assert!(
            got.pending_since > salt,
            "{name}: B's age, not A's round's: {} vs {salt}",
            got.pending_since
        );
        assert_eq!(
            (got.tab.as_str(), got.notice_pid, got.notice_start.as_str()),
            (b_tab, sf_b.pid, squash(&sf_b.proc_start).as_str()),
            "{name}: the notice is B's"
        );
        let typed = asked.lock().expect("requests").clone();
        let notices: Vec<&String> = typed.iter().filter(|l| l.contains(" turn ")).collect();
        assert_eq!(notices.len(), 1, "{name}: {typed:?}");
        assert!(
            notices[0].contains(&got.marker) && !notices[0].contains(&old),
            "{name}: {}",
            notices[0]
        );
        let rows = ledger();
        let reopened: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains(r#""step":"reopened:notice-process-gone""#))
            .collect();
        assert_eq!(reopened.len(), 1, "{name}: said once: {rows}");

        let tail = tail_to_end(
            &transcript(&opts.home, SESSION).expect("transcript"),
            TAIL_BYTES,
        )
        .0;
        assert!(
            upgrade::transcript_has_ready(&tail, &old),
            "{name}: still A's last word"
        );
        assert!(!heard(&got, &sf_b, b_tab, Some(&tail)), "{name}");
        let mut asked_afresh = gone.clone();
        assert!(model.fire("Reask", &mut asked_afresh));
        assert!(model.fire("Announce", &mut asked_afresh));
        let again = visit_as(&opts);
        assert_eq!(
            model.action_enabled("Terminate", &asked_afresh),
            again.step == "terminated",
            "{name}: Tier-1, A's READY signals nothing"
        );
        assert_eq!(again.step, "wait:awaiting-ready", "{name}");
        let typed = asked.lock().expect("requests").clone();
        assert!(
            !typed.iter().any(|l| l.contains(" signal ")),
            "{name}: {typed:?}"
        );
        assert_eq!(turns(&asked), 1, "{name}: nothing typed again");
        assert!(alive(b.id()), "{name}: B was not signalled");
        b.kill().expect("stop B");
        b.wait().expect("reap B");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A gone notice reopens the upgrade before the visit knows whether a move is still owed.
#[cfg(unix)]
#[test]
fn a_gone_notices_row_promises_no_ask_to_a_holder_already_current() {
    let dir = scratch("gone-notice-current");
    let (sock, asked) = instance_of(&dir, "s-a");
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let now = now_s();
    let nine = Version::parse("9.9.9").expect("version");
    let old = upgrade::ready_marker(SESSION, &nine, now - 7_199);
    let mut a = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("A");
    wait_exec(a.id());
    let notice_start = kernel_start(a.id()).expect("A's start");
    a.kill().expect("stop A");
    a.wait().expect("reap A");
    let mut b = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("B");
    wait_exec(b.id());
    register(&opts.home, b.id(), SESSION);
    let file = opts.home.join(format!(".claude/sessions/{}.json", b.id()));
    let text = std::fs::read_to_string(&file).expect("session file");
    std::fs::write(
        &file,
        text.replace(r#""version":"1.0.0""#, r#""version":"9.9.9""#),
    )
    .expect("rewrite");
    let sf_b = session_file_of(&opts.home, b.id()).expect("the file parses");
    assert_eq!(sf_b.version, "9.9.9");
    let st = St {
        phase: Phase::Announced {
            at_s: now - 600,
            asks: 1,
        },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: old.clone(),
        markers: vec![old.clone()],
        asked: vec![old],
        release: "void".to_string(),
        salt: now - 7_200,
        tab: "s-a".to_string(),
        notice_pid: a.id(),
        notice_start: squash(&notice_start),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".into())];
    let args = atpkg::caller_shell::process_args(b.id()).expect("B's argv");
    let visit_b = || {
        let files = session_files(&opts.home).expect("complete session scan");
        visit_with_claim(
            &opts,
            &sf_b,
            Some(&files),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    let ledger =
        || std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();

    let report = visit_b();
    assert_eq!(report.step, "current", "B runs the build: nothing owed");
    assert_eq!(turns(&asked), 0, "nothing typed");
    assert!(load(&opts, SESSION).is_none(), "closed: B is on the build");
    let rows = ledger();
    assert_eq!(
        rows.lines()
            .filter(|l| l.contains(r#""step":"reached:by-hand""#))
            .count(),
        1,
        "said once: {rows}"
    );
    assert_eq!(visit_b().step, "current");
    assert_eq!(ledger(), rows);
    assert_eq!(turns(&asked), 0);
    assert!(alive(b.id()), "B was not signalled");
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The record a gone notice leaves ([`St::notice_gone`]).
#[test]
fn a_gone_notice_leaves_a_new_round_pending_in_the_holders_tab() {
    let mut announced = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        tab: "s-a".to_string(),
        notice_pid: 4242,
        notice_start: "Fri Sep 25 16:00:00 2026".to_string(),
        salt: 1_000,
        pending_since: 900,
        noted: "attended".to_string(),
        hold_since_s: 700,
        hold_seen_s: 1_050,
        idle_looks: 1,
        held_by: vec![HeldBy {
            pid: 63_492,
            name: "zsh".to_string(),
            since: 900,
        }],
        ..St::default()
    };
    announced.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 1_100, 1);
    announced.owe_release("void");
    assert_eq!(announced.release, "void", "owed to the notified agent");
    assert!(!announced.markers.is_empty() && !announced.asked.is_empty());
    for (request, request_tab, holder, kept) in [
        (Request::Now, "s-a", "s-a", false),
        (Request::Now, "s-a", "s-b", false),
        (Request::Now, "", "s-a", false),
        (Request::Skip("9.9.9".to_string()), "s-a", "s-b", true),
        (Request::DeferUntil(9_999), "s-a", "s-b", true),
    ] {
        let mut st = St {
            request: request.clone(),
            request_tab: request_tab.to_string(),
            request_at: 1_150,
            ..announced.clone()
        };
        let dropped = st.notice_gone(holder, 5_000);
        assert_eq!(dropped, "void", "{request:?}");
        assert_eq!(st.phase, Phase::Pending);
        assert!(st.release.is_empty(), "nothing owed to a gone process");
        assert!(st.marker.is_empty() && st.markers.is_empty() && st.asked.is_empty());
        assert!(st.salt > 1_000 + u64::from(upgrade::MAX_ASKS));
        assert_eq!(st.pending_since, 900, "behind since the first round");
        assert_eq!(
            (st.tab.as_str(), st.notice_pid, st.notice_start.as_str()),
            (holder, 0, ""),
            "{request:?}"
        );
        assert!(st.noted.is_empty(), "a hold is said again in the new round");
        assert_eq!(
            (st.hold_since_s, st.hold_seen_s),
            (0, 0),
            "{request:?}: the hold clocks start again"
        );
        assert_eq!(st.idle_looks, 0, "{request:?}");
        assert!(st.held_by.is_empty(), "{request:?}");
        let (word, named, at) = (st.request.clone(), st.request_tab.as_str(), st.request_at);
        if kept {
            assert_eq!(
                (word, named, at),
                (request.clone(), request_tab, 1_150),
                "{request:?} named {request_tab:?}, holder in {holder}"
            );
        } else {
            assert_eq!(
                (word, named, at),
                (Request::None, "", 0),
                "{request:?} named {request_tab:?}, holder in {holder}"
            );
        }
    }
    let mut asked = St {
        tab: "s-a".to_string(),
        ..St::default()
    };
    asked.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 10, 1);
    assert_eq!(asked.notice_gone("s-a", 20), "");
}

/// `actions` fired in turn from `m`'s initial state.
fn run_model(
    m: &aterm_spec::derive::Model,
    actions: &[&str],
) -> std::collections::BTreeMap<&'static str, i64> {
    let mut s = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut s), "{action} at {s:?}");
    }
    s
}

/// A sibling session file half-written or unreadable as the sweep reads it leaves the roster
/// partial.
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
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#
        ),
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
    let mut sibling_agent = parked().spawn().expect("sibling");
    wait_exec(sibling_agent.id());
    let sibling_pid = sibling_agent.id();
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
        let model = aterm_models_harness::harness_upgrade_notice_owner_model();
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
    assert!(session_files(&opts.home).is_some(), "control: whole again");
    sibling_agent.kill().expect("stop sibling");
    sibling_agent.wait().expect("reap sibling");
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// One stale sibling does not stop every Claude upgrade.
#[test]
fn a_dead_or_foreign_sibling_leaves_the_roster_whole() {
    let dir = scratch("stale-sibling");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let sessions = opts.home.join(".claude/sessions");
    let gone = dead_pid();
    assert!(!alive(gone));
    let stale = sessions.join(format!("{gone}.json"));
    for shape in ["half", "dir", "schema", "foreign"] {
        match shape {
            "half" => std::fs::write(&stale, "{").expect("truncated"),
            "dir" => std::fs::create_dir(&stale).expect("unreadable"),
            "schema" => std::fs::write(
                &stale,
                format!(r#"{{"pid":{gone},"sessionId":"{SESSION}","cwd":"/"}}"#),
            )
            .expect("an older shape"),
            _ => std::fs::write(sessions.join("notes.json"), "{").expect("not a pid record"),
        }
        let roster = session_files(&opts.home)
            .unwrap_or_else(|| panic!("{shape}: a stale sibling vetoed the roster"));
        assert_eq!(roster, vec![sf.clone()], "{shape}");
        assert_eq!(require_unique_owner(&opts.home, &sf), Ok(()), "{shape}");
        match shape {
            "dir" => std::fs::remove_dir(&stale).expect("remove"),
            "foreign" => std::fs::remove_file(sessions.join("notes.json")).expect("remove"),
            _ => std::fs::remove_file(&stale).expect("remove"),
        }
    }
    std::fs::copy(sessions.join(format!("{}.json", sf.pid)), &stale).expect("copy");
    assert!(
        session_files(&opts.home).is_none(),
        "a live pid still vetoes"
    );
    std::fs::remove_file(&stale).expect("remove");
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
        unanswered: false,
    }
}

/// A real sweep's options over `dir`, dialling a socket that does not exist:
/// a sweep that got past every proof reports `wait:no-socket`.
fn drive(dir: &Path) -> Opts {
    build_start::PROBE.with(|named| {
        named
            .borrow_mut()
            .get_or_insert_with(|| std::rc::Rc::new(|_: &Path| build_start::Probe::Starts));
    });
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(dir.join("no.sock").to_string_lossy().into_owned()),
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: !QUIET.with(std::cell::Cell::get),
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
    }
}

/// What every start probe on this thread finds from now on
/// ([`build_start::PROBE`]; [`drive`] names `Starts`): a test of a build
/// that does not start names what its probe finds, and the probes it runs
/// are counted in the returned cell.
fn probe_finds(found: build_start::Probe) -> std::rc::Rc<std::cell::Cell<usize>> {
    let ran = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&ran);
    build_start::PROBE.with(|named| {
        *named.borrow_mut() = Some(std::rc::Rc::new(move |_: &Path| {
            count.set(count.get() + 1);
            found
        }));
    });
    ran
}

/// Seeds `opts`'s state with the baked-catalog cache ([`catalog::baked_cached`], keyed by the
/// binary's path, size and mtime) for the machine's managed Claude Code build, scanned once per
/// process: a scan reads the whole build (230 MB, 0.7 s of CPU in a debug build), and every
/// test's fresh state would scan it anew.
fn seed_builds(opts: &Opts) {
    type Key = (PathBuf, u64, Option<SystemTime>);
    type Files = Vec<(std::ffi::OsString, Vec<u8>)>;
    static SCANNED: std::sync::Mutex<Option<(Key, Files)>> = std::sync::Mutex::new(None);
    let Some(bin) = atpkg::store::resolve_configured()
        .and_then(|layout| std::fs::read_to_string(layout.agents_dir().join("claude")).ok())
        .and_then(|twin| catalog::twin_binary(&twin))
    else {
        return;
    };
    let Ok(meta) = std::fs::metadata(&bin) else {
        return;
    };
    let key = (bin.clone(), meta.len(), meta.modified().ok());
    let mut scanned = SCANNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if scanned.as_ref().is_none_or(|(k, _)| *k != key) {
        let once = ScratchDir::new("upgrade", "builds");
        let recording = aterm_durable::seam::Recording::start();
        let first = catalog::baked_cached(&bin, &once);
        assert_eq!(
            recording.take(),
            [aterm_durable::Flush::Ordered],
            "the facts cache is only ordered"
        );
        assert_eq!(
            catalog::baked_cached(&bin, &once),
            first,
            "the cache file reads back as the scan that wrote it"
        );
        let files = std::fs::read_dir(&*once)
            .expect("the builds cache")
            .flatten()
            .map(|e| {
                (
                    e.file_name(),
                    std::fs::read(e.path()).expect("a cache file"),
                )
            })
            .collect();
        *scanned = Some((key, files));
    }
    let builds = state_dir(opts).join("builds");
    std::fs::create_dir_all(&builds).expect("the builds cache");
    for (name, bytes) in scanned.iter().flat_map(|(_, files)| files) {
        std::fs::write(builds.join(name), bytes).expect("a cache file");
    }
}

thread_local! {
    /// This test thread's visits run under the default `upgrade_ask = false` ([`quietly`]).
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with [`drive`]'s options under the default `[harness] upgrade_ask =
/// false`: the quiet upgrade ([`upgrade::Facts::quiet`]).
fn quietly<T>(f: impl FnOnce() -> T) -> T {
    QUIET.with(|q| q.set(true));
    let out = f();
    QUIET.with(|q| q.set(false));
    out
}

/// A `/bin/sh` script at `exe` running `body`, `exe` a hard link of one stand-in per process.
#[cfg(unix)]
pub(super) fn sh_script(exe: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    const STAND_IN: &str = "#!/bin/sh\n. \"$0.sh\"\n";
    static SHARED: std::sync::Mutex<Option<(ScratchDir, PathBuf)>> = std::sync::Mutex::new(None);
    extern "C" fn remove_stand_in() {
        if let Ok(mut shared) = SHARED.try_lock() {
            shared.take();
        }
    }
    unsafe extern "C" {
        /// `stdlib.h`'s `atexit`, declared here because the workspace's `libc` (`aterm-libc`)
        /// lacks it.
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    let mut beside = exe.as_os_str().to_owned();
    beside.push(".sh");
    std::fs::write(&beside, format!("{body}\n")).expect("the script's body");
    let mut shared = SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if shared.is_none() {
        let dir = ScratchDir::new("upgrade", "stand-in");
        let file = dir.join("sh");
        if std::fs::write(&file, STAND_IN).is_ok()
            && std::fs::write(dir.join("sh.sh"), "exit 0\n").is_ok()
            && std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o555)).is_ok()
        {
            aterm_spec::wait::first_exec(&file, &[]);
            *shared = Some((dir, file));
            // SAFETY: registers a stateless `extern "C"` cleanup function via `atexit` once.
            unsafe { atexit(remove_stand_in) };
        }
    }
    let linked = shared
        .as_ref()
        .is_some_and(|(_, file)| std::fs::hard_link(file, exe).is_ok());
    drop(shared);
    if !linked {
        std::fs::write(exe, STAND_IN).expect("the script");
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// Runs independent cases each on its own thread, joins every one.
pub(super) fn side_by_side<'a>(cases: Vec<Box<dyn FnOnce() + Send + 'a>>) {
    std::thread::scope(|scope| {
        let running: Vec<_> = cases.into_iter().map(|case| scope.spawn(case)).collect();
        let mut failed = None;
        for case in running {
            if let Err(panic) = case.join() {
                failed.get_or_insert(panic);
            }
        }
        if let Some(panic) = failed {
            std::panic::resume_unwind(panic);
        }
    });
}

/// A pid that was a process and is not one now.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    dead
}

fn ledger_lines(opts: &Opts) -> usize {
    std::fs::read_to_string(state_dir(opts).join("ledger.jsonl")).map_or(0, |t| t.lines().count())
}

/// The ready answer is read whatever byte the tail window starts on, also the second byte of
/// `é` (C3 A9).
#[test]
fn a_ready_answer_is_read_when_the_tail_window_starts_inside_a_character() {
    let dir = scratch("tail");
    let path = dir.join("t.jsonl");
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let ready = format!(
        r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    ) + "\n";
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
    assert_eq!(tail_to_end(&path, 256).1, bytes.len() as u64);
    assert_eq!(tail_to_end(&path, 1 << 20).1, bytes.len() as u64);
    assert_eq!(
        tail_to_end(&dir.join("absent.jsonl"), 256),
        (String::new(), 0)
    );
    let end = bytes.len() as u64;
    assert_eq!(since(&path, end - 4, 1 << 20), "]}}\n");
    assert_eq!(since(&path, end + 1, 1 << 20), "");
    assert_eq!(since(&path, 0, 3), r#"{"t"#);
    assert!(!upgrade::transcript_has_ready(
        &tail_to_end(&path, 256).0,
        "ATERM-UPGRADE-READY-00000000"
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_transcript_append_cannot_grow_a_tail_read_past_its_budget() {
    let body = b"0123456789abcdef";
    let mut file = std::io::Cursor::new(body);
    let (tail, mark) = tail_window(&mut file, 8, 4);
    assert_eq!(tail, "4567");
    assert_eq!(mark, 8);
    let mut unread = String::new();
    file.read_to_string(&mut unread).expect("remaining bytes");
    assert_eq!(unread, "89abcdef", "the returned mark skips no append");

    let mut file = std::io::Cursor::new(body);
    assert_eq!(tail_window(&mut file, 2, 4), ("0123".to_owned(), 4));
    let mut file = std::io::Cursor::new(body);
    assert_eq!(tail_window(&mut file, 8, 0), (String::new(), 8));
}

/// `cell` replies from a Claude Code tab.
#[test]
fn a_cell_reply_is_dim_only_by_its_attrs() {
    assert!(cell_is_dim("OK p 999999 111318 dim"));
    assert!(!cell_is_dim("OK %E2%9D%AF d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %C2%A0 d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %20 d0d0d0 111318 none"));
    assert!(cell_is_dim(
        "OK x 999999 111318 bold,dim link=https%3A%2F%2Fa"
    ));
    assert!(cell_is_dim("OK  999999 111318 dim,wide_cont"));
    assert!(!cell_is_dim("OK  999999 111318 none"));
    assert!(!cell_is_dim("ERR out of range"));
    assert!(!cell_is_dim("OK x 999999 111318"), "no attrs, no proof");
    assert!(!cell_is_dim(""));
    assert!(
        !cell_is_dim("OK x 999999 111318 dim frame=open"),
        "a cell of an unfinished frame is no proof"
    );
}

/// A control connection whose server answers each request with the next of
/// `replies`, and hands back every request it read once the client is gone.
#[cfg(unix)]
pub(super) fn fake_tab(
    replies: Vec<&'static str>,
) -> (Client, std::thread::JoinHandle<Vec<String>>) {
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

/// `--status` reads other supervisors' claims and the owner's Autopilot Off from one `sessions`
/// read.
#[cfg(unix)]
#[test]
fn the_roster_names_another_supervisors_tabs_and_the_off_ones_never_the_windows_own() {
    const ROSTER: &str = concat!(
        "OK 8\n",
        "1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 autopilot=off \
         upgrade=-\n",
        "2 s-b - live claude meta=1 window=1 supervisor=aterm-harness@777 autopilot=cruise \
         upgrade=-\n",
        "3 s-c - live zsh meta=0 window=1 supervisor=- autopilot=- upgrade=-\n",
        "4 s-d s-a live supervisor=aterm-supervise:9 meta=0 window=1 supervisor=- autopilot=- \
         upgrade=-\n",
        "5 s-e - live claude meta=1 window=1 supervisor=aterm-harness%40778 autopilot=land \
         upgrade=-\n",
        "6 s-f - live claude meta=1 window=1 supervisor=a%20script upgrade=-\n",
        "7 s-g - live claude meta=1 window=1 supervisor=- autopilot=off upgrade=-\n",
        "8 s-h - live autopilot=off meta=1 window=1 supervisor=- autopilot=cruise upgrade=-",
    );
    let (mut c, server) = fake_tab(vec![ROSTER]);
    let aside = roster_aside(&mut c, &[]);
    drop(c);
    assert_eq!(
        aside,
        Some(Aside {
            claimed: ["s-a", "s-f"].map(str::to_string).into(),
            off: ["s-a", "s-g"].map(str::to_string).into(),
            ..Aside::default()
        }),
        "another's claim, never the window's own; the owner's Off"
    );
    assert_eq!(server.join().expect("server"), vec!["sessions"]);
    let (mut c, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(roster_aside(&mut c, &[]), None, "no roster");
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let wait = Duration::from_millis(50);
        let _ = tx.send(roster_aside_within(&mut RelayClient::new(ours), &[], wait));
    });
    let given_up = rx.recv_timeout(Duration::from_secs(60));
    drop(theirs);
    assert_eq!(given_up, Ok(None), "a wedged instance is no roster");
}

/// `--status` unions every instance's claims and Off tabs.
#[cfg(unix)]
#[test]
fn the_aside_of_every_instance_is_one() {
    let (a, _) = fake_tab(vec![
        "OK 1\n1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 upgrade=-",
    ]);
    let (b, _) = fake_tab(vec![concat!(
        "OK 2\n",
        "1 s-b - live claude meta=1 window=1 supervisor=a%20script upgrade=-\n",
        "2 s-o - live claude meta=1 window=1 supervisor=- autopilot=off upgrade=-",
    )]);
    let (refused, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(
        aside_of([a, refused, b], &[]),
        Aside {
            claimed: ["s-a", "s-b"].map(str::to_string).into(),
            off: ["s-o"].map(str::to_string).into(),
            ..Aside::default()
        }
    );
}

/// On `--status` too, a tab whose agent's own switch is off is set aside as that switch's, as the
/// window's view reads it.
#[cfg(unix)]
#[test]
fn a_switched_off_agents_tab_is_set_aside_as_its_switchs_on_status() {
    let roster = || {
        fake_tab(vec![concat!(
            "OK 2\n",
            "1 s-x - live codex meta=1 window=1 program=codex supervisor=- autopilot=off upgrade=-\n",
            "2 s-c - live claude meta=1 window=1 program=claude supervisor=- autopilot=off upgrade=-",
        )])
        .0
    };
    let aside = aside_of([roster()], &[aterm_phase::Program::Codex]);
    assert_eq!(aside.agent_off, ["s-x"].map(str::to_string).into());
    assert_eq!(aside.off, ["s-c", "s-x"].map(str::to_string).into());
    assert!(aside_of([roster()], &[]).agent_off.is_empty());
}

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

/// Claude Code 2.1.310's grey suggestion under the caret it draws (the cells read in the
/// real-Claude lab, 2026-10-24: the first glyph inverse, the rest dim) is an empty composer to the
/// upgrade's gate, as it is to the loop: the gate reads the glyph after the caret. NEGATIVE
/// CONTROL: the same caret over plain text is a draft.
#[cfg(unix)]
#[test]
fn a_suggestion_under_a_drawn_caret_is_an_empty_composer() {
    const SUGGESTED: &str = r#"OK {"rows":["────────────","❯ keep going","────────────"],"cursor":{"row":1,"col":2},"first":0}"#;
    for (after, empty) in [
        ("OK e 999999 111318 dim", true),
        ("OK e d0d0d0 111318 none", false),
    ] {
        let (mut c, server) = fake_tab(vec![SUGGESTED, "OK k 111318 d0d0d0 inverse", after]);
        let mut tail_available = false;
        assert_eq!(
            continuation_composer_empty(&mut c, "s-x", &mut tail_available),
            empty,
            "{after}"
        );
        drop(c);
        assert_eq!(
            server.join().expect("server"),
            ["@s-x text --json", "@s-x cell 1 2", "@s-x cell 1 3"],
            "{after}"
        );
    }
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

/// A grid displaced by a net-zero re-grid never makes a placeholder a draft.
#[cfg(unix)]
#[test]
fn a_displaced_grid_never_makes_a_placeholder_a_draft() {
    let rule = "─".repeat(20);
    let text = "push it as soon as the gates pass";
    let typed = |text: &str, cursor: Option<(usize, usize)>| Screen {
        rows: vec![rule.clone(), format!("❯\u{a0}{text}"), rule.clone()],
        cursor,
        seq: 1,
        first: 54,
        generation: None,
        human: HumanInput::Never,
    };
    let screen = |cursor: Option<(usize, usize)>| typed(text, cursor);
    let (mut c, server) = fake_tab(vec![
        "OK p 999999 111318 dim",
        "OK p d0d0d0 111318 none",
        "ERR out of range",
        "OK p 999999 111318 dim",
        "OK p 999999 111318 dim",
        "OK %20 d0d0d0 111318 none",
    ]);
    assert!(
        composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced: the cursor on the rule below, the suggestion dim"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced, typed: a draft"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced, unreadable: a draft"
    );
    assert!(
        composer_empty(&mut c, "s-x", &screen(None)),
        "no cursor at all: the dim cell decides"
    );
    assert!(
        composer_empty(&mut c, "s-x", &screen(Some((0, 40)))),
        "a cursor on another row says nothing of the composer"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((1, 40)))),
        "the cursor inside the text is typed text: nothing is asked"
    );
    assert!(
        composer_empty(&mut c, "s-x", &typed("", Some((2, 2)))),
        "a blank composer, its cell plain"
    );
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x cell 55 2"; 6],
        "always the caret row, never the cursor's"
    );
}

fn tty_rows(rows: &[(u32, u32, &str, &str)]) -> Vec<(u32, u32, String, String)> {
    rows.iter()
        .map(|(p, pp, tty, name)| (*p, *pp, (*tty).to_string(), (*name).to_string()))
        .collect()
}

/// The terminal proof over process tables.
#[test]
fn a_terminal_is_the_tabs_only_when_aterm_opened_it() {
    let live = tty_rows(&[
        (4205, 1916, "ttys000", "2.1.281"),
        (1916, 1, "ttys000", "zsh"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(4205, &live), Some((1, "launchd")));
    assert!(terminal_owner(4205, &live).is_some_and(owned_by_aterm));
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
    let script = tty_rows(&[
        (99234, 99225, "ttys002", "sleep"),
        (99225, 1, "??", "script"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(99234, &script), Some((99225, "script")));
    assert!(!terminal_owner(99234, &script).is_some_and(owned_by_aterm));
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
    let tmux = tty_rows(&[
        (800, 801, "pts/7", "claude"),
        (801, 802, "pts/7", "bash"),
        (802, 1, "?", "tmux"),
        (1, 0, "?", "systemd"),
    ]);
    assert!(!terminal_owner(800, &tmux).is_some_and(owned_by_aterm));
    assert_eq!(owner_word(terminal_owner(800, &tmux)), "tmux");
    let bare = tty_rows(&[(900, 1, "??", "claude"), (1, 0, "??", "launchd")]);
    assert_eq!(terminal_owner(900, &bare), None);
    let cut = tty_rows(&[(901, 555, "ttys001", "claude")]);
    assert_eq!(terminal_owner(901, &cut), None);
    assert_eq!(owner_word(None), "none");
    assert_eq!(owner_word(Some((5, "a b/c"))), "abc");
}

/// End to end: an agent on a pty the tab does not own ([`pty_host`]'s here, the shape of
/// every `script`, tmux or screen pane, whose environment still names the tab) is never
/// driven through that tab: held back once, naming the pty's owner, then waited on.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_on_a_pty_the_tab_does_not_own_is_never_driven_through_the_tab() {
    use std::ffi::OsStr;
    let exe = std::env::current_exe().expect("exe");
    let child_of = |parent: u32| {
        (0..3_000).find_map(|_| {
            let hit = table()
                .into_iter()
                .find(|(_, pp, _)| *pp == parent)
                .map(|(p, _, _)| p);
            if hit.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            hit
        })
    };
    // The agent as `visit` judges it: the stand-in by its argv (until it execs, a fork carries its
    // parent's, and the host's fork is this same binary, which `wait_exec` would pass), its group
    // its own and the pty's foreground (`ids`: `pgid == tpgid == pid`).
    let leads = |agent: u32| {
        (0..3_000).any(|_| {
            let led = atpkg::caller_shell::process_args(agent)
                .is_some_and(|a| a.argv.get(1).is_some_and(|w| w == PARK[0]))
                && ids(agent)
                    .is_some_and(|(_, pgid, tpgid)| pgid == i64::from(agent) && tpgid == pgid);
            if !led {
                std::thread::sleep(Duration::from_millis(20));
            }
            led
        })
    };
    for in_shell in [true, false] {
        let dir = scratch(if in_shell { "pty" } else { "pty-own" });
        let opts = drive(&dir);
        let mut argv: Vec<&OsStr> = Vec::new();
        if in_shell {
            argv.extend(["/bin/zsh", "-f", "-i", "-c", r#""$@"; :"#, "zsh"].map(OsStr::new));
        }
        argv.push(exe.as_os_str());
        argv.extend(PARK.map(OsStr::new));
        let mut host = pty_host(&argv)
            .env(PARK_ENV, "1")
            .env("ATERM_PARENT_SESSION_ID", TAB)
            .spawn()
            .expect("pty host");
        let agent = if in_shell {
            let shell = child_of(host.id()).expect("the shell on the host's pty");
            child_of(shell).expect("the agent, the shell's job")
        } else {
            child_of(host.id()).expect("the agent, the host's own program")
        };
        assert!(
            leads(agent),
            "{in_shell}: the agent {agent} never led the host's pty"
        );
        let owner = ps_name(host.id()).expect("the host, as ps names it");
        let sf = register(&opts.home, agent, SESSION);
        let got = visit(&opts, &sf, &table(), &newer(), &Live);
        let again = visit(&opts, &sf, &table(), &newer(), &Live);
        // SAFETY: kill takes two integers; the agent descends from this test and stays parked
        // until its parent goes.
        let _ = unsafe { libc::kill(libc::pid_t::try_from(agent).expect("pid"), libc::SIGKILL) };
        let _ = host.kill();
        let _ = host.wait();
        assert!(
            !owned_by_aterm((host.id(), &owner)),
            "the host is no aterm: {owner}"
        );
        let word = owner_word(Some((host.id(), &owner)));
        assert_eq!(
            got.step,
            format!("held-back:terminal:{word}"),
            "{in_shell}: {got:?}"
        );
        assert_eq!(got.tab, TAB);
        assert!(got.is_act(), "said, where the window's host logs it");
        assert_eq!(
            again.step,
            format!("wait:terminal:{word}"),
            "{in_shell}: {again:?}"
        );
        assert!(!again.is_act(), "said ONCE");
        assert_eq!(ledger_lines(&opts), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// [`Job`] from `ps`'s numbers, recorded shapes.
#[cfg(unix)]
#[test]
fn only_its_shells_foreground_job_is_ever_announced_to_or_ended() {
    assert_eq!(job(4205, 4205, 4205, 1916, false), Job::Foreground);
    assert_eq!(job(4205, 4205, 1916, 1916, false), Job::Background);
    assert_eq!(job(30771, 30770, 30770, 30770, false), Job::NoJobControl);
    assert_eq!(job(24965, 24932, 24932, 24932, false), Job::NoJobControl);
    assert_eq!(
        job(4205, 4205, 4205, 1880, true),
        Job::TabProgram { foreground: true }
    );
    assert_eq!(
        job(4205, 4205, 4300, 1880, true),
        Job::TabProgram { foreground: false }
    );
    assert_eq!(job(30771, 30770, 30770, 1880, true), Job::NoJobControl);
    assert_eq!(job(4205, 4205, 4205, 1880, false), Job::Foreground);
    let read = |job: Job| Some((job, 1880));
    assert_eq!(leads_terminal(read(Job::Foreground)), Ok(()));
    assert_eq!(
        leads_terminal(read(Job::TabProgram { foreground: true })),
        Ok(())
    );
    assert_eq!(
        leads_terminal(read(Job::TabProgram { foreground: false })),
        Err("not-foreground")
    );
    assert_eq!(leads_terminal(read(Job::Background)), Err("not-foreground"));
    assert_eq!(
        leads_terminal(read(Job::NoJobControl)),
        Err("not-a-shell-job")
    );
    assert_eq!(
        foreground_shell(read(Job::TabProgram { foreground: true })),
        Err("not-a-shell-job")
    );
    assert_eq!(
        not_a_shell_job(read(Job::TabProgram { foreground: true })),
        Some(Job::TabProgram { foreground: true })
    );
    assert_eq!(
        not_a_shell_job(read(Job::NoJobControl)),
        Some(Job::NoJobControl)
    );
    assert_eq!(not_a_shell_job(read(Job::Background)), None);
    let me = std::process::id();
    let mut plain = parked().spawn().expect("spawn");
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let mut execd = tty_agent(&[], None);
    let (a, b) = (job_of(&Live, plain.id()), job_of(&Live, own.id()));
    let opened = (
        job_of(&Live, execd.id()),
        Live.terminal(execd.id()),
        Live.opener(execd.id(), me),
    );
    let not_opened = Live.opener(plain.id(), me);
    for c in [&mut plain, &mut own] {
        let _ = c.kill();
        let _ = c.wait();
    }
    let _ = execd.kill();
    let _ = execd.wait();
    assert_eq!(a, Some((Job::NoJobControl, me)));
    assert_eq!(b, Some((Job::Background, me)));
    let (owner, name) = opened.1.expect("its terminal's owner");
    assert_eq!(owner, me, "this process opened its terminal");
    assert_eq!(opened.2.as_deref(), Some(name.as_str()), "the same opener");
    assert!(!owned_by_aterm((owner, name.as_str())), "{name}");
    assert_eq!(opened.0, Some((Job::Foreground, me)));
    assert_eq!(not_opened, None);
}

/// End to end: an agent no job-control shell started (a launcher script that runs `claude` without
/// `exec`, `sh -c`, `make`, an IDE task) is refused before anything is typed.
#[cfg(unix)]
#[test]
fn an_agent_no_job_control_shell_started_is_refused_before_anything_is_typed() {
    let dir = scratch("job");
    let opts = drive(&dir);
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
    assert_eq!(again.step, "wait:no-socket", "{again:?}");
    assert_eq!(ledger_lines(&opts), 1);
    let other = "0badf00d-1111-2222-3333-444455556667";
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), other);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let got = visit(&dry, &sf, &table(), &newer(), &Live);
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

/// A visit's own retarget is said once in the ledger (`retargeted:<from>-><to>`).
#[cfg(unix)]
#[test]
fn a_visits_own_retarget_is_said_once_in_the_ledger() {
    let dir = scratch("visit-retarget");
    let opts = drive(&dir);
    let mut agent = parked().process_group(0).spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let waiting = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.8".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 100,
        pending_since: 100,
        ..St::default()
    };
    save(&opts, SESSION, &waiting);
    let first = visit(&opts, &sf, &table(), &newer(), &Live);
    let again = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    for r in [&first, &again] {
        assert_eq!(r.step, "wait:not-foreground", "{r:?}");
        assert!(!r.is_act(), "{r:?}");
    }
    assert_eq!(
        noted_for(&opts, "retargeted:9.9.8->9.9.9", SESSION),
        1,
        "said once, by the visit that made it"
    );
    assert_eq!(ledger_lines(&opts), 1, "and nothing else");
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!((st.to.as_str(), &st.phase), ("9.9.9", &Phase::Pending));
    assert_eq!(st.behind_since(), 100, "the move clock kept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`St::retarget`] names the arm that made a retarget.
#[test]
fn a_retarget_is_told_by_the_arm_that_made_it() {
    let v = |s: &str| Version::parse(s).expect("version");
    let to = Candidate {
        exe: PathBuf::from("/nonexistent/claude"),
        version: v("2.1.291"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Pending,
        from: "2.1.290".to_string(),
        to: "2.1.290".to_string(),
        source: "managed".to_string(),
        salt: 100,
        pending_since: 100,
        wait: "not-idle:busy".to_string(),
        wait_since: 200,
        last_stop: upgrade::GAVE_UP.to_string(),
        left_typed: "[aterm harness] Claude Code 2.1.290 (man".to_string(),
        ..St::default()
    };
    let (st, how) = St::retarget(
        Some(prior.clone()),
        &v("2.1.290"),
        &to,
        Some("claude-opus-5-5"),
        Proc::default(),
        900,
    );
    assert_eq!(how, Retarget::Anew);
    assert_eq!((&st.phase, st.marker.as_str()), (&prior.phase, ""));
    assert!(st.wait.is_empty() && st.last_stop.is_empty() && st.salt == 900);
    assert_eq!(st.left_typed, prior.left_typed, "its words ride it");
    let (st, how) = St::retarget(
        Some(prior.clone()),
        &v("2.1.290"),
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(how, Retarget::InPlace);
    assert_eq!((st.wait.as_str(), st.salt), ("not-idle:busy", 100));
}

/// A [`Kernel`] that answers as scripted.
#[cfg(unix)]
struct Script {
    shell: u32,
    foreground: usize,
    reads: std::cell::Cell<usize>,
    parent: Option<u32>,
}

#[cfg(unix)]
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

#[cfg(unix)]
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

/// A visit's parked test process with its process facts injected (its start, its background work).
#[cfg(unix)]
struct ParkedProcess<'a> {
    script: &'a Script,
    pid: u32,
    start: Option<String>,
    background: Vec<String>,
}

#[cfg(unix)]
impl Kernel for ParkedProcess<'_> {
    fn job(&self, pid: u32) -> Option<(Job, u32)> {
        self.script.job(pid)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.script.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        self.script.terminal(pid)
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        (pid == self.pid).then(|| self.start.clone()).flatten()
    }

    fn restart_background(&self, pid: u32) -> Vec<String> {
        if pid == self.pid {
            self.background.clone()
        } else {
            vec![UNREADABLE_TABLE.to_string()]
        }
    }

    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        self.script.shell_holds_tab(shell)
    }

    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        self.script.describe(agent, procs)
    }
}

/// The screen [`instance`] shows: an idle Claude's empty composer, the caret at
/// column 2 of its row, and no person's input to the session (`"human_ms":null`,
/// so the tab is not attended: `upgrade::attended_by`).
#[cfg(unix)]
fn idle_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// The token [`instance`] writes beside its socket.
#[cfg(unix)]
const TOKEN: &str = "0badf00d0badf00d";

/// What [`instance_with`]'s stand-in answers, beyond its fixed replies.
#[cfg(unix)]
#[derive(Clone)]
struct Answers {
    /// Columns after the tab's `sessions` row (`active=1 wfocus=1`).
    row: &'static str,
    /// The `text --json` body.
    screen: String,
    /// After this many `text` replies, this body instead.
    after: Option<(usize, String)>,
    /// The `help <verb>` reply.
    help: &'static str,
    /// Each `turn`'s reply, in order; the last one repeats.
    turn: Vec<&'static str>,
    /// The `blocks 1 --json` reply (the shell integration's command blocks).
    blocks: &'static str,
    /// The `line <n>` reply.
    line: &'static str,
    /// The `send` reply (bash's relaunch line is typed with `send`, then
    /// `key enter`).
    send: &'static str,
    /// The `rekey shell=<pid>` reply (a `rekey withdraw` is answered
    /// `OK rekey withdrawn`).
    rekey: &'static str,
    /// The `lease acquire …` reply; a `lease release` is answered `OK lease
    /// released` after an acquire this granted, `OK lease none` otherwise.
    lease: &'static str,
    /// The `status` reply from the second read after a `lease acquire` on:
    /// what a person's keystroke, or a prompt someone sent, makes it between a
    /// restart's first look (under the hand) and its last.
    after_first_look: Option<&'static str>,
    /// The `signal` reply, sending nothing (empty: the stand-in agent named
    /// by `pid=` is sent SIGTERM, then `OK`).
    signal: &'static str,
    /// The `cell <r> <c>` reply (default: a plain blank, not dim).
    cell: &'static str,
    /// The `who` reply: the host's roster, whose `fgpgid=` proves the shell a
    /// relaunch line is typed at holds the tab (`None`: unscripted, so no
    /// relaunch line is ever typed).
    who: Option<String>,
    /// A Claude Code prompt the stand-in draws itself ([`Prompt`]) in place of `screen`:
    /// what a `turn` answered with a skip that typed part of its text (`typed=<k>`) or all
    /// of it (`reason=guard`) left there, and a person's key after it where `person_key`
    /// says; `key … ctrl+u` kills the line when its `if-gen=` names the screen drawn.
    prompt: Option<Prompt>,
}

/// What [`Answers::prompt`]'s stand-in prompt holds and how it is drawn.
#[cfg(unix)]
#[derive(Clone, Default)]
struct Prompt {
    /// The text in the prompt.
    draft: String,
    /// A person's key lands after a stopped turn's part (`x`).
    person_key: bool,
}

#[cfg(unix)]
impl Prompt {
    /// The prompt as Claude Code draws it, 40 columns wide: its rules, the
    /// caret row, the draft's continuation rows indented two, the cursor
    /// behind the text; screen generation `4.<generation>`.
    fn screen(&self, generation: u64) -> String {
        const WIDTH: usize = 40;
        let rule = "─".repeat(WIDTH);
        let chars: Vec<char> = self.draft.chars().collect();
        let mut rows = vec![rule.clone()];
        let mut chunks: Vec<String> = chars
            .chunks(WIDTH - 2)
            .map(|c| c.iter().collect())
            .collect();
        if chunks.is_empty() {
            chunks.push(String::new());
        }
        for (i, chunk) in chunks.iter().enumerate() {
            rows.push(format!("{}{chunk}", if i == 0 { "❯ " } else { "  " }));
        }
        let (row, col) = (rows.len() - 1, 2 + chunks[chunks.len() - 1].chars().count());
        rows.push(rule);
        let rows: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        format!(
            r#"{{"rows":[{}],"cursor":{{"row":{row},"col":{col}}},"seq":{generation},"first":0,"gen":"4.{generation}","human_ms":null,"human_seq":0}}"#,
            rows.join(",")
        )
    }
}

#[cfg(unix)]
impl Default for Answers {
    fn default() -> Self {
        Self {
            row: "",
            screen: idle_screen(),
            after: None,
            help: "ERR unscripted",
            turn: vec!["OK status=done"],
            blocks: "ERR unscripted",
            line: "ERR unscripted",
            send: "OK seq=1",
            rekey: "ERR unscripted",
            lease: "ERR unscripted",
            after_first_look: None,
            signal: "",
            cell: "OK %20 d0d0d0 111318 none",
            who: None,
            prompt: None,
        }
    }
}

/// A control socket at `<dir>/t.sock` (its token beside it) standing in for [`TAB`]'s aterm with
/// an idle Claude: `text --json` is [`idle_screen`], `blocks` unscripted, `status` holding
/// nothing, each overridden by `<dir>/t.screen`, `t.blocks` or `t.status`; a `turn` is answered
/// OK and recorded.
#[cfg(unix)]
fn instance(dir: &Path) -> (String, crate::harness::FakeInstanceLog) {
    instance_of(dir, TAB)
}

/// [`instance`] for the tab `tab`.
#[cfg(unix)]
fn instance_of(dir: &Path, tab: &'static str) -> (String, crate::harness::FakeInstanceLog) {
    instance_for(dir, tab, Answers::default())
}

/// [`instance`], answering as `answers` says.
#[cfg(unix)]
fn instance_with(dir: &Path, answers: Answers) -> (String, crate::harness::FakeInstanceLog) {
    instance_for(dir, TAB, answers)
}

/// [`instance`] for the tab `tab`, answering as `answers` says.
#[cfg(unix)]
fn instance_for(
    dir: &Path,
    tab: &'static str,
    answers: Answers,
) -> (String, crate::harness::FakeInstanceLog) {
    let status_file = dir.join("t.status");
    let screen_file = dir.join("t.screen");
    let blocks_file = dir.join("t.blocks");
    let mut turn_replies = answers.turn.clone();
    let mut texts = 0;
    let mut prompt = answers.prompt.clone();
    let mut generation = 77;
    let mut looks: Option<usize> = None;
    crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        match verb {
            "sessions" => format!("OK 1\nlocal {tab} 0 idle claude {}", answers.row),
            "text" if prompt.is_some() => {
                format!(
                    "OK {}",
                    prompt
                        .as_ref()
                        .map_or_else(String::new, |p| p.screen(generation))
                )
            }
            "text" => {
                texts += 1;
                match (&answers.after, std::fs::read_to_string(&screen_file)) {
                    (_, Ok(written)) => format!("OK {}", written.trim()),
                    (Some((n, later)), _) if texts > *n => format!("OK {later}"),
                    _ => format!("OK {}", answers.screen),
                }
            }
            "cell" => answers.cell.to_string(),
            "who" => answers
                .who
                .clone()
                .unwrap_or_else(|| "ERR unscripted".to_string()),
            "status" => {
                let look = looks.as_mut().map(|n| {
                    *n += 1;
                    *n
                });
                match answers.after_first_look {
                    Some(later) if look.is_some_and(|n| n >= 2) => later.to_string(),
                    _ => std::fs::read_to_string(&status_file).map_or_else(
                        |_| "OK schema=1 hold=0 hand=-".to_string(),
                        |s| s.trim().to_string(),
                    ),
                }
            }
            "help" => answers.help.to_string(),
            "blocks" => std::fs::read_to_string(&blocks_file)
                .map_or_else(|_| answers.blocks.to_string(), |b| b.trim().to_string()),
            "line" => answers.line.to_string(),
            "rekey" if line.ends_with(" rekey withdraw") => "OK rekey withdrawn".to_string(),
            "rekey" => answers.rekey.to_string(),
            "lease" if line.contains(" lease release ") => {
                if answers.lease.starts_with("OK lease acquired") {
                    "OK lease released".to_string()
                } else {
                    "OK lease none".to_string()
                }
            }
            "lease" => {
                looks.get_or_insert(0);
                answers.lease.to_string()
            }
            "turn" => {
                let next = turn_replies.first().copied().unwrap_or("OK status=done");
                if turn_replies.len() > 1 {
                    turn_replies.remove(0);
                }
                if let Some(p) = prompt.as_mut() {
                    let text = line
                        .find(" [aterm harness]")
                        .map_or("", |at| &line[at + 1..]);
                    let typed = next
                        .split_whitespace()
                        .find_map(|w| w.strip_prefix("typed="))
                        .and_then(|k| k.parse::<usize>().ok());
                    let left = match typed {
                        Some(k) if k > 0 => Some(text.chars().take(k).collect()),
                        _ if next.contains("reason=guard") => Some(text.to_string()),
                        _ => None,
                    };
                    if let Some(left) = left {
                        p.draft = left;
                        if p.person_key {
                            p.draft.push('x');
                        }
                        generation += 1;
                    }
                }
                next.to_string()
            }
            "key" if prompt.is_some() && line.ends_with(" ctrl+u") => {
                if line.contains(&format!(" if-gen=4.{generation} ")) {
                    if let Some(p) = prompt.as_mut() {
                        p.draft.clear();
                    }
                    generation += 1;
                    format!("OK seq={generation}")
                } else {
                    format!("OK skipped reason=changed seq={generation}")
                }
            }
            "send" => answers.send.to_string(),
            "key" => "OK seq=1".to_string(),
            "signal" if !answers.signal.is_empty() => answers.signal.to_string(),
            "signal" => match line
                .split_whitespace()
                .find_map(|w| w.strip_prefix("pid="))
                .and_then(|p| p.parse::<i32>().ok())
            {
                Some(pid) if line.contains(" signal term ") => {
                    // SAFETY: sends a signal to the stand-in child process spawned by this test.
                    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                        "OK signal term".to_string()
                    } else {
                        "ERR no such process".to_string()
                    }
                }
                _ => "ERR usage".to_string(),
            },
            _ => "ERR unscripted".to_string(),
        }
    })
}

/// The requests of `asked` that type into the tab.
#[cfg(unix)]
fn turns(asked: &std::sync::Mutex<Vec<String>>) -> usize {
    asked
        .lock()
        .map_or(0, |a| a.iter().filter(|l| l.contains(" turn ")).count())
}

/// A dry run has no hands, by its connection ([`connect`]; the watch reads through a dry run,
/// `upgrade_watch`).
#[cfg(unix)]
#[test]
fn a_dry_runs_connection_reads_and_has_no_hands() {
    use std::io::ErrorKind::PermissionDenied;
    let dir = scratch("dry-no-hands");
    let (sock, asked) = instance(&dir);
    let dry = Opts {
        sock: Some(sock.clone()),
        dry_run: true,
        ..drive(&dir)
    };
    let real = Opts {
        dry_run: false,
        ..dry.clone()
    };
    let status = format!("@{TAB} status");
    let key = format!("@{TAB} key enter");
    let mut c = connect(&dry, TAB).expect("the dry run's connection");
    c.request_line(&status).expect("a read goes out");
    for hand in ["key enter", "signal term pid=1", "lease acquire", "send hi"] {
        let err = c.request_line(&format!("@{TAB} {hand}")).expect_err(hand);
        assert_eq!(err.kind(), PermissionDenied, "{hand}");
    }
    let err = c
        .request_frame(&format!("@{TAB} feed-bin"), b"x")
        .expect_err("a framed write");
    assert_eq!(err.kind(), PermissionDenied);
    let mut watch = connect_reads(&real, TAB).expect("the watch's own read");
    assert_eq!(
        watch
            .request_line(&key)
            .expect_err("the watch's key")
            .kind(),
        PermissionDenied
    );
    let mut roster = roster_connection(&sock).expect("a roster read");
    assert_eq!(
        roster.request_line(&key).expect_err("a roster key").kind(),
        PermissionDenied
    );
    drop((c, watch, roster));
    assert_eq!(
        *asked.lock().expect("the log"),
        [status],
        "the read alone reached the instance"
    );

    let mut live = connect(&real, TAB).expect("a real visit's connection");
    live.request_line(&key)
        .expect("a real visit's key goes out");
    drop(live);
    assert!(
        asked.lock().expect("the log").contains(&key),
        "{:?}",
        *asked.lock().expect("the log")
    );
}

/// The initial roster is only a preliminary filter.
#[cfg(unix)]
#[test]
fn stale_preliminary_roster_cannot_authorize_an_announcement() {
    let dir = scratch("stale-precheck-roster");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut a = parked_bare().spawn().expect("agent A");
    wait_exec(a.id());
    let sf_a = register(&opts.home, a.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
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
    let table = vec![(shell, 1, "zsh".into())];
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
    let blocked = run();
    assert_eq!(blocked.step, "wait:conversation-owner-ambiguous");
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

/// The first PTY claim can be true and the tab can switch foreground groups while the continuation
/// reads its latest session file.
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
    file_transcript_in(&opts.home.join(".claude"), "p");
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
        None,
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

/// The continuation a relaunched agent is owed waits on a person's keystroke within the grace
/// (`wait:held`, nothing typed, the restart still in flight).
#[cfg(unix)]
#[test]
fn carry_on_is_held_for_a_person_at_the_keys() {
    let dir = scratch("carry-on-held");
    let (sock, asked) = instance(&dir);
    std::fs::write(
        dir.join("t.status"),
        "OK schema=1 hold=0 hand=- human_ms=800\n",
    )
    .expect("status");
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let fresh = || St {
        phase: Phase::Relaunched { at_s: now_s() },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let mut st = fresh();
    let mut c = connect(&opts, TAB).expect("control connection");
    let report = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, _, _| true,
        None,
    );
    assert_eq!(report.step, "wait:held");
    assert_eq!(turns(&asked), 0, "nothing typed under a person's hands");
    assert!(matches!(st.phase, Phase::Relaunched { .. }));
    std::fs::write(
        dir.join("t.status"),
        "OK schema=1 hold=0 hand=- human_ms=-\n",
    )
    .expect("status");
    let mut st = fresh();
    let _ = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, _, _| true,
        None,
    );
    assert_eq!(turns(&asked), 1);
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(" turn ") && l.contains(" yield=0.2 ")),
        "{:?}",
        asked.lock().unwrap()
    );
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// The continuation is typed under the harness's hand, whoever holds it.
#[cfg(unix)]
#[test]
fn the_continuation_is_typed_under_a_hand_taken_or_passed() {
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let run = |name: &str, lease: &'static str, keep: bool| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                lease,
                ..Answers::default()
            },
        );
        std::fs::write(
            dir.join("t.status"),
            "OK schema=1 hold=0 hand=- human_ms=-\n",
        )
        .expect("status");
        let opts = Opts {
            sock: Some(sock),
            timings: crate::harness::Timings::PRODUCTION,
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let mut st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            ..St::default()
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut hand = keep.then(|| Hand::take(&mut c, TAB).expect("nobody else's"));
        let report = carry_on_with_tab_probe(
            &opts,
            blank(&sf),
            &mut st,
            &mut c,
            SESSION,
            &sf,
            MODEL_WAIT,
            |_, _, _| true,
            hand.as_mut(),
        );
        if let Some(hand) = hand.as_mut() {
            hand.give_back(&mut c);
        }
        agent.kill().expect("stop agent");
        agent.wait().expect("reap agent");
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(dir);
        (report.step, asked)
    };
    let count = |asked: &[String], what: &str| asked.iter().filter(|l| l.contains(what)).count();
    let at = |asked: &[String], what: &str| asked.iter().position(|l| l.contains(what));
    let (step, asked) = run("continuation-own-hand", held, false);
    assert_eq!(step, "continued", "{asked:#?}");
    let (acquire, turn, release) = (
        at(&asked, " lease acquire ").expect("a hand taken"),
        at(&asked, " turn ").expect("the continuation"),
        at(&asked, " lease release ").expect("and given back"),
    );
    assert!(acquire < turn && turn < release, "{asked:#?}");
    assert!(asked[acquire].ends_with(" hard"), "{}", asked[acquire]);
    let (step, asked) = run("continuation-passed-hand", held, true);
    assert_eq!(step, "continued", "{asked:#?}");
    assert_eq!(count(&asked, " lease acquire "), 1, "{asked:#?}");
    assert_eq!(count(&asked, " lease release "), 1, "{asked:#?}");
    let (turn, release) = (
        at(&asked, " turn ").expect("the continuation"),
        at(&asked, " lease release ").expect("given back"),
    );
    assert!(turn < release, "still held when typed: {asked:#?}");
    let (step, asked) = run(
        "continuation-theirs",
        "ERR lease held holder=orchestrator expires_in_ms=5000",
        false,
    );
    assert_eq!(step, "wait:held", "{asked:#?}");
    assert_eq!(count(&asked, " turn "), 0, "{asked:#?}");
    assert_eq!(count(&asked, " lease release "), 0, "{asked:#?}");
}

/// [`type_relaunch_line`]'s check that no live process holds the conversation reads the records it
/// is given (the relaunched agent's own home).
#[cfg(unix)]
#[test]
fn a_relaunch_line_waits_on_a_live_owner_under_its_agents_own_home() {
    let dir = scratch("relaunch-owner-home");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("an empty roster");
    let claude_dir = dir.join("work home/.claude");
    let home = claude_dir.to_string_lossy().into_owned();
    let mut owner = parked().spawn().expect("owner");
    wait_exec(owner.id());
    register_in(&claude_dir, owner.id(), SESSION);
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let mut line = |records: &Records| {
        type_relaunch_line(
            &opts,
            records,
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude --resume",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true,
        )
    };
    assert_eq!(
        line(&Records::of(&opts.home, Some(&home))),
        Err(RelaunchLineError::Wait("conversation-owner-ambiguous"))
    );
    assert_eq!(turns(&asked), 0, "nothing typed over the live owner");
    line(&Records::of(&opts.home, None)).expect("the user's home names no owner");
    assert_eq!(turns(&asked), 1, "the negative control types");
    drop(c);
    let _ = owner.kill();
    let _ = owner.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A shell can leave the tab's foreground group during the complete session scan.
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
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
            held_since_s: 0,
        },
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
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
            held_since_s: 0,
        },
        |_, _, _| true,
    )
    .expect("unchanged tab can receive a relaunch");
    assert_eq!(turns(&asked), 1, "positive control reaches the turn");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// The relaunch line is typed under the harness's hand ([`Hand`]), so a retrying orchestrator's
/// `turn` cannot slip in around it.
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_typed_under_the_hand_never_let_go() {
    let run = |name: &str, lease: &'static str, turn: &'static str, shell: u32| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                lease,
                turn: vec![turn],
                send: turn,
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut hand = Hand::take(&mut c, TAB).expect("nobody else's");
        let typed = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude --resume",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true,
        );
        hand.give_back(&mut c);
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(dir);
        (typed, asked)
    };
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let mut bash = stand_in_shell();
    for (name, shell, typing) in [
        ("line-zsh", std::process::id(), " turn "),
        ("line-bash", bash.id(), " send "),
    ] {
        let (typed, asked) = run(name, held, "OK status=done", shell);
        assert_eq!(typed, Ok(()), "{name}: {asked:#?}");
        assert!(
            asked[0].contains(" lease acquire ") && asked[0].ends_with(" hard"),
            "{name}: the hard hold first: {asked:#?}"
        );
        let line = asked
            .iter()
            .position(|l| l.contains(typing))
            .unwrap_or_else(|| panic!("{name}: the line: {asked:#?}"));
        assert!(
            !asked[1..=line].iter().any(|l| l.contains(" lease ")),
            "{name}: never let go for the line: {asked:#?}"
        );
        if typing == " send " {
            assert!(
                asked[line].ends_with(" send -- claude --resume")
                    && asked[line + 1].ends_with(" key enter")
                    && !asked.iter().any(|l| l.contains(" turn ")),
                "{name}: {asked:#?}"
            );
        }
        assert!(
            asked.last().is_some_and(|l| l.contains(" lease release ")),
            "{name}: given back at the end: {asked:#?}"
        );
    }
    let halted = "ERR halted reason=owner origin=local";
    for (name, shell) in [
        ("line-halted-zsh", std::process::id()),
        ("line-halted-bash", bash.id()),
    ] {
        let (typed, asked) = run(name, held, halted, shell);
        assert_eq!(typed, Err(RelaunchLineError::Hand), "{name}: {asked:#?}");
    }
    let (typed, _) = run("line-denied-bash", held, "ERR denied", bash.id());
    assert!(
        matches!(typed, Err(RelaunchLineError::Turn(_))),
        "a refusal: {typed:?}"
    );
    let _ = bash.kill();
    let _ = bash.wait();
    let (typed, _) = run(
        "line-busy",
        held,
        "ERR busy lease=orchestrator",
        std::process::id(),
    );
    assert_eq!(typed, Err(RelaunchLineError::Wait("held")));
    let (typed, asked) = run(
        "line-no-hand",
        "ERR lease acquire: unknown arg 'hard' (ttl=<ms> holder=<name>)",
        "OK status=done",
        std::process::id(),
    );
    assert_eq!(typed, Ok(()), "{asked:#?}");
    assert!(
        !asked.iter().any(|l| l.contains(" lease release ")),
        "{asked:#?}"
    );
}

/// The hand outlasts every wait under it (a slow `--resume` may take 90 s to register, its first
/// idle 15 s more).
#[cfg(unix)]
#[test]
fn the_hand_is_kept_through_every_wait_under_it() {
    let dir = scratch("hand-kept");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let acquires = || {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" lease acquire ")).count())
            .unwrap_or_default()
    };
    let mut hand = Hand::take(&mut c, TAB).expect("taken");
    assert_eq!(acquires(), 1);
    hand.keep(&mut c);
    assert_eq!(acquires(), 1, "fresh: not renewed");
    hand.renewed = Instant::now().checked_sub(HAND_RENEW).expect("clock");
    hand.keep(&mut c);
    assert_eq!(acquires(), 2, "due: renewed");
    assert!(hand.held);
    hand.keep(&mut c);
    assert_eq!(acquires(), 2, "renewed just now");
    let mut unheld = Hand {
        tab: TAB.to_string(),
        held: false,
        renewed: Instant::now().checked_sub(HAND_RENEW).expect("clock"),
    };
    unheld.keep(&mut c);
    assert_eq!(acquires(), 2, "a hand that holds nothing is never renewed");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The last look before the relaunch line is a `status` read as it is typed.
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_never_typed_over_a_person_or_a_holder() {
    let dir = scratch("relaunch-held");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        human_grace_s: 120,
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let status = dir.join("t.status");
    let wait = Err(RelaunchLineError::Wait("held"));
    let hand = Err(RelaunchLineError::Hand);
    for (held, want) in [
        ("OK schema=1 hold=0 hand=- human_ms=800", &wait),
        ("OK schema=1 hold=1 hand=- human_ms=-", &hand),
        ("OK schema=1 hold=0 hand=turn:9 human_ms=-", &hand),
        (
            "OK schema=1 hold=0 hand=lease:orchestrator human_ms=-",
            &hand,
        ),
        ("ERR no such session", &wait),
    ] {
        std::fs::write(&status, held).expect("status");
        let result = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true,
        );
        assert_eq!(&result, want, "{held}");
        assert_eq!(turns(&asked), 0, "{held}: nothing typed");
    }
    std::fs::write(&status, "OK schema=1 hold=0 hand=- human_ms=120000").expect("status");
    type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
            held_since_s: 0,
        },
        |_, _, _| true,
    )
    .expect("nobody at the tab");
    assert_eq!(turns(&asked), 1, "negative control: typed");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// Every look at the job before an act stands between a ctrl-z and that act.
#[cfg(unix)]
#[test]
fn every_look_at_the_job_stands_between_a_ctrl_z_and_the_act() {
    let dir = scratch("jobs");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".into())];
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

/// The program [`PaneProgram`] says opened the agent's terminal.
#[cfg(unix)]
const PTY_OWNER: u32 = 1880;

/// Mock [`Kernel`] answering for an agent run directly in a pane (`shell: None`) or under a shell
/// (`Some`).
#[cfg(unix)]
struct PaneProgram {
    agent: u32,
    shell: Option<u32>,
    opener: &'static str,
}

#[cfg(unix)]
impl PaneProgram {
    /// `agent` the own program of the pty an aterm opened.
    fn execd(agent: u32) -> Self {
        Self {
            agent,
            shell: None,
            opener: "aterm",
        }
    }

    /// `agent` the foreground job of `shell`, in the pty an aterm opened.
    fn under(agent: u32, shell: u32) -> Self {
        Self {
            shell: Some(shell),
            ..Self::execd(agent)
        }
    }
}

#[cfg(unix)]
impl Kernel for PaneProgram {
    fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
        let front = i64::from(self.agent);
        if pid == self.agent {
            Some((self.shell.unwrap_or(PTY_OWNER), front, front))
        } else if self.shell == Some(pid) {
            Some((PTY_OWNER, i64::from(pid), front))
        } else if pid == PTY_OWNER {
            // Started by launchd, on no terminal.
            Some((1, i64::from(PTY_OWNER), 0))
        } else {
            None
        }
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.ids(pid).map(|(ppid, _, _)| ppid)
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        Some((PTY_OWNER, self.opener.to_string()))
    }
}

/// An agent a pane runs as its own program (`aterm -e claude`, `exec claude` in a tab) is no
/// shell's job.
#[cfg(unix)]
#[test]
fn an_agent_its_pane_runs_as_its_own_program_is_refused_before_the_announce_and_any_signal() {
    let dir = scratch("pane-program");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            signal: "ERR busy",
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
    let shell = dead_pid();
    let t = vec![(shell, PTY_OWNER, "zsh".into())];
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
    let run = |o: &Opts, phase: Phase, k: &PaneProgram| {
        put(phase);
        visit(o, &sf, &t, &newer(), k).step
    };
    let signals = || {
        asked.lock().map_or(usize::MAX, |a| {
            a.iter().filter(|l| l.contains(" signal ")).count()
        })
    };
    let announced = Phase::Announced {
        at_s: now - 60,
        asks: 1,
    };
    let execd = PaneProgram::execd(pid);
    let verdict = execd.job(pid);
    let dry = run(
        &Opts {
            dry_run: true,
            ..opts.clone()
        },
        Phase::Pending,
        &execd,
    );
    let before_announce = run(&opts, Phase::Pending, &execd);
    let before_signal = run(&opts, announced.clone(), &execd);
    let phase = load(&opts, SESSION).map(|s| s.phase);
    let typed_before = turns(&asked);
    let signalled = signals();
    let still_running = alive(pid);
    let under = PaneProgram::under(pid, shell);
    let control = run(&opts, Phase::Pending, &under);
    let typed_control = turns(&asked);
    let signal_control = run(&opts, announced, &under);
    let signalled_control = signals();
    let tmux = PaneProgram {
        opener: "tmux",
        ..PaneProgram::execd(pid)
    };
    let tmux_verdict = tmux.job(pid);
    let tmux_pane = run(&opts, Phase::Pending, &tmux);
    let still_running_after = alive(pid);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(
        verdict,
        Some((Job::TabProgram { foreground: true }, PTY_OWNER))
    );
    assert_eq!(dry, "would-refuse:not-a-shell-job");
    assert_eq!(before_announce, "refused:not-a-shell-job");
    assert_eq!(before_signal, "refused:not-a-shell-job");
    assert_eq!(
        phase,
        Some(Phase::Failed("not-a-shell-job".to_string())),
        "the round stopped"
    );
    assert_eq!(typed_before, 0, "nothing typed at the pane");
    assert_eq!(signalled, 0, "no signal asked for");
    assert!(still_running, "never signalled");
    assert_eq!(
        control, "announced:1",
        "CONTROL: a shell between, the path is live"
    );
    assert_eq!(typed_control, 1);
    assert_eq!(
        signal_control, "wait:signal-held",
        "CONTROL: a shell between, the restart asks for the SIGTERM"
    );
    assert_eq!(signalled_control, 1, "the one signal asked for");
    assert_eq!(tmux_verdict, Some((Job::Foreground, PTY_OWNER)));
    assert_eq!(tmux_pane, "held-back:terminal:tmux");
    assert!(still_running_after, "the server sent nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`Kernel::shell_holds_tab`]'s default reads the shell through the kernel's own [`Kernel::ids`],
/// as the job's reads do.
#[cfg(unix)]
#[test]
fn a_shells_hold_on_its_tab_is_read_through_the_kernels_ids() {
    /// Every pid leads its own group, and `.0`'s is its terminal's foreground.
    struct Front(u32);
    impl Kernel for Front {
        fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
            Some((1, i64::from(pid), i64::from(self.0)))
        }
        fn parent(&self, _: u32) -> Option<u32> {
            None
        }
        fn terminal(&self, _: u32) -> Option<(u32, String)> {
            None
        }
    }
    let (shell, agent) = (dead_pid(), dead_pid() + 1);
    assert_eq!(Front(shell).shell_holds_tab(shell), Some(true));
    assert_eq!(Front(agent).shell_holds_tab(shell), Some(false));
    assert_eq!(Front(0).shell_holds_tab(0), None, "no shell");
}

/// A tab's own program an older build announced to is released.
#[cfg(unix)]
#[test]
fn a_tabs_own_program_an_older_build_announced_to_is_released() {
    let dir = scratch("pane-program-release");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let notice = upgrade::prepare_prompt(
        &Version::parse("1.0.0").expect("from"),
        &Version::parse("9.9.9").expect("to"),
        Source::Managed,
        marker,
    );
    write_transcript(
        &opts.home,
        &[
            user_row("fix the bug"),
            user_row(&notice),
            turn_by("claude-opus-5-5", "Saving my work first."),
        ],
    );
    let now = now_s();
    let put = || {
        let st = St {
            phase: Phase::Announced {
                at_s: now - 60,
                asks: 1,
            },
            marker: marker.to_string(),
            asked: vec![marker.to_string()],
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
    let typed = || {
        asked.lock().map_or_else(
            |_| Vec::new(),
            |a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect(),
        )
    };
    let execd = PaneProgram::execd(pid);
    put();
    let refused = visit(&opts, &sf, &[], &newer(), &execd).step;
    let owed = load(&opts, SESSION).map(|s| s.release);
    let released = visit(&opts, &sf, &[], &newer(), &execd).step;
    let lines = typed();
    let kept = load(&opts, SESSION).map(|s| (s.phase, s.release));
    put();
    let control = visit(&opts, &sf, &[], &newer(), &Live).step;
    let control_owed = load(&opts, SESSION).map(|s| s.release);
    let control_next = visit(&opts, &sf, &[], &newer(), &Live).step;
    let control_lines = typed().len();
    let signalled = asked
        .lock()
        .map_or(true, |a| a.iter().any(|l| l.contains(" signal ")));
    let still_running = alive(pid);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(refused, "refused:not-a-shell-job");
    assert_eq!(owed.as_deref(), Some("not-a-shell-job"), "the release owed");
    assert_eq!(released, "released:not-a-shell-job", "typed: {lines:?}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("Upgrade off:"), "{lines:?}");
    assert_eq!(
        kept,
        Some((Phase::Failed("not-a-shell-job".to_string()), String::new())),
        "nothing owed once typed"
    );
    assert_eq!(control, "refused:not-a-shell-job");
    assert_eq!(control_owed.as_deref(), Some(""), "CONTROL: none owed");
    assert!(
        !control_next.starts_with("released"),
        "CONTROL: {control_next}"
    );
    assert_eq!(control_lines, 1, "CONTROL: nothing more typed");
    assert!(!signalled, "nothing signalled");
    assert!(still_running);
}

/// The roster is read again when the directory's stamp changes and, while it holds still, once
/// [`SESSION_ROSTER_RESCAN`] has passed (an in-place write keeps the stamp).
#[test]
fn relaunch_roster_reuses_stable_files_but_rechecks_changes_and_in_place_writes() {
    let mut roster = SessionRosterCache::default();
    let now = Instant::now();
    let stamp = UNIX_EPOCH + Duration::from_secs(1);
    let scans = std::cell::Cell::new(0);
    let scan = || {
        scans.set(scans.get() + 1);
        Some(Vec::new())
    };

    assert!(roster.files_at(Some(stamp), now, scan).is_some());
    assert!(
        roster
            .files_at(Some(stamp), now + Duration::from_millis(250), scan)
            .is_some()
    );
    assert_eq!(
        scans.get(),
        1,
        "a quiet quarter-second poll reuses the roster"
    );
    assert!(
        roster
            .files_at(
                Some(stamp + Duration::from_secs(1)),
                now + Duration::from_millis(500),
                scan
            )
            .is_some()
    );
    assert_eq!(scans.get(), 2, "a new filename bypasses the fallback");
    assert!(
        roster
            .files_at(
                Some(stamp + Duration::from_secs(1)),
                now + Duration::from_millis(500) + SESSION_ROSTER_RESCAN,
                scan
            )
            .is_some()
    );
    assert_eq!(scans.get(), 3, "an in-place edit gets a bounded re-read");
}

#[test]
fn relaunch_roster_retries_an_incomplete_or_unstatable_directory_every_poll() {
    let mut roster = SessionRosterCache::default();
    let now = Instant::now();
    let stamp = Some(UNIX_EPOCH + Duration::from_secs(1));
    assert!(roster.files_at(stamp, now, || None).is_none());
    assert!(
        roster
            .files_at(stamp, now + Duration::from_millis(250), || Some(Vec::new()))
            .is_some(),
        "a transient malformed JSON must not be cached for a second"
    );
    let mut rescanned = false;
    assert!(
        roster
            .files_at(None, now + Duration::from_millis(500), || {
                rescanned = true;
                Some(Vec::new())
            })
            .is_some()
    );
    assert!(rescanned, "missing metadata cannot suppress a roster read");
}

/// Only the relaunch itself (the same conversation, a child of the shell the line was typed at) is
/// adopted and told it was upgraded.
#[cfg(unix)]
#[test]
fn only_the_relaunch_itself_is_adopted_and_told_it_was_upgraded() {
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
        claude_dir: None,
        profile: None,
    };
    let fresh = "11111111-2222-3333-4444-555555555555";
    let adopted = [
        relaunch_of(&Live, &s(SESSION), SESSION, dead, me),
        relaunch_of(&Live, &s(fresh), SESSION, dead, me),
        relaunch_of(&Live, &s(SESSION), SESSION, dead, 1),
        relaunch_of(&Live, &s(SESSION), SESSION, pid, me),
    ];
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
}

/// A relaunch is judged from a parent read.
#[test]
fn a_parent_that_cannot_be_read_is_no_verdict_on_the_relaunch() {
    assert_eq!(relaunch_by_parent(Some(1916), 1916), Relaunch::Ours);
    assert_eq!(relaunch_by_parent(Some(1), 1916), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 1916), Relaunch::Unread);
    assert_eq!(relaunch_by_parent(Some(0), 0), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 0), Relaunch::NotOurs);
    assert_eq!(Live.parent(dead_pid()), None);
    let me = std::process::id();
    assert_eq!(Live.parent(me), ids(me).map(|(ppid, _, _)| ppid));
}

/// The in-flight branch end to end.
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
    let ours = visit(&opts, &sf, &table(), &newer(), &parent(Some(shell)));
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

/// A relaunch stopped `no-resume` whose agent came up after all is that relaunch landed.
#[cfg(unix)]
#[test]
fn a_relaunch_stopped_no_resume_whose_agent_came_up_is_carried_on() {
    let current = Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let me = std::process::id();
    let to = current.managed.clone().expect("the build");
    let case = |name: &str, shell: u32, why: &str, cause: &str, noted: bool| {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        if noted {
            failed_start_saved(&opts, ANOTHER_CONVERSATION, "1.0.0", 600);
            let probed = build_start::starts_with(
                &opts,
                &blank(&sf),
                &to,
                now_s() - 60,
                false,
                false,
                |_| build_start::Probe::Starts,
            );
            assert_eq!(probed, Ok(()));
        }
        let st = St {
            phase: Phase::Failed(why.to_string()),
            pid: dead_pid(),
            shell,
            tab: TAB.to_string(),
            from: "0.9.0".to_string(),
            to: "1.0.0".to_string(),
            source: "managed".to_string(),
            cause: cause.to_string(),
            quiet: true,
            exited_at: now_s(),
            relaunched_at: now_s(),
            failed_at: now_s(),
            streak_why: why.to_string(),
            stop_streak: 1,
            ..St::default()
        };
        save(&opts, SESSION, &st);
        if noted {
            let note = build_start::load(&opts, "managed", "1.0.0").expect("the note");
            assert!(note.blind, "{note:?}");
        }
        let asks = needs_upgrade_inspection(&sf, &current, Some(&st));
        let r = visit(&opts, &sf, &table(), &current, &Live);
        let _ = agent.kill();
        let _ = agent.wait();
        let after = load(&opts, SESSION).expect("record");
        let typed = turns(&asked);
        let reopened = ledger_details(&opts, "reopened:relaunch-landed").len();
        let note = noted.then(|| {
            let moves = build_start::starts_with(&opts, &r, &to, now_s(), false, false, |_| {
                panic!("proved since the failure that stands: no probe")
            });
            (build_start::load(&opts, "managed", "1.0.0"), moves)
        });
        let _ = std::fs::remove_dir_all(&dir);
        (asks, r, after, typed, reopened, note)
    };
    let (asks, r, after, typed, reopened, note) = case("landed-late", me, "no-resume", "", true);
    assert!(asks, "the host's due asks about it");
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(after.phase, Phase::Done, "{after:?}");
    assert_eq!(typed, 0, "a quiet move types nothing");
    assert_eq!(reopened, 1);
    assert_eq!(
        (after.stop_streak, after.streak_why.as_str()),
        (0, ""),
        "the stop taken back is no stop that repeats"
    );
    let (note, moves) = note.expect("noted");
    let note = note.expect("the note");
    assert!(!note.blind, "{note:?}");
    assert_eq!(note.failures, 1, "the other conversation's alone: {note:?}");
    assert_eq!(moves, Ok(()));
    for (name, shell, why, cause) in [
        ("landed-elsewhere", 1, "no-resume", ""),
        ("landed-stale", me, "stale-exit", ""),
        ("landed-exit", me, "no-resume", CAUSE_EXIT),
    ] {
        let (asks, r, after, typed, reopened, _) = case(name, shell, why, cause, false);
        assert!(!asks, "{name}");
        assert_eq!(r.step, "current", "{name}: {r:?}");
        assert_eq!(after.phase, Phase::Failed(why.to_string()), "{name}");
        assert_eq!((typed, reopened), (0, 0), "{name}");
        assert_eq!(after.stop_streak, 1, "{name}: its stop stands");
    }
}

/// A conversation resumed by hand after a `no-resume` stop is the person's, not a relaunch that
/// landed late.
#[cfg(unix)]
#[test]
fn a_conversation_resumed_by_hand_after_a_no_resume_stop_is_never_reopened() {
    use super::super::relaunch::CAUSE_MEMORY;
    const DAY: u64 = 86_400;
    let me = std::process::id();
    let cases = [
        ("hand-memory", CAUSE_MEMORY, "1.0.0", false, DAY),
        ("hand-announced", "", "1.0.0", false, DAY),
        ("hand-old-build", "", "9.9.9", true, 0),
    ];
    for (name, cause, to, quiet, before) in cases {
        let targets = Targets {
            managed: Some(Candidate {
                exe: PathBuf::from("/nonexistent/claude"),
                version: Version::parse(to).expect("version"),
                source: Source::Managed,
            }),
            native: None,
            unanswered: false,
        };
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let stopped = now_s() - before;
        let st = St {
            phase: Phase::Failed("no-resume".to_string()),
            pid: dead_pid(),
            shell: me,
            tab: TAB.to_string(),
            from: "1.0.0".to_string(),
            to: to.to_string(),
            source: "managed".to_string(),
            cause: cause.to_string(),
            quiet,
            exited_at: stopped,
            relaunched_at: stopped,
            failed_at: stopped,
            streak_why: "no-resume".to_string(),
            stop_streak: 1,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        save(&opts, SESSION, &st);
        if to == sf.version {
            assert!(
                !needs_upgrade_inspection(&sf, &targets, Some(&st)),
                "{name}: nothing asks about a current agent's standing stop"
            );
        }
        let r = visit(&opts, &sf, &table(), &targets, &Live);
        let after = load(&opts, SESSION).expect("record");
        let owes = owed(&opts);
        let _ = agent.kill();
        let _ = agent.wait();
        let typed = turns(&asked);
        let reopened = ledger_details(&opts, "reopened:relaunch-landed").len();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(reopened, 0, "{name}: {r:?}");
        assert_eq!(
            after.phase,
            Phase::Failed("no-resume".to_string()),
            "{name}: {r:?}"
        );
        assert!(!owes, "{name}: no continuation is owed");
        assert_eq!(typed, 0, "{name}: nothing typed");
    }
}

/// A build that failed to start for a conversation is not tried again for its hand resume.
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_is_not_retried_for_the_conversations_hand_resume() {
    let stopped = |to: &'static str, ago: u64| {
        move |opts: &Opts| {
            let at = now_s() - ago;
            let st = St {
                phase: Phase::Failed("no-resume".to_string()),
                pid: dead_pid(),
                shell: dead_pid(),
                tab: TAB.to_string(),
                from: "1.0.0".to_string(),
                to: to.to_string(),
                source: "managed".to_string(),
                proc_start: "Thu Oct 1 00:00:00 2026".to_string(),
                salt: at - 600,
                pending_since: at - 600,
                exited_at: at - 5,
                relaunched_at: at - 4,
                failed_at: at,
                streak_why: "no-resume".to_string(),
                stop_streak: 1,
                ..St::default()
            };
            std::fs::write(state_path(opts, SESSION), st.to_json()).expect("the stop");
        }
    };
    let shell = dead_pid();
    let visit = |name: &str, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &[
                    user_row("fix the bug"),
                    turn_by("claude-haiku-4-5", "Fixed."),
                ],
                false,
                &Script::new(shell, usize::MAX, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    let (r, saved, typed) = visit("hand-resume-rests", &stopped("9.9.9", 34));
    let saved = saved.expect("the record");
    assert_eq!(r.step, "wait:failed", "{r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(
        saved.phase,
        Phase::Failed("no-resume".to_string()),
        "the round rests: {saved:?}"
    );
    assert_eq!(
        (saved.streak_why.as_str(), saved.stop_streak),
        ("no-resume", 1),
        "the streak the next stop of this build lengthens"
    );
    assert_ne!(
        saved.proc_start, "Thu Oct 1 00:00:00 2026",
        "bound to the process that holds the conversation now"
    );
    assert_eq!(
        saved.exited_at, 0,
        "no agent of this record is down: the person's runs"
    );
    let (r, saved, _) = visit(
        "hand-resume-rested",
        &stopped("9.9.9", upgrade::RETRY_S + 60),
    );
    assert_eq!(r.step, "rearmed:no-resume", "{r:?}");
    assert_eq!(saved.map(|st| st.phase), Some(Phase::Pending));
}

/// A conversation's relaunch onto `to` (managed) that stopped `no-resume`
/// `ago` seconds ago, as the relaunch saves it ([`save`], the one writer):
/// the round of the process the upgrade ended, in another tab.
#[cfg(unix)]
fn failed_start_saved(opts: &Opts, session: &str, to: &str, ago: u64) {
    let at = now_s() - ago;
    let st = St {
        phase: Phase::Failed("no-resume".to_string()),
        pid: dead_pid(),
        shell: dead_pid(),
        tab: "s-0th3r0th3r0th3r0th3r".to_string(),
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        proc_start: "Thu Oct 1 00:00:00 2026".to_string(),
        salt: at - 600,
        pending_since: at - 600,
        exited_at: at - 5,
        relaunched_at: at - 4,
        failed_at: at,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    let writes = Opts {
        dry_run: false,
        ..opts.clone()
    };
    save(&writes, session, &st);
}

/// A build that failed to start is the build's fact, not only its conversation's.
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_for_any_conversation_moves_no_agent_onto_it() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let _ = probe_finds(build_start::Probe::NoExec);
    let visit_as = |name: &str, dry: bool, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &[
                    user_row("fix the bug"),
                    turn_by("claude-haiku-4-5", "Fixed."),
                ],
                dry,
                &Script::new(dead_pid(), usize::MAX, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    let visit = |name: &str, seed: &dyn Fn(&Opts)| visit_as(name, true, seed);
    let (r, saved, typed) = visit_as("another-conversations-stop", false, &|opts| {
        failed_start_saved(opts, OTHER, "9.9.9", 34);
    });
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(r.step, "wait:build-fails", "another conversation: {r:?}");
    let saved = saved.expect("the record");
    assert_eq!(
        (&saved.phase, saved.wait.as_str()),
        (&Phase::Pending, BUILD_FAILS)
    );
    let (r, _, _) = visit("its-own-stop-rested", &|opts| {
        failed_start_saved(opts, SESSION, "9.9.9", upgrade::RETRY_S + 60);
        let writes = Opts {
            dry_run: false,
            ..opts.clone()
        };
        let mut st = load(&writes, SESSION).expect("the stop");
        st.rearm(now_s());
        let holder = session_files(&opts.home)
            .and_then(|files| files.into_iter().find(|sf| sf.session_id == SESSION))
            .expect("the agent's record");
        st.proc_start = squash(&holder.proc_start);
        st.tab = TAB.to_string();
        st.last_seq = 77;
        st.seq_since_s = now_s() - 3600;
        save(&writes, SESSION, &st);
    });
    assert_eq!(r.step, "wait:build-fails", "its own rested round: {r:?}");
    let (r, _, _) = visit("an-older-builds-stop", &|opts| {
        failed_start_saved(opts, OTHER, "9.9.8", 34);
    });
    assert_eq!(r.step, "would-restart", "{r:?}");
}

/// `build_start::note_failure`, from [`save_at`], notes a `no-resume` stop of a relaunch onto a
/// build once.
#[cfg(unix)]
#[test]
fn a_failed_start_is_noted_against_its_build_once_by_the_one_writer() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let dir = scratch("build-note");
    let opts = drive(&dir);
    let failures =
        |opts: &Opts| build_start::load(opts, "managed", "9.9.9").map_or(0, |note| note.failures);
    failed_start_saved(&opts, SESSION, "9.9.9", 34);
    assert_eq!(failures(&opts), 1, "the stop is the build's failure");
    let stop = load(&opts, SESSION).expect("the stop");
    save(&opts, SESSION, &stop);
    assert_eq!(failures(&opts), 1, "saved again: the same failure");
    let mut kept = St {
        phase: Phase::Pending,
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    kept.keep_stop(stop.failed_start().expect("a failed start"));
    kept.source = "managed".to_string();
    save(&opts, SESSION, &kept);
    assert_eq!(failures(&opts), 1, "a hand resume that keeps the stop");
    let mut restart = St {
        phase: Phase::Done,
        from: "1.0.0".to_string(),
        to: "1.0.0".to_string(),
        source: SAME_PROGRAM.to_string(),
        ..St::default()
    };
    stop.kept_by_restart(TAB, &mut restart);
    save(&opts, SESSION, &restart);
    assert_eq!(failures(&opts), 1, "a restart's record that carries it");
    failed_start_saved(&opts, OTHER, "9.9.9", 10);
    assert_eq!(failures(&opts), 2, "another conversation's stop");
    let unnoted = |name: &str, st: St, opts: &Opts| {
        save(opts, OTHER, &st);
        assert_eq!(
            build_start::load(opts, &st.source, &st.to),
            None,
            "{name}: no failure to start"
        );
    };
    let failed = |why: &str, source: &str, agent: upgrade::Agent| St {
        phase: Phase::Failed(why.to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.7".to_string(),
        source: source.to_string(),
        failed_at: now_s(),
        agent,
        ..St::default()
    };
    unnoted(
        "another reason",
        failed("resumed-elsewhere", "managed", upgrade::Agent::Claude),
        &opts,
    );
    unnoted(
        "its own program",
        failed("no-resume", SAME_PROGRAM, upgrade::Agent::Claude),
        &opts,
    );
    unnoted(
        "a Codex",
        failed("no-resume", "managed", upgrade::Agent::Codex),
        &opts,
    );
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    unnoted(
        "a dry run",
        failed("no-resume", "managed", upgrade::Agent::Claude),
        &dry,
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The gate ([`build_start::starts`]).
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_is_moved_onto_only_once_proved_to_start() {
    use build_start::{BUILD_FAILS, PROBE_GAP_S, Probe, starts_with};
    let dir = scratch("build-gate");
    let opts = drive(&dir);
    let to = Candidate {
        exe: dir.join("claude"),
        version: Version::parse("9.9.9").expect("v"),
        source: Source::Managed,
    };
    let r = Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: String::new(),
    };
    let never = |_: &Path| -> Probe { panic!("no probe is run") };
    let now = now_s();
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(starts_with(&dry, &r, &to, now, false, false, never), Ok(()));
    assert_eq!(
        starts_with(&opts, &r, &to, now, false, false, |_| Probe::NoExec),
        Ok(())
    );
    assert_eq!(
        starts_with(&opts, &r, &to, now + 1, false, false, never),
        Ok(())
    );
    assert_eq!(ledger_details(&opts, "build-probe:no-exec").len(), 1);
    failed_start_saved(&opts, SESSION, "9.9.9", 34);
    assert_eq!(
        starts_with(&dry, &r, &to, now, false, false, never),
        Err(BUILD_FAILS)
    );
    let ran = std::cell::Cell::new(0);
    let exits = |_: &Path| {
        ran.set(ran.get() + 1);
        Probe::Exited
    };
    assert_eq!(
        starts_with(&opts, &r, &to, now, false, false, exits),
        Err(BUILD_FAILS)
    );
    assert_eq!(ran.get(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S - 1, false, false, never),
        Err(BUILD_FAILS),
        "inside the gap"
    );
    assert_eq!(ledger_details(&opts, "build-probe:exited").len(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S, false, false, |_| {
            Probe::Starts
        }),
        Ok(())
    );
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S + 1, false, false, never),
        Ok(())
    );
    assert_eq!(ledger_details(&opts, "build-probe:starts").len(), 1);
    let mut again = St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        relaunched_at: now + PROBE_GAP_S + 5,
        failed_at: now + PROBE_GAP_S + 9,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    save_at(
        &opts,
        "0badf00d-7777-2222-3333-444455556666",
        &again,
        now + PROBE_GAP_S + 9,
    );
    let note = build_start::load(&opts, "managed", "9.9.9").expect("the note");
    assert!(note.blind, "{note:?}");
    assert_eq!(ledger_details(&opts, "build-probe:blind").len(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to, now + 10 * PROBE_GAP_S, false, false, never),
        Err(BUILD_FAILS),
        "no probe proves a blind build again"
    );
    let older = Candidate {
        version: Version::parse("9.9.8").expect("v"),
        ..to.clone()
    };
    failed_start_saved(&opts, SESSION, "9.9.8", 34);
    assert_eq!(
        starts_with(&opts, &r, &older, now, false, false, |_| Probe::Starts),
        Ok(())
    );
    again.to = "9.9.8".to_string();
    again.relaunched_at = now - 1;
    save_at(
        &opts,
        "0badf00d-8888-2222-3333-444455556666",
        &again,
        now + 1,
    );
    let note = build_start::load(&opts, "managed", "9.9.8").expect("the note");
    assert!(!note.blind, "{note:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A crash is no time to try a build nothing has run.
#[cfg(unix)]
#[test]
fn a_crashs_relaunch_moves_only_onto_a_build_proved_to_start() {
    let case = |name: &str, found: build_start::Probe| {
        let dir = std::fs::canonicalize(scratch(name)).expect("scratch");
        let opts = Opts {
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        let ran = probe_finds(found);
        seed_builds(&opts);
        let versions = native_root(&opts.home);
        std::fs::create_dir_all(&versions).expect("versions");
        for build in ["2.1.281", "99.9.9"] {
            std::fs::write(versions.join(build), "").expect("a build");
        }
        std::fs::create_dir_all(opts.home.join(".local/bin")).expect("bin");
        std::os::unix::fs::symlink(versions.join("99.9.9"), opts.home.join(".local/bin/claude"))
            .expect("the native link");
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("sessions");
        file_transcript_in(&opts.home.join(".claude"), "p");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let mut shell = stand_in_shell();
        let dead = dead_pid();
        let start = "Thu Sep 24 01:02:03 2026";
        std::fs::write(
            opts.home.join(format!(".claude/sessions/{dead}.json")),
            format!(
                r#"{{"pid":{dead},"sessionId":"{SESSION}","cwd":"/","version":"2.1.281","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the crash's record");
        let program = versions.join("2.1.281");
        let snap = Snapshot {
            tab: TAB.to_string(),
            pid: dead,
            start: start.to_string(),
            shell: shell.id(),
            program: program.clone(),
            argv: vec![program.to_string_lossy().into_owned()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("2.1.281".to_string()),
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: Some(Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
            stored_login: false,
            profile: None,
        };
        let r = after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
        let note = build_start::load(&opts, "native", "99.9.9");
        let to = Candidate {
            exe: versions.join("99.9.9"),
            version: Version::parse("99.9.9").expect("v"),
            source: Source::Native,
        };
        let later = build_start::starts(&opts, &r, &to, now_s(), false, true);
        let probes = ledger_details(&opts, &format!("build-probe:{}", found.word())).len();
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (
            r.to,
            note.expect("the build's note"),
            ran.get(),
            later,
            probes,
        )
    };
    let (to, note, ran, later, probes) =
        case("crash-onto-a-failing-build", build_start::Probe::Exited);
    assert_eq!(to, "2.1.281(same)", "back on the program it ran");
    assert_eq!(
        (note.failures, note.probe.as_str(), note.proved()),
        (0, "exited", false),
        "the failing probe is the build's fact, no failure noted: {note:?}"
    );
    assert_eq!((ran, probes), (1, 1), "probed once, said once");
    assert_eq!(later, Err(BUILD_FAILS), "inside the probe's gap: {note:?}");
    let (to, note, ran, later, probes) =
        case("crash-onto-a-build-that-starts", build_start::Probe::Starts);
    assert_eq!(
        to, "99.9.9(native)",
        "moved onto the build its probe proved"
    );
    assert_eq!(
        (note.failures, note.probe.as_str(), note.proved()),
        (0, "starts", true),
        "proved with no failure noted: {note:?}"
    );
    assert_eq!(later, Ok(()));
    assert_eq!(
        (ran, probes),
        (1, 1),
        "a later move does not probe it again"
    );
}

/// The upgrade's own first move onto a build never run here is probed first.
#[cfg(unix)]
#[test]
fn the_upgrades_first_move_onto_a_build_never_run_here_is_probed_first() {
    use std::os::unix::process::ExitStatusExt as _;
    let rig = |name: &str| {
        let behind = St {
            pending_since: now_s() - 3 * 3600,
            ..settled()
        };
        quietly(|| Parked::new(name, Answers::default(), behind, 3600))
    };
    let h = rig("first-move-probed-exiting");
    let ran = probe_finds(build_start::Probe::Exited);
    let r = h.visit();
    assert_eq!(r.step, "wait:build-fails", "{r:?}");
    assert!(h.typed().is_empty(), "nothing typed: {:?}", h.typed());
    assert!(h.details("terminated").is_empty(), "not signalled");
    assert_eq!(ran.get(), 1, "probed before the move");
    let note = build_start::load(&h.opts, "managed", "9.9.9").expect("the build's note");
    assert_eq!(
        (note.failures, note.probe.as_str(), note.proved()),
        (0, "exited", false),
        "{note:?}"
    );
    let again = h.visit();
    assert_eq!(again.step, "wait:build-fails", "{again:?}");
    assert_eq!(ran.get(), 1, "not probed again inside its gap");
    drop(h);
    for (found, word, proved) in [
        (build_start::Probe::NoExec, "no-exec", false),
        (build_start::Probe::Starts, "starts", true),
    ] {
        let mut h = rig(&format!("first-move-probed-{word}"));
        let ran = probe_finds(found);
        let ended = h.reaped();
        let r = h.visit();
        let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
        assert_eq!(r.step, "wait:shell-prompt", "{word}: moved: {r:?}");
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{word}: {r:?}");
        assert_eq!(
            h.details("terminated"),
            ["SIGTERM (idle: nothing asked or typed; resumed as it was)"],
            "{word}"
        );
        assert_eq!(ran.get(), 1, "{word}: probed once");
        let note = build_start::load(&h.opts, "managed", "9.9.9").expect("the build's note");
        assert_eq!(
            (note.failures, note.probe.as_str(), note.proved()),
            (0, word, proved),
            "{note:?}"
        );
        drop(h);
    }
    let h = rig("first-move-onto-a-build-found-exiting");
    let ran = probe_finds(build_start::Probe::Exited);
    let to = newer().managed.expect("the build");
    assert_eq!(
        build_start::proved(&h.opts, &blank(&h.sf), &to, now_s()),
        Err(BUILD_FAILS)
    );
    let note = build_start::load(&h.opts, "managed", "9.9.9").expect("the build's note");
    assert_eq!(
        (note.failures, note.probe.as_str()),
        (0, "exited"),
        "{note:?}"
    );
    let r = h.visit();
    assert_eq!(r.step, "wait:build-fails", "{r:?}");
    assert!(h.typed().is_empty(), "nothing typed: {:?}", h.typed());
    assert!(h.details("terminated").is_empty(), "not signalled");
    assert_eq!(ran.get(), 1, "the crash's probe alone");
}

/// [`build_start::probe`] runs the build as its relaunch would, without an agent.
#[cfg(unix)]
#[test]
fn the_start_probe_runs_the_build_with_its_help_flag_bounded() {
    use build_start::{Probe, probe, probe_within};
    let dir = scratch("build-probe");
    let crashing = dir.join("crashing");
    sh_script(
        &crashing,
        "case \"$1\" in --version) echo \"2.1.999 (Claude Code)\"; exit 0 ;; esac\nexit 1",
    );
    let starts = dir.join("starts");
    sh_script(
        &starts,
        "case \"$1\" in --help) echo usage; exit 0 ;; esac\nexit 1",
    );
    let hangs = dir.join("hangs");
    sh_script(&hangs, "sleep 600");
    assert_eq!(
        version_of(&crashing),
        Probed::Version(Version::parse("2.1.999").expect("v"))
    );
    assert_eq!(
        probe(&crashing),
        Probe::Exited,
        "a build that crashes at start"
    );
    assert_eq!(probe(&starts), Probe::Starts);
    assert_eq!(probe(&dir.join("absent")), Probe::NoExec);
    assert_eq!(
        probe_within(&hangs, Duration::from_millis(300)),
        Probe::TimedOut
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A round whose build fails to start releases its agent, once.
#[cfg(unix)]
#[test]
fn a_round_onto_a_build_that_fails_to_start_releases_its_agent_once() {
    struct Round {
        name: &'static str,
        quiet: bool,
        /// The agent's last answer; `READY` adds the round's marker.
        answer: &'static str,
        ready: bool,
        record: fn(&Parked) -> St,
        /// A dry read with no failure noted, and one with it.
        control: &'static str,
        dry: &'static str,
        /// The record after the three visits, their steps, and its ledger.
        after: fn(&St, &[String], &Parked),
    }
    fn asked(h: &Parked) -> St {
        St {
            markers: vec![ASKED_MARKER.to_string()],
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..announced_to(h, ASKED_MARKER)
        }
    }
    fn owing(h: &Parked) -> St {
        St {
            phase: Phase::Pending,
            release: "retargeted".to_string(),
            asked: vec![ASKED_MARKER.to_string()],
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            pending_since: now_s() - 3 * 3600,
            ..settled()
        }
    }
    fn void(after: &St, _: &[String], _: &Parked) {
        assert_eq!(
            after.round_markers().count(),
            0,
            "its READY void: {after:?}"
        );
    }
    fn pending(after: &St, _: &[String], _: &Parked) {
        assert_eq!(after.phase, Phase::Pending);
        assert_eq!(after.wait, BUILD_FAILS);
    }
    let rounds = [
        Round {
            name: "ready",
            quiet: false,
            answer: "Saved.",
            ready: true,
            record: asked,
            control: "would-restart",
            dry: "would-fail:build-fails",
            after: |after, steps, h| {
                assert_eq!(steps[0], "released:build-fails", "{steps:?}");
                assert_eq!(after.phase, Phase::Failed(BUILD_FAILS.to_string()));
                void(after, steps, h);
                let why = h.details("failed:build-fails");
                assert_eq!(why.len(), 1, "said once: {why:?}");
            },
        },
        Round {
            name: "late-ready",
            quiet: false,
            answer: "Done.",
            ready: true,
            record: |h| {
                let mut st = asked(h);
                st.give_up(now_s() - 600);
                assert_eq!(st.release, "gave-up");
                st
            },
            control: "would-restart",
            dry: "wait:release:ready",
            after: void,
        },
        Round {
            name: "owed-ask",
            quiet: false,
            answer: "Done.",
            ready: true,
            record: owing,
            control: "would-announce",
            dry: "would-release:retargeted",
            after: pending,
        },
        Round {
            name: "owed-quiet",
            quiet: true,
            answer: "Done.",
            ready: true,
            record: owing,
            control: "would-restart",
            dry: "would-release:retargeted",
            after: pending,
        },
        Round {
            name: "announced",
            quiet: false,
            answer: "Not yet: the suite is still running.",
            ready: false,
            record: |h| St {
                phase: Phase::Announced {
                    at_s: now_s() - upgrade::REASK_S - 60,
                    asks: 1,
                },
                ..asked(h)
            },
            control: "would-announce",
            dry: "would-fail:build-fails",
            after: void,
        },
    ];
    for round in rounds {
        let rig = |name: &str| {
            let park = || Parked::new(name, Answers::default(), settled(), 3600);
            let h = if round.quiet { quietly(park) } else { park() };
            let answer = if round.ready {
                format!("{}\\n{ASKED_MARKER}", round.answer)
            } else {
                round.answer.to_string()
            };
            asked_then_answered(&h, ASKED_MARKER, &answer);
            std::fs::write(state_path(&h.opts, SESSION), (round.record)(&h).to_json())
                .expect("state");
            h
        };
        let name = round.name;
        let mut control = rig(&format!("{name}-onto-a-build"));
        control.opts.dry_run = true;
        assert_eq!(control.visit().step, round.control, "{name}");
        drop(control);
        let mut dry = rig(&format!("{name}-onto-a-failing-build-dry"));
        dry.opts.dry_run = true;
        failed_start_saved(&dry.opts, ANOTHER_CONVERSATION, "9.9.9", 34);
        let before = (load(&dry.opts, SESSION), ledger_lines(&dry.opts));
        assert_eq!(dry.visit().step, round.dry, "{name}");
        assert!(dry.typed().is_empty(), "{name}: {:?}", dry.typed());
        let after = (load(&dry.opts, SESSION), ledger_lines(&dry.opts));
        assert_eq!(after, before, "{name}: written");
        drop(dry);
        let h = rig(&format!("{name}-onto-a-failing-build"));
        let (steps, releases, after) = onto_a_failing_build(&h);
        assert_eq!(releases, 1, "{name}: released once: {steps:?}");
        assert!(after.release.is_empty(), "{name}: nothing owed: {after:?}");
        (round.after)(&after, &steps, &h);
    }
}

/// The conversation whose relaunch onto a build found it failing to start,
/// in another tab ([`failed_start_saved`]).
#[cfg(unix)]
const ANOTHER_CONVERSATION: &str = "0badf00d-9999-2222-3333-444455556666";

/// The READY marker of the round the tests below asked.
#[cfg(unix)]
const ASKED_MARKER: &str = "ATERM-UPGRADE-READY-0badf00d";

/// `h`'s agent, its round as the test left it, visited three times after
/// [`ANOTHER_CONVERSATION`]'s relaunch onto 9.9.9 failed to start (34 s
/// ago; no probe can run `/nonexistent/claude`, named so: [`probe_finds`]):
/// each visit's step, how many release lines were typed into the tab, and
/// the record after.
#[cfg(unix)]
fn onto_a_failing_build(h: &Parked) -> (Vec<String>, usize, St) {
    let _ = probe_finds(build_start::Probe::NoExec);
    failed_start_saved(&h.opts, ANOTHER_CONVERSATION, "9.9.9", 34);
    let steps = (0..3).map(|_| h.visit().step).collect();
    let releases = h
        .typed()
        .iter()
        .filter(|t| t.contains(upgrade::RELEASE_HEAD.trim_end()))
        .count();
    (steps, releases, load(&h.opts, SESSION).expect("the record"))
}

/// The owner's `--now` probes a blind build again (one a move failed on after a passing probe,
/// which no probe proves again).
#[cfg(unix)]
#[test]
fn the_owners_now_probes_a_blind_build_again() {
    let rig = |name: &str, now_word: bool| {
        let behind = St {
            pending_since: now_s() - 3 * 3600,
            ..settled()
        };
        let h = quietly(|| Parked::new(name, Answers::default(), behind, 3600));
        build_start::PROBE.with(|named| *named.borrow_mut() = None);
        let exe = h.dir.join("claude-9.9.9");
        sh_script(&exe, "case \"$1\" in --help) exit 0 ;; esac\nexit 1");
        let targets = Targets {
            managed: Some(Candidate {
                exe,
                version: Version::parse("9.9.9").expect("version"),
                source: Source::Managed,
            }),
            native: None,
            unanswered: false,
        };
        failed_start_saved(&h.opts, ANOTHER_CONVERSATION, "9.9.9", 600);
        let to = targets.managed.clone().expect("the build");
        let r = blank(&h.sf);
        assert_eq!(
            build_start::starts_with(&h.opts, &r, &to, now_s() - 300, false, false, |_| {
                build_start::Probe::Starts
            }),
            Ok(())
        );
        failed_start_saved(&h.opts, "0badf00d-7777-2222-3333-444455556666", "9.9.9", 30);
        assert!(build_start::load(&h.opts, "managed", "9.9.9").is_some_and(|n| n.blind));
        if now_word {
            let mut st = load(&h.opts, SESSION).expect("the round");
            st.request = Request::Now;
            st.request_tab = TAB.to_string();
            st.request_at = now_s();
            save(&h.opts, SESSION, &st);
        }
        let before = h.details("build-probe:starts").len();
        let r = visit(
            &h.opts,
            &h.sf,
            &[(dead_pid(), 1, "zsh".into())],
            &targets,
            &Script::new(dead_pid(), usize::MAX, None),
        );
        let probed = h.details("build-probe:starts").len() - before;
        (h, r, probed)
    };
    let (h, r, probed) = rig("blind-build-unasked", false);
    assert_eq!(r.step, "wait:build-fails", "{r:?}");
    assert_eq!(probed, 0, "not probed");
    drop(h);
    let (_h, r, probed) = rig("blind-build-owners-now", true);
    assert_eq!(probed, 1, "probed: {r:?}");
    assert_ne!(r.step, "wait:build-fails", "moved: {r:?}");
}

/// The model rule reads an agent's `--settings` off its process's argv only where that process is
/// the record's (its kernel start against Claude's `procStart`).
#[cfg(unix)]
#[test]
fn the_settings_chain_reads_no_recycled_pids_settings_flag() {
    let dir = scratch("model-settings-recycled-pid");
    let opts = Opts {
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let user = opts.home.join(".claude");
    let catalog = user.join("cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work");
    let proj = user.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let flag = dir.join("team.json");
    std::fs::write(&flag, r#"{"availableModels":["claude-fable-5-1"]}"#).expect("team");
    let mut holder = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("read line")
        .arg("--settings")
        .arg(&flag)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the process");
    for _ in 0..3_000 {
        if atpkg::caller_shell::process_args(holder.id())
            .is_some_and(|a| a.argv.iter().any(|arg| arg == "--settings"))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let its_start = kernel_start(holder.id()).expect("its start");
    let verdict = |proc_start: &str| {
        let sf = SessionFile {
            pid: holder.id(),
            proc_start: proc_start.to_string(),
            claude_dir: Some(user.clone()),
            cwd: work.to_string_lossy().into_owned(),
            ..record("2.1.296")
        };
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, &sf, None, &mctx, false, None, None).verdict
    };
    let own = verdict(&its_start);
    let recycled = verdict("Thu Oct  1 00:00:00 2026");
    let _ = holder.kill();
    let _ = holder.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(own, ModelVerdict::Keep("model-current"), "its own flag");
    assert_eq!(
        recycled,
        ModelVerdict::Due {
            from: "claude-fable-5-1".to_string(),
            to: "claude-opus-5-5".to_string(),
        },
        "a recycled pid's flag is not the record's"
    );
}

/// [`St::keep_stop`] at [`St::retarget`].
#[test]
fn a_hand_resume_keeps_the_stop_of_the_build_that_failed_to_start_for_it() {
    let running = Version::parse("2.1.296").expect("v");
    let target = |version: &str, source: Source| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(version).expect("v"),
        source,
    };
    let to = target("2.1.999", Source::Managed);
    let stopped = |why: &str| St {
        phase: Phase::Failed(why.to_string()),
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pid: 41,
        shell: 40,
        salt: 100,
        pending_since: 100,
        proc_start: "A".to_string(),
        exited_at: 990,
        relaunched_at: 991,
        relaunched_pid: 42,
        failed_at: 995,
        streak_why: why.to_string(),
        stop_streak: 1,
        notices: 2,
        ..St::default()
    };
    let (st, how) = St::retarget(
        Some(stopped("no-resume")),
        &running,
        &to,
        None,
        proc("B"),
        1_029,
    );
    assert_eq!(how, Retarget::Rested);
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(
        (st.failed_at, st.streak_why.as_str(), st.stop_streak),
        (995, "no-resume", 1),
        "the stop, its rest and its streak"
    );
    assert_eq!(
        (
            st.proc_start.as_str(),
            st.from.as_str(),
            st.to.as_str(),
            st.behind_since(),
            st.tab.as_str()
        ),
        ("B", "2.1.296", "2.1.999", 1_029, TAB),
        "bound to the process that holds the conversation now"
    );
    assert_eq!(
        (st.exited_at, st.relaunched_pid, st.pid, st.notices),
        (0, 0, 0, 0),
        "nothing of the ended agent's: its exit, what its relaunch brought up, its asks"
    );
    let (again, how) = St::retarget(Some(st.clone()), &running, &to, None, proc("B"), 1_100);
    assert_eq!(
        (how, again.phase.clone()),
        (Retarget::Kept, st.phase.clone())
    );
    let mut next = again;
    next.rearm(995 + upgrade::RETRY_S + 1);
    assert_eq!(next.phase, Phase::Pending);
    let at = 995 + upgrade::RETRY_S + 600;
    next.stop("no-resume", at);
    assert_eq!(next.stop_streak, 2, "a stop that repeats");
    assert_eq!(next.failed_at, at + upgrade::rest_extension(2));
    let dir = scratch("kept-stopped");
    let opts = drive(&dir);
    let r = Report {
        pid: 7,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "2.1.296".to_string(),
        to: "2.1.999(managed)".to_string(),
        step: String::new(),
    };
    rested_said(&opts, &r, &st, 1_029);
    let said = ledger_details(&opts, "kept-stopped:no-resume");
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(
        load(&opts, SESSION).map(|st| st.phase),
        Some(st.phase.clone()),
        "saved"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let fresh = |prior: St, to: &Candidate| {
        let (st, how) = St::retarget(Some(prior), &running, to, None, proc("B"), 1_029);
        (st.phase, how)
    };
    let pending = (Phase::Pending, Retarget::Anew);
    let (kept, how) = St::retarget(
        Some(stopped("no-resume")),
        &running,
        &to,
        None,
        proc("A"),
        1_029,
    );
    assert_eq!(
        (kept.phase, how),
        (Phase::Failed("no-resume".to_string()), Retarget::Kept),
        "its own process: as it was"
    );
    assert_eq!(
        fresh(stopped("relaunch-refused"), &to),
        pending,
        "the line's"
    );
    assert_eq!(
        fresh(stopped("resumed-elsewhere"), &to),
        pending,
        "the process's"
    );
    assert_eq!(
        fresh(stopped("no-resume"), &target("2.1.999", Source::Native)),
        pending,
        "another install of the build"
    );
    let same_program = St {
        source: SAME_PROGRAM.to_string(),
        ..stopped("no-resume")
    };
    assert_eq!(
        fresh(same_program, &to),
        pending,
        "a relaunch that kept its program"
    );
    let codex = St {
        agent: upgrade::Agent::Codex,
        ..stopped("no-resume")
    };
    assert_eq!(fresh(codex, &to).0, Phase::Pending, "a Codex record");
    assert_eq!(
        fresh(stopped("no-resume"), &target("2.1.1000", Source::Managed)),
        pending,
        "a newer build"
    );
}

/// A restart on its own program keeps the stop of a build that failed to start for the conversation
/// ([`St::failed_start`] on its record, through a second restart too).
#[test]
fn a_restart_on_its_own_program_keeps_the_stop_of_a_build_that_failed_to_start() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_MEMORY};
    let running = Version::parse("2.1.296").expect("v");
    let target = |version: &str| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(version).expect("v"),
        source: Source::Managed,
    };
    let to = target("2.1.999");
    let stopped = St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pid: 41,
        salt: 100,
        pending_since: 100,
        proc_start: "A".to_string(),
        failed_at: 995,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    let (rested, how) = St::retarget(Some(stopped), &running, &to, None, proc("B"), 1_029);
    assert_eq!(how, Retarget::Rested, "precondition: the hand resume rests");
    let restarted = |prior: &St, cause: &str, phase: Phase| {
        let mut st = St {
            phase,
            from: "2.1.296".to_string(),
            to: "2.1.296".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: cause.to_string(),
            tab: TAB.to_string(),
            salt: 1_500,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        St::from_json(&st.to_json()).expect("the record reads back")
    };
    let landed = restarted(&rested, CAUSE_MEMORY, Phase::Done);
    let (next, how) = St::retarget(Some(landed.clone()), &running, &to, None, proc("C"), 1_700);
    assert_eq!(how, Retarget::Rested, "{next:?}");
    assert_eq!(next.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(
        (next.failed_at, next.streak_why.as_str(), next.stop_streak),
        (995, "no-resume", 1),
        "the stop, its rest and its streak"
    );
    assert_eq!(
        (next.proc_start.as_str(), next.to.as_str()),
        ("C", "2.1.999"),
        "bound to the process that holds the conversation now"
    );
    let twice = restarted(&landed, CAUSE_EXIT, Phase::Done);
    let (next, how) = St::retarget(Some(twice), &running, &to, None, proc("D"), 1_800);
    assert_eq!((how, next.failed_at), (Retarget::Rested, 995), "twice");
    let mut refused = restarted(&rested, CAUSE_MEMORY, Phase::Exiting { at_s: 1_500 });
    refused.stop("relaunch-refused", 1_510);
    let (next, how) = St::retarget(Some(refused), &running, &to, None, proc("B"), 1_600);
    assert_eq!(
        (how, next.phase, next.failed_at),
        (
            Retarget::Rested,
            Phase::Failed("no-resume".to_string()),
            995
        ),
        "a restart that stopped"
    );
    let (newer, how) = St::retarget(
        Some(landed),
        &running,
        &target("2.1.1000"),
        None,
        proc("C"),
        1_700,
    );
    assert_eq!(
        (how, newer.phase),
        (Retarget::Anew, Phase::Pending),
        "a newer build"
    );
    let plain = St {
        phase: Phase::Pending,
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let (next, how) = St::retarget(
        Some(restarted(&plain, CAUSE_MEMORY, Phase::Done)),
        &running,
        &to,
        None,
        proc("C"),
        1_700,
    );
    assert_eq!(
        (how, next.phase),
        (Retarget::Anew, Phase::Pending),
        "no stop"
    );
}

/// A restart left in flight acts on its tab only while it is fresh and its shell is there.
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
    assert_eq!(steps(&opts), ["wait:no-socket"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The stale-exit bound counts from the exit a look saw, not the signal.
#[test]
fn a_restart_in_flight_is_stale_from_its_seen_exit_not_its_signal() {
    let dir = scratch("stale-from-exit");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let signal = now - STALE_S - 60;
    let cases = [
        ("aaaaaaaa-0000-0000-0000-000000000021", now - 10, 0),
        ("aaaaaaaa-0000-0000-0000-000000000022", now - STALE_S - 1, 0),
        ("aaaaaaaa-0000-0000-0000-000000000023", 0, 0),
        (
            "aaaaaaaa-0000-0000-0000-000000000024",
            signal + 30,
            now - 10,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000025",
            signal + 30,
            now - STALE_S - 1,
        ),
    ];
    for (session, exited_at, hand_seen_s) in cases {
        let st = St {
            phase: Phase::Exiting { at_s: signal },
            pid: dead,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            exited_at,
            hand_seen_s,
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let mut got: Vec<(String, String)> = orphans(&opts, &[], None)
        .into_iter()
        .map(|r| (r.session, r.step))
        .collect();
    got.sort();
    let steps: Vec<&str> = got.iter().map(|(_, step)| step.as_str()).collect();
    assert_eq!(
        steps,
        [
            "wait:no-socket",
            "failed:stale-exit",
            "failed:stale-exit",
            "wait:no-socket",
            "failed:stale-exit"
        ]
    );
    assert_eq!(
        load(&opts, cases[0].0).map(|s| (s.phase, s.exited_at)),
        Some((Phase::Exiting { at_s: signal }, now - 10)),
        "carried, its exit as the carry saw it"
    );
    assert_ne!(
        load(&opts, cases[2].0).map_or(0, |s| s.exited_at),
        0,
        "the first sighting is stamped all the same"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The orphan pass waits out a hand standing on the tab now, before its clock is read.
#[cfg(unix)]
#[test]
fn the_orphan_pass_waits_out_a_hand_on_a_restarts_tab() {
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    for (hold, want) in [
        ("hold=1", super::super::relaunch::WAIT_HAND_LIFTS),
        ("hold=0", "failed:stale-exit"),
    ] {
        let dir = scratch("stale-under-hand");
        let (sock, asked) = instance(&dir);
        std::fs::write(
            dir.join("t.status"),
            format!("OK schema=1 {hold} hand=- human_ms=-"),
        )
        .expect("status");
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let st = St {
            phase: Phase::Exiting { at_s: now - 900 },
            pid: dead,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            exited_at: now - 900,
            ..St::default()
        };
        let session = "aaaaaaaa-0000-0000-0000-000000000026";
        save(&opts, session, &st);
        let steps: Vec<String> = orphans(&opts, &[], None)
            .into_iter()
            .map(|r| r.step)
            .collect();
        assert_eq!(steps, [want], "{hold}");
        let st = load(&opts, session).expect("kept");
        if hold == "hold=1" {
            assert!(matches!(st.phase, Phase::Exiting { .. }), "{st:?}");
            assert!(st.hand_seen_s >= now, "{st:?}");
            assert_eq!(turns(&asked), 0, "nothing typed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The orphan pass stamps when a Claude Code restart's agent was seen gone ([`St::exited_at`], as
/// the Codex lane does).
#[test]
fn a_restart_whose_agent_the_orphan_pass_sees_gone_says_when() {
    let dir = scratch("seen-gone");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000011",
            Phase::Exiting {
                at_s: now - STALE_S - 1,
            },
            dead,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000012",
            Phase::Exiting { at_s: now },
            dead,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000013",
            Phase::Exiting { at_s: now },
            me,
        ),
    ];
    for (session, phase, pid) in &cases {
        let st = St {
            phase: phase.clone(),
            pid: *pid,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let seen = |session: &str| load(&opts, session).map_or(0, |st| st.exited_at);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let _ = orphans(&dry, &[], None);
    assert!(cases.iter().all(|(s, _, _)| seen(s) == 0), "a dry run");
    let _ = orphans(&opts, &[], None);
    assert_ne!(seen(cases[0].0), 0, "expired: seen gone first");
    assert_eq!(
        load(&opts, cases[0].0).map(|s| s.phase),
        Some(Phase::Failed("stale-exit".to_string()))
    );
    assert_ne!(seen(cases[1].0), 0, "carried on: seen gone");
    assert_eq!(seen(cases[2].0), 0, "still exiting: not gone");
    let mut again = load(&opts, cases[1].0).expect("stamped");
    assert_ne!(again.exited_at, 0);
    let _ = again.signalled(me, me, TAB, "claude --resume x".to_string(), now);
    assert_eq!(again.exited_at, 0, "a new signal: not seen gone");
    save(&opts, cases[1].0, &again);
    let _ = orphans(&opts, &[], None);
    assert_eq!(seen(cases[1].0), 0, "alive: still not gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ledger rows of `opts` whose step is `step`, by the conversation each
/// names.
fn noted_for(opts: &Opts, step: &str, session: &str) -> usize {
    std::fs::read_to_string(ledger_path(opts))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
        .filter(|v| v.get("session").and_then(Value::as_str) == Some(session))
        .count()
}

/// A hung exit (a SIGTERMed Claude Code stuck in its own shutdown keeps its session file) is said
/// once past [`STALE_S`] (`stuck:exiting`, keyed on [`St::noted`]) and only noted.
#[cfg(unix)]
#[test]
fn a_hung_exit_past_its_bound_is_noted_once_and_nothing_is_forced() {
    for (case, age, notes) in [("past", STALE_S + 60, 1), ("within", 60, 0)] {
        let h = Parked::new(
            &format!("stuck-exiting-{case}"),
            Answers::default(),
            settled(),
            3600,
        );
        let at_s = now_s() - age;
        save(
            &h.opts,
            SESSION,
            &St {
                phase: Phase::Exiting { at_s },
                pid: h.sf.pid,
                ..settled()
            },
        );
        let steps = [h.visit().step, h.visit().step];
        assert_eq!(steps, ["wait:exiting", "wait:exiting"], "{case}");
        let said = h.details("stuck:exiting");
        assert_eq!(said.len(), notes, "{case}: said once: {said:?}");
        let kept = load(&h.opts, SESSION).expect("state");
        assert_eq!(
            kept.phase,
            Phase::Exiting { at_s },
            "{case}: still in flight, never a failure"
        );
        let signalled = h
            .asked
            .lock()
            .map_or(true, |a| a.iter().any(|l| l.contains(" signal ")));
        assert!(!signalled, "{case}: nothing is signalled");
        assert!(h.typed().is_empty(), "{case}: nothing is typed");
        let shown = rows(&h.opts);
        assert_eq!(
            shown.first().and_then(|r| r.stall(now_s())).as_deref(),
            (notes == 1).then_some("stuck:exiting"),
            "{case}: the ledger and --status agree"
        );
    }
}

/// A hung agent that removed its session file while its pid lives is noted once past [`STALE_S`]
/// by the orphan pass, which acts on nothing.
#[test]
fn a_hung_exit_with_no_session_file_is_noted_once_by_the_orphan_pass() {
    let dir = scratch("stuck-orphan");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (me, now) = (std::process::id(), now_s());
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000031",
            now - STALE_S - 60,
            1,
        ),
        ("aaaaaaaa-0000-0000-0000-000000000032", now - 60, 0),
    ];
    for (session, at_s, _) in cases {
        let st = St {
            phase: Phase::Exiting { at_s },
            pid: me,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert!(orphans(&dry, &[], None).is_empty());
    assert_eq!(
        noted_for(&opts, "stuck:exiting", cases[0].0),
        0,
        "a dry run"
    );
    let first = orphans(&opts, &[], None);
    let second = orphans(&opts, &[], None);
    assert!(
        first.is_empty() && second.is_empty(),
        "nothing acted on: {first:?} {second:?}"
    );
    for (session, at_s, notes) in cases {
        assert_eq!(
            noted_for(&opts, "stuck:exiting", session),
            notes,
            "{session}"
        );
        assert_eq!(
            load(&opts, session).map(|s| s.phase),
            Some(Phase::Exiting { at_s }),
            "{session}: still in flight"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunched agent whose carry-on keeps waiting is said once past [`STALE_S`]
/// (`stuck:relaunched`).
#[cfg(unix)]
#[test]
fn a_relaunch_that_never_carries_on_is_noted_once_past_its_bound() {
    for (case, age, cause, notes) in [
        ("past", STALE_S + 60, "", 1),
        ("within", 60, "", 0),
        ("on-exit", STALE_S + 60, CAUSE_EXIT, 0),
    ] {
        let dir = scratch(&format!("stuck-rl-{case}"));
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        let at_s = now_s() - age;
        let mut st = St {
            phase: Phase::Relaunched { at_s },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            cause: cause.to_string(),
            ..St::default()
        };
        save(&opts, SESSION, &st);
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut steps = Vec::new();
        for _ in 0..2 {
            let r = carry_on_with_tab_probe(
                &opts,
                blank(&sf),
                &mut st,
                &mut c,
                SESSION,
                &sf,
                MODEL_WAIT,
                |_, _, _| false,
                None,
            );
            steps.push(r.step);
        }
        let _ = agent.kill();
        let _ = agent.wait();
        let said = ledger_details(&opts, "stuck:relaunched");
        let kept = load(&opts, SESSION).map(|s| s.phase);
        let typed = turns(&asked);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(steps, ["wait:tab-ownership-changed"; 2], "{case}");
        assert_eq!(said.len(), notes, "{case}: said once: {said:?}");
        assert_eq!(typed, 0, "{case}: nothing is typed");
        assert_eq!(kept, Some(Phase::Relaunched { at_s }), "{case}");
    }
}

/// A restart's agent that ended but is not reaped yet (a zombie) is seen gone ([`St::exited_at`]).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_restart_whose_agent_is_not_reaped_yet_says_when_it_ended() {
    let mut zombie = Command::new("/bin/sleep")
        .arg("60")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the agent");
    let pid = zombie.id();
    let gone = libc::pid_t::try_from(pid).expect("pid");
    // SAFETY: the test's own child; reaped below, after the look.
    unsafe { libc::kill(gone, libc::SIGKILL) };
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || super::super::upgrade_wake::wait_exit(pid, Duration::ZERO).then_some(())
        )
        .is_some(),
        "the agent ended"
    );
    assert!(
        alive(pid),
        "a zombie, not reaped: kill(pid, 0) still finds it"
    );
    let st = relaunched_over("unreaped-relaunch", pid);
    let _ = zombie.wait();
    assert_ne!(st.exited_at, 0, "its exit seen");
}

/// One [`relaunch`] of a restart whose signalled agent is `pid`, at a shell
/// that never takes its terminal back: `wait:shell-prompt`, still in flight,
/// nothing typed; the record after it.
#[cfg(unix)]
fn relaunched_over(name: &str, pid: u32) -> St {
    let dir = scratch(name);
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut shell = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("stand-in shell");
    let mut st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid,
        shell: shell.id(),
        tab: TAB.to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let r = relaunch(
        &opts,
        Report {
            pid: st.pid,
            tab: TAB.to_string(),
            session: SESSION.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283(managed)".to_string(),
            step: String::new(),
        },
        &mut st,
        &mut c,
        SESSION,
        &Live,
    );
    drop(c);
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(r.step, "wait:shell-prompt");
    assert!(matches!(st.phase, Phase::Exiting { .. }), "still in flight");
    assert_eq!(turns(&asked), 0, "nothing typed");
    let _ = std::fs::remove_dir_all(&dir);
    st
}

/// The busy footer is read below the composer, not in the transcript.
#[test]
fn a_footer_quoted_in_the_transcript_is_no_turn_running() {
    let rule = "─".repeat(40);
    let footer = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt";
    let rows = |lines: &[&str]| -> Vec<String> { lines.iter().map(|l| (*l).to_string()).collect() };
    let quoted = rows(&[
        "⏺ The footer read:",
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
        "",
        &rule,
        "❯ ",
        &rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
    ]);
    assert!(!busy_footer(&quoted), "a quote above the composer");
    let running = rows(&["⏺ Bash(sleep 600)", "", &rule, "❯ ", &rule, footer]);
    assert!(busy_footer(&running), "the footer under the composer");
    assert!(busy_footer(&parse_screen(SCREEN).expect("parses").rows));
    let drafted = rows(&[
        "",
        &rule,
        "❯ see the footer:",
        "  esc to interrupt",
        &rule,
        "  ⏵⏵ auto",
    ]);
    assert!(!busy_footer(&drafted));
    assert!(!busy_footer(&rows(&[
        "Do you want to proceed?",
        "  esc to interrupt"
    ])));
    let spinning = rows(&[
        "⏺ Bash(./run.sh --queue 8)",
        "  ⎿  queued 8 runs",
        "",
        "✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)",
        "",
        &rule,
        "❯ ",
        &rule,
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    ]);
    assert!(busy_footer(&spinning), "the spinner row above the composer");
    use aterm_phase::prompt::fixtures as f;
    assert!(!busy_footer(&f::screen(f::EXIT_RELAUNCH_STALE_FOOTER)));
    assert!(!busy_footer(&f::screen(
        f::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2
    )));
    assert!(busy_footer(&f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY)));
    let narrow = "─".repeat(30);
    let wrapped = rows(&[
        "⏺ Bash(./run.sh --queue 8)",
        "",
        "✶ Deliberating… (4s · esc to",
        "  interrupt)",
        "",
        &narrow,
        "❯ ",
        &narrow,
        "  ⏵⏵ auto mode on",
    ]);
    assert!(busy_footer(&wrapped), "the spinner's wrapped hint");
}

/// A host's `help turn` that names the rows fence (`if-rows=`).
const ROWS_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] \
                         [if-rows=<first>-<last>:<hex16>] ...";

/// A host's `help turn` that names the typing budget (`cadence=`, `cap=`, `yield=`).
const CAP_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                        [yield=<floor>] [if-gen=<epoch>.<seq>] ...";

/// The Claude notice fences on its input box where the host's `help turn` names `if-rows=`.
#[cfg(unix)]
#[test]
fn the_claude_notice_fences_on_its_input_box_where_the_host_takes_if_rows() {
    use aterm_types::control_verbs::RowsFence;
    let rule = "─".repeat(20);
    let screen = |framed: bool| {
        let edge = if framed {
            rule.clone()
        } else {
            "  ".to_string()
        };
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{edge}","❯ ","{edge}"],"cursor":{{"row":2,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let turn_line = |help: &'static str, framed: bool, name: &str| -> String {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen(framed),
                help,
                turn: vec!["OK 0 id=2 submitted=1 status=settled"],
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        let (fence, _) = claude_fence(&mut c, TAB).expect("empty composer");
        turn_fenced(&mut c, TAB, "[aterm harness] hi", fence.as_deref()).expect("typed");
        drop(c);
        let line = asked
            .lock()
            .expect("log")
            .iter()
            .find(|l| l.split_whitespace().find(|w| !w.starts_with('@')) == Some("turn"))
            .cloned()
            .expect("a turn was sent");
        let _ = std::fs::remove_dir_all(&dir);
        line
    };
    let want = RowsFence {
        first: 1,
        last: 3,
        hash: RowsFence::hash_rows([rule.as_str(), "❯ ", rule.as_str()]),
    };
    let narrow = turn_line(ROWS_HELP, true, "fence-rows");
    assert!(narrow.contains(&format!("if-rows={want} ")), "{narrow}");
    assert!(!narrow.contains("if-gen="), "{narrow}");
    let older = turn_line(FENCED_HELP, true, "fence-rows-older");
    assert!(
        older.contains("if-gen=4.77 ") && !older.contains("if-rows="),
        "{older}"
    );
    let unframed = turn_line(ROWS_HELP, false, "fence-rows-unframed");
    assert!(
        unframed.contains("if-gen=4.77 ") && !unframed.contains("if-rows="),
        "{unframed}"
    );
}

/// The fenced notice asks nothing between its read and its `turn` (the server types nothing when
/// output came after the generation it is handed).
#[cfg(unix)]
#[test]
fn the_fenced_notice_asks_nothing_between_its_read_and_its_turn() {
    let rule = "─".repeat(20);
    let at = |col: u32, text: &str| {
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{rule}","❯ {text}","{rule}"],"cursor":{{"row":2,"col":{col}}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let verbs = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.split_whitespace().find(|w| !w.starts_with('@')))
                    .filter(|v| *v != "AUTH")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let rig = |name: &str, answers: Answers| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let c = connect(&opts, TAB).expect("control connection");
        (dir, c, asked)
    };
    let answers = |after: Option<(usize, String)>| Answers {
        screen: at(2, ""),
        after,
        help: FENCED_HELP,
        turn: vec!["OK 0 id=2 submitted=1 status=settled"],
        ..Answers::default()
    };
    let (dir, mut c, asked) = rig("fence-order", answers(None));
    for _ in 0..2 {
        let generation = typing_fence(&mut c, TAB).expect("empty composer");
        assert_eq!(generation.as_deref(), Some("4.77"));
        turn_fenced(&mut c, TAB, "[aterm harness] hi", generation.as_deref()).expect("typed");
    }
    drop(c);
    let got = verbs(&asked);
    assert_eq!(
        got,
        [
            "help", "text", "cell", "text", "turn", "text", "cell", "text", "turn"
        ],
        "{got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, mut c, asked) = rig(
        "fence-no-cell",
        Answers {
            screen: at(4, "x"),
            ..answers(None)
        },
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("draft"), "a typed draft");
    drop(c);
    assert_eq!(verbs(&asked), ["help", "text"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The fence judges the composer where it is drawn.
#[cfg(unix)]
#[test]
fn the_fence_judges_the_composer_row_not_the_cursors_row() {
    let rule = "─".repeat(20);
    let displaced = |text: &str| {
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{rule}","❯ {text}","{rule}"],"cursor":{{"row":3,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let asked_cells = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" cell ")).cloned().collect())
            .unwrap_or_default()
    };
    let rig = |name: &str, answers: Answers| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let c = connect(&opts, TAB).expect("control connection");
        (dir, c, asked)
    };
    let dim = "OK p 999999 111318 dim";
    let base = |text: &str, cell: &'static str| Answers {
        screen: displaced(text),
        help: FENCED_HELP,
        cell,
        ..Answers::default()
    };
    let (dir, mut c, asked) = rig("displaced-dim", base("try the tests", dim));
    assert_eq!(typing_fence(&mut c, TAB), Ok(Some("4.77".to_string())));
    drop(c);
    assert_eq!(asked_cells(&asked), [format!("@{TAB} cell 2 2")]);
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, mut c, _asked) = rig(
        "displaced-typed",
        base("try the tests", "OK p d0d0d0 111318 none"),
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("draft"));
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, mut c, asked) = rig(
        "displaced-moved",
        Answers {
            after: Some((1, displaced("try the tests, and then the lint"))),
            ..base("try the tests", dim)
        },
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("changed"));
    drop(c);
    assert!(
        !asked
            .lock()
            .is_ok_and(|a| a.iter().any(|l| l.contains(" turn "))),
        "nothing typed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host's next move after one step.
#[test]
fn after_a_step_the_host_parks_waits_longer_stops_or_says_it_is_broken() {
    assert_eq!(after("announced:1", 0), After::NextIdle);
    assert_eq!(after("adopted", 2), After::NextIdle);
    assert_eq!(after("continued", 3), After::NextIdle);
    assert_eq!(after("wait:settling", 0), After::Later(LATER[0]));
    assert_eq!(after("wait:not-ready", 1), After::Later(LATER[1]));
    assert_eq!(after("held-back:terminal:tmux", 2), After::Later(LATER[2]));
    assert_eq!(after("busy:another-sweep", 9), After::Later(LATER[3]));
    assert_eq!(after("busy:state-unwritable", 0), After::Broken);
    assert_eq!(after("busy:lock-unopenable", 0), After::Broken);
    for held in ["wait:box", "wait:draft"] {
        assert_eq!(after(held, 9), After::Later(HOLD_LOOK), "{held}");
    }
    assert!(LATER[3].as_secs() > upgrade::HOLD_GAP_S);
    assert_eq!(after("gave-up", 0), After::NextIdle);
    assert_eq!(after("drain-expired:box", 0), After::NextIdle);
    assert_eq!(after("released:gave-up", 0), After::Later(LATER[0]));
    assert_eq!(after("wait:release:limited", 2), After::Later(LATER[2]));
    for resting in [
        "wait:failed:unanswered",
        "refused:not-a-shell-job",
        "failed:no-resume",
        "wait:failed",
        "rearmed:unanswered",
    ] {
        assert_eq!(after(resting, 0), After::Later(LATER[0]), "{resting}");
        assert_eq!(after(resting, 9), After::Later(LATER[3]), "{resting}");
    }
    for last in ["current", "done", "done:no-continue", "wait:skipped"] {
        assert_eq!(after(last, 0), After::Finished, "{last}");
        assert!(!unknown_step(last), "{last}");
    }
    for (word, waits) in [("brand-new", 0), ("brand-new:thing", 9), ("", 3)] {
        assert!(unknown_step(word), "{word}");
        assert_eq!(
            after(word, waits),
            After::Later(LATER[LATER.len() - 1]),
            "{word}"
        );
    }
    assert!(
        LATER[0] >= Duration::from_secs(upgrade::QUIET_S),
        "the first look waits out the settle a new idle point needs"
    );
    assert!(LATER.windows(2).all(|w| w[0] < w[1]), "each pause longer");
}

/// Which turn a step typed, for the turn-end policy ([`host_turn`]).
#[test]
fn a_step_names_the_turn_it_typed() {
    use crate::supervise::policy::turn_end::HostTurn;
    for carry_on in ["continued", "released:gave-up", "released:void"] {
        assert_eq!(host_turn(carry_on), Some(HostTurn::CarryOn), "{carry_on}");
    }
    assert!(
        upgrade::release_prompt(upgrade::Agent::Claude, false)
            .contains("Carry on as you would have")
    );
    for notice in ["announced:1", "announced:3"] {
        assert_eq!(host_turn(notice), Some(HostTurn::Notice), "{notice}");
    }
    assert_eq!(
        host_turn("continued:restore"),
        Some(HostTurn::Notice),
        "the restore line"
    );
    assert!(upgrade::restore_prompt(None, true).contains("Nothing else is asked"));
    for none in [
        "adopted",
        "done:fresh",
        "done",
        "wait:settling",
        "gave-up",
        "done:lost",
    ] {
        assert_eq!(host_turn(none), None, "{none}");
    }
}

/// The upgrade owns a session's turn ends by its step's word ([`owns_turn_ends`]).
#[test]
fn the_upgrade_owns_turn_ends_only_while_its_step_says_so() {
    const GRACE: u32 = 120;
    for settle in [
        "wait:release:settling",
        "wait:release:not-idle",
        "wait:release:busy",
        "wait:release:status-stale",
    ] {
        assert!(owns_turn_ends(settle, 0, GRACE), "{settle}");
        assert!(
            owns_turn_ends(settle, OWNED_SETTLE_LOOKS - 1, GRACE),
            "{settle}"
        );
        assert!(
            !owns_turn_ends(settle, OWNED_SETTLE_LOOKS, GRACE),
            "{settle}: bounded as the settle is"
        );
        assert!(bounded_wait(settle), "{settle}: counted toward that bound");
    }
    for held in [
        "wait:release:box",
        "wait:release:draft",
        "wait:release:attended",
        "wait:release:limited",
        "wait:release:ready",
        "wait:release:terminal",
    ] {
        assert!(!owns_turn_ends(held, 0, GRACE), "{held}");
        assert!(!bounded_wait(held), "{held}");
    }
    assert!(owns_turn_ends("announced", 0, GRACE));
    assert!(owns_turn_ends("announced:2", 0, GRACE));
    assert!(owns_turn_ends("wait:settling", 0, GRACE));
    assert!(!owns_turn_ends("wait:settling", OWNED_SETTLE_LOOKS, GRACE));
    assert!(owns_turn_ends("wait:status-stale", 0, GRACE));
    assert!(!owns_turn_ends(
        "wait:status-stale",
        OWNED_SETTLE_LOOKS,
        GRACE
    ));
    assert!(owns_turn_ends(
        "wait:background",
        OWNED_BACKGROUND_LOOKS - 1,
        GRACE
    ));
    assert!(!owns_turn_ends(
        "wait:background",
        OWNED_BACKGROUND_LOOKS,
        GRACE
    ));
    for last in [
        "wait:awaiting-ready",
        "wait:release:ready",
        "released:gave-up",
        "held-back:person",
        "done",
        "gave-up",
        "failed:x",
        "wait:failed",
        "adopted",
        "continued",
        "current",
    ] {
        assert!(!owns_turn_ends(last, 0, GRACE), "{last}");
    }
}

/// Under the quiet flow a wait on the agent's own work owns no turn end and bounds none
/// ([`owns_turn_ends_as`], [`bounded_wait_as`]).
#[test]
fn a_quiet_wait_on_the_agents_work_owns_no_turn_end() {
    const GRACE: u32 = 120;
    let drain = "wait:background";
    assert!(!owns_turn_ends_as(drain, 0, GRACE, true));
    assert!(!bounded_wait_as(drain, true));
    assert!(owns_turn_ends_as(drain, 0, GRACE, false));
    assert!(bounded_wait_as(drain, false));
    for owned in [
        "wait:settling",
        "wait:status-stale",
        "announced:1",
        "wait:attended",
    ] {
        for quiet in [true, false] {
            assert_eq!(
                owns_turn_ends_as(owned, 0, GRACE, quiet),
                owns_turn_ends(owned, 0, GRACE),
                "{owned} quiet={quiet}"
            );
            assert_eq!(bounded_wait_as(owned, quiet), bounded_wait(owned));
        }
    }
}

/// Muse Code's quiet restart owns the turn end its log lags at ([`log_settles`]).
#[test]
fn the_muse_restarts_log_lag_owns_its_turn_end_for_the_settles_looks() {
    const GRACE: u32 = 120;
    for lag in ["wait:turn-open", "wait:queued-work"] {
        assert!(log_settles(lag), "{lag}");
        for quiet in [true, false] {
            assert!(
                owns_turn_ends_as(lag, 0, GRACE, quiet),
                "{lag} quiet={quiet}"
            );
            assert!(
                owns_turn_ends_as(lag, OWNED_SETTLE_LOOKS - 1, GRACE, quiet),
                "{lag} quiet={quiet}"
            );
            assert!(
                !owns_turn_ends_as(lag, OWNED_SETTLE_LOOKS, GRACE, quiet),
                "{lag} quiet={quiet}: bounded as the settle is"
            );
            assert!(
                bounded_wait_as(lag, quiet),
                "{lag}: counted toward that bound"
            );
        }
        assert_eq!(after(lag, 0), After::Later(first_look()), "{lag}");
        assert!(!unknown_step(lag), "{lag}");
    }
    for other in [
        "wait:screen-not-idle",
        "wait:background-work",
        "wait:open-task",
        "wait:child-process",
        "wait:process-table",
        "wait:log-unreadable",
        "wait:log-unproved",
        "wait:draft",
        "wait:held",
        "wait:break",
        "wait:build-fails",
        "wait:changed-before-signal",
    ] {
        assert!(!log_settles(other), "{other}");
        assert!(!owns_turn_ends_as(other, 0, GRACE, true), "{other}");
        assert!(!bounded_wait_as(other, true), "{other}");
    }
}

/// A person at the tab owns the turn end for their grace, so the loop types nothing over a ready.
#[test]
fn a_person_at_the_tab_owns_the_turn_end_for_their_grace() {
    const GRACE: u32 = 120;
    let look = LATER[0].as_secs();
    assert!(owns_turn_ends("held-back:attended", 0, GRACE));
    for waits in [0, 1, 2, 7, 8, 30] {
        assert!(owns_turn_ends("wait:attended", waits, GRACE), "{waits}");
    }
    let last = u32::try_from((u64::from(GRACE) + ATTENDED_OWNED_S) / look).unwrap();
    assert!(owns_turn_ends("wait:attended", last, GRACE));
    assert!(!owns_turn_ends("wait:attended", last + 1, GRACE));
    assert!(!owns_turn_ends("held-back:attended", last + 1, GRACE));
    assert!(owns_turn_ends("wait:attended", last + 1, 600));
    for waits in [0, 1, 2, 3, 9, 40] {
        assert_eq!(
            after("wait:attended", waits),
            After::Later(LATER[0]),
            "{waits}"
        );
        assert_eq!(
            after("held-back:attended", waits),
            After::Later(LATER[0]),
            "{waits}"
        );
    }
    assert_eq!(after_attended(1, 0, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last, 40, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last + 1, 0, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last + 1, 1, GRACE), After::Later(LATER[1]));
    assert_eq!(after_attended(last + 1, 95, GRACE), After::Later(LATER[3]));
    assert!(attended("held-back:attended") && attended("wait:attended"));
    assert!(!attended("wait:awaiting-ready") && !attended("held-back:person"));
    for owns_nothing in [
        "wait:awaiting-ready",
        "held-back:person",
        "gave-up",
        "adopted",
    ] {
        assert!(!owns_turn_ends(owns_nothing, 0, GRACE), "{owns_nothing}");
    }
    assert!(released_at_break("wait:attended"), "a break gives it back");
    assert_eq!(
        after("wait:settling", 1),
        After::Later(LATER[1]),
        "the ladder stands"
    );
    assert_eq!(after("held-back:terminal:tmux", 2), After::Later(LATER[2]));
}

/// The release is typed only where it is the upgrade's next act ([`release_is_next`]).
#[test]
fn the_release_is_the_next_act_only_where_nothing_else_is() {
    for (step, word, next) in [
        (Step::GiveUp, "gave-up", true),
        (Step::Void("box"), "drain-expired:box", true),
        (Step::Wait("failed"), "wait:failed", true),
        (Step::Wait("awaiting-ready"), "wait:awaiting-ready", true),
        (Step::Wait("skipped"), "wait:skipped", true),
        (Step::Wait("deferred"), "wait:deferred", true),
        (Step::Terminate, "failed:signal-refused", true),
        (Step::Terminate, "refused:argv", true),
        (Step::Announce, "refused:line", true),
        (Step::Wait("settling"), "wait:settling", false),
        (Step::Wait("background"), "wait:background", false),
        (Step::Wait("draft"), "wait:draft", false),
        (Step::Wait("limited"), "wait:limited", false),
        (Step::Terminate, "wait:changed-before-signal", false),
        (Step::Announce, "wait:announce-refused:x", false),
        (Step::Announce, "announced:2", false),
    ] {
        assert_eq!(release_is_next(&step, word), next, "{step:?} {word}");
    }
    let owed = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        release: "void".to_string(),
        ..St::default()
    };
    let brk = Facts {
        status: "shell".to_string(),
        background: vec!["zsh".to_string()],
        background_point: true,
        ..Facts::default()
    };
    let waits = Step::Wait("background");
    let word = "wait:background";
    assert!(release_next(&waits, word, &owed, &brk, false));
    assert!(release_next(&Step::GiveUp, "gave-up", &owed, &brk, false));
    assert!(release_next(
        &Step::Wait("failed"),
        "wait:failed",
        &owed,
        &brk,
        false
    ));
    let nothing = St {
        release: String::new(),
        ..owed.clone()
    };
    assert!(!release_next(&waits, word, &nothing, &brk, false));
    assert!(!release_next(&waits, word, &owed, &brk, true), "READY");
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..owed.clone()
    };
    assert!(!release_next(&waits, word, &gave_up, &brk, true));
    let idle_point = Facts {
        background_point: false,
        ..brk.clone()
    };
    assert!(!release_next(&waits, word, &owed, &idle_point, false));
    let r = |step: &str| Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: step.to_string(),
    };
    for (word, holds, said) in [
        ("failed:signal-refused", "limited", "wait:release:limited"),
        ("refused:argv", "draft", "wait:release:draft"),
        ("wait:failed", "settling", "wait:release:settling"),
        ("wait:release", "box", "wait:release:box"),
        ("gave-up", "not-idle", "gave-up"),
        ("drain-expired:draft", "draft", "drain-expired:draft"),
    ] {
        let out = owed_word(r(word), holds);
        assert_eq!(out.step, said, "{word}");
        assert_ne!(after(&out.step, 0), After::Finished, "{word}");
    }
}

/// [`due`] parks a session only for an upgrade to take (this tab's restart in flight is one) and
/// says what it could not read rather than "not due".
#[test]
fn due_is_this_tabs_upgrade_or_restart_and_nothing_unread() {
    let dir = scratch("due");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "no socket, no roster: nothing read, nothing decided"
    );
    let other = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: dead_pid(),
        shell: std::process::id(),
        tab: "s-0ther".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), other.to_json()).expect("write");
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "another tab's restart is not this tab's"
    );
    let ours = St {
        tab: TAB.to_string(),
        ..other
    };
    std::fs::write(state_path(&opts, SESSION), ours.to_json()).expect("write");
    assert_eq!(due(&opts), Due::Yes, "this tab's restart in flight is due");
    let owed = St {
        phase: Phase::Done,
        confirm_by: now_s() + 60,
        ..ours.clone()
    };
    std::fs::write(state_path(&opts, SESSION), owed.to_json()).expect("write");
    assert_eq!(due(&opts), Due::Yes, "a model still to be read is due");
    let settled = St {
        confirm_by: 0,
        ..owed
    };
    std::fs::write(state_path(&opts, SESSION), settled.to_json()).expect("write");
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "a finished restart is not due: what decides is the roster"
    );
    std::fs::write(state_path(&opts, SESSION), ours.to_json()).expect("write");
    assert_eq!(
        step(&Opts {
            only_sid: Some("s-0ther".to_string()),
            ..opts.clone()
        })
        .step,
        "wait:tab-not-live",
        "a tab the instance does not list is not stepped"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// One Claude Code record of the tab's agent, `pid` 101, on `version`.
fn record(version: &str) -> SessionFile {
    SessionFile {
        pid: 101,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: version.to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    }
}

/// The instance's one tab, [`TAB`], its foreground group 101.
fn the_tab() -> [LiveTab; 1] {
    [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(101),
    }]
}

/// An announced round whose notice carried a model is looked at with the build current (the
/// pre-filter [`due`] asks).
#[test]
fn an_announced_round_carrying_a_model_is_looked_at_with_the_build_current() {
    let dir = scratch("due-announced-model");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    let tabs = the_tab();
    let current = || Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let look = |st: &St| {
        save(&opts, SESSION, st);
        due_among(
            &opts,
            &[record("1.0.0")],
            &tabs,
            current,
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(101),
            |_| Some((10, 101, 101)),
        )
    };
    let announced = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "1.0.0".to_string(),
        source: "managed".to_string(),
        model_list: "claude-opus-5-5".to_string(),
        ..St::default()
    };
    assert_eq!(look(&announced), Due::Yes, "the model's round is looked at");
    let no_model = St {
        model_list: String::new(),
        ..announced.clone()
    };
    assert_eq!(look(&no_model), Due::No, "no model carried");
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..announced.clone()
    };
    assert_eq!(look(&gave_up), Due::No, "a give-up, its release typed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`no_record`]: no record of the tab's agent is a wait ([`UNREAD_RECORD`]) only while the agent
/// is younger than [`RECORD_WINDOW_S`].
#[test]
fn a_launch_is_waited_on_for_its_record_only_within_its_window() {
    let tabs = the_tab();
    let now = 1_000_000;
    let at = |age: u64| move |pid: u32| (pid == 101).then_some(now - age);
    let lives = |_| true;
    assert_eq!(
        no_record(&tabs, TAB, at(RECORD_WINDOW_S - 1), lives, now),
        Due::Unread(UNREAD_RECORD),
        "the window's last second"
    );
    assert_eq!(
        no_record(&tabs, TAB, |_| None, lives, now),
        Due::Unread(UNREAD_PROCESS),
        "a live leader whose start could not be read"
    );
    assert_eq!(
        no_record(&tabs, TAB, |_| None, |_| false, now),
        Due::No,
        "a leader gone from its group: no launch to wait on"
    );
    let unknown = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: None,
    }];
    assert_eq!(
        no_record(&unknown, TAB, at(1), lives, now),
        Due::Unread(UNREAD_PROCESS),
        "a foreground group the kernel did not name"
    );
    assert_eq!(
        no_record(&[], TAB, at(1), lives, now),
        Due::No,
        "a tab gone"
    );
    let dir = scratch("records");
    assert_eq!(claude_records(&dir), Some(Vec::new()), "none kept here");
    std::fs::create_dir_all(dir.join(".claude/sessions")).expect("sessions");
    let me = std::process::id();
    std::fs::write(
        dir.join(format!(".claude/sessions/{me}.json")),
        "{\"pid\": 1",
    )
    .expect("half");
    assert_eq!(claude_records(&dir), None, "caught half-written: unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Conformance test of `HarnessUpgradeLook`, its classifier half (the host's bind in aterm-gui
/// fakes `due`).
#[test]
fn the_real_classifier_conforms_to_the_upgrade_look_model() {
    let model = aterm_models_harness::harness_upgrade_look_model();
    let dir = scratch("look-bind");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    seed_builds(&opts);
    let tabs = the_tab();
    let now = 1_000_000;
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Ids {
        Whole,
        Unread,
        /// The tab's foreground caught moving between the roster and the ids.
        Moved,
        /// The process caught leaving its group.
        Left,
    }
    let at = |due: bool, readable: bool| {
        let mut state = model.init_state();
        state.insert("due", i64::from(due));
        state.insert("readable", i64::from(readable));
        state.insert("looks", 1);
        state.insert("done", 0);
        state
    };
    let action = |due: Due| match due {
        Due::Yes => "Look",
        Due::No => "NotDue",
        Due::Unread(_) => "Unread",
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut looks = 0;
    for present in [false, true] {
        for behind in [false, true] {
            for group in [false, true] {
                for ids in [Ids::Whole, Ids::Unread, Ids::Moved, Ids::Left] {
                    for argv in [false, true] {
                        for env in [None, Some(TAB), Some("s-another")] {
                            for old in [false, true] {
                                let files = if present {
                                    vec![record(if behind { "unknown" } else { "999.0.0" })]
                                } else {
                                    Vec::new()
                                };
                                let args = argv.then(|| atpkg::caller_shell::ProcArgs {
                                    env: env
                                        .map(|t| vec![format!("ATERM_PARENT_SESSION_ID={t}")])
                                        .unwrap_or_default(),
                                    ..atpkg::caller_shell::ProcArgs::default()
                                });
                                let verdict = match due_among(
                                    &opts,
                                    &files,
                                    &tabs,
                                    || Targets::read(&opts.home),
                                    |_| args.clone(),
                                    |_| group.then_some(101),
                                    |_| match ids {
                                        Ids::Whole => Some((10, 101, 101)),
                                        Ids::Unread => None,
                                        Ids::Moved => Some((10, 101, 202)),
                                        Ids::Left => Some((10, 303, 101)),
                                    },
                                ) {
                                    Due::Unread(UNREAD_RECORD) => no_record(
                                        &tabs,
                                        TAB,
                                        |_| Some(now - if old { RECORD_WINDOW_S } else { 1 }),
                                        |_| true,
                                        now,
                                    ),
                                    due => due,
                                };
                                let leads = present && group;
                                let process = ids == Ids::Whole && argv;
                                let (due, readable) = if !leads {
                                    // No record of the tab's agent: final
                                    // past its launch, unknown before.
                                    (!old, old)
                                } else if !behind {
                                    (false, true)
                                } else if process && env == Some("s-another") {
                                    // Read whole: another tab's agent.
                                    (false, true)
                                } else {
                                    (true, process)
                                };
                                let name = action(verdict);
                                assert!(
                                    model.action_enabled(name, &at(due, readable)),
                                    "{name} at due={due} readable={readable}: present={present} \
                                     behind={behind} group={group} ids={ids:?} argv={argv} \
                                     env={env:?} old={old} -> {verdict:?}"
                                );
                                seen.insert(name);
                                looks += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(looks, 384);
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["Look", "NotDue", "Unread"],
        "every verdict reached"
    );
    let counted_early = |process: bool| if process { Due::Yes } else { Due::No };
    assert_eq!(
        due_among(
            &opts,
            &[record("unknown")],
            &tabs,
            || Targets::read(&opts.home),
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(101),
            |_| None,
        ),
        Due::Unread(UNREAD_PROCESS),
        "the real classifier"
    );
    assert!(!model.action_enabled(action(counted_early(false)), &at(true, false)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh launch is noted behind once Claude Code has written its record.
#[test]
fn a_launch_noted_once_its_record_is_there_is_behind_from_the_attach() {
    let dir = scratch("behind-since");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    assert_eq!(
        note_behind(&opts, now_s()),
        Behind::Unread(UNREAD_SOCKET),
        "no socket: nothing read, nothing noted"
    );
    let sf = register(&opts.home, std::process::id(), SESSION);
    let attached = now_s() - 90;
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, attached));
    let st = load(&opts, SESSION).expect("minted");
    assert_eq!(
        (st.phase.clone(), st.pending_since),
        (Phase::Pending, attached),
        "behind from the attach"
    );
    assert!(!mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s() + 600));
    let st = load(&opts, SESSION).expect("minted");
    assert!(st.pending_since <= now_s(), "{st:?}");
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    let current = Targets {
        managed: Some(Candidate {
            version: Version::parse("1.0.0").expect("version"),
            ..newer().managed.expect("target")
        }),
        native: None,
        unanswered: false,
    };
    assert!(!mint_behind(&opts, &sf, TAB, &current, false, now_s()));
    assert!(load(&opts, SESSION).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A state an earlier upgrade left (done, or failed on another build) is minted behind anew when a
/// newer build lands ([`behind_anew`]).
#[test]
fn a_session_upgraded_before_is_behind_anew_when_a_newer_build_lands() {
    let dir = scratch("behind-anew");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let earlier = |phase: Phase, to: &str| St {
        phase,
        from: "0.9.0".to_string(),
        to: to.to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 3_600,
        done_at: now_s() - 1_800,
        ..St::default()
    };
    for prior in [
        earlier(Phase::Done, "1.0.0"),
        earlier(Phase::Failed("no-resume".to_string()), "1.0.0"),
    ] {
        save(&opts, SESSION, &prior);
        assert!(
            mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
            "{prior:?}"
        );
        let st = load(&opts, SESSION).expect("minted");
        assert_eq!(
            (st.phase.clone(), st.to.as_str()),
            (Phase::Pending, "9.9.9")
        );
        assert!(now_s().saturating_sub(st.behind_since()) <= 1, "{st:?}");
    }
    for prior in [
        earlier(Phase::Exiting { at_s: now_s() }, "9.9.9"),
        St {
            confirm_by: now_s() + 60,
            ..earlier(Phase::Done, "1.0.0")
        },
        earlier(Phase::Failed("no-resume".to_string()), "9.9.9"),
        earlier(Phase::Pending, "9.9.9"),
    ] {
        save(&opts, SESSION, &prior);
        assert!(
            !mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
            "{prior:?}"
        );
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            (&st.phase, &st.to, st.pending_since, st.confirm_by),
            (
                &prior.phase,
                &prior.to,
                prior.pending_since,
                prior.confirm_by
            )
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conversation resumed in another tab is moved into this tab at the host's attach
/// ([`mint_behind`]) and onto the build the visit would choose (`retargeted:…`, said once).
#[test]
fn a_conversation_resumed_in_another_tab_is_retabbed_at_the_attach() {
    let dir = scratch("behind-retab");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let elsewhere = "s-0123456789abcdef0123";
    let waiting = |phase: Phase, to: &str| St {
        phase,
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        tab: elsewhere.to_string(),
        salt: 100,
        pending_since: 100,
        ..St::default()
    };
    save(&opts, SESSION, &waiting(Phase::Pending, "9.9.8"));
    assert!(
        mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
        "noted"
    );
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!(
        (st.tab.as_str(), st.to.as_str(), &st.phase),
        (TAB, "9.9.9", &Phase::Pending)
    );
    assert_eq!(
        (st.salt, st.behind_since()),
        (100, 100),
        "its round and age"
    );
    assert_eq!(noted_for(&opts, "retargeted:9.9.8->9.9.9", SESSION), 1);
    let here = Opts {
        only_sid: Some(TAB.to_string()),
        ..opts.clone()
    };
    assert!(
        rows(&here).iter().any(|r| r.session == SESSION),
        "this tab's rows name it"
    );
    assert!(!mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    assert_eq!(ledger_lines(&opts), 1);
    let resting = waiting(Phase::Failed("no-resume".to_string()), "9.9.9");
    save(&opts, SESSION, &resting);
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!((st.tab.as_str(), &st.phase), (TAB, &resting.phase));
    assert_eq!(ledger_lines(&opts), 1, "no build changed, nothing said");
    let announced = waiting(Phase::Announced { at_s: 100, asks: 1 }, "9.9.8");
    save(&opts, SESSION, &announced);
    assert!(!mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!((st.tab.as_str(), st.to.as_str()), (elsewhere, "9.9.8"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bash, a dialect the relaunch line is written for, alive until the test lets it go: it
/// blocks in its own `read` of a pipe the handle holds, so it never ends on a clock a slow
/// test outlives (a gone bash is a `<defunct>` zombie, a shell with no dialect).
fn stand_in_shell() -> aterm_uds::spawnfd::Child {
    Command::new("/bin/bash")
        .args(["-c", "read -r _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("bash")
}

/// The owner's `Skip` and `Not today` hold a crash's relaunch.
#[cfg(unix)]
#[test]
fn a_build_the_conversations_record_holds_it_off_is_no_relaunch_target() {
    let dir = std::fs::canonicalize(scratch("relaunch-hold")).expect("scratch");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let versions = native_root(&opts.home);
    std::fs::create_dir_all(&versions).expect("versions");
    for build in ["2.1.281", "99.9.9"] {
        std::fs::write(versions.join(build), "").expect("a build");
    }
    std::fs::create_dir_all(opts.home.join(".local/bin")).expect("bin");
    std::os::unix::fs::symlink(versions.join("99.9.9"), opts.home.join(".local/bin/claude"))
        .expect("the native link");
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("sessions");
    file_transcript_in(&opts.home.join(".claude"), "p");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let mut shell = stand_in_shell();
    let dead = dead_pid();
    let start = "Thu Sep 24 01:02:03 2026";
    std::fs::write(
        opts.home.join(format!(".claude/sessions/{dead}.json")),
        format!(
            r#"{{"pid":{dead},"sessionId":"{SESSION}","cwd":"/","version":"2.1.281","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("the crash's record");
    let program = versions.join("2.1.281");
    let snap = Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: dead,
        start: start.to_string(),
        shell: shell.id(),
        program: program.clone(),
        argv: vec![
            program.to_string_lossy().into_owned(),
            "--verbose".to_string(),
        ],
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("2.1.281".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let now = now_s();
    let proof = Opts {
        dry_run: false,
        ..opts.clone()
    };
    let probe_row = Report {
        pid: dead,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "2.1.281".to_string(),
        to: "99.9.9(native)".to_string(),
        step: String::new(),
    };
    let proved_build = Candidate {
        exe: versions.join("99.9.9"),
        version: Version::parse("99.9.9").expect("v"),
        source: Source::Native,
    };
    assert_eq!(
        build_start::starts_with(&proof, &probe_row, &proved_build, now, false, true, |_| {
            build_start::Probe::Starts
        }),
        Ok(())
    );
    let relaunched_to = |request: Request, request_tab: &str| {
        let st = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "99.9.9".to_string(),
            source: "native".to_string(),
            tab: TAB.to_string(),
            request,
            request_tab: request_tab.to_string(),
            request_at: now - 60,
            pending_since: now - 7_200,
            salt: now - 7_200,
            ..St::default()
        };
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("the record");
        let r = after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
        assert_eq!(r.step, "would-relaunch", "{r:?}");
        r.to
    };
    let held = "2.1.281(same)";
    let moved = "99.9.9(native)";
    assert_eq!(
        relaunched_to(Request::Skip("99.9.9".into()), TAB),
        held,
        "skip"
    );
    assert_eq!(
        relaunched_to(Request::DeferUntil(now + 3_600), TAB),
        held,
        "not today"
    );
    assert_eq!(relaunched_to(Request::None, ""), moved, "no word");
    assert_eq!(
        relaunched_to(Request::DeferUntil(now - 1), TAB),
        moved,
        "a deferral run out"
    );
    assert_eq!(
        relaunched_to(Request::Skip("99.9.9".into()), "s-0ther"),
        moved,
        "a word on another tab"
    );
    assert_eq!(relaunched_to(Request::Now, TAB), moved, "--now");
    assert_eq!(
        relaunched_to(Request::Skip("99.9.8".into()), TAB),
        moved,
        "a build newer than the skipped one"
    );
    use super::super::restart_stamp::Cause;
    let stopped = |why: &str, to: &str, ago: u64| St {
        phase: Phase::Failed(why.to_string()),
        from: "2.1.281".to_string(),
        to: to.to_string(),
        source: "native".to_string(),
        tab: TAB.to_string(),
        proc_start: "B".to_string(),
        pending_since: now - 7_200,
        salt: now - 7_200,
        failed_at: now - ago,
        streak_why: why.to_string(),
        stop_streak: 1,
        ..St::default()
    };
    let relaunched_over = |st: &St, how: &str| {
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("the record");
        let r = match how {
            "crash" => after_exit(&opts, &snap, &ExitRecord::Unread, true, None),
            "stall" => after_exit(&opts, &snap, &ExitRecord::Unread, true, Some(Cause::Stall)),
            "asked" => after_exit(&opts, &snap, &ExitRecord::Unread, true, Some(Cause::Asked)),
            _ => after_host_ended(&opts, &snap, true),
        };
        assert_eq!(r.step, "would-relaunch", "{how}: {r:?}");
        r.to
    };
    for how in ["crash", "stall", "asked", "host-ended"] {
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.9", 600), how),
            held,
            "{how}: inside the stop's rest"
        );
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.9", upgrade::RETRY_S + 60), how),
            moved,
            "{how}: the rest run out"
        );
        assert_eq!(
            relaunched_over(&stopped("resumed-elsewhere", "99.9.9", 600), how),
            moved,
            "{how}: another reason"
        );
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.8", 600), how),
            moved,
            "{how}: a stop of an older build"
        );
    }
    let stopped_on = |prior: &St, to: &str| {
        let mut st = St {
            from: "2.1.281".to_string(),
            to: to.to_string(),
            source: "native".to_string(),
            cause: CAUSE_EXIT.to_string(),
            ..St::default()
        };
        prior.retried_by(&mut st);
        st.stop("no-resume", now);
        st.stop_streak
    };
    let rested = stopped("no-resume", "99.9.9", upgrade::RETRY_S + 60);
    assert_eq!(stopped_on(&rested, "99.9.9"), 2, "a stop that repeats");
    assert_eq!(stopped_on(&rested, "99.9.10"), 1, "another build");
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    let writes = Opts {
        dry_run: false,
        ..opts.clone()
    };
    let elsewhere = St {
        agent: upgrade::Agent::Claude,
        source: "native".to_string(),
        relaunched_at: now - 40,
        ..stopped("no-resume", "99.9.9", 34)
    };
    save(&writes, "0badf00d-9999-2222-3333-444455556666", &elsewhere);
    let crash = || after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
    let r = crash();
    assert_eq!((r.step.as_str(), r.to.as_str()), ("would-relaunch", held));
    let native = Candidate {
        exe: versions.join("99.9.9"),
        version: Version::parse("99.9.9").expect("v"),
        source: Source::Native,
    };
    assert_eq!(
        build_start::starts_with(&writes, &r, &native, now, false, false, |_| {
            build_start::Probe::Starts
        }),
        Ok(())
    );
    let r = crash();
    assert_eq!((r.step.as_str(), r.to.as_str()), ("would-relaunch", moved));
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart or relaunch on its own program keeps the owner's hold and the move clock
/// ([`St::kept_by_restart`]).
#[test]
fn a_restart_on_its_own_program_keeps_the_owners_hold_and_the_round_after_it_too() {
    use super::super::relaunch::CAUSE_MEMORY;
    let now = 1_790_000_000_u64;
    let running = Version::parse("2.1.281").expect("running");
    let build = |v: &str| Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse(v).expect("target"),
        source: Source::Managed,
    };
    let upgrade_record = |request: Request, request_tab: &str| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        request,
        request_tab: request_tab.to_string(),
        request_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    let restarted = |prior: &St| {
        let mut st = St {
            phase: Phase::Done,
            from: "2.1.281".to_string(),
            to: "2.1.281".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_MEMORY.to_string(),
            salt: now,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        st
    };
    let skip = Request::Skip("2.1.300".to_string());
    let restart = restarted(&upgrade_record(skip.clone(), TAB));
    assert_eq!(restart.request_for(TAB), skip, "the restart's record");
    assert_eq!(restart.behind_since(), now - 7_200);
    let next = St::for_target(
        Some(restart.clone()),
        &running,
        &build("2.1.300"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.phase, Phase::Pending);
    assert_eq!(next.request_for(TAB), skip, "the round after it");
    assert!(next.request.holds(&next.phase, "2.1.300", now));
    assert_eq!(next.behind_since(), now - 7_200, "the move clock");
    let newer = St::for_target(
        Some(restart),
        &running,
        &build("2.1.301"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(newer.request, Request::None);
    assert_eq!(newer.behind_since(), now - 7_200, "still behind since then");
    let standing = Request::DeferUntil(now + 3_600);
    let kept = restarted(&upgrade_record(standing.clone(), TAB));
    let next = St::for_target(
        Some(kept),
        &running,
        &build("2.1.300"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.request_for(TAB), standing);
    for (word, tab) in [
        (Request::DeferUntil(now - 1), TAB),
        (Request::Now, TAB),
        (skip.clone(), "s-0ther"),
    ] {
        let r = restarted(&upgrade_record(word.clone(), tab));
        let next = St::for_target(
            Some(r),
            &running,
            &build("2.1.300"),
            None,
            Proc::default(),
            now,
        );
        assert!(
            !next.request.holds(&next.phase, "2.1.300", now),
            "{word:?} {tab}: {next:?}"
        );
    }
}

/// A restart on the session's own program that stops keeps the owner's `Skip` of the build it held
/// off ([`St::kept_by_restart`]).
#[test]
fn a_restart_that_stops_keeps_the_owners_skip_of_the_build_it_held_off() {
    use super::super::relaunch::CAUSE_EXIT;
    let now = 1_790_000_000_u64;
    let running = Version::parse("2.1.281").expect("running");
    let build = |v: &str| Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse(v).expect("target"),
        source: Source::Managed,
    };
    let skip = Request::Skip("2.1.300".to_string());
    let upgrade_round = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        request: skip.clone(),
        request_tab: TAB.to_string(),
        request_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    for why in ["no-resume", "resumed-elsewhere", "relaunch-refused"] {
        let mut restart = St {
            phase: Phase::Exiting { at_s: now - 30 },
            from: "2.1.281".to_string(),
            to: "2.1.281".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_EXIT.to_string(),
            salt: now - 30,
            ..St::default()
        };
        upgrade_round.kept_by_restart(TAB, &mut restart);
        restart.stop(why, now - 10);
        assert!(matches!(restart.phase, Phase::Failed(_)), "{why}");
        let (next, how) = St::retarget(
            Some(restart.clone()),
            &running,
            &build("2.1.300"),
            None,
            Proc::default(),
            now,
        );
        assert_eq!(how, Retarget::Anew, "{why}: the stopped arm");
        assert_eq!(next.phase, Phase::Pending, "{why}");
        assert_eq!(next.request_for(TAB), skip, "{why}: the owner's skip");
        assert!(next.request.holds(&next.phase, "2.1.300", now), "{why}");
        assert_eq!(next.behind_since(), now - 7_200, "{why}: the move clock");
        let newer = St::for_target(
            Some(restart),
            &running,
            &build("2.1.301"),
            None,
            Proc::default(),
            now,
        );
        assert_eq!(newer.request, Request::None, "{why}");
    }
    let mut stopped = upgrade_round.clone();
    stopped.stop("relaunch-refused", now - 10);
    let next = St::for_target(
        Some(stopped),
        &running,
        &build("2.1.301"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.request, Request::None);
    assert!(!next.request.holds(&next.phase, "2.1.301", now));
}

/// [`St::kept_by_restart`] keeps the move clock only of a record about the build the restart keeps
/// the session on.
#[test]
fn a_restart_keeps_the_move_clock_only_of_a_record_about_the_build_it_keeps() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_MEMORY};
    let now = 1_790_000_000_u64;
    let t0 = now - 5 * 86_400;
    let running = Version::parse("2.1.290").expect("running");
    let newer = Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse("2.1.300").expect("target"),
        source: Source::Managed,
    };
    let restarted = |prior: &St| {
        let mut st = St {
            phase: Phase::Done,
            from: "2.1.290".to_string(),
            to: "2.1.290".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_MEMORY.to_string(),
            salt: now,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        st
    };
    let resumed_by_hand = St {
        phase: Phase::Failed("resumed-elsewhere".to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.290".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: t0,
        failed_at: now - 3_600,
        ..St::default()
    };
    let restart = restarted(&resumed_by_hand);
    assert_eq!(restart.behind_since(), now, "behind anew from the restart");
    let next = St::for_target(Some(restart), &running, &newer, None, Proc::default(), now);
    assert_eq!(next.behind_since(), now, "the next build's round");
    assert_eq!(next.rung_at(now), upgrade::Rung::Prefer, "its first rung");
    let direct = St::for_target(
        Some(resumed_by_hand.clone()),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(direct.behind_since(), now);
    let older_restart = St {
        phase: Phase::Done,
        from: "2.1.281".to_string(),
        to: "2.1.281".to_string(),
        source: SAME_PROGRAM.to_string(),
        cause: CAUSE_MEMORY.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    assert_eq!(restarted(&older_restart).behind_since(), now);
    let round_on_it = St {
        phase: Phase::Pending,
        from: "2.1.290".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    assert_eq!(restarted(&round_on_it).behind_since(), t0);
    let mut stopped_relaunch = St {
        phase: Phase::Exiting { at_s: now - 60 },
        from: "2.1.290".to_string(),
        to: "2.1.290".to_string(),
        source: SAME_PROGRAM.to_string(),
        cause: CAUSE_EXIT.to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: now - 60,
        ..St::default()
    };
    stopped_relaunch.stop("exited-before-continuing", now - 30);
    assert_eq!(restarted(&stopped_relaunch).behind_since(), t0);
    let direct = St::for_target(
        Some(stopped_relaunch.clone()),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(direct.behind_since(), t0, "the retarget keeps it too");
    let other = St {
        from: "2.1.281".to_string(),
        to: "2.1.281".to_string(),
        ..stopped_relaunch
    };
    assert_eq!(restarted(&other).behind_since(), now);
    let skip = Request::Skip("2.1.300".to_string());
    let skipped = St {
        to: "2.1.300".to_string(),
        request: skip.clone(),
        request_tab: TAB.to_string(),
        request_at: now - 60,
        ..resumed_by_hand
    };
    let restart = restarted(&skipped);
    assert_eq!(restart.request_for(TAB), skip);
    assert_eq!(restart.behind_since(), now);
    let next = St::for_target(Some(restart), &running, &newer, None, Proc::default(), now);
    assert!(next.request.holds(&next.phase, "2.1.300", now), "{next:?}");
    let direct = St::for_target(Some(skipped), &running, &newer, None, Proc::default(), now);
    assert!(direct.request.holds_build("2.1.300", now), "{direct:?}");
}

/// Retargeting a stopped round preserves `St::noticed_at` and `St::last_stop` for the Codex lane.
#[test]
fn a_stopped_rounds_retarget_keeps_what_says_its_tui_was_told_to_wind_down() {
    let now = 1_790_000_000_u64;
    let running = Version::parse("0.157.0").expect("running");
    let newer = Candidate {
        exe: PathBuf::from("/p/agents/codex"),
        version: Version::parse("0.157.2").expect("target"),
        source: Source::Managed,
    };
    let round = |agent: upgrade::Agent, phase: Phase, noticed_at: u64| St {
        agent,
        phase,
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        noticed_at,
        failed_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    let gave_up = Phase::Failed(upgrade::GAVE_UP.to_string());
    let codex = upgrade::Agent::Codex;
    let refused = Phase::Failed("signal-refused".to_string());
    let next = St::for_target(
        Some(round(codex, refused, now - 3_600)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(
        (next.noticed_at, next.last_stop.as_str()),
        (now - 3_600, "")
    );
    let announced = St {
        model_list: "gpt-5.5".to_string(),
        ..round(
            codex,
            Phase::Announced {
                at_s: now - 3_600,
                asks: 1,
            },
            now - 3_600,
        )
    };
    let (next, how) = St::retarget(
        Some(announced),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(how, Retarget::Anew);
    assert_eq!(next.noticed_at, now - 3_600);
    let rearmed = St {
        model_list: "gpt-5.5".to_string(),
        last_stop: upgrade::GAVE_UP.to_string(),
        ..round(codex, Phase::Pending, 0)
    };
    let next = St::for_target(Some(rearmed), &running, &newer, None, Proc::default(), now);
    assert_eq!(next.last_stop, upgrade::GAVE_UP);
    let next = St::for_target(
        Some(round(upgrade::Agent::Claude, gave_up, now - 3_600)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.last_stop, "");
}

/// The relaunch on exit relaunches only a crashed agent (its own record left behind) on the
/// conversation that record names.
#[test]
fn a_relaunch_on_exit_needs_an_exit_and_a_conversation_on_record() {
    let dir = scratch("after-exit");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("home");
    let project = opts.home.join(".claude/projects/-w");
    let later = "0badf00d-9999-2222-3333-444455556666";
    let transcripts = || {
        std::fs::create_dir_all(&project).expect("projects");
        for s in [SESSION, later] {
            std::fs::write(project.join(format!("{s}.jsonl")), "{}\n").expect("transcript");
        }
    };
    transcripts();
    let mut shell = stand_in_shell();
    let shell_pid = shell.id();
    let snap = |pid: u32, start: &str, session: Option<&str>| Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid,
        start: start.to_string(),
        shell: shell_pid,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec![
            "/opt/claude/bin/claude".to_string(),
            "--verbose".to_string(),
        ],
        session: session.map(str::to_string),
        cwd: "/".to_string(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let at_attempt = |snap: &Snapshot, stalled: bool| {
        after_exit(
            &opts,
            snap,
            &ExitRecord::Unread,
            true,
            stalled.then_some(super::super::restart_stamp::Cause::Stall),
        )
    };
    let mut child = parked().spawn().expect("agent");
    wait_exec(child.id());
    let start = kernel_start(child.id()).expect("lstart");
    let r = at_attempt(&snap(child.id(), &start, Some(SESSION)), false);
    assert_eq!(r.step, "wait:not-exited");
    child.kill().expect("kill");
    child.wait().expect("reap");
    let dead = dead_pid();
    let start = "Thu Sep 24 01:02:03 2026";
    let r = at_attempt(&snap(dead, start, None), false);
    assert_eq!(r.step, "ended:no-conversation");
    let mut once = snap(dead, start, Some(SESSION));
    once.argv
        .extend(["-p".to_string(), "fix the bug".to_string()]);
    assert_eq!(at_attempt(&once, false).step, "ended:one-shot");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "ended:graceful-exit");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), true);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", SESSION)
    );
    let crash_record = |pid: u32, session: &str| {
        std::fs::write(
            opts.home.join(format!(".claude/sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"x","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the crash's record");
    };
    crash_record(dead, SESSION);
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", SESSION)
    );
    let projects = opts.home.join(".claude/projects");
    std::fs::remove_file(project.join(format!("{SESSION}.jsonl"))).expect("rm");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch:fresh");
    std::fs::remove_dir_all(&projects).expect("rm");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch:fresh", "no projects yet");
    std::fs::write(&projects, "").expect("not a directory");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch", "unreadable projects");
    std::fs::remove_file(&projects).expect("rm");
    transcripts();
    crash_record(dead, later);
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", later)
    );
    let r = at_attempt(
        &snap(dead, "Mon Jan  1 00:00:00 2024", Some(SESSION)),
        false,
    );
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("ended:graceful-exit", SESSION)
    );
    let st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: dead,
        shell: shell_pid,
        tab: TAB.to_string(),
        cause: super::super::relaunch::CAUSE_EXIT.to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, later), st.to_json()).expect("in flight");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-resume:exiting");
    let other = dead_pid();
    crash_record(other, later);
    let r = at_attempt(&snap(other, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch", "{r:?}");
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{other}.json"))).expect("rm");
    let stale = St {
        phase: Phase::Exiting {
            at_s: now_s() - STALE_S - 1,
        },
        ..st.clone()
    };
    std::fs::write(state_path(&opts, later), stale.to_json()).expect("stale");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch");
    std::fs::write(state_path(&opts, later), st.to_json()).expect("in flight");
    let elsewhere = Snapshot {
        tab: "s-0ther".to_string(),
        ..snap(dead, start, Some(SESSION))
    };
    assert_eq!(
        at_attempt(&elsewhere, false).step,
        "wait:conversation-in-other-tab"
    );
    std::fs::remove_file(state_path(&opts, later)).expect("rm");
    shell.kill().expect("kill");
    shell.wait().expect("reap");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "refused:shell-gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dead agent of the relaunch tests' tab, its stand-in shell alive, and the path of Claude's own
/// record of it.
struct DeadAgent {
    opts: Opts,
    dir: ScratchDir,
    shell: aterm_uds::spawnfd::Child,
    snap: Snapshot,
    record: PathBuf,
}

impl DeadAgent {
    fn new(tag: &str) -> Self {
        let dir = scratch(tag);
        let opts = Opts {
            dry_run: true,
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let sessions = opts.home.join(".claude/sessions");
        std::fs::create_dir_all(&sessions).expect("home");
        file_transcript_in(&opts.home.join(".claude"), "p");
        let shell = stand_in_shell();
        let dead = dead_pid();
        let snap = Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid: dead,
            start: "Thu Sep 24 01:02:03 2026".to_string(),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: None,
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: Some(Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
        };
        let record = sessions.join(format!("{dead}.json"));
        DeadAgent {
            opts,
            dir,
            shell,
            snap,
            record,
        }
    }

    /// Claude's own record of the agent, as it wrote it while it ran.
    fn write_record(&self) {
        std::fs::write(&self.record, self.record_text(SESSION)).expect("the record");
    }

    /// [`DeadAgent::write_record`] holding conversation `session`, filed in
    /// the Claude Code directory `claude_dir` itself — where an agent started
    /// with `CLAUDE_CONFIG_DIR=<claude_dir>` files it.
    fn write_record_in(&self, claude_dir: &Path, session: &str) {
        let sessions = claude_dir.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        std::fs::write(
            sessions.join(format!("{}.json", self.snap.pid)),
            self.record_text(session),
        )
        .expect("the record");
    }

    /// The record's text, naming conversation `session`.
    fn record_text(&self, session: &str) -> String {
        format!(
            r#"{{"pid":{},"sessionId":"{session}","cwd":"/","version":"2.1.283","status":"busy","statusUpdatedAt":1,"procStart":"{}","kind":"interactive","entrypoint":"cli"}}"#,
            self.snap.pid, self.snap.start
        )
    }

    /// The record removed: by the exit itself, or by another Claude Code.
    fn remove_record(&self) {
        std::fs::remove_file(&self.record).expect("the record was there");
    }

    /// The host's look at the exit, live, its waits simulated (`wait` sees
    /// each look's number before the next).
    fn look(&self, mut wait: impl FnMut(usize)) -> ExitRecord {
        let looks = std::cell::Cell::new(0);
        exit_record(
            || {
                looks.set(looks.get() + 1);
                Some(look_at_exit(&self.opts.home, &self.snap))
            },
            |step| {
                assert_eq!(step, EXIT_LOOK);
                wait(looks.get());
                true
            },
        )
    }

    /// One attempt at the relaunch, handed what the exit left: its word.
    fn attempt(&self, left: &ExitRecord) -> String {
        after_exit(&self.opts, &self.snap, left, true, None).step
    }
}

impl Drop for DeadAgent {
    fn drop(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A crash read at its exit stands whatever removes its record later.
#[test]
fn a_crash_read_at_its_exit_is_relaunched_whatever_removes_its_record_later() {
    let a = DeadAgent::new("exit-record");
    assert_eq!(a.attempt(&ExitRecord::Unread), "ended:graceful-exit");
    let writes = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&writes)).expect("state");
    save(
        &writes,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() - 10 },
            pid: a.snap.pid,
            shell: a.snap.shell,
            tab: TAB.to_string(),
            cause: super::super::relaunch::CAUSE_EXIT.to_string(),
            relaunched_pid: dead_pid(),
            ..St::default()
        },
    );
    assert_eq!(a.attempt(&ExitRecord::Unread), "would-relaunch");
}

/// An exit the harness's own restart made (a graceful end, its record removed) is never read as
/// someone's.
#[test]
fn an_exit_the_harness_own_restart_made_is_never_read_as_graceful() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_HOST, CAUSE_MEMORY, CAUSE_UPGRADE_FRESH};
    let a = DeadAgent::new("own-exit");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let old = now_s() - STALE_S - 1;
    let record = |phase: Phase, cause: &str, pid: u32| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        cause: cause.to_string(),
        ..St::default()
    };
    let attempt = || after_exit(&opts, &a.snap, &ExitRecord::Removed, true, None).step;
    for (cause, said) in [
        ("", "told:stale-exit"),
        (CAUSE_UPGRADE_FRESH, "told:stale-exit"),
        (CAUSE_MEMORY, "refused:stale-exit"),
        ("model:claude-fable-5-1", "refused:stale-exit"),
        ("model-back:", "refused:stale-exit"),
    ] {
        save(
            &opts,
            SESSION,
            &record(Phase::Exiting { at_s: old }, cause, a.snap.pid),
        );
        assert_eq!(attempt(), said, "{cause:?}");
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            st.phase,
            Phase::Failed("stale-exit".to_string()),
            "{cause:?}"
        );
        assert_ne!(st.exited_at, 0, "{cause:?}: its agent was seen gone");
        assert_eq!(attempt(), said, "{cause:?}");
    }
    save(
        &opts,
        SESSION,
        &record(Phase::Relaunched { at_s: old }, "", a.snap.pid),
    );
    assert_eq!(attempt(), "told:no-resume");
    save(
        &opts,
        SESSION,
        &St {
            marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
            markers: vec!["ATERM-UPGRADE-READY-0badf00d".to_string()],
            ..record(Phase::Exiting { at_s: old }, CAUSE_EXIT, a.snap.pid)
        },
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
    assert_ne!(st.exited_at, 0, "its agent was seen gone");
    assert!(st.marker.is_empty() && st.markers.is_empty(), "{st:?}");
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, "", dead_pid()),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    assert_eq!(
        load(&opts, SESSION).expect("kept").phase,
        Phase::Failed("resumed-elsewhere".to_string())
    );
    for seen in [now_s() - 30, 0] {
        save(
            &opts,
            SESSION,
            &St {
                exited_at: seen,
                ..record(Phase::Relaunched { at_s: now_s() - 10 }, "", dead_pid())
            },
        );
        assert_eq!(attempt(), "ended:graceful-exit", "{seen}");
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            st.phase,
            Phase::Failed("exited-before-continuing".to_string()),
            "{seen}"
        );
        assert_eq!(st.exited_at, 0, "{seen}: {st:?}");
        let rows = super::upgrade_status::rows(&opts);
        assert!(
            rows.iter().all(|r| !r.failed_after_exit()),
            "{seen}: {rows:?}"
        );
        assert!(
            super::upgrade_status::status_rows(&opts, true).0.is_empty(),
            "{seen}: no stall shown"
        );
    }
    for (adopted, carried) in [(dead_pid(), false), (std::process::id(), true)] {
        save(
            &opts,
            SESSION,
            &St {
                relaunched_pid: adopted,
                ..record(
                    Phase::Relaunched { at_s: now_s() - 10 },
                    CAUSE_EXIT,
                    a.snap.pid,
                )
            },
        );
        let step = attempt();
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            (step.as_str(), st.in_flight()),
            if carried {
                ("wait:no-socket", true)
            } else {
                ("ended:graceful-exit", false)
            },
            "{adopted}: {st:?}"
        );
    }
    save(
        &opts,
        SESSION,
        &record(Phase::Failed("signal-refused".to_string()), "", a.snap.pid),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, CAUSE_HOST, a.snap.pid),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, "", a.snap.pid),
    );
    assert_eq!(
        after_host_ended(&a.opts, &a.snap, true).step,
        "would-relaunch"
    );
}

/// A restart carried under a hand waits it out, uncounted, and its clock stops.
#[cfg(unix)]
#[test]
fn a_restart_carried_under_a_hand_waits_it_out_uncounted_and_its_clock_stops() {
    use super::super::relaunch::{Outcome, WAIT_HAND_LIFTS, carry_restart, outcome};
    let a = DeadAgent::new("carry-hand");
    let (sock, asked) = instance(&a.dir);
    let opts = Opts {
        dry_run: false,
        sock: Some(sock),
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let status = a.dir.join("t.status");
    let exiting = |ago: u64, hand_seen_s: u64| {
        let at = now_s() - ago;
        St {
            phase: Phase::Exiting { at_s: at },
            pid,
            shell: a.snap.shell,
            tab: TAB.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            exited_at: at,
            hand_seen_s,
            ..St::default()
        }
    };
    let attempts = || {
        asked.lock().map_or(0, |a| {
            a.iter()
                .filter(|l| {
                    [" lease ", " turn ", " send "]
                        .iter()
                        .any(|w| l.contains(w))
                })
                .count()
        })
    };
    for hand in [
        "hold=1 hand=-",
        "hold=0 hand=lease:orchestrator",
        "hold=0 hand=turn:41:s-9f00",
        "hold=0 hand=turn:41",
    ] {
        std::fs::write(&status, format!("OK schema=1 {hand} human_ms=-")).expect("status");
        save(&opts, SESSION, &exiting(240, 0));
        let (before, now) = (attempts(), now_s());
        let r = carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false);
        assert_eq!(r.step, WAIT_HAND_LIFTS, "{hand}");
        assert_eq!(outcome(&r.step), Outcome::Busy, "{hand}: no miss");
        assert_eq!(attempts(), before, "{hand}: nothing attempted");
        let st = load(&opts, SESSION).expect("kept");
        assert!(matches!(st.phase, Phase::Exiting { .. }), "{hand}: {st:?}");
        assert!(st.hand_seen_s >= now, "{hand}: its look stamped: {st:?}");
        assert_eq!(st.wait, "held", "{hand}");
    }
    std::fs::write(&status, "OK schema=1 hold=0 hand=- human_ms=-").expect("status");
    save(&opts, SESSION, &exiting(600, now_s() - 5));
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false);
    assert!(
        r.step.starts_with("wait:") && r.step != WAIT_HAND_LIFTS,
        "{r:?}"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert!(matches!(st.phase, Phase::Exiting { .. }), "{st:?}");
    let ours = format!("hold=0 hand=lease:{} human_ms=-", hand_name());
    for nobody in [
        "hold=0 hand=- human_ms=800",
        "hold=0 hand=- human_ms=- input=3",
        ours.as_str(),
    ] {
        std::fs::write(&status, format!("OK schema=1 {nobody}")).expect("status");
        save(&opts, SESSION, &exiting(240, 0));
        let r = carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false);
        assert!(
            r.step.starts_with("wait:") && r.step != WAIT_HAND_LIFTS,
            "{nobody}: {r:?}"
        );
    }
    std::fs::write(&status, "OK schema=1 hold=0 hand=- human_ms=-").expect("status");
    for hand_seen_s in [0, now_s() - STALE_S - 1] {
        save(&opts, SESSION, &exiting(600, hand_seen_s));
        assert_eq!(
            carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
            "told:stale-exit",
            "{hand_seen_s}"
        );
    }
}

/// A stopped restart a hand held is still its exit's, counted from the hand.
#[test]
fn a_stopped_restart_a_hand_held_is_its_exits_counted_from_the_hand() {
    use super::super::relaunch::{carry_restart, restarted};
    let a = DeadAgent::new("stopped-own-hand");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let stopped = |hand_seen_s: u64| St {
        phase: Phase::Failed("stale-exit".to_string()),
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at: now - 2 * STALE_S - 60,
        hand_seen_s,
        ..St::default()
    };
    save(&opts, SESSION, &stopped(now - 60));
    assert!(
        restarted(&opts, upgrade::Agent::Claude, Some(pid)),
        "held a minute ago"
    );
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "told:stale-exit"
    );
    save(&opts, SESSION, &stopped(0));
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(pid)),
        "no hand: long past"
    );
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "refused:no-restart-in-flight"
    );
}

/// A restart whose agent was seen gone before a hand held its tab, carried through that hand,
/// counts its stale bound from the lift.
#[test]
fn a_restart_carried_through_a_hand_counts_its_stale_bound_from_the_lift() {
    use super::super::relaunch::carry_restart;
    let a = DeadAgent::new("carry-through-hand");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let signal = now - STALE_S - 60;
    let stamped = St {
        phase: Phase::Exiting { at_s: signal },
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at: now - STALE_S - 1,
        ..St::default()
    };
    save(&opts, SESSION, &stamped);
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), true).step,
        "wait:no-socket",
        "carried from the lift"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(
        st.exited_at >= now,
        "its bound counts from the lift: {st:?}"
    );
    save(&opts, SESSION, &stamped);
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "told:stale-exit"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
}

/// [`restarted`] locates the restart that ended the agent, and [`carry_restart`] carries it within
/// [`STALE_S`].
#[test]
fn the_host_finds_and_carries_the_restart_that_ended_the_agent_that_left() {
    use super::super::relaunch::{Outcome, carry_restart, outcome, restarted};
    let a = DeadAgent::new("carry-restart");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let claude = |phase: Phase, exited_at: u64| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at,
        ..St::default()
    };
    save(&opts, SESSION, &claude(Phase::Exiting { at_s: now }, 0));
    assert!(restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(dead_pid())),
        "another process"
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, None),
        "no snapshot proves nothing"
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, None),
        "no Codex record"
    );
    let other_tab = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert!(
        !restarted(&other_tab, upgrade::Agent::Claude, Some(pid)),
        "another tab"
    );
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "wait:no-socket"
    );
    let signal = now - STALE_S - 60;
    save(&opts, SESSION, &claude(Phase::Exiting { at_s: signal }, 0));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "wait:no-socket",
        "carried from its exit"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(st.exited_at >= now, "seen gone at the carry: {st:?}");
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "wait:no-socket",
        "and again at the next try"
    );
    save(
        &opts,
        SESSION,
        &claude(Phase::Exiting { at_s: signal }, now - STALE_S - 1),
    );
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("told:stale-exit", SESSION)
    );
    assert_eq!(outcome(&r.step), Outcome::Cannot("stale-exit".to_string()));
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
    assert_ne!(st.exited_at, 0);
    assert!(restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "told:stale-exit"
    );
    save(
        &opts,
        SESSION,
        &claude(
            Phase::Failed("stale-exit".to_string()),
            now - 2 * STALE_S - 1,
        ),
    );
    assert!(!restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid), false).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(state_path(&opts, SESSION)).expect("rm");
    let codex = format!("codex-{TAB}");
    let tui = dead_pid();
    let relaunched = |resumed_pid: u32, relaunched_pid: u32| St {
        agent: upgrade::Agent::Codex,
        phase: Phase::Relaunched { at_s: now },
        pid: tui,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        resumed_pid,
        relaunched_pid,
        ..St::default()
    };
    for st in [
        St {
            phase: Phase::Exiting { at_s: now },
            exited_at: now,
            ..relaunched(0, 0)
        },
        relaunched(0, 0),
        relaunched(0, std::process::id()),
        relaunched(0, dead_pid()),
    ] {
        save(&opts, &codex, &st);
        let text = std::fs::read_to_string(state_path(&opts, &codex)).expect("saved");
        assert!(
            !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
            "{st:?}"
        );
        assert_eq!(
            carry_restart(&opts, upgrade::Agent::Codex, Some(tui), false).step,
            "refused:no-restart-in-flight",
            "{st:?}"
        );
        assert_eq!(
            std::fs::read_to_string(state_path(&opts, &codex)).ok(),
            Some(text),
            "nothing written"
        );
    }
    let mut again = St {
        phase: Phase::Failed("no-resume".to_string()),
        line_block: Some(7),
        ..relaunched(4_000_002, 4_000_002)
    };
    let _ = again.rearm(now);
    assert_eq!(
        (
            again.resumed_pid,
            again.relaunched_pid,
            again.exited_at,
            again.line_block
        ),
        (0, 0, 0, None)
    );
}

/// The carry of an exit whose move another actor (a hand-run `aterm harness upgrade`) finished
/// meanwhile answers `landed` (the host's `Relaunched`).
#[cfg(unix)]
#[test]
fn the_carry_of_an_exit_whose_move_landed_meanwhile_says_nothing() {
    use super::super::relaunch::{Outcome, carry_restart, outcome};
    let dir = scratch("carry-landed");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, me) = (dead_pid(), std::process::id());
    let now = now_s();
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Done,
            pid: old,
            shell: me,
            tab: TAB.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            exited_at: now - 3,
            done_at: now,
            ..St::default()
        },
    );
    let saved = load(&opts, SESSION).expect("the record");
    register(&opts.home, me, SESSION);
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old), false);
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    assert_eq!(load(&opts, SESSION), Some(saved), "nothing written");
    assert_eq!(ledger_lines(&opts), 0);
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(dead_pid()), false).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the holder's session file");
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old), false).step,
        "refused:no-restart-in-flight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The carry of a restart already adopted answers `adopted` at once, dialling and writing nothing
/// however old the relaunch.
#[cfg(unix)]
#[test]
fn the_carry_of_a_restart_already_adopted_answers_without_waiting() {
    use super::super::relaunch::carry_restart;
    let dir = scratch("carry-adopted");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, me) = (dead_pid(), std::process::id());
    let now = now_s();
    let relaunched = |at_s: u64, relaunched_pid: u32| St {
        phase: Phase::Relaunched { at_s },
        pid: old,
        shell: me,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at: now - 3,
        relaunched_pid,
        ..St::default()
    };
    register(&opts.home, me, SESSION);
    for at_s in [now, now - STALE_S - 1] {
        save(&opts, SESSION, &relaunched(at_s, me));
        let saved = load(&opts, SESSION).expect("the record");
        let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old), false);
        assert_eq!((r.step.as_str(), r.pid), ("adopted", me), "{at_s}");
        assert_eq!(load(&opts, SESSION), Some(saved), "{at_s}: nothing written");
    }
    assert_eq!(ledger_lines(&opts), 0, "nothing said");
    save(&opts, SESSION, &relaunched(now, 0));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old), false).step,
        "wait:no-socket",
        "not adopted yet"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the adopted process's session file");
    save(&opts, SESSION, &relaunched(now, me));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old), false).step,
        "wait:no-socket",
        "holding it no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The carry of a restart whose adopted process ended before its continuation closes it at once
/// (`exited-before-continuing`, as `after_exit` closes one overtaken) and answers `landed`.
#[cfg(unix)]
#[test]
fn the_carry_of_a_restart_whose_adopted_agent_is_gone_closes_it() {
    use super::super::relaunch::{Outcome, carry_restart, outcome, restarted};
    let dir = scratch("carry-adopted-gone");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, gone) = (dead_pid(), dead_pid());
    let now = now_s();
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now },
            pid: old,
            shell: std::process::id(),
            tab: TAB.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            exited_at: now - 3,
            relaunched_pid: gone,
            ..St::default()
        },
    );
    let saved = load(&opts, SESSION).expect("the record");
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        carry_restart(&dry, upgrade::Agent::Claude, Some(old), false).step,
        "landed"
    );
    assert_eq!(
        load(&opts, SESSION),
        Some(saved),
        "a dry run writes nothing"
    );
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old), false);
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(
        (st.phase, st.exited_at),
        (Phase::Failed("exited-before-continuing".to_string()), 0)
    );
    assert_eq!(ledger_lines(&opts), 1, "said once, in the ledger");
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(old)),
        "the restart's no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent aterm's own crash took (its shell's hangup made it remove its record, which reads as a
/// graceful exit) is relaunched in its reopened tab by `after_host_ended`.
#[test]
fn an_agent_whose_host_ended_is_relaunched_in_its_reopened_tab() {
    let a = DeadAgent::new("host-ended");
    assert_eq!(a.attempt(&ExitRecord::Removed), "ended:graceful-exit");
    let r = after_host_ended(&a.opts, &a.snap, true);
    assert_eq!(r.step, "would-relaunch");
    assert_eq!(r.session, SESSION);
    let old = St {
        tab: "s-0000000000000000dead".to_string(),
        pid: a.snap.pid,
        phase: super::super::upgrade::Phase::Exiting { at_s: now_s() },
        ..St::default()
    };
    save(
        &Opts {
            dry_run: false,
            ..a.opts.clone()
        },
        SESSION,
        &old,
    );
    assert_eq!(
        after_exit(
            &a.opts,
            &a.snap,
            &ExitRecord::Removed,
            true,
            Some(super::super::restart_stamp::Cause::Stall)
        )
        .step,
        "wait:conversation-in-other-tab",
        "a relaunch on exit waits for the tab its record names"
    );
    assert_eq!(
        after_host_ended(&a.opts, &a.snap, true).step,
        "would-relaunch"
    );
    assert!(super::super::relaunch::relaunch_cause(CAUSE_HOST));
}

/// Conformance test of `HarnessExitRecord` (`aterm_models_harness::harness_exit_record_model`).
#[test]
fn the_real_exit_read_conforms_to_the_model() {
    let model = aterm_models_harness::harness_exit_record_model();
    let mut paths: Vec<Vec<&'static str>> = Vec::new();
    let mut open = vec![(model.init_state(), Vec::new())];
    while let Some((state, path)) = open.pop() {
        let next: Vec<&'static str> = model
            .actions
            .iter()
            .map(|a| a.name)
            .filter(|a| model.action_enabled(a, &state))
            .collect();
        if next.is_empty() {
            paths.push(path);
            continue;
        }
        for action in next {
            let mut s = state.clone();
            assert!(model.fire(action, &mut s));
            let mut p = path.clone();
            p.push(action);
            open.push((s, p));
        }
    }
    assert!(paths.len() >= 3, "{paths:?}");
    let a = DeadAgent::new("exit-record-model");
    let project = |exit: i64, left: &Option<ExitRecord>, decided: i64| {
        [
            ("exit", exit),
            (
                "record",
                i64::from(look_at_exit(&a.opts.home, &a.snap).record.is_some()),
            ),
            ("seen", i64::from(left.is_some())),
            (
                "kept",
                i64::from(matches!(left, Some(ExitRecord::Survived(_)))),
            ),
            ("decided", decided),
        ]
        .into_iter()
        .collect::<aterm_spec::interp::State>()
    };
    let replay = |m: &aterm_spec::derive::Model, path: &[&'static str]| {
        let _ = std::fs::remove_file(&a.record);
        a.write_record();
        let mut expect = m.init_state();
        let (mut exit, mut decided) = (0, 0);
        let mut left: Option<ExitRecord> = None;
        let mut seen = expect.clone();
        for &action in path {
            match action {
                "Crash" => exit = 1,
                "Graceful" => exit = 2,
                "OwnRemoval" | "Sweep" => a.remove_record(),
                "Look" => left = Some(a.look(|_| {})),
                "Decide" | "DecideAtAttempt" => {
                    let handed = if action == "Decide" {
                        left.clone().expect("looked at")
                    } else {
                        ExitRecord::Unread
                    };
                    decided = match a.attempt(&handed).as_str() {
                        "would-relaunch" => 1,
                        "ended:graceful-exit" => 2,
                        other => panic!("{path:?}: {action} said {other}"),
                    };
                }
                other => unreachable!("{other}"),
            }
            assert!(
                m.fire(action, &mut expect),
                "{action} disabled at {expect:?}"
            );
            seen = project(exit, &left, decided);
            assert_eq!(seen, expect, "{path:?}: {action}: observed vs model");
        }
        seen
    };
    for path in &paths {
        let end = replay(&model, path);
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &end),
                "{path:?}: {} broken by {end:?}",
                inv.name
            );
        }
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let witness = ["Crash", "Look", "Sweep", "DecideAtAttempt"];
    let end = replay(&buggy, &witness);
    assert_eq!(end["decided"], 2, "the crash left as a graceful exit");
    assert!(!buggy.check_invariant("CrashIsRelaunched", &end), "{end:?}");
    let _ = std::fs::remove_file(&a.record);
    a.write_record();
    let left = Some(a.look(|looks| {
        if looks == 1 {
            a.remove_record();
        }
    }));
    let mut expect = model.init_state();
    let mut instant = buggy.init_state();
    for (action, naive) in [
        ("Graceful", "Graceful"),
        ("OwnRemoval", "LookAtInstant"),
        ("Look", "OwnRemoval"),
    ] {
        assert!(model.fire(action, &mut expect) && buggy.fire(naive, &mut instant));
    }
    let seen = project(2, &left, 0);
    assert_eq!(
        seen, expect,
        "the removal inside the settle: observed vs model"
    );
    assert_ne!(
        seen, instant,
        "the real look is not the look at the instant"
    );
    assert!(buggy.fire("Decide", &mut instant));
    assert!(
        !buggy.check_invariant("GracefulIsLeft", &instant),
        "{instant:?}"
    );
    for act in &model.actions {
        assert!(
            paths.iter().any(|p| p.contains(&act.name))
                || witness.contains(&act.name)
                || act.name == "LookAtInstant",
            "{} is never driven",
            act.name
        );
    }
}

/// The restart in place restarts the tab's own agent on the same program.
#[cfg(unix)]
#[test]
fn a_restart_in_place_is_planned_on_the_same_program_and_carried_on_once() {
    let dir = scratch("restart-here");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let snap = |argv: &[&str]| Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let claude = ["/opt/claude/bin/claude", "--verbose"];
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Err("no-foreground")).step,
        "wait:no-foreground"
    );
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&claude))).step,
        "wait:no-session-record"
    );
    register(&opts.home, agent.id(), SESSION);
    let r = restart_from(&opts, &Restart::Memory, Ok(snap(&claude)));
    assert_eq!(
        (r.step.as_str(), r.session.as_str(), r.to.as_str()),
        ("would-restart", SESSION, "1.0.0(same)"),
        "{r:?}"
    );
    let other = Snapshot {
        tab: "s-0ther".to_string(),
        ..snap(&claude)
    };
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(other)).step,
        "wait:tab-identity-conflict"
    );
    assert_eq!(
        restart_from(
            &opts,
            &Restart::Memory,
            Ok(snap(&["/opt/claude/bin/claude", "-p", "hi"]))
        )
        .step,
        "refused:one-shot"
    );
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: agent.id(),
        shell: shell.id(),
        tab: TAB.to_string(),
        cause: "memory".to_string(),
        ..St::default()
    };
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("in flight");
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&claude))).step,
        "would-resume:exiting"
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A main-chain answer by `model` stamped `at` (RFC 3339): what the live
/// model is read from past a process's start ([`models::live_model_at`]).
#[cfg(unix)]
fn answer_at(model: &str, at: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","timestamp":"{at}","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"Done."}}]}},"version":"2.1.283"}}"#
    )
}

/// The relaunch snapshot of an agent under the user's own Claude home
/// holding conversation `session`: what the window host hands the readers of
/// its limit and its wall ([`named_standing_wall`]).
#[cfg(unix)]
fn users_agent(session: &str) -> Snapshot {
    Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(session.to_string()),
        cwd: "/".to_string(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// The conversation `SESSION`'s transcript under `home`: `rows`, one a line.
#[cfg(unix)]
fn write_transcript(home: &Path, rows: &[String]) {
    let project = home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        rows.join("\n") + "\n",
    )
    .expect("transcript");
}

/// The model a person chose for `SESSION` with `/model`, as the harness's
/// record remembers it past the answers that follow ([`ModelRecord::human`]).
#[cfg(unix)]
fn chose_by_hand(opts: &Opts, model: &str) {
    let dir = state_dir(opts).join("models");
    std::fs::create_dir_all(&dir).expect("models");
    let rec = ModelRecord {
        human: model.to_string(),
        ..ModelRecord::default()
    };
    std::fs::write(dir.join(format!("{SESSION}.json")), rec.render()).expect("record");
}

/// A `/model` choice rides a memory restart only over a launch's `--model` (which overrides the
/// default `/model` saves; driven: `a_restart_in_place_saves_the_fallback_origin_it_decides`).
#[cfg(unix)]
#[test]
fn a_memory_restart_asks_no_hand_chosen_model_of_a_launch_that_named_none() {
    let dir = scratch("memory-hand-model");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let snap = |argv: &[&str]| Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    register(&opts.home, agent.id(), SESSION);
    write_transcript(
        &opts.home,
        &[answer_at("claude-opus-5-5", "2099-01-01T00:00:00Z")],
    );
    chose_by_hand(&opts, "claude-opus-5-5");
    assert_eq!(
        restart_from(
            &opts,
            &Restart::Memory,
            Ok(snap(&["/opt/claude/bin/claude", "--verbose"]))
        )
        .step,
        "would-restart"
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stalled agent ended with no record left takes its process start from the host's snapshot.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_with_no_record_carries_no_choice_older_than_the_process() {
    let mut a = DeadAgent::new("exit-hand-older");
    a.snap.argv = ["/opt/claude/bin/claude", "--model", "claude-sonnet-5"]
        .iter()
        .map(|w| (*w).to_string())
        .collect();
    chose_by_hand(&a.opts, "claude-opus-5-5");
    let stalled = |a: &DeadAgent| {
        after_exit(
            &a.opts,
            &a.snap,
            &ExitRecord::Removed,
            true,
            Some(super::super::restart_stamp::Cause::Stall),
        )
    };
    write_transcript(
        &a.opts.home,
        &[answer_at("claude-opus-5-5", "2026-09-23T00:00:00Z")],
    );
    assert_eq!(stalled(&a).step, "would-relaunch");
    write_transcript(
        &a.opts.home,
        &[answer_at("claude-opus-5-5", "2026-09-25T00:00:00Z")],
    );
    assert_eq!(stalled(&a).step, "would-relaunch:model=claude-opus-5-5");
}

/// The way back outlives a memory restart.
#[test]
fn the_way_back_from_a_fallback_returns_to_the_launched_model_across_a_memory_restart() {
    let launched = "claude-fable-5-1";
    let fallback = "claude-opus-5";
    let fell = model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        launched.to_string(),
        None,
        None,
    );
    assert_eq!(fell.model.as_deref(), Some(fallback));
    let memory = model_restart(
        &Restart::Memory,
        Some(&saved_restart(&fell)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(memory.cause, "memory");
    assert_eq!(memory.model, None);
    assert_eq!(memory.fallback_from.as_deref(), Some(launched));
    let back = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&memory)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(back.model.as_deref(), Some(launched));
    assert_eq!(back.cause, format!("model-back:{launched}"));
    let argv = |w: &[&str]| w.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
    assert_eq!(
        with_model(
            argv(&["--model", fallback, "--resume", SESSION]),
            back.model.as_deref()
        ),
        argv(&["--model", launched, "--resume", SESSION])
    );
    assert_eq!(fallback_origin(&saved_restart(&back)), None);
    let again = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&back)),
        launched.to_string(),
        None,
        None,
    );
    assert_eq!(again.model.as_deref(), Some(""), "no fallback: none");
    let second = model_restart(
        &Restart::Model {
            to: "claude-sonnet-5".to_string(),
        },
        Some(&saved_restart(&memory)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(second.fallback_from.as_deref(), Some(launched));
    let legacy = St {
        phase: Phase::Done,
        cause: format!("model:{fallback}"),
        launch_model: launched.to_string(),
        ..St::default()
    };
    assert_eq!(fallback_origin(&legacy).as_deref(), Some(launched));
}

/// The way back prefers what the person had over the notice's alias.
#[test]
fn the_way_back_prefers_the_persons_model_over_the_notices_alias() {
    let launched = "claude-fable-5-1[1m]";
    let fell = model_restart(
        &Restart::Model {
            to: "claude-opus-5-5".to_string(),
        },
        None,
        launched.to_string(),
        None,
        None,
    );
    let named = || Restart::ModelBack {
        to: Some("fable".to_string()),
    };
    let back = model_restart(
        &named(),
        Some(&saved_restart(&fell)),
        "claude-opus-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(back.model.as_deref(), Some(launched), "the exact origin");
    let chosen = model_restart(
        &named(),
        Some(&saved_restart(&fell)),
        "claude-opus-5-5".to_string(),
        None,
        Some("claude-sonnet-5-5".to_string()),
    );
    assert_eq!(
        chosen.model.as_deref(),
        Some("claude-sonnet-5-5"),
        "a /model since the fallback is kept"
    );
    let bare = model_restart(
        &Restart::Model {
            to: "claude-opus-5-5".to_string(),
        },
        None,
        String::new(),
        None,
        None,
    );
    let to_default = model_restart(
        &named(),
        Some(&saved_restart(&bare)),
        "claude-opus-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(to_default.model.as_deref(), Some(""));
    let alias = model_restart(&named(), None, "claude-opus-5-5".to_string(), None, None);
    assert_eq!(alias.model.as_deref(), Some("fable"));
}

/// The record a restart saves, as the state file keeps it.
fn saved_restart(r: &super::super::relaunch::ModelRestart) -> St {
    let st = St {
        phase: Phase::Done,
        cause: r.cause.clone(),
        launch_model: r.launch_model.clone(),
        fallback_from: r.fallback_from.clone(),
        ..St::default()
    };
    St::from_json(&st.to_json()).expect("the record reads back")
}

/// A fallback from a launch that named no model still stands (recorded apart from the model it
/// replaced).
#[test]
fn a_second_fallback_from_a_launch_with_no_model_goes_back_to_no_model() {
    let first = model_restart(
        &Restart::Model {
            to: "claude-opus-5".to_string(),
        },
        None,
        String::new(),
        None,
        None,
    );
    assert_eq!(
        fallback_origin(&saved_restart(&first)).as_deref(),
        Some(""),
        "a fallback stands, from no model"
    );
    let second = model_restart(
        &Restart::Model {
            to: "claude-sonnet-5".to_string(),
        },
        Some(&saved_restart(&first)),
        "claude-opus-5".to_string(),
        None,
        None,
    );
    let back = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&second)),
        "claude-sonnet-5".to_string(),
        None,
        None,
    );
    assert_eq!(
        back.model.as_deref(),
        Some(""),
        "drop --model: Claude's default"
    );
    assert_eq!(
        with_model(
            vec!["--model".into(), "claude-sonnet-5".into()],
            back.model.as_deref()
        ),
        Vec::<String>::new()
    );
}

/// A `/model` made with no fallback standing is no fallback's origin.
#[test]
fn a_hand_choice_with_no_fallback_standing_is_no_fallbacks_origin() {
    let dir = scratch("hand-over-fallback");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Done,
            ..St::default()
        },
    );
    assert_eq!(
        exit_fallback(&opts, SESSION, None, Some("claude-sonnet-5")),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The way back outlives an upgrade.
#[test]
fn an_upgrade_record_keeps_the_fallbacks_origin() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: "claude-opus-5".to_string(),
        },
        None,
        "claude-fable-5-1".to_string(),
        None,
        None,
    ));
    let pending = St {
        phase: Phase::Pending,
        to: "2.1.282".to_string(),
        ..fell.clone()
    };
    for (name, prior) in [("done", fell.clone()), ("retargeted", pending)] {
        let next = St::for_target(Some(prior), &from, &to, None, Proc::default(), 900);
        assert_eq!(
            fallback_origin(&next).as_deref(),
            Some("claude-fable-5-1"),
            "{name}"
        );
    }
    let mut legacy = St {
        phase: Phase::Failed("unanswered".to_string()),
        cause: "model:claude-opus-5".to_string(),
        launch_model: "claude-fable-5-1".to_string(),
        ..St::default()
    };
    legacy.rearm(900);
    assert_eq!(
        fallback_origin(&legacy).as_deref(),
        Some("claude-fable-5-1")
    );
    let none = St::for_target(
        Some(St {
            phase: Phase::Done,
            ..St::default()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(fallback_origin(&none), None);
}

/// The profile context a live snapshot captures of an agent started under the test's own home,
/// read off the process's exec environment as `relaunch::snapshot_with` reads it
/// ([`Records::billing_context`]): `HOME` names that home, no `CLAUDE_CONFIG_DIR`, no custom OAuth
/// URL, and the login variable as `login` names it. Its profile file is written as `{}`.
#[cfg(unix)]
fn live_profile(opts: &Opts, login: Option<&str>) -> super::super::profile::Context {
    let env: Vec<String> = std::iter::once(format!("HOME={}", opts.home.display()))
        .chain(login.map(|dir| format!("{}={dir}", crate::harness::accounts::LOGIN_ENV)))
        .collect();
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: String::new(),
        argv: Vec::new(),
        env,
    };
    let records = Records::at(&opts.home, HomeVars::of(&args), || Some(PathBuf::from("/")));
    let context = records
        .billing_context(&opts.home, &args)
        .expect("a live agent's profile context");
    let home = context.plain.parent().expect("the profile's home");
    std::fs::create_dir_all(home).expect("the home");
    std::fs::write(&context.plain, "{}").expect("the profile");
    context
}

/// A restart in place of a stand-in agent launched `--model <launch>`,
/// driven for real ([`restart_with`], its job and terminal scripted): the
/// conversation's last record `prior`, a `/model` of `hand` answered since
/// the agent started — and what it says, and the record it saved.
#[cfg(unix)]
fn restart_saved(
    name: &str,
    why: &Restart,
    launch: &str,
    prior: Option<&St>,
    hand: Option<&str>,
) -> (Report, Option<St>) {
    restart_saved_as(
        name,
        why,
        launch,
        prior,
        hand,
        None,
        &crate::harness::accounts::Roster::default(),
    )
}

/// [`restart_saved`] of an agent whose own exec environment named `login`
/// ([`Snapshot::login`]), by a host that keeps `roster`
/// ([`crate::harness::relaunch::restart_as`]).
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn restart_saved_as(
    name: &str,
    why: &Restart,
    launch: &str,
    prior: Option<&St>,
    hand: Option<&str>,
    login: Option<&str>,
    roster: &crate::harness::accounts::Roster,
) -> (Report, Option<St>) {
    let dir = scratch(name);
    let (sock, _asked) = instance_with(&dir, Answers::default());
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    register(&opts.home, agent.id(), SESSION);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    if let Some(prior) = prior {
        save(&opts, SESSION, prior);
    }
    let answered = hand.unwrap_or(launch);
    write_transcript(&opts.home, &[answer_at(answered, "2099-01-01T00:00:00Z")]);
    if let Some(hand) = hand {
        chose_by_hand(&opts, hand);
    }
    let snap = Snapshot {
        stored_login: false,
        profile: Some(live_profile(&opts, login)),
        tab: TAB.to_string(),
        pid: agent.id(),
        start: kernel_start(agent.id()).expect("lstart"),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: ["/opt/claude/bin/claude", "--model", launch]
            .iter()
            .map(|w| (*w).to_string())
            .collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: login.map(str::to_string),
        home: None,
        claude_dir: None,
    };
    let r = crate::harness::relaunch::restart_as(
        &opts,
        why,
        roster,
        Ok(snap),
        &Script::new(shell.id(), usize::MAX, None),
    );
    let saved = load(&opts, SESSION);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    (r, saved)
}

/// The restart in place saves the fallback origin it decides ([`restart_from`] into
/// [`model_restart`]), driven past its signal.
#[cfg(unix)]
#[test]
fn a_restart_in_place_saves_the_fallback_origin_it_decides() {
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let hand = "claude-opus-5-5";
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        origin.to_string(),
        None,
        None,
    ));
    for (name, why, launch, prior, chose, asks, want) in [
        (
            "saved-memory-hand",
            Restart::Memory,
            fallback,
            Some(&fell),
            Some(hand),
            hand,
            hand,
        ),
        (
            "saved-memory-nobody",
            Restart::Memory,
            fallback,
            Some(&fell),
            None,
            fallback,
            origin,
        ),
        (
            "saved-fallback-hand",
            Restart::Model {
                to: fallback.to_string(),
            },
            origin,
            None,
            Some(hand),
            fallback,
            hand,
        ),
    ] {
        let (r, saved) = restart_saved(name, &why, launch, prior, chose);
        let saved = saved.unwrap_or_else(|| panic!("{name}: no record: {r:?}"));
        assert!(
            !matches!(saved.phase, Phase::Pending | Phase::Done),
            "{name}: signalled: {r:?} {saved:?}"
        );
        assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
        assert!(
            saved.line.contains(&format!("'--model' '{asks}'")),
            "{name}: {}",
            saved.line
        );
        assert_eq!(
            saved.line.contains("'CLAUDE_CODE_CUSTOM_OAUTH_URL='"),
            matches!(why, Restart::Model { .. }),
            "{name}: {}",
            saved.line
        );
    }
}

/// A login directory with a space and an apostrophe in it.
#[cfg(unix)]
const LOGIN_DIR: &str = "/tmp/aterm state/logins/owner's work-2";

/// [`LOGIN_DIR`] as a POSIX shell's relaunch line writes it.
#[cfg(unix)]
const LOGIN_WORD: &str =
    r"CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner'\''s work-2' ";

/// A two-row roster: `main` on this machine's login, `work-2` under [`LOGIN_DIR`].
#[cfg(unix)]
fn two_accounts() -> crate::harness::accounts::Roster {
    crate::harness::accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n"
    ))
    .expect("a roster")
}

/// Every restart in place re-applies the login its process had.
#[cfg(unix)]
#[test]
fn a_restart_in_place_types_back_the_login_its_process_had() {
    let roster = crate::harness::accounts::Roster::default();
    let launch = "claude-fable-5-1";
    let line = |env: &str| {
        format!(
            " ( cd -- '/' && {env}CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec \
             '/opt/claude/bin/claude' '--model' '{launch}' '--resume' '{SESSION}' )"
        )
    };
    for (name, login, want) in [
        ("login-memory-dir", Some(LOGIN_DIR), line(LOGIN_WORD)),
        (
            "login-memory-empty",
            Some(""),
            line("CLAUDE_SECURESTORAGE_CONFIG_DIR='' "),
        ),
        ("login-memory-none", None, line("")),
    ] {
        let (r, saved) =
            restart_saved_as(name, &Restart::Memory, launch, None, None, login, &roster);
        let saved = saved.unwrap_or_else(|| panic!("{name}: no record: {r:?}"));
        assert_eq!(saved.line, want, "{name}");
        assert_eq!(saved.login.as_deref(), login, "{name}");
        assert_eq!(saved.cause, "memory", "{name}");
        assert_eq!(
            saved.to_json().contains("\"login\""),
            login.is_some(),
            "{name}: the key is written only when set"
        );
        assert_eq!(
            St::from_json(&saved.to_json()).map(|st| st.login),
            Some(login.map(str::to_string)),
            "{name}: read back"
        );
    }
    let old = St::from_json(r#"{"phase":"done","line":" '/opt/claude/bin/claude'"}"#);
    assert_eq!(old.map(|st| st.login), Some(None));
}

/// An account move types the target's login from the roster (`work-2`'s directory, or the empty
/// value for `main`, the row with none) and records `account:<label>`.
#[cfg(unix)]
#[test]
fn an_account_move_types_the_rosters_login_and_a_label_nobody_lists_is_refused() {
    use crate::supervise::policy::turn_end::AccountAt;
    let roster = two_accounts();
    let launch = "claude-fable-5-1";
    let to = |label: &str| Restart::Account {
        to: label.to_string(),
        at: AccountAt::Wall,
    };
    let (r, saved) = restart_saved_as(
        "account-to-work",
        &to("work-2"),
        launch,
        None,
        None,
        None,
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(&format!(
            "&& {LOGIN_WORD}CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec "
        )),
        "{}",
        saved.line
    );
    assert_eq!(saved.cause, "account:work-2");
    assert_eq!(saved.login.as_deref(), Some(LOGIN_DIR));
    assert!(!saved.cause.contains("/tmp"), "the label only");
    let (r, saved) = restart_saved_as(
        "account-to-main",
        &to("main"),
        launch,
        None,
        None,
        Some(LOGIN_DIR),
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(
            "&& CLAUDE_SECURESTORAGE_CONFIG_DIR='' CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec "
        ),
        "{}",
        saved.line
    );
    assert!(!saved.line.contains(LOGIN_DIR), "{}", saved.line);
    assert_eq!(
        (saved.cause.as_str(), saved.login.as_deref()),
        ("account:main", Some(""))
    );
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        origin.to_string(),
        None,
        None,
    ));
    let (r, saved) = restart_saved_as(
        "account-way-back",
        &to("work-2"),
        fallback,
        Some(&fell),
        None,
        None,
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(&format!("'--model' '{origin}'")),
        "{}",
        saved.line
    );
    assert_eq!(saved.fallback_from, None, "{r:?}");
    for (name, roster) in [
        ("account-not-listed", &roster),
        (
            "account-no-roster",
            &crate::harness::accounts::Roster::default(),
        ),
    ] {
        let (r, saved) = restart_saved_as(name, &to("pool-a"), launch, None, None, None, roster);
        assert_eq!(r.step, "refused:account-not-listed", "{name}");
        assert_eq!(saved, None, "{name}");
    }
}

/// An exit's relaunch types back the login the agent ran under (read off the process while it ran).
#[cfg(unix)]
#[test]
fn an_exit_relaunch_types_back_the_login_the_agent_ran_under() {
    let case = |name: &str, home: bool, login: Option<&str>, host_ended: bool| {
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        let lab_home = a.dir.join("lab home/.claude");
        a.snap.login = login.map(str::to_string);
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        if home {
            a.snap.home = Some(lab_home.to_string_lossy().into_owned());
            a.write_record_in(&lab_home, SESSION);
            file_transcript_in(&lab_home, "p");
        } else {
            a.write_record();
        }
        let (r, cause) = if host_ended {
            (after_host_ended(&a.opts, &a.snap, true), CAUSE_HOST)
        } else {
            let left = a.look(|_| {});
            (after_exit(&a.opts, &a.snap, &left, true, None), CAUSE_EXIT)
        };
        let saved = load(&a.opts, SESSION).expect("state");
        assert_eq!(saved.cause, cause, "{name}: {r:?}");
        assert_eq!(saved.login.as_deref(), login, "{name}");
        assert_eq!(
            saved.line.contains(LOGIN_WORD),
            login.is_some(),
            "{name}: {}",
            saved.line
        );
        assert_eq!(
            saved.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            login.is_some(),
            "{name}: {}",
            saved.line
        );
        assert_eq!(
            saved
                .line
                .find(&format!("CLAUDE_CONFIG_DIR='{}' ", lab_home.display())),
            home.then(|| saved.line.find("CLAUDE_")).flatten(),
            "{name}: the home, first, or nothing of one: {}",
            saved.line
        );
    };
    std::thread::scope(|scope| {
        let moved = scope.spawn(|| case("exit-login-dir", false, Some(LOGIN_DIR), false));
        let cold = scope.spawn(|| case("host-home-login", true, Some(LOGIN_DIR), true));
        case("exit-login-none", false, None, false);
        moved.join().expect("exit-login-dir");
        cold.join().expect("host-home-login");
    });
}

/// A move outlives the records written over it.
#[cfg(unix)]
#[test]
fn a_move_outlives_the_relaunch_records_written_over_it() {
    const WRITTEN: u64 = 1_791_477_331;
    let weekly = include_str!("../supervise/fixtures/claude-2.1.294-transcript-weekly-limit.jsonl");
    let sonnet = weekly
        .replace(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            "You've hit your Sonnet limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
        )
        .replace(
            "\"rateLimitType\":\"seven_day\"",
            "\"rateLimitType\":\"seven_day_sonnet\"",
        );
    let case = |name: &str, cause: &str, moved: u64, host_ended: bool, tail: &str| {
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        a.write_record();
        let account = cause.starts_with("account:");
        save(
            &a.opts,
            SESSION,
            &St {
                phase: Phase::Relaunched { at_s: now_s() },
                cause: cause.to_string(),
                pid: dead_pid(),
                relaunched_pid: a.snap.pid,
                relaunched_at: moved,
                login: account.then(|| "/accts/work-2".to_string()),
                tab: if host_ended {
                    "s-0000000000000000dead".to_string()
                } else {
                    TAB.to_string()
                },
                from: "2.1.283".to_string(),
                to: "2.1.283".to_string(),
                source: SAME_PROGRAM.to_string(),
                ..St::default()
            },
        );
        let (r, want) = if host_ended {
            (after_host_ended(&a.opts, &a.snap, true), CAUSE_HOST)
        } else {
            let left = a.look(|_| {});
            (after_exit(&a.opts, &a.snap, &left, true, None), CAUSE_EXIT)
        };
        let saved = load(&a.opts, SESSION).expect("state");
        assert_eq!(saved.cause, want, "{name}: {r:?}");
        if account || cause.starts_with("model:") {
            assert_eq!(
                saved.moved_login.as_deref(),
                account.then_some("/accts/work-2"),
                "{name}"
            );
        }
        let live = Snapshot {
            login: account.then(|| "/accts/work-2".to_string()),
            argv: if cause.starts_with("model:") {
                vec![
                    a.snap.program.to_string_lossy().into_owned(),
                    "--model".to_string(),
                    "opus".to_string(),
                ]
            } else {
                a.snap.argv.clone()
            },
            ..a.snap.clone()
        };
        super::super::relaunch::standing_limit(&a.opts, &live, tail)
    };
    let after = WRITTEN + 600;
    let before = WRITTEN - 600;
    std::thread::scope(|scope| {
        let cold = [
            scope.spawn(|| case("moved-account-cold", "account:work-2", after, true, weekly)),
            scope.spawn(|| case("moved-model-cold", "model:opus", after, true, &sonnet)),
            scope.spawn(|| {
                case(
                    "moved-account-crash",
                    "account:work-2",
                    after,
                    false,
                    weekly,
                )
            }),
            scope.spawn(|| case("moved-model-crash", "model:opus", after, false, &sonnet)),
        ];
        for (n, moved) in cold.into_iter().enumerate() {
            assert_eq!(
                moved.join().expect("joined"),
                None,
                "case {n}: the move left it"
            );
        }
        let controls = [
            scope.spawn(|| case("row-after-account", "account:work-2", before, true, weekly)),
            scope.spawn(|| case("row-after-model", "model:opus", before, false, &sonnet)),
            scope.spawn(|| case("no-move", CAUSE_EXIT, after, true, weekly)),
        ];
        for (n, stands) in controls.into_iter().enumerate() {
            assert!(
                stands.join().expect("joined").is_some(),
                "control {n}: the limit stands"
            );
        }
    });
}

/// A transcript of [`SESSION`] with one prompt, filed under `claude_dir`'s project
/// `project` as Claude Code files one with its first message.
#[cfg(unix)]
fn file_transcript_in(claude_dir: &Path, project: &str) {
    let dir = claude_dir.join("projects").join(project);
    std::fs::create_dir_all(&dir).expect("project");
    std::fs::write(
        dir.join(format!("{SESSION}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":"port the parser"}}"#.to_string()
            + "\n",
    )
    .expect("transcript");
}

/// [`Tasked::of_snapshot`].
#[cfg(unix)]
#[test]
fn the_turn_ends_task_read_is_the_agents_own_records() {
    let dir = scratch("tasked-own-home");
    let user_home = dir.join("home");
    std::fs::create_dir_all(user_home.join(".claude")).expect("the user's home, no projects");
    let work = dir.join("work/.claude");
    file_transcript_in(&work, "p");
    let own = Snapshot {
        home: Some(work.to_string_lossy().into_owned()),
        claude_dir: Some(work.clone()),
        ..users_agent(SESSION)
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &own, &[]),
        Some(true),
        "its own transcript: a person's prompt"
    );
    assert_eq!(
        Tasked::default().of(&user_home, SESSION, &[]),
        Some(false),
        "the user's ~/.claude says nothing of it"
    );
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &users_agent(SESSION), &[]),
        Some(false)
    );
    let relative = Snapshot {
        home: Some("work/.claude".to_string()),
        cwd: dir.to_string_lossy().into_owned(),
        ..users_agent(SESSION)
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &relative, &[]),
        Some(true),
        "a relative home, where it resolves"
    );
    let unknown = Snapshot {
        cwd: String::new(),
        ..relative
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &unknown, &[]),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent started under another Claude home is relaunched after aterm ended
/// ([`after_host_ended`]) off its own records in `<dir>`.
#[cfg(unix)]
#[test]
fn an_agent_under_another_claude_home_is_relaunched_off_its_own_records() {
    /// Where the conversation's transcript is.
    #[derive(Clone, Copy)]
    enum Filed {
        /// Under the agent's own Claude home.
        Own,
        /// Under the user's `~/.claude` alone.
        UsersOnly,
    }
    let case = |name: &str, own_home: bool, record: bool, transcript: Filed, resumed: bool| {
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        let users = a.opts.home.join(".claude");
        std::fs::create_dir_all(users.join("projects/-other")).expect("the user's projects");
        let claude_dir = if own_home {
            let dir = a.dir.join("work home/.claude");
            a.snap.home = Some(dir.to_string_lossy().into_owned());
            dir
        } else {
            users.clone()
        };
        std::fs::create_dir_all(claude_dir.join("projects/-elsewhere")).expect("its projects");
        if record {
            a.snap.session = None;
            a.write_record_in(&claude_dir, SESSION);
        }
        match transcript {
            Filed::Own => file_transcript_in(&claude_dir, "-work"),
            Filed::UsersOnly => file_transcript_in(&users, "-work"),
        }
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        let r = after_host_ended(&a.opts, &a.snap, true);
        let saved =
            load(&a.opts, SESSION).unwrap_or_else(|| panic!("{name}: nothing relaunched: {r:?}"));
        let cause = if resumed {
            CAUSE_HOST
        } else {
            super::super::relaunch::CAUSE_FRESH
        };
        assert_eq!(saved.cause, cause, "{name}: {r:?}");
        assert_eq!(
            saved.line.contains(&format!("'--resume' '{SESSION}'")),
            resumed,
            "{name}: {}",
            saved.line
        );
        let home_word = format!("CLAUDE_CONFIG_DIR='{}' ", claude_dir.display());
        assert_eq!(
            saved.line.contains(&home_word),
            own_home,
            "{name}: {}",
            saved.line
        );
        assert_eq!(saved.home, a.snap.home, "{name}");
        assert_eq!(
            saved.to_json().contains("\"home\""),
            own_home,
            "{name}: the key is written only when set"
        );
        assert_eq!(
            St::from_json(&saved.to_json()).map(|st| st.home),
            Some(a.snap.home.clone()),
            "{name}: read back"
        );
    };
    std::thread::scope(|scope| {
        let crashed = scope.spawn(|| case("home-record", true, true, Filed::Own, true));
        let known = scope.spawn(|| case("home-transcript", true, false, Filed::Own, true));
        let not_its = scope.spawn(|| case("home-not-its", true, false, Filed::UsersOnly, false));
        case("home-users", false, true, Filed::Own, true);
        crashed.join().expect("home-record");
        known.join().expect("home-transcript");
        not_its.join().expect("home-not-its");
    });
    let old = St::from_json(r#"{"phase":"relaunched","at":7,"cause":"host"}"#);
    assert_eq!(old.map(|st| st.home), Some(None));
}

/// The host's looks ([`look_at_exit`], [`follow`]) read the agent's record under the Claude home
/// its snapshot names.
#[cfg(unix)]
#[test]
fn the_hosts_looks_read_the_record_under_the_agents_own_claude_home() {
    const CLEARED: &str = "3d1c5a0e-7f42-4b8e-9c61-0a5b2e7d9f13";
    let mut a = DeadAgent::new("look-home");
    let claude_dir = a.dir.join("work home/.claude");
    a.snap.home = Some(claude_dir.to_string_lossy().into_owned());
    a.write_record_in(&claude_dir, SESSION);
    assert!(
        look_at_exit(&a.opts.home, &a.snap).record.is_some(),
        "its record is read where it is"
    );
    let left = a.look(|_| {});
    assert!(matches!(left, ExitRecord::Survived(_)), "a crash: {left:?}");
    a.write_record_in(&claude_dir, CLEARED);
    let mut snap = a.snap.clone();
    assert!(follow(&a.opts.home, &mut snap), "followed");
    assert_eq!(snap.session.as_deref(), Some(CLEARED));
}

/// Claude's record of `pid` (started `start`, idle) holding [`SESSION`],
/// running in `cwd`, filed in the Claude Code directory `claude_dir`: where a
/// Claude Code whose home resolves there files it.
fn register_running_in(claude_dir: &Path, pid: u32, start: &str, cwd: &Path) {
    std::fs::create_dir_all(claude_dir.join("sessions")).expect("sessions");
    std::fs::write(
        claude_dir.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"{}","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#,
            cwd.display()
        ),
    )
    .expect("the record");
}

/// A relative Claude home (`CLAUDE_CONFIG_DIR=work home/.claude`) is followed where the line's `cd`
/// puts it.
#[cfg(unix)]
#[test]
fn a_relative_claude_home_is_followed_by_the_relaunch_and_the_restart_in_place() {
    const RELATIVE: &str = "work home/.claude";
    let record = register_running_in;
    let mut a = DeadAgent::new("relative-home-relaunch");
    let (sock, _asked) = instance_with(&a.dir, Answers::default());
    a.opts = Opts {
        sock: Some(sock),
        dry_run: false,
        ..a.opts.clone()
    };
    let work = a.dir.to_path_buf();
    let claude_dir = work.join(RELATIVE);
    a.snap.cwd = work.to_string_lossy().into_owned();
    a.snap.home = Some(RELATIVE.to_string());
    record(&claude_dir, a.snap.pid, &a.snap.start, &work);
    file_transcript_in(&claude_dir, "-work");
    assert!(
        look_at_exit(&a.opts.home, &a.snap).record.is_some(),
        "its crash's record is read where its home resolves"
    );
    std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
    let r = after_host_ended(&a.opts, &a.snap, true);
    let saved = load(&a.opts, SESSION).unwrap_or_else(|| panic!("nothing relaunched: {r:?}"));
    assert!(
        saved.line.contains(&format!("'--resume' '{SESSION}'")),
        "resumed: {}",
        saved.line
    );
    assert!(
        saved
            .line
            .contains(&format!("CLAUDE_CONFIG_DIR='{RELATIVE}' ")),
        "the line carries the home as the agent had it: {}",
        saved.line
    );
    assert_eq!(
        saved.home.as_deref().map(Path::new),
        Some(claude_dir.as_path()),
        "the relaunched process is looked for where it files its record"
    );

    let dir = scratch("relative-home-restart");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let work = dir.join("work");
    let claude_dir = work.join(RELATIVE);
    let snap = Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: Some(RELATIVE.to_string()),
        claude_dir: None,
    };
    record(&claude_dir, agent.id(), &start, &work);
    let restarted = restart_from(&opts, &Restart::Memory, Ok(snap.clone())).step;
    record(&claude_dir, agent.id(), &start, &dir.join("elsewhere"));
    let moved = restart_from(&opts, &Restart::Memory, Ok(snap)).step;
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(restarted, "would-restart", "restarted on its record");
    assert_eq!(
        moved, "would-refuse:home",
        "a line that would move its home"
    );
}

/// The restart in place of a relative home's agent records the directory the relaunched process
/// files under ([`Records::home_word`]).
#[cfg(unix)]
#[test]
fn a_restart_in_place_records_the_directory_a_relative_homes_agent_files_under() {
    const RELATIVE: &str = "work home/.claude";
    let dir = scratch("restart-relative-home-record");
    let (sock, _asked) = instance_with(&dir, Answers::default());
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let work = dir.join("work");
    let claude_dir = work.join(RELATIVE);
    let mut shell = stand_in_shell();
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    register_running_in(&claude_dir, agent.id(), &start, &work);
    file_transcript_in(&claude_dir, "-work");
    let snap = Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: agent.id(),
        start,
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: Some(RELATIVE.to_string()),
        claude_dir: None,
    };
    let r = crate::harness::relaunch::restart_as(
        &opts,
        &Restart::Memory,
        &crate::harness::accounts::Roster::default(),
        Ok(snap),
        &Script::new(shell.id(), usize::MAX, None),
    );
    let saved = load(&opts, SESSION);
    let mut relaunched = parked_bare().spawn().expect("the relaunched agent");
    wait_exec(relaunched.id());
    let again = kernel_start(relaunched.id()).expect("lstart");
    register_running_in(&claude_dir, relaunched.id(), &again, &work);
    let found = saved
        .as_ref()
        .and_then(|st| st.agent_records(&opts.home).session_file(relaunched.id()));
    let _ = relaunched.kill();
    let _ = relaunched.wait();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let saved = saved.unwrap_or_else(|| panic!("nothing restarted: {r:?}"));
    assert!(
        saved
            .line
            .contains(&format!("CLAUDE_CONFIG_DIR='{RELATIVE}' ")),
        "the line carries the home as the agent had it: {}",
        saved.line
    );
    assert_eq!(
        saved.home.as_deref().map(Path::new),
        Some(claude_dir.as_path()),
        "the record names the directory, not the raw value"
    );
    assert!(
        found.is_some(),
        "the relaunched process is found where it files its record"
    );
}

/// The restart in place types the usage tap under the switch and writes no settings file.
#[cfg(unix)]
#[test]
fn a_restart_in_place_types_the_usage_tap_under_the_switch_and_writes_no_settings() {
    use super::super::accounts::{self, Roster};
    use super::super::tap;
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let value = tap::settings_value(exe).expect("an admissible path");
    let armed = |lone_login: Option<&str>| {
        Some(tap::Armed {
            exe: exe.to_path_buf(),
            lone_login: lone_login.map(str::to_owned),
            listed: Vec::new(),
        })
    };
    let run = |label: &str, usage_tap: Option<tap::Armed>, why: &Restart, roster: &Roster| {
        let dir = scratch(label);
        let (sock, _asked) = instance_with(&dir, Answers::default());
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            usage_tap,
            ..drive(&dir)
        };
        seed_builds(&opts);
        let claude_dir = opts.home.join(".claude");
        std::fs::create_dir_all(claude_dir.join("sessions")).expect("the user's sessions");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let settings = claude_dir.join("settings.json");
        let text = "{\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n";
        std::fs::write(&settings, text).expect("the user's settings");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("work");
        let mut shell = stand_in_shell();
        let mut agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let start = kernel_start(agent.id()).expect("lstart");
        register_running_in(&claude_dir, agent.id(), &start, &work);
        file_transcript_in(&claude_dir, "-work");
        let snap = Snapshot {
            stored_login: true,
            profile: None,
            tab: TAB.to_string(),
            pid: agent.id(),
            start,
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: work.to_string_lossy().into_owned(),
            version: Some("1.0.0".to_string()),
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let r = crate::harness::relaunch::restart_as(
            &opts,
            why,
            roster,
            Ok(snap),
            &Script::new(shell.id(), usize::MAX, None),
        );
        let saved = load(&opts, SESSION);
        let after = std::fs::read_to_string(&settings).expect("settings");
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let saved = saved.unwrap_or_else(|| panic!("nothing restarted: {r:?}"));
        assert_eq!(after, text, "the user's settings are untouched");
        saved.line
    };
    let none = Roster::default();
    let tapped = run("restart-usage-tap-on", armed(None), &Restart::Memory, &none);
    assert!(
        tapped.contains(&format!("'--settings' '{value}' '--resume' '{SESSION}'")),
        "{tapped}"
    );
    let plain = run("restart-usage-tap-off", None, &Restart::Memory, &none);
    assert!(!plain.contains("--settings"), "{plain}");
    assert!(
        plain.contains(&format!("'--resume' '{SESSION}'")),
        "{plain}"
    );
    let renewed = "/tmp/aterm-state/logins/home-a.1";
    let one = Roster::parse(
        "[[account]]\nlabel = \"home-a\"\nclass = \"dedicated\"\nemail = \"owner@example.com\"\n",
    )
    .expect("a roster");
    let mut logins = accounts::Logins::default();
    let mut state = accounts::State::default();
    accounts::note_renewed(&mut state, &mut logins, "home-a", None, renewed, None, 1);
    let one = accounts::bound(&one, &logins);
    let follow = Restart::Account {
        to: "home-a".to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Ahead,
    };
    let followed = run(
        "restart-usage-tap-follow",
        armed(one.lone_login()),
        &follow,
        &one,
    );
    assert!(
        followed.contains(&format!(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR={renewed} CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1"
        )),
        "{followed}"
    );
    assert!(
        followed.contains(&format!("'--settings' '{value}' '--resume' '{SESSION}'")),
        "{followed}"
    );
}

/// The relaunch on exit refuses a line whose `cd` would move a relative home (the crash's record
/// says it ran elsewhere, where the same home names another directory).
#[cfg(unix)]
#[test]
fn the_relaunch_on_exit_refuses_a_line_that_would_move_a_relative_home() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, ran_in: &str| {
        let mut a = DeadAgent::new(name);
        let work = a.dir.join("work");
        let claude_dir = work.join(RELATIVE);
        a.snap.cwd = work.to_string_lossy().into_owned();
        a.snap.home = Some(RELATIVE.to_string());
        a.snap.claude_dir = Some(claude_dir.clone());
        register_running_in(&claude_dir, a.snap.pid, &a.snap.start, &work.join(ran_in));
        file_transcript_in(&claude_dir, "-work");
        after_exit(&a.opts, &a.snap, &ExitRecord::Unread, false, None).step
    };
    assert_eq!(
        case("exit-relative-home-moved", "elsewhere"),
        "would-refuse:home",
        "a line that would move its home"
    );
    assert_eq!(
        case("exit-relative-home-kept", ""),
        "would-relaunch",
        "CONTROL: its record names where its home resolves"
    );
}

/// The relaunch on exit carries an agent's own `HOME`.
#[cfg(unix)]
#[test]
fn the_relaunch_on_exit_carries_an_agents_own_home() {
    let mut a = DeadAgent::new("exit-own-home");
    let (sock, _asked) = instance_with(&a.dir, Answers::default());
    a.opts = Opts {
        sock: Some(sock),
        dry_run: false,
        ..a.opts.clone()
    };
    let other = a.dir.join("other home");
    let claude_dir = other.join(".claude");
    a.snap.home = None;
    a.snap.claude_dir = Some(claude_dir.clone());
    a.write_record_in(&claude_dir, SESSION);
    file_transcript_in(&claude_dir, "-work");
    std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
    let left = a.look(|_| {});
    assert!(matches!(left, ExitRecord::Survived(_)), "a crash: {left:?}");
    let r = after_exit(&a.opts, &a.snap, &left, true, None);
    let saved = load(&a.opts, SESSION).unwrap_or_else(|| panic!("nothing relaunched: {r:?}"));
    assert!(
        saved.line.contains(&format!("HOME='{}' ", other.display())),
        "{}",
        saved.line
    );
    assert!(
        saved.line.contains(&format!("'--resume' '{SESSION}'")),
        "{}",
        saved.line
    );
    assert_eq!(
        saved.home.as_deref(),
        Some(claude_dir.to_string_lossy().as_ref()),
        "where the new process files its record"
    );
    let mut users = DeadAgent::new("exit-users-home");
    let (sock, _users_asked) = instance_with(&users.dir, Answers::default());
    users.opts = Opts {
        sock: Some(sock),
        dry_run: false,
        ..users.opts.clone()
    };
    users.write_record();
    std::fs::create_dir_all(state_dir(&users.opts)).expect("state");
    let left = users.look(|_| {});
    let r = after_exit(&users.opts, &users.snap, &left, true, None);
    let saved = load(&users.opts, SESSION).unwrap_or_else(|| panic!("nothing relaunched: {r:?}"));
    assert!(!saved.line.contains("HOME="), "{}", saved.line);
}

/// [`super::super::relaunch::snapshot`] resolves the tab leader's `CLAUDE_CONFIG_DIR` into
/// [`Snapshot::claude_dir`].
#[cfg(unix)]
#[test]
fn the_relaunch_snapshot_reads_a_relative_or_empty_home_where_its_agent_runs() {
    /// The agent's `CLAUDE_CONFIG_DIR`.
    #[derive(Clone, Copy)]
    enum Home {
        /// Unset: the user's own `~/.claude`.
        Users,
        /// `<scratch>/abs/.claude`.
        Absolute,
        /// `work home/.claude`, under the directory the agent runs in.
        Relative,
        /// Set and empty: the directory the agent runs in itself.
        Empty,
    }
    let case = |name: &str, home: Home| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("its working directory");
        let absolute = dir.join("abs/.claude");
        let runs_in = std::fs::canonicalize(&work).expect("its working directory");
        let (raw, claude_dir) = match home {
            Home::Users => (None, dir.join("home/.claude")),
            Home::Absolute => (
                Some(absolute.to_string_lossy().into_owned()),
                absolute.clone(),
            ),
            Home::Relative => (
                Some("work home/.claude".to_string()),
                runs_in.join("work home/.claude"),
            ),
            Home::Empty => (Some(String::new()), runs_in.clone()),
        };
        let env: Vec<(&str, &Path)> = raw
            .as_deref()
            .map(|h| (crate::harness::accounts::HOME_ENV, Path::new(h)))
            .into_iter()
            .collect();
        let mut agent = tty_agent(&env, Some(&work));
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        let start = kernel_start(pid).expect("lstart");
        register_running_in(&claude_dir, pid, &start, &work);
        let snap = crate::harness::relaunch::snapshot_with(
            &opts,
            &Script::new(std::process::id(), usize::MAX, None),
        );
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let snap = snap.unwrap_or_else(|why| panic!("{name}: no snapshot: {why}"));
        assert_eq!(
            (snap.session.as_deref(), snap.home.as_deref()),
            (Some(SESSION), raw.as_deref()),
            "{name}: its conversation, and its home as it had it"
        );
        assert_eq!(
            snap.claude_dir.as_deref(),
            Some(claude_dir.as_path()),
            "{name}: where its records are"
        );
    };
    case("snapshot-relative-home", Home::Relative);
    case("snapshot-empty-home", Home::Empty);
    case("snapshot-absolute-home", Home::Absolute);
    case("snapshot-users-home", Home::Users);
}

/// An unset `CLAUDE_CONFIG_DIR` is the agent's own `HOME`'s `.claude`, not the window's
/// (`homedir()` is the agent's `HOME`).
#[cfg(unix)]
#[test]
fn an_unset_claude_home_is_the_agents_own_homes_not_the_windows() {
    let dir = scratch("agent-home-unset");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("its working directory");
    let other = dir.join("other");
    let claude_dir = other.join(".claude");
    let mut agent = tty_agent(&[("HOME", other.as_path())], Some(&work));
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let start = kernel_start(pid).expect("lstart");
    register_running_in(&claude_dir, pid, &start, &work);
    let snap = crate::harness::relaunch::snapshot_with(
        &opts,
        &Script::new(std::process::id(), usize::MAX, None),
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(pid)),
    }];
    let homes = Homes::read(
        &opts,
        Some(&tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let snap = snap.unwrap_or_else(|why| panic!("no snapshot: {why}"));
    assert_eq!(
        (snap.session.as_deref(), snap.claude_dir.as_deref()),
        (Some(SESSION), Some(claude_dir.as_path())),
        "the snapshot reads its record where its own HOME keeps it"
    );
    assert_eq!(
        homes.dirs(),
        vec![claude_dir.clone()],
        "the sweep lists its home"
    );
    use crate::harness::footer::claude_dir_of;
    let home = "/Users//someone";
    let runs_in = || Some(PathBuf::from("/work"));
    assert_eq!(
        claude_dir_of(Some(""), Some(home), runs_in),
        Some(PathBuf::from("/work")),
        "an empty CLAUDE_CONFIG_DIR read as unset"
    );
    assert_eq!(
        claude_dir_of(Some("rel/.claude"), Some(home), runs_in),
        Some(PathBuf::from("/work/rel/.claude")),
        "a relative CLAUDE_CONFIG_DIR read as none"
    );
    assert_eq!(
        claude_dir_of(None, Some(home), runs_in),
        Some(PathBuf::from("/Users//someone/.claude"))
    );
}

/// PID 0 is no process ([`alive`]).
#[test]
fn pid_zero_is_never_alive() {
    assert!(!alive(crate::harness::relaunch::NO_SHELL));
    assert!(alive(std::process::id()));
}

/// The relaunch keeps a snapshot of a tab's own program, and relaunches nothing from it.
#[cfg(unix)]
#[test]
fn the_relaunch_keeps_a_snapshot_of_a_tabs_own_program_and_relaunches_nothing() {
    use crate::harness::relaunch::{
        Launch, NO_SHELL, NoPlan, carries_restart, exit_line, plan, restart_with, resumes_on_exit,
        snapshot_with,
    };
    let dir = scratch("snapshot-pane-program");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("its working directory");
    let mut agent = tty_agent(&[], Some(&work));
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let start = kernel_start(pid).expect("lstart");
    register_running_in(&opts.home.join(".claude"), pid, &start, &work);
    write_transcript(
        &opts.home,
        &[r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-30T19:02:11.123Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]},"quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"error":"rate_limit","isApiErrorMessage":true}"#.to_string()],
    );
    let live = snapshot_with(&opts, &Live).map(|s| s.session);
    let execd = PaneProgram::execd(pid);
    let snap = snapshot_with(&opts, &execd);
    let me = std::process::id();
    let control = snapshot_with(&opts, &PaneProgram::under(pid, me)).map(|s| (s.shell, s.session));
    let snap = snap.unwrap_or_else(|why| panic!("no snapshot: {why}"));
    let wall = named_standing_wall(&opts.home, &snap).and_then(|w| w.until);
    let reset = named_limit_reset(&opts.home, &snap);
    let running = look_at_exit(&opts.home, &snap).running;
    let launch = Launch {
        session: SESSION,
        resume: true,
        cwd: &snap.cwd,
        agent: pid,
        argv: &snap.argv,
        exe: &snap.program,
        model: None,
        mode: None,
        home: None,
        homedir: None,
        login: None,
        stored_login: false,
        billing: crate::harness::relaunch::ModelBilling::Keep,
    };
    let planned = match plan(&opts, &launch, NO_SHELL, &[]) {
        Ok(p) => Ok(p.line),
        Err(NoPlan::Refused { what, .. }) => Err(what),
        Err(NoPlan::Wait(why)) => Err(format!("wait:{why}")),
    };
    let restart = restart_with(&opts, &Restart::Memory, Ok(snap.clone()), &execd).step;
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let mut shell = stand_in_shell();
    let with_shell = Snapshot {
        shell: shell.id(),
        dialect: shell_dialect(shell.id(), &[]),
        argv: snap.argv[..1].to_vec(),
        ..snap.clone()
    };
    let restart_control = restart_with(
        &dry,
        &Restart::Memory,
        Ok(with_shell.clone()),
        &PaneProgram::under(pid, shell.id()),
    )
    .step;
    let _ = agent.kill();
    let _ = agent.wait();
    let look = look_at_exit(&opts.home, &snap);
    let left = ExitRecord::Survived(Box::new(look.record.clone().expect("its record")));
    let relaunch = after_exit(&opts, &snap, &left, false, None).step;
    let relaunch_control = after_exit(&dry, &with_shell, &left, false, None).step;
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(live, Err("terminal"), "no aterm opened its pty");
    assert_eq!(
        control,
        Ok((me, Some(SESSION.to_string()))),
        "CONTROL: a shell between"
    );
    assert_eq!(
        (snap.shell, snap.session.as_deref(), snap.dialect),
        (NO_SHELL, Some(SESSION), None),
        "read, with no shell"
    );
    assert_eq!(wall, Some(1_790_870_400), "its wall read");
    assert_eq!(reset, Some(1_790_870_400), "its limit's reset read");
    assert!(running, "its exit watched");
    assert!(!look.running && look.record.is_some(), "its exit read");
    assert!(!resumes_on_exit(&opts.home, &snap) && !carries_restart(&opts.home, &snap));
    assert_eq!(exit_line(&opts.home, &snap), None);
    assert_eq!(planned, Err("not-a-shell-job".to_string()));
    assert_eq!(restart, "refused:not-a-shell-job");
    assert_eq!(relaunch, "refused:not-a-shell-job");
    assert!(
        resumes_on_exit(&opts.home, &with_shell),
        "CONTROL: {with_shell:?}"
    );
    assert_eq!(restart_control, "would-restart", "CONTROL: a shell");
    assert!(
        relaunch_control.starts_with("would-relaunch"),
        "CONTROL: a shell: {relaunch_control}"
    );
}

/// A relaunch is promised only where its line would carry the agent's claude HOME.
#[cfg(unix)]
#[test]
fn an_agent_whose_relaunch_would_refuse_its_home_is_promised_none() {
    use super::super::relaunch::{carries_restart, exit_line, resumes_on_exit};
    #[derive(Clone, Copy)]
    enum Home {
        /// `HOME=<scratch>/other`, `CLAUDE_CONFIG_DIR` unset.
        Other,
        /// The user's own `HOME`.
        Users,
        /// `HOME=<scratch>/other`, `CLAUDE_CONFIG_DIR=<scratch>/abs/.claude`.
        Absolute,
        /// `CLAUDE_CONFIG_DIR=rel/.claude`, where the agent runs.
        Relative,
    }
    let case = |name: &str, home: Home| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("its working directory");
        let other = dir.join("other");
        let absolute = dir.join("abs/.claude");
        let runs_in = std::fs::canonicalize(&work).expect("its working directory");
        let opts = drive(&dir);
        let home_env = crate::harness::accounts::HOME_ENV;
        let (env, claude_dir): (Vec<(&str, PathBuf)>, PathBuf) = match home {
            Home::Other => (vec![("HOME", other.clone())], other.join(".claude")),
            Home::Users => (vec![("HOME", opts.home.clone())], opts.home.join(".claude")),
            Home::Absolute => (
                vec![("HOME", other.clone()), (home_env, absolute.clone())],
                absolute.clone(),
            ),
            Home::Relative => (
                vec![(home_env, PathBuf::from("rel/.claude"))],
                runs_in.join("rel/.claude"),
            ),
        };
        let env: Vec<(&str, &Path)> = env.iter().map(|(k, v)| (*k, v.as_path())).collect();
        let mut agent = tty_agent(&env, Some(&work));
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..opts
        };
        let start = kernel_start(pid).expect("lstart");
        register_running_in(&claude_dir, pid, &start, &runs_in);
        let snap = crate::harness::relaunch::snapshot_with(
            &opts,
            &Script::new(std::process::id(), usize::MAX, None),
        );
        let _ = agent.kill();
        let _ = agent.wait();
        let mut snap = snap.unwrap_or_else(|why| panic!("{name}: no snapshot: {why}"));
        assert_eq!(
            snap.claude_dir.as_deref(),
            Some(claude_dir.as_path()),
            "{name}: where its records are"
        );
        snap.dialect = Some(Dialect::Zsh);
        snap.argv = vec![snap.program.to_string_lossy().into_owned()];
        (snap, opts, dir, claude_dir, start)
    };
    let promised = |home: &Path, snap: &Snapshot| {
        (
            exit_line(home, snap).is_some(),
            resumes_on_exit(home, snap),
            carries_restart(home, snap),
        )
    };
    for (name, home) in [
        ("promise-users-home", Home::Users),
        ("promise-absolute-home", Home::Absolute),
        ("promise-other-home", Home::Other),
    ] {
        let (snap, opts, dir, ..) = case(name, home);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(promised(&opts.home, &snap), (true, true, true), "{name}");
    }
    let (mut snap, opts, dir, claude_dir, start) = case("promise-relative-home", Home::Relative);
    assert_eq!(
        promised(&opts.home, &snap),
        (true, true, true),
        "relative, kept"
    );
    let elsewhere = dir.join("elsewhere");
    register_running_in(&claude_dir, snap.pid, &start, &elsewhere);
    assert!(super::super::relaunch::follow(&opts.home, &mut snap));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        promised(&opts.home, &snap),
        (false, false, false),
        "relative, its record moved to {}",
        snap.cwd
    );
}

/// [`Records::at`] for an agent's own `HOME`.
#[test]
fn an_agents_own_home_keeps_its_records_and_its_profile() {
    let user = Path::new("/Users//someone");
    let other = "/Users//someone/other";
    let records = Records::at(
        user,
        HomeVars {
            config: None,
            home: Some(other),
        },
        || None,
    );
    assert_eq!(
        records.dir(),
        Some(Path::new("/Users//someone/other/.claude"))
    );
    assert_eq!(
        records.profile(user),
        Some(PathBuf::from("/Users//someone/other/.claude.json"))
    );
    let mut st = St::default();
    assert!(st.keep_home(&records, user));
    assert_eq!(st.home.as_deref(), Some("/Users//someone/other/.claude"));
    assert_eq!(
        st.home_profile.as_deref(),
        Some("/Users//someone/other/.claude.json")
    );
    assert_eq!(st.agent_records(user), records, "read back whole");
    let empty = Records::at(
        user,
        HomeVars {
            config: Some(""),
            home: Some(other),
        },
        || Some(PathBuf::from("/work")),
    );
    assert_eq!(empty.dir(), Some(Path::new("/work")));
    assert_eq!(
        empty.profile(user),
        Some(PathBuf::from("/Users//someone/other/.claude.json"))
    );
    for home in [Some("/Users//someone"), None] {
        let own = Records::at(user, HomeVars { config: None, home }, || None);
        assert_eq!(own.dir(), Some(Path::new("/Users//someone/.claude")));
        assert_eq!(own.home_word(user), None, "{home:?}");
        assert_eq!(
            own.profile(user),
            Some(PathBuf::from("/Users//someone/.claude.json"))
        );
    }
}

/// The user's own `~/.claude` named by `CLAUDE_CONFIG_DIR` keeps its profile inside it
/// ([`Records::at`], [`profile_apart`]).
#[test]
fn the_users_own_claude_home_named_explicitly_keeps_its_profile_inside_it() {
    let user = Path::new("/Users//someone");
    let named = Records::at(
        user,
        HomeVars::config(Some("/Users//someone/.claude")),
        || None,
    );
    assert_eq!(named.dir(), Some(Path::new("/Users//someone/.claude")));
    assert_eq!(
        named.profile(user),
        Some(PathBuf::from("/Users//someone/.claude/.claude.json"))
    );
    let mut st = St::default();
    assert!(st.keep_home(&named, user));
    assert_eq!(st.home, None, "the directory is the user's own");
    assert_eq!(
        st.home_profile.as_deref(),
        Some("/Users//someone/.claude/.claude.json")
    );
    assert_eq!(st.agent_records(user), named, "read back whole");
    let unset = Records::at(user, HomeVars::config(None), || None);
    assert_eq!(unset.dir(), named.dir());
    assert_eq!(
        unset.profile(user),
        Some(PathBuf::from("/Users//someone/.claude.json"))
    );
    let mut st = St::default();
    assert!(!st.keep_home(&unset, user), "nothing kept apart");
    assert_eq!((st.home, st.home_profile), (None, None));
}

/// Every reader finds a relative home's records where its agent filed them at start.
#[cfg(unix)]
#[test]
fn every_reader_finds_a_relative_homes_records_where_its_agent_filed_them() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool| {
        let dir = scratch(name);
        let work = dir.join("work");
        let wt = work.join("wt");
        std::fs::create_dir_all(&wt).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let claude_dir = started.join(RELATIVE);
        let runs_in = if moved { &wt } else { &work };
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(runs_in),
        );
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
        register_as(&claude_dir, pid, SESSION, "unknown");
        file_transcript_in(&claude_dir, "-work");
        let snap = crate::harness::relaunch::snapshot_with(
            &opts,
            &Script::new(std::process::id(), usize::MAX, None),
        );
        let due_now = due(&opts);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let named = snap
            .as_ref()
            .map(|snap| (snap.session.clone(), snap.claude_dir.clone()));
        assert_eq!(
            (named, due_now),
            (Ok((Some(SESSION.to_string()), Some(claude_dir))), Due::Yes),
            "{name}: the snapshot names its conversation where it filed it, the sweep finds it"
        );
    };
    case("records-where-filed-unmoved", false);
    case("records-where-filed-moved", true);
}

/// Live upgrade discovers an agent running under a custom `CLAUDE_CONFIG_DIR` via the `who` roster
/// and process env.
#[cfg(unix)]
#[test]
fn the_live_upgrade_finds_an_agent_under_another_claude_home() {
    /// The agent's Claude home.
    #[derive(Clone, Copy, PartialEq)]
    enum Home {
        /// The user's own (no `CLAUDE_CONFIG_DIR`).
        Users,
        /// `CLAUDE_CONFIG_DIR=<dir>`.
        Absolute,
    }
    let case = |name: &str, home: Home, users_records: bool| {
        let dir = scratch(name);
        let claude_dir = dir.join("work home/.claude");
        let env: Vec<(&str, &Path)> = match home {
            Home::Users => Vec::new(),
            Home::Absolute => vec![(crate::harness::accounts::HOME_ENV, claude_dir.as_path())],
        };
        let mut agent = tty_agent(&env, Some(&dir));
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let users = opts.home.join(".claude");
        if users_records {
            std::fs::create_dir_all(users.join("sessions")).expect("the user's sessions");
        }
        register_as(
            if home == Home::Users {
                &users
            } else {
                &claude_dir
            },
            pid,
            SESSION,
            "unknown",
        );
        let due_now = due(&opts);
        let r = step(&Opts {
            dry_run: true,
            ..opts.clone()
        });
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(due_now, Due::Yes, "{name}: the tab's agent is due");
        assert_eq!(
            (r.tab.as_str(), r.session.as_str(), r.pid),
            (TAB, SESSION, pid),
            "{name}: the step's pass visits it: {r:?}"
        );
    };
    case("sweep-users-home", Home::Users, true);
    case("sweep-own-home", Home::Absolute, true);
    case("sweep-own-home-only", Home::Absolute, false);
}

/// The upgrade view shows a pending upgrade only while a live process holds its conversation
/// (`Row::standing`).
#[cfg(unix)]
#[test]
fn the_upgrade_view_counts_an_agent_under_another_claude_home() {
    let case = |name: &str, own_home: bool| {
        let dir = scratch(name);
        let opts = drive(&dir);
        let users = opts.home.join(".claude");
        let claude_dir = if own_home {
            dir.join("work home/.claude")
        } else {
            users.clone()
        };
        std::fs::create_dir_all(users.join("sessions")).expect("the user's sessions");
        let mut agent = parked();
        if own_home {
            agent.env(crate::harness::accounts::HOME_ENV, &claude_dir);
        }
        let mut agent = agent.spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_in(&claude_dir, agent.id(), SESSION);
        let now = now_s();
        let st = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            salt: now,
            pending_since: now,
            proc_start: squash(&sf.proc_start),
            home: own_home.then(|| claude_dir.to_string_lossy().into_owned()),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        save(&opts, SESSION, &st);
        let (rows, whole) = status_rows(&opts, false);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(whole, "{name}: the holders read whole");
        assert!(
            rows.iter()
                .any(|row| row.session == SESSION && row.tab == TAB),
            "{name}: shown and counted: {rows:?}"
        );
    };
    case("view-users-home", false);
    case("view-own-home", true);
}

/// [`Homes`]: the Claude homes one pass reads besides the user's, off each tab leader's exec
/// environment.
#[cfg(unix)]
#[test]
fn a_pass_reads_every_claude_home_its_tabs_agents_run_under() {
    let dir = scratch("homes");
    let opts = drive(&dir);
    let users = opts.home.join(".claude");
    std::fs::create_dir_all(&users).expect("the user's home");
    let link = dir.join("users-link");
    std::os::unix::fs::symlink(&users, &link).expect("a link to the user's home");
    let link = link.to_string_lossy().into_owned();
    let tab = |sid: &str, group: Option<i64>| LiveTab {
        sid: sid.to_string(),
        fgpgid: group,
    };
    let me = std::process::id();
    struct Reaped(aterm_uds::spawnfd::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Reaped(parked_bare().spawn().expect("a second live leader"));
    let other = child.0.id();
    let gone = dead_pid();
    let tabs = [
        tab("s-a", Some(1)),
        tab("s-b", Some(2)),
        tab("s-c", Some(3)),
        tab("s-d", Some(4)),
        tab("s-e", Some(i64::from(me))),
        tab("s-f", Some(i64::from(other))),
        tab("s-g", Some(7)),
        tab("s-h", None),
        tab("s-i", Some(1)),
        tab("s-j", Some(i64::from(gone))),
    ];
    let env = |home: Option<&str>| {
        Some(atpkg::caller_shell::ProcArgs {
            env: home
                .map(|h| vec![format!("CLAUDE_CONFIG_DIR={h}")])
                .unwrap_or_default(),
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    let read_args = |pid: u32| match pid {
        1 => env(Some("/work/a/.claude")),
        2 => env(Some("rel/../rel/.claude")),
        3 => env(None),
        4 => env(Some("")),
        p if p == other => env(Some("rel/.claude")),
        7 => env(Some(&link)),
        _ => None,
    };
    let read_cwd = |pid: u32| match pid {
        2 => Some(PathBuf::from("/work/b")),
        4 => Some(PathBuf::from("/work/d")),
        _ => None,
    };
    let homes = Homes::read(&opts, Some(&tabs), read_args, read_cwd);
    assert_eq!(
        homes.dirs(),
        [
            PathBuf::from("/work/a/.claude"),
            PathBuf::from("/work/b/rel/.claude"),
            PathBuf::from("/work/d"),
        ]
    );
    assert_eq!(homes.unnamed, ["s-e", "s-f"]);
    assert!(homes.unnamed("s-f") && !homes.unnamed("s-a"));
    let scoped = |sid: &str| Opts {
        only_sid: Some(sid.to_string()),
        ..opts.clone()
    };
    let homes = Homes::read(&scoped("s-b"), Some(&tabs), read_args, read_cwd);
    assert_eq!(homes.dirs(), [PathBuf::from("/work/b/rel/.claude")]);
    assert!(homes.unnamed.is_empty());
    let homes = Homes::read(&scoped("s-c"), Some(&tabs), read_args, read_cwd);
    assert_eq!(homes, Homes::default(), "the user's own is read anyway");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    for (session, home) in [
        (
            "aaaaaaaa-0000-0000-0000-000000000001",
            Some("/work/a/.claude"),
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000002",
            Some("/work/a/.claude"),
        ),
        ("aaaaaaaa-0000-0000-0000-000000000003", None),
    ] {
        let st = St {
            home: home.map(str::to_string),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let homes = Homes::read(
        &opts,
        None,
        |_| panic!("no process is read without a roster"),
        |_| panic!("no process is read without a roster"),
    );
    assert_eq!(homes.dirs(), [PathBuf::from("/work/a/.claude")]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`Homes::roster`]: every home's records, each stamped with where it was read
/// ([`SessionFile::claude_dir`]).
#[test]
fn a_roster_reads_every_home_whole_or_not_at_all() {
    let dir = scratch("roster");
    let me = std::process::id();
    let a = dir.join("a/.claude");
    register_as(&a, me, SESSION, "1.0.0");
    let fresh = dir.join("fresh/.claude");
    std::fs::create_dir_all(&fresh).expect("a home with no records yet");
    let broken = dir.join("broken/.claude");
    std::fs::create_dir_all(&broken).expect("a home");
    std::fs::write(broken.join("sessions"), "").expect("records that cannot be listed");
    let homes = |dirs: &[&PathBuf]| Homes {
        dirs: dirs
            .iter()
            .map(|d| Records::of(&dir.join("home"), d.to_str()))
            .collect(),
        unnamed: Vec::new(),
    };
    let got = homes(&[&a, &fresh])
        .roster(Some(Vec::new()))
        .expect("read whole");
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].pid, got[0].claude_dir.as_deref()),
        (me, Some(a.as_path()))
    );
    assert_eq!(homes(&[&a, &broken]).roster(Some(Vec::new())), None);
    assert_eq!(homes(&[&a]).roster(None), None, "the user's own unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`home_carried`]: a relative Claude home is carried only where the relaunch line's `cd` keeps it
/// resolving to the agent's records.
#[test]
fn a_relative_claude_home_the_line_would_move_is_refused() {
    let user = Path::new("/home");
    let records = Records::of(user, Some("/work/b/rel/.claude"));
    let refused = |r: Result<Option<String>, NoPlan>| matches!(r, Err(NoPlan::Refused { why, what, .. }) if why == "line:home" && what == "home");
    assert!(home_carried(user, None, "/work/c", &Records::of(user, None)).is_ok());
    assert!(home_carried(user, Some("/elsewhere/.claude"), "/work/c", &records).is_ok());
    assert!(home_carried(user, Some("rel/.claude"), "/work/b", &records).is_ok());
    assert!(home_carried(user, Some("rel/.claude"), "", &records).is_ok());
    assert!(refused(home_carried(
        user,
        Some("rel/.claude"),
        "/work/c",
        &records
    )));
    let other = Records::at(
        user,
        HomeVars {
            config: None,
            home: Some("/other"),
        },
        || None,
    );
    assert!(matches!(
        home_carried(user, None, "/work/c", &other),
        Ok(Some(homedir)) if homedir == "/other"
    ));
    assert!(matches!(
        home_carried(user, None, "/work/c", &Records::of(user, None)),
        Ok(None)
    ));
    assert!(refused(home_carried(
        user,
        None,
        "/work/c",
        &Records::of(user, Some("/other/claude-home"))
    )));
}

/// The upgrade's record names the Claude home its agent runs under ([`St::home`]).
#[cfg(unix)]
#[test]
fn an_upgrades_record_names_the_claude_home_its_agent_runs_under() {
    for (name, own) in [("bind-own", true), ("bind-users", false)] {
        let dir = scratch(name);
        let opts = drive(&dir);
        let claude_dir = if own {
            dir.join("work home/.claude")
        } else {
            opts.home.join(".claude")
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_in(&claude_dir, agent.id(), SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let r = visit(&opts, &sf, &table(), &newer(), &Live);
        let saved = load(&opts, SESSION).expect("the record");
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r.session, SESSION, "{name}: visited: {r:?}");
        assert_eq!(
            saved.home,
            own.then(|| claude_dir.to_string_lossy().into_owned()),
            "{name}: {r:?}"
        );
        assert_eq!(saved.to_json().contains("\"home\""), own, "{name}");
        assert_eq!(
            saved.agent_records(&opts.home).dir(),
            Some(claude_dir.as_path()),
            "{name}: read back where its agent keeps its records"
        );
    }
}

/// An exit's relaunch saves the fallback origin it decides ([`after_exit`] into [`exit_fallback`]).
#[cfg(unix)]
#[test]
fn an_exit_relaunch_saves_the_fallback_origin_it_decides() {
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let hand = "claude-opus-5-5";
    let case = |name: &str, chose: Option<&str>, want: &str| {
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        a.snap.argv = ["/opt/claude/bin/claude", "--model", fallback]
            .iter()
            .map(|w| (*w).to_string())
            .collect();
        write_transcript(
            &a.opts.home,
            &[answer_at(chose.unwrap_or(fallback), "2026-09-25T00:00:00Z")],
        );
        if let Some(hand) = chose {
            chose_by_hand(&a.opts, hand);
        }
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        save(
            &a.opts,
            SESSION,
            &saved_restart(&model_restart(
                &Restart::Model {
                    to: fallback.to_string(),
                },
                None,
                origin.to_string(),
                None,
                None,
            )),
        );
        a.write_record();
        let left = a.look(|_| {});
        let r = after_exit(&a.opts, &a.snap, &left, true, None);
        let saved = load(&a.opts, SESSION).expect("state");
        assert_eq!(saved.cause, CAUSE_EXIT, "{name}: {r:?}");
        assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
        assert!(
            saved
                .line
                .contains(&format!("'--model' '{}'", chose.unwrap_or(fallback))),
            "{name}: {}",
            saved.line
        );
    };
    std::thread::scope(|scope| {
        let hand_case = scope.spawn(|| case("exit-saved-hand", Some(hand), hand));
        case("exit-saved-nobody", None, origin);
        hand_case.join().expect("exit-saved-hand");
    });
}

/// A relaunch left in flight finds its process under the Claude home its line carried
/// ([`St::home`]).
#[cfg(unix)]
#[test]
fn a_relaunch_in_flight_finds_its_process_under_the_home_its_line_carried() {
    let dir = scratch("adopt-home");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me) = (agent.id(), std::process::id());
    wait_exec(pid);
    let home = dir.join("work home/.claude");
    let sf = register_in(&home, pid, SESSION);
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: CAUSE_HOST.to_string(),
        home: Some(home.to_string_lossy().into_owned()),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let r = {
        let mut c = connect(&opts, TAB).expect("control connection");
        await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live)
    };
    assert_eq!((r.step.as_str(), r.pid), ("adopted", pid), "{r:?}");
    let carried = resume(&opts);
    assert_eq!(
        (carried.step.as_str(), carried.pid),
        ("wait:tab-ownership-changed", pid),
        "{carried:?}"
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch handed back (`hand_back`).
#[cfg(unix)]
#[test]
fn a_relaunch_handed_to_its_loop_ends_at_adoption_and_is_owed_to_its_tab() {
    let dir = scratch("hand-back");
    let (sock, _asked) = instance(&dir);
    let mut opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me, dead) = (agent.id(), std::process::id(), dead_pid());
    wait_exec(pid);
    register(&opts.home, pid, SESSION);
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead,
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: super::super::relaunch::CAUSE_EXIT.to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let mut c = connect(&opts, TAB).expect("control connection");
    let sf = register(&opts.home, pid, SESSION);
    let r = await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", pid));
    assert!(
        matches!(st.phase, Phase::Relaunched { .. }),
        "still in flight"
    );
    let adopted = load(&opts, SESSION).expect("state");
    assert_eq!((adopted.relaunched_pid, adopted.resumed_pid), (pid, 0));
    assert!(owed(&opts), "owed to its tab");
    let other = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert!(!owed(&other), "another tab owes nothing");
    opts.hand_back = false;
    let sf = register(&opts.home, pid, SESSION);
    let carried = await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live);
    assert_ne!(carried.step, "adopted", "{carried:?}");
    drop(c);
    st.phase = Phase::Relaunched { at_s: now_s() };
    save(&opts, SESSION, &st);
    assert_eq!(resume(&opts).step, carried.step);
    let _ = agent.kill();
    let _ = agent.wait();
    let confirming = St {
        phase: Phase::Done,
        confirm_by: now_s() + 60,
        resumed_on: "9.9.9".to_string(),
        resumed_pid: pid,
        ..st.clone()
    };
    save(&opts, SESSION, &confirming);
    assert!(owed(&opts), "its model is owed");
    let r = resume(&opts);
    assert_eq!(r.step, "done:model-unconfirmed", "{r:?}");
    assert!(!owed(&opts), "read: nothing more");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch of a task done under land is carried on with nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_of_a_task_done_under_land_is_carried_on_with_nothing() {
    let dir = scratch("said-done");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me, dead) = (agent.id(), std::process::id(), dead_pid());
    wait_exec(pid);
    register(&opts.home, pid, SESSION);
    let in_flight = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead,
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: CAUSE_HOST.to_string(),
        ..St::default()
    };
    let typing = |log: &crate::harness::FakeInstanceLog| -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| {
                let verb = l.split_whitespace().find(|w| !w.starts_with('@'));
                matches!(verb, Some("turn" | "send" | "paste" | "key"))
            })
            .cloned()
            .collect()
    };
    save(&opts, SESSION, &in_flight);
    let carried = resume(&opts);
    assert_eq!(carried.step, "wait:tab-ownership-changed", "{carried:?}");
    assert!(owed(&opts), "the carry-on is still owed");
    save(&opts, SESSION, &in_flight);
    asked.lock().unwrap().clear();
    let r = resume_as(&opts, true, None);
    assert_eq!((r.step.as_str(), r.pid), (CARRIED_DONE, pid), "{r:?}");
    let st = load(&opts, SESSION).expect("state");
    assert_eq!(st.phase, Phase::Done, "{st:?}");
    assert!(!st.confirming(), "no model is owed: {st:?}");
    assert_eq!(st.relaunched_pid, pid, "adopted first: {st:?}");
    assert!(!owed(&opts), "owed no more");
    assert!(
        st.outcome
            .starts_with("claude restarted on 9.9.9 · nothing typed"),
        "{st:?}"
    );
    assert_eq!(typing(&asked), Vec::<String>::new(), "nothing typed");
    let rows: Vec<String> = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("step").and_then(Value::as_str) == Some(CARRIED_DONE))
        .filter_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        .collect();
    assert_eq!(rows, std::slice::from_ref(&st.outcome), "said once");
    assert_eq!(resume_as(&opts, true, None).step, "current");
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// One move, one `adopted` row.
#[cfg(unix)]
#[test]
fn an_adoption_is_said_once_in_the_words_of_its_move() {
    let dir = scratch("adopted-once");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut first = parked().spawn().expect("spawn");
    let mut second = parked().spawn().expect("spawn");
    let mut third = parked().spawn().expect("spawn");
    let mut fourth = parked().spawn().expect("spawn");
    let (a, b, t, f, me) = (
        first.id(),
        second.id(),
        third.id(),
        fourth.id(),
        std::process::id(),
    );
    for pid in [a, b, t, f] {
        wait_exec(pid);
    }
    let mut c = connect(&opts, TAB).expect("control connection");
    let sessions = opts.home.join(".claude/sessions");
    let mut adopt = |pid: u32| {
        let _ = std::fs::remove_dir_all(&sessions);
        let sf = register(&opts.home, pid, SESSION);
        let mut st = load(&opts, SESSION).expect("state");
        await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live)
    };
    let quiet_move = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        quiet: true,
        ..St::default()
    };
    save(&opts, SESSION, &quiet_move);
    let r = adopt(a);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", a), "{r:?}");
    let adopted = load(&opts, SESSION).expect("state");
    assert_eq!((adopted.relaunched_pid, adopted.resumed_pid), (a, 0));
    let again = adopt(a);
    assert_eq!(
        (again.step.as_str(), again.pid),
        ("adopted", a),
        "{again:?}"
    );
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 1, "one move, one row: {rows:?}");
    assert_eq!(
        rows[0],
        Adopted::Quiet.detail(),
        "a quiet move promises nothing"
    );
    write_transcript(
        &opts.home,
        &[
            user_row("Reply with one short sentence saying hello."),
            turn_by("claude-haiku-4-5", "Hello."),
        ],
    );
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: a,
            quiet: false,
            ..load(&opts, SESSION).expect("state")
        },
    );
    let r = adopt(b);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", b), "{r:?}");
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 2, "a new move, its own row: {rows:?}");
    assert_eq!(rows[1], Adopted::Owed.detail(), "{rows:?}");
    std::fs::write(
        dir.join("t.status"),
        "OK schema=1 hold=0 hand=- supervisor=- autopilot=land",
    )
    .expect("status");
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: b,
            ..load(&opts, SESSION).expect("state")
        },
    );
    let r = adopt(t);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", t), "{r:?}");
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 3, "a new move, its own row: {rows:?}");
    assert_eq!(rows[2], Adopted::Land.detail(), "under Land: {rows:?}");
    write_transcript(
        &opts.home,
        &[
            harness_row("Relaunched: Claude Code exited without anyone asking"),
            turn_by(
                "claude-haiku-4-5",
                "What would you like me to help you with?",
            ),
        ],
    );
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: t,
            ..load(&opts, SESSION).expect("state")
        },
    );
    let r = adopt(f);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", f), "{r:?}");
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 4, "a new move, its own row: {rows:?}");
    assert_eq!(
        rows[3],
        Adopted::NothingOwed(NOBODY_ASKED).detail(),
        "taskless: {rows:?}"
    );
    drop(c);
    for p in [&mut first, &mut second, &mut third, &mut fourth] {
        let _ = p.kill();
        let _ = p.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The orphan pass leaves a restart whose relaunched process registered after the pass read its
/// list.
#[cfg(unix)]
#[test]
fn the_orphan_pass_leaves_a_restart_relaunched_since_its_list_was_read() {
    let dir = scratch("orphan-fresh-read");
    let opts = Opts {
        hand_back: true,
        ..drive(&dir)
    };
    let (old, me) = (dead_pid(), std::process::id());
    let listed = upgrade::parse_session_file(&format!(
        r#"{{"pid":{old},"sessionId":"{SESSION}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"gone","kind":"interactive","entrypoint":"cli"}}"#
    ))
    .expect("a session file");
    let record = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: old,
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        quiet: true,
        ..St::default()
    };
    save(&opts, SESSION, &record);
    let saved = load(&opts, SESSION).expect("the record");
    let mut new = parked().spawn().expect("spawn");
    wait_exec(new.id());
    register(&opts.home, new.id(), SESSION);
    let steps = || {
        orphans(&opts, std::slice::from_ref(&listed), None)
            .into_iter()
            .map(|r| r.step)
            .collect::<Vec<_>>()
    };
    assert_eq!(steps(), Vec::<String>::new(), "the relaunch holds it");
    assert_eq!(ledger_lines(&opts), 0, "nothing said");
    assert_eq!(load(&opts, SESSION), Some(saved), "nothing written");
    let _ = new.kill();
    let _ = new.wait();
    std::fs::remove_file(
        opts.home
            .join(format!(".claude/sessions/{}.json", new.id())),
    )
    .expect("remove the relaunch's session file");
    assert_eq!(steps(), ["wait:no-socket"], "an orphan is carried on");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An assistant row naming `model`, as Claude Code writes one, saying `text`.
#[cfg(unix)]
fn turn_by(model: &str, text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}},"version":"2.1.282"}}"#
    )
}

/// A user row saying `text`.
#[cfg(unix)]
fn user_row(text: &str) -> String {
    format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#)
}

/// Land's instruction as a [`user_row`], its words escaped for JSON.
#[cfg(unix)]
fn land_row() -> String {
    let json = aterm_json::to_string(&aterm_json::Value::from(
        crate::supervise::policy::turn_end::land_text(None),
    ))
    .expect("json");
    user_row(&json[1..json.len() - 1])
}

/// A clean defense, escaped for [`turn_by`]: what lands a task answering
/// Land's instruction ([`land_row`]; Autopilot Land).
#[cfg(unix)]
const LANDED_JSON: &str =
    "Landed and verified.\\nNothing left: it is merged into the remote main branch.";

/// [`user_row`], recorded `at` (Claude Code's `timestamp`).
#[cfg(unix)]
fn user_row_at(text: &str, at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"{text}"}}}}"#
    )
}

/// A restart relaunched from 2.1.281 onto 2.1.282 over a stand-in agent in [`TAB`].
#[cfg(unix)]
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    asked: crate::harness::FakeInstanceLog,
    agent: aterm_uds::spawnfd::Child,
    sf: SessionFile,
    path: PathBuf,
    st: St,
}

#[cfg(unix)]
impl Rig {
    fn new(name: &str, before: &[String]) -> Rig {
        Rig::with(name, before, Answers::default())
    }

    /// [`Rig::new`], the tab's instance answering as `answers` says.
    fn with(name: &str, before: &[String], answers: Answers) -> Rig {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
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
            None,
        )
    }

    /// Every turn typed into the tab.
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

/// What one carried-on restart left.
#[cfg(unix)]
struct Carried {
    report: Report,
    typed: Vec<String>,
    detail: String,
    st: St,
}

/// A restart carried on end to end ([`Rig`]): its transcript is `before` when
/// the restart records what the agent ran, and gains `after` since, and the
/// resumed answer is waited for at most `wait`.
#[cfg(unix)]
fn carried_on(name: &str, before: &[String], after: &[String], wait: Duration) -> Carried {
    let rig = Rig::new(name, before);
    rig.append(after);
    let mut st = rig.st.clone();
    let typed = rig.carry_on_from(&mut st, wait);
    assert_eq!(typed.step, "continued", "{typed:?}");
    let mut said = confirmations(&rig.opts);
    assert_eq!(said.len(), 1, "{said:?}");
    let report = said.remove(0);
    let detail = rig.details(&report.step);
    assert_eq!(detail.len(), 1, "the outcome is recorded once: {detail:?}");
    Carried {
        typed: rig.typed(),
        report,
        detail: detail[0].clone(),
        st: load(&rig.opts, SESSION).expect("state"),
    }
}

/// The continuation names the model the ready answer ran.
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
        changed.typed[0].contains(" claude-opus-5 "),
        "the model before is named: {:?}",
        changed.typed
    );
    assert!(
        changed.detail.contains("claude-opus-5 -> claude-opus-5-5")
            && !changed.detail.contains("fable"),
        "the first answer past the mark is the model after: {}",
        changed.detail
    );
    let before_same = [before[0].clone(), turn_by("claude-opus-5-5", marker)];
    let same = carried_on(
        "model-same",
        &before_same,
        &[turn_by("claude-opus-5-5", "Resumed.")],
        MODEL_WAIT,
    );
    assert_eq!(same.report.step, "done");
    assert!(same.typed[0].contains("claude-opus-5-5"));
    assert!(
        same.detail.contains("claude-opus-5-5") && !same.detail.contains("->"),
        "{}",
        same.detail
    );
}

/// A [`Kernel`] whose one scripted answer is whether the relaunch's shell has its terminal back.
#[cfg(unix)]
struct ShellHolds {
    looks: &'static [Option<bool>],
    next: std::cell::Cell<usize>,
}

#[cfg(unix)]
impl ShellHolds {
    fn new(looks: &'static [Option<bool>]) -> Self {
        Self {
            looks,
            next: std::cell::Cell::new(0),
        }
    }
}

#[cfg(unix)]
impl Kernel for ShellHolds {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        None
    }

    fn parent(&self, _: u32) -> Option<u32> {
        None
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }

    fn shell_holds_tab(&self, _: u32) -> Option<bool> {
        let at = self.next.get();
        self.next.set(at + 1);
        self.looks
            .get(at)
            .or_else(|| self.looks.last())
            .copied()
            .flatten()
    }
}

/// The shell held its terminal at every look: what it does before the line's
/// command starts (its hooks), or a command that came and went between looks.
#[cfg(unix)]
const HELD: &[Option<bool>] = &[Some(true)];

/// The line's command held the terminal at the first look (the agent ran),
/// and the shell has it back at every look since.
#[cfg(unix)]
const RAN: &[Option<bool>] = &[Some(false), Some(true)];

/// What the looks of one [`held_step`] read.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct Looks {
    holds: &'static [Option<bool>],
    block: Option<(u64, &'static str)>,
}

#[cfg(unix)]
impl Looks {
    /// Only the kernel's witness: no block was read.
    const fn holds(holds: &'static [Option<bool>]) -> Self {
        Self { holds, block: None }
    }
}

/// The line was typed into block 3, which is still the prompt it was typed
/// at: the shell has not run the line's command yet (its hooks).
#[cfg(unix)]
const BLOCK_NOT_RUN: &str = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":5,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// Block 3 is still running its command.
#[cfg(unix)]
const BLOCK_RUNS: &str = r#"OK {"blocks":[{"id":3,"state":"executing","exit":null,"prompt":10,"cmd":10,"cmdcol":5,"out":11,"end":null,"cwd":"","cmdline":"claude --resume x"}]}"#;

/// Block 3's command ended (its `133;D`).
#[cfg(unix)]
const BLOCK_ENDED: &str = r#"OK {"blocks":[{"id":3,"state":"complete","exit":1,"prompt":10,"cmd":10,"cmdcol":5,"out":11,"end":null,"cwd":"","cmdline":"claude --resume x"}]}"#;

/// The shell drew its next prompt: block 4 began after block 3.
#[cfg(unix)]
const BLOCK_NEXT: &str = r#"OK {"blocks":[{"id":4,"state":"entering","exit":null,"prompt":12,"cmd":12,"cmdcol":5,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// One [`await_new`] step over a relaunch of `cause` in flight since just
/// now, its looks reading `looks` ([`Looks`]), and the wait for the
/// relaunched process cut to `wait` (a step that must not end early is
/// waited out): the step, the record after it, its ledger rows.
#[cfg(unix)]
fn held_step(name: &str, cause: &str, looks: Looks, wait: Duration) -> (Report, St, usize) {
    let (r, st, rows, _) = held_step_shown(name, cause, looks, wait);
    (r, st, rows)
}

/// [`held_step`], and the owner's rows of the records after it ([`rows`]),
/// the step's record saved as its callers save it.
#[cfg(unix)]
fn held_step_shown(
    name: &str,
    cause: &str,
    looks: Looks,
    wait: Duration,
) -> (Report, St, usize, Vec<Row>) {
    let _waits = crate::harness::relaunch::virtual_waits();
    let dir = scratch(name);
    let (sock, _asked) = instance(&dir);
    if let Some((_, blocks)) = looks.block {
        std::fs::write(dir.join("t.blocks"), blocks).expect("blocks");
    }
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: std::process::id(),
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: cause.to_string(),
        exited_at: now_s(),
        line_block: looks.block.map(|(block, _)| block),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let mut c = connect(&opts, TAB).expect("control connection");
    let r = Report {
        pid: st.pid,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "9.9.9".to_string(),
        to: "9.9.9(same)".to_string(),
        step: String::new(),
    };
    let opts = Opts {
        timings: crate::harness::Timings {
            held: wait,
            ..opts.timings
        },
        ..opts
    };
    let r = await_new(
        &opts,
        r,
        &mut st,
        &mut c,
        SESSION,
        &ShellHolds::new(looks.holds),
    );
    let rows = ledger_lines(&opts);
    save(&opts, SESSION, &st);
    let shown = super::rows(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    (r, st, rows, shown)
}

/// A wait long enough that a shell read as holding the tab throughout would have read as ended
/// (`Timings::FAST.ended_at_once` and several looks more): the negative controls wait it out.
#[cfg(unix)]
const PAST_ENDED: Duration = Duration::from_millis(1_500);

/// The wait of a step that must end early: only a hang waits it out.
#[cfg(unix)]
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// The early verdict is never the live relaunch on exit's ([`CAUSE_EXIT`]).
#[cfg(unix)]
#[test]
fn a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab() {
    let (r, st, rows) = held_step("held-live", CAUSE_EXIT, Looks::holds(RAN), PAST_ENDED);
    assert_eq!(r.step, "wait:resume", "{r:?}");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert_eq!(rows, 0);
    let (r, st, rows) = held_step(
        "held-restored",
        CAUSE_HOST,
        Looks::holds(RAN),
        HANG_DETECTOR,
    );
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(rows, 1);
}

/// A shell that holds its tab is no proof the agent ended.
#[cfg(unix)]
#[test]
fn a_relaunch_whose_shell_has_not_run_its_line_yet_is_waited_on() {
    side_by_side(
        [
            ("hook-upgrade", "", Looks::holds(HELD)),
            ("hook-account", "account:work-2", Looks::holds(HELD)),
            (
                "hook-marked",
                "",
                Looks {
                    holds: HELD,
                    block: Some((3, BLOCK_NOT_RUN)),
                },
            ),
        ]
        .into_iter()
        .map(|(name, cause, looks)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let (r, st, rows) = held_step(name, cause, looks, PAST_ENDED);
                assert_eq!(r.step, "wait:resume", "{name}: {r:?}");
                assert!(
                    matches!(st.phase, Phase::Relaunched { .. }),
                    "{name}: {st:?}"
                );
                assert_eq!(rows, 0, "{name}");
            })
        })
        .collect(),
    );
}

/// Where the shell's integration marks the line's block ([`St::line_block`]).
#[cfg(unix)]
#[test]
fn the_shells_own_block_says_whether_the_line_ran() {
    side_by_side(
        [
            ("marked-ended", BLOCK_ENDED, HELD, true),
            ("marked-next", BLOCK_NEXT, HELD, true),
            ("marked-runs", BLOCK_RUNS, RAN, false),
        ]
        .into_iter()
        .map(|(name, blocks, holds, ended)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let looks = Looks {
                    holds,
                    block: Some((3, blocks)),
                };
                let wait = if ended { HANG_DETECTOR } else { PAST_ENDED };
                let (r, st, rows) = held_step(name, "", looks, wait);
                if ended {
                    assert_eq!(r.step, "failed:no-resume", "{name}: {r:?}");
                    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()), "{name}");
                    assert_eq!(rows, 1, "{name}");
                } else {
                    assert_eq!(r.step, "wait:resume", "{name}: {r:?}");
                    assert!(
                        matches!(st.phase, Phase::Relaunched { .. }),
                        "{name}: {st:?}"
                    );
                    assert_eq!(rows, 0, "{name}");
                }
            })
        })
        .collect(),
    );
}

/// The tab's newest block is the prompt the shell waits at (block 3), its
/// input start not marked yet (`133;B`): what a relaunch line is typed into.
#[cfg(unix)]
const BLOCK_AT_PROMPT: &str = r#"OK {"blocks":[{"id":3,"state":"prompt","exit":null,"prompt":10,"cmd":null,"cmdcol":null,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// A restart stamps the block its line is typed into ([`St::line_block`]) and judges the line by
/// it.
#[cfg(unix)]
#[test]
fn a_restarts_line_is_judged_by_the_block_it_was_typed_into() {
    let case = |name: &str, next_prompt: bool| {
        let dir = scratch(name);
        let mut shell = tty_zsh();
        let who = format!(
            "OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={}",
            shell.id()
        );
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                blocks: BLOCK_AT_PROMPT,
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let mut agent = parked_bare().spawn().expect("agent");
        let pid = agent.id();
        wait_exec(pid);
        register(&opts.home, pid, SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        write_transcript(
            &opts.home,
            &[answer_at("claude-opus-5", "2099-01-01T00:00:00Z")],
        );
        let snap = Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid,
            start: kernel_start(pid).expect("lstart"),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("1.0.0".to_string()),
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let (tx, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(agent.wait());
        });
        let next = next_prompt.then(|| {
            let (asked, blocks) = (asked.clone(), dir.join("t.blocks"));
            std::thread::spawn(move || {
                let typed = aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(5),
                    || {
                        asked
                            .lock()
                            .ok()?
                            .iter()
                            .any(|l| l.contains(" turn "))
                            .then_some(())
                    },
                );
                if typed.is_some() {
                    std::fs::write(blocks, BLOCK_NEXT).expect("the next prompt's block");
                }
            })
        });
        let wait = if next_prompt {
            HANG_DETECTOR
        } else {
            PAST_ENDED
        };
        let opts = Opts {
            timings: crate::harness::Timings {
                held: wait,
                ..opts.timings
            },
            ..opts
        };
        let restart = || {
            crate::harness::relaunch::restart_with(
                &opts,
                &Restart::Memory,
                Ok(snap.clone()),
                &Script::new(shell.id(), usize::MAX, None),
            )
        };
        let mut r = restart();
        assert!(
            ended.recv_timeout(HANG_DETECTOR).is_ok(),
            "{name}: the agent ended"
        );
        if turns(&asked) == 0 {
            r = restart();
        }
        let st = load(&opts, SESSION).expect("the restart's record");
        let typed = turns(&asked);
        if let Some(next) = next {
            let _ = next.join();
        }
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r, st, typed)
    };
    let case = &case;
    side_by_side(vec![
        Box::new(move || {
            let (r, st, typed) = case("block-next-prompt", true);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(st.line_block, Some(3), "{st:?}");
            assert_eq!(r.step, "failed:no-resume", "{r:?}");
            assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
        }),
        Box::new(move || {
            let (r, st, typed) = case("block-still-prompt", false);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(st.line_block, Some(3), "{st:?}");
            assert_eq!(r.step, "wait:resume", "{r:?}");
            assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
        }),
    ]);
}

/// An upgrade's relaunch that ended as it started stops at once (`failed:no-resume`) and reads as
/// `agent-down`.
#[cfg(unix)]
#[test]
fn an_upgrade_relaunch_that_ended_as_it_started_is_an_agent_down_at_once() {
    use super::super::relaunch::CAUSE_UPGRADE_FRESH;
    side_by_side(
        [
            ("upgrade-ended", "", format!("claude --resume {SESSION}")),
            ("fresh-ended", CAUSE_UPGRADE_FRESH, "claude".to_string()),
        ]
        .into_iter()
        .map(|(name, cause, command)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let (r, st, rows, shown) =
                    held_step_shown(name, cause, Looks::holds(RAN), HANG_DETECTOR);
                assert_eq!(r.step, "failed:no-resume", "{cause:?}: {r:?}");
                assert_eq!(
                    st.phase,
                    Phase::Failed("no-resume".to_string()),
                    "{cause:?}"
                );
                assert_eq!(rows, 1, "{cause:?}");
                let row = shown
                    .iter()
                    .find(|row| row.session == SESSION)
                    .expect("its row");
                match row.attention(now_s()) {
                    Attention::Interrupt(need) => {
                        assert_eq!(need.kind, "agent-down", "{cause:?}");
                        assert_eq!(need.command.as_deref(), Some(command.as_str()), "{cause:?}");
                    }
                    other => panic!("{cause:?}: {other:?}"),
                }
            })
        })
        .collect(),
    );
}

/// A [`Kernel`] over [`Script`] whose shell runs each relaunch line it is typed.
#[cfg(unix)]
struct EachLineEnds {
    script: Script,
    asked: crate::harness::FakeInstanceLog,
    seen: std::cell::Cell<usize>,
    ends: usize,
    image: Option<PathBuf>,
}

#[cfg(unix)]
impl Kernel for EachLineEnds {
    fn job(&self, pid: u32) -> Option<(Job, u32)> {
        self.script.job(pid)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.script.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        self.script.terminal(pid)
    }

    fn shell_holds_tab(&self, _: u32) -> Option<bool> {
        let typed = turns(&self.asked);
        if typed > self.seen.get() {
            self.seen.set(typed);
            return Some(false);
        }
        Some(typed <= self.ends)
    }

    fn image(&self, _: u32) -> Option<PathBuf> {
        self.image.clone()
    }
}

/// A move onto a build that fails only at a real start is taken back onto the build its agent ran.
#[cfg(unix)]
#[test]
fn a_move_onto_a_build_that_fails_only_at_a_real_start_is_taken_back_onto_the_build_it_ran() {
    const IMAGE: &str = "/opt/claude/versions/1.0.0";
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let run = |name: &str, image: &str, ends: usize, up: bool| {
        let mut shell = tty_zsh();
        let who = format!(
            "OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={}",
            shell.id()
        );
        let mut h = Parked::new(
            name,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
            St::default(),
            3600,
        );
        h.opts.hand_back = true;
        h.opts.timings.held = HANG_DETECTOR;
        h.opts.timings.shell_look = Duration::from_millis(10);
        h.opts.timings.ended_at_once = Duration::from_millis(20);
        transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
        let announced = St {
            seq_since_s: now_s() - 3_600,
            last_seq: 77,
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), announced.to_json()).expect("state");
        let ended = h.reaped();
        let k = EachLineEnds {
            script: Script::new(shell.id(), usize::MAX, Some(shell.id())),
            asked: h.asked.clone(),
            seen: std::cell::Cell::new(0),
            ends,
            image: Some(PathBuf::from(image)),
        };
        let (asked, home) = (h.asked.clone(), h.opts.home.clone());
        let up = up.then(|| {
            std::thread::spawn(move || {
                aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(5),
                    || (turns(&asked) > ends).then_some(()),
                )?;
                let agent = parked_bare().spawn().ok()?;
                wait_exec(agent.id());
                register(&home, agent.id(), SESSION);
                Some(agent)
            })
        });
        let r = visit(
            &h.opts,
            &h.sf,
            &[(shell.id(), 1, "zsh".into())],
            &newer(),
            &k,
        );
        assert!(
            ended.recv_timeout(HANG_DETECTOR).is_ok(),
            "{name}: the agent ended"
        );
        let mut back = up.and_then(|up| up.join().ok().flatten());
        let st = load(&h.opts, SESSION).expect("the record");
        let typed = h.typed();
        let failed = h.details("failed:no-resume");
        let shown = super::rows(&h.opts);
        if let Some(agent) = back.as_mut() {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        let _ = shell.kill();
        let _ = shell.wait();
        (r, st, typed, failed, shown, h)
    };
    let down = |shown: &[Row], name: &str| {
        let row = shown
            .iter()
            .find(|row| row.session == SESSION)
            .expect("its row");
        match row.attention(now_s()) {
            Attention::Interrupt(need) => assert_eq!(need.kind, "agent-down", "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    };
    let kept_is = |st: &St| {
        st.failed_start()
            .is_some_and(|f| f.to == "9.9.9" && f.source == "managed")
    };

    let (r, st, typed, failed, _, h) = run("back-up", IMAGE, 1, true);
    assert_eq!(
        typed.len(),
        2,
        "the move's line, then the line back: {typed:?} {r:?}"
    );
    assert!(typed[0].contains("/nonexistent/claude"), "{typed:?}");
    assert!(
        typed[1].contains(IMAGE)
            && typed[1].contains(SESSION)
            && !typed[1].contains("/nonexistent/"),
        "the same resume on the image it ran: {typed:?}"
    );
    assert_eq!(r.step, "adopted", "{r:?}");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert_eq!((st.source.as_str(), st.to.as_str()), ("same", "1.0.0"));
    assert!(st.back_line.is_empty(), "taken once: {st:?}");
    assert!(kept_is(&st), "the failure is kept: {st:?}");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("resumed on 1.0.0"), "{failed:?}");
    let note = build_start::load(&h.opts, "managed", "9.9.9").expect("the build's note");
    assert_eq!(note.failures, 1, "the build's failure is counted: {note:?}");
    let to = newer().managed.expect("the target");
    let (next, how) = St::retarget(
        Some(St {
            phase: Phase::Done,
            ..st
        }),
        &Version::parse("1.0.0").expect("the build it runs"),
        &to,
        None,
        Proc::default(),
        now_s(),
    );
    assert_eq!(how, Retarget::Rested, "{next:?}");
    assert_eq!(next.phase, Phase::Failed("no-resume".to_string()));
    drop(h);

    let (r, st, typed, failed, shown, h) = run("back-ends", IMAGE, 2, false);
    assert_eq!(typed.len(), 2, "no third line: {typed:?}");
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert!(kept_is(&st), "the failure is still the build's: {st:?}");
    assert_eq!(failed.len(), 2, "{failed:?}");
    down(&shown, "back-ends");
    drop(h);

    let (r, st, typed, failed, shown, h) = run("no-back", "/nonexistent/claude", 1, false);
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(st.source, "managed");
    assert!(kept_is(&st), "{st:?}");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(!failed[0].contains("resumed on"), "{failed:?}");
    down(&shown, "no-back");
    drop(h);
}

/// The kernel's own answer ([`Kernel::shell_holds_tab`]'s default, [`Live`]'s) on real processes.
#[cfg(unix)]
#[test]
fn the_kernel_reads_whether_a_shell_has_its_tab_back() {
    let mut zsh = tty_zsh();
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let mut plain = parked().spawn().expect("spawn");
    wait_exec(own.id());
    wait_exec(plain.id());
    let got = [
        Live.shell_holds_tab(zsh.id()),
        Live.shell_holds_tab(own.id()),
        Live.shell_holds_tab(plain.id()),
    ];
    let _ = zsh.kill();
    let _ = zsh.wait();
    for c in [&mut own, &mut plain] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(got, [Some(true), Some(false), Some(false)]);
    assert_eq!(Live.shell_holds_tab(0), None);
    assert_eq!(Live.shell_holds_tab(dead_pid()), None);
}

/// A resumed session that has not answered by the first sweep past the bound is said
/// `done:model-unconfirmed`, once.
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
    assert!(rig.typed()[0].contains("claude-opus-5-5"));
    assert_eq!(confirmations(&rig.opts), []);
    assert!(load(&rig.opts, SESSION).is_some_and(|st| st.confirming()));
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
    assert_eq!(rig.details("done:model-unconfirmed").len(), 1, "said once");
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "said once, never re-asked");
    assert!(!saved.confirming());
    assert_eq!(confirmations(&rig.opts), []);
    assert_eq!(rig.typed().len(), 1);
}

/// A restart an older aterm began took no mark.
#[cfg(unix)]
#[test]
fn a_restart_without_a_mark_is_unconfirmed_at_once() {
    let before = [
        ASKED.to_string(),
        turn_by("claude-opus-5-5", "ATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-no-mark", &before);
    rig.append(&[turn_by("claude-opus-5-5", "Resumed.")]);
    let mut st = St {
        mark: 0,
        ..rig.st.clone()
    };
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued");
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done:model-unconfirmed"]
    );
    assert!(!load(&rig.opts, SESSION).expect("state").confirming());
    assert_eq!(rig.typed().len(), 1);
}

/// A restart records the model its ready answer ran and where that read ended, from the tail that
/// proved the answer.
#[cfg(unix)]
#[test]
fn a_restart_records_the_model_its_ready_answer_ran_and_where_that_read_ended() {
    let dir = scratch("model-before");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let path = project.join(format!("{SESSION}.jsonl"));
    let body = [
        ASKED.to_string(),
        turn_by("claude-fable-5-1", "Earlier."),
        turn_by("claude-opus-5-5", &format!("Saved.\\n{marker}")),
        r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#.to_string(),
    ]
    .join("\n")
        + "\n";
    std::fs::write(&path, &body).expect("transcript");
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".into())];
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
    let mut args = atpkg::caller_shell::process_args(pid).expect("agent argv");
    args.argv
        .extend(["--model".to_string(), "claude-sonnet-5".to_string()]);
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
    assert!(saved.restart_look, "the restart's own word: {saved:?}");
    assert!(saved.held_past_go("would-restart"));
    assert!(!saved.held_past_go("would-announce"), "another act's read");
    assert_eq!(saved.model_before, "claude-opus-5-5");
    assert_eq!(saved.mark, body.len() as u64, "the read ended at the end");
    assert_eq!(saved.launch_model, "claude-sonnet-5", "the kept --model");
    assert!(
        matches!(saved.phase, Phase::Announced { .. }),
        "never signalled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The visit that types the continuation does not wait for the answer.
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
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(rig.typed().len(), 1);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(r.is_act(), "the continuation typed is said");
    assert_eq!(rig.details("continued").len(), 1);
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "never in flight again");
    assert!(saved.confirming());
    assert_eq!(confirmations(&rig.opts), []);
    let old_build = turn_by("claude-opus-5", "One more.").replace("2.1.282", "2.1.281");
    rig.append(&[old_build, turn_by("claude-opus-5-5", "Resumed.")]);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done"]
    );
    let done = rig.details("done");
    assert!(
        done.len() == 1 && done[0].ends_with("claude-opus-5-5"),
        "the model after, not the old build's row: {done:?}"
    );
    assert!(!load(&rig.opts, SESSION).expect("state").confirming());
    assert_eq!(confirmations(&rig.opts), [], "said once");
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

/// One outcome per restart.
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
    let pending = load(&opts, SESSION).expect("saved");
    let r = visit(&opts, &sf, &[], &newer(), &Script::new(1, usize::MAX, None));
    assert_eq!(r.step, "wait:confirming", "{r:?}");
    assert_eq!(
        load(&opts, SESSION),
        Some(pending),
        "the owed outcome is kept"
    );
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

/// A refusal with a model due (`models::model_moves_now` answers `build-restart` every pass) is
/// said and ledgered once.
#[cfg(unix)]
#[test]
fn a_refusal_with_a_model_due_is_said_once_not_every_pass() {
    let dir = scratch("job-model");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(now)
        ),
    )
    .expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    let mread = model_read(&opts, &sf, None, &mctx);
    assert!(
        matches!(&mread.verdict, ModelVerdict::Due { to, .. } if to == "claude-opus-5-5"),
        "the move must be due for this test to mean anything"
    );
    assert!(!mread.cold, "the transcript must read WARM");
    assert_eq!(
        models::model_moves_now(mread.cold, true, mread.due_for_s),
        Some("build-restart")
    );
    let files = session_files(&opts.home);
    let pass = || {
        visit_models(
            &opts,
            &sf,
            files.as_deref(),
            &table(),
            &newer(),
            &Live,
            None,
            None,
            &mctx,
        )
    };
    let first = pass();
    let again = pass();
    let third = pass();
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(first.step, "refused:not-a-shell-job", "{first:?}");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("not-a-shell-job".to_string()))
    );
    for later in [&again, &third] {
        assert!(
            !later.is_act(),
            "a final refusal was said again on a later pass: {later:?}"
        );
    }
    assert_eq!(ledger_lines(&opts), 1, "one refusal, one ledger row");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ride is re-judged at every visit.
#[cfg(unix)]
#[test]
fn a_barred_ride_is_dropped_once_and_the_round_goes_on_without_it() {
    let dir = scratch("ride-barred");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let now = now_s();
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(i64::try_from(now).expect("now"))
        ),
    )
    .expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    let salt = now - 500;
    let announced = |to: &str| St {
        phase: Phase::Announced { at_s: now, asks: 1 },
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        model_list: "claude-opus-5-5".to_string(),
        marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        salt,
        ..St::default()
    };
    let current = Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let files = session_files(&opts.home);
    let kernel = Script::new(1, 1_000, None);
    let pass = |targets: &Targets| {
        visit_models(
            &opts,
            &sf,
            files.as_deref(),
            &table(),
            targets,
            &kernel,
            None,
            None,
            &mctx,
        )
    };
    let dropped = || -> Vec<String> {
        std::fs::read_to_string(ledger_path(&opts))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"step\":\"model-ride-dropped\""))
            .map(str::to_string)
            .collect()
    };

    save(&opts, SESSION, &announced("9.9.9"));
    let _ = pass(&newer());
    assert!(dropped().is_empty(), "{:?}", dropped());
    let kept = load(&opts, SESSION).expect("state");
    assert_eq!(kept.model_list, "claude-opus-5-5");

    let failed = ModelRecord {
        failed: vec!["claude-opus-5-5".to_string()],
        ..ModelRecord::default()
    };
    save_model_record(&opts, SESSION, &failed);
    save(&opts, SESSION, &announced("9.9.9"));
    let _ = pass(&newer());
    let rows = dropped();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("model:claude-opus-5-5") && rows[0].contains("model-failed-before"),
        "{rows:?}"
    );
    let kept = load(&opts, SESSION).expect("state");
    assert!(kept.model_list.is_empty(), "the round forgot the model");
    assert!(
        matches!(kept.phase, Phase::Announced { .. }),
        "{:?}",
        kept.phase
    );
    assert_eq!(
        (kept.to.as_str(), kept.salt),
        ("9.9.9", salt),
        "the round kept"
    );
    assert_eq!(kept.marker, "ATERM-UPGRADE-READY-0badf00d");
    let _ = pass(&newer());
    assert_eq!(dropped().len(), 1, "said once");

    save(&opts, SESSION, &announced("1.0.0"));
    let _ = pass(&current);
    let rows = dropped();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let released = load(&opts, SESSION).expect("state");
    assert_eq!(released.phase, Phase::Pending);
    assert!(released.model_list.is_empty() && released.marker.is_empty());
    assert_eq!(
        released.release, "model-dropped",
        "the agent is owed its release"
    );

    save_model_record(&opts, SESSION, &ModelRecord::default());
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(i64::try_from(now).expect("now"))
        ),
    )
    .expect("transcript");
    let fable = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec![
            "claude-opus-5-5".to_string(),
            "claude-fable-5-1".to_string(),
        ],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    save(&opts, SESSION, &announced("9.9.9"));
    let _ = visit_models(
        &opts,
        &sf,
        files.as_deref(),
        &table(),
        &newer(),
        &kernel,
        None,
        None,
        &fable,
    );
    let rows = dropped();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows[2].contains("model-off-list"), "{rows:?}");

    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The planned line carries the usage tap where its gate holds.
#[test]
fn the_planned_relaunch_line_carries_the_usage_tap_only_where_its_gate_holds() {
    use super::super::tap;
    let dir = scratch("line-tap");
    let mut opts = drive(&dir);
    let project = dir.join("proj");
    std::fs::create_dir_all(project.join(".claude")).expect("project");
    std::fs::create_dir_all(opts.home.join(".claude")).expect("user dir");
    let cwd = project.to_str().expect("utf-8").to_owned();
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let value = tap::settings_value(exe).expect("an admissible path");
    let planned_under = |opts: &Opts, argv: &[String], stored: bool, homedir: Option<&str>| {
        let launch = crate::harness::relaunch::Launch {
            stored_login: stored,
            billing: crate::harness::relaunch::ModelBilling::Keep,
            session: SESSION,
            resume: true,
            cwd: &cwd,
            agent: 0,
            argv,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            login: None,
            home: None,
            homedir,
        };
        crate::harness::relaunch::plan(opts, &launch, 999_999, &[(999_999, 1, "zsh".into())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let planned =
        |opts: &Opts, argv: &[String], stored: bool| planned_under(opts, argv, stored, None);
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let plain = planned(&opts, &argv, true);
    assert!(
        plain.contains(&format!(
            "'/opt/claude' '--verbose' '--resume' '{SESSION}' )"
        )),
        "{plain}"
    );
    assert!(!plain.contains("--settings"), "{plain}");
    opts.usage_tap = Some(tap::Armed {
        exe: exe.to_path_buf(),
        lone_login: None,
        listed: Vec::new(),
    });
    let pair = format!("'--settings' '{value}'");
    let tapped = planned(&opts, &argv, true);
    assert!(
        tapped.contains(&format!(
            "'/opt/claude' '--verbose' {pair} '--resume' '{SESSION}' )"
        )),
        "{tapped}"
    );
    assert_eq!(tapped.replacen(&format!(" {pair}"), "", 1), plain);
    assert_eq!(planned(&opts, &argv, false), plain);
    let other = planned_under(&opts, &argv, true, Some("/Users//other"));
    assert!(other.contains("HOME=/Users//other"), "{other}");
    assert!(!other.contains("harness tap"), "{other}");
    let renewed = "/tmp/aterm-state/logins/home-a.1";
    let on_login_of = |lone: Option<&str>, listed: &[&str]| {
        let opts = Opts {
            usage_tap: Some(tap::Armed {
                exe: exe.to_path_buf(),
                lone_login: lone.map(str::to_owned),
                listed: listed.iter().map(|l| (*l).to_owned()).collect(),
            }),
            ..opts.clone()
        };
        let launch = crate::harness::relaunch::Launch {
            login: Some(renewed),
            stored_login: true,
            billing: crate::harness::relaunch::ModelBilling::Keep,
            session: SESSION,
            resume: true,
            cwd: &cwd,
            agent: 0,
            argv: &argv,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            home: None,
            homedir: None,
        };
        crate::harness::relaunch::plan(&opts, &launch, 999_999, &[(999_999, 1, "zsh".into())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let on_login = |lone: Option<&str>| on_login_of(lone, &[]);
    let renewed_line = on_login(Some(renewed));
    assert!(renewed_line.contains(renewed), "{renewed_line}");
    assert!(renewed_line.contains(&pair), "{renewed_line}");
    for lone in [None, Some("/tmp/aterm-state/logins/home-a.2")] {
        let line = on_login(lone);
        assert!(line.contains(renewed), "{line}");
        assert!(!line.contains("--settings"), "{lone:?}: {line}");
    }
    let work = "/tmp/aterm-state/logins/work";
    let listed = on_login_of(None, &[work, renewed]);
    assert!(
        listed.contains(&pair),
        "no lone login, a login the roster names: tapped as the machine's own: {listed}"
    );
    let unlisted = on_login_of(None, &[work]);
    assert!(
        !unlisted.contains("--settings"),
        "a login the roster does not name: {unlisted}"
    );
    let beside_lone = on_login_of(Some("/tmp/aterm-state/logins/home-a.2"), &[renewed]);
    assert!(
        !beside_lone.contains("--settings"),
        "a lone login is the only one: {beside_lone}"
    );
    let stale = tap::settings_value(Path::new("/Users//example/old/aterm")).expect("a path");
    let rerun: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), stale.clone()])
        .collect();
    let line = planned(&opts, &rerun, true);
    assert_eq!(line, tapped, "the stale tap is replaced");
    assert!(!line.contains("/Users//example/old"), "{line}");
    let status = r#"{"statusLine":{"type":"command","command":"echo mine"}}"#;
    let helper = r#"{"apiKeyHelper":"/bin/echo"}"#;
    let credential = r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#;
    for file in [
        opts.home.join(".claude/settings.json"),
        opts.home.join(".claude/cowork_settings.json"),
        project.join(".claude/settings.json"),
        project.join(".claude/settings.local.json"),
    ] {
        for text in [status, helper, credential] {
            std::fs::write(&file, text).expect("write");
            if file == opts.home.join(".claude/settings.json") && text == status {
                let carried =
                    tap::settings_value_chained(exe, Some("echo mine")).expect("a carried value");
                assert_eq!(
                    planned(&opts, &argv, true),
                    tapped.replacen(&value, &carried, 1),
                    "the owner's own statusLine in the user's settings.json is carried (R-12)"
                );
                assert!(
                    super::tap_settings_admit(&opts.home, &argv, &cwd),
                    "and the footer's reason judges it alike"
                );
                continue;
            }
            assert_eq!(
                planned(&opts, &argv, true),
                plain,
                "{}: {text}",
                file.display()
            );
            assert!(
                !super::tap_settings_admit(&opts.home, &argv, &cwd),
                "the footer's reason judges the line's documents alike: {}: {text}",
                file.display()
            );
        }
        std::fs::remove_file(&file).expect("remove");
    }
    assert_eq!(
        planned(&opts, &argv, true),
        tapped,
        "every document quiet again"
    );
    assert!(
        super::tap_settings_admit(&opts.home, &argv, &cwd),
        "and so does the footer's reason"
    );
    let managed = dir.join("managed");
    std::fs::create_dir_all(&managed).expect("managed");
    std::fs::write(managed.join("managed-settings.json"), status).expect("write");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = Some(managed.clone()));
    assert_eq!(planned(&opts, &argv, true), plain, "a managed statusLine");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = None);
    let prefs = dir.join("prefs");
    std::fs::create_dir_all(&prefs).expect("prefs");
    std::fs::write(prefs.join("com.anthropic.claudecode.plist"), "<plist/>").expect("write");
    MANAGED_PREFERENCES.with(|m| *m.borrow_mut() = Some(prefs.clone()));
    assert_eq!(
        planned(&opts, &argv, true),
        plain,
        "a managed preference domain"
    );
    MANAGED_PREFERENCES.with(|m| *m.borrow_mut() = None);
    let configs = opts.home.join(".config/anthropic/configs");
    std::fs::create_dir_all(&configs).expect("configs");
    std::fs::write(configs.join("default.json"), "{}").expect("write");
    assert_eq!(planned(&opts, &argv, true), plain, "a profile store config");
    std::fs::remove_file(configs.join("default.json")).expect("remove");
    assert_eq!(planned(&opts, &argv, true), tapped, "and back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `--model` this harness asked and the record marks failed never rides the line.
#[test]
fn a_failed_model_on_the_launch_line_is_dropped_by_a_relaunch_asking_none() {
    let dir = scratch("line-failed");
    let opts = drive(&dir);
    let argv = |model: &str| -> Vec<String> {
        ["claude", "--model", model, "--verbose"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    };
    let line = |argv: &[String], model: Option<&str>| {
        line_for(
            Dialect::Bash,
            None,
            None,
            Path::new("/opt/claude"),
            argv,
            Some(SESSION),
            line_model_of(&opts, SESSION, argv, model),
            None,
            &[],
        )
        .unwrap_or_else(|_| panic!("a line"))
    };
    let ours = argv("claude-opus-5-5[1m]");
    let kept = format!(
        " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--model' 'claude-opus-5-5[1m]' \
         '--verbose' '--resume' '{SESSION}'"
    );
    let planned = || {
        let launch = crate::harness::relaunch::Launch {
            stored_login: false,
            billing: crate::harness::relaunch::ModelBilling::Keep,
            session: SESSION,
            resume: true,
            cwd: "/tmp",
            agent: 0,
            argv: &ours,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            login: None,
            home: None,
            homedir: None,
        };
        crate::harness::relaunch::plan(&opts, &launch, 999_999, &[(999_999, 1, "zsh".into())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let told = || resume_command_kept(Some(&opts.state), &ours, SESSION).expect("a resume line");
    register(&opts.home, std::process::id(), SESSION);
    let footer = || {
        let offset = |_: Option<&str>, _: i64| None;
        crate::harness::footer::read_pid(
            &opts.home.join(".claude"),
            std::process::id(),
            None,
            |_| (crate::harness::footer::LaunchFacts::default(), ours.clone()),
            &mut crate::harness::footer::TailCache::default(),
            &mut crate::harness::footer::FooterCache::default(),
            &crate::harness::footer::ReadClock {
                read_at: std::time::SystemTime::now(),
                now: 0,
                offset_at: &offset,
            },
            Some(&opts.state),
            Some(&opts.home),
        )
        .and_then(|facts| facts.resume)
        .expect("the footer's line")
    };
    assert_eq!(line(&ours, None), kept);
    assert!(planned().contains("'--model'"), "{}", planned());
    assert!(told().contains("--model"), "{}", told());
    assert_eq!(footer(), told());
    assert_eq!(
        launch_kept(&opts, SESSION, &ours).as_deref(),
        Some("claude-opus-5-5[1m]")
    );
    save_model_record(
        &opts,
        SESSION,
        &ModelRecord {
            failed: vec!["claude-opus-5-5".to_string()],
            ..ModelRecord::default()
        },
    );
    assert_eq!(
        line(&ours, None),
        format!(
            " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--verbose' '--resume' \
             '{SESSION}'"
        )
    );
    assert_eq!(launch_kept(&opts, SESSION, &ours), None);
    assert!(!planned().contains("--model"), "{}", planned());
    assert!(
        !told().contains("--model") && told().contains(&format!("--resume {SESSION}")),
        "{}",
        told()
    );
    assert_eq!(footer(), told());
    assert!(line(&ours, Some("claude-fable-5-1")).contains("'--model' 'claude-fable-5-1'"));
    for theirs in ["claude-fable-5-1", "opus"] {
        let argv = argv(theirs);
        assert!(line(&argv, None).contains(&format!("'--model' '{theirs}'")));
        assert_eq!(launch_kept(&opts, SESSION, &argv).as_deref(), Some(theirs));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A due model rides every restart (a relaunch on exit, the restart in place), even on a warm
/// conversation.
#[test]
fn a_due_model_rides_a_restart_made_for_another_reason() {
    let dir = scratch("riding-model");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(now)
        ),
    )
    .expect("transcript");
    let argv = vec!["/opt/claude/bin/claude".to_string()];
    let due = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    assert_eq!(
        riding_model_in(&opts, &sf, &argv, &due, None).as_deref(),
        Some("claude-opus-5-5"),
        "warm, and it rides all the same"
    );
    record_asked(&opts, SESSION, "claude-opus-5-5");
    assert_eq!(load_model_record(&opts, SESSION).set, "claude-opus-5-5");
    assert_eq!(
        riding_model_in(&opts, &sf, &argv, &ModelCtx::none(), None),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The due clock survives an unknown read (`model-unknown`, e.g. a tail holding no answer), or an
/// active session's one-hour bound would restart every visit.
#[test]
fn the_due_clock_survives_an_unknown_read_and_clears_when_nothing_is_due() {
    let dir = scratch("due-clock");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let path = proj.join(format!("{SESSION}.jsonl"));
    let answered = format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
        crate::harness::usage::rfc3339_utc(now)
    );
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    std::fs::write(&path, &answered).expect("transcript");
    assert!(matches!(
        model_read(&opts, &sf, None, &mctx).verdict,
        ModelVerdict::Due { .. }
    ));
    let mut rec = load_model_record(&opts, SESSION);
    assert_eq!(
        rec.due_to, "claude-opus-5-5",
        "the clock started on the due visit"
    );
    rec.due_since = now_s() - 3000;
    save_model_record(&opts, SESSION, &rec);

    std::fs::write(&path, "{\"type\":\"user\"}\n").expect("tail with no answer");
    let unknown = model_read(&opts, &sf, None, &mctx);
    assert_eq!(unknown.verdict, ModelVerdict::Keep("model-unknown"));
    let kept = load_model_record(&opts, SESSION);
    assert_eq!(
        (kept.due_to.as_str(), kept.due_since),
        ("claude-opus-5-5", rec.due_since),
        "an unknown read reset the due clock"
    );

    std::fs::write(&path, &answered).expect("transcript");
    let back = model_read(&opts, &sf, None, &mctx);
    assert!(
        back.due_for_s >= 3000,
        "the bound restarted: {}",
        back.due_for_s
    );

    let mut ahead = load_model_record(&opts, SESSION);
    ahead.due_since = now_s() + 10_000;
    save_model_record(&opts, SESSION, &ahead);
    let _ = model_read(&opts, &sf, None, &mctx);
    assert!(
        load_model_record(&opts, SESSION).due_since <= now_s(),
        "a future-dated start survived and would postpone the bound"
    );

    let none = ModelCtx {
        offered: Vec::new(),
        ..mctx
    };
    let _ = model_read(&opts, &sf, None, &none);
    let cleared = load_model_record(&opts, SESSION);
    assert!(
        cleared.due_to.is_empty() && cleared.due_since == 0,
        "the early return left a stale clock: {cleared:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An ask that ran stands.
#[test]
fn an_ask_that_ran_is_never_recorded_failed_when_a_person_moves_off_it() {
    let dir = scratch("ask-ran-stands");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let answered = format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
        crate::harness::usage::rfc3339_utc(now)
    );
    std::fs::write(proj.join(format!("{SESSION}.jsonl")), &answered).expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    let asked = |applied: bool| {
        let mut rec = ModelRecord {
            set: "claude-opus-5-5".to_string(),
            set_at: now_s() - 2 * models::MODEL_SETTLE_S,
            ..ModelRecord::default()
        };
        if applied {
            rec.applied.push(rec.set.clone());
        }
        save_model_record(&opts, SESSION, &rec);
    };

    asked(true);
    let _ = model_read(&opts, &sf, None, &mctx);
    let rec = load_model_record(&opts, SESSION);
    assert!(
        rec.failed.is_empty(),
        "an ask that ran was recorded failed: {rec:?}"
    );
    assert_eq!(
        rec.set, "claude-opus-5-5",
        "the ask stays the harness's own"
    );

    asked(false);
    let _ = model_read(&opts, &sf, None, &mctx);
    let rec = load_model_record(&opts, SESSION);
    assert_eq!(rec.failed, ["claude-opus-5-5"], "{rec:?}");
    assert!(rec.set.is_empty(), "{rec:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The transcript of a conversation whose last answer, on `model`, came
/// `ago_s` seconds ago.
fn answered_on(opts: &Opts, model: &str, ago_s: i64) {
    let at = i64::try_from(now_s()).expect("now") - ago_s;
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"{model}\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
}

/// A session on the native build is judged by the native build's own catalog and offer (here Fable
/// 5.1, where the managed view offers Opus 5.5).
#[test]
fn a_native_session_is_judged_by_the_native_builds_own_offer() {
    let dir = scratch("native-view");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    answered_on(&opts, "claude-opus-5", 7200);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-fable-5-1".to_string()])),
    };
    let due_to = |native: bool, ctx: &ModelCtx| match model_read_for(
        &opts, &sf, None, ctx, native, None, None,
    )
    .verdict
    {
        ModelVerdict::Due { to, .. } => Some(to),
        ModelVerdict::Keep(_) => None,
    };
    assert_eq!(due_to(false, &mctx).as_deref(), Some("claude-opus-5-5"));
    assert_eq!(due_to(true, &mctx).as_deref(), Some("claude-fable-5-1"));
    let no_native = ModelCtx {
        native: None,
        ..mctx
    };
    assert_eq!(due_to(true, &no_native).as_deref(), Some("claude-opus-5-5"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host's pre-filter never restarts a native session's warm wait.
#[test]
fn the_pre_filter_never_restarts_a_native_sessions_warm_wait() {
    let dir = scratch("prefilter-views");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    answered_on(&opts, "claude-opus-5", 0);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-opus-5-5".to_string()])),
    };
    let first = model_read_for(&opts, &sf, None, &mctx, true, None, None);
    assert!(
        matches!(&first.verdict, ModelVerdict::Due { to, .. } if to == "claude-opus-5-5"),
        "the native view must see the move due: {:?}",
        first.verdict
    );
    let mut rec = load_model_record(&opts, SESSION);
    rec.due_since = now_s() - 3000;
    save_model_record(&opts, SESSION, &rec);
    for sweep in 0..3 {
        assert!(model_wants_a_look(&opts, &sf, &mctx), "sweep {sweep}");
        let read = model_read_for(&opts, &sf, None, &mctx, true, None, None);
        assert!(
            read.due_for_s >= 3000,
            "sweep {sweep}: the pre-filter restarted the warm wait ({} s due)",
            read.due_for_s
        );
    }
    let mut over = load_model_record(&opts, SESSION);
    over.due_since = now_s() - models::MODEL_WARM_MAX_S;
    save_model_record(&opts, SESSION, &over);
    assert!(model_wants_a_look(&opts, &sf, &mctx));
    let read = model_read_for(&opts, &sf, None, &mctx, true, None, None);
    assert_eq!(
        models::model_moves_now(read.cold, false, read.due_for_s),
        Some("warm-wait-over"),
        "the warm wait must end for an active native session"
    );

    let agree = ModelCtx {
        offered: vec!["claude-opus-5-5".to_string()],
        ..mctx
    };
    record_asked(&opts, SESSION, "claude-opus-5-5");
    answered_on(&opts, "claude-opus-5-5", 0);
    assert!(!model_wants_a_look(&opts, &sf, &agree), "nothing due");
    assert_eq!(
        load_model_record(&opts, SESSION).applied,
        ["claude-opus-5-5"],
        "the pre-filter's settle step was not kept where the views agree"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A crashed native session's riding model is the native build's.
#[test]
fn a_crashed_native_sessions_riding_model_is_the_native_builds() {
    let dir = scratch("riding-native");
    let opts = drive(&dir);
    let mut sf = register(&opts.home, std::process::id(), SESSION);
    sf.pid = dead_pid();
    assert_eq!(exe_of(sf.pid), None, "the pid must be gone for this test");
    answered_on(&opts, "claude-opus-5", 0);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-opus-5-5".to_string()])),
    };
    let native = native_root(&opts.home).join("2.1.283");
    let argv = vec![native.to_string_lossy().into_owned()];
    assert_eq!(
        riding_model_in(&opts, &sf, &argv, &mctx, None).as_deref(),
        Some("claude-opus-5-5"),
        "a crashed native session was judged by the managed view"
    );
    let managed = vec!["/opt/aterm/pkg/agents/claude".to_string()];
    assert_eq!(riding_model_in(&opts, &sf, &managed, &mctx, None), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent under another Claude home is judged by that home's model files ([`ModelCtx::at`]).
#[test]
fn an_agent_under_another_claude_home_is_judged_by_its_homes_model_files() {
    let dir = scratch("model-home");
    let opts = Opts {
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let served_in = |claude_dir: &Path| {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    };
    served_in(&opts.home.join(".claude"));
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let other = dir.join("work/.claude");
    served_in(&other);
    let answered = |claude_dir: &Path| {
        let proj = claude_dir.join("projects/-work");
        std::fs::create_dir_all(&proj).expect("projects");
        let at = i64::try_from(now_s()).expect("now") - 7_200;
        std::fs::write(
            proj.join(format!("{SESSION}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
                crate::harness::usage::rfc3339_utc(at)
            ),
        )
        .expect("transcript");
    };
    answered(&other);
    let sf = SessionFile {
        claude_dir: Some(other.clone()),
        ..record("2.1.296")
    };
    let verdict = |opts: &Opts, sf: &SessionFile| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(opts, sf, None, &mctx, false, None, None).verdict
    };
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    assert_eq!(verdict(&opts, &sf), due, "the judge is not switched off");
    assert!(model_wants_a_look(&opts, &sf, &mctx));
    let settings = other.join("settings.json");
    let profile = other.join(".claude.json");
    for (name, file, text, kept) in [
        (
            "its saved default",
            &settings,
            r#"{"model":"claude-fable-5-1"}"#,
            "model-chosen-by-hand",
        ),
        (
            "its login's denial",
            &profile,
            r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#,
            "model-current",
        ),
    ] {
        std::fs::write(file, text).expect("the home's file");
        assert_eq!(
            verdict(&opts, &sf),
            ModelVerdict::Keep(kept),
            "{name}: judged by the user's home"
        );
        assert!(!model_wants_a_look(&opts, &sf, &mctx), "{name}");
        let argv = vec!["claude".to_string()];
        assert_eq!(
            riding_model_in(&opts, &sf, &argv, &mctx, None),
            None,
            "{name}"
        );
        answered(&opts.home.join(".claude"));
        let own = SessionFile {
            claude_dir: Some(opts.home.join(".claude")),
            ..sf.clone()
        };
        assert_eq!(verdict(&opts, &own), due, "{name}: the user's own home");
        let _ = std::fs::remove_file(file);
    }
    let several = Opts {
        several_logins: true,
        instances: Instances::Listed(Vec::new()),
        ..opts.clone()
    };
    assert_eq!(
        verdict(&several, &sf),
        ModelVerdict::Keep("no-model-available")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`agent_settings`] resolves `model` and `availableModels` by precedence across managed, project,
/// and user files.
#[test]
fn the_model_rule_takes_each_key_from_the_highest_document_that_sets_it() {
    let dir = scratch("model-settings-order");
    let opts = Opts {
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let user = opts.home.join(".claude");
    let catalog = user.join("cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let work = dir.join("work");
    let proj = user.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let sf = SessionFile {
        claude_dir: Some(user.clone()),
        cwd: work.to_string_lossy().into_owned(),
        ..record("2.1.296")
    };
    let managed = dir.join("managed");
    std::fs::create_dir_all(managed.join("managed-settings.d")).expect("the managed tier");
    std::fs::create_dir_all(work.join(".claude")).expect("the project's .claude");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = Some(managed.clone()));
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    let only_fable = r#"{"availableModels":["claude-fable-5-1"]}"#;
    let both_allowed = r#"{"availableModels":["claude-opus-5-5","claude-fable-5-1"]}"#;
    let files = [
        ("managed", managed.join("managed-settings.json")),
        ("drop-in", managed.join("managed-settings.d/10-models.json")),
        ("project", work.join(".claude/settings.json")),
        ("local", work.join(".claude/settings.local.json")),
        ("user", user.join("settings.json")),
    ];
    let case = |name: &str, docs: &[(&str, &str)]| {
        for (_, path) in &files {
            let _ = std::fs::remove_file(path);
        }
        for (which, text) in docs {
            let path = &files
                .iter()
                .find(|(w, _)| w == which)
                .expect("a document")
                .1;
            std::fs::write(path, text).expect("the document");
        }
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        let verdict = model_read_for(&opts, &sf, None, &mctx, false, None, None).verdict;
        let _ = name;
        verdict
    };
    assert_eq!(
        case("nothing set", &[]),
        due,
        "the judge is not switched off"
    );
    assert_eq!(
        case(
            "managed over project",
            &[("managed", only_fable), ("project", both_allowed)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "a drop-in over the managed file",
            &[("managed", both_allowed), ("drop-in", only_fable)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "the local file over the user's",
            &[("local", only_fable), ("user", both_allowed)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "a higher list replaces a lower one whole",
            &[("project", both_allowed), ("user", only_fable)]
        ),
        due
    );
    assert_eq!(
        case(
            "the managed model",
            &[("managed", r#"{"model":"claude-fable-5-1"}"#)]
        ),
        ModelVerdict::Keep("model-chosen-by-hand")
    );
    assert_ne!(
        case("a document that does not parse", &[("project", "{")]),
        due,
        "unknown: nothing is moved"
    );
    assert_ne!(
        case(
            "an allowlist that is no list",
            &[("project", r#"{"availableModels":"opus"}"#)]
        ),
        due,
        "an empty allowlist"
    );
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent under an empty `CLAUDE_CONFIG_DIR` keeps its records and settings in the directory it
/// runs in.
#[test]
fn an_agent_under_an_empty_claude_home_is_judged_by_the_profile_claude_code_keeps_for_it() {
    let dir = scratch("model-empty-home-profile");
    let opts = Opts {
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let work = dir.join("work");
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    for claude_dir in [opts.home.join(".claude"), work.clone()] {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    }
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    const LEADER: u32 = 4_242;
    std::fs::create_dir_all(work.join("sessions")).expect("sessions");
    std::fs::write(
        work.join(format!("sessions/{LEADER}.json")),
        format!(
            r#"{{"pid":{LEADER},"sessionId":"{SESSION}","cwd":"{}","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#,
            work.display()
        ),
    )
    .expect("the record");
    let proj = work.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(LEADER)),
    }];
    let read = |home: &str| {
        let args = |_: u32| {
            Some(atpkg::caller_shell::ProcArgs {
                env: vec![format!("CLAUDE_CONFIG_DIR={home}")],
                ..atpkg::caller_shell::ProcArgs::default()
            })
        };
        Homes::read(&opts, Some(&tabs), args, |_| Some(work.clone()))
            .roster(Some(Vec::new()))
            .and_then(|files| files.into_iter().find(|sf| sf.pid == LEADER))
            .expect("the record is read where its Claude Code files it")
    };
    let verdict = |sf: &SessionFile| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, sf, None, &mctx, false, None, None).verdict
    };
    let denial = r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#;
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    let empty = read("");
    assert_eq!(verdict(&empty), due, "the judge is not switched off");
    let home_profile = opts.home.join(".claude.json");
    std::fs::write(&home_profile, denial).expect("the profile");
    assert_eq!(
        verdict(&empty),
        ModelVerdict::Keep("model-current"),
        "an empty home's login is read in $HOME/.claude.json"
    );
    std::fs::remove_file(&home_profile).expect("rm");
    let cwd_profile = work.join(".claude.json");
    std::fs::write(&cwd_profile, denial).expect("a stray file");
    assert_eq!(
        verdict(&empty),
        due,
        "an empty home's working directory holds no profile of its"
    );
    let dot = read(".");
    assert_eq!(dot.claude_dir.as_deref(), Some(work.as_path()));
    assert_eq!(
        verdict(&dot),
        ModelVerdict::Keep("model-current"),
        "a relative home's profile is inside it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The CLI's sweep (no roster, [`sweep`]) finds an agent under another Claude home that no visit
/// has recorded.
#[cfg(unix)]
#[test]
fn the_clis_sweep_finds_an_agent_under_another_claude_home_no_visit_recorded() {
    let dir = scratch("cli-sweep-other-home");
    let claude_dir = dir.join("work home/.claude");
    let mut agent = tty_agent(
        &[(crate::harness::accounts::HOME_ENV, claude_dir.as_path())],
        Some(&dir),
    );
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    register_as(&claude_dir, pid, SESSION, "unknown");
    let reports = sweep(&opts);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        reports.iter().any(|r| r.session == SESSION && r.pid == pid),
        "the sweep visits it: {reports:?}"
    );
}

/// Claude's session file for `pid` as [`register`] writes it, its status
/// changed `ago_s` seconds before now: a turn that just ended is `0`.
fn register_idle_since(home: &Path, pid: u32, session: &str, ago_s: u64) -> SessionFile {
    let sf = register(home, pid, session);
    let at_ms = (now_s() - ago_s) * 1000;
    let path = home.join(format!(".claude/sessions/{pid}.json"));
    let text = std::fs::read_to_string(&path).expect("session file");
    std::fs::write(
        &path,
        text.replace(
            r#""statusUpdatedAt":1,"#,
            &format!(r#""statusUpdatedAt":{at_ms},"#),
        ),
    )
    .expect("rewrite");
    let again = session_file_of(home, pid).expect("the file parses");
    assert_eq!(again.status_updated_at_ms, at_ms);
    assert_eq!(again.session_id, sf.session_id);
    again
}

/// A stand-in agent parked in [`TAB`], registered with its status changed `ago_s` seconds ago.
#[cfg(unix)]
struct Parked {
    dir: ScratchDir,
    opts: Opts,
    /// The stand-in agent, until a test takes it to reap it itself
    /// ([`Parked::reaped`]).
    agent: Option<aterm_uds::spawnfd::Child>,
    /// The agent [`Parked::reaped`] handed to its reaping thread, until that
    /// thread reaps it: the rig still ends it when the test did not.
    handed: Option<Handed>,
    sf: SessionFile,
    asked: crate::harness::FakeInstanceLog,
}

/// A stand-in agent its reaping thread holds, and reaps only under this lock.
#[cfg(unix)]
type Handed = std::sync::Arc<std::sync::Mutex<Option<aterm_uds::spawnfd::Child>>>;

#[cfg(unix)]
impl Parked {
    fn new(name: &str, answers: Answers, st: St, ago_s: u64) -> Parked {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_idle_since(&opts.home, agent.id(), SESSION, ago_s);
        file_transcript_in(&opts.home.join(".claude"), "p");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
        Parked {
            dir,
            opts,
            agent: Some(agent),
            handed: None,
            sf,
            asked,
        }
    }

    fn visit(&self) -> Report {
        self.visit_under(dead_pid())
    }

    /// [`Self::visit`] with the agent the foreground job of `shell`, a zsh.
    fn visit_under(&self, shell: u32) -> Report {
        self.visit_with(shell, &[])
    }

    /// [`Self::visit_under`], the process table holding `more` too.
    fn visit_with(&self, shell: u32, more: &[ProcRow]) -> Report {
        let mut table = vec![(shell, 1, "zsh".into())];
        table.extend(more.iter().cloned());
        visit(
            &self.opts,
            &self.sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    }

    /// The stand-in agent handed to a thread that reaps it the moment it ends
    /// and removes its session file, as Claude's own exit does, so a restart's
    /// wait for it to be gone ends on the event; says how it ended.
    fn reaped(&mut self) -> std::sync::mpsc::Receiver<std::process::ExitStatus> {
        let agent = self.agent.take().expect("the agent");
        let file = self
            .opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.id()));
        let held: Handed = std::sync::Arc::new(std::sync::Mutex::new(Some(agent)));
        self.handed = Some(std::sync::Arc::clone(&held));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut slot = held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(agent) = slot.as_mut() else { return };
                match agent.try_wait() {
                    Ok(Some(status)) => {
                        *slot = None;
                        drop(slot);
                        let _ = std::fs::remove_file(&file);
                        let _ = tx.send(status);
                        return;
                    }
                    Ok(None) => {}
                    Err(_) => return,
                }
                drop(slot);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        rx
    }

    /// The details of the ledger's rows whose step is `step`.
    fn details(&self, step: &str) -> Vec<String> {
        ledger_details(&self.opts, step)
    }

    fn typed(&self) -> Vec<String> {
        self.asked.lock().map_or_else(
            |_| Vec::new(),
            |a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect(),
        )
    }
}

#[cfg(unix)]
impl Drop for Parked {
    fn drop(&mut self) {
        if let Some(agent) = &mut self.agent {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        if let Some(handed) = &self.handed
            && let Some(agent) = handed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
        {
            let _ = agent.kill();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A rig ends the stand-in agent it handed to [`Parked::reaped`] when the test did not, so an
/// agent the visit never signalled cannot outlive its test.
#[cfg(unix)]
#[test]
fn a_rig_ends_the_agent_it_handed_to_its_reaper() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut h = Parked::new("handed-ends", Answers::default(), St::default(), 3600);
    let ended = h.reaped();
    assert!(
        ended.try_recv().is_err(),
        "control: the agent runs until the rig ends"
    );
    drop(h);
    let status = ended
        .recv_timeout(HANG_DETECTOR)
        .expect("the rig ended its agent");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
}

/// The details of `opts`' ledger rows whose step is `step`.
fn ledger_details(opts: &Opts, step: &str) -> Vec<String> {
    std::fs::read_to_string(state_dir(opts).join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
        .filter_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// `/bin/zsh`, or where none is installed (stock Debian and Ubuntu ship none) a
/// copy of `/bin/sh` named `zsh`: [`tty_zsh`] needs a shell that leads its
/// terminal and whose executable reads as zsh (`process_args(..).exec_path`,
/// `Dialect::from_exe_name`), and only the file's name decides that.
#[cfg(unix)]
fn zsh_executable() -> &'static Path {
    static ZSH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ZSH.get_or_init(|| {
        let installed = Path::new("/bin/zsh");
        if installed.exists() {
            return installed.to_path_buf();
        }
        let exe = std::env::current_exe().expect("the test binary's path");
        let dir = exe
            .parent()
            .expect("the test binary's folder")
            .join("aterm-agent-zsh-stand-in");
        let zsh = dir.join("zsh");
        if zsh.exists() {
            return zsh;
        }
        std::fs::create_dir_all(&dir).expect("the stand-in's folder");
        let laying = dir.join(format!("zsh.{}", std::process::id()));
        let copied = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("cp")
                .arg("/bin/sh")
                .arg(&laying),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap_or_else(|e| panic!("cp within the hang detector: {e}"))
        .status;
        assert!(copied.success(), "copy /bin/sh to {laying:?}: {copied}");
        std::fs::rename(&laying, &zsh).expect("lay the stand-in");
        zsh
    })
}

/// A zsh ([`zsh_executable`]) that is the session leader of a pseudo-terminal
/// of its own and its foreground process group (`pgid == tpgid`): the shell a
/// relaunch waits to hold its terminal again.
#[cfg(unix)]
fn tty_zsh() -> std::process::Child {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned exclusively here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let mut zsh = std::process::Command::new(zsh_executable());
    zsh.inherit_nothing()
        .args(["-f", "-c", "sleep 60; :"])
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    // SAFETY: calls only async-signal-safe `setsid`/`ioctl` between fork and exec.
    unsafe {
        zsh.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = zsh.spawn().expect("zsh");
    std::mem::forget(master);
    drop(slave);
    for _ in 0..6_000 {
        if ids(child.id())
            .is_some_and(|(_, pgid, tpgid)| i64::from(child.id()) == pgid && pgid == tpgid)
            && atpkg::caller_shell::process_args(child.id())
                .is_some_and(|a| a.exec_path.ends_with("zsh"))
        {
            return child;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("zsh never held its terminal");
}

/// The pty host's argv: this test binary, running only [`host_a_pty_when_asked`].
#[cfg(target_os = "macos")]
const PTY_HOST: [&str; 2] = [
    "harness::upgrade_drive::tests::host_a_pty_when_asked",
    "--exact",
];

/// What tells [`host_a_pty_when_asked`] to host, and the argv it runs, its words split by a
/// unit separator (`0x1f`): an argv after `--exact` would be more test filters.
const PTY_HOST_ENV: &str = "UPGRADE_DRIVE_TEST_PTY_HOST";

/// The pty host's body ([`pty_host`]): `script(1)`'s part, without `script(1)`, whose BSD
/// and util-linux forms differ.
#[cfg(unix)]
#[test]
fn host_a_pty_when_asked() {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::process::CommandExt as _;
    let Some(argv) = std::env::var_os(PTY_HOST_ENV) else {
        return;
    };
    let words: Vec<&std::ffi::OsStr> = argv
        .as_bytes()
        .split(|b| *b == 0x1f)
        .map(std::ffi::OsStr::from_bytes)
        .collect();
    let (program, args) = words.split_first().expect("a program to host");
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned exclusively here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let mut hosted = std::process::Command::new(program);
    hosted
        .inherit_nothing()
        .args(args)
        .env_remove(PTY_HOST_ENV)
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    // SAFETY: calls only async-signal-safe `setsid`/`ioctl` between fork and exec.
    unsafe {
        hosted.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // SAFETY: getppid takes nothing and cannot fail.
    let parent = unsafe { libc::getppid() };
    let mut child = hosted.spawn().expect("the hosted program");
    drop(slave);
    let mut master = std::fs::File::from(master);
    std::thread::spawn(move || {
        let mut sink = [0_u8; 4096];
        while std::io::Read::read(&mut master, &mut sink).is_ok_and(|n| n > 0) {}
    });
    // SAFETY: as above.
    while unsafe { libc::getppid() } == parent {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `argv` on a pty a process of its own opened ([`host_a_pty_when_asked`]): the shape of
/// `script -q /dev/null argv…`, of every tmux or screen pane.
#[cfg(target_os = "macos")]
fn pty_host(argv: &[&std::ffi::OsStr]) -> Command {
    use std::os::unix::ffi::OsStrExt as _;
    let mut words = Vec::new();
    for (i, word) in argv.iter().enumerate() {
        assert!(!word.as_bytes().contains(&0x1f), "{word:?}");
        if i > 0 {
            words.push(0x1f);
        }
        words.extend_from_slice(word.as_bytes());
    }
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.args(PTY_HOST)
        .env(PTY_HOST_ENV, std::ffi::OsStr::from_bytes(&words))
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    c
}

/// The name `ps` gives `pid` (`tty_table`, the oracle), `None` once it is gone.
#[cfg(target_os = "macos")]
fn ps_name(pid: u32) -> Option<String> {
    tty_table()
        .into_iter()
        .find(|(p, _, _, _)| *p == pid)
        .map(|(_, _, _, name)| name)
}

/// A stand-in agent ([`parked`]) that leads a terminal of its own, as a Claude Code leads its
/// aterm tab: a new session on a pty that is its controlling terminal, so its group is the
/// terminal's foreground (`ids`: `pgid == tpgid == pid`) and a roster naming that group (`who …
/// fgpgid=<pid>`) claims it ([`host_candidates`]).
#[cfg(unix)]
fn tty_agent(env: &[(&str, &Path)], cwd: Option<&Path>) -> std::process::Child {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned exclusively here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let mut agent = std::process::Command::new(std::env::current_exe().expect("exe"));
    agent
        .inherit_nothing()
        .args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .env_remove(crate::harness::accounts::LOGIN_ENV)
        .env_remove(crate::harness::accounts::HOME_ENV)
        .env_remove("HOME")
        .stdin(tty(&slave))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, value) in env {
        agent.env(key, value);
    }
    if let Some(cwd) = cwd {
        agent.current_dir(cwd);
    }
    // SAFETY: calls only async-signal-safe `setsid`/`ioctl` between fork and exec.
    unsafe {
        agent.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = agent.spawn().expect("agent");
    std::mem::forget(master);
    drop(slave);
    let me = std::env::current_exe().expect("exe");
    for _ in 0..6_000 {
        if ids(child.id())
            .is_some_and(|(_, pgid, tpgid)| i64::from(child.id()) == pgid && pgid == tpgid)
            && atpkg::caller_shell::process_args(child.id())
                .is_some_and(|a| Path::new(&a.exec_path).file_name() == me.file_name())
        {
            return child;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("the stand-in agent never held its terminal");
}

/// A pending state for [`SESSION`] whose screen ([`idle_screen`], seq 77) has
/// just been seen to move.
fn unsettled() -> St {
    St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 76,
        seq_since_s: now_s(),
        salt: 1,
        pending_since: now_s(),
        ..St::default()
    }
}

/// [`unsettled`], its screen (seq 77) unchanged for an hour, in [`TAB`]: a
/// settled idle point the notice may be typed at.
fn settled() -> St {
    St {
        last_seq: 77,
        seq_since_s: now_s() - 3600,
        tab: TAB.to_string(),
        ..unsettled()
    }
}

/// The screen [`idle_screen`] shows, with the server's person stamp `human`
/// (`"human_ms"`'s value; `None`: a host that sends none).
#[cfg(unix)]
fn stamped_screen(human: Option<&str>) -> String {
    let rule = "─".repeat(20);
    let stamp = human.map_or_else(String::new, |h| format!(r#","human_ms":{h}"#));
    format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77{stamp},"first":0}}"#
    )
}

/// The presence guard: a tab is attended when the server's per-session stamp (`human_ms`) on the
/// read the sweep judges shows input within `human_grace_s`.
#[cfg(unix)]
#[test]
fn an_attended_tab_has_a_person_at_it_and_is_said_once() {
    let settled = settled();
    let front = "meta=0 window=1 active=1 wfocus=1";
    let rig = |name: &str, human: Option<&str>| {
        Parked::new(
            name,
            Answers {
                row: front,
                screen: stamped_screen(human),
                ..Answers::default()
            },
            settled.clone(),
            3600,
        )
    };
    let h = rig("attended", Some("30000"));
    assert_eq!(h.visit().step, "held-back:attended");
    assert_eq!(h.visit().step, "wait:attended");
    assert_eq!(h.visit().step, "wait:attended");
    assert!(h.typed().is_empty());
    assert_eq!(h.details("held-back:attended").len(), 1, "said once");
    drop(h);
    let h = rig("attended-unstamped", None);
    assert_eq!(h.visit().step, "held-back:attended", "fails closed");
    drop(h);
    let h = rig("attended-now", Some("30000"));
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    assert_eq!(h.visit().step, "announced:1");
    drop(h);
    for (case, human) in [("person-away", "660000"), ("never", "null")] {
        let h = rig(&format!("unattended-{case}"), Some(human));
        assert_eq!(h.visit().step, "announced:1", "{case}");
    }
}

/// A pressed notice leaves Claude Code's open session survey to the person (its first key would
/// close the card unanswered).
#[cfg(unix)]
#[test]
fn a_pressed_notice_leaves_an_open_survey_to_the_person() {
    let rule = "─".repeat(80);
    let survey = [
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
        "",
    ];
    let with_cap = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    for (name, card) in [("survey-open", true), ("survey-none", false)] {
        let mut rows: Vec<String> = if card {
            survey.iter().map(|r| (*r).to_string()).collect()
        } else {
            vec![String::new(); 3]
        };
        rows.extend([rule.clone(), "❯ ".to_string(), rule.clone()]);
        let screen = format!(
            r#"{{"rows":{},"cursor":{{"row":4,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#,
            aterm_json::to_string(&rows).expect("rows")
        );
        let h = Parked::new(
            name,
            Answers {
                screen,
                help: with_cap,
                turn: vec!["OK 0 turn skipped reason=changed submitted=0 seq=78 id=1"],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let r = h.visit();
        let typed = h.typed();
        if card {
            assert!(typed.is_empty(), "{name}: nothing typed: {typed:?}");
            assert!(r.step.contains("survey"), "{name}: {r:?}");
        } else {
            assert!(
                typed.iter().any(|t| t.contains(" typed=1 cadence=2 ")),
                "{name}: the notice is pressed: {typed:?}"
            );
        }
    }
}

/// Only the notice is pressed key by key.
#[cfg(unix)]
#[test]
fn only_the_agents_own_words_are_pressed_where_the_host_takes_cap() {
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let answers = || Answers {
        help: CAP_HELP,
        ..fenced_answers(vec![SETTLED])
    };
    let dir = scratch("pressed-only-notice");
    let (sock, asked) = instance_with(&dir, answers());
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let line = " '/pkg/claude' '--resume' 'x'";
    let got = type_relaunch_line_with(
        &opts,
        &mut c,
        4242,
        TAB,
        line,
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
            held_since_s: 0,
        },
        || true,
        |_, _, _| true,
    );
    assert_eq!(got, Ok(()));
    let generation = typing_fence(&mut c, TAB).expect("empty composer");
    notice_fenced(&mut c, TAB, "[aterm harness] notice", generation.as_deref()).expect("typed");
    drop(c);
    let turns: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert!(
        turns[0].contains(" turn if-gen=4.77 yield=0.2 ") && turns[0].ends_with(line),
        "the relaunch line is one fenced paste: {turns:#?}"
    );
    assert!(
        turns[1].contains(" turn if-gen=4.77 typed=1 cadence=2 cap=4096 yield=0.2 "),
        "the notice is pressed: {turns:#?}"
    );
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::with("pressed-not-continue", &before, answers());
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(&format!(" turn if-gen=4.77 yield=0.2 {TURN_WAIT} ")),
        "the continuation is one fenced paste: {typed:#?}"
    );
}

/// A pressed notice's turn budget covers its keys and its digits.
#[cfg(unix)]
#[test]
fn a_pressed_notices_turn_budget_covers_its_keys_and_its_digits() {
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let v = |s: &str| Version::parse(s).expect("version");
    let held: Vec<upgrade::Held> = (0..5u32)
        .map(|i| upgrade::Held {
            pid: 70_100 + i,
            name: "zsh".to_string(),
            age_s: 90_061 + u64::from(i),
            command: format!("/bin/zsh -c sleep 3{i}00; make test-{i}"),
        })
        .collect();
    let notice = upgrade::with_running(
        &upgrade::prepare_prompt_with_model(
            &v("2.1.290"),
            &v("2.1.291"),
            Source::Managed,
            &upgrade::ready_marker(SESSION, &v("2.1.291"), 1),
            None,
            None,
        ),
        &upgrade::running_clause(&held),
    );
    let dir = scratch("pressed-budget");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            help: CAP_HELP,
            ..fenced_answers(vec![SETTLED])
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    turn_fenced(&mut c, TAB, "'/pkg/claude' '--resume' 'x'", None).expect("pasted");
    notice_fenced(&mut c, TAB, &notice, None).expect("pressed");
    drop(c);
    let turns: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert!(
        turns[0].contains(&format!(" yield=0.2 {TURN_WAIT} ")),
        "the paste waits TURN_WAIT: {turns:#?}"
    );
    let count = |n: usize| u64::try_from(n).expect("count");
    let keys = count(notice.chars().count());
    let digits = count(notice.chars().filter(char::is_ascii_digit).count());
    assert!(digits >= 40, "a notice carries dozens of digits: {digits}");
    assert!(
        turns[1].contains(" typed=1 cadence=2 cap=4096 yield=0.2 idle=1 timeout="),
        "{}",
        turns[1]
    );
    let timeout: u64 = turns[1]
        .split_whitespace()
        .find_map(|w| w.strip_prefix("timeout="))
        .and_then(|t| t.parse().ok())
        .expect("a timeout");
    assert!(
        timeout >= keys * 2 + digits * 150 + 10_000,
        "timeout={timeout} keys={keys} digits={digits}"
    );
}

/// A notice the host cut short (`skipped reason=<why> typed=<k>`, [`Typed::Cut`]) is never
/// announced (the agent has half a notice, no ready sentence) nor typed again in the visit.
#[cfg(unix)]
#[test]
fn a_notice_the_host_cut_short_is_never_announced_or_typed_again() {
    let cut = "OK 0 turn skipped reason=person typed=40 pasted=0 submitted=0 seq=78 id=1";
    for (head, want) in [
        (cut, Err(Typed::Cut("person".to_string()))),
        ("OKAY", Err(Typed::Refused("OKAY".to_string()))),
        (
            "OK 0 turn skipped reason=deadline typed=12 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Cut("deadline".to_string())),
        ),
        (
            "OK 0 turn skipped reason=changed typed=5 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Cut("changed".to_string())),
        ),
        (
            "OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Yielded),
        ),
        (
            "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
            Err(Typed::Changed),
        ),
        (
            "OK 0 turn submitted=1 status=settled seq=9 id=3 dur_ms=1 hash=0 typed=40 pasted=0",
            Ok(()),
        ),
        ("OK 3 id=1 submitted=1 status=settled", Ok(())),
        (
            "OK 0 turn skipped reason=guard submitted=0 seq=9 id=1",
            Err(Typed::Cut("guard".to_string())),
        ),
        ("ERR yield timeout momentum=0.84", Err(Typed::Yielded)),
    ] {
        assert_eq!(turn_verdict(head), want, "{head}");
    }
    assert!(matches!(
        turn_verdict("ERR busy turn=4"),
        Err(Typed::Refused(_))
    ));
    assert!(is_generation("12.345") && !is_generation("12") && !is_generation("1.x"));
    assert!(!is_generation("18446744073709551616.15"));
    assert_eq!(Typed::Cut("person".to_string()).word(), "cut-person");
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let with_cap = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                    [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    let visit = |name: &str, reply: &'static str| {
        let h = Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help: with_cap,
                turn: vec![reply],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let step = h.visit().step;
        let phase = load(&h.opts, SESSION).expect("the record").phase;
        (step, phase, h.typed().len())
    };
    let (step, phase, typed) = visit("notice-cut", cut);
    assert_eq!(step, "wait:announce-refused:cut-person");
    assert_eq!(phase, Phase::Pending, "not announced");
    assert_eq!(typed, 1, "not typed again over its own half");
    let (step, _, _) = visit(
        "notice-cut-whole",
        "OK 0 turn submitted=1 status=settled seq=78 id=1 dur_ms=2 hash=0 typed=900 pasted=0",
    );
    assert_eq!(step, "announced:1");
}

/// A labelled top rule (an effort tag, `sandboxed`) is the input box too.
#[cfg(unix)]
#[test]
fn a_labelled_top_rule_is_the_input_box_the_notice_fences_on() {
    use aterm_types::control_verbs::RowsFence;
    for (name, text) in [
        (
            "wait_bg",
            include_str!("../../../aterm-phase/src/fixtures/wait_bg.out"),
        ),
        (
            "footer-auto-idle",
            include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-idle.txt"),
        ),
    ] {
        let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .unwrap_or_else(|| panic!("{name}: a caret row"));
        let scr = Screen {
            rows: rows.clone(),
            cursor: Some((caret, 2)),
            seq: 1,
            first: 0,
            generation: Some("4.77".to_string()),
            human: HumanInput::Unknown,
        };
        let fence = composer_rows(&scr).unwrap_or_else(|| panic!("{name}: the box"));
        assert_eq!((fence.first, fence.last), (caret - 1, caret + 1), "{name}");
        let three = rows[caret - 1..=caret + 1].iter().map(String::as_str);
        assert_eq!(fence.hash, RowsFence::hash_rows(three), "{name}");
        assert!(
            rows[caret - 1].trim_end().ends_with(" ─"),
            "{name}: labelled: {}",
            rows[caret - 1]
        );
        let off = Screen {
            cursor: Some((caret - 1, 0)),
            ..scr
        };
        assert!(composer_rows(&off).is_none(), "{name}");
    }
    let top = format!("{} sandboxed ─", "─".repeat(20));
    let rule = "─".repeat(33);
    let dir = scratch("fence-rows-labelled");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            screen: format!(
                r#"{{"rows":["✻ Waiting for 1 dynamic workflow to finish","{top}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
            ),
            help: ROWS_HELP,
            turn: vec!["OK 0 id=2 submitted=1 status=settled"],
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let (fence, _) = claude_fence(&mut c, TAB).expect("empty composer");
    notice_fenced(&mut c, TAB, "[aterm harness] hi", fence.as_deref()).expect("typed");
    drop(c);
    let line = asked
        .lock()
        .expect("log")
        .iter()
        .find(|l| l.split_whitespace().find(|w| !w.starts_with('@')) == Some("turn"))
        .cloned()
        .expect("a turn was sent");
    let _ = std::fs::remove_dir_all(&dir);
    let want = RowsFence {
        first: 1,
        last: 3,
        hash: RowsFence::hash_rows([top.as_str(), "❯ ", rule.as_str()]),
    };
    assert!(line.contains(&format!("if-rows={want} ")), "{line}");
    assert!(!line.contains("if-gen="), "{line}");
}

/// A host's `help` that names every option these rigs send: the guarded
/// submit, the key-by-key `cap=`, the generation fence — and a `key` entry
/// naming its fences, as the take-back asks.
#[cfg(unix)]
const PRESS_HELP: &str = "OK 3\nturn [option=value ...] <text>: ... [submit=<key|none|guarded:<re>>] \
                          [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...\n\
                          key <name>: one key\n    FENCES (key [if-gen=<epoch>.<seq>] \
                          [if-human=<n>] <name>) ...";

/// The row a composer's text ends on, word-wrapped at `width` columns as Claude Code wraps it.
#[cfg(unix)]
pub(super) fn last_wrapped_row(text: &str, width: usize) -> String {
    const INDENT: &str = "  ";
    let len = |s: &str| s.chars().count();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let fresh = row.is_empty() || row == INDENT;
            let need = len(&word) + usize::from(!fresh);
            if len(&row) + need <= width {
                if !fresh {
                    row.push(' ');
                }
                row.push_str(&word);
                break;
            }
            if fresh {
                let room = width - len(&row);
                row.extend(word.chars().take(room));
                word = word.chars().skip(room).collect();
            }
            row = INDENT.to_string();
        }
    }
    row
}

/// A pressed notice's Enter is guarded on its own last row (`submit=guarded:`, the supervisor's
/// composer guard).
#[cfg(unix)]
#[test]
fn a_pressed_notices_enter_is_guarded_on_its_own_last_row() {
    let v = |s: &str| Version::parse(s).expect("version");
    let marker = upgrade::ready_marker(SESSION, &v("2.1.291"), 1);
    let notice = upgrade::prepare_prompt_with_model(
        &v("2.1.290"),
        &v("2.1.291"),
        Source::Managed,
        &marker,
        None,
        None,
    );
    let release = upgrade::release_prompt(upgrade::Agent::Claude, false);
    let the_box = [
        " Do you want to proceed?",
        "❯ 1. Yes",
        "  2. Yes, and don't ask again for this command",
        "  3. No, and tell Claude what to do differently (esc)",
    ];
    for text in [&notice, &release] {
        let guard = aterm_observe::row_matcher(&composer_guard('❯', text))
            .expect("the host compiles the guard");
        for width in 30..=160 {
            let last = last_wrapped_row(&format!("❯ {text}"), width);
            assert!(guard.matches(&last), "width {width}: {last:?}");
        }
        for row in the_box {
            assert!(!guard.matches(row), "{row}");
        }
    }
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let rig = |name: &str, help: &'static str, reply: &'static str| {
        let h = Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help,
                turn: vec![reply],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let step = h.visit().step;
        let phase = load(&h.opts, SESSION).expect("the record").phase;
        (step, phase, h.typed())
    };
    const WHOLE: &str =
        "OK 0 turn submitted=1 status=settled seq=78 id=1 dur_ms=2 hash=0 typed=900 pasted=0";
    let (step, _, typed) = rig("guarded-notice", PRESS_HELP, WHOLE);
    assert_eq!(step, "announced:1");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    let text = typed[0]
        .find(" [aterm harness]")
        .map(|at| &typed[0][at + 1..])
        .expect("the notice");
    assert!(
        typed[0].contains(&format!(
            " typed=1 cadence=2 cap=4096 submit=guarded:{} yield=0.2 ",
            composer_guard('❯', text)
        )),
        "{}",
        typed[0]
    );
    let (step, phase, typed) = rig(
        "guarded-notice-missed",
        PRESS_HELP,
        "OK 0 turn skipped reason=guard submitted=0 seq=78 id=1",
    );
    assert_eq!(step, "wait:announce-refused:cut-guard");
    assert_eq!(phase, Phase::Pending, "not announced");
    assert_eq!(typed.len(), 1, "not typed again over its own words");
    const CAP_ONLY: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                            [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    let (step, _, typed) = rig("unguarded-notice", CAP_ONLY, WHOLE);
    assert_eq!(step, "announced:1");
    assert!(
        typed[0].contains(" typed=1 cadence=2 cap=4096 yield=0.2 ")
            && !typed[0].contains("submit="),
        "{}",
        typed[0]
    );
}

/// What a stand-in prompt's `ctrl+u` takes back ([`prompt_tab`]).
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kill {
    /// The whole line.
    Line,
    /// The row the cursor is on, as a composer that kills a wrapped row at a
    /// time would.
    Row,
    /// Nothing.
    Nothing,
}

/// A Claude Code prompt for [`left_standing`] alone, 40 columns wide, holding `draft`
/// ([`Prompt::screen`]): `text --json` draws it, a `cell` reads plain, `help` answers `help`, and
/// `key … ctrl+u` (only while its `if-gen=` names the screen drawn) takes back what `kill` says.
#[cfg(unix)]
fn prompt_tab(
    draft: &str,
    kill: Kill,
    help: &'static str,
) -> (Client, std::thread::JoinHandle<(Vec<String>, String)>) {
    use std::io::BufRead as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let mut prompt = Prompt {
        draft: draft.to_string(),
        person_key: false,
    };
    let server = std::thread::spawn(move || {
        let mut out = theirs.try_clone().expect("clone");
        let mut asked = Vec::new();
        let mut generation = 77;
        for line in std::io::BufReader::new(theirs).lines() {
            let Ok(line) = line else { break };
            let verb = line
                .split_whitespace()
                .find(|w| !w.starts_with('@'))
                .unwrap_or("");
            let reply = match verb {
                "text" => format!("OK {}", prompt.screen(generation)),
                "cell" => "OK %20 d0d0d0 111318 none".to_string(),
                "help" => help.to_string(),
                "key" if !line.contains(&format!(" if-gen=4.{generation} ")) => {
                    format!("OK skipped reason=changed seq={generation}")
                }
                "key" => {
                    let n = prompt.draft.chars().count();
                    let keep = match kill {
                        Kill::Line => 0,
                        Kill::Row => n.saturating_sub(1) / 38 * 38,
                        Kill::Nothing => n,
                    };
                    prompt.draft = prompt.draft.chars().take(keep).collect();
                    generation += 1;
                    format!("OK seq={generation}")
                }
                _ => "ERR unscripted".to_string(),
            };
            asked.push(line);
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
        (asked, prompt.draft)
    });
    (RelayClient::new(ours), server)
}

/// Aterm never takes its own words back (a `ctrl+u` fenced on the read cannot see a key the host
/// counted but Claude Code has not drawn).
#[cfg(unix)]
#[test]
fn aterms_own_words_are_read_never_taken_back() {
    let release = upgrade::release_prompt(upgrade::Agent::Claude, false);
    let part: String = release.chars().take(150).collect();
    let presses = |asked: &[String]| asked.iter().filter(|l| l.contains(" key ")).count();
    for (draft, want) in [
        (part.clone(), Left::Kept("unsent")),
        (format!("{part}x"), Left::Kept("mixed")),
        ("push it as soon as the gates pass".to_string(), Left::Gone),
        (String::new(), Left::Gone),
    ] {
        for kill in [Kill::Line, Kill::Row, Kill::Nothing] {
            let (mut c, server) = prompt_tab(&draft, kill, PRESS_HELP);
            assert_eq!(left_standing(&mut c, TAB, &release), want, "{draft:?}");
            drop(c);
            let (asked, left) = server.join().expect("server");
            assert_eq!(
                (presses(&asked), left),
                (0, draft.clone()),
                "nothing pressed"
            );
        }
    }
}

/// A notice the host cut short is recorded (`St::left_typed`) and read first at every later look as
/// aterm's own (`wait:left-typed:unsent`).
#[cfg(unix)]
#[test]
fn a_notice_the_host_stopped_stands_unsent_and_is_never_pressed_back() {
    let rig = |name: &str, reply: &'static str, person_key: bool| {
        Parked::new(
            name,
            Answers {
                help: PRESS_HELP,
                turn: vec![reply],
                prompt: Some(Prompt {
                    draft: String::new(),
                    person_key,
                }),
                ..Answers::default()
            },
            settled(),
            3600,
        )
    };
    let keys = |h: &Parked| {
        h.asked
            .lock()
            .map_or(0, |a| a.iter().filter(|l| l.ends_with(" ctrl+u")).count())
    };
    let person = "OK 0 turn skipped reason=person typed=40 pasted=0 submitted=0 seq=78 id=1";
    for (name, reply, word) in [
        (
            "cut-deadline",
            "OK 0 turn skipped reason=deadline typed=300 pasted=0 submitted=0 seq=78 id=1",
            "wait:announce-refused:cut-deadline",
        ),
        (
            "cut-guard",
            "OK 0 turn skipped reason=guard submitted=0 seq=78 id=1",
            "wait:announce-refused:cut-guard",
        ),
        ("cut-person", person, "wait:announce-refused:cut-person"),
    ] {
        let h = rig(name, reply, false);
        assert_eq!(h.visit().step, word, "{name}");
        let st = load(&h.opts, SESSION).expect("the record");
        assert!(
            st.left_typed.starts_with(upgrade::ANNOUNCE_HEAD),
            "{name}: {st:?}"
        );
        assert_eq!(h.visit().step, "wait:left-typed:unsent", "{name}");
        assert_eq!(keys(&h), 0, "{name}: nothing pressed, ever");
        assert!(h.details("left-typed-cleared").is_empty(), "{name}");
        assert_eq!(h.typed().len(), 1, "{name}: typed once, never again");
    }
    let h = rig("cut-mixed", person, true);
    let _ = h.visit();
    assert_eq!(h.visit().step, "wait:left-typed:mixed");
    assert_eq!(keys(&h), 0);
    drop(h);
    let h = rig("cut-forgotten", person, false);
    let _ = h.visit();
    let mut st = load(&h.opts, SESSION).expect("the record");
    st.left_typed.clear();
    save(&h.opts, SESSION, &st);
    assert_eq!(h.visit().step, "wait:draft");
    assert_eq!(keys(&h), 0);
}

/// A turn begun since the look takes no notice and no release under the rows fence.
#[cfg(unix)]
#[test]
fn a_turn_begun_since_the_look_takes_no_notice_and_no_release_under_the_rows_fence() {
    let rule = "─".repeat(20);
    let idle = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let busy = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"first":0,"gen":"4.79","human_ms":null}}"#
    );
    let answers = |after: Option<(usize, String)>| Answers {
        screen: idle.clone(),
        after,
        help: ROWS_HELP,
        turn: vec!["OK 0 id=2 submitted=1 status=settled"],
        ..Answers::default()
    };
    let text_reads = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .take_while(|l| !l.contains(" turn "))
            .filter(|l| l.contains(" text "))
            .count()
    };
    let h = Parked::new("rows-turn-idle", answers(None), settled(), 3600);
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(h.typed().len(), 1);
    assert!(h.typed()[0].contains(" if-rows="), "{:?}", h.typed());
    let look = text_reads(&h) - 2;
    drop(h);
    let h = Parked::new(
        "rows-turn-begun",
        answers(Some((look, busy.clone()))),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:busy", "typed: {:?}", h.typed());
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert_eq!(load(&h.opts, SESSION).expect("state").phase, Phase::Pending);
    drop(h);
    let owing = |h: &Parked| {
        let st = St {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            release: "gave-up".to_string(),
            failed_at: now_s(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    };
    let h = Parked::new("rows-release-idle", answers(None), settled(), 3600);
    owing(&h);
    assert_eq!(h.visit().step, "released:gave-up");
    assert_eq!(h.typed().len(), 1);
    let look = text_reads(&h) - 2;
    drop(h);
    let h = Parked::new(
        "rows-release-begun",
        answers(Some((look, busy))),
        settled(),
        3600,
    );
    owing(&h);
    assert_eq!(
        h.visit().step,
        "wait:release:busy",
        "typed: {:?}",
        h.typed()
    );
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert_eq!(
        load(&h.opts, SESSION).expect("state").release,
        "gave-up",
        "still owed"
    );
}

/// The notice is fenced on the generation it was judged on (`if-gen=<gen> yield=0.2` where
/// `help turn` names it).
#[cfg(unix)]
#[test]
fn the_notice_is_fenced_on_the_generation_it_was_judged_on() {
    let rule = "─".repeat(20);
    let fenced_screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let help = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";
    let answers = |turn| Answers {
        screen: fenced_screen.clone(),
        help,
        turn,
        ..Answers::default()
    };
    let h = Parked::new(
        "fence-changed",
        answers(vec![
            "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
        ]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:announce-refused:changed");
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(st.refused, 1, "one look refused");
    assert_ne!(st.refused_at, 0, "since when");
    let typed = h.typed();
    assert_eq!(typed.len(), NOTICE_FENCE_TRIES, "{typed:#?}");
    assert!(
        typed.iter().all(|t| t.contains(&format!(
            " turn if-gen=4.77 yield=0.2 {TURN_WAIT} [aterm harness]"
        ))),
        "{typed:?}"
    );
    let reads = {
        let asked = h.asked.lock().expect("asked").clone();
        asked
            .iter()
            .take_while(|l| !l.contains(" turn "))
            .filter(|l| l.contains(" text "))
            .count()
    };
    assert!(reads >= 2, "the look's read and the fence's: {reads}");
    let first = st.refused_at;
    assert_eq!(h.visit().step, "wait:announce-refused:changed");
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!((st.refused, st.refused_at), (2, first));
    drop(h);
    let busy_screen = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"first":0,"gen":"4.79","human_ms":null}}"#
    );
    for (case, later, word, refused) in [
        (
            "hand",
            fenced_screen.replace(r#""human_ms":null"#, r#""human_ms":40"#),
            "wait:announce-refused:changed",
            1,
        ),
        ("turn", busy_screen, "wait:busy", 0),
    ] {
        let h = Parked::new(
            &format!("fence-retry-{case}"),
            Answers {
                after: Some((reads, later)),
                ..answers(vec![
                    "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
                    "OK 0 id=2 submitted=1 status=settled",
                ])
            },
            settled(),
            3600,
        );
        assert_eq!(h.visit().step, word, "{case}: never typed again");
        assert_eq!(h.typed().len(), 1, "{case}: typed once, never again");
        let st = load(&h.opts, SESSION).expect("the record");
        assert_eq!((st.phase, st.refused), (Phase::Pending, refused), "{case}");
        drop(h);
    }
    let h = Parked::new(
        "fence-retry-repaint",
        Answers {
            after: Some((reads, fenced_screen.replace("4.77", "4.78"))),
            ..answers(vec![
                "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
                "OK 0 id=2 submitted=1 status=settled",
            ])
        },
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(h.typed().len(), 2);
    assert!(
        h.typed()[1].contains(" turn if-gen=4.78 "),
        "{:?}",
        h.typed()
    );
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!(
        (st.refused, st.refused_at),
        (0, 0),
        "typed within the visit: nothing refused"
    );
    drop(h);
    let h = Parked::new(
        "fence-yield",
        answers(vec!["ERR yield timeout momentum=0.84"]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:announce-refused:yield");
    assert_eq!(h.typed().len(), 1);
    drop(h);
    let h = Parked::new(
        "fence-absent",
        Answers {
            screen: fenced_screen.clone(),
            ..Answers::default()
        },
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    let typed = h.typed();
    assert!(!typed[0].contains("if-gen="), "{typed:?}");
}

/// A tab with an idle Claude Code for [`type_fenced`] alone: `text --json` draws an empty
/// composer at one generation, a `cell` reads plain, `help` takes `if-gen=`, the `lease
/// acquire` is answered `lease` (a `lease release` is `OK lease released` after an acquire it
/// granted, `OK lease none` otherwise), and each `turn` the next of `turns`, the last repeating.
#[cfg(unix)]
fn hand_tab(
    lease: &'static str,
    turns: Vec<&'static str>,
) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    use std::io::BufRead as _;
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let server = std::thread::spawn(move || {
        let mut out = theirs.try_clone().expect("clone");
        let mut asked = Vec::new();
        let mut turns = turns.into_iter().peekable();
        let mut last = "ERR no turn scripted";
        for line in std::io::BufReader::new(theirs).lines() {
            let Ok(line) = line else { break };
            let verb = line
                .split_whitespace()
                .find(|w| !w.starts_with('@'))
                .unwrap_or("");
            let reply = match verb {
                "lease" if line.contains(" lease release ") => {
                    if lease.starts_with("OK lease acquired") {
                        "OK lease released".to_string()
                    } else {
                        "OK lease none".to_string()
                    }
                }
                "lease" => lease.to_string(),
                "help" => "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ..."
                    .to_string(),
                "text" => format!("OK {screen}"),
                "cell" => "OK %20 d0d0d0 111318 none".to_string(),
                "turn" => {
                    last = turns.next().unwrap_or(last);
                    last.to_string()
                }
                _ => "ERR unscripted".to_string(),
            };
            asked.push(line);
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
        asked
    });
    (RelayClient::new(ours), server)
}

/// The notice is typed under the harness's own hand (`lease acquire … hard`, [`Hand`]), so the
/// window shows no driver row for it.
#[cfg(unix)]
#[test]
fn the_notice_is_typed_under_the_harnesss_hand_and_a_refused_fence_is_retried() {
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let refused = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let taken = "OK 0 id=2 submitted=1 status=settled";
    let retry = Retry {
        looked: HandAt::Never,
        owner_now: false,
        at_break: false,
        grace_s: 120,
    };
    let typed = |lease: &'static str, turns: Vec<&'static str>| {
        let (mut c, server) = hand_tab(lease, turns);
        let out = type_fenced(
            &mut c,
            TAB,
            "[aterm harness] the notice",
            Some(&retry),
            Put::Pressed,
            Duration::ZERO,
        );
        drop(c);
        (out, server.join().expect("server"))
    };
    let order = |asked: &[String]| -> Vec<&'static str> {
        asked
            .iter()
            .filter_map(|l| {
                if l.contains(" lease acquire ") {
                    Some("acquire")
                } else if l.contains(" lease release ") {
                    Some("release")
                } else if l.contains(" turn ") {
                    Some("turn")
                } else {
                    None
                }
            })
            .collect()
    };
    let (out, asked) = typed(held, vec![refused, taken]);
    assert_eq!(out, Ok(()), "{asked:?}");
    assert_eq!(order(&asked), ["acquire", "turn", "turn", "release"]);
    assert!(
        asked
            .iter()
            .find(|l| l.contains(" lease acquire "))
            .is_some_and(|l| l.ends_with(" hard")),
        "the hard hold: {asked:?}"
    );
    assert!(
        asked
            .iter()
            .filter(|l| l.contains(" turn "))
            .all(|l| l.contains(" if-gen=4.77 ")),
        "each try fenced: {asked:?}"
    );
    let (out, asked) = typed(held, vec![refused]);
    assert_eq!(out, Err(Typed::Changed), "{asked:?}");
    let mut want = vec!["acquire"];
    want.extend(std::iter::repeat_n("turn", NOTICE_FENCE_TRIES));
    want.push("release");
    assert_eq!(order(&asked), want);
    let (out, asked) = typed("ERR busy lease=orchestrator", vec![taken]);
    assert_eq!(out, Err(Typed::Refused("held".to_string())));
    assert_eq!(order(&asked), ["acquire"]);
    let (out, asked) = typed(
        "ERR lease acquire: unknown arg 'hard' (ttl=<ms> holder=<name>)",
        vec![taken],
    );
    assert_eq!(out, Ok(()), "{asked:?}");
    assert_eq!(order(&asked), ["acquire", "turn"]);
}

/// The wait log (the ledger holds only acts): each change of wait and each act is one row with the
/// facts the look decided on.
#[test]
fn the_wait_log_keeps_each_change_of_wait_with_the_facts_that_decided_it() {
    let mut st = St::default();
    st.note_wait("draft", 10);
    st.annotate_wait(
        10,
        "status=idle composer_empty=0 render=ok cursor=56,2 caret=55",
    );
    st.note_wait("draft", 20);
    st.annotate_wait(20, "ignored: not a change");
    st.note_wait("settling", 30);
    st.note_step("announced:1", "idle", 40);
    st.note_step("wait:background", "busy", 50);
    st.note_step("skip:not-selected", "busy", 60);
    st.note_step("wait:background", "busy", 70);
    let words: Vec<(u64, &str)> = st.waits.iter().map(|w| (w.at, w.word.as_str())).collect();
    assert_eq!(
        words,
        [
            (10, "draft"),
            (30, "settling"),
            (40, "announced:1"),
            (50, "background")
        ],
        "a change of wait or an act; a repeat and a skip add nothing"
    );
    assert!(st.waits[0].note.contains("caret=55"), "{:?}", st.waits[0]);
    assert!(st.waits[1].note.is_empty(), "no facts were attached there");
    let back = St::from_json(&st.to_json()).expect("state");
    assert_eq!(back.waits, st.waits, "it survives the state file");
    for i in 0..(WAITS_KEPT as u64 + 5) {
        st.note_wait(&format!("w{i}"), 100 + i);
    }
    assert_eq!(st.waits.len(), WAITS_KEPT, "bounded");
    assert_eq!(st.waits.last().map(|w| w.word.as_str()), Some("w28"));
}

/// A version probe's deadline (300 ms here) bounds a child that keeps running and one that exits
/// while its own child still holds stdout. Each script has run once before its probe
/// ([`crate::harness::stand_in::proven`]), so the deadline meets the script, not macOS's
/// assessment of a new file.
#[cfg(unix)]
#[test]
fn a_version_probe_is_bounded_whole() {
    let mut dir = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("aterm-version-probe-")
            .tempdir()
            .expect("exclusive probe directory")
            .keep(),
    );
    let script = |name: &str, body: &str| {
        let path = dir.path().join(name);
        crate::harness::stand_in::proven(&path, body);
        path
    };
    let parked = aterm_spec::wait::PARKED_SECS;
    let hang = script(
        "hang",
        &format!(
            "printf '%s\n' \"$$\" > \"$0.pid\"\n/bin/sleep {parked} &\nprintf '%s\n' \"$!\" > \"$0.child\"\nwait"
        ),
    );
    let lingers = script(
        "lingers",
        &format!(
            "printf '%s\n' \"$$\" > \"$0.pid\"\n/bin/sleep {parked} &\nprintf '%s\n' \"$!\" > \"$0.child\"\nprintf '%s\n' '2.1.282 (Claude Code)'\nexit 0"
        ),
    );
    let answer = script("answer", "printf '%s\n' '2.1.282 (Claude Code)'");
    let silent = script("silent", "exit 3");
    dir.ran();
    let results = [&hang, &lingers].map(|exe| version_within(exe, Duration::from_millis(300)));
    let mut observed = Vec::new();
    for exe in [&hang, &lingers] {
        for suffix in [".pid", ".child"] {
            let mut path = exe.as_os_str().to_owned();
            path.push(suffix);
            match std::fs::read_to_string(PathBuf::from(path)) {
                Ok(text) => {
                    let Some(line) = text.strip_suffix('\n') else {
                        continue;
                    };
                    let pid = line.parse::<u32>().expect("the script's PID");
                    assert!(pid > 1 && i32::try_from(pid).is_ok(), "{pid}");
                    observed.push(pid);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("unreadable owned PID receipt: {error}"),
            }
        }
    }
    eprintln!("version probe complete PID receipts: {observed:?}");
    let gone = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(20),
        || observed.iter().all(|pid| !alive(*pid)).then_some(()),
    )
    .is_some();
    assert!(gone, "the recorded probe PIDs did not retire: {observed:?}");
    for (name, result) in ["hang", "lingers"].into_iter().zip(results) {
        assert_eq!(result, Probed::TimedOut, "{name}");
    }
    assert_eq!(
        version_within(&answer, Duration::from_secs(60)),
        Probed::Version(Version::parse("2.1.282").expect("v"))
    );
    assert_eq!(
        version_within(&silent, Duration::from_secs(60)),
        Probed::NoVersion
    );
}

/// The version cache remembers a clean answer while the file is unchanged, but a timeout is no
/// verdict on the build.
#[test]
fn a_timed_out_version_probe_is_retried_on_a_back_off() {
    let dir = scratch("version-cache");
    let slow = dir.join("slow");
    std::fs::write(&slow, "#!/bin/sh\n").expect("file");
    let t0 = Instant::now();
    let probes = std::cell::Cell::new(0);
    let timed_out = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::TimedOut
    };
    let first = t0 + VERSION_RETRY_FIRST;
    assert_eq!(cached_version_with(&slow, t0, timed_out), Asked::Unanswered);
    assert_eq!(probes.get(), 1);
    assert_eq!(
        cached_version_with(&slow, first - Duration::from_secs(1), timed_out),
        Asked::Unanswered,
        "unanswered still"
    );
    assert_eq!(probes.get(), 1, "not before the back-off");
    let second = first + 2 * VERSION_RETRY_FIRST;
    assert_eq!(
        cached_version_with(&slow, first, timed_out),
        Asked::Unanswered
    );
    assert_eq!(probes.get(), 2, "asked again");
    assert_eq!(
        cached_version_with(&slow, second - Duration::from_secs(1), timed_out),
        Asked::Unanswered
    );
    assert_eq!(probes.get(), 2, "the back-off doubled");
    let v = Version::parse("2.1.282").expect("v");
    let answers = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::Version(Version::parse("2.1.282").expect("v"))
    };
    assert_eq!(
        cached_version_with(&slow, second, answers),
        Asked::Answered(Some(v.clone()))
    );
    assert_eq!(
        cached_version_with(&slow, second + VERSION_RETRY_MAX, answers),
        Asked::Answered(Some(v))
    );
    assert_eq!(probes.get(), 3, "an answer is not asked again");
    let bare = dir.join("bare");
    std::fs::write(&bare, "#!/bin/sh\n").expect("file");
    let fails = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::NoVersion
    };
    assert_eq!(cached_version_with(&bare, t0, fails), Asked::Answered(None));
    assert_eq!(
        cached_version_with(&bare, t0 + VERSION_RETRY_MAX, fails),
        Asked::Answered(None)
    );
    assert_eq!(probes.get(), 4, "a clean failure is not asked again");
    std::fs::write(&bare, "#!/bin/sh\necho 2.1.283\n").expect("file");
    let _ = cached_version_with(&bare, t0, fails);
    assert_eq!(probes.get(), 5, "a changed file is asked at once");
    assert_eq!(
        cached_version_with(&dir.join("absent"), t0, fails),
        Asked::Answered(None)
    );
    assert_eq!(probes.get(), 5);
    let _ = std::fs::remove_dir_all(dir);
}

/// A draft begun after the restart's own screen check is read again right before the SIGTERM and
/// vetoes it (`wait:changed-before-signal`, still announced).
#[cfg(unix)]
#[test]
fn a_draft_begun_before_the_signal_vetoes_it() {
    let rule = "─".repeat(20);
    let draft = format!(
        r#"{{"rows":["{rule}","❯ wait, one more thing","{rule}"],"cursor":{{"row":1,"col":22}},"seq":79,"human_ms":null,"first":0}}"#
    );
    for (case, clean_reads, expect) in [
        ("before-signal", 2, "wait:changed-before-signal"),
        ("first-look", 1, "wait:changed"),
    ] {
        let answers = Answers {
            after: Some((clean_reads, draft.clone())),
            ..Answers::default()
        };
        let h = Parked::new(&format!("presignal-{case}"), answers, St::default(), 3600);
        let marker = "ATERM-UPGRADE-READY-0badf00d";
        let project = h.opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!(
                r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
            ) + "\n",
        )
        .expect("transcript");
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - 60,
                asks: 1,
            },
            marker: marker.to_string(),
            tab: TAB.to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..unsettled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        assert_eq!(h.visit().step, expect, "{case}");
        assert!(alive(h.sf.pid), "{case}: never signalled");
        assert!(
            matches!(
                load(&h.opts, SESSION).map(|s| s.phase),
                Some(Phase::Announced { .. })
            ),
            "{case}: still announced"
        );
    }
}

/// An idle Claude's screen as a host with the generation fence sends it (a
/// `gen` beside the `seq`), and that host's `help turn` naming the fence.
fn fenced_answers(turn: Vec<&'static str>) -> Answers {
    let rule = "─".repeat(20);
    Answers {
        screen: format!(
            r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        ),
        help: "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...",
        turn,
        ..Answers::default()
    }
}

/// An announced state for [`SESSION`] in [`TAB`] whose notice the parked
/// agent of `h` holds, `marker` out, its screen just seen to move.
#[cfg(unix)]
fn announced_to(h: &Parked, marker: &str) -> St {
    St {
        phase: Phase::Announced {
            at_s: now_s() - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: h.sf.pid,
        notice_start: squash(&h.sf.proc_start),
        ..unsettled()
    }
}

/// The transcript of [`SESSION`] under `h`'s home, its last assistant row
/// saying `text`.
#[cfg(unix)]
fn transcript_says(h: &Parked, text: &str) {
    let project = h.opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
}

/// A continuation on a screen that keeps redrawing is typed unfenced after its fenced tries (still
/// checked for an empty composer).
#[cfg(unix)]
#[test]
fn a_continuation_on_a_screen_that_keeps_moving_is_still_typed() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let moved = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![moved; CONTINUE_FENCE_TRIES];
    replies.push("OK 0 id=2 submitted=1 status=settled");
    let rig = Rig::with("continue-moving", &before, fenced_answers(replies));
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), CONTINUE_FENCE_TRIES + 1, "{typed:#?}");
    assert!(
        typed[..CONTINUE_FENCE_TRIES]
            .iter()
            .all(|t| t.contains(" turn if-gen=4.77 yield=0.2 ")),
        "{typed:#?}"
    );
    assert!(
        !typed[CONTINUE_FENCE_TRIES].contains("if-gen="),
        "{typed:#?}"
    );
    drop(rig);
    let rig = Rig::with(
        "continue-refused",
        &before,
        fenced_answers(vec!["ERR denied"]),
    );
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:no-continue", "{r:?}");
    assert_eq!(rig.typed().len(), 1);
}

/// A fenced relaunch line that parks past the turn's timeout on a person's typing
/// (`ERR yield timeout`) waits `yield`.
#[cfg(unix)]
#[test]
fn a_yield_that_times_out_never_fails_the_relaunch() {
    let dir = scratch("relaunch-yield");
    let yielded = "ERR yield timeout momentum=0.84";
    let (sock, asked) = instance_with(
        &dir,
        fenced_answers(vec![yielded, "OK 0 id=2 submitted=1 status=settled"]),
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let line = "claude --resume";
    assert_eq!(
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true
        ),
        Err(RelaunchLineError::Wait("yield"))
    );
    let typed: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        typed.len(),
        1,
        "one fenced try, nothing unfenced: {typed:#?}"
    );
    assert!(typed[0].contains("if-gen="), "{typed:#?}");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
    let dir = scratch("relaunch-refused");
    let (sock, asked) = instance_with(&dir, fenced_answers(vec!["ERR denied"]));
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    assert!(matches!(
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true
        ),
        Err(RelaunchLineError::Turn(_))
    ));
    assert_eq!(turns(&asked), 1);
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the shell's dialect known, the relaunch attempt asks the window for a typed re-key
/// (`rekey shell=<pid>`) and types the healed line.
#[cfg(unix)]
#[test]
fn the_relaunch_line_carries_a_rekey_and_never_waits_on_one() {
    const KEY: &str = "/c/aterm/rekey/s-0a.1";
    const ISSUED: &str = "OK rekey ttl=120 path=/c/aterm/rekey/s-0a.1";
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let line = " '/pkg/claude' '--resume' 'x'";
    let run = |name: &str, rekey: &'static str, turn: &'static str, dialect, line: &str| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                rekey,
                ..fenced_answers(vec![turn])
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        let got = type_relaunch_line_with(
            &opts,
            &mut c,
            4242,
            TAB,
            line,
            dialect,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            || true,
            |_, _, _| true,
        );
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, asked)
    };
    let of = |asked: &[String], word: &str| -> Vec<String> {
        asked.iter().filter(|l| l.contains(word)).cloned().collect()
    };
    let healed = upgrade::with_rekey(Dialect::Zsh, line, Path::new(KEY)).expect("fits");
    assert!(healed.contains(&format!("<'{KEY}';")) && healed.ends_with(&line[1..]));

    let (got, asked) = run("rekey-issued", ISSUED, SETTLED, Some(Dialect::Zsh), line);
    assert_eq!(got, Ok(()));
    assert_eq!(of(&asked, " rekey "), [format!("@{TAB} rekey shell=4242")]);
    let typed = of(&asked, " turn ");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(typed[0].ends_with(&healed), "{typed:#?}");

    for (name, reply) in [
        (
            "rekey-healthy",
            "ERR rekey integration=on (a working key is never moved)",
        ),
        ("rekey-older", "ERR unknown verb (try: help)"),
    ] {
        let (got, asked) = run(name, reply, SETTLED, Some(Dialect::Zsh), line);
        assert_eq!(got, Ok(()), "{name}");
        let typed = of(&asked, " turn ");
        assert!(
            typed[0].ends_with(line) && !typed[0].contains("__aterm_shell_nonce"),
            "{name}: {typed:#?}"
        );
        assert!(of(&asked, "rekey withdraw").is_empty(), "{name}");
    }

    let (got, asked) = run("rekey-no-dialect", ISSUED, SETTLED, None, line);
    assert_eq!(got, Ok(()));
    assert!(of(&asked, " rekey ").is_empty(), "{asked:#?}");

    let long = format!(" '/pkg/claude' '{}'", "p".repeat(upgrade::MAX_LINE - 20));
    let (got, asked) = run("rekey-too-long", ISSUED, SETTLED, Some(Dialect::Zsh), &long);
    assert_eq!(got, Ok(()));
    assert_eq!(of(&asked, "rekey withdraw").len(), 1, "{asked:#?}");
    assert!(of(&asked, " turn ")[0].ends_with(&long));

    for (name, rekey, withdrawn) in [
        ("rekey-yield", ISSUED, 1),
        (
            "rekey-yield-none",
            "ERR rekey integration=off (nothing to heal)",
            0,
        ),
    ] {
        let yielded = "ERR yield timeout momentum=0.84";
        let (got, asked) = run(name, rekey, yielded, Some(Dialect::Zsh), line);
        assert_eq!(got, Err(RelaunchLineError::Wait("yield")), "{name}");
        let withdraws: Vec<usize> = asked
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("rekey withdraw"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(withdraws.len(), withdrawn, "{name}: {asked:#?}");
        if let Some(&at) = withdraws.first() {
            let last_turn = asked
                .iter()
                .rposition(|l| l.contains(" turn "))
                .expect("a try");
            assert!(at > last_turn, "{name}: taken back after the last try");
        }
    }
}

/// At bash the dialect is read off the live shell ([`shell_dialect`]) and the healed line is typed
/// (`send`, never pasted).
#[cfg(unix)]
#[test]
fn a_relaunch_typed_at_bash_takes_the_key_its_live_shell_names() {
    const ISSUED: &str = "OK rekey ttl=120 path=/c/aterm/rekey/s-0a.1";
    let line = " '/pkg/claude' '--resume' 'x'";
    let mut bash = stand_in_shell();
    let mut dialect = None;
    for _ in 0..6_000 {
        dialect = shell_dialect(bash.id(), &table());
        if dialect == Some(Dialect::Bash) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(dialect, Some(Dialect::Bash), "the live shell names bash");
    let healed =
        upgrade::with_rekey(Dialect::Bash, line, Path::new("/c/aterm/rekey/s-0a.1")).expect("fits");
    for (name, rekey, want) in [
        ("rekey-bash", ISSUED, healed.as_str()),
        (
            "rekey-bash-refused",
            "ERR rekey integration=on (a working key is never moved)",
            line,
        ),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                rekey,
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            line,
            dialect,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true,
        )
        .expect("typed at bash");
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        assert!(
            asked
                .iter()
                .any(|r| r == &format!("@{TAB} rekey shell={}", bash.id())),
            "{name}: {asked:#?}"
        );
        assert!(
            asked
                .iter()
                .any(|r| r.ends_with(&format!("send -- {want}"))),
            "{name}: {asked:#?}"
        );
        assert_eq!(turns_in(&asked), 0, "{name}: never a paste");
        assert!(
            !asked.iter().any(|r| r.ends_with(" rekey withdraw")),
            "{name}: {asked:#?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = bash.kill();
    let _ = bash.wait();
}

/// The requests of `asked` that paste into the tab.
#[cfg(unix)]
fn turns_in(asked: &[String]) -> usize {
    asked.iter().filter(|l| l.contains(" turn ")).count()
}

/// The relaunch line is never typed onto a person's command (the fence proves only that the screen
/// held still, not that the input line is empty).
#[cfg(unix)]
#[test]
fn the_relaunch_line_waits_for_a_person_typing_at_the_prompt() {
    let (prompt, help) = (prompt_screen, FENCED_HELP);
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let run = |name: &str, answers: Answers, mark: &mut Option<PromptMark>| {
        relaunch_attempt(name, answers, mark, |_, _, _| true)
    };
    let mut mark = None;
    let (got, typed) = run(
        "relaunch-typing",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            after: Some((1, prompt("~/proj % rm -rf build", 21, 78))),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "nothing typed onto theirs: {typed:#?}");
    let (got, typed) = run(
        "relaunch-still-typed",
        Answers {
            screen: prompt("~/proj % rm -rf build", 21, 78),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "a later attempt types nothing either");
    let (got, typed) = run(
        "relaunch-deleted",
        Answers {
            screen: prompt("~/proj % ", 9, 79),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
    let st = St {
        prompt: Some(PromptMark {
            row: 3,
            col: 9,
            left: "a, b % ".to_string(),
            right: Some(40),
        }),
        ..St::default()
    };
    assert_eq!(
        St::from_json(&st.to_json()).map(|s| s.prompt),
        Some(st.prompt)
    );
    let (got, typed) = run(
        "relaunch-still",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(" turn if-gen=4.77 yield=0.2 "),
        "{typed:#?}"
    );
    let mut mark = None;
    let (got, typed) = run(
        "relaunch-redrawn",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            after: Some((1, prompt("~/proj (main) % ", 16, 78))),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("prompt-moved")));
    assert!(typed.is_empty(), "{typed:#?}");
    assert_eq!(mark, None, "the moved prompt's mark is dropped");
    let (got, typed) = run(
        "relaunch-redrawn-next",
        Answers {
            screen: prompt("~/proj (main) % ", 16, 78),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Ok(()), "marked by the next attempt, then typed");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(typed[0].contains(" turn if-gen=4.78 "), "{typed:#?}");
    let moved = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![moved; RELAUNCH_FENCE_TRIES];
    replies.push(settled);
    let (got, typed) = run(
        "relaunch-ticking",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            help,
            turn: replies,
            ..Answers::default()
        },
        &mut None,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), RELAUNCH_FENCE_TRIES + 1, "{typed:#?}");
    assert!(
        !typed[RELAUNCH_FENCE_TRIES].contains("if-gen="),
        "{typed:#?}"
    );
}

/// A person's typing that wraps is typing too.
#[test]
fn typing_that_wraps_at_the_bottom_of_the_screen_is_still_typing() {
    let at = |rows: &[&str], row: usize, col: usize| Screen {
        rows: rows.iter().map(|r| (*r).to_string()).collect(),
        cursor: Some((row, col)),
        seq: 1,
        first: 0,
        generation: None,
        human: HumanInput::Never,
    };
    let mark = PromptMark::of(&at(&["out", "~/p % "], 1, 6)).expect("mark");
    assert_eq!(mark.at(&at(&["out", "~/p % "], 1, 6)), AtPrompt::Same);
    assert_eq!(mark.at(&at(&["out", "~/p % ls"], 1, 8)), AtPrompt::Typed);
    assert_eq!(
        mark.at(&at(&["~/p % a-very-long-comm", "and"], 1, 3)),
        AtPrompt::Typed,
        "wrapped and scrolled"
    );
    assert_eq!(
        mark.at(&at(&["out", "~/p (main) % "], 1, 13)),
        AtPrompt::Moved,
        "an async redraw of the prompt"
    );
}

/// A shell prompt's one-row `text --json`: `text`, the cursor at `col`, the
/// content sequence (and generation `4.<seq>`) `seq`.
#[cfg(unix)]
fn prompt_screen(text: &str, col: usize, seq: u64) -> String {
    format!(
        r#"{{"rows":["{text}"],"cursor":{{"row":0,"col":{col}}},"seq":{seq},"first":0,"gen":"4.{seq}"}}"#
    )
}

/// One relaunch attempt ([`type_relaunch_line`]) against a stand-in instance
/// answering `answers`, the shell's tab claim read by `probe`: what it
/// returned, and every `turn` it sent.
#[cfg(unix)]
fn relaunch_attempt(
    name: &str,
    answers: Answers,
    mark: &mut Option<PromptMark>,
    probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> (Result<(), RelaunchLineError>, Vec<String>) {
    relaunch_attempt_ended(name, answers, mark, 0, probe)
}

/// [`relaunch_attempt`] for an agent whose end was asked at `ended_at_s`
/// (Unix seconds, [`PromptLook::ended_at_s`]).
#[cfg(unix)]
fn relaunch_attempt_ended(
    name: &str,
    answers: Answers,
    mark: &mut Option<PromptMark>,
    ended_at_s: u64,
    probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> (Result<(), RelaunchLineError>, Vec<String>) {
    relaunch_attempt_held(name, answers, mark, ended_at_s, 0, probe)
}

/// [`relaunch_attempt_ended`] for a line a hand held since `held_since_s`
/// (Unix seconds, [`PromptLook::held_since_s`]; `0`, none).
#[cfg(unix)]
fn relaunch_attempt_held(
    name: &str,
    answers: Answers,
    mark: &mut Option<PromptMark>,
    ended_at_s: u64,
    held_since_s: u64,
    probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> (Result<(), RelaunchLineError>, Vec<String>) {
    let dir = scratch(name);
    let (sock, asked) = instance_with(&dir, answers);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let got = type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        std::process::id(),
        TAB,
        SESSION,
        " '/pkg/claude' '--resume' 'x'",
        None,
        PromptLook {
            mark,
            ended_at_s,
            held_since_s,
        },
        probe,
    );
    drop(c);
    let typed: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (got, typed)
}

/// The `help turn` reply of a host whose `turn` takes the fence.
#[cfg(unix)]
const FENCED_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";

/// At a bash prompt (typed to with `send`, which takes no `if-gen=` fence) the prompt is read
/// against its mark once more right before the keys.
#[cfg(unix)]
#[test]
fn a_bash_prompt_with_typing_after_its_mark_is_never_typed_at() {
    let mut bash = stand_in_shell();
    for _ in 0..6_000 {
        if atpkg::caller_shell::process_args(bash.id())
            .is_some_and(|a| Dialect::from_exe_name(&a.exec_path) == Some(Dialect::Bash))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let attempt = |name: &str, screen: String| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen,
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut mark = Some(PromptMark {
            row: 0,
            col: 9,
            left: "~/proj % ".to_string(),
            right: None,
        });
        let got = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            "claude --resume x",
            None,
            PromptLook {
                mark: &mut mark,
                ended_at_s: 0,
                held_since_s: 0,
            },
            |_, _, _| true,
        );
        drop(c);
        let sent = asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" send ")).count())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, sent)
    };
    let (got, sent) = attempt("bash-typing", prompt_screen("~/proj % ls", 11, 78));
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert_eq!(sent, 0, "nothing sent over a person's typing");
    let (got, sent) = attempt("bash-clear", prompt_screen("~/proj % ", 9, 78));
    assert_eq!(got, Ok(()), "negative control: the bare prompt is typed at");
    assert_eq!(sent, 1);
    let _ = bash.kill();
    let _ = bash.wait();
}

/// The relaunch line is never typed into a program a person started at the returned prompt during
/// the attempt's tries (a fenced turn can park until its timeout).
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_never_typed_into_a_program_started_at_the_prompt() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let program = r#"{"rows":["hello","world","~"],"cursor":{"row":1,"col":0},"seq":90,"first":0,"gen":"5.90"}"#;
    let mut calls = 0;
    let mut mark = None;
    let (got, typed) = relaunch_attempt(
        "relaunch-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            after: Some((1, program.to_string())),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
        |_, _, _| {
            calls += 1;
            calls <= 2
        },
    );
    assert!(matches!(got, Err(RelaunchLineError::Wait(_))), "{got:?}");
    assert!(
        typed.is_empty(),
        "nothing typed into the program: {typed:#?}"
    );
    assert_eq!(mark, None, "a later attempt marks afresh");
    let mut calls = 0;
    let (got, typed) = relaunch_attempt(
        "relaunch-silent-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
        |_, _, _| {
            calls += 1;
            calls <= 2
        },
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("tab-ownership-changed")));
    assert!(typed.is_empty(), "{typed:#?}");
    let skipped = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![skipped; RELAUNCH_FENCE_TRIES];
    replies.push(settled);
    let mut calls = 0;
    let (got, typed) = relaunch_attempt(
        "relaunch-fallback-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: replies,
            ..Answers::default()
        },
        &mut None,
        |_, _, _| {
            calls += 1;
            calls <= 2 + RELAUNCH_FENCE_TRIES
        },
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("tab-ownership-changed")));
    assert_eq!(typed.len(), RELAUNCH_FENCE_TRIES, "{typed:#?}");
    assert!(typed.iter().all(|t| t.contains("if-gen=")), "{typed:#?}");
    let (got, typed) = relaunch_attempt(
        "relaunch-shell-holds",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
        |_, _, _| true,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
}

/// A caret moved home over a half-typed command is typing (a pure cursor move changes no content,
/// so the fence passes it).
#[cfg(unix)]
#[test]
fn a_caret_moved_home_over_a_half_typed_command_is_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let mut mark = Some(PromptMark {
        row: 0,
        col: 9,
        left: "~/proj % ".to_string(),
        right: None,
    });
    let (got, typed) = relaunch_attempt(
        "relaunch-caret-home",
        Answers {
            screen: prompt_screen("~/proj % rm -rf build", 9, 78),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
        |_, _, _| true,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "never typed in front of it: {typed:#?}");
    let at = |row: &str, col: usize| Screen {
        rows: vec![row.to_string()],
        cursor: Some((0, col)),
        seq: 1,
        first: 0,
        generation: None,
        human: HumanInput::Never,
    };
    let clock = PromptMark::of(&at("~/proj %            12:34:56", 9)).expect("mark");
    assert_eq!(clock.right, Some(20));
    assert_eq!(
        clock.at(&at("~/proj %            12:34:57", 9)),
        AtPrompt::Same,
        "the clock ticked in place"
    );
    assert_eq!(
        clock.at(&at("~/proj % rm         12:34:57", 9)),
        AtPrompt::Typed,
        "a caret home over `rm`"
    );
    assert_eq!(
        clock.at(&at("~/proj %  rm        12:34:57", 9)),
        AtPrompt::Typed,
        "a leading blank hides nothing"
    );
    let bare = PromptMark::of(&at("~/proj %", 9)).expect("mark");
    assert_eq!(bare.right, None);
    assert_eq!(bare.at(&at("~/proj %", 9)), AtPrompt::Same);
    assert_eq!(bare.at(&at("~/proj % x", 9)), AtPrompt::Typed);
    assert_eq!(PromptMark::parse(&clock.word()), Some(clock));
    assert_eq!(PromptMark::parse("3,9,a, b % "), None);
}

/// Typeahead before the first read is never the mark (marking where the cursor sits would append
/// the line to it).
#[cfg(unix)]
#[test]
fn typeahead_before_the_first_read_is_never_the_mark() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let entering = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":9,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
    let attempt = |name: &str, screen: String, line: &'static str| {
        let mut mark = None;
        let (got, typed) = relaunch_attempt(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks: entering,
                line,
                ..Answers::default()
            },
            &mut mark,
            |_, _, _| true,
        );
        (got, typed, mark)
    };
    let (got, typed, mark) = attempt(
        "typeahead-home",
        prompt_screen("~/proj % rm -rf build", 9, 77),
        "OK ~/proj % rm -rf build",
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty() && mark.is_none(), "{typed:#?} {mark:?}");
    let (got, typed, mark) = attempt("clean-133", prompt_screen("~/proj %", 9, 77), "OK ~/proj %");
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(mark.is_some());
}

/// A wide prompt glyph is two cells to the prompt read.
#[cfg(unix)]
#[test]
fn a_wide_prompt_glyph_never_hides_a_persons_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let entering = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":5,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
    let attempt = |name: &str, screens: (String, Option<String>), marked: Option<&'static str>| {
        let (blocks, line) = marked.map_or(("ERR unscripted", "ERR unscripted"), |l| (entering, l));
        let mut mark = None;
        let (got, typed) = relaunch_attempt(
            name,
            Answers {
                screen: screens.0,
                after: screens.1.map(|s| (1, s)),
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            |_, _, _| true,
        );
        (got, typed.len(), mark.map(|m| (m.col, m.left)))
    };
    let typing = (Err(RelaunchLineError::Wait("typing")), 0, None);
    for (name, row, col) in [("wide-emoji", "🦀 % x", 6), ("wide-cjk", "~/文档 % ls", 11)] {
        let screen = prompt_screen(row, col, 77);
        assert_eq!(attempt(name, (screen, None), None), typing, "{name}");
    }
    let home = prompt_screen("🦀 % x", 5, 77);
    assert_eq!(
        attempt("wide-home", (home, None), Some("OK 🦀 % x")),
        typing,
        "a caret home onto `x`"
    );
    let (got, typed, _) = attempt(
        "wide-home-after-mark",
        (
            prompt_screen("🦀 %", 5, 77),
            Some(prompt_screen("🦀 % x", 5, 78)),
        ),
        Some("OK 🦀 %"),
    );
    assert_eq!(
        (got, typed),
        (Err(RelaunchLineError::Wait("typing")), 0),
        "`x` typed under the caret after the bare prompt's mark"
    );
    let (got, typed, _) = attempt(
        "wide-right-home-after-mark",
        (
            prompt_screen("🦀🦀🦀 %   ok", 9, 77),
            Some(prompt_screen("🦀🦀🦀 % x ok", 9, 78)),
        ),
        None,
    );
    assert_eq!(
        (got, typed),
        (Err(RelaunchLineError::Wait("typing")), 0),
        "`x` typed under the caret before a right prompt"
    );
    for (marked, row, col, left) in [
        (None, "🦀 %", 5, "🦀 %"),
        (Some("OK 🦀 %"), "🦀 %", 5, "🦀 %"),
        (None, "🦀🦀🦀 %   ok", 9, "🦀🦀🦀 % "),
        (None, "% \u{200B}", 2, "% \u{200B}"),
    ] {
        assert_eq!(
            attempt("wide-clean", (prompt_screen(row, col, 77), None), marked),
            (Ok(()), 1, Some((col, left.to_string()))),
            "{row:?} {marked:?}"
        );
    }
}

/// A line a hand held is never typed into a shell a person used meanwhile.
#[cfg(unix)]
#[test]
fn a_line_a_hand_held_is_never_typed_into_a_shell_used_meanwhile() {
    use super::super::relaunch::hand_held_since;
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let entering = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":9,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
    let exited = now_s() - 3600;
    let since = r#","human_ms":120000"#;
    let before = r#","human_ms":7200000"#;
    let attempt = |name: &str, held: u64, human: &str, integrated: bool| {
        let screen = format!(
            r#"{{"rows":["~/proj % "],"cursor":{{"row":0,"col":9}},"seq":77,"first":0,"gen":"4.77"{human}}}"#
        );
        let (blocks, line) = if integrated {
            (entering, "OK ~/proj % ")
        } else {
            ("ERR unscripted", "ERR unscripted")
        };
        let mut mark = None;
        let (got, typed) = relaunch_attempt_held(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            exited,
            held,
            |_, _, _| true,
        );
        (got, typed.len(), mark.is_some())
    };
    let waits = || Err(RelaunchLineError::Wait("keyed-while-held"));
    for integrated in [true, false] {
        for (name, human) in [
            ("held-keyed", since),
            ("held-never", r#","human_ms":null"#),
            (
                "held-never-since-after",
                r#","human_ms":null,"unkeyed_ms":60000"#,
            ),
            ("held-unstamped", ""),
        ] {
            let name = format!("{name}-{integrated}");
            assert_eq!(
                attempt(&name, exited, human, integrated),
                (waits(), 0, false),
                "{name}"
            );
        }
        for (name, held, human) in [
            ("unheld-keyed", 0, since),
            ("held-quiet", exited, before),
            (
                "held-never-since-before",
                exited,
                r#","human_ms":null,"unkeyed_ms":7200000"#,
            ),
        ] {
            let name = format!("{name}-{integrated}");
            assert_eq!(
                attempt(&name, held, human, integrated),
                (Ok(()), 1, true),
                "{name}"
            );
        }
    }
    let mut st = St {
        phase: Phase::Exiting { at_s: exited },
        cause: String::new(),
        hand_seen_s: exited + 60,
        ..St::default()
    };
    assert_eq!(hand_held_since(&st), exited);
    let look = PromptLook::of(&mut st);
    assert_eq!((look.ended_at_s, look.held_since_s), (exited, exited));
    let unheld = St {
        hand_seen_s: 0,
        ..st.clone()
    };
    assert_eq!(hand_held_since(&unheld), 0, "no hand seen");
    let typed = St {
        phase: Phase::Relaunched { at_s: exited + 120 },
        ..st.clone()
    };
    assert_eq!(hand_held_since(&typed), 0, "past its line");
}

/// Text left on the prompt line by a driver under its lease holds the relaunch line.
#[cfg(unix)]
#[test]
fn what_a_driver_left_on_the_line_under_its_lease_holds_the_relaunch_line() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let exited = now_s() - 3600;
    let driven = r#","human_ms":null,"unkeyed_ms":7200000"#;
    let entering = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":9,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
    let attempt = |name: &str, left: bool, integrated: bool, shell_leads: bool| {
        let (row, line) = if left {
            ("~/proj % make test", "OK ~/proj % make test")
        } else {
            ("~/proj % ", "OK ~/proj % ")
        };
        let col = row.chars().count();
        let screen = format!(
            r#"{{"rows":[{row:?}],"cursor":{{"row":0,"col":{col}}},"seq":77,"first":0,"gen":"4.77"{driven}}}"#
        );
        let (blocks, line) = if integrated {
            (entering, line)
        } else {
            ("ERR unscripted", "ERR unscripted")
        };
        let mut mark = None;
        let (got, typed) = relaunch_attempt_held(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            exited,
            exited,
            move |_, _, _| shell_leads,
        );
        (got, typed.len(), mark.is_some())
    };
    for integrated in [true, false] {
        let left_typed = if integrated { "typing" } else { "unmarked" };
        assert_eq!(
            attempt(&format!("driver-left-{integrated}"), true, integrated, true),
            (Err(RelaunchLineError::Wait(left_typed)), 0, false),
            "a line the driver typed and did not send, integrated={integrated}"
        );
        assert_eq!(
            attempt(
                &format!("driver-runs-{integrated}"),
                false,
                integrated,
                false
            ),
            (
                Err(RelaunchLineError::Wait("tab-ownership-changed")),
                0,
                false
            ),
            "a command the driver left running, integrated={integrated}"
        );
        assert_eq!(
            attempt(
                &format!("driver-clean-{integrated}"),
                false,
                integrated,
                true
            ),
            (Ok(()), 1, true),
            "a clean prompt, integrated={integrated}"
        );
    }
}

/// An `OSC 133;B` mark on another row does not count as user typing when `human_ms` has not
/// advanced.
#[cfg(unix)]
#[test]
fn a_mark_that_names_another_row_is_no_persons_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    const PROMPT: &str = "me@a-studio-of-128gb aterm %";
    let col = PROMPT.len() + 1;
    let screen = |rows: &[&str], cursor: (usize, usize), human: &str| {
        let rows: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        format!(
            r#"{{"rows":[{}],"cursor":{{"row":{},"col":{}}},"seq":77,"first":0,"gen":"4.77"{human}}}"#,
            rows.join(","),
            cursor.0,
            cursor.1
        )
    };
    let returned = [
        " '--resume' '253a40df-d85e-420b-85f5-0e322e9e663b'",
        "Resume this session with:",
        "claude --resume 253a40df-d85e-420b-85f5-0e322e9e663b",
        PROMPT,
    ];
    let blocks = r#"OK {"blocks":[{"id":0,"state":"entering","exit":null,"prompt":69,"cmd":69,"cmdcol":29,"out":null,"end":null,"cwd":"/Users//me/aterm","cmdline":""}]}"#;
    let above = "OK claude --resume 253a40df-d85e-420b-85f5-0e322e9e663b";
    assert_eq!(col, 29, "the fixture's mark column is the prompt's length");
    let ended = now_s() - 3;
    let attempt = |name: &str, screen: String, line: &'static str, ended_at_s: u64| {
        let mut mark = None;
        let (got, typed) = relaunch_attempt_ended(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            ended_at_s,
            |_, _, _| true,
        );
        (got, typed, mark)
    };
    let quiet = r#","human_ms":70000000,"human_seq":51"#;
    let (got, typed, mark) = attempt(
        "mark-elsewhere-quiet",
        screen(&returned, (3, col), quiet),
        above,
        ended,
    );
    assert_eq!(got, Ok(()), "the prompt nobody touched is typed at");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert_eq!(mark.map(|m| (m.row, m.col)), Some((3, col)));
    let (got, typed, mark) = attempt(
        "mark-elsewhere-end-unknown",
        screen(&returned, (3, col), quiet),
        above,
        0,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("mark-elsewhere")));
    assert!(typed.is_empty() && mark.is_none(), "{typed:#?}");
    let wrapped = [
        "me@a-studio-of-128gb aterm % git commit -am 'fix the relaunch: a mark that na",
        "mes another row",
    ];
    let (got, typed, _) = attempt(
        "mark-elsewhere-wrapped",
        screen(&wrapped, (1, 16), r#","human_ms":800"#),
        "OK me@a-studio-of-128gb aterm % git commit -am 'fix the relaunch: a mark that na",
        ended,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("mark-elsewhere")));
    assert!(typed.is_empty(), "never typed onto theirs: {typed:#?}");
}

/// Conformance test of `RelaunchPromptRead` (`aterm_models_harness::relaunch_prompt_read_model`).
#[cfg(unix)]
#[test]
fn the_real_prompt_read_conforms_to_the_model() {
    let model = aterm_models_harness::relaunch_prompt_read_model();
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let ended = now_s() - 3;
    let mut typed_worlds = 0;
    let mut waited_worlds = 0;
    for mark in 0..=2i64 {
        for stamped in 0..=1i64 {
            for blank in 0..=1i64 {
                for person in 0..=1i64 {
                    let (row, col) = match (person, blank) {
                        (0, 1) => ("me@box proj %", 14),
                        (0, _) => ("[me@box proj]$", 14),
                        (_, 1) => ("me@box proj % ls", 17),
                        _ => ("me@box proj % ls", 16),
                    };
                    let humans: &[&str] = match (stamped, person) {
                        (0, _) => &["", r#","human_ms":null"#],
                        (_, 0) => &[r#","human_ms":60000"#],
                        _ => &[r#","human_ms":300"#],
                    };
                    let entering: &'static str = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":14,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
                    let running: &'static str = r#"OK {"blocks":[{"id":3,"state":"running","exit":null,"prompt":9,"cmd":9,"cmdcol":14,"out":10,"end":null,"cwd":"","cmdline":"claude; read"}]}"#;
                    let marks: Vec<(&'static str, String)> = match mark {
                        0 => vec![
                            ("ERR unscripted", "ERR unscripted".to_string()),
                            (running, "OK me@box proj % claude; read".to_string()),
                        ],
                        1 => vec![(entering, format!("OK {row}"))],
                        _ => vec![(entering, "OK claude --resume 253a40df".to_string())],
                    };
                    for (spelling, (human, (blocks, line))) in humans
                        .iter()
                        .flat_map(|h| marks.iter().map(move |m| (h, m)))
                        .enumerate()
                    {
                        let screen = format!(
                            r#"{{"rows":["{row}"],"cursor":{{"row":0,"col":{col}}},"seq":77,"first":0,"gen":"4.77"{human}}}"#
                        );
                        let line: &'static str = Box::leak(line.clone().into_boxed_str());
                        let name = format!("prompt-read-{mark}{stamped}{blank}{person}-{spelling}");
                        let (got, typed) = relaunch_attempt_ended(
                            &name,
                            Answers {
                                screen,
                                help: FENCED_HELP,
                                turn: vec![settled],
                                blocks,
                                line,
                                ..Answers::default()
                            },
                            &mut None,
                            ended,
                            |_, _, _| true,
                        );
                        let real = got.is_ok() && typed.len() == 1;
                        assert!(
                            got.is_ok() || typed.is_empty(),
                            "{name}: a wait types nothing: {got:?} {typed:#?}"
                        );
                        let mut state = model.init_state();
                        for (var, value) in [
                            ("phase", 3),
                            ("mark", mark),
                            ("stamped", stamped),
                            ("blank", blank),
                            ("person", person),
                        ] {
                            state.insert(var, value);
                        }
                        let lane: Vec<&str> =
                            ["Read", "WaitTyping", "WaitElsewhere", "WaitUnmarked"]
                                .into_iter()
                                .filter(|a| model.action_enabled(a, &state))
                                .collect();
                        let made = match &got {
                            Ok(()) => "Read",
                            Err(RelaunchLineError::Wait("typing")) => "WaitTyping",
                            Err(RelaunchLineError::Wait("mark-elsewhere")) => "WaitElsewhere",
                            Err(RelaunchLineError::Wait("unmarked")) => "WaitUnmarked",
                            other => panic!("{name}: a read the model has no move for: {other:?}"),
                        };
                        assert_eq!(
                            lane,
                            [made],
                            "{name}: the real read ({got:?}) against the model's lane at {state:?}"
                        );
                        let modelled = model.action_enabled("Read", &state);
                        assert_eq!(real, modelled, "{name}: typed exactly where `Read` is");
                        if real {
                            typed_worlds += 1;
                        } else {
                            waited_worlds += 1;
                        }
                        if (mark, stamped, person) == (2, 1, 0) {
                            assert!(real, "{name}: tab #1's world types");
                            let mut unstamped = state.clone();
                            unstamped.insert("stamped", 0);
                            assert!(
                                !model.action_enabled("Read", &unstamped),
                                "{name}: a projection blind to the stamp would call the real \
                             verdict wrong here — the bind reads the stamp"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(
        typed_worlds > 0 && waited_worlds > 0,
        "{typed_worlds} {waited_worlds}"
    );
}

/// `relaunch::end_asked_at` dates an exit only where its restart ended the agent itself: a cause
/// filed after the agent left, an unknown one, or a line already typed dates nothing.
#[test]
fn only_a_restart_that_ended_its_agent_dates_a_persons_keys() {
    use super::super::relaunch::{
        CAUSE_EXIT, CAUSE_FRESH, CAUSE_HOST, CAUSE_MEMORY, CAUSE_STALL, CAUSE_UPGRADE_FRESH,
        end_asked_at,
    };
    let at = |cause: &str| {
        end_asked_at(&St {
            phase: Phase::Exiting {
                at_s: 1_790_880_325,
            },
            cause: cause.to_string(),
            ..St::default()
        })
    };
    for asked in [
        "",
        CAUSE_UPGRADE_FRESH,
        CAUSE_MEMORY,
        "model:opus",
        "model-back:",
    ] {
        assert_eq!(at(asked), 1_790_880_325, "{asked:?} ended its agent itself");
    }
    for seen in [
        CAUSE_EXIT,
        CAUSE_FRESH,
        CAUSE_STALL,
        CAUSE_HOST,
        "a-cause-added-later",
    ] {
        assert_eq!(at(seen), 0, "{seen:?} was filed after its agent left");
    }
    assert_eq!(
        end_asked_at(&St {
            phase: Phase::Relaunched {
                at_s: 1_790_880_325
            },
            ..St::default()
        }),
        0,
        "no end is asked once the line is typed"
    );
}

/// A `--now` ([`ask`]) waives exactly the settling window and the next visit types the notice.
#[cfg(unix)]
#[test]
fn an_owners_now_moves_a_settling_session_and_the_wait_is_kept_until_then() {
    let dir = scratch("owner-now");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let behind = now_s() - 600;
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now_s(),
        pending_since: behind,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".into())];
    let run = || {
        visit(
            &opts,
            &sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    };

    let first = run();
    let kept = load(&opts, SESSION).expect("state");
    let second = run();
    let still = load(&opts, SESSION).expect("state");
    let typed_before = turns(&asked);
    let row = ask(&opts, TAB, Ask::Now, false).expect("the owner's word lands");
    let moved = run();
    let after = load(&opts, SESSION).expect("state");
    let typed_after = turns(&asked);
    let _ = agent.kill();
    let _ = agent.wait();

    assert_eq!(first.step, "wait:settling");
    assert_eq!(second.step, "wait:settling");
    assert_eq!(typed_before, 0, "no request: the settling window stands");
    assert_eq!(kept.wait, "settling");
    assert!(kept.wait_since >= behind, "{kept:?}");
    assert_eq!(
        still.wait_since, kept.wait_since,
        "the same wait keeps its start"
    );
    assert_eq!(kept.tab, TAB, "a pending upgrade names its tab");
    assert_eq!(kept.behind_since(), behind);
    assert_eq!(row.request, Request::Now);
    assert_eq!(
        row.stall(now_s()),
        None,
        "ten minutes behind is not stalled"
    );
    assert_eq!(
        moved.step, "announced:1",
        "the owner's word waived the window"
    );
    assert_eq!(typed_after, 1);
    assert!(after.wait.is_empty(), "an act clears the kept wait");
    assert_eq!(after.behind_since(), behind, "the age is the session's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ladder on Claude Code, climbed through the real `visit` and its [`ladder_look`].
#[cfg(unix)]
#[test]
fn the_real_look_climbs_the_ladder_on_claude_code() {
    let near = idle_screen().replace(r#""human_ms":null"#, r#""human_ms":60000"#);
    let typing = idle_screen().replace(r#""human_ms":null"#, r#""human_ms":1000"#);
    let case = |name: &str, behind_s: u64, screen: &str, quiet: bool, looks: usize| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen.to_string(),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let now = now_s();
        let st = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            last_seq: if quiet { 77 } else { 76 },
            seq_since_s: if quiet { now - 60 } else { now },
            pending_since: now - behind_s,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
        let shell = dead_pid();
        let table = vec![(shell, 1, "zsh".into())];
        let mut steps = Vec::new();
        for n in 0..looks {
            if n > 0 {
                let mut st = load(&opts, SESSION).expect("state");
                assert!(st.still_since != 0, "{name}: the look began a still run");
                st.still_since -= upgrade::QUIET_S;
                st.still_at -= upgrade::QUIET_S;
                if !quiet {
                    st.last_seq = 76;
                    st.seq_since_s = now_s();
                }
                std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
            }
            let r = visit(
                &opts,
                &sf,
                &table,
                &newer(),
                &Script::new(shell, usize::MAX, None),
            );
            steps.push(r.step);
        }
        let typed = turns(&asked);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (steps, typed)
    };
    let settled = upgrade::RUNG_SETTLED_S + 60;
    assert_eq!(
        case("ladder-c-repaint", settled, &idle_screen(), false, 2),
        (
            vec!["wait:settling".to_string(), "announced:1".to_string()],
            1
        )
    );
    assert_eq!(
        case("ladder-c-prefer", 600, &idle_screen(), false, 2),
        (
            vec!["wait:settling".to_string(), "wait:settling".to_string()],
            0
        )
    );
    assert_eq!(
        case("ladder-c-near", upgrade::RUNG_KEYS_S + 60, &near, true, 1),
        (vec!["announced:1".to_string()], 1)
    );
    assert_eq!(
        case("ladder-c-near-settled", settled, &near, true, 1),
        (vec!["held-back:attended".to_string()], 0)
    );
    assert_eq!(
        case(
            "ladder-c-typing",
            upgrade::RUNG_LAND_S + 60,
            &typing,
            true,
            2
        ),
        (
            vec![
                "held-back:attended".to_string(),
                "wait:attended".to_string()
            ],
            0
        )
    );
}

/// A `--now` waives the settling window only in the tab it named.
#[cfg(unix)]
#[test]
fn the_owners_word_waives_nothing_in_a_tab_it_did_not_name() {
    let dir = scratch("owner-tab");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let settling = |named: &str| St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now_s(),
        request: Request::Now,
        request_tab: named.to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".into())];
    let run = |named: &str| {
        std::fs::write(state_path(&opts, SESSION), settling(named).to_json()).expect("write");
        visit(
            &opts,
            &sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    };
    let elsewhere = run("s-4906566e7ab0a0c15ee3");
    let typed_elsewhere = turns(&asked);
    let here = run(TAB);
    let typed_here = turns(&asked);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(elsewhere.step, "wait:settling", "another tab's word");
    assert_eq!(typed_elsewhere, 0, "nothing typed on it");
    assert_eq!(
        here.step, "announced:1",
        "this tab's word waives the window"
    );
    assert_eq!(typed_here, 1);
}

/// A ready answer a hold (`--defer`, `--skip`) superseded is no consent.
#[cfg(unix)]
#[test]
fn a_ready_answer_the_owners_hold_superseded_is_never_signalled_after_it() {
    use std::os::unix::process::ExitStatusExt as _;
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    for (case, word) in [("defer", Ask::Defer(600)), ("skip", Ask::Skip)] {
        let mut h = Parked::new(
            &format!("hold-superseded-{case}"),
            Answers::default(),
            St::default(),
            3600,
        );
        transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
        let announced = St {
            phase: Phase::Announced {
                at_s: now_s() - 3_600,
                asks: 1,
            },
            to: "9.9.9".to_string(),
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), announced.to_json()).expect("state");
        ask(&h.opts, TAB, word, false).expect("the owner's word lands");
        let held = load(&h.opts, SESSION).expect("state");
        assert_eq!(
            held.phase,
            Phase::Pending,
            "{case}: the hold ends the notice"
        );
        assert!(held.marker.is_empty(), "{case}: its marker with it");
        if case == "defer" {
            let lapsed = St {
                request: Request::DeferUntil(now_s() - 1),
                last_seq: 77,
                seq_since_s: now_s() - 3_600,
                ..held
            };
            std::fs::write(state_path(&h.opts, SESSION), lapsed.to_json()).expect("lapse");
        } else {
            ask(&h.opts, TAB, Ask::Now, false).expect("--now");
        }
        let ended = h.reaped();
        let mut shell = tty_zsh();
        let r = h.visit_under(shell.id());
        let _ = shell.kill();
        let _ = shell.wait();
        assert!(ended.try_recv().is_err(), "{case}: never signalled: {r:?}");
        assert!(h.details("terminated").is_empty(), "{case}: {r:?}");
        assert_eq!(r.step, "announced:1", "{case}: a fresh notice");
        let fresh = load(&h.opts, SESSION).expect("state");
        assert!(
            !fresh.marker.is_empty() && fresh.marker != marker,
            "{case}: a fresh READY is asked for: {fresh:?}"
        );
    }
    let mut h = Parked::new("hold-none", Answers::default(), St::default(), 3600);
    transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
    let announced = St {
        seq_since_s: now_s() - 3_600,
        last_seq: 77,
        ..announced_to(&h, marker)
    };
    std::fs::write(state_path(&h.opts, SESSION), announced.to_json()).expect("state");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// A ready session a person gave input in within the grace (`human_ms`) waits `attended` however
/// long past the re-ask clock.
#[cfg(unix)]
#[test]
fn an_attended_ready_session_is_never_signalled_without_the_owners_now() {
    use std::os::unix::process::ExitStatusExt as _;
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let rig = |name: &str| {
        let h = Parked::new(
            name,
            Answers {
                screen: stamped_screen(Some("30000")),
                ..Answers::default()
            },
            St::default(),
            3600,
        );
        transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - upgrade::REASK_S - 60,
                asks: 1,
            },
            seq_since_s: now_s() - 3_600,
            last_seq: 77,
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h
    };
    let mut h = rig("attended-ready");
    assert_eq!(h.visit().step, "held-back:attended");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let _ = shell.kill();
    let _ = shell.wait();
    assert!(ended.try_recv().is_err(), "never signalled: {r:?}");
    assert_eq!(r.step, "wait:attended");
    assert!(h.details("terminated").is_empty());
    drop(h);
    let mut h = rig("attended-ready-now");
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// A newer target keeps how long the session has been behind and the request word (except a skip,
/// which is of one version) while nothing has begun.
#[test]
fn a_newer_target_keeps_the_age_and_the_owners_word_but_not_a_skip() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Announced { at_s: 5, asks: 1 },
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        request: Request::Now,
        request_tab: TAB.to_string(),
        ..St::default()
    };
    let next = St::for_target(Some(prior.clone()), &from, &to, None, Proc::default(), 900);
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.to, "2.1.283");
    assert_eq!(next.behind_since(), 100);
    assert_eq!(next.request, Request::Now);
    assert_eq!(next.request_tab, TAB, "the word stays on the tab it named");
    assert_eq!(next.tab, TAB);
    let skipped = St {
        request: Request::Skip("2.1.282".to_string()),
        ..prior.clone()
    };
    let unskipped = St::for_target(Some(skipped), &from, &to, None, Proc::default(), 900);
    assert_eq!(
        unskipped.request,
        Request::None,
        "a skip of 2.1.282 is not a skip of 2.1.283"
    );
    assert!(unskipped.request_tab.is_empty(), "nor on any tab");
    let done = St {
        phase: Phase::Done,
        ..prior.clone()
    };
    let fresh = St::for_target(Some(done), &from, &to, None, Proc::default(), 900);
    assert_eq!(fresh.behind_since(), 900, "behind again from now");
    assert_eq!(fresh.request, Request::None);
    let same_target = Candidate {
        version: Version::parse("2.1.282").expect("v"),
        ..to.clone()
    };
    let same = St::for_target(
        Some(prior.clone()),
        &from,
        &same_target,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(same, prior, "the same target is the same upgrade");
    let remodel = St::for_target(
        Some(prior.clone()),
        &from,
        &same_target,
        Some("claude-opus-5-5"),
        Proc::default(),
        900,
    );
    assert_eq!(remodel.phase, Phase::Pending, "a new model is asked afresh");
    assert_eq!(remodel.behind_since(), 100);
    assert_eq!(remodel.request, Request::Now);
    let pending = St {
        phase: Phase::Pending,
        ..prior.clone()
    };
    assert_eq!(
        St::for_target(
            Some(pending.clone()),
            &from,
            &same_target,
            Some("claude-opus-5-5"),
            Proc::default(),
            900
        ),
        pending,
        "nothing announced yet: the pending state is the same upgrade"
    );
}

/// A process named by its kernel start alone.
fn proc(start: &str) -> Proc<'_> {
    Proc {
        start,
        started_s: 0,
    }
}

/// An age belongs to one process.
#[test]
fn an_age_belongs_to_one_process() {
    let running = Version::parse("2.1.283").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.284").expect("v"),
        source: Source::Managed,
    };
    let minted = |from: &str, proc_start: &str, phase: Phase| St {
        phase,
        from: from.to_string(),
        to: "2.1.283".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: proc_start.to_string(),
        ..St::default()
    };
    for phase in [Phase::Pending, Phase::Announced { at_s: 5, asks: 1 }] {
        let stale = minted("2.1.274", "A", phase.clone());
        let next = St::for_target(Some(stale), &running, &to, None, proc("B"), 900);
        assert_eq!(next.behind_since(), 900, "{phase:?}: B's age begins now");
        assert_eq!(next.proc_start, "B");
        assert_eq!(next.from, "2.1.283");
    }
    let legacy = minted("2.1.274", "", Phase::Pending);
    let next = St::for_target(Some(legacy), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 900, "found behind another build");
    let same_proc = minted("2.1.283", "B", Phase::Pending);
    let next = St::for_target(Some(same_proc), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "the same process is still behind");
    let same_build = minted("2.1.283", "", Phase::Pending);
    let next = St::for_target(Some(same_build), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "a legacy record, the same build");
    assert_eq!(next.proc_start, "B", "and it learns its process");
    let unknown = minted("", "", Phase::Pending);
    let next = St::for_target(Some(unknown), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "names nothing: carried as ever");
    let waiting = St {
        wait: "background".to_string(),
        wait_since: 50,
        salt: 10,
        held_by: vec![HeldBy {
            pid: 7,
            name: "zsh".to_string(),
            since: 60,
        }],
        stop_streak: 3,
        streak_why: "signal-refused".to_string(),
        last_stop: "signal-refused".to_string(),
        refused: 3,
        refused_at: 40,
        ..minted("2.1.274", "A", Phase::Pending)
    };
    let target = Candidate {
        version: Version::parse("2.1.283").expect("v"),
        ..to.clone()
    };
    let from = Version::parse("2.1.275").expect("v");
    let next = St::for_target(Some(waiting.clone()), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (
            next.behind_since(),
            next.from.as_str(),
            next.wait.as_str(),
            next.held_by.len()
        ),
        (900, "2.1.275", "", 0),
        "the record follows its process"
    );
    assert_eq!(
        (
            next.stop_streak,
            next.streak_why.as_str(),
            next.last_stop.as_str(),
            next.refused
        ),
        (0, "", "", 0),
        "and nothing the old process answered: B never refused a signal"
    );
    let same = St {
        proc_start: "B".to_string(),
        from: "2.1.275".to_string(),
        ..waiting
    };
    let kept_same = St::for_target(Some(same), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (kept_same.stop_streak, kept_same.streak_why.as_str()),
        (3, "signal-refused"),
        "the same process is still the one that stopped"
    );
    assert_eq!(
        (next.salt, next.proc_start.as_str()),
        (900, "B"),
        "its minting is the new process's too: the classifier's horizons count from it"
    );
    let announced = St {
        salt: 10,
        ..minted("2.1.274", "A", Phase::Announced { at_s: 50, asks: 1 })
    };
    let kept = St::for_target(Some(announced), &from, &target, None, proc("B"), 900);
    assert_eq!(kept.proc_start, "A", "not stamped with the holder's start");
    let target_next = Candidate {
        version: Version::parse("2.1.284").expect("v"),
        ..to.clone()
    };
    let later = St::for_target(Some(kept), &from, &target_next, None, proc("B"), 950);
    assert_eq!(
        later.behind_since(),
        950,
        "B's age begins when B's target does"
    );
    let target_284 = Candidate {
        version: Version::parse("2.1.284").expect("v"),
        ..to.clone()
    };
    let old_record = St {
        phase: Phase::Announced { at_s: 650, asks: 1 },
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        salt: 700,
        ..St::default()
    };
    let born = |started_s| Proc {
        start: "B",
        started_s,
    };
    for (started_s, want) in [(500, 700), (800, 800), (0, 100)] {
        let next = St::for_target(
            Some(old_record.clone()),
            &running,
            &target_284,
            None,
            born(started_s),
            900,
        );
        assert_eq!(
            next.behind_since(),
            want,
            "the process began at {started_s}"
        );
        assert_eq!(next.phase, old_record.phase, "the notice stands");
        assert_eq!(next.proc_start, "B", "and the record learns its process");
    }
    let after = St {
        pending_since: 600,
        ..old_record
    };
    let next = St::for_target(Some(after), &running, &target_284, None, born(500), 900);
    assert_eq!(
        next.behind_since(),
        600,
        "an age from within the process stands"
    );
    let stopped = minted("2.1.275", "B", Phase::Failed("no-resume".to_string()));
    let next = St::for_target(Some(stopped.clone()), &from, &target, None, proc("B"), 900);
    assert_eq!((next.phase, next.pending_since), (stopped.phase, 100));
    let of_a = St {
        wait: "failed".to_string(),
        held_by: vec![HeldBy {
            pid: 7,
            name: "zsh".to_string(),
            since: 60,
        }],
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 3,
        own_work_at: 80,
        ..minted("2.1.274", "A", Phase::Failed(upgrade::GAVE_UP.to_string()))
    };
    let next = St::for_target(Some(of_a), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (
            next.phase.clone(),
            next.behind_since(),
            next.held_by.len(),
            next.stop_streak,
            next.own_work_at
        ),
        (Phase::Pending, 900, 0, 0, 0),
        "another process starts afresh"
    );
    assert_eq!(next.proc_start, "B");
}

/// A newer build landing in a rest keeps the same process's age, unanswered rounds.
#[test]
fn a_newer_build_landing_in_a_rest_keeps_the_process_s_history() {
    let running = Version::parse("2.1.283").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    let rested = |proc_start: &str, phase: Phase| St {
        phase,
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: proc_start.to_string(),
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 2,
        own_work_at: 700,
        waits: vec![WaitNote {
            at: 650,
            word: "background".to_string(),
            note: "holders=3".to_string(),
        }],
        ..St::default()
    };
    let gave_up = Phase::Failed(upgrade::GAVE_UP.to_string());
    let next = St::for_target(
        Some(rested("B", gave_up.clone())),
        &running,
        &to,
        None,
        proc("B"),
        900,
    );
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.to, "2.1.285");
    assert_eq!(next.behind_since(), 100, "the same process keeps its age");
    assert_eq!(
        (next.streak_why.as_str(), next.stop_streak, next.own_work_at),
        (upgrade::GAVE_UP, 2, 700),
        "and the rounds it answered not yet"
    );
    assert_eq!(next.waits.len(), 1, "and why it has waited");
    let refused = St {
        streak_why: "no-resume".to_string(),
        stop_streak: 2,
        ..rested("B", Phase::Failed("no-resume".to_string()))
    };
    let next = St::for_target(Some(refused), &running, &to, None, proc("B"), 900);
    assert_eq!(
        (
            next.behind_since(),
            next.streak_why.as_str(),
            next.stop_streak
        ),
        (100, "no-resume", 2)
    );
    let next = St::for_target(
        Some(rested("A", gave_up)),
        &running,
        &to,
        None,
        proc("B"),
        900,
    );
    assert_eq!(
        (next.behind_since(), next.stop_streak, next.waits.len()),
        (900, 0, 0),
        "another process starts fresh"
    );
}

/// Conformance test checking `harness_upgrade_age_model` against [`St::for_target`] and
/// [`forget_settled_pending`].
#[test]
fn the_real_age_conforms_to_the_age_model() {
    use aterm_models_harness::harness_upgrade_age_model;
    use aterm_spec::interp;
    let model = harness_upgrade_age_model();
    let day = STALLED_AFTER_S;
    let now = 10 * day;
    let v = |t: &str| Version::parse(t).expect("version");
    let target = |t: &str| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: v(t),
        source: Source::Managed,
    };
    let holder = |proc: i64| if proc == 0 { "A" } else { "B" };
    let dir = scratch("age-model");
    let opts = drive(&dir);
    let record = |s: &std::collections::BTreeMap<&'static str, i64>| -> Option<St> {
        let counts = s["rec"] - 1;
        (s["rec"] > 0).then(|| St {
            phase: Phase::Pending,
            from: if counts == 0 { "2.1.274" } else { "2.1.283" }.to_string(),
            to: if counts == 0 { "2.1.283" } else { "2.1.284" }.to_string(),
            tab: TAB.to_string(),
            proc_start: holder(counts).to_string(),
            pending_since: if s["aged"] == 1 { now - day } else { now - 60 },
            ..St::default()
        })
    };
    let running = |s: &std::collections::BTreeMap<&'static str, i64>| {
        // Where the holder stands, as `record` says.
        if s["proc"] == 0 {
            v("2.1.274")
        } else {
            v("2.1.283")
        }
    };
    let project = |st: Option<&St>| -> (i64, i64) {
        st.map_or((0, 0), |st| {
            (
                if st.proc_start == "A" { 1 } else { 2 },
                i64::from(now.saturating_sub(st.behind_since()) >= day),
            )
        })
    };
    let alarm = |st: &St| -> i64 {
        save(&opts, SESSION, st);
        i64::from(
            rows(&opts)
                .iter()
                .any(|r| r.session == SESSION && r.stall(now).as_deref() == Some("overdue")),
        )
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([model.init_state()]);
    let mut states = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &model.actions {
            let mut next = s.clone();
            if model.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        states.push(s);
    }
    assert!(states.len() > 5, "{} states", states.len());
    let mut fired = std::collections::BTreeSet::new();
    for s in &states {
        for inv in &model.invariants {
            assert!(model.check_invariant(inv.name, s), "{} at {s:?}", inv.name);
        }
        for a in &model.actions {
            let mut next = s.clone();
            if !model.fire(a.name, &mut next) {
                continue;
            }
            fired.insert(a.name);
            let at = format!("{} at {s:?}", a.name);
            let rec = record(s);
            let real: (i64, i64, i64) = match a.name {
                "Time" => {
                    let st = St {
                        pending_since: now - day,
                        ..rec.expect("a record")
                    };
                    let (r, aged) = project(Some(&st));
                    (r, aged, s["alarm"])
                }
                "HandRestart" => {
                    let (r, aged) = project(rec.as_ref());
                    (r, aged, 0)
                }
                "Visit" => {
                    let st = rec.expect("a record");
                    save(&opts, SESSION, &st);
                    let closed =
                        forget_settled_pending(&opts, SESSION, Some(&st), &v("2.1.283")).is_some();
                    let (r, aged) = project(load(&opts, SESSION).as_ref());
                    assert!(closed || load(&opts, SESSION).is_none(), "{at}");
                    (r, aged, 0)
                }
                "NewBuild" => {
                    let st = St::for_target(
                        rec,
                        &running(s),
                        &target("2.1.284"),
                        None,
                        proc(holder(s["proc"])),
                        now,
                    );
                    let (r, aged) = project(Some(&st));
                    (r, aged, s["alarm"])
                }
                "Judge" => {
                    let st = rec.expect("a record");
                    let (r, aged) = project(Some(&st));
                    (r, aged, alarm(&st))
                }
                other => panic!("an action the bind does not drive: {other}"),
            };
            assert_eq!(
                (next["rec"], next["aged"], next["alarm"]),
                real,
                "the real code left another state than the model at {at}"
            );
        }
    }
    assert_eq!(
        fired,
        std::collections::BTreeSet::from(["HandRestart", "Judge", "NewBuild", "Time", "Visit"]),
        "every action of the model is driven"
    );
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Time", "HandRestart", "NewBuild", "Judge"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("AgeIsThisProcess", &b), "{b:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A stopped upgrade retargeted keeps its move clock, the owner's word and its stop streak.
#[test]
fn a_stopped_upgrade_retargeted_keeps_its_clock_word_and_streak() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.280".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        request: Request::DeferUntil(5_000),
        request_tab: TAB.to_string(),
        request_at: 50,
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 3,
        ..St::default()
    };
    let next = St::for_target(Some(prior.clone()), &from, &to, None, Proc::default(), 900);
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.behind_since(), 100, "the move clock is kept");
    assert_eq!(
        next.request,
        Request::DeferUntil(5_000),
        "the defer is kept"
    );
    assert_eq!((next.request_tab.as_str(), next.request_at), (TAB, 50));
    assert_eq!(
        (next.streak_why.as_str(), next.stop_streak),
        (upgrade::GAVE_UP, 3),
        "the same stop in a row is kept"
    );
    assert_eq!(next.tab, TAB);
    let moved = St::for_target(
        Some(St {
            from: "2.1.279".to_string(),
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(moved.behind_since(), 900, "moved since: behind from now");
    assert_eq!(
        moved.request,
        Request::DeferUntil(5_000),
        "the owner's snooze is the session's, not the process's"
    );
    assert_eq!(moved.stop_streak, 0);
    let moved_now = St::for_target(
        Some(St {
            from: "2.1.279".to_string(),
            request: Request::Now,
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(moved_now.request, Request::None);
    let same_now = St::for_target(
        Some(St {
            request: Request::Now,
            ..prior
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(same_now.request, Request::Now, "the process it hurried");
}

/// The label follows the build until the restart begins.
#[test]
fn a_reused_state_names_the_build_the_sweep_would_start_now() {
    let from = Version::parse("2.1.280").expect("v");
    let managed = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.282").expect("v"),
        source: Source::Managed,
    };
    for phase in [Phase::Pending, Phase::Announced { at_s: 5, asks: 1 }] {
        let prior = St {
            phase: phase.clone(),
            to: "2.1.282".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        let next = St::for_target(
            Some(prior.clone()),
            &from,
            &managed,
            None,
            Proc::default(),
            900,
        );
        assert_eq!(next.source, "managed", "{phase:?}: relabelled");
        assert_eq!(
            St {
                source: prior.source.clone(),
                ..next
            },
            prior,
            "{phase:?}: nothing but the label moved"
        );
    }
    for phase in [
        Phase::Exiting { at_s: 5 },
        Phase::Failed("no-resume".to_string()),
    ] {
        let prior = St {
            phase: phase.clone(),
            to: "2.1.282".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        let next = St::for_target(
            Some(prior.clone()),
            &from,
            &managed,
            None,
            Proc::default(),
            900,
        );
        assert_eq!(next, prior, "{phase:?}: begun or final, kept as it was");
    }
}

/// [`prune_states`] removes a settled upgrade of a conversation no live Claude holds once it has
/// sat [`STATE_KEEP_S`].
#[test]
fn settled_states_of_dead_conversations_are_pruned_after_a_week_and_nothing_else() {
    let dir = scratch("prune-states");
    let opts = drive(&dir);
    let me = std::process::id();
    let live = register(&opts.home, me, SESSION);
    let settled = |phase: Phase| St {
        phase,
        to: "2.1.282".to_string(),
        ..St::default()
    };
    let gone = [
        ("dead-done", settled(Phase::Done)),
        (
            "dead-failed",
            settled(Phase::Failed("unanswered".to_string())),
        ),
        ("dead-pending", settled(Phase::Pending)),
    ];
    let kept = [
        (SESSION, settled(Phase::Done)),
        (
            "dead-announced",
            settled(Phase::Announced { at_s: 5, asks: 1 }),
        ),
        ("dead-exiting", settled(Phase::Exiting { at_s: 5 })),
        (
            "dead-skipped",
            St {
                request: Request::Skip("2.1.282".to_string()),
                ..settled(Phase::Pending)
            },
        ),
        (
            "codex-s-feed",
            St {
                agent: upgrade::Agent::Codex,
                ..settled(Phase::Done)
            },
        ),
    ];
    for (id, st) in gone.iter().chain(kept.iter()) {
        save(&opts, id, st);
    }
    let models = state_dir(&opts).join("models.json");
    std::fs::write(&models, "{}").expect("models");
    let files = [live];
    let week = STATE_KEEP_S;
    let present = |id: &str| state_path(&opts, id).exists();

    prune_states(&opts, &files, now_s() + 24 * 60 * 60);
    assert!(gone.iter().chain(kept.iter()).all(|(id, _)| present(id)));
    prune_states(
        &Opts {
            dry_run: true,
            ..drive(&dir)
        },
        &files,
        now_s() + 2 * week,
    );
    assert!(gone.iter().chain(kept.iter()).all(|(id, _)| present(id)));

    prune_states(&opts, &files, now_s() + week + 60);
    for (id, _) in &gone {
        assert!(!present(id), "{id}: settled, dead and a week old");
    }
    for (id, _) in &kept {
        assert!(present(id), "{id}: kept");
    }
    assert!(
        models.exists(),
        "a file that is no state is not the prune's"
    );
    let stamp_at = |at| super::super::restart_stamp::Stamp {
        pid: dead_pid(),
        start: "Thu Jan 1 00:00:00 1970".to_string(),
        cause: super::super::restart_stamp::Cause::Asked,
        at,
    };
    let at = now_s();
    let fresh_at = at + super::super::relaunch::STALE_S;
    super::super::restart_stamp::write(&opts.state, "s-old", &stamp_at(at)).expect("stamp");
    super::super::restart_stamp::write(&opts.state, "s-new", &stamp_at(fresh_at)).expect("stamp");
    prune_states(&opts, &files, fresh_at + 1);
    assert_eq!(
        super::super::restart_stamp::read(&opts.state, "s-old"),
        None,
        "a stale stamp is pruned"
    );
    assert!(
        super::super::restart_stamp::read(&opts.state, "s-new").is_some(),
        "a standing stamp is kept"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A note of where a moved agent wrote goes once it names nothing.
#[test]
fn a_writes_note_goes_once_it_names_nothing_and_nothing_else() {
    let dir = scratch("prune-writes-notes");
    let opts = drive(&dir);
    let live = register(&opts.home, std::process::id(), SESSION);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let note = |id: &str, body: &str| {
        let p = state_dir(&opts).join(format!("{id}.writes"));
        std::fs::write(&p, body).expect("note");
        p
    };
    let named = |d: &Path| format!("{}\n", d.display());
    let removed = dir.join("removed-worktree").join(".claude");
    let kept_wt = dir.join("kept-worktree").join(".claude");
    std::fs::create_dir_all(kept_wt.join("projects").join("p")).expect("wt");
    std::fs::write(
        kept_wt.join("projects").join("p").join("dead-kept.jsonl"),
        "{}\n",
    )
    .expect("transcript");
    let emptied = dir.join("emptied-worktree").join(".claude");
    std::fs::create_dir_all(emptied.join("projects").join("p")).expect("wt");
    let walled = dir.join("walled-worktree").join(".claude");
    std::fs::create_dir_all(walled.join("projects")).expect("wt");
    let gone = [
        note("dead-gone", &named(&removed)),
        note("dead-emptied", &named(&emptied)),
        note("dead-blank", "\n"),
    ];
    let kept = [
        note("dead-kept", &named(&kept_wt)),
        note(SESSION, &named(&removed)),
        note("dead-walled", &named(&walled)),
    ];
    let files = [live];
    prune_states(
        &Opts {
            dry_run: true,
            ..drive(&dir)
        },
        &files,
        now_s(),
    );
    for p in gone.iter().chain(&kept) {
        assert!(p.exists(), "a dry run removes nothing: {}", p.display());
    }
    use std::os::unix::fs::PermissionsExt;
    let projects = walled.join("projects");
    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o000)).expect("wall");
    prune_states(&opts, &files, now_s());
    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o700)).expect("unwall");
    for p in &gone {
        assert!(!p.exists(), "it names nothing: {}", p.display());
    }
    let [transcript, mine, unlisted] = &kept;
    assert!(
        transcript.exists(),
        "CONTROL: its transcript is still there"
    );
    assert!(mine.exists(), "CONTROL: the live conversation's");
    assert!(unlisted.exists(), "CONTROL: a `projects` it cannot list");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record of an agent this build does not know is a newer build's ([`upgrade::Agent::parse`]).
#[test]
fn a_record_of_an_unknown_agent_is_passed_by_and_left_as_it_lies() {
    use super::super::relaunch::{carry_restart, restarted};
    const THEIRS: &str = "s-9e31a1c0ffee0000beef";
    const CODEX: &str = "s-c0dec0dec0dec0dec0de";
    let dir = scratch("unknown-agent");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let in_tab = |tab: &str| Opts {
        only_sid: Some(tab.to_string()),
        ..opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let now = now_s();
    let tui = dead_pid();
    let codex = |tab: &str, phase: Phase| St {
        agent: upgrade::Agent::Codex,
        phase,
        pid: tui,
        tab: tab.to_string(),
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        ready_since: now - 10,
        ..St::default()
    };
    let worded = |st: &St, word: &str| {
        let text = st.to_json();
        let out = text.replace(r#""agent":"codex""#, &format!(r#""agent":"{word}""#));
        assert_ne!(out, text, "the word is the record's");
        out
    };
    let plant = |name: &str, text: &str| {
        std::fs::write(state_path(&opts, name), text).expect("plant");
    };
    let mut theirs: Vec<(String, String)> = Vec::new();
    let mut plant_theirs = |name: String, st: &St| {
        let text = worded(st, "opencode");
        plant(&name, &text);
        theirs.push((name, text));
    };
    let codex_name = format!("codex-{CODEX}");

    let claude_done = "5d1c7f0e-2b8a-4c3d-9e6f-1a2b3c4d5e6f";
    plant(claude_done, &worded(&codex(THEIRS, Phase::Done), ""));
    plant_theirs(
        "7a9e0d1b-3c4d-4e5f-8a9b-0c1d2e3f4a5b".to_string(),
        &codex(THEIRS, Phase::Done),
    );
    plant_theirs(format!("codex-{THEIRS}"), &codex(THEIRS, Phase::Done));
    plant(&codex_name, &codex(CODEX, Phase::Done).to_json());
    let relaunched = Phase::Relaunched { at_s: now };
    plant_theirs(
        format!("opencode-{THEIRS}"),
        &codex(THEIRS, relaunched.clone()),
    );
    let as_it_lies = |theirs: &[(String, String)]| {
        for (name, text) in theirs {
            let on_disk = std::fs::read_to_string(state_path(&opts, name)).ok();
            assert_eq!(on_disk.as_deref(), Some(text.as_str()), "{name}");
        }
    };
    for (name, _) in &theirs {
        assert_eq!(load(&opts, name), None, "{name}");
    }
    assert_eq!(
        load(&opts, claude_done).map(|st| st.agent),
        Some(upgrade::Agent::Claude)
    );
    assert_eq!(
        load(&opts, &codex_name).map(|st| st.agent),
        Some(upgrade::Agent::Codex)
    );
    let week_on = now + STATE_KEEP_S + 60;
    prune_states(&opts, &[], week_on);
    assert!(
        !state_path(&opts, claude_done).exists(),
        "control: Claude Code's settled record is pruned"
    );
    codex_alab_drive::prune(&opts, week_on, |_| TabLife::Gone);
    assert!(
        !state_path(&opts, &codex_name).exists(),
        "control: the Codex lane's settled record is pruned"
    );
    as_it_lies(&theirs);

    plant(&codex_name, &codex(CODEX, relaunched).to_json());
    assert_eq!(in_flight(&opts), [codex_name.as_str()], "control");
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let orphaned = orphans(&dry, &[], None);
    assert!(orphaned.is_empty(), "{orphaned:?}");
    let (mine, cdx) = (in_tab(THEIRS), in_tab(CODEX));
    for lane in [upgrade::Agent::Codex, upgrade::Agent::Claude] {
        assert!(!restarted(&mine, lane, Some(tui)));
        assert_eq!(
            carry_restart(&mine, lane, Some(tui), false).step,
            "refused:no-restart-in-flight"
        );
    }
    assert!(!owed(&mine));
    assert_eq!(resume(&mine).step, "current");
    assert!(owed(&cdx), "control: its continuation owed");
    as_it_lies(&theirs);
    assert!(hold_clock(&mine, now + 600));
    as_it_lies(&theirs);
    assert!(hold_clock(&cdx, now + 600));
    assert_eq!(
        load(&opts, &codex_name).map(|st| st.ready_since),
        Some(now + 600),
        "control: the Codex record's clock is held"
    );
    let rows: Vec<String> = super::upgrade_status::rows(&opts)
        .into_iter()
        .map(|row| row.session)
        .collect();
    assert_eq!(rows, [codex_name.as_str()], "control: the owner's view");
    let word = |tab: &str| {
        super::upgrade_status::ask(&opts, tab, super::upgrade_status::Ask::Skip, false)
            .expect_err("nothing to skip")
    };
    let refused = word(THEIRS);
    let under_way = word(CODEX);
    assert_ne!(refused, under_way, "control: the Codex record is seen");
    as_it_lies(&theirs);
    let told = Watcher {
        told: true,
        ..Watcher::default()
    };
    let read = |o: &Opts| Report {
        pid: 0,
        tab: o.only_sid.clone().unwrap_or_default(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "wait:not-idle".to_string(),
    };
    let looked = |tab: &str| watch_with(&opts, tab, &told, now, &read);
    assert_eq!(looked(THEIRS).skipped.as_deref(), Some("no-record"));
    let control = looked(CODEX);
    assert_ne!(
        control.skipped.as_deref(),
        Some("no-record"),
        "control: {control:?}"
    );
    as_it_lies(&theirs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a report says about waiting.
#[test]
fn a_report_records_its_wait_and_an_act_clears_it() {
    let mut st = St::default();
    st.note_step("wait:not-idle", "busy", 10);
    assert_eq!((st.wait.as_str(), st.wait_since), ("not-idle:busy", 10));
    st.note_step("wait:not-idle", "busy", 70);
    assert_eq!(st.wait_since, 10, "the same wait keeps its start");
    st.note_step("wait:not-idle", "shell", 130);
    assert_eq!((st.wait.as_str(), st.wait_since), ("not-idle:shell", 130));
    st.note_step("skip:not-selected", "idle", 190);
    st.note_step("would-announce", "idle", 190);
    assert_eq!(st.wait, "not-idle:shell", "neither is an act");
    st.note_step("announced:1", "idle", 250);
    assert!(st.wait.is_empty() && st.wait_since == 0);
    st.note_step("held-back:attended", "idle", 300);
    assert_eq!((st.wait.as_str(), st.wait_since), ("attended", 300));
    assert_eq!(st.wait_seen, 300);
    st.note_step("wait:attended", "idle", 360);
    assert_eq!(st.wait_since, 300, "the same wait keeps its start");
    assert_eq!(st.wait_seen, 360, "the last look that saw the person");
    st.note_step("wait:settling", "idle", 380);
    assert_eq!(st.wait_seen, 0);
    st.note_step("wait:attended", "idle", 400);
    st.note_step("gave-up", "idle", 420);
    assert!(st.wait.is_empty() && st.wait_since == 0 && st.wait_seen == 0);
}

/// The ledger is bounded: past the disk journal's 1 MiB it is cut back to its newest rows at the
/// next write.
#[test]
fn the_upgrade_ledger_is_cut_back_to_its_newest_rows() {
    let dir = scratch("ledger-bound");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let r = |step: &str| Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: step.to_string(),
    };
    ledger(&opts, &r("announced:1"), "small");
    ledger(&opts, &r("announced:2"), "small");
    assert_eq!(ledger_lines(&opts), 2, "a small ledger is appended to");
    let row = format!(
        "{{\"step\":\"old\",\"detail\":\"{}\"}}\n",
        "x".repeat(1_000)
    );
    let big: String = std::iter::repeat_n(row.as_str(), 1_200).collect();
    std::fs::write(ledger_path(&opts), big).expect("a ledger past 1 MiB");
    ledger(&opts, &r("done"), "the newest");
    let text = std::fs::read_to_string(ledger_path(&opts)).expect("ledger");
    assert_eq!(text.lines().count(), LEDGER_KEEP_ROWS);
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.contains(r#""step":"done""#)),
        "the newest row survives the cut"
    );
    assert!(text.len() < 1_200 * row.len(), "the file shrank");
    let _ = std::fs::remove_dir_all(dir);
}

/// The model on the relaunch line.
#[test]
fn a_listed_model_replaces_every_launch_model_spelling() {
    use super::super::relaunch::with_model;
    let v = |xs: &[&str]| xs.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert_eq!(
        with_model(
            v(&[
                "--model",
                "opus",
                "--verbose",
                "--model=sonnet",
                "--resume",
                "ID"
            ]),
            Some("claude-opus-5-5")
        ),
        v(&["--verbose", "--model", "claude-opus-5-5", "--resume", "ID"])
    );
    assert_eq!(
        with_model(v(&["--resume", "ID"]), Some("claude-fable-5-1")),
        v(&["--model", "claude-fable-5-1", "--resume", "ID"])
    );
    let kept = v(&["--model", "opus", "--resume", "ID"]);
    assert_eq!(with_model(kept.clone(), None), kept);
    assert_eq!(
        with_model(
            v(&[
                "--model",
                "opus",
                "--verbose",
                "--model=x",
                "--resume",
                "ID"
            ]),
            Some("")
        ),
        v(&["--verbose", "--resume", "ID"])
    );
}

/// The relaunch argv end to end, through the calls the upgrade makes.
#[test]
fn the_relaunch_line_moves_the_model_within_its_family_first_and_keeps_the_rest() {
    const ID: &str = "446a0b3c-1979-414f-ae20-e06e26a2ef44";
    let offered: Vec<String> = [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable-5-1",
        "claude-opus-5",
    ]
    .map(String::from)
    .to_vec();
    let relaunch = |argv: &[&str], live: &str, offered: &[String]| -> String {
        let argv: Vec<String> = argv.iter().map(|w| (*w).to_string()).collect();
        let launch = upgrade::launch_model(&argv);
        let live = models::LiveModel {
            id: live.to_string(),
            by_command: false,
        };
        let asked = match models::model_due(
            &Priority::seed(0),
            Some(&live),
            offered,
            launch.as_deref(),
            None,
            &ModelRecord::default(),
        ) {
            ModelVerdict::Due { to, .. } => Some(to),
            ModelVerdict::Keep(_) => None,
        };
        line_for(
            Dialect::Bash,
            None,
            None,
            std::path::Path::new("/x/claude"),
            &argv,
            Some(ID),
            asked.as_deref(),
            None,
            &[],
        )
        .unwrap_or_else(|_| panic!("a line for {argv:?}"))
    };
    let line = |words: &[&str]| {
        std::iter::once("/x/claude")
            .chain(words.iter().copied())
            .chain(["--resume", ID])
            .map(|w| format!(" '{w}'"))
            .collect::<String>()
            .replacen(' ', " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 ", 1)
    };
    let rows: [(&[&str], &str, &[&str]); 4] = [
        (
            &["claude", "--dangerously-skip-permissions"],
            "claude-opus-5",
            &[
                "--dangerously-skip-permissions",
                "--model",
                "claude-opus-5-5",
            ],
        ),
        (
            &[
                "claude",
                "--fallback-model",
                "claude-opus-5",
                "--model",
                "claude-opus-5",
            ],
            "claude-opus-5",
            &[
                "--fallback-model",
                "claude-opus-5",
                "--model",
                "claude-opus-5-5",
            ],
        ),
        (
            &["claude", "--model=claude-opus-5[1m]"],
            "claude-opus-5",
            &["--model", "claude-opus-5-5[1m]"],
        ),
        (
            &["claude", "--model", "opus"],
            "claude-opus-5",
            &["--model", "opus"],
        ),
    ];
    for (argv, live, want) in rows {
        assert_eq!(relaunch(argv, live, &offered), line(want), "{argv:?}");
    }
}

/// A move outlives its record's removal (closed or pruned) and rides the next record, until the
/// longest limit and its grace have passed.
#[test]
fn a_move_outlives_its_records_removal() {
    const WRITTEN: u64 = 1_791_477_331;
    let tail = include_str!("../supervise/fixtures/claude-2.1.294-transcript-weekly-limit.jsonl");
    let dir = scratch("move-outlives-removal");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let running = Version::parse("2.1.282").expect("v");
    let live = Snapshot {
        login: Some("/accts/work-2".to_string()),
        ..users_agent(SESSION)
    };
    let after = now_s() - 60;
    let pending = |moved_at: u64| St {
        from: "2.1.274".to_string(),
        to: "2.1.282".to_string(),
        pending_since: 5,
        moved_at,
        moved_login: (moved_at != 0).then(|| "/accts/work-2".to_string()),
        ..St::default()
    };
    let stands = || super::super::relaunch::standing_limit(&opts, &live, tail).is_some();
    let forget = || {
        let prior = load(&opts, SESSION);
        forget_settled_pending(&opts, SESSION, prior.as_ref(), &running).is_some()
    };
    let mut wrong = Vec::new();
    save(&opts, SESSION, &pending(after));
    assert!(!stands(), "the record keeps the move");
    assert!(forget(), "reached by hand");
    assert!(load(&opts, SESSION).is_none(), "the record is closed");
    if stands() {
        wrong.push("closed: the move went with its record");
    }
    save(
        &opts,
        SESSION,
        &St {
            cause: CAUSE_EXIT.to_string(),
            relaunched_at: after + 60,
            ..St::default()
        },
    );
    let saved = load(&opts, SESSION).expect("the crash's record");
    if (saved.moved_at, saved.moved_login.as_deref()) != (after, Some("/accts/work-2")) {
        wrong.push("the next record does not carry the move");
    }
    if stands() {
        wrong.push("the next record: the limit stands");
    }
    let _ = std::fs::remove_file(state_dir(&opts).join(format!("{SESSION}.move")));
    let done = St {
        phase: Phase::Done,
        ..saved
    };
    save(&opts, SESSION, &done);
    let week_on = now_s() + STATE_KEEP_S + 60;
    prune_states(&opts, &[], week_on);
    assert!(load(&opts, SESSION).is_none(), "pruned");
    if stands() {
        wrong.push("pruned: the move went with its record");
    }
    let aged = after + upgrade_status::LONGEST_LIMIT_S + LIMIT_GRACE_S;
    prune_states(&opts, &[], aged - 1);
    if stands() {
        wrong.push("a move younger than the longest limit was dropped");
    }
    prune_states(&opts, &[], aged);
    if !stands() {
        wrong.push("a move older than the longest limit is kept");
    }
    assert!(wrong.is_empty(), "{wrong:#?}");

    save(&opts, SESSION, &pending(0));
    assert!(forget());
    assert!(stands(), "no move: closed, the row stands");
    save(&opts, SESSION, &pending(WRITTEN - 600));
    assert!(forget());
    assert!(stands(), "a row written after the move");
    let _ = std::fs::remove_dir_all(dir);
}

/// [`forget_settled_pending`] forgets a same-build (model) restart the list no longer asks for
/// before it begins.
#[test]
fn an_unwanted_model_restart_is_forgotten_before_it_begins() {
    let dir = scratch("forget-model-restart");
    let opts = drive(&dir);
    let running = Version::parse("2.1.282").expect("v");
    let model_only = St {
        from: "2.1.282".to_string(),
        to: "2.1.282".to_string(),
        pending_since: 5,
        ..St::default()
    };
    let build = St {
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        ..model_only.clone()
    };
    let announced = St {
        phase: Phase::Announced { at_s: 9, asks: 1 },
        ..model_only.clone()
    };
    let stopped = St {
        phase: Phase::Failed("unanswered".to_string()),
        from: "2.1.281".to_string(),
        ..model_only.clone()
    };
    let owing = St {
        release: "gave-up".to_string(),
        from: "2.1.281".to_string(),
        ..model_only.clone()
    };
    for (why, st) in [
        ("a build upgrade to a newer build", &build),
        ("an announced one", &announced),
        ("a stopped round", &stopped),
        ("a record that owes a release", &owing),
    ] {
        save(&opts, SESSION, st);
        let written = load(&opts, SESSION);
        assert_eq!(
            forget_settled_pending(&opts, SESSION, Some(st), &running),
            None
        );
        assert_eq!(load(&opts, SESSION), written, "{why} is kept");
    }
    save(&opts, SESSION, &model_only);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    forget_settled_pending(&dry, SESSION, Some(&model_only), &running);
    assert!(load(&opts, SESSION).is_some(), "a dry run writes nothing");
    assert_eq!(
        forget_settled_pending(&opts, SESSION, Some(&model_only), &running),
        None,
        "a model restart is forgotten without a word"
    );
    assert!(
        load(&opts, SESSION).is_none(),
        "the pending model restart is gone"
    );
    let reached = St {
        from: "2.1.274".to_string(),
        to: "2.1.282".to_string(),
        pending_since: 5,
        ..St::default()
    };
    save(&opts, SESSION, &reached);
    assert_eq!(
        forget_settled_pending(&opts, SESSION, Some(&reached), &running).as_ref(),
        Some(&reached),
        "reported, so the ledger says it once"
    );
    assert!(load(&opts, SESSION).is_none(), "and gone");
    let _ = std::fs::remove_dir_all(dir);
}

/// A busy Claude Code (its turn over, waiting on a workflow) is announced to once at a break
/// (`Opts::background`).
#[cfg(unix)]
#[test]
fn a_break_of_background_work_takes_a_notice_and_ends_nothing() {
    let busy = |h: &mut Parked| set_status(h, "busy");
    let waiting = Answers {
        screen: waiting_screen(),
        ..Answers::default()
    };
    let mut h = Parked::new("bg-idle-point", waiting, unsettled(), 3600);
    busy(&mut h);
    for _ in 0..3 {
        assert_eq!(h.visit().step, "wait:not-idle", "an idle point's step");
    }
    assert!(h.typed().is_empty());
    drop(h);
    let mut h = Parked::new("bg-break", Answers::default(), unsettled(), 3600);
    busy(&mut h);
    h.opts.background = true;
    assert_eq!(
        h.visit().step,
        "announced:1",
        "the first notice, at the break"
    );
    assert_eq!(h.typed().len(), 1);
    assert!(h.typed()[0].contains("[aterm harness]"), "{:?}", h.typed());
    assert_eq!(h.visit().step, "wait:background", "and nothing more there");
    assert_eq!(h.typed().len(), 1);
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
}

/// Claude's own session file for `h`'s agent, rewritten to say `status`.
#[cfg(unix)]
fn set_status(h: &mut Parked, status: &str) {
    let path = h
        .opts
        .home
        .join(format!(".claude/sessions/{}.json", h.sf.pid));
    let text = std::fs::read_to_string(&path).expect("session file");
    let was = format!(r#""status":"{}""#, h.sf.status);
    std::fs::write(
        &path,
        text.replace(&was, &format!(r#""status":"{status}""#)),
    )
    .expect("rewrite");
    h.sf = session_file_of(&h.opts.home, h.sf.pid).expect("parses");
    assert_eq!(h.sf.status, status);
}

/// Stamps `h`'s record with its agent's process start ([`St::proc_start`]), as this build
/// writes it: a record naming none is an older build's, whose age is repaired to the
/// process's start at the next look.
#[cfg(unix)]
fn own_record(h: &Parked) {
    let mut st = load(&h.opts, SESSION).expect("state");
    st.proc_start = squash(&h.sf.proc_start);
    save(&h.opts, SESSION, &st);
}

/// The process table's rows for two shells under `h`'s agent that never end
/// (a widowed `tail -f … | grep -m1 …`, its `grep` long gone).
#[cfg(unix)]
fn shells_under(h: &Parked) -> Vec<ProcRow> {
    vec![
        (999_901, h.sf.pid, "zsh".into()),
        (999_902, h.sf.pid, "zsh".into()),
    ]
}

/// [`SESSION`]'s transcript under `h`'s home: a person's task, the notice
/// whose marker is `marker`, and the agent's last answer `text`.
#[cfg(unix)]
fn asked_then_answered(h: &Parked, marker: &str, text: &str) {
    let notice = upgrade::prepare_prompt(
        &Version::parse("1.0.0").expect("from"),
        &Version::parse("9.9.9").expect("to"),
        Source::Managed,
        marker,
    );
    let project = h.opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let rows = [
        user_row("fix the bug"),
        user_row(&notice),
        turn_by("claude-opus-5-5", text),
    ];
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        rows.join("\n") + "\n",
    )
    .expect("transcript");
}

/// The quiet flow releases an agent an older build announced to.
#[cfg(unix)]
#[test]
fn a_quiet_round_an_older_build_announced_gives_up_and_releases_its_agent() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let rig = |name: &str, at_s: u64| {
        let mut h = quietly(|| Parked::new(name, Answers::default(), unsettled(), 3600));
        assert!(!h.opts.ask, "the default flow");
        set_status(&mut h, "shell");
        asked_then_answered(&h, marker, "Not yet: my shells still run.");
        let st = St {
            phase: Phase::Announced { at_s, asks: 3 },
            markers: vec![marker.to_string()],
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h.opts.background = true;
        h
    };
    let h = rig("quiet-announced-old", now_s() - 3 * 86_400);
    let shells = shells_under(&h);
    let words: Vec<String> = (0..2)
        .map(|_| h.visit_with(dead_pid(), &shells).step)
        .collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(
        typed[0].contains(upgrade::RELEASE_HEAD.trim_end()),
        "{typed:?}"
    );
    assert_eq!(h.details("gave-up").len(), 1);
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(kept.release.is_empty(), "nothing owed: {kept:?}");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert!(kept.work_seen, "{kept:?}");
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
    drop(h);
    let h = rig("quiet-announced-new", now_s() - 60);
    let shells = shells_under(&h);
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
}

/// A quiet pending round owing a release (a notice reached this agent, its round abandoned since)
/// types it where the agent's own work holds the restart (`released:retargeted`) and stays pending.
#[cfg(unix)]
#[test]
fn a_quiet_pending_round_types_the_release_it_owes_where_work_holds_its_restart() {
    let rig = |name: &str, owed: Option<bool>| {
        let mut h = quietly(|| Parked::new(name, Answers::default(), unsettled(), 3600));
        set_status(&mut h, "shell");
        let mut st = St {
            tab: TAB.to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            asked: vec!["ATERM-UPGRADE-READY-0badf00d".to_string()],
            ..unsettled()
        };
        if let Some(to_this) = owed {
            st.release = "retargeted".to_string();
            if !to_this {
                st.notice_pid = dead_pid();
            }
        }
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h.opts.background = true;
        h
    };
    let h = rig("quiet-owed-here", Some(true));
    let shells = shells_under(&h);
    assert_eq!(
        h.visit_with(dead_pid(), &shells).step,
        "released:retargeted",
        "typed: {:?}",
        h.typed()
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(
        typed[0].contains(upgrade::RELEASE_HEAD.trim_end()),
        "{typed:?}"
    );
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(kept.release.is_empty(), "{kept:?}");
    assert_eq!(
        kept.phase,
        Phase::Pending,
        "its quiet restart still to come"
    );
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert_eq!(h.typed().len(), 1, "once");
    drop(h);
    for (name, owed) in [
        ("quiet-owed-elsewhere", Some(false)),
        ("quiet-owes-nothing", None),
    ] {
        let h = rig(name, owed);
        let shells = shells_under(&h);
        assert_eq!(
            h.visit_with(dead_pid(), &shells).step,
            "wait:background",
            "{name}"
        );
        assert!(h.typed().is_empty(), "{name}: {:?}", h.typed());
    }
}

/// The owner's `--now` moves a quiet session whose status only lags (`shell` over an idle screen,
/// nothing under the agent) to its signal.
#[cfg(unix)]
#[test]
fn the_owners_now_restarts_a_quiet_session_whose_status_only_lags() {
    use std::os::unix::process::ExitStatusExt as _;
    let rig = |name: &str| {
        let behind = St {
            pending_since: now_s() - 3_600,
            ..settled()
        };
        let mut h = quietly(|| Parked::new(name, Answers::default(), behind, 3600));
        assert!(!h.opts.ask, "the default flow");
        set_status(&mut h, "shell");
        own_record(&h);
        transcript_says(&h, "Done: the report is written.");
        h
    };
    let h = rig("quiet-lag-unasked");
    for _ in 0..3 {
        assert_eq!(h.visit().step, "wait:status-stale");
    }
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert!(h.details("terminated").is_empty());
    drop(h);
    let mut h = rig("quiet-lag-now");
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    assert_eq!(h.visit().step, "wait:status-stale", "one look is no lag");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    let terminated = h.details("terminated");
    assert_eq!(terminated.len(), 1, "{r:?}");
    assert!(terminated[0].starts_with("SIGTERM"), "{terminated:?}");
    assert!(h.typed().is_empty(), "nothing typed first: {:?}", h.typed());
    drop(h);
    let looked = SessionFile {
        pid: 4242,
        session_id: SESSION.into(),
        cwd: "/".into(),
        version: "1.0.0".into(),
        status: "shell".into(),
        status_updated_at_ms: 1_000,
        proc_start: "Wed Sep 30 12:00:00 2026".into(),
        kind: "interactive".into(),
        entrypoint: "cli".into(),
        claude_dir: None,
        profile: None,
    };
    let idle = SessionFile {
        status: "idle".into(),
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    let rewritten = SessionFile {
        status: "busy".into(),
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    let restamped = SessionFile {
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    for lagging in [false, true] {
        assert!(status_lets_signal(&idle, &looked, lagging), "{lagging}");
        assert!(
            !status_lets_signal(&rewritten, &looked, lagging),
            "{lagging}"
        );
        assert!(
            !status_lets_signal(&restamped, &looked, lagging),
            "{lagging}"
        );
    }
    assert!(
        status_lets_signal(&looked, &looked, true),
        "the lag read past"
    );
    assert!(
        !status_lets_signal(&looked, &looked, false),
        "never unasked"
    );
}

/// The owner's `--now` never signals a turn begun since the look over a lagging status.
#[cfg(unix)]
#[test]
fn the_owners_now_never_signals_a_turn_begun_since_the_look_over_a_lagging_status() {
    use std::os::unix::process::ExitStatusExt as _;
    let rule = "─".repeat(20);
    let turn = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"human_ms":null,"first":0}}"#
    );
    let rig = |name: &str, after: Option<(usize, String)>| {
        let behind = St {
            pending_since: now_s() - 3_600,
            ..settled()
        };
        let answers = Answers {
            after,
            ..Answers::default()
        };
        let mut h = quietly(|| Parked::new(name, answers, behind, 3600));
        set_status(&mut h, "busy");
        own_record(&h);
        transcript_says(&h, "Done: the report is written.");
        ask(&h.opts, TAB, Ask::Now, false).expect("--now");
        assert_eq!(h.visit().step, "wait:status-stale", "one look is no lag");
        h
    };
    let signalled = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains(" signal "))
    };
    let mut h = rig("lag-turn-idle", None);
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    let before_hand = h
        .asked
        .lock()
        .expect("asked")
        .iter()
        .take_while(|l| !l.contains(" lease acquire "))
        .filter(|l| l.contains(" text "))
        .count();
    drop(h);
    for (after, want, name) in [
        (before_hand, "wait:changed", "lag-turn-at-last-look"),
        (
            before_hand + 1,
            "wait:changed-before-signal",
            "lag-turn-before-signal",
        ),
    ] {
        let h = rig(name, Some((after, turn.clone())));
        let mut shell = tty_zsh();
        let r = h.visit_under(shell.id());
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert!(!signalled(&h), "{name}: signalled mid-turn");
        assert!(h.details("terminated").is_empty(), "{name}");
        assert!(alive(h.sf.pid), "{name}: the agent lives on");
        assert!(h.typed().is_empty(), "{name}: {:?}", h.typed());
        let st = load(&h.opts, SESSION).expect("state");
        assert_eq!(st.phase, Phase::Pending, "{name}: the round as it was");
    }
}

/// A give-up's owed release is typed at a break screen.
#[cfg(unix)]
#[test]
fn a_give_up_at_a_break_types_the_release_it_owes() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let signalled = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal"))
    };
    let mut h = Parked::new("bg-giveup-ready", Answers::default(), unsettled(), 3600);
    set_status(&mut h, "shell");
    asked_then_answered(&h, marker, &format!("Done.\\n{marker}"));
    let mut st = St {
        markers: vec![marker.to_string()],
        ready_since: now_s() - upgrade::DRAIN_S - 60,
        ..announced_to(&h, marker)
    };
    st.give_up(now_s());
    assert_eq!(st.release, "gave-up");
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let shells = shells_under(&h);
    let words: Vec<String> = (0..2)
        .map(|_| h.visit_with(dead_pid(), &shells).step)
        .collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    assert_eq!(h.details("drain-expired:background").len(), 1);
    assert_eq!(h.typed().len(), 1);
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(
        kept.release.is_empty() && kept.markers.is_empty(),
        "{kept:?}"
    );
    assert!(!signalled(&h), "nothing ended");
    drop(h);

    let waiting = Answers {
        screen: waiting_screen(),
        ..Answers::default()
    };
    let mut h = Parked::new("bg-giveup-inproc", waiting, unsettled(), 3600);
    set_status(&mut h, "busy");
    asked_then_answered(&h, marker, &format!("Done.\\n{marker}"));
    let mut st = St {
        markers: vec![marker.to_string()],
        ready_since: now_s() - 3 * upgrade::DRAIN_S,
        ..announced_to(&h, marker)
    };
    st.give_up(now_s());
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let words: Vec<String> = (0..2).map(|_| h.visit().step).collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    assert_eq!(h.details("drain-expired:background").len(), 1);
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(
        typed[0].contains(upgrade::RELEASE_HEAD.trim_end()),
        "{typed:?}"
    );
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(
        kept.release.is_empty() && kept.markers.is_empty(),
        "{kept:?}"
    );
    assert!(!signalled(&h), "nothing ended");
    drop(h);

    let mut h = Parked::new("bg-standing", Answers::default(), unsettled(), 3600);
    set_status(&mut h, "shell");
    asked_then_answered(&h, marker, "Waiting on the closers.");
    let st = St {
        markers: vec![marker.to_string()],
        ..announced_to(&h, marker)
    };
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let shells = shells_under(&h);
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    let held: Vec<(u32, String)> = load(&h.opts, SESSION)
        .expect("state")
        .held_by
        .iter()
        .map(|p| (p.pid, p.name.clone()))
        .collect();
    assert_eq!(
        held,
        [(999_901, "zsh".to_string()), (999_902, "zsh".to_string())]
    );
}

/// [`St::note_held`] keeps what holds the move by start, not age.
#[test]
fn what_holds_the_move_is_kept_by_its_start() {
    let zsh = |pid: u32, age_s: u64| upgrade::Held {
        pid,
        name: "zsh".to_string(),
        age_s,
        command: "tail -f journal.jsonl".to_string(),
    };
    let mut st = St::default();
    st.note_held("wait:background", &[zsh(63_492, 100)], 1_000);
    assert_eq!(st.held_by[0].since, 900);
    st.note_held("wait:background", &[zsh(63_492, 101)], 1_002);
    assert_eq!(st.held_by[0].since, 900, "the same process, the same start");
    st.note_held("wait:background", &[zsh(63_492, 5)], 1_002);
    assert_eq!(st.held_by[0].since, 997, "a new process under an old pid");
    let json = st.to_json();
    assert!(!json.contains("tail -f"), "never a command: {json}");
    assert_eq!(St::from_json(&json).expect("parses").held_by, st.held_by);
    st.note_held("announced:2", &[zsh(63_492, 5)], 1_003);
    assert!(st.held_by.is_empty(), "an act clears it");
    st.note_held("wait:awaiting-ready", &[], 1_004);
    assert!(st.held_by.is_empty());
}

/// A Claude Code whose turn is over, a dynamic workflow it started still
/// running: the wait drawn over the composer (`wait_bg.out`'s shape).
#[cfg(unix)]
fn waiting_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["⏺ Both workflows are launched.","","✻ Waiting for 2 dynamic workflows to finish","","{rule}","❯ ","{rule}","  ⏵⏵ bypass permissions on (shift+tab to cycle)"],"cursor":{{"row":5,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// A Claude Code mid-turn, a Bash call running in the foreground: the
/// spinner over the composer, the busy footer under it.
#[cfg(unix)]
fn mid_turn_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["⏺ Bash(sleep 600)","  ⎿  Running…","","✻ Pondering… (12s · esc to interrupt)","","{rule}","❯ ","{rule}","  ⏵⏵ bypass permissions on · esc to interrupt"],"cursor":{{"row":6,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// An idle screen over Claude Code's own work (status `shell` over two never-ending shells) waits
/// `status-stale` one look and is asked at its second.
#[cfg(unix)]
#[test]
fn an_idle_screen_over_claudes_own_work_is_asked_at_its_second_look() {
    let mut h = Parked::new("shell-idle", Answers::default(), settled(), 3600);
    set_status(&mut h, "shell");
    let shells = shells_under(&h);
    assert_eq!(
        h.visit_with(dead_pid(), &shells).step,
        "wait:status-stale",
        "one look"
    );
    assert_eq!(
        load(&h.opts, SESSION).expect("state").wait,
        "status-stale:shell"
    );
    assert!(h.typed().is_empty());
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "announced:1");
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("pid 999901 (zsh"), "{typed:?}");
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
    drop(h);

    let mut h = Parked::new(
        "shell-mid-turn",
        Answers {
            screen: mid_turn_screen(),
            ..Answers::default()
        },
        settled(),
        3600,
    );
    set_status(&mut h, "shell");
    let shells = shells_under(&h);
    for _ in 0..3 {
        assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:not-idle");
    }
    assert!(h.typed().is_empty(), "{:?}", h.typed());
}

/// [`St::idle_looks`] counts one process's idle looks in a row.
#[cfg(unix)]
#[test]
fn an_idle_look_of_another_process_counts_toward_no_lag() {
    let mut old = Parked::new("idle-old-holder", Answers::default(), settled(), 3600);
    set_status(&mut old, "busy");
    assert_eq!(old.visit().step, "wait:status-stale", "its one look");
    let mut new = Parked::new("idle-new-holder", Answers::default(), settled(), 3600);
    set_status(&mut new, "busy");
    assert_ne!(new.sf.pid, old.sf.pid);
    std::fs::copy(
        state_path(&old.opts, SESSION),
        state_path(&new.opts, SESSION),
    )
    .expect("the record the old process's look left");
    drop(old);
    assert_eq!(
        new.visit().step,
        "wait:status-stale",
        "the new agent's first look is one look"
    );
    assert!(new.typed().is_empty(), "{:?}", new.typed());
    assert_eq!(new.visit().step, "announced:1", "its second");
    assert_eq!(new.typed().len(), 1);
    drop(new);

    let mut late = Parked::new("idle-long-before", Answers::default(), settled(), 3600);
    set_status(&mut late, "busy");
    let mut st = load(&late.opts, SESSION).expect("state");
    (st.idle_looks, st.idle_pid, st.idle_at) = (1, late.sf.pid, now_s() - IDLE_RUN_GAP_S - 1);
    save(&late.opts, SESSION, &st);
    assert_eq!(late.visit().step, "wait:status-stale", "no run: one look");
    assert_eq!(late.visit().step, "announced:1", "in a row: its second");
}

/// A user row the harness typed, under its `[aterm harness]` mark.
#[cfg(unix)]
fn harness_row(text: &str) -> String {
    user_row(&format!("[aterm harness] {text}"))
}

/// [`Tasked`] reads a conversation's task on from where its last read ended.
#[cfg(unix)]
#[test]
fn whether_a_conversation_has_a_task_is_read_on_from_where_the_last_read_ended() {
    let dir = scratch("tasked-memo");
    let home = dir.join("home");
    assert_eq!(
        tasked(&home, SESSION, &[]),
        None,
        "no Claude home: nobody can say"
    );
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    assert_eq!(tasked(&home, SESSION, &[]), Some(false), "no projects yet");
    std::fs::write(home.join(".claude/projects"), "").expect("not a directory");
    assert_eq!(tasked(&home, SESSION, &[]), None, "unreadable projects");
    std::fs::remove_file(home.join(".claude/projects")).expect("rm");
    let project = home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("projects");
    assert_eq!(tasked(&home, SESSION, &[]), Some(false), "no message yet");
    let path = project.join(format!("{SESSION}.jsonl"));
    let ready = turn_by("claude-haiku-4-5", "ATERM-UPGRADE-READY-5b2e0fc4");
    let own = [
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        ready.clone(),
        harness_row("Upgraded: carry on"),
        turn_by(
            "claude-haiku-4-5",
            "What would you like me to help you with?",
        ),
    ];
    std::fs::write(&path, own.join("\n") + "\n").expect("transcript");
    let mut memo = Tasked::default();
    assert_eq!(
        memo.of(&home, SESSION, &[]),
        Some(false),
        "the harness's own turns"
    );
    let read = memo.read_to;
    assert_eq!(read, std::fs::metadata(&path).expect("len").len());
    let person = user_row("Staged harness test, please follow exactly.");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    write!(f, "{}", &person[..20]).expect("half");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(false));
    assert_eq!(memo.read_to, read, "a half line is not taken");
    writeln!(f, "{}", &person[20..]).expect("rest");
    assert_eq!(
        memo.of(&home, SESSION, &[]),
        Some(true),
        "asked: a task before its answer"
    );
    writeln!(f, "{}", turn_by("claude-haiku-4-5", "Stage 1 complete.")).expect("answer");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(true));
    std::fs::write(&path, "").expect("emptied");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(true));
    let other = "0badf00d-9999-2222-3333-444455556666";
    std::fs::write(
        project.join(format!("{other}.jsonl")),
        own.join("\n") + "\n",
    )
    .expect("other");
    assert_eq!(memo.of(&home, other, &[]), Some(false));
    let second = home.join(".claude/projects/q");
    std::fs::create_dir_all(&second).expect("second project");
    std::fs::write(second.join(format!("{other}.jsonl")), "").expect("twin");
    assert_eq!(tasked(&home, other, &[]), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in agent in [`TAB`] whose conversation holds `rows` (Pending on the newer build,
/// its screen quiet an hour): the visit's report under `kernel`, the state it left, and how
/// many turns it typed.
#[cfg(unix)]
fn visit_holding(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
) -> (Report, Option<St>, usize) {
    visit_seen(name, rows, dry_run, kernel, 77, None, None)
}

/// [`visit_holding`] with the last look an hour ago at `last_seq` (the screen now at `seq` 77),
/// the tab's `status` reading `status` (`None`: nothing held, no agent verdict), and the tab's
/// supervisor having ledgered `ledger` as typed ([`Opts::aterm_state`]; `None`: no ledger).
#[cfg(unix)]
fn visit_seen(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
) -> (Report, Option<St>, usize) {
    let (r, st, asked) = visit_answered(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        Answers::default(),
    );
    (r, st, asked.iter().filter(|l| l.contains(" turn ")).count())
}

/// [`visit_seen`] on a stand-in answering as `answers` says: every request
/// the visit made, in order, instead of the count of its turns.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_answered(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
) -> (Report, Option<St>, Vec<String>) {
    visit_launched(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        &[],
        &|_| {},
    )
}

/// [`visit_answered`], the agent's argv carrying `launch` after its own
/// words — the flags a person launched it with, which the relaunch carries —
/// and `seed` run over the sweep's options before the visit (the harness's
/// own records a test lays down: a `/model` choice remembered).
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
) -> (Report, Option<St>, Vec<String>) {
    visit_launched_env(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        launch,
        seed,
        &[],
    )
}

/// [`visit_launched`], the agent's exec environment as the visit reads it
/// carrying `env` (`NAME=VALUE` each) beside its own — what a process
/// relaunched under a login of the harness's choosing has, which the
/// stand-in's real environment cannot be given after its start.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched_env(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
    env: &[String],
) -> (Report, Option<St>, Vec<String>) {
    visit_launched_probed(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        launch,
        seed,
        env,
        |pid, start| ParkedProcess {
            script: kernel,
            pid,
            start: Some(start),
            background: Vec::new(),
        },
    )
}

/// The same visit with its last-look process reads injected (`make_kernel`), so the parked
/// fixture and the last-look controls drive one real visit.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched_probed<K: Kernel>(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
    env: &[String],
    make_kernel: impl FnOnce(u32, String) -> K,
) -> (Report, Option<St>, Vec<String>) {
    use crate::supervise::approvals;
    let dir = scratch(name);
    let (sock, asked) = instance_with(&dir, answers);
    if let Some(status) = status {
        std::fs::write(dir.join("t.status"), format!("{status}\n")).expect("status");
    }
    let aterm_state = ledger.map(|typed| {
        let root = dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let rows: Vec<String> = typed
            .iter()
            .map(|text| {
                approvals::Row {
                    rule_id: "continue@v1",
                    outcome: approvals::Outcome::Typed,
                    command: text,
                    reason: "the turn-end policy",
                    box_seq: 1,
                }
                .to_json(1_000, Some(TAB))
            })
            .collect();
        std::fs::write(path, rows.join("\n") + "\n").expect("ledger");
        root
    });
    let opts = Opts {
        sock: Some(sock),
        dry_run,
        aterm_state,
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("projects");
    if !rows.is_empty() {
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            rows.join("\n") + "\n",
        )
        .expect("transcript");
    }
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq,
        seq_since_s: now_s() - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    seed(&opts);
    let table = vec![(kernel.shell, 1, "zsh".into())];
    let files = session_files(&opts.home);
    let mut args = atpkg::caller_shell::process_args(agent.id()).expect("agent argv");
    args.argv.extend(launch.iter().map(|w| (*w).to_string()));
    args.env.extend(env.iter().cloned());
    let stable = make_kernel(sf.pid, squash(&sf.proc_start));
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &table,
        &newer(),
        &stable,
        Some(&args),
        None,
    );
    let saved = load(&opts, SESSION);
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    (r, saved, asked)
}

/// A conversation with no task (fresh, or only the harness's own turns) is restarted afresh, never
/// announced to.
#[cfg(unix)]
#[test]
fn an_upgrade_restarts_a_conversation_with_no_task_afresh_and_announces_to_one_with_a_task() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let person = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let first = vec![own[0].clone(), own[1].clone(), person[0].clone()];
    for (name, rows, want) in [
        ("fresh-none", Vec::new(), "would-restart:fresh"),
        ("fresh-own", own, "would-restart:fresh"),
        ("fresh-person", person, "would-announce"),
        ("fresh-first-prompt", first, "would-announce"),
    ] {
        let (r, _, typed) = visit_holding(name, &rows, true, &Script::new(shell, usize::MAX, None));
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}: a dry run types nothing");
    }
}

/// A Claude home with no `projects/` holds no transcript (Claude Code makes it with the home's
/// first message).
#[cfg(unix)]
#[test]
fn an_upgrade_in_a_claude_home_with_no_projects_restarts_the_session_afresh() {
    let shell = dead_pid();
    let kernel = || Script::new(shell, usize::MAX, None);
    let projects_as = |file: bool| {
        move |opts: &Opts| {
            let projects = opts.home.join(".claude/projects");
            std::fs::remove_dir_all(&projects).expect("no projects yet");
            if file {
                std::fs::write(&projects, "").expect("projects that cannot be listed");
            }
        }
    };
    let visit = |name: &str, file: bool| {
        visit_launched(
            name,
            &[],
            true,
            &kernel(),
            77,
            None,
            None,
            Answers::default(),
            &[],
            &projects_as(file),
        )
    };
    let (r, _, typed) = quietly(|| visit("no-projects-quiet", false));
    assert_eq!(r.step, "would-restart:fresh", "quiet: {r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "{typed:?}");
    let (r, _, typed) = visit("no-projects-ask", false);
    assert_eq!(r.step, "would-restart:fresh", "ask: {r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "{typed:?}");
    let (r, _, _) = quietly(|| visit("unreadable-projects-quiet", true));
    assert_eq!(r.step, "would-restart", "{r:?}");
}

/// A [`Script`] that, at its `at`-th job read, runs `then`.
#[cfg(unix)]
struct Racing<'a> {
    script: Script,
    at: usize,
    then: Box<dyn Fn() + 'a>,
}

#[cfg(unix)]
impl Kernel for Racing<'_> {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        if self.script.reads.get() == self.at {
            (self.then)();
        }
        self.script.job(agent)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.script.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        self.script.terminal(pid)
    }
}

/// A restart never resumes a conversation whose transcript went after its step (Claude Code refuses
/// that `--resume` and exits, leaving the tab without an agent).
#[cfg(unix)]
#[test]
fn a_restart_never_resumes_a_conversation_whose_transcript_went_after_its_step() {
    use super::super::relaunch::CAUSE_UPGRADE_FRESH;
    use std::os::unix::process::ExitStatusExt as _;
    let run = |name: &str, gone: bool| {
        let pending = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            last_seq: 77,
            seq_since_s: now_s() - 3_600,
            ..St::default()
        };
        let mut h = quietly(|| Parked::new(name, Answers::default(), pending, 3_600));
        let transcript = h
            .opts
            .home
            .join(format!(".claude/projects/p/{SESSION}.jsonl"));
        let ended = h.reaped();
        let mut shell = tty_zsh();
        let table = vec![(shell.id(), 1, "zsh".into())];
        let kernel = Racing {
            script: Script::new(shell.id(), usize::MAX, None),
            at: 1,
            then: Box::new(|| {
                if gone {
                    let _ = std::fs::remove_file(&transcript);
                }
            }),
        };
        h.opts.timings.held = Duration::from_millis(100);
        let r = visit(&h.opts, &h.sf, &table, &newer(), &kernel);
        let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{name}: {r:?}");
        let st = load(&h.opts, SESSION).expect("record");
        (st, h.details("terminated"))
    };
    let (st, terminated) = run("resume-gone", true);
    assert!(!st.line.contains("--resume"), "{}", st.line);
    assert_eq!(st.cause, CAUSE_UPGRADE_FRESH);
    assert_eq!(terminated.len(), 1, "{terminated:?}");
    let (st, terminated) = run("resume-kept", false);
    assert!(st.line.contains("'--resume'"), "{}", st.line);
    assert_eq!(st.cause, "");
    assert_eq!(terminated.len(), 1, "{terminated:?}");
}

/// The quiet upgrade (the default) restarts a tasked conversation at an idle point with nothing
/// typed first.
#[cfg(unix)]
#[test]
fn a_quiet_upgrade_restarts_an_idle_conversation_with_nothing_typed_first() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let kernel = || Script::new(shell, usize::MAX, None);
    let (r, _, typed) = quietly(|| visit_holding("quiet-idle", &asked, true, &kernel()));
    assert_eq!(r.step, "would-restart", "{r:?}");
    assert_eq!(typed, 0);
    let (r, _, _) = visit_holding("ask-idle", &asked, true, &kernel());
    assert_eq!(r.step, "would-announce", "{r:?}");
    let just_behind = |opts: &Opts| {
        let mut st = load(opts, SESSION).expect("seeded");
        st.pending_since = now_s();
        std::fs::write(state_path(opts, SESSION), st.to_json()).expect("write");
    };
    for (name, rows, quiet) in [
        ("untold-quiet", &asked, true),
        ("untold-taskless-quiet", &own, true),
        ("untold-taskless-ask", &own, false),
    ] {
        let visit = || {
            visit_launched(
                name,
                rows,
                true,
                &kernel(),
                77,
                None,
                None,
                Answers::default(),
                &[],
                &just_behind,
            )
        };
        let (r, _, typed) = if quiet { quietly(visit) } else { visit() };
        assert_eq!(r.step, "wait:notice-shown", "{name}: {r:?}");
        assert!(!typed.iter().any(|l| l.contains(" turn ")), "{name}");
    }
    let (r, saved, typed) =
        quietly(|| visit_holding("quiet-run", &asked, false, &Script::new(shell, 4, None)));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(typed, 0, "no notice, nothing typed");
    let saved = saved.expect("state");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert!(
        saved.line.contains("--resume"),
        "resumed, not afresh: {}",
        saved.line
    );
    assert!(
        saved.marker.is_empty() && saved.asked.is_empty(),
        "no READY asked"
    );
}

/// The upgrade's relaunch line re-applies exactly the login the agent's exec environment names (an
/// account move's `work-2` stays `work-2`).
#[cfg(unix)]
#[test]
fn the_upgrades_own_relaunch_types_back_the_login_the_agent_runs_under() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let run = |name: &str, env: &[String]| {
        let (r, saved, _) = quietly(|| {
            visit_launched_env(
                name,
                &asked,
                false,
                &Script::new(shell, 4, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                &|_| {},
                env,
            )
        });
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        saved.expect("state")
    };
    let moved = run(
        "upgrade-login-dir",
        &[format!("CLAUDE_SECURESTORAGE_CONFIG_DIR={LOGIN_DIR}")],
    );
    assert!(
        moved.line.contains(LOGIN_WORD) && moved.line.contains("--resume"),
        "{}",
        moved.line
    );
    assert_eq!(moved.login.as_deref(), Some(LOGIN_DIR));
    let named = run(
        "upgrade-login-empty",
        &["CLAUDE_SECURESTORAGE_CONFIG_DIR=".to_string()],
    );
    assert!(
        named.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR='' "),
        "{}",
        named.line
    );
    assert_eq!(named.login.as_deref(), Some(""));
    let plain = run("upgrade-login-none", &[]);
    assert!(
        !plain.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR") && plain.line.contains("--resume"),
        "{}",
        plain.line
    );
    assert_eq!(plain.login, None);
}

/// A quiet restart ([`St::quiet`]) and every account move (`relaunch::carried_quietly`) are carried
/// on with nothing.
#[cfg(unix)]
#[test]
fn a_quiet_restart_is_carried_on_with_nothing() {
    let asked = [
        user_row("Reply with one short sentence saying hello."),
        turn_by("claude-haiku-4-5", "Hello."),
    ];
    let rig = Rig::new("carry-quiet", &asked);
    let mut st = rig.st.clone();
    st.quiet = true;
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert_eq!(st.phase, Phase::Done);
    assert_eq!(rig.details("done:resumed").len(), 1, "said once");
    assert!(!moved("done:resumed"), "the point still stands");
    let rig = Rig::new("carry-account", &asked);
    let mut st = rig.st.clone();
    st.cause = "account:work-2".to_string();
    assert!(!st.quiet);
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert!(!typed("done:resumed"), "no turn of the host's to await");
    let rig = Rig::new("carry-announced", &asked);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_ne!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
    let rig = Rig::new("carry-memory", &asked);
    let mut st = rig.st.clone();
    st.cause = super::super::relaunch::CAUSE_MEMORY.to_string();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
}

/// The resumed build's own warning that something did not come back, as
/// 2.1.300 wrote it, by build `version` at `at` (unix seconds), at `level`.
#[cfg(unix)]
fn resumed_warning(version: &str, at: u64, level: &str) -> String {
    format!(
        r#"{{"type":"system","subtype":"informational","level":"{level}","isMeta":false,"isSidechain":false,"version":"{version}","timestamp":"{}","content":"Artifact comment monitor for https://claude.ai/artifact/x didn't resume: it couldn't restart in this session. Publish the Artifact again to turn automatic replies back on."}}"#,
        aterm_types::rfc3339::format_rfc3339(at)
    )
}

/// A quiet restart carried on as [`a_quiet_restart_is_carried_on_with_nothing`]
/// does, over `rows`: the rig and the record as the carry-on left it.
#[cfg(unix)]
fn quietly_resumed(name: &str, rows: &[String]) -> (Rig, St) {
    let rig = Rig::new(name, rows);
    let mut st = rig.st.clone();
    st.quiet = true;
    st.relaunched_at = now_s().saturating_sub(5);
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:resumed", "{name}: {r:?}");
    (rig, st)
}

/// The quiet restart's look at the tab's idle point
/// ([`super::super::relaunch::restore_look`]), at `now`, the tab's claim on
/// the stand-in's process granted.
#[cfg(unix)]
fn restore_look_at(rig: &Rig, st: &mut St, now: u64) -> Option<Report> {
    super::super::relaunch::restore_look(
        &rig.opts,
        &blank(&rig.sf),
        st,
        SESSION,
        now,
        Some(TAB),
        |_, _, _| true,
    )
}

/// A quiet restart says once what the resumed build did not bring back.
#[cfg(unix)]
#[test]
fn a_quiet_restart_says_once_what_the_resumed_build_did_not_bring_back() {
    let asked = [
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let (rig, mut st) = quietly_resumed("quiet-lost", &asked);
    assert!(st.confirming(), "the look is open");
    assert!(
        st.confirm_by
            <= st
                .done_at
                .saturating_add(super::super::relaunch::RESTORE_LOOK.as_secs()),
        "a short look, never MODEL_WAIT"
    );
    assert_eq!(restore_look_at(&rig, &mut st, now_s()), None, "no row yet");
    assert!(rig.typed().is_empty());
    rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
    let r = restore_look_at(&rig, &mut st, now_s()).expect("decided");
    assert_eq!(r.step, "continued:restore", "{r:?}");
    assert!(
        typed(&r.step) && moved(&r.step),
        "a turn of the harness's own"
    );
    let lines = rig.typed();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(upgrade::HARNESS_MARK) && lines[0].contains("did not come back"),
        "{lines:?}"
    );
    for vendor in ["Artifact", "Publish", "didn't resume", "claude.ai"] {
        assert!(!lines[0].contains(vendor), "no vendor words: {lines:?}");
    }
    assert!(!st.confirming(), "the look is over");
    assert_eq!(rig.details("continued:restore").len(), 1);
    drop(rig);
    let old_warning = [
        asked[0].clone(),
        resumed_warning("2.1.281", now_s().saturating_sub(60), "warning"),
        asked[1].clone(),
    ];
    for (name, before, after) in [
        (
            "quiet-notice",
            asked.to_vec(),
            vec![resumed_warning("2.1.282", now_s(), "notice")],
        ),
        ("quiet-old-build", old_warning.to_vec(), Vec::new()),
        ("quiet-no-row", asked.to_vec(), Vec::new()),
    ] {
        let (rig, mut st) = quietly_resumed(name, &before);
        rig.append(&after);
        assert_eq!(restore_look_at(&rig, &mut st, now_s()), None, "{name}");
        let over = st.confirm_by;
        let r = restore_look_at(&rig, &mut st, over).expect("over");
        assert_eq!(r.step, "done:resumed", "{name}: {r:?}");
        assert!(rig.typed().is_empty(), "{name}: {:?}", rig.typed());
        assert!(!st.confirming(), "{name}");
        assert_eq!(rig.details("done:resumed").len(), 1, "{name}: said once");
    }
    let (rig, mut st) = quietly_resumed("quiet-lost-finished", &finished_rows());
    rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
    let r = restore_look_at(&rig, &mut st, now_s()).expect("decided");
    assert_eq!(r.step, "done:lost", "{r:?}");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    assert!(
        st.outcome.contains("did not come back") && st.outcome.contains("finished its task"),
        "{}",
        st.outcome
    );
    assert_eq!(rig.details("done:lost").len(), 1);
    assert!(!st.confirming());
}

/// The quiet look is the host's to type and a sweep's to end.
#[cfg(unix)]
#[test]
fn the_quiet_look_is_the_hosts_to_type_and_a_sweeps_to_end() {
    let asked = [
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let (rig, st) = quietly_resumed("quiet-host", &asked);
    let tab = Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    assert!(owed(&tab), "the look is owed at the next idle point");
    assert_eq!(resume(&tab).step, "wait:restore");
    let mut over = st.clone();
    over.confirm_by = now_s().saturating_sub(1);
    save(&rig.opts, SESSION, &over);
    let ended = confirmations(&rig.opts);
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].step, "done:resumed");
    assert!(!owed(&tab));
    assert!(rig.typed().is_empty());
    assert_eq!(rig.details("done:resumed").len(), 1, "no second row");
    drop(rig);
    let (rig, st) = quietly_resumed("quiet-sweep", &asked);
    rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
    assert!(confirmations(&rig.opts).is_empty(), "a sweep types nothing");
    let mut late = load(&rig.opts, SESSION).expect("record");
    assert!(late.confirming());
    late.confirm_by = now_s().saturating_sub(STALE_S + 1);
    save(&rig.opts, SESSION, &late);
    let ended = confirmations(&rig.opts);
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].step, "done:lost");
    assert!(rig.typed().is_empty());
    let _ = st;
    drop(rig);
    let rig = Rig::new("quiet-ask", &asked);
    let mut st = rig.st.clone();
    st.relaunched_at = now_s().saturating_sub(5);
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
    let tab = Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    assert_eq!(resume(&tab).step, "wait:model");
    assert_eq!(rig.typed().len(), 1, "the carry-on alone");
}

/// A sweep types the restore line only into a tab whose own autopilot is on.
#[cfg(unix)]
#[test]
fn a_sweep_types_the_restore_line_only_into_a_tab_whose_autopilot_is_on() {
    let asked = [
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let sweep_at = |rig: &Rig, st: &mut St, now: u64| {
        super::super::relaunch::restore_look(
            &rig.opts,
            &blank(&rig.sf),
            st,
            SESSION,
            now,
            None,
            |_, _, _| true,
        )
    };
    let swept = |name: &str, status: &str, rows: &[String]| {
        let (rig, mut st) = quietly_resumed(name, rows);
        std::fs::write(rig.dir.join("t.status"), status).expect("status");
        rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
        let said = sweep_at(&rig, &mut st, now_s());
        (rig, st, said)
    };
    for word in ["cruise", "land"] {
        let name = format!("sweep-restore-{word}");
        let (rig, st, said) = swept(
            &name,
            &format!("OK schema=1 hold=0 hand=- human_ms=- autopilot={word}"),
            &asked,
        );
        assert_eq!(
            said.map(|r| r.step).as_deref(),
            Some("continued:restore"),
            "{name}"
        );
        let lines = rig.typed();
        assert_eq!(lines.len(), 1, "{name}: {lines:?}");
        assert!(lines[0].contains("did not come back"), "{name}: {lines:?}");
        assert!(!st.confirming(), "{name}: the look is over");
        assert_eq!(rig.details("continued:restore").len(), 1, "{name}");
        assert!(confirmations(&rig.opts).is_empty(), "{name}: typed once");
        assert_eq!(rig.typed().len(), 1, "{name}");
    }
    for (name, status) in [
        (
            "sweep-restore-off",
            "OK schema=1 hold=0 hand=- autopilot=off",
        ),
        (
            "sweep-restore-none",
            "OK schema=1 hold=0 hand=- autopilot=-",
        ),
        ("sweep-restore-older", "OK schema=1 hold=0 hand=-"),
        ("sweep-restore-err", "ERR no such session"),
    ] {
        let (rig, mut st, said) = swept(name, status, &asked);
        assert_eq!(said, None, "{name}");
        assert!(rig.typed().is_empty(), "{name}: {:?}", rig.typed());
        assert!(st.confirming(), "{name}: still open");
        let late = st.confirm_by.saturating_add(STALE_S);
        let ended = sweep_at(&rig, &mut st, late).expect("its bound passed");
        assert_eq!(ended.step, "done:lost", "{name}");
        assert!(rig.typed().is_empty(), "{name}");
        assert_eq!(rig.details("done:lost").len(), 1, "{name}");
    }
    let (rig, st, said) = swept(
        "sweep-restore-finished",
        "OK schema=1 hold=0 hand=- autopilot=cruise",
        &finished_rows(),
    );
    assert_eq!(said, None);
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    assert!(st.confirming());
}

/// A pending round that still owes its agent a release is no quiet restart ([`St::quiet`] false).
#[cfg(unix)]
#[test]
fn a_pending_round_that_owes_its_agent_a_release_is_carried_on_not_quiet() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let owing = |to_this: bool| {
        move |opts: &Opts| {
            let sf = session_files(&opts.home)
                .and_then(|files| files.into_iter().find(|f| f.session_id == SESSION))
                .expect("the stand-in's session file");
            let mut st = load(opts, SESSION).expect("seeded");
            st.release = "retargeted".to_string();
            st.tab = TAB.to_string();
            st.notice_pid = if to_this { sf.pid } else { dead_pid() };
            st.notice_start = squash(&sf.proc_start);
            std::fs::write(state_path(opts, SESSION), st.to_json()).expect("write");
        }
    };
    let run = |name: &str, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &asked,
                false,
                &Script::new(shell, 4, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    let (r, saved, typed) = run("owes-here", &owing(true));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert!(!typed.iter().any(|l| l.contains(" turn ")), "nothing typed");
    let saved = saved.expect("state");
    assert!(!saved.quiet, "carried on, not resumed as if never asked");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert_eq!(saved.release, "retargeted", "the release put back");
    for (name, seed) in [
        ("owes-nothing", &(|_: &Opts| {}) as &dyn Fn(&Opts)),
        ("owes-elsewhere", &owing(false)),
    ] {
        let (r, saved, _) = run(name, seed);
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        assert!(saved.expect("state").quiet, "{name}: a quiet restart");
    }
}

/// The tab's own supervisor's words (`keep going`, `answer_text`, found in its loop's ledger under
/// [`Opts::aterm_state`]) are no task.
#[cfg(unix)]
#[test]
fn what_the_tabs_own_supervisor_typed_is_no_task() {
    let shell = dead_pid();
    let cfg = crate::supervise::SupervisorConfig {
        continue_text: "keep going".to_string(),
        ..crate::supervise::SupervisorConfig::default()
    };
    let rows = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
        user_row_at(&cfg.continue_text, "1970-01-01T00:00:01.000Z"),
        turn_by("claude-haiku-4-5", "What would you like me to work on?"),
        user_row_at(&cfg.answer_text, "1970-01-01T00:00:01.000Z"),
        turn_by("claude-haiku-4-5", "Please tell me what to do."),
    ];
    let typed = [cfg.continue_text.as_str(), cfg.answer_text.as_str()];
    for (name, ledger, want) in [
        ("ours-ledgered", Some(&typed[..]), "would-restart:fresh"),
        ("ours-unknown", None, "would-announce"),
        ("ours-other", Some(&["carry on"][..]), "would-announce"),
    ] {
        let (r, _, typed) = visit_seen(
            name,
            &rows,
            true,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            ledger,
        );
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}: a dry run types nothing");
    }
}

/// The tab's ledger spans every conversation it held.
#[cfg(unix)]
#[test]
fn the_loops_words_in_another_conversation_are_no_turn_of_this_one() {
    let shell = dead_pid();
    let cfg = crate::supervise::SupervisorConfig {
        continue_text: "keep going".to_string(),
        ..crate::supervise::SupervisorConfig::default()
    };
    let rows = vec![
        user_row_at(&cfg.continue_text, "2026-09-30T12:00:00.000Z"),
        turn_by("claude-haiku-4-5", "Picking up the plan where it stopped."),
    ];
    let (r, _, typed) = visit_seen(
        "another-conversation",
        &rows,
        true,
        &Script::new(shell, usize::MAX, None),
        77,
        None,
        Some(&[cfg.continue_text.as_str()][..]),
    );
    assert_eq!(r.step, "would-announce", "{r:?}");
    assert_eq!(typed, 0, "a dry run types nothing");
}

/// The fresh restart is the upgrade's restart without the notice and without `--resume`.
#[cfg(unix)]
#[test]
fn a_fresh_restart_types_nothing_plans_no_resume_and_keeps_every_last_look() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let (r, saved, typed) =
        visit_holding("fresh-restart", &own, false, &Script::new(shell, 4, None));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(typed, 0, "no notice, nothing typed");
    let saved = saved.expect("state");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert!(saved.cause.is_empty(), "{:?}", saved.cause);
    assert!(saved.line.contains("/nonexistent/claude"), "{}", saved.line);
    assert!(!saved.line.contains("--resume"), "afresh: {}", saved.line);
}

/// The upgrade resumes in the mode the tab's pill shows.
#[cfg(unix)]
#[test]
fn an_upgrade_resumes_in_the_mode_the_pill_shows_not_the_transcripts_older_one() {
    let shell = dead_pid();
    let mode_row = |mode: &str| {
        format!(r#"{{"type":"permission-mode","permissionMode":"{mode}","sessionId":"{SESSION}"}}"#)
    };
    let rows = |mode: &str| {
        vec![
            harness_row("Claude Code 2.1.283 (managed) is installed"),
            mode_row(mode),
            turn_by("claude-haiku-4-5", "I cannot stop now."),
        ]
    };
    let rule = "─".repeat(20);
    let auto_pill = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    );
    let bypass = ["--dangerously-skip-permissions"];
    for (name, transcript, screen, launch, want, gone) in [
        (
            "pill-auto-over-bypass",
            "bypassPermissions",
            auto_pill.clone(),
            &bypass[..],
            Some("'--permission-mode' 'auto'"),
            Some("'--dangerously-skip-permissions'"),
        ),
        (
            "no-pill-transcript-plan",
            "plan",
            idle_screen(),
            &[][..],
            Some("'--permission-mode' 'plan'"),
            Some("'auto'"),
        ),
    ] {
        let (r, saved, _) = visit_launched(
            name,
            &rows(transcript),
            false,
            &Script::new(shell, 4, None),
            77,
            None,
            None,
            Answers {
                screen,
                ..Answers::default()
            },
            launch,
            &|_| {},
        );
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        let line = saved.expect("state").line;
        if let Some(want) = want {
            assert!(line.contains(want), "{name}: {line}");
        }
        if let Some(gone) = gone {
            assert!(!line.contains(gone), "{name}: {line}");
        }
    }
}

/// The announcement plans its relaunch line in the mode the pill shows, as the restart does, and
/// keeps the line.
#[cfg(unix)]
#[test]
fn an_announcement_is_planned_in_the_mode_the_pill_shows() {
    let shell = dead_pid();
    let rows = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        format!(
            r#"{{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"{SESSION}"}}"#
        ),
        user_row("fix the parser"),
        turn_by("claude-haiku-4-5", "Fixed."),
    ];
    let rule = "─".repeat(20);
    let auto_pill = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    );
    for (name, screen, want, gone) in [
        (
            "announce-pill-auto",
            auto_pill,
            "'--permission-mode' 'auto'",
            "'--dangerously-skip-permissions'",
        ),
        (
            "announce-no-pill",
            idle_screen(),
            "'--dangerously-skip-permissions'",
            "'--permission-mode'",
        ),
    ] {
        let (r, saved, _) = visit_launched(
            name,
            &rows,
            false,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            None,
            Answers {
                screen,
                ..Answers::default()
            },
            &["--dangerously-skip-permissions"],
            &|_| {},
        );
        assert_eq!(r.step, "announced:1", "{name}: {r:?}");
        let line = saved.expect("state").line;
        assert!(line.contains(want), "{name}: {line}");
        assert!(!line.contains(gone), "{name}: {line}");
    }
}

/// A model chosen by hand (`/model`, answered on since) rides the relaunch line when the list moves
/// none.
#[cfg(unix)]
#[test]
fn an_upgrade_carries_a_model_chosen_by_hand_when_the_list_moves_none() {
    let shell = dead_pid();
    let later = "2099-01-01T00:00:00Z";
    let task = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        user_row("fix the parser"),
        answer_at("claude-opus-5-5", later),
    ];
    let fresh = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        answer_at("claude-opus-5-5", later),
    ];
    let launch = ["--model", "claude-sonnet-5-5"];
    let chose = |opts: &Opts| chose_by_hand(opts, "claude-opus-5-5");
    for (name, rows, kernel, step, chosen) in [
        ("hand-announce", &task, usize::MAX, "announced:1", true),
        ("hand-fresh", &fresh, 4, "wait:changed-before-signal", true),
        ("nobody-announce", &task, usize::MAX, "announced:1", false),
        (
            "nobody-fresh",
            &fresh,
            4,
            "wait:changed-before-signal",
            false,
        ),
    ] {
        let seed: &dyn Fn(&Opts) = if chosen { &chose } else { &|_| {} };
        let (r, saved, _) = visit_launched(
            name,
            rows,
            false,
            &Script::new(shell, kernel, None),
            77,
            None,
            None,
            Answers::default(),
            &launch,
            seed,
        );
        assert_eq!(r.step, step, "{name}: {r:?}");
        let saved = saved.expect("state");
        assert!(saved.model_list.is_empty(), "{name}: the list moved none");
        let (want, gone) = if chosen {
            ("'claude-opus-5-5'", "'claude-sonnet-5-5'")
        } else {
            ("'claude-sonnet-5-5'", "'claude-opus-5-5'")
        };
        assert!(saved.line.contains(want), "{name}: {}", saved.line);
        assert!(!saved.line.contains(gone), "{name}: {}", saved.line);
    }
}

/// A hand `/model` choice on a bucket's fallback is relaunched and, once the signal is sent, saved
/// as the fallback's origin.
#[cfg(unix)]
#[test]
fn an_upgrade_restart_makes_the_hand_choice_it_carries_the_fallbacks_origin() {
    let shell = dead_pid();
    let origin = "claude-fable-5-1";
    let hand = "claude-opus-5-5";
    let later = "2099-01-01T00:00:00Z";
    let rows = |model: &str| {
        vec![
            harness_row("Claude Code 2.1.283 (managed) is installed"),
            answer_at(model, later),
        ]
    };
    let seed = |chose: bool| {
        move |opts: &Opts| {
            let mut st = load(opts, SESSION).expect("state");
            st.fallback_from = Some(origin.to_string());
            save(opts, SESSION, &st);
            if chose {
                chose_by_hand(opts, hand);
            }
        }
    };
    let refused = Answers {
        signal: "ERR busy",
        ..Answers::default()
    };
    let cases = [
        (
            "origin-hand-sent",
            true,
            usize::MAX,
            Answers::default(),
            None,
            hand,
        ),
        (
            "origin-nobody-sent",
            false,
            usize::MAX,
            Answers::default(),
            None,
            origin,
        ),
        (
            "origin-hand-changed",
            true,
            4,
            Answers::default(),
            Some("wait:changed-before-signal"),
            origin,
        ),
        (
            "origin-hand-refused",
            true,
            usize::MAX,
            refused,
            Some("wait:signal-held"),
            origin,
        ),
    ];
    std::thread::scope(|scope| {
        let runs: Vec<_> = cases
            .into_iter()
            .map(|(name, chose, foreground, answers, step, want)| {
                let seed = seed(chose);
                scope.spawn(move || {
                    let model = if chose { hand } else { "claude-sonnet-5-5" };
                    let (r, saved, asked) = visit_launched(
                        name,
                        &rows(model),
                        false,
                        &Script::new(shell, foreground, None),
                        77,
                        None,
                        None,
                        answers,
                        &["--model", "claude-sonnet-5-5"],
                        &seed,
                    );
                    let signalled = asked.iter().any(|l| l.contains(" signal "));
                    match step {
                        Some(step) => assert_eq!(r.step, step, "{name}: {r:?}"),
                        None => assert!(signalled, "{name}: {r:?}"),
                    }
                    let saved = saved.expect("state");
                    assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
                    assert!(
                        saved.line.contains(&format!("'--model' '{model}'")),
                        "{name}: {}",
                        saved.line
                    );
                })
            })
            .collect();
        for run in runs {
            run.join().expect("case");
        }
    });
}

/// Unknown process identity, running background work or an unreadable process table vetoes the
/// restart's last look (`wait:changed`).
#[cfg(unix)]
#[test]
fn the_restart_last_look_keeps_unknown_identity_and_background_work_fenced() {
    let shell = dead_pid();
    for (name, known, background, expected) in [
        ("last-look-stable", true, Vec::new(), "wait:signal-held"),
        ("last-look-unknown", false, Vec::new(), "wait:changed"),
        (
            "last-look-work",
            true,
            vec!["sh".to_string()],
            "wait:changed",
        ),
        (
            "last-look-unreadable",
            true,
            vec![UNREADABLE_TABLE.to_string()],
            "wait:changed",
        ),
    ] {
        let script = Script::new(shell, usize::MAX, None);
        let (report, saved, asked) = visit_launched_probed(
            name,
            &[
                harness_row("Claude Code 2.1.283 (managed) is installed"),
                answer_at("claude-opus-5-5", "2099-01-01T00:00:00Z"),
            ],
            false,
            &script,
            77,
            None,
            None,
            Answers {
                signal: "ERR busy",
                ..Answers::default()
            },
            &["--model", "claude-sonnet-5-5"],
            &|opts| {
                let mut st = load(opts, SESSION).expect("state");
                st.fallback_from = Some("claude-fable-5-1".to_string());
                save(opts, SESSION, &st);
                chose_by_hand(opts, "claude-opus-5-5");
            },
            &[],
            |pid, start| ParkedProcess {
                script: &script,
                pid,
                start: known.then_some(start),
                background,
            },
        );
        assert_eq!(report.step, expected, "{name}: {report:?}");
        assert_eq!(
            asked.iter().any(|line| line.contains(" signal ")),
            known && name == "last-look-stable",
            "{name}: {asked:?}"
        );
        assert_eq!(
            saved.expect("state").fallback_from.as_deref(),
            Some("claude-fable-5-1")
        );
    }
}

/// A restart takes the tab's hand ([`Hand`]) before its last look.
#[cfg(unix)]
#[test]
fn a_restart_holds_the_tab_from_its_last_look_and_gives_it_back_when_it_stops() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let acquire = format!(
        "lease acquire ttl={HAND_TTL_MS} holder={} hard",
        hand_name()
    );
    let release = format!("lease release holder={}", hand_name());
    let (r, saved, asked) = visit_answered(
        "hand-taken",
        &own,
        false,
        &Script::new(shell, 4, None),
        77,
        None,
        None,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000",
            ..Answers::default()
        },
    );
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let at = |needle: &str| asked.iter().position(|l| l.ends_with(needle));
    let taken = at(&acquire).unwrap_or_else(|| panic!("the hand taken: {asked:#?}"));
    let given = at(&release).unwrap_or_else(|| panic!("the hand given back: {asked:#?}"));
    assert!(taken < given, "{asked:#?}");
    for read in [" text ", " status"] {
        assert!(
            asked[taken..given].iter().any(|l| l.contains(read)),
            "{read} under the hand: {asked:#?}"
        );
    }
    assert!(!asked.iter().any(|l| l.contains(" signal ")), "{asked:#?}");
    assert_eq!(turns_in(&asked), 0, "nothing typed");
    let (r, saved, asked) = visit_answered(
        "hand-theirs",
        &own,
        false,
        &Script::new(shell, usize::MAX, None),
        77,
        None,
        None,
        Answers {
            lease: "ERR lease held holder=orchestrator expires_in_ms=5000",
            ..Answers::default()
        },
    );
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let taken = at_in(&asked, &acquire).unwrap_or_else(|| panic!("asked: {asked:#?}"));
    assert!(
        !asked[taken..].iter().any(|l| l.contains(" text ")),
        "no last look: {asked:#?}"
    );
    assert!(!asked.iter().any(|l| l.contains(" signal ")), "{asked:#?}");
    assert!(!asked.iter().any(|l| l.ends_with(&release)), "{asked:#?}");
    for blocked in [true, false] {
        let h = Parked::new(
            if blocked {
                "claim-unwritten"
            } else {
                "claim-written"
            },
            Answers {
                lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
                signal: "ERR busy person human_ms=300",
                ..Answers::default()
            },
            St::default(),
            3600,
        );
        let mut st = announced_to(&h, "ATERM-UPGRADE-READY-0badf00d");
        st.owe_release("void");
        st.fallback_from = Some("claude-fable-5-1".to_string());
        let prior = st.clone();
        let record = state_path(&h.opts, SESSION);
        let kept = record.join("kept");
        if blocked {
            std::fs::remove_file(&record).expect("remove the owned record");
            std::fs::create_dir(&record).expect("a directory at the claim path");
            std::fs::write(&kept, b"keep the existing destination").expect("owned marker");
        }
        let shell = dead_pid();
        let script = Script::new(shell, usize::MAX, None);
        let kernel = ParkedProcess {
            script: &script,
            pid: h.sf.pid,
            start: Some(h.sf.proc_start.clone()),
            background: Vec::new(),
        };
        let argv = vec!["/opt/claude/bin/claude".to_string()];
        let target = newer().managed.expect("target");
        let how = Restarting {
            argv: &argv,
            target: &target,
            host_claim: None,
            grace: 120,
            fresh: false,
            quiet: false,
            lagging: false,
            mode: None,
            model: None,
            login: None,
            home: None,
            stored_login: false,
            profile: None,
        };
        let report = Report {
            pid: h.sf.pid,
            tab: TAB.to_string(),
            session: SESSION.to_string(),
            from: "1.0.0".to_string(),
            to: "9.9.9(managed)".to_string(),
            step: String::new(),
        };
        let mut c = connect(&h.opts, TAB).expect("owned control socket");
        let result = super::restart(
            &h.opts,
            report,
            &mut st,
            &mut c,
            TAB,
            &h.sf,
            &[(shell, 1, "zsh".into())],
            &kernel,
            &how,
        );
        assert_eq!(
            result.step,
            if blocked {
                "wait:claim-unwritten"
            } else {
                "wait:signal-held"
            },
            "{result:?}"
        );
        let asked = h.asked.lock().expect("requests").clone();
        assert_eq!(
            asked.iter().any(|line| line.contains(" signal ")),
            !blocked,
            "{asked:#?}"
        );
        assert_eq!(turns_in(&asked), 0, "no relaunch typed");
        assert!(
            asked.last().is_some_and(|line| line.ends_with(&release)),
            "the hand returned: {asked:#?}"
        );
        assert_eq!(st.phase, prior.phase);
        assert_eq!(st.marker, prior.marker);
        assert_eq!(st.markers, prior.markers);
        assert_eq!(st.release, prior.release);
        assert_eq!(st.fallback_from, prior.fallback_from);
        assert!(st.cause.is_empty() && st.back_line.is_empty());
        if blocked {
            assert_eq!(
                std::fs::read(&kept).expect("destination retained"),
                b"keep the existing destination"
            );
        } else {
            assert_eq!(load(&h.opts, SESSION).expect("record").phase, prior.phase);
        }
    }
}

/// A key (`human_ms` inside the grace) or an unread prompt (`input=pending`) after the restart's
/// first look stops it before its signal.
#[cfg(unix)]
#[test]
fn a_key_or_a_prompt_after_the_first_look_stops_the_restart_before_its_signal() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let release = format!("lease release holder={}", hand_name());
    let restart = |name: &str, after_first_look: Option<&'static str>, signal: &'static str| {
        visit_answered(
            name,
            &own,
            false,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            None,
            Answers {
                lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
                after_first_look,
                signal,
                ..Answers::default()
            },
        )
    };
    for (name, later) in [
        ("look-person", "OK schema=1 hold=0 hand=- human_ms=500"),
        ("look-prompt", "OK schema=1 hold=0 hand=- input=pending"),
    ] {
        let (r, saved, asked) = restart(name, Some(later), "");
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        let saved = saved.expect("state");
        assert_eq!(saved.phase, Phase::Pending, "{name}: back as it was");
        assert!(saved.cause.is_empty(), "{name}: {saved:?}");
        assert!(
            !asked.iter().any(|l| l.contains(" signal ")),
            "{name}: {asked:#?}"
        );
        assert!(
            asked.last().is_some_and(|l| l.ends_with(&release)),
            "{name}: the hand given back: {asked:#?}"
        );
    }
    let (r, saved, asked) = restart("look-signal", None, "ERR busy person human_ms=300");
    assert_eq!(r.step, "wait:signal-held", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let signal = asked
        .iter()
        .find(|l| l.contains(" signal "))
        .unwrap_or_else(|| panic!("the signal asked: {asked:#?}"));
    assert!(signal.ends_with(" quiet=120"), "{signal}");
    assert!(
        asked.last().is_some_and(|l| l.ends_with(&release)),
        "the hand given back: {asked:#?}"
    );
}

/// A refused signal takes back the model ask its restart recorded, and the earlier ask it replaced
/// stands again, time and all.
#[cfg(unix)]
#[test]
fn a_refused_signal_takes_back_the_model_it_asked() {
    let dir = scratch("ask-refused");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
            signal: "ERR busy person human_ms=300",
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let body = [
        ASKED.to_string(),
        turn_by("claude-opus-5", &format!("Saved.\\n{marker}")),
    ]
    .join("\n")
        + "\n";
    std::fs::write(project.join(format!("{SESSION}.jsonl")), &body).expect("transcript");
    let shell = dead_pid();
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        model_list: "claude-opus-5-5".to_string(),
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
    let earlier = ModelRecord {
        set: "claude-fable-5-1".to_string(),
        set_at: now - 10,
        ..ModelRecord::default()
    };
    save_model_record(&opts, SESSION, &earlier);
    let args = atpkg::caller_shell::process_args(pid).expect("agent argv");
    let files = session_files(&opts.home);
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &[(shell, 1, "zsh".into())],
        &newer(),
        &Script::new(shell, usize::MAX, None),
        Some(&args),
        None,
    );
    let record = load_model_record(&opts, SESSION);
    let saved = load(&opts, SESSION).expect("state");
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.step, "wait:signal-held", "{r:?}\n{asked:#?}");
    assert!(
        saved.line.contains("'--model' 'claude-opus-5-5'"),
        "the restart asked the model: {}",
        saved.line
    );
    assert_eq!(
        (record.set.as_str(), record.set_at),
        ("claude-fable-5-1", now - 10),
        "the refused restart's ask taken back, the one before it standing"
    );
    assert!(record.failed.is_empty(), "{record:?}");
    assert_eq!(
        (saved.model_list.as_str(), saved.marker.as_str()),
        ("claude-opus-5-5", marker),
        "the round, its model and its READY stand"
    );
}

/// The restart in place records a model ask only once the relaunch line carrying it is typed (an
/// ask recorded with no line would settle failed and bar the model).
#[cfg(unix)]
#[test]
fn a_restart_in_place_records_its_model_ask_only_with_the_line_that_asks_it() {
    let case = |name: &str, signal: &'static str, roster: bool| {
        let dir = scratch(name);
        let mut shell = tty_zsh();
        let who = format!(
            "OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={}",
            shell.id()
        );
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                signal,
                who: roster.then_some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let mut agent = Some(parked_bare().spawn().expect("agent"));
        let pid = agent.as_ref().map_or(0, aterm_uds::spawnfd::Child::id);
        wait_exec(pid);
        register(&opts.home, pid, SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        write_transcript(
            &opts.home,
            &[answer_at("claude-opus-5", "2099-01-01T00:00:00Z")],
        );
        let cat = opts.home.join(".claude/cache/model-catalog");
        std::fs::create_dir_all(&cat).expect("catalog dir");
        std::fs::write(
            cat.join("a-cc.json"),
            r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"main"},{"id":"claude-opus-5","section":"main"}]}}}"#,
        )
        .expect("catalog");
        let snap = Snapshot {
            stored_login: false,
            profile: Some(live_profile(&opts, None)),
            tab: TAB.to_string(),
            pid,
            start: kernel_start(pid).expect("lstart"),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("1.0.0".to_string()),
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let ended = signal.is_empty().then(|| {
            let mut sent = agent.take().expect("the agent");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(sent.wait());
            });
            rx
        });
        let opts = Opts {
            timings: crate::harness::Timings {
                held: Duration::from_millis(100),
                ..opts.timings
            },
            ..opts
        };
        let restart = || {
            crate::harness::relaunch::restart_with(
                &opts,
                &Restart::Memory,
                Ok(snap.clone()),
                &Script::new(shell.id(), usize::MAX, None),
            )
        };
        let mut r = restart();
        if let Some(ended) = &ended {
            assert!(
                ended.recv_timeout(HANG_DETECTOR).is_ok(),
                "{name}: the agent ended"
            );
            r = restart();
        }
        let record = load_model_record(&opts, SESSION);
        let typed = turns(&asked);
        if let Some(mut parked) = agent {
            let _ = parked.kill();
            let _ = parked.wait();
        }
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r, record, typed)
    };
    let case = &case;
    side_by_side(vec![
        Box::new(move || {
            let (r, record, typed) = case("ask-refused", "ERR busy person human_ms=300", false);
            assert_eq!(r.step, "wait:signal-held", "{r:?}");
            assert_eq!(typed, 0);
            assert_eq!(
                (record.set.as_str(), record.failed.len()),
                ("", 0),
                "nothing relaunched asked it: {record:?}"
            );
        }),
        Box::new(move || {
            let (r, record, typed) = case("ask-untyped", "", false);
            assert_eq!(r.step, "wait:tab-ownership-changed", "{r:?}");
            assert_eq!(typed, 0);
            assert_eq!(
                (record.set.as_str(), record.failed.len()),
                ("", 0),
                "no line asked it: {record:?}"
            );
        }),
        Box::new(move || {
            let (r, record, typed) = case("ask-typed", "", true);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(record.set, "claude-opus-5-5", "{r:?} {record:?}");
            assert!(record.failed.is_empty(), "{record:?}");
        }),
    ]);
}

/// The first request of `asked` that ends with `needle`.
#[cfg(unix)]
fn at_in(asked: &[String], needle: &str) -> Option<usize> {
    asked.iter().position(|l| l.ends_with(needle))
}

/// A relaunched conversation containing only harness turns settles as `done:taskless` with nothing
/// typed.
#[cfg(unix)]
#[test]
fn a_conversation_with_no_task_is_carried_on_with_nothing() {
    let own = [
        harness_row("Relaunched: Claude Code exited without anyone asking"),
        turn_by(
            "claude-haiku-4-5",
            "What would you like me to help you with?",
        ),
    ];
    let rig = Rig::new("carry-taskless", &own);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:taskless", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert_eq!(st.phase, Phase::Done);
    assert_eq!(rig.details("done:taskless").len(), 1, "said once");
}

/// A relaunch that typed nothing still looks at what did not come back.
#[cfg(unix)]
#[test]
fn a_relaunchs_lost_monitor_is_looked_at_and_recorded() {
    let taskless = [
        harness_row("Relaunched: Claude Code exited without anyone asking"),
        turn_by(
            "claude-opus-5-5",
            "What would you like me to help you with?",
        ),
    ];
    let on_tab = |rig: &Rig| Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    for cause in [CAUSE_EXIT, CAUSE_HOST] {
        for (word, rows, why) in [
            ("done:finished", finished_rows(), "finished its task"),
            (
                "done:taskless",
                taskless.to_vec(),
                "nobody has asked it anything",
            ),
        ] {
            let name = format!("lost-{cause}-{word}");
            let rig = Rig::new(&name, &rows);
            let mut st = rig.st.clone();
            st.cause = cause.to_string();
            let r = rig.carry_on_from(&mut st, MODEL_WAIT);
            assert_eq!(r.step, word, "{name}: {r:?}");
            assert!(rig.typed().is_empty(), "{name}");
            assert!(owed(&on_tab(&rig)), "{name}: the look is open");
            rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
            assert!(
                confirmations(&rig.opts).is_empty(),
                "{name}: a sweep waits for the tab's host"
            );
            let r = resume(&on_tab(&rig));
            assert_eq!(r.step, "done:lost", "{name}: {r:?}");
            assert!(rig.typed().is_empty(), "{name}: {:?}", rig.typed());
            let st = load(&rig.opts, SESSION).expect("record");
            assert!(
                st.outcome.contains("did not come back") && st.outcome.contains(why),
                "{name}: {}",
                st.outcome
            );
            assert_eq!(rig.details("done:lost").len(), 1, "{name}");
            assert!(!owed(&on_tab(&rig)), "{name}: the look is over");
        }
    }
    let rig = Rig::new("lost-sweep", &finished_rows());
    let mut st = rig.st.clone();
    st.cause = CAUSE_HOST.to_string();
    assert_eq!(rig.carry_on_from(&mut st, MODEL_WAIT).step, "done:finished");
    rig.append(&[resumed_warning("2.1.282", now_s(), "warning")]);
    let mut late = load(&rig.opts, SESSION).expect("record");
    late.confirm_by = now_s().saturating_sub(STALE_S + 1);
    save(&rig.opts, SESSION, &late);
    let ended = confirmations(&rig.opts);
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].step, "done:lost");
    assert!(rig.typed().is_empty());
    drop(rig);
    let asked = [
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let rig = Rig::new("lost-tasked", &asked);
    let mut st = rig.st.clone();
    st.cause = CAUSE_EXIT.to_string();
    assert_eq!(rig.carry_on_from(&mut st, MODEL_WAIT).step, "continued");
    let lines = rig.typed();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("may not have come back"), "{lines:?}");
    drop(rig);
    let old_warning = {
        let mut rows = finished_rows();
        rows.insert(
            1,
            resumed_warning("2.1.281", now_s().saturating_sub(60), "warning"),
        );
        rows
    };
    for (name, before, after) in [
        (
            "lost-notice",
            finished_rows(),
            vec![resumed_warning("2.1.282", now_s(), "notice")],
        ),
        ("lost-old-build", old_warning, Vec::new()),
        ("lost-no-row", finished_rows(), Vec::new()),
    ] {
        let rig = Rig::new(name, &before);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        assert_eq!(rig.carry_on_from(&mut st, MODEL_WAIT).step, "done:finished");
        rig.append(&after);
        assert_eq!(resume(&on_tab(&rig)).step, "wait:restore", "{name}");
        let mut over = load(&rig.opts, SESSION).expect("record");
        over.confirm_by = now_s().saturating_sub(1);
        save(&rig.opts, SESSION, &over);
        assert_eq!(resume(&on_tab(&rig)).step, "done:finished", "{name}");
        assert!(rig.typed().is_empty(), "{name}: {:?}", rig.typed());
        assert!(rig.details("done:lost").is_empty(), "{name}");
        assert_eq!(rig.details("done:finished").len(), 1, "{name}: said once");
        assert!(!owed(&on_tab(&rig)), "{name}");
    }
}

/// A main-chain user row Claude Code asked itself (`origin.kind` `kind`), as
/// 2.1.300 records one: a background task's notification, its usage-limit
/// continuation (`isMeta`), a peer session's message (`isMeta`).
#[cfg(unix)]
fn own_row(kind: &str, meta: bool, text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"user","isMeta":{meta},"message":{{"role":"user","content":"{text}"}},"origin":{{"kind":"{kind}"}},"promptSource":"system"}}"#
    )
}

/// The tab a cold-restored [`TAB`] reopened from.
#[cfg(unix)]
const OLD_TAB: &str = "s-00000000000000000001";

/// `text` in tab `tab`'s loop ledger under `root`, as typed at `ts_ms`.
#[cfg(unix)]
fn ledger_typed(root: &Path, tab: &str, text: &str, ts_ms: i64) {
    ledger_typed_as(root, tab, text, ts_ms, "continue@v1");
}

/// [`ledger_typed`] under the rule `rule_id` (a wall's retry is ledgered under
/// its own rule, which the record reads: `upgrade::FinishedScan`).
fn ledger_typed_as(root: &Path, tab: &str, text: &str, ts_ms: i64, rule_id: &str) {
    use crate::supervise::approvals;
    let path = approvals::path_under(root, Some(tab)).expect("ledger dir");
    let row = approvals::Row {
        rule_id,
        outcome: approvals::Outcome::Typed,
        command: text,
        reason: "the turn-end policy",
        box_seq: 1,
    }
    .to_json(ts_ms, Some(tab));
    std::fs::write(path, row + "\n").expect("ledger");
}

/// A cold-restored tab reads its earlier tab's loop turns as the harness's.
#[cfg(unix)]
#[test]
fn a_cold_restored_tab_reads_its_earlier_tabs_loop_turns_as_the_harnesss() {
    let at = "2026-10-15T10:00:00.000Z";
    let t_ms = i64::try_from(super::super::upgrade_models::parse_utc(at).expect("a time"))
        .expect("seconds")
        * 1000;
    let rows = vec![
        user_row("Fix the lexer and land it."),
        land_row(),
        turn_by("claude-opus-5-5", LANDED_JSON),
        user_row_at("keep going", at),
        turn_by(
            "claude-opus-5-5",
            "Nothing further: the lexer fix is landed.",
        ),
    ];
    let case = |name: &str, tabs: &[&str], ts_ms: i64| {
        let mut rig = Rig::new(name, &rows);
        let root = rig.dir.join("aterm-state");
        ledger_typed(&root, OLD_TAB, "keep going", ts_ms);
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.tabs = tabs.iter().map(|t| (*t).to_string()).collect();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        let typed = rig.typed().len();
        let said = rig.details("done:finished").len();
        (r.step, typed, said)
    };
    assert_eq!(
        case("cold-tab-earlier", &[OLD_TAB], t_ms),
        ("done:finished".to_string(), 0, 1)
    );
    assert_eq!(
        case("cold-tab-unknown", &[], t_ms),
        ("continued".to_string(), 1, 0),
        "no earlier tab known: the bug this guards"
    );
    assert_eq!(
        case("cold-tab-person", &[OLD_TAB], t_ms + 61_000),
        ("continued".to_string(), 1, 0),
        "a person's own `keep going` reopens the task"
    );
}

/// A cold restore's note of the tab it reopened from is read with no record naming it.
#[cfg(unix)]
#[test]
fn a_cold_restores_note_of_its_earlier_tab_is_read_with_no_record() {
    const MID_TAB: &str = "s-00000000000000000002";
    let at = "2026-10-15T10:00:00.000Z";
    let t_ms = i64::try_from(super::super::upgrade_models::parse_utc(at).expect("a time"))
        .expect("seconds")
        * 1000;
    let rows = vec![
        user_row("Fix the lexer and land it."),
        land_row(),
        turn_by("claude-opus-5-5", LANDED_JSON),
        user_row_at("keep going", at),
        turn_by(
            "claude-opus-5-5",
            "Nothing further: the lexer fix is landed.",
        ),
    ];
    let case = |name: &str, notes: &[(&str, &str)]| {
        let mut rig = Rig::new(name, &rows);
        let root = rig.dir.join("aterm-state");
        ledger_typed(&root, OLD_TAB, "keep going", t_ms);
        rig.opts.aterm_state = Some(root);
        for (tab, was) in notes {
            note_earlier_tab(&rig.opts, tab, was);
        }
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.tabs = Vec::new();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    assert_eq!(
        case("noted-earlier", &[(TAB, OLD_TAB)]),
        ("done:finished".to_string(), 0)
    );
    assert_eq!(
        case("noted-twice", &[(TAB, MID_TAB), (MID_TAB, OLD_TAB)]),
        ("done:finished".to_string(), 0),
        "followed tab to tab"
    );
    assert_eq!(
        case("noted-none", &[]),
        ("continued".to_string(), 1),
        "nothing noted: the reading before the note"
    );
    assert_eq!(
        case("noted-kept-sid", &[(TAB, TAB)]),
        ("continued".to_string(), 1),
        "a kept sid notes nothing"
    );
}

/// A tab an older cold restore minted is linked to its earlier tab by the upgrade ledger.
#[cfg(unix)]
#[test]
fn an_older_restores_tab_is_linked_to_its_earlier_tab_by_the_upgrade_ledger() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let at = "2026-10-15T10:00:00.000Z";
    let t_s = super::super::upgrade_models::parse_utc(at).expect("a time");
    let t_ms = i64::try_from(t_s).expect("seconds") * 1000;
    let rows = vec![
        user_row("Fix the lexer and land it."),
        land_row(),
        turn_by("claude-opus-5-5", LANDED_JSON),
        user_row_at("keep going", at),
        turn_by(
            "claude-opus-5-5",
            "Nothing further: the lexer fix is landed.",
        ),
    ];
    let row = |t: u64, tab: &str, session: &str, step: &str| {
        format!(
            r#"{{"t":{t},"pid":1,"tab":"{tab}","session":"{session}","from":"2.1.281","to":"2.1.283","step":"{step}","detail":""}}"#
        )
    };
    let case = |name: &str, ledger: &[String]| {
        let mut rig = Rig::new(name, &rows);
        let root = rig.dir.join("aterm-state");
        ledger_typed(&root, OLD_TAB, "keep going", t_ms);
        rig.opts.aterm_state = Some(root);
        std::fs::create_dir_all(state_dir(&rig.opts)).expect("state");
        std::fs::write(
            ledger_path(&rig.opts),
            ledger.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .expect("the upgrade ledger");
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.tabs = Vec::new();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    let moved_there = row(t_s - 3_600, OLD_TAB, SESSION, "done");
    assert_eq!(
        case("ledgered-earlier", std::slice::from_ref(&moved_there)),
        ("done:finished".to_string(), 0)
    );
    assert_eq!(
        case(
            "ledgered-moved-on-later",
            &[
                moved_there.clone(),
                row(t_s + 600, OLD_TAB, OTHER, "announced")
            ]
        ),
        ("done:finished".to_string(), 0),
        "it moved on after the `keep going`"
    );
    assert_eq!(
        case(
            "ledgered-other-conversation",
            &[row(t_s - 3_600, OLD_TAB, OTHER, "done")]
        ),
        ("continued".to_string(), 1),
        "another conversation's row: the reading before this link"
    );
    assert_eq!(
        case(
            "ledgered-moved-on-before",
            &[moved_there, row(t_s - 600, OLD_TAB, OTHER, "announced")]
        ),
        ("continued".to_string(), 1),
        "it moved on before the `keep going`"
    );
}

/// [`ledger_tabs`]: every other tab the upgrade ledger names a conversation in, oldest first.
#[test]
fn the_upgrade_ledger_names_the_tabs_a_conversation_was_moved_in() {
    const A: &str = "s-0a";
    const B: &str = "s-0b";
    let row = |t: u64, tab: &str, session: &str| {
        format!(r#"{{"t":{t},"tab":"{tab}","session":"{session}","step":"done"}}"#)
    };
    let body = [
        row(10, A, SESSION),
        row(11, "-", SESSION),
        row(12, A, "-"),
        "not json".to_string(),
        row(13, "codex-tab", SESSION),
        row(20, B, SESSION),
        row(30, A, SESSION),
        row(40, TAB, SESSION),
        row(50, A, "0badf00d-9999-2222-3333-444455556666"),
    ]
    .join("\n");
    assert_eq!(
        ledger_tabs(&body, SESSION, TAB),
        [(B.to_string(), Some(30)), (A.to_string(), Some(40))]
    );
    let body = [row(10, A, SESSION), row(20, A, SESSION)].join("\n");
    assert_eq!(ledger_tabs(&body, SESSION, TAB), [(A.to_string(), None)]);
    assert!(ledger_tabs(&body, SESSION, A).is_empty(), "the current tab");
    assert!(ledger_tabs("", SESSION, TAB).is_empty());
}

/// The earlier tabs are the conversation's, carried by the one writer ([`save_at`]).
#[cfg(unix)]
#[test]
fn the_earlier_tabs_are_the_conversations_and_ride_every_record() {
    let dir = scratch("earlier-tabs");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let tabs = |o: &Opts| load(o, SESSION).expect("saved").tabs;
    save(
        &opts,
        SESSION,
        &St {
            tab: TAB.to_string(),
            ..St::default()
        },
    );
    save(
        &opts,
        SESSION,
        &St {
            tab: TAB.to_string(),
            ..St::default()
        },
    );
    assert!(tabs(&opts).is_empty(), "its own tab, none");
    save(
        &opts,
        SESSION,
        &St {
            tab: OLD_TAB.to_string(),
            ..St::default()
        },
    );
    assert_eq!(tabs(&opts), [TAB]);
    save(
        &opts,
        SESSION,
        &St {
            tab: TAB.to_string(),
            ..St::default()
        },
    );
    assert_eq!(tabs(&opts), [OLD_TAB], "the current tab never listed");
    save(
        &opts,
        SESSION,
        &St {
            tab: TAB.to_string(),
            ..St::default()
        },
    );
    assert_eq!(tabs(&opts), [OLD_TAB], "kept, once");
    assert_eq!(
        earlier_tabs(
            "now",
            &(0..12).map(|i| format!("s-{i}")).collect::<Vec<_>>()
        )
        .first()
        .map(String::as_str),
        Some("s-4"),
        "the newest EARLIER_TABS"
    );
    for (name, was, kept) in [
        (
            "earlier-tabs-cold",
            Some(OLD_TAB),
            vec![OLD_TAB.to_string()],
        ),
        ("earlier-tabs-kept-sid", Some(TAB), Vec::new()),
        ("earlier-tabs-none", None, Vec::new()),
    ] {
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        a.write_record();
        let r = after_host_ended_from(&a.opts, &a.snap, true, was);
        let saved = load(&a.opts, SESSION).unwrap_or_else(|| panic!("{name}: {r:?}"));
        assert_eq!(saved.cause, CAUSE_HOST, "{name}: {r:?}");
        assert_eq!(saved.tabs, kept, "{name}: {r:?}");
        let stamped: Vec<&str> = saved.tabs_until.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(stamped, kept, "{name}: {:?}", saved.tabs_until);
        assert!(saved.tabs_until.iter().all(|(_, at)| *at > 0), "{name}");
    }
}

/// A moved agent's finish is read where it wrote it, its process gone.
#[cfg(unix)]
#[test]
fn a_moved_agents_finish_is_read_where_it_wrote_it_after_its_process_is_gone() {
    const M: &str = "claude-opus-5-5";
    let at = |row: String, ts: &str| row.replacen('{', &format!("{{\"timestamp\":\"{ts}\","), 1);
    let filed = vec![
        user_row_at("Fix the lexer and land it.", "2026-10-15T10:00:00.000Z"),
        at(
            turn_by(M, "Entering a worktree."),
            "2026-10-15T10:00:05.000Z",
        ),
    ];
    let case = |name: &str, noted: bool, finished_at: &str| {
        let rig = Rig::new(name, &filed);
        let wt = rig.dir.join("work/wt/.claude");
        let project = wt.join("projects/wt");
        std::fs::create_dir_all(&project).expect("the worktree's project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            at(land_row(), finished_at)
                + "
" + &at(turn_by(M, LANDED_JSON), finished_at)
                + "
",
        )
        .expect("the worktree's transcript");
        let snap = users_agent(SESSION);
        note_writes(&rig.opts, &snap, &rig.opts.home.join(".claude"));
        assert!(
            finish_dir(
                &rig.opts,
                &Records::of_snapshot(&rig.opts.home, &snap),
                SESSION
            )
            .is_some_and(|d| d == rig.opts.home.join(".claude")),
            "{name}: nothing noted reads the filed directory"
        );
        if noted {
            note_writes(&rig.opts, &snap, &wt);
        }
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    assert_eq!(
        case("moved-finish-noted", true, "2026-10-15T10:30:00.000Z"),
        ("done:finished".to_string(), 0)
    );
    assert_eq!(
        case("moved-finish-unnoted", false, "2026-10-15T10:30:00.000Z"),
        ("continued".to_string(), 1),
        "no note: the filed file, as before"
    );
    assert_eq!(
        case("moved-finish-older", true, "2026-10-15T09:00:00.000Z"),
        ("continued".to_string(), 1),
        "the filed file written since is the newer"
    );
}

/// A moved agent's finish is found by its relaunch when the host never noted it, under a git
/// worktree of its relative home that the repository registers.
#[cfg(unix)]
#[test]
fn a_moved_agents_finish_is_found_by_its_relaunch_when_the_host_never_noted_it() {
    const M: &str = "claude-opus-5-5";
    let at = |row: String, ts: &str| row.replacen('{', &format!("{{\"timestamp\":\"{ts}\","), 1);
    let filed = vec![
        user_row_at("Fix the lexer and land it.", "2026-10-15T10:00:00.000Z"),
        at(
            turn_by(M, "Entering a worktree."),
            "2026-10-15T10:00:05.000Z",
        ),
    ];
    let case = |name: &str, registered: bool, finished_at: &str| {
        let rig = Rig::new(name, &filed);
        let proj = rig.opts.home.clone();
        let wt = proj.join(".claude/worktrees/wt1");
        std::fs::create_dir_all(proj.join(".git")).expect("the repository");
        std::fs::create_dir_all(&wt).expect("the worktree");
        if registered {
            let gitdir = proj.join(".git/worktrees/wt1");
            std::fs::create_dir_all(&gitdir).expect("its git directory");
            std::fs::write(
                gitdir.join("gitdir"),
                format!("{}\n", wt.join(".git").display()),
            )
            .expect("gitdir");
            std::fs::write(gitdir.join("commondir"), "../..\n").expect("commondir");
            std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display()))
                .expect("the worktree's .git");
        }
        let project = wt.join(".claude/projects/wt1");
        std::fs::create_dir_all(&project).expect("the worktree's project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            at(land_row(), finished_at)
                + "
" + &at(turn_by(M, LANDED_JSON), finished_at)
                + "
",
        )
        .expect("the worktree's transcript");
        let snap = Snapshot {
            pid: dead_pid(),
            shell: dead_pid(),
            cwd: proj.to_string_lossy().into_owned(),
            home: Some(".claude".to_string()),
            claude_dir: Some(proj.join(".claude")),
            ..users_agent(SESSION)
        };
        let _ = std::fs::remove_file(state_path(&rig.opts, SESSION));
        let first = after_host_ended(&rig.opts, &snap, false);
        assert_eq!(first.step, "refused:shell-gone", "{name}: {first:?}");
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    assert_eq!(
        case("moved-unnoted-found", true, "2026-10-15T10:30:00.000Z"),
        ("done:finished".to_string(), 0)
    );
    assert_eq!(
        case("moved-unregistered", false, "2026-10-15T10:30:00.000Z"),
        ("continued".to_string(), 1),
        "a directory no repository names a worktree is never read"
    );
    assert_eq!(
        case("moved-unnoted-older", true, "2026-10-15T09:00:00.000Z"),
        ("continued".to_string(), 1),
        "the filed file is the newer"
    );
}

/// An earlier tab's loop turns count only until the conversation left it.
#[cfg(unix)]
#[test]
fn an_earlier_tabs_loop_turns_count_only_until_the_conversation_left_it() {
    let at = "2026-10-15T10:00:00.000Z";
    let t_s = super::super::upgrade_models::parse_utc(at).expect("a time");
    let t_ms = i64::try_from(t_s).expect("seconds") * 1000;
    let rows = vec![
        user_row("Fix the lexer and land it."),
        land_row(),
        turn_by("claude-opus-5-5", LANDED_JSON),
        user_row_at("keep going", at),
        turn_by(
            "claude-opus-5-5",
            "Nothing further: the lexer fix is landed.",
        ),
    ];
    let case = |name: &str, until: Option<u64>| {
        let mut rig = Rig::new(name, &rows);
        let root = rig.dir.join("aterm-state");
        ledger_typed(&root, OLD_TAB, "keep going", t_ms);
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.tabs = vec![OLD_TAB.to_string()];
        st.tabs_until = until
            .map(|u| vec![(OLD_TAB.to_string(), u)])
            .unwrap_or_default();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    assert_eq!(
        case("left-before", Some(t_s - 300)),
        ("continued".to_string(), 1),
        "the old tab's row is another conversation's: the person's prompt reopens"
    );
    assert_eq!(
        case("left-after", Some(t_s)),
        ("done:finished".to_string(), 0)
    );
    assert_eq!(
        case("left-unstamped", None),
        ("done:finished".to_string(), 0),
        "an older record's tab: unbounded, as before"
    );
    let dir = scratch("tabs-until");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let saved_for = |tab: &str, now: u64| {
        save_at(
            &opts,
            SESSION,
            &St {
                tab: tab.to_string(),
                ..St::default()
            },
            now,
        );
        load(&opts, SESSION).expect("saved").tabs_until
    };
    assert!(saved_for(OLD_TAB, 100).is_empty(), "its first tab: none");
    assert_eq!(saved_for(TAB, 200), [(OLD_TAB.to_string(), 200)]);
    assert_eq!(
        saved_for(TAB, 300),
        [(OLD_TAB.to_string(), 200)],
        "kept as stamped"
    );
    assert_eq!(
        saved_for(OLD_TAB, 400),
        [(TAB.to_string(), 400)],
        "the tab it is back in is no earlier tab"
    );
    assert_eq!(saved_for(TAB, 500), [(OLD_TAB.to_string(), 500)]);
}

#[cfg(unix)]
fn finished_rows() -> Vec<String> {
    vec![
        user_row("Fix the lexer and land it."),
        land_row(),
        turn_by("claude-opus-5-5", LANDED_JSON),
        own_row(
            "task-notification",
            false,
            "<task-notification><status>completed</status></task-notification>",
        ),
        own_row(
            "auto-continuation",
            true,
            "Your claude.ai usage limit has reset. Continue the task you were working on.",
        ),
        turn_by("claude-opus-5-5", "The worktree removal finished."),
    ]
}

/// A conversation its worker finished is carried on with nothing, whatever brought it back.
#[cfg(unix)]
#[test]
fn a_finished_conversation_is_carried_on_with_nothing() {
    use super::super::relaunch::{CAUSE_ASKED, CAUSE_MEMORY, CAUSE_STALL};
    for cause in [
        CAUSE_HOST,
        CAUSE_MEMORY,
        CAUSE_EXIT,
        CAUSE_STALL,
        CAUSE_ASKED,
        "",
    ] {
        let rig = Rig::new(&format!("carry-finished-{cause}"), &finished_rows());
        let mut st = rig.st.clone();
        st.cause = cause.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "done:finished", "{cause}: {r:?}");
        assert!(rig.typed().is_empty(), "{cause}: {:?}", rig.typed());
        assert_eq!(st.phase, Phase::Done, "{cause}");
        assert!(st.confirming(), "{cause}: the look is open");
        assert_eq!(st.look_word, "done:finished", "{cause}");
        let said = rig.details("done:finished");
        assert_eq!(said.len(), 1, "{cause}: said once");
        assert!(said[0].contains("nothing typed"), "{said:?}");
        assert!(!moved("done:finished"), "the point still stands");
    }
    let pad = "x".repeat(1000);
    let long: Vec<String> = finished_rows()
        .into_iter()
        .chain((0..300).flat_map(|_| {
            [
                own_row(
                    "task-notification",
                    false,
                    "<task-notification></task-notification>",
                ),
                turn_by("claude-opus-5-5", &format!("Still finished. {pad}")),
            ]
        }))
        .collect();
    let rig = Rig::new("carry-finished-long", &long);
    let mut st = rig.st.clone();
    st.cause = CAUSE_HOST.to_string();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:finished", "behind 300 KiB of wakes: {r:?}");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    let reopened = |more: &[String]| {
        finished_rows()
            .into_iter()
            .chain(more.iter().cloned())
            .collect::<Vec<_>>()
    };
    for (what, cause, rows) in [
        (
            "a person's prompt after it",
            CAUSE_HOST,
            reopened(&[
                user_row("Now add the flag."),
                turn_by("claude-opus-5-5", "Started on the flag."),
            ]),
        ),
        (
            "a peer's message after it",
            CAUSE_ASKED,
            reopened(&[own_row(
                "peer",
                true,
                "Another Claude session sent a message: rebase onto main",
            )]),
        ),
        (
            "the token quoted",
            CAUSE_EXIT,
            vec![
                user_row("Fix the lexer and land it."),
                turn_by(
                    "claude-opus-5-5",
                    "I will write ATERM-TASK-DONE once the suite passes.",
                ),
            ],
        ),
    ] {
        let rig = Rig::new(&format!("carry-reopened-{cause}"), &rows);
        let mut st = rig.st.clone();
        st.cause = cause.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued", "{what}: {r:?}");
        assert_eq!(rig.typed().len(), 1, "{what}: {r:?}");
    }
}

/// A relaunch into a done report in words asks the done check, never [`upgrade::CARRY_ON`].
#[cfg(unix)]
#[test]
fn a_relaunch_into_a_done_report_in_words_asks_the_done_check() {
    use super::super::relaunch::CAUSE_MEMORY;
    use crate::supervise::policy::turn_end::{DONE_CHECK, DONE_CHECK_REVERSIBLE, HostTurn};
    let done = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "All tasks are complete and verified; nothing is left to do.",
        ),
    ];
    for cause in [CAUSE_HOST, CAUSE_EXIT, CAUSE_MEMORY, ""] {
        let rig = Rig::new(&format!("carry-said-done-{cause}"), &done);
        let mut st = rig.st.clone();
        st.cause = cause.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued:check", "{cause}: {r:?}");
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "{cause}: {typed:?}");
        assert!(typed[0].contains(DONE_CHECK), "{cause}: {typed:?}");
        assert!(!typed[0].contains(upgrade::CARRY_ON), "{cause}: {typed:?}");
        assert!(
            typed[0].contains(upgrade::HARNESS_MARK),
            "{cause}: the facts kept"
        );
        assert_eq!(st.phase, Phase::Done, "{cause}");
        assert_eq!(host_turn(&r.step), Some(HostTurn::Check), "{cause}");
        assert!(moved(&r.step), "{cause}: the point is gone");
    }
    let caveat = r#"{"isSidechain":false,"type":"user","isMeta":true,"promptId":"p-cav","message":{"role":"user","content":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"}}"#.to_string();
    let model_command = r#"{"isSidechain":false,"type":"user","promptId":"p-cav","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#.to_string();
    let model_out = r#"{"isSidechain":false,"type":"user","promptId":"p-cav","message":{"role":"user","content":"<local-command-stdout>Set model to Fable 5.1</local-command-stdout>"}}"#.to_string();
    let usage = [
        user_row("<command-name>/usage</command-name>"),
        user_row("<local-command-stdout>Status dialog dismissed</local-command-stdout>"),
    ];
    let shell = [
        user_row("<bash-input>git log --oneline -3</bash-input>"),
        user_row("<bash-stdout>abc123 fix lexer</bash-stdout><bash-stderr></bash-stderr>"),
    ];
    let relaunched = upgrade::checking(&resumed_prompt("2.1.282", CAUSE_HOST), false);
    for (what, extra) in [
        ("a /model", vec![caveat.clone(), model_command, model_out]),
        ("a /usage", usage.to_vec()),
        ("a ! shell command", shell.to_vec()),
        ("a caveat alone", vec![caveat]),
        ("a relaunch's line unanswered", vec![user_row(&relaunched)]),
        ("the done check unanswered", vec![user_row(DONE_CHECK)]),
    ] {
        let rows: Vec<String> = done.iter().cloned().chain(extra).collect();
        let rig = Rig::new(&format!("carry-looked-{}", what.len()), &rows);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued:check", "{what}: {r:?}");
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "{what}: {typed:?}");
        assert!(typed[0].contains(DONE_CHECK), "{what}: {typed:?}");
        assert!(!typed[0].contains(upgrade::CARRY_ON), "{what}: {typed:?}");
    }
    for (what, answer) in [
        (
            "an idle report",
            "No lander is running, so I'm not re-arming that watch. All queued work has landed.",
        ),
        ("the defense", LANDED_JSON),
    ] {
        let rows: Vec<String> = done
            .iter()
            .cloned()
            .chain([user_row(DONE_CHECK), turn_by("claude-opus-5-5", answer)])
            .collect();
        let rig = Rig::new(&format!("carry-claimed-{}", what.len()), &rows);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued:check", "{what}: {r:?}");
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "{what}: {typed:?}");
        assert!(typed[0].contains(DONE_CHECK), "{what}: {typed:?}");
    }
    let irreversible = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "All tasks are complete and verified. Should I force-push the rewritten history to \
             origin main?",
        ),
    ];
    for (what, rows, reversible) in [
        ("an irreversible act named", &irreversible, true),
        ("the plain report", &done, false),
    ] {
        let rig = Rig::new(&format!("carry-irreversible-{reversible}"), rows);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued:check", "{what}: {r:?}");
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "{what}: {typed:?}");
        assert!(typed[0].contains(DONE_CHECK), "{what}: {typed:?}");
        assert_eq!(
            typed[0].contains(DONE_CHECK_REVERSIBLE),
            reversible,
            "{what}: {typed:?}"
        );
        assert!(!typed[0].contains(upgrade::CARRY_ON), "{what}: {typed:?}");
    }
    let notice = format!(
        "{}2.1.282 (managed) is installed; this session runs 2.1.281. reply with READY-x1 on a \
         line by itself.",
        upgrade::ANNOUNCE_HEAD
    );
    let rows: Vec<String> = done
        .iter()
        .cloned()
        .chain([user_row(&notice), turn_by("claude-opus-5-5", "READY-x1")])
        .collect();
    let rig = Rig::new("carry-notice-ready", &rows);
    let mut st = rig.st.clone();
    st.cause = String::new();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued:check", "the notice's READY: {r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains(DONE_CHECK), "{typed:?}");
    assert!(!typed[0].contains(upgrade::CARRY_ON), "{typed:?}");
    for (what, rows) in [
        (
            "work going on",
            vec![
                user_row("Fix the lexer and land it."),
                turn_by("claude-opus-5-5", "Landed the parser; the lexer is next."),
            ],
        ),
        (
            "a done phrase inside another word",
            vec![
                user_row("Fix the lexer and land it."),
                turn_by(
                    "claude-opus-5-5",
                    "Install done; running the integration suite now.",
                ),
            ],
        ),
        (
            "a person's prompt after it",
            done.iter()
                .cloned()
                .chain([user_row("Now add the flag.")])
                .collect(),
        ),
        (
            "the Cruise prompt answered with work going on",
            done.iter()
                .cloned()
                .chain([
                    user_row(DONE_CHECK),
                    turn_by(
                        "claude-opus-5-5",
                        "Ran the suite again: two tests fail. Fixing them now.",
                    ),
                ])
                .collect(),
        ),
        (
            "the notice answered with a done phrase after work going on",
            vec![
                user_row("Fix the lexer and land it."),
                turn_by("claude-opus-5-5", "Landed the parser; the lexer is next."),
                user_row(&notice),
                turn_by("claude-opus-5-5", "All tasks are complete. READY-x1"),
            ],
        ),
        (
            "a command that started a turn",
            done.iter()
                .cloned()
                .chain([
                    user_row("<command-name>/review</command-name>"),
                    r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Review the diff."}}"#
                        .to_string(),
                    turn_by("claude-opus-5-5", "Reviewing the diff now; two files left."),
                ])
                .collect(),
        ),
        (
            "a wake answered with work going on",
            done.iter()
                .cloned()
                .chain([
                    own_row(
                        "task-notification",
                        false,
                        "<task-notification>build failed</task-notification>",
                    ),
                    turn_by(
                        "claude-opus-5-5",
                        "The build failed on the lexer test; fixing it now.",
                    ),
                ])
                .collect(),
        ),
    ] {
        let rig = Rig::new(&format!("carry-not-done-{}", what.len()), &rows);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued", "{what}: {r:?}");
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "{what}: {typed:?}");
        assert!(typed[0].contains(upgrade::CARRY_ON), "{what}: {typed:?}");
    }
}

#[cfg(unix)]
/// A relaunch into a task land's act landed types nothing, also where a wall cut the act and the
/// loop's ledgered retry landed it.
#[cfg(unix)]
#[test]
fn a_relaunch_into_a_task_a_land_act_landed_types_nothing() {
    use crate::supervise::policy::turn_end::{DONE_CHECK, LAND, RULE_API_RETRY, wall_retry_text};
    let at = "2026-10-15T10:00:00.000Z";
    let t_ms = i64::try_from(super::super::upgrade_models::parse_utc(at).expect("a time"))
        .expect("seconds")
        * 1000;
    let retry = wall_retry_text(
        RULE_API_RETRY,
        aterm_phase::WallKind::Overloaded,
        aterm_phase::Program::Claude,
    );
    let wall = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 Overloaded"}]},"isApiErrorMessage":true,"error":"unknown"}"#.to_string();
    let asked = |more: &[String]| {
        [
            user_row("Fix the lexer and land it."),
            turn_by(
                "claude-opus-5-5",
                "All tasks are complete and verified; nothing is left to do.",
            ),
        ]
        .into_iter()
        .chain(more.iter().cloned())
        .collect::<Vec<_>>()
    };
    let done = turn_by("claude-opus-5-5", LANDED_JSON);
    let idle = turn_by(
        "claude-opus-5-5",
        "No lander is running, so I'm not re-arming that watch.",
    );
    let cut = |act: &str| {
        asked(&[
            user_row(act),
            wall.clone(),
            user_row_at(&retry, at),
            done.clone(),
        ])
    };
    let case = |name: &str, rows: &[String], ledgered: bool| {
        let mut rig = Rig::new(name, rows);
        let root = rig.dir.join("aterm-state");
        if ledgered {
            ledger_typed_as(&root, TAB, &retry, t_ms, RULE_API_RETRY);
        }
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        (r.step, rig.typed().len())
    };
    assert_eq!(
        case("land-done", &asked(&[user_row(LAND), done.clone()]), false),
        ("done:finished".to_string(), 0),
        "LAND answered with the clean defense"
    );
    assert_eq!(
        case("cut-land-retried", &cut(LAND), true),
        ("done:finished".to_string(), 0),
        "the cut LAND's retry answered with the clean defense"
    );
    assert_eq!(
        case("cut-land-unledgered", &cut(LAND), false),
        ("continued:check".to_string(), 1),
        "no ledger: the retry is someone's words"
    );
    assert_eq!(
        case("land-idle", &asked(&[user_row(LAND), idle]), false),
        ("continued:check".to_string(), 1),
        "an idle report answering LAND"
    );
    assert_eq!(
        case("cut-check-retried", &cut(DONE_CHECK), true),
        ("continued:check".to_string(), 1),
        "the Cruise prompt's retry answered with the defense"
    );
}

/// A relaunch the tab's loop resumed first is carried on with nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_the_tabs_loop_resumed_first_is_carried_on_with_nothing() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::{
        RULE_API_RETRY, RULE_MODEL_FALLBACK, RULE_USAGE_PROBE,
    };
    let going = vec![
        user_row("Fix the lexer and land it."),
        turn_by("claude-opus-5-5", "Landed the parser; the lexer is next."),
    ];
    let said_done = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "All tasks are complete and verified; nothing is left to do.",
        ),
    ];
    let line = now_s().saturating_sub(120);
    let at_ms = |s: u64| i64::try_from(s).expect("seconds") * 1000;
    let retry = "The API was overloaded; trying again.";
    let case = |name: &str, rows: &[String], tab: &str, (rule, command): (&str, &str), ts_ms| {
        let mut rig = Rig::new(name, rows);
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(tab)).expect("ledger dir");
        let row = approvals::Row {
            rule_id: rule,
            outcome: approvals::Outcome::Typed,
            command,
            reason: "the turn-end policy",
            box_seq: 1,
        }
        .to_json(ts_ms, Some(tab));
        std::fs::write(path, row + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.relaunched(line);
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        let typed = rig.typed().len();
        let said = rig.details("done:carried");
        if r.step == "done:carried" {
            assert_eq!(st.phase, Phase::Done, "{name}");
            assert!(st.confirming(), "{name}: the look is open");
            assert_eq!(st.look_word, "done:carried", "{name}");
            assert_eq!(said.len(), 1, "{name}: said once");
            assert!(said[0].contains("nothing typed"), "{said:?}");
            assert!(!moved(&r.step), "{name}: the point still stands");
        }
        (r.step, typed)
    };
    let after = at_ms(line + 40);
    assert_eq!(
        case(
            "carry-loop-retried",
            &going,
            TAB,
            (RULE_API_RETRY, retry),
            after
        ),
        ("done:carried".to_string(), 0)
    );
    assert_eq!(
        case(
            "carry-loop-retried-done",
            &said_done,
            TAB,
            (RULE_API_RETRY, retry),
            after
        ),
        ("done:carried".to_string(), 0),
        "the lab's L4b: the retry's answer a done report"
    );
    assert_eq!(
        case(
            "carry-loop-before",
            &going,
            TAB,
            (RULE_API_RETRY, retry),
            at_ms(line - 30)
        ),
        ("continued".to_string(), 1),
        "CONTROL: typed before the relaunch line"
    );
    assert_eq!(
        case(
            "carry-loop-probe",
            &going,
            TAB,
            (RULE_USAGE_PROBE, "/usage"),
            after
        ),
        ("continued".to_string(), 1),
        "CONTROL: the accounts probe's `/usage`"
    );
    assert_eq!(
        case(
            "carry-loop-switch",
            &going,
            TAB,
            (RULE_MODEL_FALLBACK, "relaunch --model opus"),
            after
        ),
        ("continued".to_string(), 1),
        "CONTROL: the model switch's own restart row is this relaunch"
    );
    assert_eq!(
        case(
            "carry-loop-other-tab",
            &going,
            OLD_TAB,
            (RULE_API_RETRY, retry),
            after
        ),
        ("continued".to_string(), 1),
        "CONTROL: another tab's row"
    );
}

/// An owed relaunch is settled by its loop's own act, off the host's point.
#[cfg(unix)]
#[test]
fn an_owed_relaunch_is_settled_by_its_loops_own_act() {
    use super::super::relaunch::settle;
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::{DONE_CHECK, RULE_DONE_CHECK};
    let open = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "All tasks are complete and verified; nothing is left to do.",
        ),
    ];
    let line = now_s().saturating_sub(600);
    let at_ms = |s: u64| i64::try_from(s).expect("seconds") * 1000;
    let on_tab = |rig: &Rig| Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    let case = |name: &str, rows: &[String], more: &[String], typed_at, adopted: Option<u32>| {
        let mut rig = Rig::new(name, rows);
        rig.append(more);
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let row = approvals::Row {
            rule_id: RULE_DONE_CHECK,
            outcome: approvals::Outcome::Typed,
            command: DONE_CHECK,
            reason: "the turn-end policy",
            box_seq: 1,
        }
        .to_json(at_ms(typed_at), Some(TAB));
        std::fs::write(path, row + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.relaunched(line);
        st.relaunched_pid = adopted.unwrap_or(rig.sf.pid);
        save(&rig.opts, SESSION, &st);
        let r = settle(&on_tab(&rig));
        assert!(rig.typed().is_empty(), "{name}: {:?}", rig.typed());
        (rig, r.step)
    };
    let warning = [resumed_warning("2.1.282", now_s(), "warning")];
    let (rig, step) = case("settle-carried", &open, &[], line + 40, None);
    assert_eq!(step, "done:carried");
    let st = load(&rig.opts, SESSION).expect("record");
    assert_eq!(st.phase, Phase::Done);
    assert!(st.confirming(), "the look open for its window: {st:?}");
    assert!(
        st.outcome
            .contains("nothing typed: the tab's loop resumed it first"),
        "{}",
        st.outcome
    );
    let over = st.done_at + super::super::relaunch::RESTORE_LOOK.as_secs();
    let ended = at_second(over, || settle(&on_tab(&rig)));
    assert_eq!(ended.step, "done:carried");
    assert!(!load(&rig.opts, SESSION).expect("record").confirming());
    assert_eq!(rig.details("done:carried").len(), 1, "said once");
    assert!(!owed(&on_tab(&rig)), "owed no more");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let (rig, step) = case("settle-lost", &finished_rows(), &warning, line + 40, None);
    assert_eq!(step, "done:lost", "a finished task's warning is recorded");
    assert!(!owed(&on_tab(&rig)));
    drop(rig);
    let (rig, step) = case("settle-open-warned", &open, &warning, line + 40, None);
    assert_eq!(step, "done:carried");
    assert!(
        owed(&on_tab(&rig)),
        "an open task's warning: the look's line is the idle point's"
    );
    drop(rig);
    for (name, typed_at, adopted, word) in [
        ("settle-before-line", line - 30, None, "wait:owed"),
        ("settle-not-adopted", line + 40, Some(0), "wait:resume"),
        (
            "settle-not-holding",
            line + 40,
            Some(std::process::id()),
            "wait:changed",
        ),
    ] {
        let (rig, step) = case(name, &open, &[], typed_at, adopted);
        assert_eq!(step, word, "{name}");
        let st = load(&rig.opts, SESSION).expect("record");
        assert!(
            matches!(st.phase, Phase::Relaunched { .. }),
            "{name}: {st:?}"
        );
        assert!(owed(&on_tab(&rig)), "{name}: still owed");
        assert!(rig.details("done:carried").is_empty(), "{name}");
    }
}

/// A warning the resumed build writes just after the loop's act is still read.
#[cfg(unix)]
#[test]
fn a_warning_written_just_after_the_loops_act_is_still_read() {
    use super::super::relaunch::{RESTORE_LOOK, settle};
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::{DONE_CHECK, RULE_DONE_CHECK};
    let open = vec![
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let act = now_s();
    let line = act - 10;
    let look = RESTORE_LOOK.as_secs();
    let at_ms = |s: u64| i64::try_from(s).expect("seconds") * 1000;
    let on_tab = |rig: &Rig| Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    let setup = |name: &str, rows: &[String]| {
        let mut rig = Rig::new(name, rows);
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let row = approvals::Row {
            rule_id: RULE_DONE_CHECK,
            outcome: approvals::Outcome::Typed,
            command: DONE_CHECK,
            reason: "the turn-end policy",
            box_seq: 1,
        }
        .to_json(at_ms(line + 3), Some(TAB));
        std::fs::write(path, row + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.relaunched(line);
        st.relaunched_pid = rig.sf.pid;
        save(&rig.opts, SESSION, &st);
        (rig, st)
    };
    let settle_at = |rig: &Rig, now: u64| at_second(now, || settle(&on_tab(rig))).step;
    let warning = |at: u64| [resumed_warning("2.1.282", at, "warning")];
    for (name, rows, step, end) in [
        ("late-ctl-open", &open, "done:carried", "continued:restore"),
        (
            "late-ctl-fin",
            &finished_rows(),
            "done:finished",
            "done:lost",
        ),
    ] {
        let (rig, mut st) = setup(name, rows);
        let r = at_second(act, || rig.carry_on_from(&mut st, MODEL_WAIT));
        assert_eq!(r.step, step, "{name}");
        assert!(st.confirming(), "{name}: the look is open");
        rig.append(&warning(act + 1));
        let r = at_second(act + 5, || restore_look_at(&rig, &mut st, act + 5)).expect("decided");
        assert_eq!(r.step, end, "{name}");
    }
    let (rig, _) = setup("late-open", &open);
    assert_eq!(settle_at(&rig, act), "done:carried");
    let mut st = load(&rig.opts, SESSION).expect("record");
    assert!(st.confirming(), "the look is open for its window: {st:?}");
    assert_eq!(st.confirm_by, act + look);
    assert!(owed(&on_tab(&rig)));
    rig.append(&warning(act + 1));
    assert_eq!(
        settle_at(&rig, act + 5),
        "wait:owed",
        "the open task's line is the idle point's: never settled again"
    );
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    assert!(
        owed(&on_tab(&rig)),
        "the open task's line is the idle point's"
    );
    st = load(&rig.opts, SESSION).expect("record");
    let r = at_second(act + 6, || restore_look_at(&rig, &mut st, act + 6)).expect("decided");
    assert_eq!(r.step, "continued:restore");
    assert_eq!(rig.typed().len(), 1, "{:?}", rig.typed());
    drop(rig);
    let (rig, _) = setup("late-fin", &finished_rows());
    assert_eq!(settle_at(&rig, act), "done:finished");
    assert!(rig.details("done:lost").is_empty(), "no warning yet");
    assert!(owed(&on_tab(&rig)), "the look is open for its window");
    rig.append(&warning(act + 1));
    assert_eq!(settle_at(&rig, act + 5), "done:lost");
    assert_eq!(rig.details("done:lost").len(), 1);
    assert!(!owed(&on_tab(&rig)));
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let (rig, _) = setup("late-after-window", &open);
    assert_eq!(settle_at(&rig, act), "done:carried");
    rig.append(&warning(act + look + 1));
    assert_eq!(settle_at(&rig, act + look + 1), "done:carried");
    assert!(!owed(&on_tab(&rig)), "the look is over");
    assert_eq!(rig.details("done:carried").len(), 1, "said once");
    assert!(rig.details("done:lost").is_empty());
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
}

/// A settle the loop's last point missed is settled again at its later wakes.
#[cfg(unix)]
#[test]
fn a_settle_the_loops_last_point_missed_is_settled_again_off_the_points() {
    use super::super::relaunch::{BUSY, RESTORE_LOOK, settle, settles_again};
    let act = now_s();
    let line = act - 10;
    let busy = BUSY.as_secs();
    let on_tab = |rig: &Rig| Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    let setup = |name: &str, rows: &[String], pid: Option<u32>| {
        let mut rig = Rig::new(name, rows);
        let root = rig.dir.join("aterm-state");
        std::fs::create_dir_all(&root).expect("state root");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.relaunched(line);
        st.relaunched_pid = pid.unwrap_or(rig.sf.pid);
        save(&rig.opts, SESSION, &st);
        rig
    };
    let settle_at = |rig: &Rig, now: u64| at_second(now, || settle(&on_tab(rig))).step;
    let rig = setup("again-busy", &finished_rows(), None);
    let held = sweep_lock(&rig.opts)
        .expect("free")
        .expect("a real run locks");
    let step = settle_at(&rig, act);
    assert_eq!(step, "busy:another-sweep");
    assert!(
        settles_again(&step),
        "another sweep's lock is settled again"
    );
    drop(held);
    assert!(owed(&on_tab(&rig)), "the missed settle left it owed");
    let step = settle_at(&rig, act + busy);
    assert_eq!(
        step, "done:finished",
        "a later wake's settle takes the step"
    );
    assert!(settles_again(&step), "its look is open");
    let st = load(&rig.opts, SESSION).expect("record");
    assert_eq!(st.confirm_by, act + busy + RESTORE_LOOK.as_secs());
    assert!(owed(&on_tab(&rig)), "owed while the look is open");
    let step = settle_at(&rig, act + 2 * busy);
    assert_eq!(step, "wait:restore");
    assert!(settles_again(&step));
    let step = settle_at(&rig, act + 3 * busy);
    assert_eq!(step, "done:finished", "the look ends on the step's word");
    assert!(!owed(&on_tab(&rig)), "owed no more: settled again no more");
    assert_eq!(rig.details("done:finished").len(), 1, "said once");
    assert!(rig.details("done:lost").is_empty());
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let rig = setup("again-adopting", &finished_rows(), Some(0));
    let step = settle_at(&rig, act);
    assert_eq!(step, "wait:resume");
    assert!(settles_again(&step), "a relaunch not adopted yet");
    let mut st = load(&rig.opts, SESSION).expect("record");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    st.relaunched_pid = rig.sf.pid;
    save(&rig.opts, SESSION, &st);
    assert_eq!(settle_at(&rig, act + busy), "done:finished");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let open = vec![
        user_row("Fix the lexer and land it."),
        turn_by("claude-opus-5-5", "Working on the lexer."),
    ];
    let rig = setup("again-would-type", &open, None);
    let step = settle_at(&rig, act);
    assert_eq!(step, "wait:owed");
    assert!(!settles_again(&step), "what would type is the idle point's");
    let st = load(&rig.opts, SESSION).expect("record");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert!(owed(&on_tab(&rig)), "still owed, for the idle point");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    for word in [
        "wait:model",
        "wait:changed",
        "done:lost",
        "current",
        "busy:state-unwritable",
        "would-settle",
    ] {
        assert!(!settles_again(word), "{word}");
    }
}

/// An open task's restore line is never decided off the point.
#[cfg(unix)]
#[test]
fn an_open_tasks_restore_line_is_never_decided_off_the_point() {
    use super::super::relaunch::{BUSY, RESTORE_LOOK, settle, settles_again};
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::{DONE_CHECK, RULE_DONE_CHECK};
    let open = vec![
        user_row("Watch the artifact's comments and answer them."),
        turn_by("claude-opus-5-5", "Watching the comments."),
    ];
    let act = now_s();
    let line = act - 10;
    let busy = BUSY.as_secs();
    let at_ms = |s: u64| i64::try_from(s).expect("seconds") * 1000;
    let on_tab = |rig: &Rig| Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    let setup = |name: &str, rows: &[String], warned: bool| {
        let mut rig = Rig::new(name, rows);
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let row = approvals::Row {
            rule_id: RULE_DONE_CHECK,
            outcome: approvals::Outcome::Typed,
            command: DONE_CHECK,
            reason: "the turn-end policy",
            box_seq: 1,
        }
        .to_json(at_ms(line + 3), Some(TAB));
        std::fs::write(path, row + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        st.relaunched(line);
        st.relaunched_pid = rig.sf.pid;
        save(&rig.opts, SESSION, &st);
        if warned {
            rig.append(&[resumed_warning("2.1.282", line + 2, "warning")]);
        }
        rig
    };
    let settle_at = |rig: &Rig, now: u64| at_second(now, || settle(&on_tab(rig))).step;
    let again = |rig: &Rig| {
        let mut words = Vec::new();
        let mut t = act + busy;
        while t <= act + 3600 {
            let word = settle_at(rig, t);
            let more = settles_again(&word);
            words.push(word);
            if !more {
                break;
            }
            t += busy;
        }
        words
    };
    let rig = setup("open-warned", &open, true);
    assert_eq!(settle_at(&rig, act), "done:carried", "the loop's act");
    assert!(owed(&on_tab(&rig)));
    assert_eq!(
        again(&rig),
        ["wait:owed"],
        "the chain ends at its first try"
    );
    let later = act + RESTORE_LOOK.as_secs() + 5 * 60 + busy;
    assert_eq!(settle_at(&rig, later), "wait:owed", "a loop's later point");
    assert!(
        owed(&on_tab(&rig)),
        "the line is still owed, for the idle point"
    );
    assert!(rig.details("done:lost").is_empty(), "nothing recorded lost");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    let turn_end = act + 360;
    let mut st = load(&rig.opts, SESSION).expect("record");
    let r = at_second(turn_end, || restore_look_at(&rig, &mut st, turn_end)).expect("decided");
    assert_eq!(r.step, "continued:restore");
    assert_eq!(rig.typed().len(), 1, "{:?}", rig.typed());
    assert!(rig.details("done:lost").is_empty());
    assert!(!owed(&on_tab(&rig)));
    drop(rig);
    let rig = setup("finished-warned", &finished_rows(), true);
    assert_eq!(settle_at(&rig, act), "done:lost");
    assert_eq!(rig.details("done:lost").len(), 1);
    assert!(!owed(&on_tab(&rig)));
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let rig = setup("open-quiet", &open, false);
    assert_eq!(settle_at(&rig, act), "done:carried");
    assert_eq!(again(&rig), ["wait:restore", "done:carried", "current"]);
    assert!(!owed(&on_tab(&rig)));
    assert_eq!(rig.details("done:carried").len(), 1, "said once");
    assert!(rig.details("done:lost").is_empty());
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
}

/// A relaunch into a person's own exchange types nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_into_a_persons_own_exchange_types_nothing() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::RULE_LEFT;
    let exchange = vec![
        r#"{"type":"user","uuid":"u-1","promptId":"p-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#
            .to_string(),
        r#"{"type":"assistant","uuid":"a-1","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}"#
            .to_string(),
        r#"{"type":"system","subtype":"turn_duration","durationMs":3000,"isSidechain":false}"#
            .to_string(),
    ];
    let with = |more: &[&str]| {
        exchange
            .iter()
            .cloned()
            .chain(more.iter().map(|r| (*r).to_string()))
            .collect::<Vec<_>>()
    };
    let config = with(&[
        r#"{"type":"user","uuid":"c-0","promptId":"p-2","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"}}"#,
        r#"{"type":"user","uuid":"c-1","promptId":"p-2","isSidechain":false,"message":{"role":"user","content":"<command-name>/config</command-name>\n            <command-message>config</command-message>\n            <command-args></command-args>"}}"#,
        r#"{"type":"user","uuid":"c-2","promptId":"p-2","isSidechain":false,"message":{"role":"user","content":"<local-command-stdout>Disabled auto-compact</local-command-stdout>"}}"#,
    ]);
    let bash = with(&[
        r#"{"type":"user","uuid":"b-1","promptId":"p-3","isSidechain":false,"message":{"role":"user","content":"<bash-input>ls</bash-input>"}}"#,
        r#"{"type":"user","uuid":"b-2","promptId":"p-3","isSidechain":false,"message":{"role":"user","content":"<bash-stdout>notes.txt</bash-stdout><bash-stderr></bash-stderr>"}}"#,
    ]);
    let init = with(&[
        r#"{"type":"user","uuid":"i-1","promptId":"p-4","isSidechain":false,"message":{"role":"user","content":"<command-message>init is analyzing your codebase…</command-message>\n<command-name>/init</command-name>"}}"#,
        r#"{"type":"user","uuid":"i-2","promptId":"p-4","isMeta":true,"isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"Please analyze this codebase and create a CLAUDE.md file"}]}}"#,
        r#"{"type":"assistant","uuid":"a-2","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"I wrote CLAUDE.md."}]}}"#,
    ]);
    let case_on =
        |name: &str, rows: &[String], tab: &str, key: &str, closed: bool, tabs: &[&str]| {
            let mut rig = Rig::new(name, rows);
            let root = rig.dir.join("aterm-state");
            let path = approvals::path_under(&root, Some(tab)).expect("ledger dir");
            let row = |rule, outcome, command| {
                approvals::Row {
                    rule_id: rule,
                    outcome,
                    command,
                    reason: approvals::LEFT_REASON,
                    box_seq: 1,
                }
                .to_json(1_000, Some(tab))
            };
            let mut body = vec![row(RULE_LEFT, approvals::Outcome::Skipped, key)];
            if closed {
                body.push(row("continue@v1", approvals::Outcome::Typed, "keep going"));
            }
            std::fs::write(path, body.join("\n") + "\n").expect("ledger");
            rig.opts.aterm_state = Some(root);
            let mut st = rig.st.clone();
            st.cause = CAUSE_HOST.to_string();
            st.tabs = tabs.iter().map(|t| (*t).to_string()).collect();
            let r = rig.carry_on_from(&mut st, MODEL_WAIT);
            if r.step == "done:left" {
                assert_eq!(st.phase, Phase::Done, "{name}");
                assert!(st.confirming(), "{name}: the look is open");
                let said = rig.details("done:left");
                assert_eq!(said.len(), 1, "{name}: said once");
                assert!(said[0].contains("nothing typed"), "{said:?}");
                assert!(!moved(&r.step), "{name}: the point still stands");
            }
            (r.step, rig.typed().len())
        };
    let case = |name: &str, tab: &str, key: &str, closed: bool, tabs: &[&str]| {
        case_on(name, &exchange, tab, key, closed, tabs)
    };
    let left = ("done:left".to_string(), 0);
    let carried = ("continued".to_string(), 1);
    assert_eq!(case("left-here", TAB, "answer:a-1", false, &[]), left);
    assert_eq!(
        case("left-by-prompt", TAB, "prompt:u-1", false, &[]),
        left,
        "the row keyed by the prompt that opened the exchange (ATERM-KI-1016-1)"
    );
    assert_eq!(
        case("left-earlier-tab", OLD_TAB, "answer:a-1", false, &[OLD_TAB]),
        left,
        "a cold restore: the row is the tab the record names"
    );
    assert_eq!(
        case("left-other-answer", TAB, "answer:a-0", false, &[]),
        carried,
        "CONTROL: a turn since"
    );
    assert_eq!(
        case("left-other-prompt", TAB, "prompt:u-0", false, &[]),
        carried,
        "CONTROL: another prompt's row"
    );
    assert_eq!(
        case("left-closed", TAB, "answer:a-1", true, &[]),
        carried,
        "CONTROL: a typed act closed it"
    );
    assert_eq!(
        case("left-unnamed-tab", OLD_TAB, "answer:a-1", false, &[]),
        carried,
        "CONTROL: a tab the conversation was never in"
    );
    for (what, rows) in [("config", &config), ("bash", &bash)] {
        for key in ["prompt:u-1", "answer:a-1"] {
            assert_eq!(
                case_on(&format!("left-{what}-{key}"), rows, TAB, key, false, &[]),
                left,
                "{what} after the exchange left under {key}"
            );
        }
    }
    assert_eq!(
        case_on("left-config-key", &config, TAB, "prompt:c-1", false, &[]),
        left,
        "the row a loop that read the command wrote"
    );
    assert_eq!(
        case_on("left-init", &init, TAB, "prompt:u-1", false, &[]),
        carried,
        "CONTROL: a prompt command answered since"
    );
}

/// A relaunch into a person's exchange retried at a wall types nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_into_a_persons_exchange_retried_at_a_wall_types_nothing() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::{RULE_API_RETRY, RULE_COMPACT, RULE_LEFT};
    let retry = "Claude Code reported an overloaded API (529). Carry on.";
    let rows = |answer_ms: u64| {
        vec![
            r#"{"type":"user","uuid":"u-1","promptId":"p-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#.to_string(),
            r#"{"type":"assistant","uuid":"e-1","isSidechain":false,"isApiErrorMessage":true,"apiErrorStatus":529,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"API Error: 529 Overloaded"}]}}"#.to_string(),
            r#"{"type":"system","subtype":"turn_duration","durationMs":1815,"isSidechain":false}"#.to_string(),
            format!(r#"{{"type":"user","uuid":"r-1","promptId":"p-2","isSidechain":false,"message":{{"role":"user","content":"{retry}"}}}}"#),
            r#"{"type":"assistant","uuid":"a-1","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}"#.to_string(),
            format!(r#"{{"type":"system","subtype":"turn_duration","durationMs":{answer_ms},"isSidechain":false}}"#),
        ]
    };
    let wall = approvals::left_at_wall_key("prompt:u-1", Duration::from_secs(120));
    let case = |name: &str, answer_ms: u64, after: &[(&'static str, &'static str)]| {
        let mut rig = Rig::new(name, &rows(answer_ms));
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let row = |rule, outcome, command: &str| {
            approvals::Row {
                rule_id: rule,
                outcome,
                command,
                reason: approvals::LEFT_AT_WALL_REASON,
                box_seq: 1,
            }
            .to_json(1_000, Some(TAB))
        };
        let mut body = vec![row(RULE_LEFT, approvals::Outcome::Skipped, &wall)];
        body.extend(
            after
                .iter()
                .map(|(rule, command)| row(rule, approvals::Outcome::Typed, command)),
        );
        std::fs::write(path, body.join("\n") + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        if r.step == "done:left" {
            assert_eq!(st.phase, Phase::Done, "{name}");
            assert_eq!(rig.details("done:left").len(), 1, "{name}: said once");
        }
        (r.step, rig.typed().len())
    };
    let left = ("done:left".to_string(), 0);
    let carried = ("continued".to_string(), 1);
    let retried: &[(&str, &str)] = &[(RULE_API_RETRY, retry)];
    assert_eq!(case("retried-short", 20_000, retried), left);
    assert_eq!(
        case("retried-long", 9 * 60 * 1000, retried),
        carried,
        "CONTROL: a job past the bound, the work before the wall counted"
    );
    assert_eq!(
        case(
            "retried-continued",
            20_000,
            &[(RULE_API_RETRY, retry), ("continue@v1", "keep going")]
        ),
        carried,
        "CONTROL: a continuation after the retry"
    );
    assert_eq!(
        case("compacted", 20_000, &[(RULE_COMPACT, "/compact")]),
        carried,
        "CONTROL: a command in the retry's place"
    );
}

/// The rows Claude Code 2.1.30x writes when it compacts an idle conversation by itself.
pub(super) fn idle_compaction_rows(kb: usize) -> Vec<String> {
    let mut rows = vec![
        r#"{"type":"system","subtype":"compact_boundary","isSidechain":false,"compactMetadata":{"trigger":"idle"}}"#.to_string(),
        format!(
            r#"{{"type":"user","uuid":"s-1","isSidechain":false,"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"promptId":"q-2","message":{{"role":"user","content":"This session is being continued from a previous conversation. {}"}}}}"#,
            "y".repeat(16_000)
        ),
    ];
    for i in 0..kb / 20 {
        rows.push(format!(
            r#"{{"type":"attachment","isSidechain":false,"attachment":{{"type":"file","filename":"/tmp/f{i}.rs","content":"{}"}}}}"#,
            "x".repeat(20_000)
        ));
    }
    rows.push(r#"{"type":"system","subtype":"informational","isSidechain":false,"content":"Compacted while idle, before the prompt cache expired"}"#.to_string());
    rows
}

/// A relaunch after an idle compaction still types nothing into a person's own exchange.
#[cfg(unix)]
#[test]
fn a_relaunch_after_an_idle_compaction_types_nothing_into_a_persons_exchange() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::RULE_LEFT;
    let exchange = [
        r#"{"type":"user","uuid":"u-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#,
        r#"{"type":"assistant","uuid":"a-1","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}"#,
        r#"{"type":"system","subtype":"turn_duration","durationMs":4000,"isSidechain":false}"#,
    ];
    for (kb, key) in [
        (100, "answer:a-1"),
        (320, "answer:a-1"),
        (320, "prompt:u-1"),
    ] {
        let rows: Vec<String> = exchange
            .iter()
            .map(|r| (*r).to_string())
            .chain(idle_compaction_rows(kb))
            .collect();
        let name = format!("idle-compaction-{kb}k-{}", key.replace(':', "-"));
        let mut rig = Rig::new(&name, &rows);
        let root = rig.dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let row = approvals::Row {
            rule_id: RULE_LEFT,
            outcome: approvals::Outcome::Skipped,
            command: key,
            reason: approvals::LEFT_REASON,
            box_seq: 1,
        }
        .to_json(1_000, Some(TAB));
        std::fs::write(path, row + "\n").expect("ledger");
        rig.opts.aterm_state = Some(root);
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(
            (r.step.as_str(), rig.typed()),
            ("done:left", Vec::<String>::new()),
            "{name}"
        );
    }
}

/// The finished memo keys the turn under an idle compaction.
#[cfg(unix)]
#[test]
fn the_finished_memo_keys_the_turn_under_an_idle_compaction() {
    let dir = scratch("finished-memo-key");
    let claude = dir.join(".claude");
    let project = claude.join("projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let path = project.join(format!("{SESSION}.jsonl"));
    let mut rows = vec![
        r#"{"type":"user","uuid":"u-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#.to_string(),
        r#"{"type":"assistant","uuid":"a-1","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}"#.to_string(),
    ];
    rows.extend(idle_compaction_rows(320));
    std::fs::write(&path, rows.join("\n") + "\n").expect("write");
    assert_eq!(
        upgrade::last_turn_key(&tail_to_end(&path, TAIL_BYTES).0),
        None,
        "negative control: the tail names no turn"
    );
    let mut memo = Finished::default();
    assert_eq!(
        memo.of_in_from(&claude, SESSION, &[], FINISHED_ONCE_BYTES),
        Some(false)
    );
    assert_eq!(memo.last_turn_key().as_deref(), Some("answer:a-1"));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    writeln!(
        f,
        r#"{{"type":"user","uuid":"u-2","isSidechain":false,"message":{{"role":"user","content":"Now add the flag."}},"origin":{{"kind":"human"}},"promptSource":"typed"}}"#
    )
    .expect("row");
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(false));
    assert_eq!(memo.last_turn_key().as_deref(), Some("prompt:u-2"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restored tab's loop takes up the exchange its earlier tab left.
#[test]
fn a_restored_tabs_loop_reads_the_exchange_its_earlier_tab_left() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::RULE_LEFT;
    let dir = scratch("lefts-beside");
    let root = dir.join("aterm-state");
    let opts = Opts {
        aterm_state: Some(root.clone()),
        ..drive(&dir)
    };
    let old = approvals::path_under(&root, Some(OLD_TAB)).expect("ledger dir");
    let row = approvals::Row {
        rule_id: RULE_LEFT,
        outcome: approvals::Outcome::Skipped,
        command: "answer:a-1",
        reason: approvals::LEFT_REASON,
        box_seq: 1,
    }
    .to_json(1_000, Some(OLD_TAB));
    std::fs::write(&old, row + "\n").expect("ledger");
    let mine = approvals::ledger_under(&root, Some(TAB));
    assert!(
        lefts_beside(&mine, Some(TAB)).is_empty(),
        "CONTROL: no note"
    );
    let recording = aterm_durable::seam::Recording::start();
    note_earlier_tab(&opts, TAB, OLD_TAB);
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "the earlier-tab note is only ordered"
    );
    assert_eq!(lefts_beside(&mine, Some(TAB)), ["answer:a-1"]);
    assert_eq!(lefts_beside(&old, Some(OLD_TAB)), ["answer:a-1"], "its own");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A finished task's record is read on from where the last read ended ([`Finished`]).
#[cfg(unix)]
#[test]
fn a_finished_task_is_read_on_from_where_the_last_read_ended() {
    let dir = scratch("finished-memo");
    let claude = dir.join(".claude");
    let project = claude.join("projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let path = project.join(format!("{SESSION}.jsonl"));
    let write = |rows: &[String]| std::fs::write(&path, rows.join("\n") + "\n").expect("write");
    let append = |rows: &[String]| {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        for row in rows {
            writeln!(f, "{row}").expect("row");
        }
    };
    write(&finished_rows()[..3]);
    let mut memo = Finished::default();
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(true));
    let pad = "x".repeat(1000);
    let wakes: Vec<String> = (0..300)
        .flat_map(|_| {
            [
                own_row(
                    "task-notification",
                    false,
                    "<task-notification></task-notification>",
                ),
                turn_by("claude-opus-5-5", &format!("Still finished. {pad}")),
            ]
        })
        .collect();
    append(&wakes);
    assert!(std::fs::metadata(&path).expect("len").len() > TAIL_BYTES + 100_000);
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(true), "kept");
    assert!(
        !upgrade::transcript_finished(&tail_to_end(&path, TAIL_BYTES).0, &[]),
        "negative control: the tail alone no longer holds the token"
    );
    assert_eq!(
        Finished::default().of_in(&claude, SESSION, &[]),
        Some(true),
        "a fresh read whose tail settles nothing reads from further back"
    );
    assert_eq!(
        Finished::default().of_in_from(&claude, SESSION, &[], FINISHED_ONCE_BYTES),
        Some(true),
        "a restart's one read starts further back"
    );
    append(&[user_row("Now add the flag.")]);
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(false));
    append(&[land_row(), turn_by("claude-opus-5-5", LANDED_JSON)]);
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(true));
    assert_eq!(Finished::default().of_in(&claude, SESSION, &[]), Some(true));
    write(&finished_rows()[..1]);
    assert_eq!(memo.of_in(&claude, SESSION, &[]), Some(false));
    assert_eq!(memo.of_in(&claude, "other-session", &[]), None);
}

/// A new loop's first read reaches a token behind long wakes ([`Finished::of_snapshot`], what the
/// window's host answers its loop with).
#[cfg(unix)]
#[test]
fn a_new_loops_first_read_reaches_a_token_behind_long_wakes() {
    let dir = scratch("finished-new-loop");
    let pad = "x".repeat(1000);
    let mut rows = finished_rows()[..3].to_vec();
    rows.extend((0..300).flat_map(|_| {
        [
            own_row(
                "task-notification",
                false,
                "<task-notification></task-notification>",
            ),
            turn_by("claude-opus-5-5", &format!("Still finished. {pad}")),
        ]
    }));
    write_transcript(&dir, &rows);
    let len = std::fs::metadata(dir.join(format!(".claude/projects/p/{SESSION}.jsonl")))
        .expect("len")
        .len();
    assert!(
        len > TAIL_BYTES + 50_000 && len < FINISHED_ONCE_BYTES,
        "{len}"
    );
    let snap = users_agent(SESSION);
    assert_eq!(
        Finished::default().of_snapshot(&dir, &snap, &[]),
        Some(true),
        "a new loop's first read"
    );
    let path = dir.join(format!(".claude/projects/p/{SESSION}.jsonl"));
    assert!(
        !upgrade::transcript_finished(&tail_to_end(&path, TAIL_BYTES).0, &[]),
        "negative control: the tail alone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A token behind more wakes than a restart's one read still ends the task.
#[cfg(unix)]
#[test]
fn a_token_behind_more_wakes_than_a_restarts_read_ends_the_task() {
    let pad = "x".repeat(4000);
    let wakes: Vec<String> = (0..1200)
        .flat_map(|_| {
            [
                own_row(
                    "task-notification",
                    false,
                    "<task-notification></task-notification>",
                ),
                turn_by("claude-opus-5-5", &format!("Still here. {pad}")),
            ]
        })
        .collect();
    let then = |head: &[String]| -> Vec<String> { head.iter().chain(&wakes).cloned().collect() };
    let finished = then(&finished_rows()[..3]);
    assert!(finished.iter().map(String::len).sum::<usize>() as u64 > FINISHED_ONCE_BYTES + 100_000);
    let rig = Rig::new("carry-finished-behind-more-wakes", &finished);
    assert!(
        !upgrade::transcript_finished(&tail_to_end(&rig.path, FINISHED_ONCE_BYTES).0, &[]),
        "negative control: the window alone reads open"
    );
    let mut st = rig.st.clone();
    st.cause = CAUSE_HOST.to_string();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:finished", "{r:?}");
    assert!(rig.typed().is_empty(), "{:?}", rig.typed());
    drop(rig);
    let mut reopened = finished_rows()[..3].to_vec();
    reopened.extend([
        user_row("Now add the flag."),
        turn_by("claude-opus-5-5", "Started on the flag."),
    ]);
    for (i, (what, head)) in [
        (
            "an open task",
            vec![
                user_row("Fix the lexer and land it."),
                turn_by("claude-opus-5-5", "Landed the first half."),
            ],
        ),
        ("a person's prompt after the token", reopened),
    ]
    .into_iter()
    .enumerate()
    {
        let rig = Rig::new(&format!("carry-open-behind-more-wakes-{i}"), &then(&head));
        let mut st = rig.st.clone();
        st.cause = CAUSE_HOST.to_string();
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued", "{what}: {r:?}");
        assert_eq!(rig.typed().len(), 1, "{what}: {r:?}");
    }
}

/// The settle counts how long the server's own verdict has said the agent's turn is over
/// (`status agent=idle agent_since_ms=`).
#[cfg(unix)]
#[test]
fn the_settle_is_measured_from_the_agents_verdict_not_from_a_repaint() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let kernel = || Script::new(shell, usize::MAX, None);
    let idle = |ms: u32| format!("OK schema=1 hold=0 hand=- agent=idle agent_since_ms={ms}");
    let other = |word: &str| format!("OK schema=1 hold=0 hand=- agent={word} agent_since_ms=90000");
    for (name, last_seq, status, want) in [
        ("settle-idle-30s", 76, Some(idle(30_000)), "would-announce"),
        ("settle-idle-5s", 76, Some(idle(5_000)), "wait:settling"),
        ("settle-busy", 77, Some(other("busy")), "wait:settling"),
        ("settle-no-verdict", 76, None, "wait:settling"),
        (
            "settle-unknown-moved",
            76,
            Some(other("unknown")),
            "wait:settling",
        ),
        (
            "settle-unknown-still",
            77,
            Some(other("unknown")),
            "would-announce",
        ),
    ] {
        let (r, _, typed) = visit_seen(
            name,
            &asked,
            true,
            &kernel(),
            last_seq,
            status.as_deref(),
            None,
        );
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}");
    }
}

/// Under `upgrade_ask = true` a finished conversation moves as a quiet move
/// ([`upgrade::Facts::quiet`]).
#[cfg(unix)]
#[test]
fn a_finished_conversation_is_moved_quietly_under_upgrade_ask() {
    let shell = dead_pid();
    let kernel = || Script::new(shell, usize::MAX, None);
    let idle = Some("OK schema=1 hold=0 hand=- agent=idle agent_since_ms=30000");
    let open = [
        user_row("Fix the lexer and land it."),
        turn_by("claude-opus-5-5", "Landed the first half."),
    ];
    for (name, rows, announced) in [
        ("ask-finished", finished_rows(), false),
        ("ask-open", open.to_vec(), true),
    ] {
        let (r, _, typed) = visit_seen(name, &rows, true, &kernel(), 76, idle, None);
        let want = if announced {
            "would-announce"
        } else {
            "would-restart"
        };
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}");
    }
}

/// Under `upgrade_ask = true` a person's own exchange left to them moves as a quiet move
/// ([`left_to_person`]).
#[cfg(unix)]
#[test]
fn a_persons_own_exchange_left_to_them_is_moved_quietly_under_upgrade_ask() {
    use crate::supervise::approvals;
    use crate::supervise::policy::turn_end::RULE_LEFT;
    let shell = dead_pid();
    let kernel = || Script::new(shell, usize::MAX, None);
    let idle = Some("OK schema=1 hold=0 hand=- agent=idle agent_since_ms=30000");
    let answer = |uuid: &str, text: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"assistant","uuid":"{uuid}","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    };
    let rows = vec![
        r#"{"type":"user","uuid":"u-1","isSidechain":false,"message":{"role":"user","content":"Fix the lexer and land it."},"origin":{"kind":"human"}}"#.to_string(),
        answer("a-1", "Landed the first half."),
        r#"{"type":"user","uuid":"u-2","isSidechain":false,"message":{"role":"user","content":"What is left?"},"origin":{"kind":"human"}}"#.to_string(),
        answer("a-2", "The parser and the docs."),
    ];
    for (name, key, closed, announced, round) in [
        ("ask-left", Some("prompt:u-2"), false, false, false),
        (
            "ask-left-by-answer",
            Some("answer:a-2"),
            false,
            false,
            false,
        ),
        ("ask-no-left-row", None, false, true, false),
        ("ask-left-closed", Some("prompt:u-2"), true, true, false),
        ("ask-left-earlier", Some("prompt:u-1"), false, true, false),
        ("ask-round-left", Some("prompt:u-2"), false, false, true),
        ("ask-round-no-left-row", None, false, true, true),
    ] {
        let seed = |opts: &Opts| {
            if round {
                let mut st = load(opts, SESSION).expect("the record");
                st.phase = Phase::Announced {
                    at_s: now_s() - 3_600,
                    asks: 1,
                };
                st.marker = "ATERM-UPGRADE-READY-0badf00d".to_string();
                st.tab = TAB.to_string();
                save(opts, SESSION, &st);
            }
            let root = opts.aterm_state.as_deref().expect("the state root");
            let path = approvals::path_under(root, Some(TAB)).expect("ledger dir");
            let row = |rule_id, outcome, command| {
                approvals::Row {
                    rule_id,
                    outcome,
                    command,
                    reason: approvals::LEFT_REASON,
                    box_seq: 1,
                }
                .to_json(1_000, Some(TAB))
            };
            let mut body = Vec::new();
            if let Some(key) = key {
                body.push(row(RULE_LEFT, approvals::Outcome::Skipped, key));
            }
            if closed {
                body.push(row("continue@v1", approvals::Outcome::Typed, "keep going"));
            }
            std::fs::write(path, body.join("\n") + "\n").expect("ledger");
        };
        let (r, _, asked) = visit_launched(
            name,
            &rows,
            true,
            &kernel(),
            76,
            idle,
            Some(&[]),
            Answers::default(),
            &[],
            &seed,
        );
        let want = if announced {
            "would-announce"
        } else {
            "would-restart"
        };
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert!(!asked.iter().any(|l| l.contains(" turn ")), "{name}");
    }
}

/// Under `upgrade_ask = true` a token behind long wakes still moves quietly
/// ([`Records::finished`]).
#[cfg(unix)]
#[test]
fn a_token_behind_long_wakes_is_moved_quietly_under_upgrade_ask() {
    let shell = dead_pid();
    let kernel = || Script::new(shell, usize::MAX, None);
    let idle = Some("OK schema=1 hold=0 hand=- agent=idle agent_since_ms=30000");
    let pad = "x".repeat(1000);
    let wakes: Vec<String> = (0..300)
        .flat_map(|_| {
            [
                own_row(
                    "task-notification",
                    false,
                    "<task-notification></task-notification>",
                ),
                turn_by("claude-opus-5-5", &format!("Still here. {pad}")),
            ]
        })
        .collect();
    let open = vec![
        user_row("Fix the lexer and land it."),
        turn_by("claude-opus-5-5", "Landed the first half."),
    ];
    for (name, head, announced) in [
        (
            "ask-finished-behind-wakes",
            finished_rows()[..3].to_vec(),
            false,
        ),
        ("ask-open-behind-wakes", open, true),
    ] {
        let rows: Vec<String> = head.iter().chain(&wakes).cloned().collect();
        assert!(rows.iter().map(String::len).sum::<usize>() as u64 > TAIL_BYTES + 50_000);
        let (r, _, typed) = visit_seen(name, &rows, true, &kernel(), 76, idle, None);
        let want = if announced {
            "would-announce"
        } else {
            "would-restart"
        };
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}");
    }
}

/// The look's memo reads on from where its last read ended, and one carry-on's readers share one
/// read.
#[cfg(unix)]
#[test]
fn the_looks_memo_reads_on_and_a_carry_ons_readers_share_one_read() {
    let dir = scratch("memo-reads-on");
    let opts = drive(&dir);
    let path = opts
        .home
        .join(format!(".claude/projects/p/{SESSION}.jsonl"));
    let records = Records::of_snapshot(&opts.home, &users_agent(SESSION));
    let claude = opts.home.join(".claude");
    let rewrite = |from: &str, to: &str| {
        assert_eq!(from.len(), to.len());
        let text = std::fs::read_to_string(&path).expect("transcript");
        assert!(text.contains(from), "{from}");
        let wake = own_row(
            "task-notification",
            false,
            "<task-notification></task-notification>",
        );
        std::fs::write(&path, text.replace(from, to) + &wake + "\n").expect("rewrite");
    };
    write_transcript(&opts.home, &finished_rows());
    let memos = std::sync::Mutex::new(Vec::new());
    assert_eq!(
        records.finished_in(&memos, SESSION, &[]),
        Some(true),
        "the first look"
    );
    rewrite("Nothing left:", "Nothing more ");
    assert_eq!(
        records.finished_in(&memos, SESSION, &[]),
        Some(true),
        "the next look reads on from where the first ended"
    );
    let len = std::fs::metadata(&path).expect("len").len();
    assert_eq!(memos.lock().expect("memos")[0].read_to, len);
    assert_eq!(
        Finished::default().of_in_from(&claude, SESSION, &[], FINISHED_ONCE_BYTES),
        Some(false),
        "NEGATIVE CONTROL: a fresh read sees the rewrite"
    );
    write_transcript(
        &opts.home,
        &[
            user_row("Fix the lexer and land it."),
            turn_by("claude-opus-5-5", "All tasks are complete."),
        ],
    );
    let written = WhereWritten::new(&opts, &records, SESSION, &[]);
    assert_eq!(written.finished(), Some(false));
    rewrite("All tasks are complete.", "All tasks are pending..");
    assert_eq!(written.said_done(), Some((true, false)), "the same read");
    assert_eq!(
        WhereWritten::new(&opts, &records, SESSION, &[]).said_done(),
        Some((false, false)),
        "NEGATIVE CONTROL: a read of its own sees the rewrite"
    );
}

/// Upgrade stall tests through the real driver, and the conformance bind of
/// `harness_upgrade_never_strands_model`.
#[path = "upgrade_stall_tests.rs"]
mod stall;

#[cfg(target_os = "macos")]
#[path = "upgrade_drive_kproc_tests.rs"]
mod kproc;

/// A stop that repeats rests longer, never for good: the same reason in a row doubles the rest up
/// to four times `RETRY_S`.
#[test]
fn a_stop_that_repeats_rests_longer_and_never_for_good() {
    let now = 1_000_000;
    let rest = |st: &St| st.failed_at.saturating_add(upgrade::RETRY_S) - now;
    let mut st = St::default();
    st.fail("relaunch-refused", now);
    assert_eq!(rest(&st), upgrade::RETRY_S, "the first stop rests RETRY_S");
    let mut rests = Vec::new();
    for _ in 0..5 {
        st.fail("relaunch-refused", now);
        rests.push(rest(&st));
    }
    let r = upgrade::RETRY_S;
    assert_eq!(
        rests,
        [2 * r, 4 * r, 4 * r, 4 * r, 4 * r],
        "doubling, then capped"
    );
    assert_eq!(st.stop_streak, 6);
    st.fail("signal-refused", now);
    assert_eq!(
        (st.stop_streak, rest(&st)),
        (1, r),
        "a new reason starts over"
    );
    st.fail("signal-refused", now);
    let due_at = st.failed_at + upgrade::RETRY_S;
    assert!(!upgrade::retry_due(
        &st.phase,
        st.failed_for(due_at - 1),
        false
    ));
    assert!(upgrade::retry_due(&st.phase, st.failed_for(due_at), false));
    let back = St::from_json(&st.to_json()).expect("parses");
    assert_eq!(
        (back.stop_streak, back.streak_why.as_str()),
        (2, "signal-refused")
    );
}

/// The login wall: no notice into it, no ask spent on a notice it answered, a late ready honoured.
#[path = "upgrade_login_wall_tests.rs"]
mod login_wall;

/// The record's progress stamp and its deadlines.
#[path = "upgrade_watch_tests.rs"]
mod watch;

/// The end of the agent's own work re-arms a given-up round (`rearmed:unanswered`) and types its
/// first notice in the same visit.
#[cfg(unix)]
#[test]
fn the_end_of_the_agents_work_re_arms_and_asks_in_the_same_visit() {
    let gave_up = |work_seen: bool| St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        failed_at: now_s() - 3_600,
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 1,
        work_seen,
        ..settled()
    };
    let h = Parked::new("work-end-rearm", Answers::default(), gave_up(true), 3600);
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(
        h.details("rearmed:unanswered").len(),
        1,
        "the re-arm is said"
    );
    assert_eq!(h.typed().len(), 1, "one notice");
    let st = load(&h.opts, SESSION).expect("the record");
    assert!(
        matches!(st.phase, Phase::Announced { asks: 1, .. }),
        "{:?}",
        st.phase
    );
    assert!(!st.work_seen, "spent with the notice");
    drop(h);
    let h = Parked::new("work-end-none", Answers::default(), gave_up(false), 3600);
    assert_eq!(h.visit().step, "wait:failed");
    assert!(h.typed().is_empty());
    drop(h);

    let floor = upgrade::WORK_END_FLOOR_S;
    for (since, want) in [(floor + 60, "announced:2"), (60, "wait:awaiting-ready")] {
        let h = Parked::new(&format!("we-{since}"), Answers::default(), settled(), 3600);
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - since,
                asks: 1,
            },
            marker: "ATERM-UPGRADE-READY-00000001".to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            noticed_at: now_s() - since,
            work_seen: true,
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("write");
        assert_eq!(h.visit().step, want, "{since} s since the notice");
        drop(h);
    }
}

/// `Not today` is a `--defer` on the session, carried to the next build by the retarget arm.
#[test]
fn closing_a_reached_pending_record_keeps_the_owners_defer() {
    let dir = scratch("rv-defer");
    let opts = drive(&dir);
    let running = Version::parse("2.1.284").expect("v");
    let record = |request| St {
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 5,
        request,
        request_tab: TAB.to_string(),
        request_at: 10,
        ..St::default()
    };
    let deferred = record(Request::DeferUntil(now_s() + 3_600));
    save(&opts, SESSION, &deferred);
    assert!(forget_settled_pending(&opts, SESSION, Some(&deferred), &running).is_none());
    let kept = load(&opts, SESSION).expect("the record stays");
    assert!(matches!(kept.request, Request::DeferUntil(_)));
    let to285 = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    let next = St::for_target(Some(kept), &running, &to285, None, proc("B"), now_s());
    assert!(
        matches!(next.request, Request::DeferUntil(t) if t > now_s()),
        "the snooze holds for the next build: {:?}",
        next.request
    );
    for request in [Request::DeferUntil(1), Request::None] {
        let spent = record(request);
        save(&opts, SESSION, &spent);
        assert!(forget_settled_pending(&opts, SESSION, Some(&spent), &running).is_some());
        assert!(load(&opts, SESSION).is_none());
    }
}

/// A caller that names no process gets the build test, which cannot tell a same-build restart by
/// hand from the record's process.
#[test]
fn an_unnamed_process_falls_back_to_the_build_test() {
    let running = Version::parse("2.1.283").expect("v");
    let to285 = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    let rec = St {
        phase: Phase::Pending,
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: "A".to_string(),
        ..St::default()
    };
    let named = St::for_target(Some(rec.clone()), &running, &to285, None, proc("B"), 900);
    let unnamed = St::for_target(Some(rec), &running, &to285, None, Proc::default(), 900);
    assert_eq!(named.behind_since(), 900, "B's age begins now");
    assert_eq!(
        unnamed.behind_since(),
        100,
        "unnamed: the same build reads as the same process"
    );
}

/// An idle, settled agent at an idle point: the facts of a look that saw nothing
/// under it (the simulation adds what the adversary runs).
fn cadence_facts() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 600,
        composer_empty: true,
        approval_box: false,
        busy_footer: false,
        background: Vec::new(),
        held: false,
        quiet_s: 600,
        hold_s: 0,
        owner_now: false,
        attended: false,
        background_point: false,
        taskless: false,
        limited: false,
        login: false,
        undelivered: false,
        queued: false,
        ready_s: 0,
        idle_looks: upgrade::IDLE_LOOKS,
        failed_s: 0,
        work_ended: false,
        ..Facts::default()
    }
}

/// The work-end ask is exempt from the round's ask cap and clock, so the driver bounds the fact.
#[test]
fn the_end_of_the_work_asks_no_sooner_than_its_floor_and_no_more_than_its_budget() {
    let now = 1_790_645_000;
    let floor = upgrade::WORK_END_FLOOR_S;
    let ended = |st: &St| {
        let mut facts = cadence_facts();
        st.time_work(&mut facts, now);
        facts.work_ended
    };
    let announced = |since: u64, asks: u32| St {
        phase: Phase::Announced {
            at_s: now - since,
            asks,
        },
        noticed_at: now - since,
        work_seen: true,
        ..St::default()
    };
    assert!(!ended(&announced(60, 2)), "inside the floor");
    assert!(!ended(&announced(floor - 1, 2)), "one second short");
    assert!(ended(&announced(floor, 2)), "at the floor");
    assert!(
        ended(&announced(floor + 60, 9)),
        "the ask count is not the bound"
    );
    assert!(!ended(&St {
        work_seen: false,
        ..announced(floor + 60, 2)
    }));
    let resting = |stopped_ago: u64| St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        failed_at: now - stopped_ago,
        work_seen: true,
        ..St::default()
    };
    assert!(!ended(&resting(60)), "it gave up a minute ago");
    assert!(ended(&resting(floor + 1)));
    assert!(!ended(&St {
        failed_at: now + 3_600,
        ..resting(0)
    }));
    let declined = |stopped_ago: u64, streak: u32| {
        let mut st = St {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            streak_why: upgrade::GAVE_UP.to_string(),
            stop_streak: streak,
            work_seen: true,
            ..St::default()
        };
        st.failed_at = (now - stopped_ago) + st.rest_extension();
        st
    };
    assert!(!ended(&declined(60, 3)), "inside the floor after the stop");
    assert!(ended(&declined(3_600, 2)), "an hour after a streak-2 stop");
    assert!(
        ended(&declined(3 * 3_600, 3)),
        "three hours after a streak-3 stop"
    );
    assert!(
        ended(&declined(10 * 3_600, 4)),
        "ten hours into a day's rest"
    );
    let mut st = announced(floor + 60, 2);
    for spent in 0..upgrade::WORK_END_DAILY {
        assert!(ended(&st), "ask {} is within the budget", spent + 1);
        st.spend_work_end(now);
    }
    assert!(!ended(&st), "the budget is spent");
    assert_eq!(st.work_end_left(now), 0);
    assert_eq!(st.work_end_left(now + upgrade::WORK_END_WINDOW_S - 1), 0);
    assert_eq!(
        st.work_end_left(now + upgrade::WORK_END_WINDOW_S),
        upgrade::WORK_END_DAILY,
        "a day on the budget is whole again"
    );
    let mut rolling = St::default();
    for k in 0..u64::from(upgrade::WORK_END_DAILY) {
        rolling.spend_work_end(now + k * 3_600);
    }
    assert_eq!(rolling.work_end_left(now + 6 * 3_600), 0);
    assert_eq!(
        rolling.work_end_left(now + upgrade::WORK_END_WINDOW_S),
        1,
        "the first ask is a day old, the rest are not"
    );
    let again = St::from_json(&st.to_json()).expect("record");
    assert_eq!(again.work_end_at, st.work_end_at);
}

/// The seconds at which an agent that declines every notice is asked over `hours`, by the
/// real reducer and record transitions: after each notice a job runs under it for `job_s`
/// seconds, then ends.
fn declined_notices(hours: u64, job_s: u64) -> Vec<u64> {
    let t0 = 1_790_000_000;
    let to = Version::parse("2.1.284").expect("v");
    let mut st = St {
        phase: Phase::Pending,
        from: "2.1.283".to_string(),
        to: to.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    let mut notices: Vec<u64> = Vec::new();
    let mut last_notice = 0;
    for minute in 0..hours * 60 {
        let now = t0 + minute * 60;
        let mut facts = cadence_facts();
        if last_notice != 0 && now < last_notice + job_s {
            facts.background = vec!["zsh".to_string()];
        }
        st.saw_work(&facts);
        st.time_failed(&mut facts, now);
        st.time_work(&mut facts, now);
        match upgrade::next_step(&st.phase, &facts, false, now) {
            Step::Announce => {
                let asks = upgrade::announce_asks(&st.phase, false);
                if facts.work_ended {
                    st.spend_work_end(now);
                }
                let marker =
                    upgrade::ready_marker(SESSION, &to, st.salt.wrapping_add(u64::from(asks)));
                st.announced(marker, now, asks);
                last_notice = now;
                notices.push(now);
            }
            Step::GiveUp => st.give_up(now),
            Step::Rearm => {
                st.rearm(now);
            }
            _ => {}
        }
    }
    notices
}

/// A session that keeps starting and ending work is asked a bounded number of times a day.
#[test]
fn a_declining_agent_is_asked_a_bounded_number_of_times_a_day() {
    let day = 86_400;
    let rolling = |notices: &[u64], from: u64| {
        notices
            .iter()
            .filter(|&&t| t >= from)
            .map(|&first| {
                notices
                    .iter()
                    .filter(|&&t| t >= first && t < first + day)
                    .count()
            })
            .max()
            .unwrap_or(0)
    };
    let rounds = usize::try_from(upgrade::MAX_ASKS).expect("small");
    let budget = usize::try_from(upgrade::WORK_END_DAILY).expect("small");
    let first_day = 3 * rounds + budget + 1;
    let steady = rounds + budget + 1;
    for job_s in [120, 300, 660, 1_500] {
        let notices = declined_notices(120, job_s);
        let t0 = notices[0];
        assert!(notices.len() >= 8, "the round does ask: {job_s}");
        let busiest = rolling(&notices, 0);
        assert!(
            busiest <= first_day,
            "a job of {job_s} s: {busiest} notices in one day, more than {first_day}: {notices:?}"
        );
        let later = rolling(&notices, t0 + 3 * day);
        assert!(
            later <= steady,
            "a job of {job_s} s, from the fourth day: {later} in one day, more than {steady}"
        );
    }
}

/// The ask at the end of the agent's own work needs nothing under the agent and Claude's own status
/// idle.
#[cfg(unix)]
#[test]
fn tab_ones_recorded_shape_is_not_asked_until_nothing_runs_and_claude_idles() {
    let marker = "ATERM-UPGRADE-READY-00000001";
    let cases = [
        (true, "busy", "wait:awaiting-ready"),
        (false, "busy", "wait:status-stale"),
        (true, "idle", "wait:awaiting-ready"),
        (false, "idle", "announced:3"),
    ];
    for (n, (watcher, status, want)) in cases.into_iter().enumerate() {
        let mut h = Parked::new(&format!("t1s{n}"), Answers::default(), settled(), 3600);
        set_status(&mut h, status);
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - 900,
                asks: 2,
            },
            noticed_at: now_s() - 900,
            marker: marker.to_string(),
            markers: vec![marker.to_string()],
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            work_seen: true,
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        let table = if watcher {
            shells_under(&h)
        } else {
            Vec::new()
        };
        let word = h.visit_with(dead_pid(), &table).step;
        assert_eq!(word, want, "watcher: {watcher}, status: {status}");
        assert_eq!(
            h.typed().len(),
            usize::from(want.starts_with("announced")),
            "watcher: {watcher}, status: {status}"
        );
    }
}

/// An idle Claude Code screen whose last answer is `said`, one row a line
/// (the first under `⏺`, the rest at the message's column), its empty
/// composer holding the caret, read at `seq`.
#[cfg(unix)]
fn said_screen(said: &[&str], seq: u64) -> String {
    let rule = "─".repeat(40);
    let mut rows: Vec<String> = said
        .iter()
        .enumerate()
        .map(|(i, line)| match (i, line.is_empty()) {
            (0, _) => format!("⏺ {line}"),
            (_, true) => String::new(),
            (_, false) => format!("  {line}"),
        })
        .collect();
    rows.extend([String::new(), rule.clone()]);
    let caret = rows.len();
    rows.extend(["❯ ".to_string(), rule]);
    let rows = rows
        .iter()
        .map(|r| aterm_json::to_string(&Value::from(r.as_str())).expect("row"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"rows":[{rows}],"cursor":{{"row":{caret},"col":2}},"seq":{seq},"human_ms":null,"first":0}}"#
    )
}

/// A ready answer on the idle screen before Claude Code appends its transcript row (a batched
/// append, `FLUSH_INTERVAL_MS=100`).
#[cfg(unix)]
#[test]
fn a_ready_answer_is_heard_at_its_turn_end_and_a_superseded_one_is_asked_again() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let answer = [
        "Step 2 increment 2 passed, and it's merged and pushed.",
        "",
        "Result. The codegen path now keeps THIR alive: 5 of 6 functions.",
        "",
        "Nothing of mine is running now, so this is a good point for the upgrade:",
        "",
        marker,
    ];
    let p = Parked::new(
        "ready-at-turn-end",
        Answers {
            screen: said_screen(&answer, 78),
            ..Answers::default()
        },
        St::default(),
        60,
    );
    let screen_file = p.dir.join("t.screen");
    let grace = p.opts.human_grace_s;
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 945,
            asks: 1,
        },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        markers: vec![marker.to_string()],
        asked: vec![marker.to_string()],
        tab: TAB.to_string(),
        notice_pid: p.sf.pid,
        notice_start: squash(&p.sf.proc_start),
        last_seq: 78,
        seq_since_s: now,
        ..St::default()
    };
    std::fs::write(state_path(&p.opts, SESSION), st.to_json()).expect("state");
    let json = |text: &str| aterm_json::to_string(&Value::from(text)).expect("json");
    let typed = |text: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"origin":{{"kind":"human"}},"promptSource":"typed","version":"2.1.284"}}"#,
            json(text)
        )
    };
    let model = |text: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}],"stop_reason":"end_turn"}},"version":"2.1.284"}}"#,
            json(text)
        )
    };
    let notice = upgrade::prepare_prompt(
        &Version::parse("1.0.0").expect("from"),
        &Version::parse("9.9.9").expect("to"),
        Source::Managed,
        marker,
    );
    write_transcript(
        &p.opts.home,
        &[
            ASKED.to_string(),
            typed(&format!(
                "\n\n<pasted_content id=\"6dd8\">\n{notice}\n</pasted_content id=\"6dd8\">\n"
            )),
            model("Not yet: my build still runs. I will answer once it is done."),
        ],
    );
    let append = |rows: &[String]| {
        let path = transcript(&p.opts.home, SESSION).expect("transcript");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("append");
        for row in rows {
            writeln!(f, "{row}").expect("row");
        }
    };

    std::fs::write(&screen_file, said_screen(&["Still building."], 77)).expect("screen");
    let control = p.visit();
    assert_eq!(control.step, "wait:awaiting-ready");
    assert!(!owns_turn_ends(&control.step, 0, grace));
    std::fs::remove_file(&screen_file).expect("screen");

    let shown = p.visit();
    assert_eq!(shown.step, "wait:ready-unrecorded", "{shown:?}");
    assert!(
        owns_turn_ends(&shown.step, 0, grace),
        "nothing typed over it"
    );
    assert!(
        !owns_turn_ends(&shown.step, OWNED_SETTLE_LOOKS, grace),
        "bounded"
    );
    assert_eq!(
        load(&p.opts, SESSION).expect("state").wait,
        "ready-unrecorded"
    );

    append(&[model(&answer.join("\n"))]);
    let heard = p.visit();
    assert_eq!(heard.step, "wait:settling", "{heard:?}");
    assert!(owns_turn_ends(&heard.step, 0, grace));
    assert_ne!(load(&p.opts, SESSION).expect("state").ready_since, 0);

    append(&[
        typed("keep going"),
        model("Two pieces of work are running in the background."),
    ]);
    std::fs::write(
        &screen_file,
        said_screen(&["Two pieces of work are running in the background."], 79),
    )
    .expect("screen");
    let moved_on = p.visit();
    assert_eq!(moved_on.step, "wait:superseded", "{moved_on:?}");
    assert!(owns_turn_ends(&moved_on.step, 0, grace));
    assert_eq!(load(&p.opts, SESSION).expect("state").wait, "superseded");
    assert_eq!(turns(&p.asked), 0, "nothing typed yet");

    let mut st = load(&p.opts, SESSION).expect("state");
    st.seq_since_s = now_s() - 60;
    std::fs::write(state_path(&p.opts, SESSION), st.to_json()).expect("state");
    let asked_again = p.visit();
    assert_eq!(asked_again.step, "announced:2", "{asked_again:?}");
    assert_eq!(turns(&p.asked), 1);
    let st = load(&p.opts, SESSION).expect("state");
    assert!(
        matches!(st.phase, Phase::Announced { asks: 2, .. }),
        "{st:?}"
    );
    assert_ne!(st.marker, marker, "a marker of its own");
    assert!(alive(p.sf.pid), "the agent was never signalled");
}

/// [`named_standing_wall`] reads the quota wall over the same bounded tail as [`named_limit_reset`]
/// ([`TAIL_BYTES`]).
#[cfg(unix)]
#[test]
fn the_standing_wall_is_read_from_the_named_transcripts_bounded_tail() {
    let dir = scratch("standing-wall");
    let home = dir.join("home");
    let wall = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-30T19:02:11.123Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]},"quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"error":"rate_limit","isApiErrorMessage":true}"#.to_string();
    let read = upgrade::StandingWall {
        at: 1_790_794_931,
        until: Some(1_790_870_400),
        window: Some("seven_day".to_string()),
        credits: Some(upgrade::CreditsWord::Off {
            reason: Some("org_level_disabled".to_string()),
            until: None,
        }),
    };
    let filler = format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{}"}}}}"#,
        "x".repeat(4000)
    );
    let mut rows = vec![filler.clone(); 256];
    rows.push(wall.clone());
    write_transcript(&home, &rows);
    assert_eq!(
        named_standing_wall(&home, &users_agent(SESSION)),
        Some(read)
    );
    assert_eq!(
        named_limit_reset(&home, &users_agent(SESSION)),
        Some(1_790_870_400),
        "the same row"
    );
    let mut rows = vec![wall];
    rows.extend(vec![filler; 80]);
    assert!(rows[1..].iter().map(String::len).sum::<usize>() as u64 > TAIL_BYTES);
    write_transcript(&home, &rows);
    assert_eq!(named_standing_wall(&home, &users_agent(SESSION)), None);
}

/// [`named_reset_line`]: the line the usage-limit dialog ended with, over the same bounded tail.
#[cfg(unix)]
#[test]
fn the_reset_line_is_read_from_the_named_transcripts_bounded_tail() {
    let dir = scratch("reset-line");
    let home = dir.join("home");
    let claimed = 1_790_794_931;
    let said = "Weekly reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let row = |content: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","timestamp":"2026-09-30T19:02:40.500Z","message":{{"role":"user","content":"{content}"}}}}"#
        )
    };
    let filler = format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{}"}}}}"#,
        "x".repeat(4000)
    );
    let mut rows = vec![filler.clone(); 256];
    rows.push(row("<command-name>/rate-limit-options</command-name>"));
    rows.push(row(&format!(
        "<local-command-stdout>{said}</local-command-stdout>"
    )));
    write_transcript(&home, &rows);
    assert_eq!(
        named_reset_line(&home, &users_agent(SESSION), claimed).as_deref(),
        Some(said)
    );
    assert_eq!(
        named_reset_line(&home, &users_agent(SESSION), claimed + 60),
        None
    );
}

/// Window host limit and wall readers read the transcript under the agent's own Claude home
/// ([`Records::of_snapshot`]).
#[cfg(unix)]
#[test]
fn the_window_hosts_limit_and_wall_readers_read_the_agents_own_claude_home() {
    let dir = scratch("named-own-home");
    let user_home = dir.join("home");
    std::fs::create_dir_all(user_home.join(".claude/projects/p")).expect("the user's projects");
    let claimed = 1_790_794_931;
    let said = "Weekly reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let wall = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-30T19:02:11.123Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]},"quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"error":"rate_limit","isApiErrorMessage":true}"#;
    let row = |content: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","timestamp":"2026-09-30T19:02:40.500Z","message":{{"role":"user","content":"{content}"}}}}"#
        )
    };
    let rows = [
        wall.to_string(),
        row("<command-name>/rate-limit-options</command-name>"),
        row(&format!(
            "<local-command-stdout>{said}</local-command-stdout>"
        )),
    ];
    let file = |claude_dir: &Path| {
        let project = claude_dir.join("projects/p");
        std::fs::create_dir_all(&project).expect("its project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            rows.join("\n") + "\n",
        )
        .expect("its transcript");
    };
    let work = dir.join("work");
    let snap = |home: Option<&str>, claude_dir: Option<PathBuf>| Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: home.map(str::to_string),
        claude_dir,
    };
    let read = |snap: &Snapshot| {
        (
            named_limit_reset(&user_home, snap),
            named_standing_wall(&user_home, snap).map(|w| w.until),
            named_reset_line(&user_home, snap, claimed),
        )
    };
    let named = (
        Some(1_790_870_400),
        Some(Some(1_790_870_400)),
        Some(said.to_string()),
    );
    let own = dir.join("work home/.claude");
    file(&own);
    let absolute = own.to_string_lossy().into_owned();
    assert_eq!(
        read(&snap(Some(&absolute), Some(own.clone()))),
        named,
        "its own absolute home"
    );
    let relative = work.join("rel/.claude");
    file(&relative);
    assert_eq!(
        read(&snap(Some("rel/.claude"), None)),
        named,
        "a relative home"
    );
    file(&work);
    assert_eq!(read(&snap(Some(""), None)), named, "set and empty");
    assert_eq!(
        read(&snap(None, None)),
        (None, None, None),
        "not the user's"
    );
    file(&user_home.join(".claude"));
    assert_eq!(read(&snap(None, None)), named, "the user's own");
    let unknown = Snapshot {
        cwd: String::new(),
        ..snap(Some("nowhere/.claude"), None)
    };
    assert_eq!(read(&unknown), (None, None, None), "no directory known");
    let unnamed = Snapshot {
        session: None,
        ..snap(Some(&absolute), Some(own))
    };
    assert_eq!(read(&unnamed), (None, None, None), "no conversation");
}

/// [`super::super::relaunch::account_restart`] names the account move whose restart signalled this
/// process in this tab.
#[test]
fn the_account_move_that_signalled_an_agent_is_named_by_its_label() {
    use super::super::relaunch::{account_restart, restarted};
    let a = DeadAgent::new("account-restart");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let record = |cause: &str, phase: Phase, exited_at: u64| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.287".to_string(),
        to: "2.1.287".to_string(),
        source: "same".to_string(),
        cause: cause.to_string(),
        exited_at,
        ..St::default()
    };
    assert_eq!(account_restart(&opts, pid), None, "no record at all");
    let exiting = Phase::Exiting { at_s: now };
    save(
        &opts,
        SESSION,
        &record("account:work-2", exiting.clone(), 0),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));
    save(
        &opts,
        SESSION,
        &record("account:work-2", Phase::Relaunched { at_s: now }, now),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));
    let stopped = Phase::Failed("no-resume".to_string());
    save(
        &opts,
        SESSION,
        &record("account:work-2", stopped.clone(), now),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));

    for cause in ["", "memory", "model:claude-sonnet-5-5"] {
        save(&opts, SESSION, &record(cause, exiting.clone(), 0));
        assert!(
            restarted(&opts, upgrade::Agent::Claude, Some(pid)),
            "{cause:?}: the harness did signal it"
        );
        assert_eq!(
            account_restart(&opts, pid),
            None,
            "{cause:?}: for no account move"
        );
    }
    save(&opts, SESSION, &record("account:work-2", exiting, 0));
    assert_eq!(account_restart(&opts, dead_pid()), None, "another process");
    let other_tab = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert_eq!(account_restart(&other_tab, pid), None, "another tab");
    save(
        &opts,
        SESSION,
        &record("account:work-2", stopped, now - 2 * STALE_S - 1),
    );
    assert_eq!(
        account_restart(&opts, pid),
        None,
        "a stop long past its exit is no longer this exit's"
    );
}

/// [`conversation_under`]: a conversation is under the home whose `projects/` holds its transcript.
#[cfg(unix)]
#[test]
fn a_conversation_is_under_the_home_its_transcript_is_filed_in() {
    let dir = scratch("conversation-under");
    let home = dir.join("home");
    write_transcript(&home, &["{}".to_string()]);
    assert!(conversation_under(&home, SESSION));
    assert!(!conversation_under(
        &home,
        "00000000-0000-4000-8000-000000000000"
    ));
    let other = dir.join("other-home");
    assert!(!conversation_under(&other, SESSION), "no such home");
    std::fs::create_dir_all(other.join(".claude/projects")).expect("projects");
    assert!(!conversation_under(&other, SESSION), "no project holds it");
    std::fs::write(home.join(".claude/projects/outside.jsonl"), "{}\n").expect("a file");
    for name in ["", "../outside", "p/../../outside", "not a session"] {
        assert!(!conversation_under(&home, name), "{name:?}");
    }
    let dir_named = "0badf00d-1111-2222-3333-44445555aaaa";
    std::fs::create_dir_all(home.join(format!(".claude/projects/p/{dir_named}.jsonl")))
        .expect("a directory");
    assert!(!conversation_under(&home, dir_named));
}

/// [`process_transcript_tail`] finds a process's conversation under its own `CLAUDE_CONFIG_DIR`.
#[test]
fn the_transcript_that_confirms_a_wall_is_found_from_the_process() {
    let root = ScratchDir::new("upgrade", "wall-tail");
    let home = root.join("home");
    let alt = root.join("claude-alt");
    let start = "Thu Oct 8 18:04:23 2026";
    let cwd = "/private/tmp/at.0000/proj";
    let slug = "-private-tmp-at-0000-proj";
    let pid = dead_pid();
    let record = |dir: &Path, id: &str, proc_start: &str| {
        std::fs::create_dir_all(dir.join("sessions")).expect("sessions");
        std::fs::write(
            dir.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{cwd}","procStart":"{proc_start}","version":"2.1.294","kind":"interactive","entrypoint":"cli","status":"idle"}}"#
            ),
        )
        .expect("record");
    };
    let transcript = |dir: &Path, project: &str, id: &str, text: &str| {
        let at = dir.join("projects").join(project);
        std::fs::create_dir_all(&at).expect("project");
        std::fs::write(at.join(format!("{id}.jsonl")), text).expect("transcript");
    };
    let alt_id = "da373640-37c2-443b-b001-4b6cab95a6cf";
    let home_id = "80fb925f-a7d3-401b-9031-8241ff9919ab";
    record(&alt, alt_id, start);
    transcript(&alt, slug, alt_id, "{\"the\":\"alt tail\"}\n");
    record(&home.join(".claude"), home_id, start);
    transcript(
        &home.join(".claude"),
        slug,
        home_id,
        "{\"the\":\"home tail\"}\n",
    );
    let tail = |config: Option<&Path>, at: &str| {
        process_transcript_tail(config.and_then(Path::to_str), home.to_str(), None, pid, at)
    };
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the process's own CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        tail(None, start).as_deref(),
        Some("{\"the\":\"home tail\"}\n"),
        "no config directory: the home's .claude"
    );
    assert_eq!(tail(Some(&alt), "Thu Oct 8 18:04:24 2026"), None);
    assert_eq!(tail(Some(&alt), "not a start"), None);
    transcript(&alt, "-elsewhere", alt_id, "{\"the\":\"planted\"}\n");
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the project's own transcript, not the planted one"
    );
    std::fs::remove_file(
        alt.join("projects")
            .join(slug)
            .join(format!("{alt_id}.jsonl")),
    )
    .expect("its own place emptied");
    transcript(&alt, "-moved", alt_id, "{\"the\":\"moved\"}\n");
    assert_eq!(
        tail(Some(&alt), start),
        None,
        "two, and none in its own place"
    );
    std::fs::remove_file(
        alt.join("projects/-elsewhere")
            .join(format!("{alt_id}.jsonl")),
    )
    .expect("planted");
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"moved\"}\n"),
        "one elsewhere, read"
    );
    std::fs::remove_file(alt.join("projects/-moved").join(format!("{alt_id}.jsonl")))
        .expect("moved");
    assert_eq!(tail(Some(&alt), start), None, "no transcript");
    std::fs::remove_file(alt.join(format!("sessions/{pid}.json"))).expect("record");
    assert_eq!(tail(Some(&alt), start), None, "no record");
}

/// [`snapshot_transcript_tail`] takes the config directory from the live process's exec
/// environment, never the home's `.claude`.
#[cfg(unix)]
#[test]
fn the_transcript_is_found_under_the_live_processs_own_config_dir() {
    let root = ScratchDir::new("upgrade", "wall-tail-live");
    let home = root.join("home");
    let alt = root.join("claude-alt");
    let start = "Thu Oct 8 18:04:23 2026";
    let cwd = "/private/tmp/at.0000/proj";
    let slug = "-private-tmp-at-0000-proj";
    let keep = |dir: &Path, pid: u32, id: &str, text: &str| {
        std::fs::create_dir_all(dir.join("sessions")).expect("sessions");
        std::fs::write(
            dir.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{cwd}","procStart":"{start}","version":"2.1.294","kind":"interactive","entrypoint":"cli","status":"idle"}}"#
            ),
        )
        .expect("record");
        let at = dir.join("projects").join(slug);
        std::fs::create_dir_all(&at).expect("project");
        std::fs::write(at.join(format!("{id}.jsonl")), text).expect("transcript");
    };
    let tail_of = |config: Option<&Path>, runs_in: Option<&Path>| {
        let mut c = parked();
        c.env("HOME", &home);
        if let Some(dir) = runs_in {
            c.current_dir(dir);
        }
        match config {
            Some(dir) => c.env("CLAUDE_CONFIG_DIR", dir),
            None => c.env_remove("CLAUDE_CONFIG_DIR"),
        };
        let mut agent = c.spawn().expect("the stand-in");
        let pid = agent.id();
        wait_exec(pid);
        keep(
            &alt,
            pid,
            "da373640-37c2-443b-b001-4b6cab95a6cf",
            "{\"the\":\"alt tail\"}\n",
        );
        keep(
            &home.join(".claude"),
            pid,
            "80fb925f-a7d3-401b-9031-8241ff9919ab",
            "{\"the\":\"home tail\"}\n",
        );
        let snap = Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid,
            start: start.to_string(),
            shell: 1,
            program: PathBuf::from("/usr/local/bin/claude"),
            argv: vec!["claude".to_string()],
            session: None,
            cwd: cwd.to_string(),
            version: None,
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let tail = snapshot_transcript_tail(&snap);
        let _ = agent.kill();
        let _ = agent.wait();
        tail
    };
    assert_eq!(
        tail_of(Some(&alt), None).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the process's own CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        tail_of(None, None).as_deref(),
        Some("{\"the\":\"home tail\"}\n"),
        "none: its HOME's .claude"
    );
    assert_eq!(
        tail_of(Some(Path::new("claude-alt")), Some(&root)).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "a relative one, under the directory it runs in"
    );
    assert_eq!(
        tail_of(Some(Path::new("")), Some(&alt)).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "set and empty: the directory it runs in"
    );
}

/// The CLI's sweep judges an empty-home agent by its login's profile (`$HOME/.claude.json`,
/// [`profile_apart`]) also after a window recorded it.
#[cfg(unix)]
#[test]
fn the_clis_sweep_judges_an_empty_homes_agent_a_window_recorded_by_its_login() {
    const LEADER: u32 = 4_242;
    let dir = scratch("cli-empty-home-recorded");
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={LEADER}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let listed = Opts {
        sock: Some(sock),
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let unlisted = Opts {
        dry_run: true,
        usage_credits: true,
        ..drive(&dir)
    };
    let work = dir.join("work");
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    for claude_dir in [listed.home.join(".claude"), work.clone()] {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    }
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&listed, &targets);
    std::fs::create_dir_all(work.join("sessions")).expect("sessions");
    std::fs::write(
        work.join(format!("sessions/{LEADER}.json")),
        format!(
            r#"{{"pid":{LEADER},"sessionId":"{SESSION}","cwd":"{}","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#,
            work.display()
        ),
    )
    .expect("the record");
    let proj = work.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    std::fs::write(
        listed.home.join(".claude.json"),
        r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#,
    )
    .expect("the login's profile");
    let args = |_: u32| {
        Some(atpkg::caller_shell::ProcArgs {
            env: vec!["CLAUDE_CONFIG_DIR=".to_string()],
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    let record = |opts: &Opts, tabs: Option<&[LiveTab]>| {
        Homes::read(opts, tabs, args, |_| Some(work.clone()))
            .roster(Some(Vec::new()))
            .and_then(|files| files.into_iter().find(|sf| sf.pid == LEADER))
    };
    let verdict = |opts: &Opts| {
        record(opts, None).map(|sf| {
            let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
            model_read_for(opts, &sf, None, &mctx, false, None, None).verdict
        })
    };
    let keep = Some(ModelVerdict::Keep("model-current"));
    assert_eq!(verdict(&listed), keep, "CONTROL: read from its tab");
    assert_eq!(verdict(&unlisted), None, "CONTROL: nothing names its home");
    std::fs::create_dir_all(state_dir(&listed)).expect("state");
    let older = St {
        home: Records::at(&listed.home, HomeVars::config(Some("")), || {
            Some(work.clone())
        })
        .home_word(&listed.home),
        ..St::default()
    };
    std::fs::write(state_path(&listed, SESSION), older.to_json()).expect("the older record");
    assert_eq!(
        verdict(&listed),
        keep,
        "a home read off its live leader comes before a record's word"
    );
    std::fs::remove_file(state_path(&listed, SESSION)).expect("rm");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(LEADER)),
    }];
    let sf = record(&listed, Some(&tabs)).expect("the window reads its record");
    assert!(
        mint_behind(
            &Opts {
                dry_run: false,
                ..listed.clone()
            },
            &sf,
            TAB,
            &newer(),
            false,
            now_s(),
        ),
        "noted behind"
    );
    let (after_listed, after_unlisted) = (verdict(&listed), verdict(&unlisted));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        (after_listed, after_unlisted),
        (keep.clone(), keep),
        "judged by its login's denial, from its tab and from the window's record alike"
    );
}

/// Under a relative home, a Claude Code that changed directory keeps its record where it started
/// but writes its conversation under the home its new directory names.
#[cfg(unix)]
#[test]
fn the_walls_tail_is_the_transcript_its_agent_writes_now() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool, written_now: Option<&str>| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("wt")).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let runs_in = if moved {
            started.join("wt")
        } else {
            started.clone()
        };
        let claude_dir = started.join(RELATIVE);
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(&runs_in),
        );
        let pid = agent.id();
        let start = kernel_start(pid).expect("lstart");
        register_running_in(&claude_dir, pid, &start, &started);
        file_transcript_in(&claude_dir, &crate::harness::footer::project_slug(&started));
        if let Some(text) = written_now {
            let now_dir = runs_in
                .join(RELATIVE)
                .join("projects")
                .join(crate::harness::footer::project_slug(&runs_in));
            std::fs::create_dir_all(&now_dir).expect("projects");
            std::fs::write(
                now_dir.join(format!("{SESSION}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
                ),
            )
            .expect("transcript");
        }
        let tail = snapshot_transcript_tail(&Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid,
            start,
            shell: 1,
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["claude".to_string()],
            session: None,
            cwd: "/".to_string(),
            version: None,
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: Some(RELATIVE.to_string()),
            claude_dir: None,
        });
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        tail
    };
    let unmoved = case("wall-tail-unmoved", false, None);
    let moved = case("wall-tail-moved", true, Some("WRITTEN-NOW limit notice"));
    let unwritten = case("wall-tail-moved-unwritten", true, None);
    assert!(
        unmoved
            .as_deref()
            .is_some_and(|t| t.contains("port the parser")),
        "CONTROL: an agent that never moved, its transcript's: {unmoved:?}"
    );
    assert!(
        moved
            .as_deref()
            .is_some_and(|t| t.contains("WRITTEN-NOW") && !t.contains("port the parser")),
        "the transcript it writes now: {moved:?}"
    );
    assert_eq!(
        unwritten, None,
        "nothing written where it writes now: no tail, never the one it left"
    );
}

/// A moved agent's finish is read where it writes now.
#[cfg(unix)]
#[test]
fn a_moved_agents_finish_is_read_where_it_writes_now() {
    const RELATIVE: &str = "other/.claude";
    const M: &str = "claude-opus-5-5";
    let dir = scratch("moved-finish");
    let home = dir.join("home");
    std::fs::create_dir_all(dir.join("work/wt")).expect("its directories");
    let started = std::fs::canonicalize(dir.join("work")).expect("where it started");
    let wt = started.join("wt");
    let filed = started.join(RELATIVE);
    let writes = wt.join(RELATIVE);
    let slug = crate::harness::footer::project_slug;
    let at = |claude: &Path, cwd: &Path| {
        let d = claude.join("projects").join(slug(cwd));
        std::fs::create_dir_all(&d).expect("projects");
        d.join(format!("{SESSION}.jsonl"))
    };
    let start_file = at(&filed, &started);
    let wt_file = at(&writes, &wt);
    let lines = |rows: &[String]| rows.join("\n") + "\n";
    std::fs::write(
        &start_file,
        lines(&[
            user_row("Fix the lexer and land it."),
            turn_by(M, "Entering a worktree."),
        ]),
    )
    .expect("the start file");
    std::fs::write(
        &wt_file,
        lines(&[
            land_row(),
            turn_by(M, LANDED_JSON),
            own_row(
                "task-notification",
                false,
                "<task-notification></task-notification>",
            ),
            turn_by(M, "The background build finished."),
        ]),
    )
    .expect("the worktree file");
    let snap = Snapshot {
        home: Some(RELATIVE.to_string()),
        claude_dir: Some(filed.clone()),
        cwd: started.to_string_lossy().into_owned(),
        ..users_agent(SESSION)
    };
    let read = |cwd: &Path| {
        Finished::default().of_snapshot_at(&home, &snap, &[], || Some(cwd.to_path_buf()))
    };
    assert_eq!(read(&wt), Some(true), "where it writes now");
    assert_eq!(read(&started), Some(false), "CONTROL: never moved");
    let tail =
        |cwd: &Path| snapshot_tail_at(&home, &snap, || Some(cwd.to_path_buf())).unwrap_or_default();
    assert!(
        tail(&wt).contains("background build finished"),
        "{}",
        tail(&wt)
    );
    assert!(!tail(&started).contains("background build finished"));
    let mut memo = Finished::default();
    assert_eq!(
        memo.of_snapshot_at(&home, &snap, &[], || Some(started.clone())),
        Some(false)
    );
    assert_eq!(
        memo.of_snapshot_at(&home, &snap, &[], || Some(wt.clone())),
        Some(true)
    );
    let mut more = std::fs::OpenOptions::new()
        .append(true)
        .open(&wt_file)
        .expect("append");
    writeln!(more, "{}", user_row("Now add the flag.")).expect("row");
    drop(more);
    assert_eq!(
        memo.of_snapshot_at(&home, &snap, &[], || Some(wt.clone())),
        Some(false)
    );
    assert_eq!(Tasked::default().of_snapshot(&home, &snap, &[]), Some(true));
    let absolute = Snapshot {
        home: Some(filed.to_string_lossy().into_owned()),
        ..snap.clone()
    };
    assert_eq!(
        Finished::default().of_snapshot_at(&home, &absolute, &[], || Some(wt.clone())),
        Some(false)
    );
    std::fs::remove_file(&wt_file).expect("removed");
    assert_eq!(read(&wt), Some(false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`a_moved_agents_finish_is_read_where_it_writes_now`] over a live process.
#[cfg(unix)]
#[test]
fn a_live_moved_agents_finish_is_read_where_it_writes_now() {
    const RELATIVE: &str = "work home/.claude";
    let dir = scratch("moved-finish-live");
    let work = dir.join("work");
    std::fs::create_dir_all(work.join("wt")).expect("its directories");
    let started = std::fs::canonicalize(&work).expect("where it started");
    let runs_in = started.join("wt");
    let filed = started.join(RELATIVE);
    let mut agent = tty_agent(
        &[
            (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
            ("PWD", started.as_path()),
        ],
        Some(&runs_in),
    );
    let pid = agent.id();
    let start = kernel_start(pid).expect("lstart");
    register_running_in(&filed, pid, &start, &started);
    file_transcript_in(&filed, &crate::harness::footer::project_slug(&started));
    let now_dir = runs_in
        .join(RELATIVE)
        .join("projects")
        .join(crate::harness::footer::project_slug(&runs_in));
    std::fs::create_dir_all(&now_dir).expect("projects");
    std::fs::write(
        now_dir.join(format!("{SESSION}.jsonl")),
        land_row()
            + "
" + &turn_by("claude-opus-5-5", LANDED_JSON)
            + "
",
    )
    .expect("transcript");
    let snap = Snapshot {
        pid,
        start: start.clone(),
        home: Some(RELATIVE.to_string()),
        claude_dir: Some(filed.clone()),
        cwd: started.to_string_lossy().into_owned(),
        ..users_agent(SESSION)
    };
    let home = dir.join("home");
    let moved = Finished::default().of_snapshot(&home, &snap, &[]);
    let reused = Finished::default().of_snapshot(
        &home,
        &Snapshot {
            start: "Thu Jan  1 00:00:00 2026".to_string(),
            ..snap.clone()
        },
        &[],
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(moved, Some(true), "its live directory's");
    assert_eq!(reused, Some(false), "CONTROL: a reused pid's is no one's");
}

/// A tab the CLI's sweep only learned homes from is not its to wait on: an unreadable leader there
/// is no `wait:no-process` row.
#[cfg(unix)]
#[test]
fn the_clis_sweep_waits_on_no_tab_it_only_learned_homes_from() {
    let dir = scratch("cli-sweep-unread-leader");
    let leader = std::process::id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={leader}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        dry_run: true,
        ..drive(&dir)
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(leader)),
    }];
    let window = Homes::read(&opts, Some(&tabs), |_| None, |_| None).unnamed(TAB);
    let cli = Homes::read(&opts, None, |_| None, |_| None).unnamed(TAB);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(window, "CONTROL: the window's own tab waits on its leader");
    assert!(
        !cli,
        "the CLI's sweep reports no wait for a tab it only learned homes from"
    );
}

/// An agent under another Claude home is judged by a list grown from that home's announcements
/// ([`ModelCtx::at`]).
#[test]
fn another_homes_announcement_grows_only_its_own_agents_list() {
    let dir = scratch("model-home-grows-alone");
    let opts = Opts {
        dry_run: false,
        ..drive(&dir)
    };
    let bin = dir.join("build/claude");
    std::fs::create_dir_all(bin.parent().expect("dir")).expect("the build");
    std::fs::write(
        &bin,
        concat!(
            r#"VERSION:"2.1.300",models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5"},"#,
            r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5"},"#,
            r#"{id:"claude-opus-5-6",family:"opus",display_name:"Opus 5.6"},"#,
            r#"{id:"claude-fable-5-1",family:"fable",display_name:"Fable 5.1"}],"#,
            r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5"},alias_migration:{}};"#,
        ),
    )
    .expect("the build's catalog");
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(twin.parent().expect("dir")).expect("the twin");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.display()),
    )
    .expect("the twin");
    let targets = Targets {
        managed: Some(Candidate {
            exe: twin,
            version: Version::parse("2.1.300").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let answered = |claude_dir: &Path| {
        let proj = claude_dir.join("projects/-work");
        std::fs::create_dir_all(&proj).expect("projects");
        let at = i64::try_from(now_s()).expect("now") - 7_200;
        std::fs::write(
            proj.join(format!("{SESSION}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n",
                crate::harness::usage::rfc3339_utc(at)
            ),
        )
        .expect("transcript");
    };
    let announced = r#"{"cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-6-launch","text":"t","requiresModel":"claude-opus-5-6"}]}}"#;
    let other = dir.join("work/.claude");
    std::fs::create_dir_all(&other).expect("its home");
    std::fs::write(other.join(".claude.json"), announced).expect("its profile");
    answered(&other);
    let users = opts.home.join(".claude");
    answered(&users);
    let verdict = |sf: &SessionFile, mctx: &ModelCtx| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, sf, None, mctx, false, None, None).verdict
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let path = state_dir(&opts).join("models.json");
    let list = std::fs::read_to_string(&path).expect("the list");
    let changed = ledger_details(&opts, "models-changed");
    let theirs = SessionFile {
        claude_dir: Some(other.clone()),
        ..record("2.1.300")
    };
    let second = SessionFile {
        pid: 102,
        ..theirs.clone()
    };
    let moved = ModelVerdict::Due {
        from: "claude-opus-5-5".to_string(),
        to: "claude-opus-5-6".to_string(),
    };
    let judged = [verdict(&theirs, &mctx), verdict(&second, &mctx)];
    let (list_after, changed_after) = (
        std::fs::read_to_string(&path).expect("the list"),
        ledger_details(&opts, "models-changed"),
    );
    let own = SessionFile {
        claude_dir: Some(users.clone()),
        ..record("2.1.300")
    };
    let next = verdict(&own, &ModelCtx::read(&opts, &targets));
    std::fs::write(opts.home.join(".claude.json"), announced).expect("the user's profile");
    let announced_to_user = verdict(&own, &ModelCtx::read(&opts, &targets));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        judged,
        [moved.clone(), moved.clone()],
        "each moved to the model its home announces"
    );
    assert_eq!(announced_to_user, moved, "CONTROL: the user's own agent");
    assert_eq!(
        (list_after, changed_after),
        (list, changed),
        "the shared list and its journal as the pass's own read left them"
    );
    assert!(
        !matches!(next, ModelVerdict::Due { .. }),
        "CONTROL: the user's agent, announced nothing, is not moved: {next:?}"
    );
}

/// The pass's own growth of the list is only ordered (aterm-durable's counting seam).
#[test]
fn the_passs_growth_of_the_model_list_is_only_ordered() {
    use aterm_durable::{Flush, seam::Recording};
    let dir = scratch("model-growth-flush");
    let opts = Opts {
        dry_run: false,
        ..drive(&dir)
    };
    let bin = dir.join("build/claude");
    std::fs::create_dir_all(bin.parent().expect("dir")).expect("the build");
    std::fs::write(
        &bin,
        concat!(
            r#"VERSION:"2.1.300",models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5"},"#,
            r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5"},"#,
            r#"{id:"claude-opus-5-6",family:"opus",display_name:"Opus 5.6"},"#,
            r#"{id:"claude-fable-5-1",family:"fable",display_name:"Fable 5.1"}],"#,
            r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5"},alias_migration:{}};"#,
        ),
    )
    .expect("the build's catalog");
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(twin.parent().expect("dir")).expect("the twin");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.display()),
    )
    .expect("the twin");
    let targets = Targets {
        managed: Some(Candidate {
            exe: twin,
            version: Version::parse("2.1.300").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let users = opts.home.join(".claude");
    let proj = users.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    std::fs::write(
        opts.home.join(".claude.json"),
        r#"{"cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-6-launch","text":"t","requiresModel":"claude-opus-5-6"}]}}"#,
    )
    .expect("the user's profile");
    let path = state_dir(&opts).join("models.json");
    models::load_or_seed(&path, at).expect("the seeded list");
    let _ = catalog::baked_cached(&bin, &state_dir(&opts).join("builds"));
    let seeded = std::fs::read_to_string(&path).expect("the list");

    let recording = Recording::start();
    let _ = ModelCtx::read(&opts, &targets);
    let grew = recording.take();
    let grown = std::fs::read_to_string(&path).expect("the list");
    let changed = ledger_details(&opts, "models-changed");
    let _ = ModelCtx::read(&opts, &targets);
    let again = recording.take();
    drop(recording);
    let _ = std::fs::remove_dir_all(&dir);
    assert_ne!(grown, seeded, "PRECONDITION: the user's home grew the list");
    assert!(!changed.is_empty(), "and each change is journaled");
    assert_eq!(grew, [Flush::Ordered], "the growth's write is only ordered");
    assert_eq!(again, [], "CONTROL: nothing new to grow, nothing written");
}

/// Under a relative home, a Claude Code that changed directory leaves its record (status frozen)
/// where it filed it.
#[cfg(unix)]
#[test]
fn an_agent_whose_records_a_worktree_split_waits_and_is_asked_nothing() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("wt")).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let runs_in = if moved {
            started.join("wt")
        } else {
            started.clone()
        };
        let claude_dir = started.join(RELATIVE);
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(&runs_in),
        );
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        assert!(opts.ask, "the ask flow");
        let sf = register_as(&claude_dir, pid, SESSION, "1.0.0");
        file_transcript_in(&claude_dir, "-work");
        let shell = dead_pid();
        let r = visit(
            &opts,
            &sf,
            &[(shell, 1, "zsh".into())],
            &newer(),
            &Script::new(shell, usize::MAX, None),
        );
        let typed: Vec<String> = asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default();
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r.step, typed)
    };
    let (moved, typed) = case("split-records-moved", true);
    let (unmoved, _) = case("split-records-unmoved", false);
    assert_eq!(
        (moved.as_str(), typed),
        ("wait:home-moved", Vec::<String>::new()),
        "its look waits by name, typing nothing"
    );
    assert_ne!(unmoved, "wait:home-moved", "CONTROL: never moved");
}

/// With no roster of its own, the CLI's sweep learns Claude homes from the `who` roster of every
/// instance [`Opts::instances`] names.
#[cfg(unix)]
#[test]
fn the_clis_sweep_reads_the_tabs_of_every_instance_it_is_given() {
    const OTHER_TAB: &str = "s-0123456789abcdef0123";
    let dir = scratch("cli-sweep-two-instances");
    let mut socks = Vec::new();
    let mut instances = Vec::new();
    for (name, tab, leader) in [("a", TAB, 4_242_u32), ("b", OTHER_TAB, 4_343)] {
        let at = dir.join(name);
        std::fs::create_dir_all(&at).expect("the instance's directory");
        let who = format!("OK 1\n0 {tab} driving=- watchers=0 turns=0 alive fgpgid={leader}");
        let (sock, asked) = instance_for(
            &at,
            tab,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        socks.push(sock);
        instances.push(asked);
        let home = dir.join(format!("{name}/.claude"));
        std::fs::create_dir_all(home.join("sessions")).expect("sessions");
        std::fs::write(
            home.join(format!("sessions/{leader}.json")),
            format!(
                r#"{{"pid":{leader},"sessionId":"{SESSION}","cwd":"/","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the record");
    }
    let args = |pid: u32| {
        let name = if pid == 4_242 { "a" } else { "b" };
        Some(atpkg::caller_shell::ProcArgs {
            env: vec![format!(
                "CLAUDE_CONFIG_DIR={}",
                dir.join(format!("{name}/.claude")).display()
            )],
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    let read = |instances: Vec<String>| {
        let opts = Opts {
            sock: None,
            dry_run: true,
            instances: Instances::Listed(instances),
            ..drive(&dir)
        };
        let mut pids: Vec<u32> = Homes::read(&opts, None, args, |_| None)
            .roster(Some(Vec::new()))
            .expect("the records")
            .iter()
            .map(|sf| sf.pid)
            .collect();
        pids.sort_unstable();
        pids
    };
    let both = read(socks.clone());
    let one = read(socks[..1].to_vec());
    let none = read(Vec::new());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        both,
        [4_242, 4_343],
        "every instance's tabs teach their homes"
    );
    assert_eq!(one, [4_242], "CONTROL: the instance not given is not read");
    assert_eq!(none, Vec::<u32>::new(), "CONTROL: none given, none read");
}

/// Under autopilot land a relaunch's carry-on types nothing of cruise's.
#[cfg(unix)]
#[test]
fn a_relaunch_under_land_types_no_carry_on_and_leaves_its_point_to_the_loop() {
    use super::super::relaunch::{CAUSE_MEMORY, CAUSE_STALL};
    let working = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "Committing the parser now; the lexer is next.",
        ),
    ];
    let done = vec![
        user_row("Fix the lexer and land it."),
        turn_by(
            "claude-opus-5-5",
            "All tasks are complete and verified; nothing is left to do.",
        ),
    ];
    for (what, rows, cruise) in [
        ("work going on", &working, "continued"),
        ("a done report", &done, "continued:check"),
    ] {
        for cause in [CAUSE_HOST, CAUSE_EXIT, CAUSE_MEMORY, CAUSE_STALL, ""] {
            for (land, by_status) in [(true, false), (false, true), (false, false)] {
                let rig = Rig::new(
                    &format!("carry-land-{land}-{by_status}-{}-{cause}", what.len()),
                    rows,
                );
                if by_status {
                    std::fs::write(
                        rig.dir.join("t.status"),
                        "OK schema=1 hold=0 hand=- supervisor=- autopilot=land\n",
                    )
                    .expect("status");
                }
                let opts = Opts {
                    land,
                    ..rig.opts.clone()
                };
                let mut st = rig.st.clone();
                st.cause = cause.to_string();
                let mut c = connect(&opts, TAB).expect("control connection");
                let r = carry_on_with_tab_probe(
                    &opts,
                    blank(&rig.sf),
                    &mut st,
                    &mut c,
                    SESSION,
                    &rig.sf,
                    MODEL_WAIT,
                    |_, _, _| true,
                    None,
                );
                let typed = rig.typed();
                let case = format!(
                    "{what}, cause `{cause}`, land {land}, by status {by_status}: {r:?} {typed:?}"
                );
                if land || by_status {
                    assert_eq!(r.step, "done:landing", "{case}");
                    assert!(typed.is_empty(), "{case}");
                    assert_eq!(st.phase, Phase::Done, "{case}");
                    assert!(!moved(&r.step), "the point stands: {case}");
                    assert_eq!(host_turn(&r.step), None, "{case}");
                    assert!(st.confirming(), "the look stays open: {case}");
                    assert_eq!(
                        load(&opts, SESSION).map(|saved| saved.phase),
                        Some(Phase::Done),
                        "saved: {case}"
                    );
                } else {
                    assert_eq!(r.step, cruise, "{case}");
                    assert_eq!(typed.len(), 1, "{case}");
                }
            }
        }
    }
}

/// The explicit restart offers exactly what its relaunch carries.
#[cfg(unix)]
#[test]
fn the_restart_offers_no_relaunch_its_exit_refuses_under_another_home() {
    use super::super::relaunch::{carries_restart, resumes_on_exit};
    use super::super::restart_stamp::Cause;
    let mut a = DeadAgent::new("lc3-other-home");
    let other = a.dir.join("other/records");
    a.write_record_in(&other, SESSION);
    file_transcript_in(&other, "p");
    a.snap.claude_dir = Some(other);
    for asked in [None, Some(Cause::Asked), Some(Cause::Stall)] {
        assert_eq!(
            after_exit(&a.opts, &a.snap, &ExitRecord::Unread, false, asked).step,
            "would-refuse:home",
            "{asked:?}: its relaunch refuses it"
        );
    }
    assert!(
        !carries_restart(&a.opts.home, &a.snap),
        "the act is offered only where its relaunch would carry it"
    );
    assert!(
        !resumes_on_exit(&a.opts.home, &a.snap),
        "no relaunch promised either"
    );
    let b = DeadAgent::new("lc3-own-home");
    b.write_record();
    assert_eq!(
        after_exit(
            &b.opts,
            &b.snap,
            &ExitRecord::Unread,
            false,
            Some(Cause::Asked)
        )
        .step,
        "would-relaunch"
    );
    assert!(carries_restart(&b.opts.home, &b.snap) && resumes_on_exit(&b.opts.home, &b.snap));
    let mut c = DeadAgent::new("lc3-other-claude-home");
    let carried = c.dir.join("other/.claude");
    c.write_record_in(&carried, SESSION);
    file_transcript_in(&carried, "p");
    c.snap.claude_dir = Some(carried);
    assert_eq!(
        after_exit(
            &c.opts,
            &c.snap,
            &ExitRecord::Unread,
            false,
            Some(Cause::Asked)
        )
        .step,
        "would-relaunch"
    );
    assert!(carries_restart(&c.opts.home, &c.snap) && resumes_on_exit(&c.opts.home, &c.snap));
}

/// A crash is never moved onto a newer build not proved to start.
#[cfg(unix)]
#[test]
fn a_crash_is_never_moved_onto_a_newer_build_not_proved_to_start() {
    let case = |name: &str, help_exits: u8| -> (String, usize) {
        let dir = std::fs::canonicalize(scratch(name)).expect("scratch");
        let opts = Opts {
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        build_start::PROBE.with(|named| *named.borrow_mut() = None);
        seed_builds(&opts);
        let versions = native_root(&opts.home);
        std::fs::create_dir_all(&versions).expect("versions");
        std::fs::write(versions.join("2.1.281"), "").expect("the build it ran");
        sh_script(
            &versions.join("99.9.9"),
            &format!(
                "case \"$1\" in --version) echo \"99.9.9 (Claude Code)\"; exit 0 ;; --help) exit \
                 {help_exits} ;; esac\nexit 1"
            ),
        );
        let warmed = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(versions.join("99.9.9")).arg("--version"),
            std::time::Instant::now() + aterm_spec::wait::FIRST_EXEC,
        )
        .unwrap_or_else(|e| panic!("the stand-in's first exec within its hang detector: {e}"));
        assert!(warmed.status.success(), "{warmed:?}");
        std::fs::create_dir_all(opts.home.join(".local/bin")).expect("bin");
        std::os::unix::fs::symlink(versions.join("99.9.9"), opts.home.join(".local/bin/claude"))
            .expect("the native link");
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("sessions");
        file_transcript_in(&opts.home.join(".claude"), "p");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let mut shell = stand_in_shell();
        let dead = dead_pid();
        let start = "Thu Sep 24 01:02:03 2026";
        std::fs::write(
            opts.home.join(format!(".claude/sessions/{dead}.json")),
            format!(
                r#"{{"pid":{dead},"sessionId":"{SESSION}","cwd":"/","version":"2.1.281","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the crash's record");
        let program = versions.join("2.1.281");
        let snap = Snapshot {
            tab: TAB.to_string(),
            pid: dead,
            start: start.to_string(),
            shell: shell.id(),
            program: program.clone(),
            argv: vec![program.to_string_lossy().into_owned()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("2.1.281".to_string()),
            run: crate::harness::relaunch::AgentRun::Claude,
            dialect: Some(Dialect::Zsh),
            login: None,
            stored_login: false,
            profile: None,
            home: None,
            claude_dir: None,
        };
        let r = after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
        let probed = ledger_details(&opts, "build-probe:exited").len();
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r.to, probed)
    };
    let (to, probed) = case("lc5-crashing-build", 1);
    assert_eq!(
        to, "2.1.281(same)",
        "a build that does not start is no relaunch target"
    );
    assert_eq!(probed, 1, "its probe said so on the ledger");
    let (to, probed) = case("lc5-starting-build", 0);
    assert_eq!(to, "99.9.9(native)", "CONTROL: proved to start, moved onto");
    assert_eq!(probed, 0);
}

/// The gate of a crash's relaunch: a build with no failure noted is probed before a crashed agent
/// is moved onto it.
#[cfg(unix)]
#[test]
fn a_crashs_relaunch_is_moved_only_onto_a_build_its_probe_proved() {
    use build_start::{BUILD_FAILS, PROBE_GAP_S, Probe, starts_with};
    let dir = scratch("build-proved");
    let opts = drive(&dir);
    let to = |version: &str| Candidate {
        exe: dir.join("claude"),
        version: Version::parse(version).expect("v"),
        source: Source::Managed,
    };
    let r = Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: String::new(),
    };
    let never = |_: &Path| -> Probe { panic!("no probe is run") };
    let now = now_s();
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        starts_with(&dry, &r, &to("9.9.9"), now, false, false, never),
        Ok(())
    );
    assert_eq!(
        starts_with(&dry, &r, &to("9.9.9"), now, false, true, never),
        Err(BUILD_FAILS)
    );
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.9"), now, false, true, |_| Probe::Exited),
        Err(BUILD_FAILS)
    );
    assert_eq!(ledger_details(&opts, "build-probe:exited").len(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.9"), now + 60, false, true, never),
        Err(BUILD_FAILS)
    );
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.9"), now + 60, false, false, never),
        Err(BUILD_FAILS),
        "the upgrade is held off it too"
    );
    assert_eq!(
        starts_with(
            &opts,
            &r,
            &to("9.9.9"),
            now + PROBE_GAP_S,
            false,
            true,
            |_| { Probe::Starts }
        ),
        Ok(())
    );
    for require in [true, false] {
        assert_eq!(
            starts_with(
                &opts,
                &r,
                &to("9.9.9"),
                now + PROBE_GAP_S + 1,
                false,
                require,
                never
            ),
            Ok(()),
            "{require}"
        );
    }
    let failed = St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        relaunched_at: now + PROBE_GAP_S + 5,
        failed_at: now + PROBE_GAP_S + 9,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    save_at(&opts, SESSION, &failed, now + PROBE_GAP_S + 9);
    let note = build_start::load(&opts, "managed", "9.9.9").expect("the note");
    assert!(!note.blind && note.failures == 1, "{note:?}");
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.8"), now, false, true, |_| Probe::Starts),
        Ok(())
    );
    assert_eq!(ledger_details(&opts, "build-probe:starts").len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A crash's probe holds the upgrade's own moves only by what it found failing.
#[test]
fn a_crashs_probe_holds_the_upgrade_only_by_what_it_found_failing() {
    use build_start::{BUILD_FAILS, Probe, starts_with};
    let dir = scratch("build-crash-probe");
    let opts = drive(&dir);
    let to = |version: &str| Candidate {
        exe: dir.join("claude"),
        version: Version::parse(version).expect("v"),
        source: Source::Managed,
    };
    let r = Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: String::new(),
    };
    let never = |_: &Path| -> Probe { panic!("no probe is run") };
    let now = now_s();
    let failed_move = |relaunched_at: u64, failed_at: u64| St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        relaunched_at,
        failed_at,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.9"), now, false, true, |_| Probe::Starts),
        Ok(())
    );
    save_at(&opts, SESSION, &failed_move(now + 5, now + 9), now + 9);
    let note = build_start::load(&opts, "managed", "9.9.9").expect("the note");
    assert!(!note.blind && note.failures == 1, "{note:?}");
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.9"), now + 10, false, false, |_| {
            Probe::Starts
        }),
        Ok(())
    );
    save_at(&opts, SESSION, &failed_move(now + 11, now + 12), now + 12);
    let note = build_start::load(&opts, "managed", "9.9.9").expect("the note");
    assert!(note.blind && note.failures == 2, "{note:?}");
    for (version, found) in [("9.9.7", Probe::TimedOut), ("9.9.6", Probe::NoExec)] {
        assert_eq!(
            starts_with(&opts, &r, &to(version), now, false, true, |_| found),
            Err(BUILD_FAILS),
            "{found:?}"
        );
        assert_eq!(
            starts_with(&opts, &r, &to(version), now + 60, false, true, never),
            Err(BUILD_FAILS),
            "{found:?}: the crash's relaunch waits the gap"
        );
        assert_eq!(
            starts_with(&opts, &r, &to(version), now + 60, false, false, never),
            Ok(()),
            "{found:?}: the upgrade's move is not held"
        );
        assert_eq!(
            build_start::load(&opts, "managed", version).map(|n| n.failures),
            Some(0),
            "{found:?}: no failure noted"
        );
    }
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.5"), now, false, true, |_| Probe::Exited),
        Err(BUILD_FAILS)
    );
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.5"), now + 60, false, false, never),
        Err(BUILD_FAILS)
    );
    let failed_before = St {
        to: "9.9.4".to_string(),
        ..failed_move(now - 40, now - 34)
    };
    save_at(
        &opts,
        "0badf00d-4444-2222-3333-444455556666",
        &failed_before,
        now - 34,
    );
    let failures = || build_start::load(&opts, "managed", "9.9.4").map(|n| n.failures);
    assert_eq!(failures(), Some(1));
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.4"), now, false, true, |_| {
            Probe::TimedOut
        }),
        Err(BUILD_FAILS)
    );
    assert_eq!(failures(), Some(1), "no failure added");
    assert_eq!(
        starts_with(&opts, &r, &to("9.9.4"), now + 60, false, false, never),
        Err(BUILD_FAILS),
        "the upgrade's move waits on the failure noted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The managed target is never a dev link's.
#[test]
fn a_dev_links_twin_is_no_managed_build() {
    let dir = scratch("managed-twin");
    let layout = atpkg::store::Layout {
        prefix: dir.to_path_buf(),
    };
    std::fs::create_dir_all(dir.join("agents")).expect("agents");
    let twin = dir.join("agents/claude");
    std::fs::write(&twin, "#!/bin/sh\necho stand-in\n").expect("stand-in");
    assert_eq!(
        managed_twin(&layout, "claude"),
        Some(twin.clone()),
        "CONTROL: not linked, the twin is managed"
    );
    let checkout = dir.join("claude-code");
    std::fs::create_dir_all(checkout.join("bin")).expect("checkout");
    std::fs::write(checkout.join("bin/claude"), "#!/bin/sh\n").expect("dev build");
    atpkg::linkmode::link(&layout, "claude", &checkout, &[PathBuf::from("bin/claude")])
        .expect("dev link");
    assert!(atpkg::linkmode::is_linked(&layout, "claude"));
    std::fs::write(
        &twin,
        format!(
            "#!/bin/sh\nexec '{}' \"$@\"\n",
            checkout.join("bin/claude").display()
        ),
    )
    .expect("dev twin");
    assert_eq!(managed_twin(&layout, "claude"), None, "a dev link's");
    let store = dir.join("store/claude/7/bin/claude");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", store.display()),
    )
    .expect("twin");
    assert_eq!(
        managed_twin(&layout, "claude"),
        Some(twin),
        "CONTROL: linked, but the twin names a store build"
    );
}

/// A single restored tab is one provenance identity.
#[test]
fn earlier_tab_notes_cannot_split_one_restored_identity_into_other_tabs() {
    let root = Path::new("/this-ram-test-does-not-open-files");
    let tab = "s-cafe";
    let decode =
        |body: &str| super::noted_earlier_tabs_read(root, tab, &|_| Some(body.to_string()));
    let first = super::earlier_tab_note(tab, "s-aaaa", Vec::new()).expect("one earlier tab");
    assert_eq!(decode(&first), ["s-aaaa"]);
    assert!(super::earlier_tab_note(tab, "s-aaaa", decode(&first)).is_none());
    let next = super::earlier_tab_note(tab, "s-bbbb", decode(&first)).expect("another earlier tab");
    assert_eq!(decode(&next), ["s-aaaa", "s-bbbb"]);
    assert!(super::earlier_tab_note(tab, tab, decode(&next)).is_none());

    let mut introduced = Vec::new();
    for malformed in ["s-aaaa\ns-bbbb", "s-aaaa\r\ns-bbbb", " s-aaaa"] {
        let tabs = super::earlier_tab_note(tab, malformed, Vec::new())
            .map(|body| decode(&body))
            .unwrap_or_default();
        if !tabs.is_empty() {
            introduced.push((malformed, tabs));
        }
    }
    assert!(
        introduced.is_empty(),
        "malformed restored identities introduced ledger provenance: {introduced:?}"
    );
}

/// A note of where a moved agent wrote is kept while any transcript evidence is unreadable.
#[test]
fn a_writes_note_is_kept_when_any_transcript_evidence_is_unreadable() {
    use std::io::{Error, ErrorKind, Result};

    fn note() -> Result<String> {
        Ok("/moved/.claude\n".to_string())
    }
    fn listed(entries: &[Option<&str>]) -> Result<Vec<Result<PathBuf>>> {
        Ok(entries
            .iter()
            .map(|entry| match entry {
                Some(path) => Ok(PathBuf::from(*path)),
                None => Err(Error::from(ErrorKind::PermissionDenied)),
            })
            .collect())
    }
    fn decide(
        text: Result<String>,
        listing: Result<Vec<Result<PathBuf>>>,
        facts: Vec<Result<bool>>,
    ) -> (bool, Vec<PathBuf>) {
        let mut facts = facts.into_iter();
        let mut probed = Vec::new();
        let prunable = note_names_nothing_with(
            text,
            "conversation",
            |path| {
                assert_eq!(path, Path::new("/moved/.claude/projects"));
                listing
            },
            |path| {
                probed.push(path.to_path_buf());
                facts
                    .next()
                    .expect("a fact for every actual metadata probe")
            },
        );
        (prunable, probed)
    }
    let a = "/moved/.claude/projects/a";
    let b = "/moved/.claude/projects/b";
    let transcript = |project: &str| Path::new(project).join("conversation.jsonl");

    assert!(decide(Ok("\n".to_string()), listed(&[]), vec![]).0, "blank");
    assert!(
        decide(note(), Err(Error::from(ErrorKind::NotFound)), vec![]).0,
        "the projects directory is absent"
    );
    assert!(decide(note(), listed(&[]), vec![]).0, "empty projects");
    let absent = decide(
        note(),
        listed(&[Some(a), Some(b)]),
        vec![Ok(false), Ok(false)],
    );
    assert_eq!(
        absent,
        (true, vec![transcript(a), transcript(b)]),
        "all absent"
    );
    let present = decide(note(), listed(&[Some(a)]), vec![Ok(true)]);
    assert_eq!(
        present,
        (false, vec![transcript(a)]),
        "a transcript is present"
    );
    assert!(
        !decide(
            Err(Error::from(ErrorKind::PermissionDenied)),
            listed(&[]),
            vec![]
        )
        .0,
        "an unreadable note is kept"
    );
    assert!(
        !decide(
            note(),
            Err(Error::from(ErrorKind::PermissionDenied)),
            vec![]
        )
        .0,
        "an unlisted projects directory is kept"
    );
    assert!(
        !decide(note(), listed(&[Some(a), None]), vec![Ok(true)]).0,
        "a present transcript alone already preserves the note"
    );

    let unknown = [
        ("entry", decide(note(), listed(&[None]), vec![])),
        (
            "later entry",
            decide(note(), listed(&[Some(a), None]), vec![Ok(false)]),
        ),
        (
            "metadata",
            decide(
                note(),
                listed(&[Some(a)]),
                vec![Err(Error::from(ErrorKind::PermissionDenied))],
            ),
        ),
        (
            "later metadata",
            decide(
                note(),
                listed(&[Some(a), Some(b)]),
                vec![Ok(false), Err(Error::from(ErrorKind::Other))],
            ),
        ),
    ];
    assert!(
        unknown.iter().all(|(_, (prunable, _))| !prunable),
        "WRITES_NOTE_REQUIRES_COMPLETE_TRANSCRIPT_EVIDENCE: {unknown:?}"
    );
}

/// A sweep over several live conversations reads the session roster once, not once per candidate.
#[test]
fn multiple_candidates_share_one_preliminary_session_scan() {
    let dir = scratch("one-precheck-scan");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&opts);
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

/// [`pick`] answers the window's step with the tab's own word, never another tab's act.
#[test]
fn the_windows_step_answers_with_the_tabs_own_word_before_another_tabs() {
    let report = |tab: &str, step: &str| Report {
        pid: 1,
        tab: tab.to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: step.to_string(),
    };
    let tab = "s-c0dec0dec0dec0dec0de";
    let other = report("s-0ther0ther0ther0the", "relaunched");
    let words = |reports: &[Report]| pick(reports, tab).map(|r| r.step.clone());
    assert_eq!(
        words(&[other.clone(), report(tab, "wait:not-idle")]),
        Some("wait:not-idle".to_string()),
        "the tab's wait, not another tab's act"
    );
    assert_eq!(
        after("wait:not-idle", 0),
        After::Later(LATER[0]),
        "and the tab is looked at again"
    );
    assert_eq!(
        words(&[
            other.clone(),
            report(tab, "terminated"),
            report(tab, "done")
        ]),
        Some("terminated".to_string()),
        "the tab's own act first"
    );
    assert_eq!(
        words(&[other.clone(), report(tab, "current")]),
        Some("relaunched".to_string()),
        "a current tab leaves the other's word"
    );
    assert_eq!(
        words(&[report("-", "wait:session-files-unreadable"), other.clone()]),
        Some("relaunched".to_string())
    );
    assert_eq!(
        words(&[report("-", "wait:session-files-unreadable")]),
        Some("wait:session-files-unreadable".to_string())
    );
    assert_eq!(words(&[report(tab, "current")]), None);
}

/// The ladder's conformance bind on Claude Code's lane (`HarnessUpgradeLadder` against the real
/// `look_facts` and `upgrade::next_step`).
#[path = "upgrade_ladder_claude_tests.rs"]
mod ladder_claude;
#[cfg(unix)]
#[test]
fn a_bounded_reader_requires_complete_successful_utf8_evidence() {
    let read = |script: &str, cap| {
        bounded_output(
            Path::new("/bin/sh"),
            &["-c".as_ref(), script.as_ref()],
            Duration::from_secs(60),
            cap,
        )
    };
    assert_eq!(read("printf 'idle'", 4).as_deref(), Some("idle"));
    assert_eq!(read("printf 'idle\\nbusy\\n'", 5), None);
    assert_eq!(read("printf 'idle'; exit 7", 64), None);
    assert_eq!(read("printf '\\377'", 64), None);
    assert_eq!(read("printf ''", 0).as_deref(), Some(""));
    assert_eq!(read("printf 'x'", 0), None);
}

/// The token [`token_server`] closes a connection on after reading its first line, writing
/// nothing: the connection a refusal closed before its `ERR auth` could be read.
#[cfg(unix)]
const CLOSED_TOKEN: &str = "closed-tok";

/// A control server at `<dir>/a.sock` with `beside-tok` beside it, serving one connection after
/// another until one presents `stop` ([`token_server_seen`]). A connection whose `AUTH` names
/// `refused` is answered `ERR auth` and closed at its first line, as a successor that rebound the
/// socket answers the window's old token; one naming [`CLOSED_TOKEN`] is closed at its first line
/// with nothing written; any other line is answered `ERR busy` when it asks for the hand and `OK`
/// otherwise. Its receiver gets what it was sent, `(token, line)` in order.
#[cfg(unix)]
fn token_server(
    dir: &Path,
    refused: &'static str,
) -> (String, std::sync::mpsc::Receiver<Vec<(String, String)>>) {
    use std::io::BufRead as _;
    let names = aterm_types::control_socket::token_names_for_sock("a.sock");
    std::fs::write(dir.join(&names[0]), "beside-tok\n").expect("the token beside the socket");
    let sock = dir.join("a.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            let mut w = stream.try_clone().expect("clone");
            let mut r = std::io::BufReader::new(stream);
            let mut auth = String::new();
            let _ = r.read_line(&mut auth);
            let token = auth.trim_end().trim_start_matches("AUTH ").to_string();
            if token == "stop" {
                break;
            }
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end().to_string();
                seen.push((token.clone(), line.clone()));
                if token == CLOSED_TOKEN {
                    break;
                }
                if token == refused {
                    let _ = w.write_all(b"ERR auth\n");
                    break;
                }
                let reply: &[u8] = if line.contains(" lease acquire ") {
                    b"ERR busy\n"
                } else {
                    b"OK\n"
                };
                let _ = w.write_all(reply);
            }
        }
        let _ = tx.send(seen);
    });
    (sock.to_string_lossy().into_owned(), rx)
}

/// What [`token_server`] at `sock` was sent, once a last connection tells it to stop.
#[cfg(unix)]
fn token_server_seen(
    sock: &str,
    rx: &std::sync::mpsc::Receiver<Vec<(String, String)>>,
) -> Vec<(String, String)> {
    let mut stop = std::os::unix::net::UnixStream::connect(sock).expect("the server");
    stop.write_all(b"AUTH stop\n").expect("stop");
    drop(stop);
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the server read every connection")
}

/// The window's drive presents the harness's own token on the window's socket, probed once with
/// `version`, so what it types reads `by=harness`; a refused token (`ERR auth`, or the connection
/// closed with nothing written) is replaced once by the one beside the socket, and the request
/// after it goes once, on that connection. With no token, or on any other socket, the token beside
/// the socket goes unprobed.
#[cfg(unix)]
#[test]
fn the_window_drive_presents_the_harness_token_and_falls_back_beside_once() {
    let dir = scratch("harness-token");
    let (sock, server) = token_server(&dir, "stale-tok");
    let pair = |t: &str, l: &str| (t.to_string(), l.to_string());
    let memory = HarnessToken::new("memory-tok");
    let mut c = harness_client(&sock, Some(&memory)).expect("the harness's connection");
    c.request_line("@s-1 send x").expect("sent");
    drop(c);
    let stale = HarnessToken::new("stale-tok");
    let mut c = harness_client(&sock, Some(&stale)).expect("the fallback's connection");
    c.request_line("@s-1 send y").expect("sent");
    drop(c);
    let closed = HarnessToken::new(CLOSED_TOKEN);
    let mut c = harness_client(&sock, Some(&closed)).expect("the closed connection's fallback");
    c.request_line("@s-1 send u").expect("sent");
    drop(c);
    let mut c = harness_client(&sock, None).expect("the CLI's connection");
    c.request_line("@s-1 send z").expect("sent");
    drop(c);
    let opts = Opts {
        sock: Some(sock.clone()),
        token: Some(memory.clone()),
        ..drive(&dir)
    };
    let mut c = connect(&opts, "s-1").expect("the sweep's connection");
    c.request_line("@s-1 send v").expect("sent");
    drop(c);
    let elsewhere = Opts {
        sock: Some(dir.join("other.sock").to_string_lossy().into_owned()),
        ..opts.clone()
    };
    let mut c = connect_at(&elsewhere, &sock).expect("another socket's connection");
    c.request_line("@s-1 send w").expect("sent");
    drop(c);
    let typed = super::super::disk::type_resume(&sock, Some(&memory), "s-1", "carry on");
    assert_eq!(typed, "wait:held", "the hand was refused");
    let typed = super::super::disk::type_resume(&sock, None, "s-1", "carry on");
    assert_eq!(typed, "wait:held", "the hand was refused");
    let seen = token_server_seen(&sock, &server);
    let lease = format!("@s-1 {}", hand_take_request());
    assert_eq!(
        seen,
        [
            pair("memory-tok", "version"),
            pair("memory-tok", "@s-1 send x"),
            pair("stale-tok", "version"),
            pair("beside-tok", "@s-1 send y"),
            pair(CLOSED_TOKEN, "version"),
            pair("beside-tok", "@s-1 send u"),
            pair("beside-tok", "@s-1 send z"),
            pair("memory-tok", "version"),
            pair("memory-tok", "@s-1 send v"),
            pair("beside-tok", "@s-1 send w"),
            pair("memory-tok", "version"),
            pair("memory-tok", &lease),
            pair("beside-tok", &lease),
        ]
    );
}

/// The harness's token is never printed with the sweep's options.
#[test]
fn the_harness_token_is_never_in_the_options_debug() {
    let dir = scratch("harness-token-debug");
    let opts = Opts {
        token: Some(HarnessToken::new("QZP6SECRET7")),
        ..drive(&dir)
    };
    let shown = format!("{opts:?}");
    assert!(!shown.contains("QZP6SECRET7"), "{shown}");
    assert!(
        shown.contains("token: Some(HarnessToken(redacted))"),
        "{shown}"
    );
}

/// A session file that cannot be parsed fails the roster only while its writer may live: a
/// process of this user's at its number that started by the file's last write. Root's process
/// at that number (`kill` answers `EPERM`, which [`alive`] counts as alive), an unreaped zombie
/// (`kill` answers 0), or a process that started after the file was last written cannot be
/// this user's Claude Code writing it, and Claude Code's own sweep leaves such a file in place.
/// Negative control: judged by [`alive`], each of those fails the roster; this process at the
/// number, with the file written after it started, fails it.
#[cfg(target_vendor = "apple")]
#[test]
fn an_unparsable_session_file_fails_the_roster_only_while_this_users_process_may_write_it() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let claude_dir = scratch.path().join(".claude");
    let sessions = claude_dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    let half = |pid: u32| {
        std::fs::write(sessions.join(format!("{pid}.json")), b"{\"pid\":").expect("a torn record");
    };
    half(1);
    assert_eq!(
        session_files_in(&claude_dir).map(|f| f.len()),
        Some(0),
        "launchd's number: root's process writes no session of this user's"
    );
    let zombie = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("a child");
    let pid = zombie.id();
    let started = Instant::now();
    while aterm_uds::process::read_short_info(i32::try_from(pid).expect("a pid")).is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the child never exited"
        );
        std::thread::yield_now();
    }
    half(pid);
    let with_zombie = session_files_in(&claude_dir).map(|f| f.len());
    drop(zombie);
    assert_eq!(with_zombie, Some(0), "an unreaped zombie writes nothing");
    std::fs::remove_file(sessions.join(format!("{pid}.json"))).expect("the zombie's record");
    let me = std::process::id();
    half(me);
    assert!(
        session_files_in(&claude_dir).is_none(),
        "this user's live process may be writing its record"
    );
    let born = started_s(me).expect("this process's start");
    std::fs::File::open(sessions.join(format!("{me}.json")))
        .and_then(|f| f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(born - 10)))
        .expect("a write before this process started");
    assert_eq!(
        session_files_in(&claude_dir).map(|f| f.len()),
        Some(0),
        "last written before this process started: a process that took the number since"
    );
}
