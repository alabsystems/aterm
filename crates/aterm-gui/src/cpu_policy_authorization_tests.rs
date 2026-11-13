// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn cpu_policy_fixed_sudo_arguments_reach_authentication_without_a_prompt() {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
    command
        .args(SUDO_ARGS.map(|arg| if arg == "-A" { "-n" } else { arg }))
        .arg("/usr/bin/true")
        .env("LC_ALL", "C");
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
        .expect("fixed noninteractive sudo invocation completed");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !error.contains("usage:") && !error.contains("Only one of"),
        "sudo rejected its fixed arguments: {error}"
    );
    assert!(
        output.status.success()
            || error.contains("a password is required")
            || error.contains("not allowed to execute")
            || error.contains("not in the sudoers file")
            || error.contains("authentication failure"),
        "sudo did not reach command authorization: {error}"
    );
}

#[test]
fn cpu_policy_no_askpass_ack_cannot_bypass_current_admission() {
    let claims = Claims::default();
    assert!(!claims.released());
    let writes = std::cell::Cell::new(0);
    for (live, halted, off) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        assert!(!issue_ack(
            || admission_allowed(live, halted, off),
            || false,
            || {
                writes.set(writes.get() + 1);
                true
            }
        ));
    }
    assert_eq!(writes.get(), 0, "NOPASSWD still cannot reach the ACK write");
    assert!(!issue_ack(
        || true,
        || true,
        || {
            writes.set(writes.get() + 1);
            true
        }
    ));
    assert_eq!(writes.get(), 0, "cancellation after permit forbids ACK");
    assert!(issue_ack(
        || true,
        || false,
        || {
            writes.set(writes.get() + 1);
            true
        }
    ));
    assert_eq!(writes.get(), 1);
}

#[test]
fn cpu_policy_no_askpass_still_requires_live_origin_halt_and_relay_admission() {
    assert!(admission_allowed(true, false, false));
    assert!(!admission_allowed(false, false, false));
    assert!(!admission_allowed(true, true, false));
    assert!(!admission_allowed(true, false, true));
    assert!(
        !owns_no_live_collector(None, false, false, false, false, false),
        "NOPASSWD may start a collector without any secret"
    );
}

#[test]
fn cpu_policy_control_is_owner_only_and_has_no_command_or_path_payload() {
    use crate::control::Scope;
    assert_eq!(admit(Scope::Owner, "policy"), Ok(Action::Status));
    assert_eq!(
        admit(Scope::Owner, "policy collect --json"),
        Ok(Action::Collect)
    );
    assert_eq!(admit(Scope::Owner, "policy cancel"), Ok(Action::Cancel));
    assert!(admit(Scope::Bridge, "policy collect").is_err());
    for (words, action) in [
        ("policy caller=s-origin_1", Action::Status),
        ("policy status caller=s-origin_1 --json", Action::Status),
        ("policy status --json caller=s-origin_1", Action::Status),
        ("policy collect caller=s-origin_1 --json", Action::Collect),
        ("policy collect --json caller=s-origin_1", Action::Collect),
        ("policy cancel caller=s-origin_1 --json", Action::Cancel),
        ("policy cancel --json caller=s-origin_1", Action::Cancel),
    ] {
        assert_eq!(admit(Scope::Owner, words), Ok(action), "{words}");
        assert!(admit(Scope::Bridge, words).is_err(), "{words}");
    }
    for arg in [
        "policy collect /bin/sh",
        "policy collect path=/tmp/report",
        "policy collect selector=7",
        "policy collect instances=all",
        "policy collect --force",
        "policy collect caller=",
        "policy collect caller=s/other",
        "policy collect caller=a%20b",
        "policy collect caller=s-1 caller=s-2",
        "policy collect caller=s-1 --json caller=s-1",
    ] {
        assert!(admit(Scope::Owner, arg).is_err(), "{arg}");
    }
    let boundary = format!("policy collect caller={}", "a".repeat(64));
    assert_eq!(admit(Scope::Owner, &boundary), Ok(Action::Collect));
    let too_long = format!("policy collect caller={}", "a".repeat(65));
    assert!(admit(Scope::Owner, &too_long).is_err());
}

#[test]
fn cpu_policy_closed_or_replaced_origin_cannot_authorize() {
    assert!(origin_matches("origin", Some("origin"), true));
    assert!(!origin_matches("origin", Some("replacement"), true));
    assert!(!origin_matches("origin", Some("origin"), false));
    assert!(!origin_matches("origin", None, true));
}

#[test]
fn cpu_policy_request_and_secret_are_each_claimed_once() {
    let claims = Arc::new(Claims::default());
    assert!(!claims.secret(), "no unrequested secret release");
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let claims = Arc::clone(&claims);
            std::thread::spawn(move || claims.request())
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert!(claims.secret());
    assert!(!claims.secret());
    assert!(
        !claims.request(),
        "sudo password retries do not create another consent"
    );
}

#[test]
fn cpu_policy_wrapper_exit_does_not_prove_collector_exit() {
    let id = Identity {
        pid: 1,
        start_s: 100,
        start_us: 0,
    };
    assert!(!owns_no_live_collector(
        Some(id),
        false,
        true,
        true,
        false,
        false
    ));
    assert!(owns_no_live_collector(
        Some(id),
        true,
        true,
        true,
        false,
        false
    ));
    assert!(!owns_no_live_collector(
        None, false, true, true, false, false
    ));
    assert!(owns_no_live_collector(
        None, false, false, true, false, false
    ));
    assert!(
        owns_no_live_collector(None, false, true, true, true, false),
        "a refused retry occurs before sudo can execute the collector"
    );
    let stdout = PipeState::default();
    let stderr = PipeState::default();
    stdout.done.store(true, Ordering::Release);
    assert!(
        !readers_finished(&stdout, &stderr, &None, &None),
        "stderr may still have a writer"
    );
    stderr.done.store(true, Ordering::Release);
    assert!(readers_finished(&stdout, &stderr, &None, &None));
    assert!(
        !stderr.eof.load(Ordering::Acquire),
        "completion and true EOF remain distinct"
    );
}

#[test]
fn cpu_policy_unbound_sudo_refusal_requires_reaping_and_true_owned_pipe_eof() {
    let stdout = PipeState::default();
    let stderr = PipeState::default();
    let drained = || owned_pipes_drained(true, &stdout, &stderr, &None, &None);
    assert!(!drained(), "wrapper exit alone leaves an unknown collector");
    stdout.done.store(true, Ordering::Release);
    stderr.done.store(true, Ordering::Release);
    assert!(!drained(), "read errors may finish without EOF");
    stdout.eof.store(true, Ordering::Release);
    assert!(!drained(), "one pipe still has an unknown writer");
    stderr.eof.store(true, Ordering::Release);
    assert!(!owned_pipes_drained(false, &stdout, &stderr, &None, &None));
    assert!(drained());
    assert!(
        owns_no_live_collector(None, false, false, false, false, drained()),
        "sudo can refuse before requesting any password"
    );
    assert!(
        owns_no_live_collector(None, false, true, true, false, drained()),
        "an authorized collector may exit before its hello"
    );
    let id = Identity {
        pid: 1,
        start_s: 100,
        start_us: 0,
    };
    assert!(
        !owns_no_live_collector(Some(id), false, true, true, false, drained()),
        "a bound generation still needs its independent exit observation"
    );
}

#[test]
fn cpu_policy_output_limit_and_partial_line_do_not_forge_eof_or_capture() {
    let (tx, rx) = std::sync::mpsc::channel();
    let state = PipeState::default();
    read_output(
        std::io::Cursor::new(b"{\"x\":1}\n{\"x\":2}\n"),
        &tx,
        &state,
        9,
    );
    assert!(state.eof.load(Ordering::Acquire));
    assert!(state.done.load(Ordering::Acquire));
    assert_eq!(rx.try_recv().unwrap(), Err("collector_output_limit"));
    assert!(rx.try_recv().is_err());
    let (tx, rx) = std::sync::mpsc::channel();
    let state = PipeState::default();
    read_output(std::io::Cursor::new(b"{\"x\":1}"), &tx, &state, 100);
    assert_eq!(rx.try_recv().unwrap(), Err("collector_output_incomplete"));
}

#[test]
fn cpu_policy_identity_parser_rejects_missing_reused_and_bad_generation() {
    let value = json!({"pid":1,"sec":"100","usec":5});
    assert_eq!(
        parsed_identity(&value, "pid", "sec", "usec"),
        Some(Identity {
            pid: 1,
            start_s: 100,
            start_us: 5
        })
    );
    assert!(
        parsed_identity(&json!({"pid":0,"sec":"100","usec":5}), "pid", "sec", "usec").is_none()
    );
    assert!(parsed_identity(&json!({"pid":1,"sec":"0","usec":5}), "pid", "sec", "usec").is_none());
    assert!(
        parsed_identity(
            &json!({"pid":1,"sec":"100","usec":1000000}),
            "pid",
            "sec",
            "usec"
        )
        .is_none()
    );
}
