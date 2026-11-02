// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::upgrade_drive::{HarnessToken, Instances, Tell};
use super::*;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;

const TAB: &str = "s-disabled-native-caller";
const SESSION: &str = "bbbbbbbb-0000-0000-0000-0000000000ca";

fn options(world: &Path) -> Opts {
    let mut timings = crate::harness::Timings::FAST;
    // This guard must observe the still-live test process, never wait for it to exit.
    // Zero is the negative policy input; there is no latency assertion or vendor process.
    timings.exited = Duration::ZERO;
    Opts {
        home: world.join("home"),
        state: world.join("state"),
        sock: Some(world.join("fake.sock").to_string_lossy().into_owned()),
        token: Some(HarnessToken::new("private-caller-fixture")),
        only_sid: Some(TAB.into()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings,
        tell: Tell::Off,
        press: None,
    }
}

fn endpoint() -> (Client, std::thread::JoinHandle<Vec<String>>) {
    let (near, far) = UnixStream::pair().unwrap();
    for stream in [&near, &far] {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        stream
            .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    let worker = std::thread::spawn(move || {
        let mut writer = far.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(far);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let line = line.trim_end().to_string();
            let reply = if line == format!("@{TAB} native-act constructors") {
                "OK native-act state=constructors plans=-\n"
            } else if line.starts_with(&format!("@{TAB} lease acquire ")) {
                "ERR busy\n"
            } else {
                // A regression can reach only this private fake endpoint, never a real PTY.
                "ERR unexpected fixture action\n"
            };
            seen.push(line);
            writer.write_all(reply.as_bytes()).unwrap();
        }
        seen
    });
    (crate::RelayClient::new(near), worker)
}

fn report() -> Report {
    Report {
        pid: std::process::id(),
        tab: TAB.into(),
        session: SESSION.into(),
        from: "1".into(),
        to: "2".into(),
        step: String::new(),
    }
}

fn state(coordinated: bool) -> St {
    St {
        agent: upgrade::Agent::Claude,
        phase: upgrade::Phase::Exiting { at_s: now_s() },
        pid: std::process::id(),
        shell: std::process::id(),
        tab: TAB.into(),
        line: "fixed legacy fixture".into(),
        native_coordination: coordinated,
        ..St::default()
    }
}

fn assert_only_discovery_and_hand(seen: &[String]) {
    assert!(
        seen.iter()
            .any(|line| line == &format!("@{TAB} native-act constructors"))
    );
    assert!(
        seen.iter()
            .all(|line| line == &format!("@{TAB} native-act constructors")
                || line.starts_with(&format!("@{TAB} lease acquire ")))
    );
}

fn exit_input(world: &Path, st: &St) -> (Snapshot, Plan) {
    let snap = Snapshot {
        stored_login: false,
        profile: None,
        tab: TAB.into(),
        pid: std::process::id(),
        start: "captured start".into(),
        shell: std::process::id(),
        program: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        session: Some(SESSION.into()),
        cwd: world.to_string_lossy().into_owned(),
        version: None,
        run: AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let plan = Plan {
        shell: snap.shell,
        dialect: Dialect::Zsh,
        line: st.line.clone(),
        tap_dropped: None,
        line_dir: None,
        command: None,
    };
    (snap, plan)
}

#[test]
fn disabled_native_kind_relaunch_held_continues_legacy_exit_guard_only_when_fresh() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = options(world.path());
        let (mut client, worker) = endpoint();
        let mut st = state(coordinated);
        let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-constructor-disabled"
            } else {
                "wait:exiting"
            }
        );
        assert_eq!(st.native_coordination, coordinated);
        assert_only_discovery_and_hand(&seen);
    }
}

#[test]
fn disabled_native_kind_exit_launch_tail_preserves_legacy_guard_and_coordinated_refusal() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = options(world.path());
        let (client, worker) = endpoint();
        let st = state(coordinated);
        let (snap, plan) = exit_input(world.path(), &st);
        // This is the production after_exit_as tail, after its record/plan/roster selection.
        // The live PID simulates the mandatory final exit guard refusing a late continuation.
        let result = launch_after_exit(&opts, &snap, ExitCause::Exit, report(), st, plan, client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-constructor-disabled"
            } else {
                "wait:exiting"
            }
        );
        assert_only_discovery_and_hand(&seen);
    }
}

#[test]
fn missing_private_token_relaunch_held_preserves_only_zero_native_legacy_state() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.token = None;
        let (mut client, worker) = endpoint();
        let mut st = state(coordinated);
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-coordinated-unavailable"
            } else {
                "wait:exiting"
            }
        );
        assert_eq!(st.native_coordination, coordinated);
        assert!(
            seen.iter()
                .all(|line| line.starts_with(&format!("@{TAB} lease acquire ")))
        );
    }
}

#[test]
fn missing_private_token_exit_launch_tail_preserves_only_zero_native_legacy_state() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.token = None;
        let (client, worker) = endpoint();
        let st = state(coordinated);
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        let (snap, plan) = exit_input(world.path(), &st);
        let result = launch_after_exit(&opts, &snap, ExitCause::Exit, report(), st, plan, client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-coordinated-unavailable"
            } else {
                "wait:exiting"
            }
        );
        assert!(
            seen.iter()
                .all(|line| line.starts_with(&format!("@{TAB} lease acquire ")))
        );
    }
}
