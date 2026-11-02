// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual private constructor, selected with token=Some, against a bounded endpoint.
//! These controls exercise persistence and serialized requests; GUI tests prove admission.

use super::super::relaunch::PlannedCommand;
use super::super::upgrade::{Agent, Dialect, Phase};
use super::super::upgrade_drive::{HarnessToken, Instances, Tell};
use super::*;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

const SID: &str = "s-native-transport";

fn binding() -> Binding {
    Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    }
}

fn endpoint(
    answer: impl FnMut(&str) -> Option<String> + Send + 'static,
) -> (Client, std::thread::JoinHandle<Vec<String>>) {
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
        let mut answer = answer;
        let mut writer = far.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(far);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            seen.push(line.clone());
            let Some(reply) = answer(&line) else {
                break;
            };
            writer.write_all(reply.as_bytes()).unwrap();
        }
        seen
    });
    (crate::RelayClient::new(near), worker)
}

fn common(line: &str) -> Option<String> {
    if line == format!("@{SID} native-act constructors") {
        return Some("OK native-act state=constructors plans=private-restart\n".into());
    }
    if line == "help native-act" {
        return Some("OK 0\n".into());
    }
    if line == format!("@{SID} status") {
        return Some(
            "OK supervisor_revision=7 human_seq=0 human_ms=- hold=0 hand=- input=clear\n".into(),
        );
    }
    if line == format!("@{SID} native-act info") {
        return Some(format!(
            "OK native-act state=info epoch={} native-act=1 handoff=blocked clock=monotonic-us own-image=grok-alab:upgrade\n",
            "34".repeat(16)
        ));
    }
    None
}

fn request(line: &str) -> Request {
    Request::parse(
        line.strip_prefix(&format!("@{SID} native-act "))
            .expect("typed native request"),
    )
    .unwrap()
}

fn actual_request(line: &str) -> Option<Request> {
    Request::parse(line.strip_prefix(&format!("@{SID} native-act "))?)
}

fn opts(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(dir.join("native.sock").to_string_lossy().into_owned()),
        token: Some(HarnessToken::new("private-memory-fixture-token")),
        only_sid: Some(SID.into()),
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
        timings: crate::harness::Timings::FAST,
        tell: Tell::Off,
        press: None,
    }
}

fn planned() -> PlannedCommand {
    PlannedCommand {
        dialect: Dialect::Zsh,
        heal: None,
        cwd: None,
        target: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
    }
}

/// The test process stands in for both the agent and its shell: its own image is one nothing
/// relinks while it runs. Its launcher's is not: `verify`'s own test compile relinks the
/// `aterm-verify` it runs from, and [`Process::observe`] refuses a process whose file was replaced.
fn state(command: &PlannedCommand) -> (St, super::super::upgrade_drive::Unsent) {
    let process = Process::observe(std::process::id()).expect("actual test process");
    let mut st = St {
        agent: Agent::Claude,
        phase: Phase::Announced { at_s: 1, asks: 1 },
        source: "native".into(),
        from: "1.0.1".into(),
        to: "1.0.2".into(),
        ..St::default()
    };
    let line = command.capture().unwrap().legacy_line().unwrap();
    let back = st.signalled(process.pid, process.pid, SID, line.clone(), 2);
    st.back_line = line;
    (st, back)
}

#[test]
fn fresh_private_constructor_keeps_legacy_before_any_prepare_when_kind_is_disabled_or_absent() {
    for reply in [
        "OK native-act state=constructors plans=-\n",
        "OK native-act state=constructors plans=own-image\n",
        "ERR usage: unknown native-act verb\n",
        "OK native-act state=constructors plans=private-restart,unknown\n",
    ] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        let command = planned();
        let (mut st, _) = state(&command);
        let (mut client, worker) = endpoint(move |line| {
            (line == format!("@{SID} native-act constructors")).then(|| reply.to_string())
        });
        let result = begin(
            &opts,
            &mut client,
            &mut st,
            SID,
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command),
        );
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(result, Ok(Recovery::Legacy));
        assert_eq!(seen, [format!("@{SID} native-act constructors")]);
        assert!(!st.native_coordination);
        assert!(st.native_prepare.is_empty());
        assert!(st.native.is_none());
        assert!(
            !opts.state.exists(),
            "no preparation/persistence precedes enabled-kind discovery"
        );
    }
}

#[test]
fn disabled_kind_never_restores_raw_fallback_after_terminal_native_coordination() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let (mut st, back) = state(&command);
    st.native_coordination = true;
    st.native = Some(binding());
    st.native_prepare = "retained uncertain original".into();
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(Recovery::Cancelled)
    );
    st.unsent(back);
    let (mut client, worker) = endpoint(|line| {
        (line == format!("@{SID} native-act constructors"))
            .then(|| "OK native-act state=constructors plans=-\n".into())
    });
    let result = begin(
        &opts,
        &mut client,
        &mut st,
        SID,
        Scope::Upgrade,
        &command.target,
        Some(&command),
        Some(&command),
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(result, Err("native-constructor-disabled"));
    assert!(st.native_coordination);
    assert_eq!(seen, [format!("@{SID} native-act constructors")]);
    let mut no_private_token = opts.clone();
    no_private_token.token = None;
    let (mut client, worker) =
        endpoint(|_| panic!("no authority means no new native or raw request"));
    let result = begin(
        &no_private_token,
        &mut client,
        &mut st,
        SID,
        Scope::Upgrade,
        &command.target,
        Some(&command),
        Some(&command),
    );
    drop(client);
    assert!(worker.join().unwrap().is_empty());
    assert_eq!(result, Err("native-coordinated-unavailable"));
}

#[test]
fn private_token_constructor_retains_both_plans_and_original_budget_through_rollback() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let original = std::sync::Arc::new(std::sync::Mutex::new(None::<Prepare>));
    let kept = original.clone();
    let mut issued = None;
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let reply = match request(line) {
            Request::Prepare(plan) => {
                let mut saved = kept.lock().unwrap();
                if let Some(before) = saved.as_ref() {
                    assert_eq!(before, &*plan);
                } else {
                    *saved = Some((*plan).clone());
                }
                let cap = Binding {
                    id: plan.id,
                    capability: binding().capability,
                };
                issued = Some(cap.clone());
                format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    cap.id, cap.capability
                )
            }
            Request::Signal(cap) => {
                assert_eq!(Some(cap), issued);
                "OK native-act state=signalled\n".into()
            }
            Request::Resume(cap, phase, _, seq) => {
                assert_eq!(Some(cap), issued);
                assert_eq!(seq, 0);
                format!("OK native-act state=resumed phase={}\n", phase.word())
            }
            Request::Query(_) => "OK native-act state=resumed phase=rollback\n".into(),
            Request::Precommit(_, phase) => {
                assert_eq!(phase, ImagePhase::Rollback);
                "OK native-act state=precommitted\n".into()
            }
            Request::Commit(_, phase, version) => {
                assert_eq!(phase, ImagePhase::Rollback);
                assert_eq!(version, "1.0.1");
                "OK native-act state=committed claim=builtin revision=-\n".into()
            }
            other => panic!("unexpected constructor request {other:?}"),
        };
        Some(reply)
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    let prepared = st.native_prepare.clone();
    let cap = st.native.clone().unwrap();
    assert!(matches!(signal(&mut client, SID, &cap), Terminated::Sent));
    assert!(resume(&mut client, SID, &cap, ImagePhase::Forward, "1.1", 0).is_ok());
    st.relaunched(3);
    assert!(st.roll_back(4, 0));
    assert!(st.native_rollback);
    assert_eq!(recover(&mut client, SID, &mut st), Ok(Recovery::Admitted));
    assert_eq!(
        st.native_prepare, prepared,
        "rollback never creates another id or deadline"
    );
    assert!(resume(&mut client, SID, &cap, phase(&st), "1.2", 0).is_ok());
    assert!(complete(&mut client, SID, &mut st));
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    let prepared_plan = original.lock().unwrap().clone().unwrap();
    let NativePlan::Restart {
        forward,
        rollback: Some(back),
        ..
    } = prepared_plan.plan
    else {
        panic!("both fixed commands required");
    };
    assert_eq!(forward, back);
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Signal(_))))
            .count(),
        1
    );
    assert!(seen.iter().all(|line| line == "help native-act"
        || line.ends_with(" status")
        || line.contains(" native-act ")));
}

#[test]
fn queued_prepare_safe_receipt_restores_unsent_but_unknown_keeps_original_dispatch() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, back) = state(&command);
    let mut prepares = 0;
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let Request::Prepare(plan) = request(line) else {
            panic!("only the original prepare can be replayed");
        };
        prepares += 1;
        Some(if prepares == 1 {
            format!(
                "OK native-act state=pending id={} cap={}\n",
                plan.id,
                binding().capability
            )
        } else {
            "OK native-act state=rollback-safe\n".into()
        })
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-predecessors")
    );
    assert!(!st.native_prepare.is_empty());
    assert!(abort(&mut client, SID, &mut st));
    st.unsent(back);
    assert_eq!(st.phase, Phase::Announced { at_s: 1, asks: 1 });
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    drop(client);
    worker.join().unwrap();

    let (mut st, _) = state(&command);
    let (mut client, worker) =
        endpoint(|line| common(line).or_else(|| Some("OK native-act state=unknown\n".into())));
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-unknown")
    );
    let before = st.clone();
    assert_eq!(recover(&mut client, SID, &mut st), Err("native-unknown"));
    assert_eq!(st, before);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn private_constructor_refuses_current_person_before_prepare_and_never_sends_raw_signal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(dir.path());
    opts.human_grace_s = 120;
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) = endpoint(|line| {
        if line.ends_with(" status") {
            Some("OK supervisor_revision=7 human_seq=1 human_ms=0 hand=- input=clear\n".into())
        } else {
            common(line)
        }
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-person-or-hand")
    );
    assert!(st.native_prepare.is_empty());
    drop(client);
    let seen = worker.join().unwrap();
    assert!(!seen.iter().any(|line| line.contains(" native-act prepare")));

    let (mut client, worker) = endpoint(|_| Some("ERR denied native operation authority\n".into()));
    assert!(matches!(
        signal(&mut client, SID, &binding()),
        Terminated::Refused(_)
    ));
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(matches!(request(&seen[0]), Request::Signal(_)));
}

#[test]
fn completed_forward_receipt_can_only_prepare_its_fixed_old_slot_as_a_distinct_crash() {
    use super::super::native_observe::{ObservationBinding, Registration, SavedObservation};
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let plans = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Prepare>::new()));
    let recorded = plans.clone();
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let Request::Prepare(plan) = request(line) else {
            panic!("no effect may be dispatched by preparation");
        };
        recorded.lock().unwrap().push((*plan).clone());
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            plan.id,
            binding().capability
        ))
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    let original = plans.lock().unwrap()[0].clone();
    let process = Process::observe(std::process::id()).unwrap();
    let saved = SavedObservation {
        registration: Registration {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            vendor: process.clone(),
            shell: process.clone(),
        },
        binding: ObservationBinding {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            capability: "76".repeat(32),
        },
    };
    // The endpoint mocks a positive host outcome; GUI conformance proves the actual watch.
    assert_eq!(
        terminal_reply(&mut st, &Reply::RestartCommitted(Box::new(saved.clone()))),
        Some(Recovery::Completed)
    );
    assert!(st.roll_back(upgrade_drive::now_s(), 0));
    let attempt = st.failed_start_kept.clone();
    for case in 0..3 {
        let mut wrong = st.clone();
        match case {
            0 => {
                wrong
                    .native_observation
                    .as_mut()
                    .unwrap()
                    .registration
                    .host_epoch = "ff".repeat(16)
            }
            1 => {
                wrong
                    .native_observation
                    .as_mut()
                    .unwrap()
                    .registration
                    .vendor
                    .image
                    .inode += 1
            }
            _ => wrong.line.push(' '),
        }
        assert_eq!(
            begin_completed_rollback(&opts, &mut client, &mut wrong, "conversation"),
            Err("native-completed-recipe-changed")
        );
    }
    let mut active = st.clone();
    active.native_prepare = Request::prepare(original.clone()).encode().unwrap();
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut active, "conversation"),
        Err("native-unresolved-operation")
    );
    assert_eq!(
        plans.lock().unwrap().len(),
        1,
        "negative controls send no new prepare"
    );
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut st, "conversation"),
        Ok(Recovery::Admitted)
    );
    let next = plans.lock().unwrap()[1].clone();
    assert_ne!(next.id, original.id);
    assert_eq!(st.native_completed_prepare.as_deref(), Some(&original));
    assert_eq!(
        st.failed_start_kept, attempt,
        "native ids never reset the session's retry policy"
    );
    let NativePlan::Restart {
        rollback: Some(old),
        ..
    } = &original.plan
    else {
        panic!("old slot");
    };
    let NativePlan::Restart {
        process,
        observation: Some(observation),
        forward,
        rollback: None,
        ..
    } = &next.plan
    else {
        panic!("fixed fresh crash plan");
    };
    assert_eq!(forward, old);
    assert_eq!(process, &saved.registration.vendor);
    assert_eq!(observation, &saved.binding);
    assert_eq!(
        upgrade_drive::load(&opts, "conversation")
            .unwrap()
            .native_observation,
        Some(saved)
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert!(!seen.iter().any(|line| matches!(
        actual_request(line),
        Some(Request::Signal(_) | Request::Resume(..))
    )));
}
