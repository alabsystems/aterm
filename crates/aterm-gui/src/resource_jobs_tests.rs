// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn native_cut(clock: u64) -> aterm_vitals::model::NativeCpuCut {
    aterm_vitals::model::NativeCpuCut {
        continuous_ns: Some(clock),
        boot: Some(clock),
    }
}

#[cfg(unix)]
fn closed_native_source(
    host: aterm_resource_host::resource_observation::ProcessKey,
) -> aterm_resource_host::resource_observation::Observation {
    use aterm_resource_host::resource_observation::{
        Observation, ProcessKey, SessionUsage, Terminal,
    };
    Observation {
        host,
        host_child_cpu_ns: Some(10),
        at_unix_ms: 1,
        originals: Vec::new(),
        sessions: vec![SessionUsage {
            session: "s-acde1122".into(),
            reaped_cpu_ns: 10,
            unread: 0,
        }],
        outstanding_sessions: Vec::new(),
        first_seq: 1,
        next_seq: 1,
        terminals: vec![Terminal {
            seq: 1,
            session: "s-acde1122".into(),
            process: ProcessKey { pid: 300, start: 3 },
            uid: Some(501),
            coalition: None,
            version: None,
            cpu_ns: Some(10),
            child_cpu_ns: Some(0),
            wait_cpu_ns: Some(10),
            reaps_unwaited: Some(false),
            at_unix_ms: 1,
            exit_cut_ns: Some(10),
        }],
        unproven_active: 0,
        lost_origins: 0,
    }
}

#[cfg(unix)]
#[test]
fn ended_native_source_recovery_preserves_final_custody_and_accepts_the_new_cursor() {
    use aterm_resource_host::{ObservationRead, resource_observation::ProcessKey as NativeKey};
    let old = NativeKey { pid: 100, start: 1 };
    let new = NativeKey { pid: 200, start: 2 };
    let receipt = |seq| PendingNative {
        host_pid: old.pid,
        host_start: old.start,
        seq,
        session: "s-acde1122".into(),
        pid: 400 + seq as u32,
        start: 4 + seq,
        uid: Some(501),
        coalition: None,
        version: None,
        cpu_ns: Some(10),
        child_cpu_ns: Some(0),
        wait_cpu_ns: Some(10),
        reaps_unwaited: Some(false),
        exit_cut_ns: Some(8),
        window: None,
    };
    let mut state = State {
        native_host: Some(old),
        native_seq: 99,
        native_outstanding: true,
        native_awaiting_observation: true,
        native_tracking: true,
        native_tracking_cut: Some(5),
        ..State::default()
    };
    state
        .native_pending
        .insert((old.pid, old.start, 98), receipt(98));
    state
        .native_delivered
        .insert((old.pid, old.start, 97), receipt(97));
    let mut observed = Observation {
        procs: Some(Vec::new()),
        ..Observation::default()
    };
    apply_native_read(
        &mut state,
        &mut observed,
        ObservationRead {
            source_ended: true,
            result: Ok(closed_native_source(new)),
        },
        7,
        &[(7, "s-acde1122".into())],
    );
    assert_eq!(state.native_host, Some(new));
    assert_eq!(
        state.native_seq, 1,
        "a new source's low cursor is not the old source rolling back"
    );
    assert_eq!(state.native_tracking_cut, Some(5));
    assert!(state.native_pending.contains_key(&(old.pid, old.start, 98)));
    assert!(
        state
            .native_delivered
            .contains_key(&(old.pid, old.start, 97))
    );
    assert!(state.native_pending.contains_key(&(new.pid, new.start, 1)));
    assert!(
        observed.source_lost && !observed.incomplete,
        "lost historical CPU cannot make a complete current memory census partial"
    );
    deliver_native(
        &mut state,
        &mut observed,
        &[(7, "s-acde1122".into())],
        native_cut(20),
    );
    assert_eq!(observed.native_terminals.len(), 3);
    assert!(
        observed.procs.as_ref().unwrap().is_empty(),
        "historical finals never inject live PID rows"
    );
    native_model_pending_into(&mut state, false);
    assert!(!native_demand(&state, 0, 7));
    assert!(
        native_demand(&state, 0, 8),
        "a concurrent later submission still owes observation"
    );
    state.native_host = None;
    state.native_tracking = false;
    state.native_tracking_cut = None;
    let carried = carry_native(&state, 0, 7).unwrap();
    assert!(
        carried.source_lost,
        "historical loss survives even when it creates no sampling demand"
    );
    let mut restored = State::default();
    restore_native(&mut restored, carried, &|_| None);
    assert!(restored.native_source_lost);
    assert!(!native_demand(&restored, 0, 0));
}

#[cfg(unix)]
#[test]
fn source_ended_before_intake_clears_only_proven_observer_debt_and_transient_errors_retry() {
    use aterm_resource_host::{ObservationRead, resource_observation::ProcessKey as NativeKey};
    let old = NativeKey { pid: 100, start: 1 };
    let mut carried = State {
        native_host: Some(old),
        native_seq: 99,
        native_outstanding: true,
        native_awaiting_observation: true,
        native_tracking: true,
        native_tracking_cut: Some(5),
        ..State::default()
    };
    let mut transient = Observation::default();
    apply_native_read(
        &mut carried,
        &mut transient,
        ObservationRead {
            source_ended: false,
            result: Err(std::io::ErrorKind::PermissionDenied.into()),
        },
        7,
        &[],
    );
    deliver_native(&mut carried, &mut transient, &[], native_cut(6));
    assert_eq!(
        carried.native_cpu_cut,
        native_cut(6),
        "frontend cuts advance through observer failure"
    );
    assert_eq!(carried.native_host, Some(old));
    assert_eq!(carried.native_seq, 99);
    assert!(carried.native_outstanding && carried.native_awaiting_observation);
    assert!(!carried.native_source_lost);
    assert!(transient.incomplete);
    let mut restored = State::default();
    restore_native(
        &mut restored,
        carry_native(&carried, 0, 7).unwrap(),
        &|_| None,
    );
    assert!(restored.native_host.is_none() && native_demand(&restored, 0, 0));
    let mut absent = Observation::default();
    apply_native_read(
        &mut restored,
        &mut absent,
        ObservationRead {
            source_ended: true,
            result: Err(std::io::ErrorKind::NotConnected.into()),
        },
        7,
        &[],
    );
    assert!(absent.source_lost && !absent.incomplete);
    assert!(!native_demand(&restored, 0, 7));
    assert_eq!(restored.native_tracking_cut, Some(5));
    let new = NativeKey { pid: 200, start: 2 };
    let mut current = Observation::default();
    apply_native_read(
        &mut restored,
        &mut current,
        ObservationRead {
            source_ended: false,
            result: Ok(closed_native_source(new)),
        },
        8,
        &[],
    );
    assert_eq!(restored.native_host, Some(new));
    assert_eq!(restored.native_seq, 1);
    assert!(
        restored
            .native_pending
            .contains_key(&(new.pid, new.start, 1))
    );
    assert!(current.source_lost && !current.incomplete);
}

#[cfg(unix)]
#[test]
fn completed_native_journal_gaps_are_cpu_only_but_unproven_active_origins_affect_memory() {
    use aterm_resource_host::resource_observation::ProcessKey as NativeKey;
    let host = NativeKey { pid: 100, start: 1 };
    for kind in [
        "journal",
        "lost_origin",
        "unread_wait",
        "replacement",
        "initial_lost_origin",
        "active_origin",
    ] {
        let mut state = State {
            native_host: Some(host),
            native_seq: 1,
            native_tracking: true,
            native_tracking_cut: Some(0),
            ..State::default()
        };
        state.native_receipts.insert("s-acde1122".into(), (10, 0));
        let mut native = closed_native_source(host);
        match kind {
            "journal" => {
                native.first_seq = 3;
                native.next_seq = 3;
                native.terminals[0].seq = 3;
            }
            "lost_origin" => native.lost_origins = 1,
            "unread_wait" => native.sessions[0].unread = 1,
            "replacement" => native.host = NativeKey { pid: 200, start: 2 },
            "initial_lost_origin" => {
                state.native_host = None;
                native.lost_origins = 1;
            }
            "active_origin" => native.unproven_active = 1,
            _ => unreachable!(),
        }
        let mut observed = Observation {
            procs: Some(Vec::new()),
            ..Observation::default()
        };
        assert!(apply_native(&mut state, &mut observed, native, &[]));
        assert_eq!(observed.source_lost, kind != "active_origin", "{kind}");
        assert_eq!(observed.incomplete, kind == "active_origin", "{kind}");
    }
}

#[test]
fn natural_origins_cross_private_handoff_by_stable_session_and_current_generation() {
    use aterm_vitals::model::{ProcSample, Sample, TabRoot};
    let sid = "s-acde1122".to_string();
    let tab = |key| TabRoot {
        key,
        tab: key,
        shell_pid: 200,
        label: "worker".into(),
        program: "sh".into(),
        tab_id: None,
        agent_leader: None,
    };
    let worker = ProcSample {
        pid: 201,
        ppid: 200,
        start: Some(201),
        cpu_ns: Some(10),
        footprint: Some(32),
        ..Default::default()
    };
    let mut previous = aterm_vitals::Vitals::new();
    previous.absorb(&Sample {
        at_ns: 1_000_000_000,
        self_pid: 100,
        tabs: vec![tab(7)],
        procs: Some(vec![worker.clone()]),
        ..Default::default()
    });
    let mut source = State::default();
    publish_origins_into(&mut source, previous.process_origins(), &[(7, sid.clone())]);
    let wire = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        native: None,
        origins: source.origins,
        origins_incomplete: source.origins_incomplete,
    })
    .unwrap();
    for (current_boot, current_sid, generation, owned, tab_owned) in [
        ([7; 16], sid.clone(), 201, true, true),
        ([7; 16], "s-fedc3322".into(), 201, true, false),
        ([7; 16], sid.clone(), 999, false, false),
        ([8; 16], sid.clone(), 201, false, false),
    ] {
        let carried: Carry = aterm_json::from_value(wire.clone()).unwrap();
        let mut state = State::default();
        restore_origin_carry(&mut state, &carried, Some(current_boot));
        let mut next = aterm_vitals::Vitals::new();
        assert!(!restore_origins_into(
            &mut state,
            &mut next,
            &[(99, current_sid)]
        ));
        next.absorb(&Sample {
            at_ns: 1_000_000_000,
            self_pid: 101,
            tabs: vec![tab(99)],
            procs: Some(Vec::new()),
            procs_truncated: true,
            ..Default::default()
        });
        let snapshot = next.absorb(&Sample {
            at_ns: 2_000_000_000,
            self_pid: 101,
            tabs: vec![tab(99)],
            procs: Some(vec![ProcSample {
                ppid: 1,
                start: Some(generation),
                ..worker.clone()
            }]),
            ..Default::default()
        });
        assert_eq!(
            snapshot.procs.len(),
            1,
            "handoff does not fabricate a live PID row"
        );
        assert_eq!(snapshot.aterm.as_ref().unwrap().procs, u32::from(owned));
        assert_eq!(snapshot.tabs[0].scope.procs, u32::from(tab_owned));
        publish_origins_into(&mut state, next.process_origins(), &[(99, sid.clone())]);
        assert!(!state.origins_incomplete);
    }
}

#[test]
fn an_inflight_reading_cannot_overwrite_unadopted_origin_handoff() {
    use aterm_vitals::model::ProcessOrigin;
    let sessions = [(7, "s-acde1122".to_string())];
    let original = ProcessOrigin {
        process: ProcessKey {
            pid: 201,
            start: 201,
        },
        tab: Some(7),
        shared: false,
    };
    let mut state = State::default();
    publish_origins_into(&mut state, (vec![original], false), &sessions);
    state.origins_restore_pending = true;
    publish_origins_into(&mut state, (Vec::new(), false), &sessions);
    assert_eq!(
        state.origins.len(),
        1,
        "an older in-flight model has not adopted the incoming proof"
    );
    let mut next = aterm_vitals::Vitals::new();
    restore_origins_into(&mut state, &mut next, &sessions);
    assert_eq!(next.process_origins().0, [original]);
    publish_origins_into(&mut state, (Vec::new(), false), &sessions);
    assert!(
        state.origins.is_empty(),
        "after adoption a complete model may prune ended generations"
    );
}

#[test]
fn natural_origin_inventory_bounds_and_missing_coverage_survive_private_handoff() {
    use aterm_vitals::model::{PROCESS_ORIGIN_CAP, ProcessOrigin};
    let mut state = State::default();
    publish_origins_into(
        &mut state,
        (
            (0..=PROCESS_ORIGIN_CAP)
                .map(|index| ProcessOrigin {
                    process: ProcessKey {
                        pid: 1000 + index as u32,
                        start: index as u64 + 1,
                    },
                    tab: Some(7),
                    shared: false,
                })
                .collect(),
            false,
        ),
        &[(7, "s-acde1122".into())],
    );
    assert_eq!(state.origins.len(), PROCESS_ORIGIN_CAP);
    assert!(state.origins_incomplete);
    let wire = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        native: None,
        origins: state.origins,
        origins_incomplete: state.origins_incomplete,
    })
    .unwrap();
    let carried: Carry = aterm_json::from_value(wire).unwrap();
    let mut restored = State::default();
    restore_origin_carry(&mut restored, &carried, Some([7; 16]));
    let mut model = aterm_vitals::Vitals::new();
    assert!(restore_origins_into(
        &mut restored,
        &mut model,
        &[(99, "s-acde1122".into())]
    ));
    assert_eq!(model.process_origins().0.len(), PROCESS_ORIGIN_CAP);
    let mut foreign_boot = State::default();
    restore_origin_carry(&mut foreign_boot, &carried, Some([8; 16]));
    assert!(foreign_boot.origins.is_empty());
    assert!(
        !foreign_boot.origins_incomplete,
        "another boot transfers neither authority nor its gaps"
    );
    let mut unread_boot = State::default();
    restore_origin_carry(&mut unread_boot, &carried, None);
    assert!(unread_boot.origins.is_empty());
    assert!(
        unread_boot.origins_incomplete,
        "an unavailable boot identity cannot prove old custody ended"
    );
}

#[test]
fn conflicting_carried_origins_are_rejected_instead_of_choosing_a_tab() {
    let claim = CarriedOrigin {
        pid: 201,
        start: 201,
        session: Some("s-acde1122".into()),
        shared: false,
    };
    let mut carry = Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        native: None,
        origins: vec![claim.clone(), claim.clone()],
        origins_incomplete: false,
    };
    let mut same = State::default();
    restore_origin_carry(&mut same, &carry, Some([7; 16]));
    assert_eq!(same.origins.len(), 1);
    assert!(!same.origins_incomplete);
    carry.origins.insert(
        1,
        CarriedOrigin {
            session: Some("s-fedc3322".into()),
            ..claim
        },
    );
    let mut conflicting = State::default();
    restore_origin_carry(&mut conflicting, &carry, Some([7; 16]));
    assert!(
        conflicting.origins.is_empty(),
        "a later duplicate cannot revive a rejected process claim"
    );
    assert!(conflicting.origins_incomplete);
}

#[cfg(unix)]
#[test]
fn native_demand_keeps_inflight_jobs_unread_revisions_and_model_custody() {
    let mut state = State::default();
    assert!(!native_demand(&state, 0, 0));
    assert!(native_demand(&state, 1, 0));
    assert!(
        native_demand(&state, 0, 2),
        "a short job still owes its final observer drain"
    );
    state.native_observed_revision = 2;
    assert!(!native_demand(&state, 0, 2));
    state.native_outstanding = true;
    assert!(
        native_demand(&state, 0, 2),
        "queued native work is not an empty process tree"
    );
    state.native_outstanding = false;
    state.native_unadopted = true;
    assert!(
        native_demand(&state, 0, 2),
        "a truncated model sweep still owes adoption"
    );
}

#[cfg(unix)]
#[test]
fn historical_native_receipts_survive_their_custodian_and_missing_time_does_not_poll_forever() {
    let record = PendingNative {
        host_pid: 100,
        host_start: 1,
        seq: 1,
        session: "s-acde1122".into(),
        pid: 200,
        start: 2,
        uid: Some(0),
        coalition: Some(900),
        version: Some(2),
        cpu_ns: Some(10),
        child_cpu_ns: Some(0),
        wait_cpu_ns: Some(10),
        reaps_unwaited: Some(false),
        exit_cut_ns: Some(10),
        window: None,
    };
    let mut state = State::default();
    restore_native(
        &mut state,
        NativeCarry {
            pid: Some(100),
            start: Some(1),
            seq: 1,
            lost_origins: 0,
            pending: vec![record.clone()],
            queue_lost: 0,
            tracking_cut: Some(0),
            tracking: true,
            outstanding: true,
            awaiting_observation: false,
            observer: None,
            source_lost: false,
            cpu_cut: native_cut(5).into(),
        },
        &|_| None,
    );
    assert!(
        state.native_host.is_none(),
        "a historical receipt never creates a live host claim"
    );
    let handoff = carry_native(&state, 0, 0).expect("a historical pending receipt is still owned");
    assert!(handoff.pid.is_none());
    let wire = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        native: Some(handoff),
        origins: Vec::new(),
        origins_incomplete: false,
    })
    .unwrap();
    let carried: Carry = aterm_json::from_value(wire).unwrap();
    let mut second = State::default();
    restore_native(&mut second, carried.native.unwrap(), &|_| None);
    state = second;
    let mut observed = Observation::default();
    deliver_native(
        &mut state,
        &mut observed,
        &[(7, record.session.clone())],
        native_cut(20),
    );
    assert_eq!(observed.native_terminals.len(), 1);
    assert_eq!(observed.native_terminals[0].process.uid, Some(0));
    native_model_pending_into(&mut state, false);
    let mut untimed = record;
    untimed.seq = 2;
    untimed.exit_cut_ns = None;
    state.native_pending.insert((100, 1, 2), untimed);
    let mut missing = Observation::default();
    deliver_native(&mut state, &mut missing, &[], native_cut(30));
    assert!(missing.source_lost);
    assert!(
        !missing.incomplete,
        "a lost final timestamp says nothing about current memory ownership"
    );
    assert_eq!(missing.native_terminals.len(), 1);
    assert_eq!(missing.native_terminals[0].process.cpu_ns, Some(10));
    assert_eq!(missing.native_terminals[0].process.child_cpu_ns, Some(0));
    assert_eq!(
        missing.native_terminals[0].timing.exited_ns, None,
        "a missing clock preserves the final split without inventing its CPU interval"
    );
    assert!(
        native_demand(&state, 0, 0),
        "diagnostic receipt custody still awaits model adoption"
    );
    native_model_pending_into(&mut state, false);
    let mut later = Observation::default();
    deliver_native(&mut state, &mut later, &[], native_cut(40));
    assert!(later.native_terminals.is_empty());
    assert_eq!(state.native_queue_lost, 0);
    assert!(!native_demand(&state, 0, 0));
}

#[cfg(unix)]
#[test]
fn handoff_before_the_first_native_observation_keeps_the_unread_submission_demand() {
    let state = State {
        native_tracking: true,
        native_tracking_cut: Some(10),
        ..State::default()
    };
    let native = carry_native(&state, 0, 2).expect("a completed submission still owes observation");
    assert!(native.pid.is_none());
    assert!(native.awaiting_observation);
    let encoded = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        native: Some(native),
        origins: Vec::new(),
        origins_incomplete: false,
    })
    .unwrap();
    let carried: Carry = aterm_json::from_value(encoded).unwrap();
    let mut restored = State::default();
    restore_native(&mut restored, carried.native.unwrap(), &|_| None);
    assert!(native_demand(&restored, 0, 0));
    native_model_pending_into(&mut restored, false);
    assert!(
        native_demand(&restored, 0, 0),
        "an empty model is not an observer acknowledgement"
    );
}

#[cfg(unix)]
#[test]
fn bounding_combined_native_custody_preserves_current_scope_gaps_and_separates_final_loss() {
    let mut state = State::default();
    for seq in 1..=65538 {
        let record = PendingNative {
            host_pid: 100,
            host_start: 1,
            seq,
            session: "s-acde1122".into(),
            pid: 200,
            start: 2,
            uid: None,
            coalition: None,
            version: None,
            cpu_ns: Some(1),
            child_cpu_ns: Some(0),
            wait_cpu_ns: Some(1),
            reaps_unwaited: Some(false),
            exit_cut_ns: Some(seq),
            window: None,
        };
        if seq % 2 == 0 {
            state.native_delivered.insert((100, 1, seq), record);
        } else {
            state.native_pending.insert((100, 1, seq), record);
        }
    }
    let mut observed = Observation {
        unproved_tabs: vec![7, 7],
        ..Observation::default()
    };
    bound_native_queue(&mut state, &mut observed);
    assert_eq!(
        state.native_pending.len() + state.native_delivered.len(),
        65536
    );
    assert_eq!(state.native_queue_lost, 2);
    assert!(observed.source_lost);
    assert!(!observed.incomplete);
    assert_eq!(observed.unproved_tabs, [7]);
    assert!(!state.native_pending.contains_key(&(100, 1, 1)));
    assert!(!state.native_delivered.contains_key(&(100, 1, 2)));
    assert!(state.native_delivered.contains_key(&(100, 1, 65538)));
}

fn carried(root: ProcessKey, member: ProcessKey) -> aterm_json::Value {
    aterm_json::to_value(Carry {
        boot: [7; 16],
        native: None,
        origins: Vec::new(),
        origins_incomplete: false,
        jobs: vec![CarriedJob {
            pid: root.pid,
            start: root.start,
            session: "s-a".into(),
            ai: "codex".into(),
            label: "project:compiler".into(),
            active: true,
            members: vec![(member.pid, member.start)],
        }],
    })
    .unwrap()
}

fn changed_carry(root: ProcessKey, change: impl FnOnce(&mut CarriedJob)) -> aterm_json::Value {
    let mut carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
    change(&mut carry.jobs[0]);
    aterm_json::to_value(carry).unwrap()
}

#[test]
fn capabilities_default_to_the_current_observed_program_not_a_previous_agent() {
    let session = crate::stub_session(0);
    for observed in [Some("codex"), Some("claude"), Some("zsh"), None] {
        {
            let mut timeline = session.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            timeline.set_program(42, observed.map(str::to_string));
        }
        let expected = observed.unwrap_or("unknown");
        assert_eq!(program(&session.ctx), expected);
        let reply = command(&session.ctx, true, "capabilities");
        assert!(
            reply
                .split_whitespace()
                .any(|part| part == format!("ai={expected}"))
        );
    }
}

#[test]
fn cpu_control_reports_the_shared_observation_without_claiming_a_lease() {
    use std::io::{BufRead, BufReader, Write};
    let session = crate::stub_session(0);
    let reply = command(&session.ctx, true, "cpu");
    let within = std::time::Duration::from_secs(60);
    let (server, client) = aterm_uds::CtlStream::pair().unwrap();
    server.set_read_timeout(Some(within)).unwrap();
    server.set_write_timeout(Some(within)).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let mut reader = BufReader::new(&server);
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            assert_eq!(line.trim_end(), "resources cpu");
            (&server).write_all(reply.as_bytes()).unwrap();
        });
        let lines = aterm_ctl::exchange_lines(&client, "fixture", "resources cpu", within)
            .expect("resources cpu must use the client's line framing");
        assert_eq!(lines.len(), 1);
        let json: aterm_json::Value = aterm_json::from_str(&lines[0]).unwrap();
        assert!(json.get("state").is_some());
    });
    assert!(command(&session.ctx, true, "cpu extra").starts_with("ERR usage:"));
}

fn claimed_session() -> crate::Session {
    let session = crate::stub_session(0);
    let mut meta = session.ctx.meta.lock().unwrap();
    meta.supervisor = Some(crate::session_timeline::SupervisorClaim {
        holder: "codex-alab@42".into(),
        conn: Some(1),
        expires_us: None,
        proven: Some(crate::self_claim::Proven {
            pid: 42,
            start: "fixture birth".into(),
        }),
        scoped: None,
    });
    meta.supervisor_revision = 1;
    drop(meta);
    session
}

#[test]
fn a_live_proven_foreground_claim_fills_an_unresolved_resource_program() {
    let session = claimed_session();
    let ctx = &session.ctx;
    assert_eq!(
        program_by(
            ctx,
            || {
                assert!(ctx.meta.try_lock().is_ok());
                assert!(ctx.timeline.try_lock().is_ok());
                42
            },
            |proof| {
                assert_eq!(proof.pid, 42);
                assert_eq!(proof.start, "fixture birth");
                assert!(ctx.meta.try_lock().is_ok());
                assert!(ctx.timeline.try_lock().is_ok());
                true
            },
            |pid| {
                assert_eq!(pid, 42);
                assert!(ctx.meta.try_lock().is_ok());
                assert!(ctx.timeline.try_lock().is_ok());
                true
            },
        ),
        "codex"
    );
}

#[test]
fn resource_program_needs_proof_and_a_current_process_image_and_foreground() {
    for failure in [
        "unproven",
        "expired",
        "dead",
        "reused",
        "image",
        "background",
        "unreadable",
    ] {
        let session = claimed_session();
        let ctx = &session.ctx;
        {
            let mut meta = ctx.meta.lock().unwrap();
            let claim = meta.supervisor.as_mut().unwrap();
            match failure {
                "unproven" => claim.proven = None,
                "expired" => claim.expires_us = Some(0),
                _ => {}
            }
        }
        assert_eq!(
            program_by(
                ctx,
                || match failure {
                    "background" => 43,
                    "unreadable" => -1,
                    _ => 42,
                },
                |_| !matches!(failure, "dead" | "reused"),
                |_| failure != "image",
            ),
            "unknown",
            "{failure}"
        );
    }
    let session = crate::stub_session(0);
    assert_eq!(
        program_by(
            &session.ctx,
            || panic!("no proof"),
            |_| panic!("no proof"),
            |_| panic!("no proof")
        ),
        "unknown"
    );
}

#[test]
fn resource_program_rejects_identity_image_or_foreground_lost_during_probes() {
    for changed in ["birth", "image", "foreground"] {
        let session = claimed_session();
        let mut identities = 0;
        let mut images = 0;
        let mut foregrounds = 0;
        assert_eq!(
            program_by(
                &session.ctx,
                || {
                    foregrounds += 1;
                    if changed == "foreground" && foregrounds == 2 {
                        43
                    } else {
                        42
                    }
                },
                |_| {
                    identities += 1;
                    changed != "birth" || identities == 1
                },
                |_| {
                    images += 1;
                    changed != "image" || images == 1
                },
            ),
            "unknown",
            "{changed}"
        );
    }
}

#[test]
fn resource_program_rechecks_claim_revision_proof_and_expiry_after_probes() {
    for changed in ["renewed", "replaced", "released", "expired"] {
        let session = claimed_session();
        let ctx = &session.ctx;
        assert_eq!(
            program_by(
                ctx,
                || {
                    let mut meta = ctx.meta.lock().unwrap();
                    match changed {
                        "renewed" => meta.supervisor_revision += 1,
                        "replaced" => {
                            meta.supervisor
                                .as_mut()
                                .unwrap()
                                .proven
                                .as_mut()
                                .unwrap()
                                .start = "another birth".into()
                        }
                        "released" => meta.supervisor = None,
                        "expired" => meta.supervisor.as_mut().unwrap().expires_us = Some(0),
                        _ => unreachable!(),
                    }
                    42
                },
                |_| true,
                |_| true,
            ),
            "unknown",
            "{changed}"
        );
    }
}

#[test]
fn resource_program_preserves_observed_names_including_names_resolved_during_probes() {
    for name in ["codex", "claude", "zsh"] {
        let session = claimed_session();
        let ctx = &session.ctx;
        let observe = || {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            timeline.set_program(42, Some(name.into()));
        };
        assert_eq!(
            program_by(
                ctx,
                || 42,
                |_| true,
                |_| {
                    observe();
                    true
                }
            ),
            name
        );
        assert_eq!(
            program_by(
                ctx,
                || panic!("already observed"),
                |_| panic!("already observed"),
                |_| panic!("already observed")
            ),
            name
        );
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.set_program(42, None);
        }
        assert_eq!(
            program_by(
                ctx,
                || 42,
                |_| true,
                |_| {
                    observe();
                    false
                }
            ),
            name,
            "a resolved name also wins when the proof's image is gone"
        );
    }
}

#[test]
fn activation_is_bounded_under_tab_churn_and_keeps_closed_tabs_live_jobs() {
    let mut state = State::default();
    for id in 0..1000 {
        activate_resource_sessions(&mut state, &[(id, format!("s-{id:x}"))]);
    }
    assert!(state.activated.is_empty());
    let root = ProcessKey { pid: 20, start: 2 };
    state
        .ledger
        .register(Job {
            root,
            session: "s-a".into(),
            ai: "codex".into(),
            label: "build".into(),
        })
        .unwrap();
    activate_resource_sessions(&mut state, &[(10, "s-a".into())]);
    activate_resource_sessions(&mut state, &[]);
    assert_eq!(state.activated, HashSet::from(["s-a".to_string()]));
    state.ledger.observe(&[]);
    activate_resource_sessions(&mut state, &[]);
    assert!(state.activated.is_empty());
}

#[test]
fn a_carried_closed_tabs_authorized_job_stays_active_but_new_jobs_wait_for_hosting() {
    let root = ProcessKey { pid: 20, start: 2 };
    let current = |pid| (pid == root.pid).then_some(root);
    let mut state = State::default();
    let active = restore_into(
        &mut state.ledger,
        carried(root, root),
        Some([7; 16]),
        &current,
    );
    state.activated.extend(active);
    activate_resource_sessions(&mut state, &[]);
    assert!(state.activated.contains("s-a"));
    let mut fresh = State::default();
    let value = changed_carry(root, |job| job.active = false);
    let active = restore_into(&mut fresh.ledger, value, Some([7; 16]), &current);
    fresh.activated.extend(active);
    activate_resource_sessions(&mut fresh, &[]);
    assert!(fresh.activated.is_empty());
    activate_resource_sessions(&mut fresh, &[(10, "s-a".into())]);
    assert!(fresh.activated.contains("s-a"));
}

#[test]
fn unrepresentable_resource_carry_is_omitted_before_terminal_serialization() {
    let root = ProcessKey {
        pid: 20,
        start: u64::MAX,
    };
    let carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
    assert!(encode_carry(carry).is_none());
    let root = ProcessKey {
        pid: 20,
        start: i64::MAX as u64,
    };
    let carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
    assert!(encode_carry(carry).is_some());
}

#[test]
fn registration_requires_the_callers_expected_start_and_two_matching_kernel_reads() {
    let root = ProcessKey { pid: 20, start: 2 };
    assert!(confirmed_peer(root, Some(2), Some(root)));
    assert!(!confirmed_peer(root, None, Some(root)));
    assert!(!confirmed_peer(root, Some(1), Some(root)));
    assert!(!confirmed_peer(root, Some(2), None));
    assert!(!confirmed_peer(
        root,
        Some(2),
        Some(ProcessKey { pid: 20, start: 3 })
    ));
}

#[test]
fn late_registered_root_replaces_the_sweeps_stale_pid_identity() {
    let root = ProcessKey { pid: 20, start: 2 };
    let mut ledger = Ledger::default();
    ledger
        .register(Job {
            root,
            session: "s-a".into(),
            ai: "codex".into(),
            label: "build".into(),
        })
        .unwrap();
    let sweep = aterm_sysprobe::Sweep {
        procs: vec![aterm_sysprobe::ProcessReading {
            pid: 20,
            start: Some(1),
            ppid: 1,
            ..Default::default()
        }],
        truncated: false,
    };
    assert!(!reconcile(&mut ledger, &sweep, &|_| Identity::Known(root)));
    assert_eq!(ledger.jobs().len(), 1);
    assert_eq!(
        ledger.bindings(),
        vec![Binding {
            process: root,
            root
        }]
    );
    reconcile(&mut ledger, &aterm_sysprobe::Sweep::default(), &|_| {
        Identity::Absent
    });
    assert!(ledger.is_empty());
}

#[test]
fn truncated_sweep_retains_ownership_and_never_refreshes_old_usage_as_zero() {
    let root = ProcessKey { pid: 20, start: 2 };
    let mut state = State::default();
    state
        .ledger
        .register(Job {
            root,
            session: "s-a".into(),
            ai: "codex".into(),
            label: "build".into(),
        })
        .unwrap();
    state.ledger.restore(Binding {
        process: root,
        root,
    });
    let usage = vec![aterm_vitals::model::DelegatedUsage {
        root,
        cores: 1.0,
        footprint: Some(4096),
        procs: 1,
    }];
    let sampled = Instant::now();
    publish_into(&mut state, Some(usage.clone()), sampled);
    reconcile(
        &mut state.ledger,
        &aterm_sysprobe::Sweep {
            truncated: true,
            ..Default::default()
        },
        &|_| Identity::Absent,
    );
    publish_into(
        &mut state,
        None,
        sampled + std::time::Duration::from_secs(5),
    );
    assert_eq!(
        state.ledger.bindings(),
        vec![Binding {
            process: root,
            root
        }]
    );
    assert_eq!(state.usage, usage);
    assert_eq!(state.sampled, Some(sampled));
    assert!(!state.current_complete);
}

fn reading(key: ProcessKey, parent: u32, cpu_ns: u64) -> aterm_sysprobe::ProcessReading {
    aterm_sysprobe::ProcessReading {
        pid: key.pid,
        start: Some(key.start),
        ppid: parent,
        cpu_ns: Some(cpu_ns),
        footprint: Some(u64::from(key.pid) * 1024),
        ..Default::default()
    }
}

fn sampled(
    state: &mut State,
    vitals: &mut aterm_vitals::Vitals,
    sweep: &aterm_sysprobe::Sweep,
    lookup: &dyn Fn(u32) -> Identity,
    second: u64,
    epoch: Instant,
) -> aterm_vitals::model::Snapshot {
    let observed = sample_into(state, sweep, &[(1, "s-a".into())], lookup, epoch);
    let complete = !observed.incomplete;
    let expected_jobs = observed.delegated.len();
    let snapshot = vitals.absorb(&aterm_vitals::model::Sample {
        at_ns: second * 1_000_000_000,
        at_unix_ms: second as i64 * 1000,
        mono_ms: second * 1000,
        cadence_ms: 1000,
        self_pid: 100,
        tabs: vec![aterm_vitals::model::TabRoot {
            key: 1,
            tab: 1,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "codex".into(),
            tab_id: None,
            agent_leader: None,
        }],
        procs: observed.procs,
        delegated: observed.delegated,
        native_incomplete: observed.incomplete,
        unproved_tabs: observed.unproved_tabs,
        ..Default::default()
    });
    publish_into(
        state,
        (complete && snapshot.delegated.len() == expected_jobs).then(|| snapshot.delegated.clone()),
        epoch + std::time::Duration::from_secs(second),
    );
    snapshot
}

#[test]
fn unreadable_retained_members_keep_ownership_and_old_age_until_complete_recovery() {
    let root = ProcessKey { pid: 20, start: 2 };
    let member = ProcessKey { pid: 21, start: 3 };
    for root_live in [false, true] {
        for (failure, fresh_available) in [
            ("omitted", false),
            ("start", false),
            ("omitted", true),
            ("start", true),
            ("cpu", true),
            ("footprint", true),
        ] {
            let mut state = State::default();
            state
                .ledger
                .register(Job {
                    root,
                    session: "s-a".into(),
                    ai: "codex".into(),
                    label: "build".into(),
                })
                .unwrap();
            let mut vitals = aterm_vitals::Vitals::new();
            let epoch = Instant::now();
            let initial = aterm_sysprobe::Sweep {
                procs: vec![reading(root, 10, 0), reading(member, root.pid, 0)],
                truncated: false,
            };
            sampled(
                &mut state,
                &mut vitals,
                &initial,
                &|pid| {
                    if pid == root.pid {
                        Identity::Known(root)
                    } else {
                        Identity::Absent
                    }
                },
                1,
                epoch,
            );
            let healthy = |second: u64| aterm_sysprobe::Sweep {
                procs: if root_live {
                    vec![
                        reading(root, 10, second * 1_000_000_000),
                        reading(member, root.pid, second * 1_000_000_000),
                    ]
                } else {
                    vec![reading(member, 1, second * 1_000_000_000)]
                },
                truncated: false,
            };
            let lookup = |pid| {
                if pid == member.pid {
                    Identity::Known(member)
                } else if root_live && pid == root.pid {
                    Identity::Known(root)
                } else {
                    Identity::Absent
                }
            };
            sampled(&mut state, &mut vitals, &healthy(2), &lookup, 2, epoch);
            assert!(state.current_complete);
            assert!(state.usage[0].cores > 0.0);
            let old_usage = state.usage.clone();
            let old_age = state.sampled;
            let old_bindings = state.ledger.bindings();
            let mut unreadable = healthy(3);
            match failure {
                "omitted" => unreadable.procs.retain(|p| p.pid != member.pid),
                _ => {
                    let p = unreadable
                        .procs
                        .iter_mut()
                        .find(|p| p.pid == member.pid)
                        .unwrap();
                    match failure {
                        "start" => p.start = None,
                        "cpu" => p.cpu_ns = None,
                        "footprint" => p.footprint = None,
                        _ => unreachable!(),
                    }
                }
            }
            let partial_lookup = |pid| {
                if !fresh_available && pid == member.pid {
                    Identity::Unavailable
                } else {
                    lookup(pid)
                }
            };
            let partial = sampled(
                &mut state,
                &mut vitals,
                &unreadable,
                &partial_lookup,
                3,
                epoch,
            );
            let app = partial
                .aterm
                .as_ref()
                .expect("partial evidence remains readable");
            assert!(
                app.footprint_unread > 0,
                "{root_live}/{failure}/fresh={fresh_available} cannot publish reduced memory as exact"
            );
            assert!(partial.tab(1).unwrap().scope.footprint_unread > 0);
            assert!(partial.memory_owners.unwrap().ownership_incomplete);
            let cpu = partial.resource(aterm_vitals::Resource::Cpu, &[1]);
            assert!(cpu.aterm.unread > 0);
            assert!(cpu.tab.unread > 0);
            assert!(partial.delegated.is_empty());
            assert_eq!(state.ledger.bindings(), old_bindings);
            assert_eq!(state.usage, old_usage);
            assert_eq!(state.sampled, old_age);
            assert!(!state.current_complete);
            let mut recovered = sampled(&mut state, &mut vitals, &healthy(4), &lookup, 4, epoch);
            let completed_at = if matches!(failure, "omitted" | "start" | "cpu") {
                assert!(
                    recovered.delegated.is_empty(),
                    "the first readable counter is a baseline, not zero work"
                );
                assert_eq!(state.usage, old_usage);
                assert_eq!(state.sampled, old_age);
                assert!(!state.current_complete);
                recovered = sampled(&mut state, &mut vitals, &healthy(5), &lookup, 5, epoch);
                5
            } else {
                4
            };
            let count = if root_live { 2 } else { 1 };
            assert!(state.current_complete);
            assert_eq!(state.ledger.bindings(), old_bindings);
            assert_eq!(state.usage.len(), 1);
            assert_eq!(state.usage[0].procs, count);
            assert_eq!(recovered.aterm.as_ref().unwrap().procs, count);
            assert_eq!(recovered.tab(1).unwrap().scope.procs, count);
            assert_eq!(
                state.sampled,
                Some(epoch + std::time::Duration::from_secs(completed_at))
            );
        }
    }
}

#[test]
fn fresh_reused_member_identity_never_repairs_old_detached_ownership() {
    let root = ProcessKey { pid: 20, start: 2 };
    let member = ProcessKey { pid: 21, start: 3 };
    let reused = ProcessKey {
        pid: member.pid,
        start: 9,
    };
    let mut ledger = Ledger::default();
    ledger
        .register(Job {
            root,
            session: "s-a".into(),
            ai: "codex".into(),
            label: "build".into(),
        })
        .unwrap();
    ledger.observe(&[
        Process {
            key: root,
            parent: 10,
        },
        Process {
            key: member,
            parent: root.pid,
        },
    ]);
    ledger.observe(&[Process {
        key: member,
        parent: 1,
    }]);
    assert!(reconcile(
        &mut ledger,
        &aterm_sysprobe::Sweep::default(),
        &|pid| if pid == reused.pid {
            Identity::Known(reused)
        } else {
            Identity::Absent
        }
    ));
    assert!(ledger.is_empty());
    assert!(ledger.bindings().is_empty());
    reconcile(
        &mut ledger,
        &aterm_sysprobe::Sweep {
            procs: vec![reading(reused, 1, 1)],
            truncated: false,
        },
        &|_| Identity::Known(reused),
    );
    assert!(ledger.bindings().is_empty());
}

#[test]
fn confirmed_exit_or_reuse_prunes_previously_unavailable_detached_members() {
    let root = ProcessKey { pid: 20, start: 2 };
    let member = ProcessKey { pid: 21, start: 3 };
    for later in [
        Identity::Absent,
        Identity::Known(ProcessKey {
            pid: member.pid,
            start: 9,
        }),
    ] {
        let mut ledger = Ledger::default();
        ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
        ledger.observe(&[
            Process {
                key: root,
                parent: 10,
            },
            Process {
                key: member,
                parent: root.pid,
            },
        ]);
        ledger.observe(&[Process {
            key: member,
            parent: 1,
        }]);
        assert!(!reconcile(
            &mut ledger,
            &aterm_sysprobe::Sweep::default(),
            &|pid| if pid == member.pid {
                Identity::Unavailable
            } else {
                Identity::Absent
            }
        ));
        assert_eq!(
            ledger.bindings(),
            vec![Binding {
                process: member,
                root
            }]
        );
        reconcile(&mut ledger, &aterm_sysprobe::Sweep::default(), &|pid| {
            if pid == member.pid {
                later
            } else {
                Identity::Absent
            }
        });
        assert!(ledger.is_empty());
        assert!(ledger.bindings().is_empty());
    }
}

#[test]
fn missing_old_and_new_roots_at_one_pid_share_one_fresh_identity_read() {
    let old = ProcessKey { pid: 20, start: 2 };
    let new = ProcessKey { pid: 20, start: 9 };
    let child = ProcessKey { pid: 21, start: 3 };
    let mut ledger = Ledger::default();
    for root in [old, new] {
        ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
    }
    ledger.restore(Binding {
        process: child,
        root: old,
    });
    let reads = std::cell::Cell::new(0);
    assert!(!reconcile(
        &mut ledger,
        &aterm_sysprobe::Sweep::default(),
        &|pid| {
            if pid == new.pid {
                reads.set(reads.get() + 1);
                Identity::Known(new)
            } else if pid == child.pid {
                Identity::Known(child)
            } else {
                Identity::Absent
            }
        }
    ));
    assert_eq!(reads.get(), 1);
    assert_eq!(ledger.jobs().len(), 2);
    assert_eq!(
        ledger.bindings(),
        vec![
            Binding {
                process: new,
                root: new
            },
            Binding {
                process: child,
                root: old
            }
        ]
    );
}

#[test]
fn handoff_keeps_live_detached_members_and_refuses_other_boots_and_reused_pids() {
    let root = ProcessKey { pid: 20, start: 2 };
    let member = ProcessKey { pid: 21, start: 3 };
    let current = |pid| (pid == member.pid).then_some(member);
    let mut ledger = Ledger::default();
    restore_into(&mut ledger, carried(root, member), Some([7; 16]), &current);
    assert_eq!(ledger.jobs().len(), 1);
    assert_eq!(
        ledger.bindings(),
        vec![Binding {
            process: member,
            root
        }]
    );
    assert_eq!(ledger.jobs()[0].session, "s-a");
    let mut other = Ledger::default();
    restore_into(&mut other, carried(root, member), Some([8; 16]), &current);
    assert!(other.is_empty());
    restore_into(&mut other, carried(root, member), Some([7; 16]), &|pid| {
        Some(ProcessKey { pid, start: 99 })
    });
    assert!(other.is_empty());
}

#[test]
fn handoff_bounds_and_invalid_sessions_never_create_job_ownership() {
    let root = ProcessKey { pid: 20, start: 2 };
    let value = changed_carry(root, |job| job.session = "s-not-a-valid-id".into());
    let mut ledger = Ledger::default();
    restore_into(&mut ledger, value, Some([7; 16]), &|_| Some(root));
    assert!(ledger.is_empty());
    let value = changed_carry(root, |job| {
        job.members = vec![(20, 2); aterm_vitals::delegated::MAX_MEMBERS + 1];
    });
    restore_into(&mut ledger, value, Some([7; 16]), &|_| Some(root));
    assert!(ledger.is_empty());
}

#[cfg(unix)]
#[test]
fn native_receipt_inventory_is_order_independent_and_accepts_new_sessions() {
    use aterm_resource_host::resource_observation::{ProcessKey as NativeKey, SessionUsage};
    let host = NativeKey { pid: 100, start: 1 };
    let count = 4096u64;
    let receipts: Vec<_> = (0..count)
        .map(|i| SessionUsage {
            session: format!("s-{i:08x}"),
            reaped_cpu_ns: 1000 + i,
            unread: i % 3,
        })
        .collect();
    let previous: HashMap<_, _> = receipts
        .iter()
        .map(|receipt| {
            (
                receipt.session.clone(),
                (receipt.reaped_cpu_ns, receipt.unread),
            )
        })
        .collect();
    let mut state = State {
        native_host: Some(host),
        native_seq: count,
        native_receipts: previous.clone(),
        ..State::default()
    };
    let mut native = closed_native_source(host);
    native.sessions = receipts;
    native.sessions.reverse();
    native.sessions.rotate_left(1013);
    let increased = native.sessions[0].session.clone();
    native.sessions[0].reaped_cpu_ns += 10;
    native.sessions.push(SessionUsage {
        session: "s-ffffffff".into(),
        reaped_cpu_ns: 10,
        unread: 0,
    });
    native.next_seq = count + 1;
    native.terminals[0].seq = native.next_seq;
    native.terminals[0].session = "s-ffffffff".into();
    let mut observed = Observation::default();
    assert!(apply_native(&mut state, &mut observed, native.clone(), &[]));
    assert!(!observed.incomplete && !observed.source_lost);
    assert_eq!(state.native_seq, count + 1);
    assert_eq!(state.native_receipts.len(), previous.len() + 1);
    for (session, mut counters) in previous {
        if session == increased {
            counters.0 += 10;
        }
        assert_eq!(state.native_receipts.get(&session), Some(&counters));
    }
    assert_eq!(state.native_receipts["s-ffffffff"], (10, 0));
    assert_eq!(state.native_pending.len(), 1);
    assert_eq!(
        state.native_pending[&(host.pid, host.start, count + 1)].session,
        "s-ffffffff"
    );

    // An increasing unread counter is accepted, but still records historical CPU loss.
    native.terminals.clear();
    native.sessions[0].unread += 1;
    let unread_after = native.sessions[0].unread;
    let mut unread = Observation::default();
    assert!(apply_native(&mut state, &mut unread, native, &[]));
    assert!(unread.source_lost && !unread.incomplete);
    assert_eq!(state.native_receipts[&increased].1, unread_after);
    assert_eq!(state.native_pending.len(), 1, "no final is adopted twice");
}

#[cfg(unix)]
#[test]
fn native_receipts_reject_missing_or_decreasing_counters_before_adoption() {
    use aterm_resource_host::resource_observation::{ProcessKey as NativeKey, SessionUsage};
    let host = NativeKey { pid: 100, start: 1 };
    let previous = HashMap::from([
        ("s-acde1122".into(), (100, 2)),
        ("s-acde3344".into(), (200, 3)),
    ]);
    for missing in ["session", "cpu", "unread"] {
        let mut state = State {
            native_host: Some(host),
            native_seq: 2,
            native_lost_origins: 4,
            native_receipts: previous.clone(),
            native_outstanding: true,
            native_awaiting_observation: true,
            ..State::default()
        };
        let mut native = closed_native_source(host);
        native.next_seq = 3;
        native.terminals[0].seq = 3;
        native.lost_origins = 4;
        native.sessions = vec![
            SessionUsage {
                session: "s-acde1122".into(),
                reaped_cpu_ns: 100,
                unread: 2,
            },
            SessionUsage {
                session: "s-acde3344".into(),
                reaped_cpu_ns: 200,
                unread: 3,
            },
        ];
        match missing {
            "session" => {
                native.sessions.pop();
            }
            "cpu" => native.sessions[0].reaped_cpu_ns -= 1,
            "unread" => native.sessions[1].unread -= 1,
            _ => unreachable!(),
        }
        let mut observed = Observation::default();
        assert!(
            !apply_native(&mut state, &mut observed, native, &[]),
            "{missing}"
        );
        assert!(observed.incomplete, "{missing}");
        assert_eq!(state.native_receipts, previous, "{missing}");
        assert_eq!(state.native_seq, 2, "{missing}");
        assert_eq!(state.native_lost_origins, 4, "{missing}");
        assert!(state.native_pending.is_empty(), "{missing}");
        assert!(
            state.native_outstanding && state.native_awaiting_observation,
            "{missing}"
        );
        assert!(!state.native_source_lost, "{missing}");
    }
}

#[cfg(unix)]
#[test]
fn a_new_native_host_starts_new_receipt_counters() {
    use aterm_resource_host::resource_observation::ProcessKey as NativeKey;
    let old = NativeKey { pid: 100, start: 1 };
    let new = NativeKey { pid: 200, start: 2 };
    let mut state = State {
        native_host: Some(old),
        native_seq: 99,
        native_lost_origins: 4,
        native_receipts: HashMap::from([
            ("s-acde1122".into(), (1000, 7)),
            ("s-acde3344".into(), (2000, 8)),
        ]),
        native_tracking: true,
        native_tracking_cut: Some(0),
        ..State::default()
    };
    let mut observed = Observation::default();
    assert!(apply_native(
        &mut state,
        &mut observed,
        closed_native_source(new),
        &[]
    ));
    assert_eq!(state.native_host, Some(new));
    assert_eq!(state.native_seq, 1);
    assert_eq!(state.native_lost_origins, 0);
    assert_eq!(
        state.native_receipts,
        HashMap::from([("s-acde1122".into(), (10, 0))])
    );
    assert!(observed.source_lost && !observed.incomplete);
    assert_eq!(state.native_pending.len(), 1);
}

#[cfg(unix)]
#[test]
fn duplicate_native_receipts_reject_the_whole_observation_before_any_adoption() {
    use aterm_resource_host::resource_observation::{ProcessKey as NativeKey, SessionUsage};
    let old = NativeKey { pid: 100, start: 1 };
    let new = NativeKey { pid: 200, start: 2 };
    for host in [old, new] {
        for reversed in [false, true] {
            let previous = HashMap::from([("s-acde1122".into(), (1000, 0))]);
            let mut state = State {
                native_host: Some(old),
                native_seq: 99,
                native_lost_origins: 4,
                native_receipts: previous.clone(),
                native_tracking: true,
                native_tracking_cut: Some(7),
                native_outstanding: true,
                native_awaiting_observation: true,
                ..State::default()
            };
            let mut native = closed_native_source(host);
            if host == old {
                native.next_seq = 100;
                native.terminals[0].seq = 100;
                native.lost_origins = 4;
            }
            native.sessions = vec![
                SessionUsage {
                    session: "s-acde1122".into(),
                    reaped_cpu_ns: 1000,
                    unread: 0,
                },
                SessionUsage {
                    session: "s-acde1122".into(),
                    reaped_cpu_ns: 2000,
                    unread: 0,
                },
            ];
            if reversed {
                native.sessions.reverse();
            }
            let mut observed = Observation::default();
            assert!(!apply_native(&mut state, &mut observed, native, &[]));
            assert!(observed.incomplete);
            assert_eq!(state.native_host, Some(old));
            assert_eq!(state.native_seq, 99);
            assert_eq!(state.native_lost_origins, 4);
            assert_eq!(state.native_receipts, previous);
            assert_eq!(state.native_tracking_cut, Some(7));
            assert!(state.native_tracking);
            assert!(state.native_outstanding && state.native_awaiting_observation);
            assert!(!state.native_source_lost);
            assert!(state.native_pending.is_empty() && state.native_delivered.is_empty());
            assert!(observed.native_roots.is_empty() && observed.native_terminals.is_empty());
        }
    }
}

#[cfg(unix)]
#[test]
fn native_observations_keep_session_origin_and_consume_final_generations_once() {
    use aterm_resource_host::resource_observation::{
        Observation as Native, Original, ProcessKey as NativeKey, Terminal,
    };
    let host = NativeKey { pid: 100, start: 1 };
    let root = NativeKey { pid: 20, start: 2 };
    let mut native = Native {
        host,
        host_child_cpu_ns: Some(0),
        at_unix_ms: 1000,
        originals: vec![Original {
            session: "s-a".into(),
            process: root,
            cpu_ns: Some(10),
            child_cpu_ns: Some(0),
            reaps_unwaited: Some(false),
            footprint: Some(1024),
            peak_footprint: Some(2048),
        }],
        sessions: Vec::new(),
        outstanding_sessions: Vec::new(),
        terminals: Vec::new(),
        first_seq: 1,
        next_seq: 0,
        unproven_active: 0,
        lost_origins: 0,
    };
    let mut state = State::default();
    let mut observed = Observation {
        procs: Some(vec![
            aterm_vitals::model::ProcSample {
                pid: host.pid,
                start: Some(host.start),
                child_cpu_ns: Some(0),
                ..Default::default()
            },
            aterm_vitals::model::ProcSample {
                pid: root.pid,
                start: Some(root.start),
                cpu_ns: Some(7),
                child_cpu_ns: Some(0),
                at_ns: Some(100),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };
    let tabs = [(1, "s-a".into()), (2, "s-b".into())];
    apply_native(&mut state, &mut observed, native.clone(), &tabs);
    assert_eq!(observed.native_roots[0].tab, Some(1));
    assert!(
        observed
            .native_roots
            .iter()
            .any(|root| root.process.pid == host.pid && root.tab.is_none()),
        "the authenticated cached custodian belongs to aterm even when its image is ad hoc"
    );
    let mut reused = Observation {
        procs: observed.procs.clone(),
        ..Default::default()
    };
    reused
        .procs
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|process| process.pid == host.pid)
        .unwrap()
        .start = Some(host.start + 1);
    apply_native(&mut state, &mut reused, native.clone(), &tabs);
    assert!(
        reused
            .native_roots
            .iter()
            .all(|root| root.process.pid != host.pid),
        "a reused PID has no custodian proof"
    );
    assert!(observed.native_terminals.is_empty());
    assert_eq!(
        observed
            .procs
            .as_ref()
            .unwrap()
            .iter()
            .find(|p| p.pid == 20)
            .unwrap()
            .cpu_ns,
        Some(7),
        "a later native snapshot must not overwrite the earlier CPU source"
    );
    native.originals.clear();
    native.next_seq = 1;
    native.host_child_cpu_ns = Some(35);
    native.terminals.push(Terminal {
        seq: 1,
        session: "s-a".into(),
        process: root,
        uid: Some(0),
        coalition: None,
        version: None,
        cpu_ns: Some(30),
        child_cpu_ns: Some(5),
        wait_cpu_ns: Some(35),
        reaps_unwaited: Some(false),
        at_unix_ms: 2000,
        exit_cut_ns: Some(2000),
    });
    let mut next = Observation {
        procs: Some(vec![aterm_vitals::model::ProcSample {
            // PID reuse is real current work and must remain separate from the original final.
            pid: root.pid,
            start: Some(root.start + 1),
            cpu_ns: Some(7),
            ..Default::default()
        }]),
        ..Default::default()
    };
    apply_native(&mut state, &mut next, native.clone(), &tabs);
    deliver_native(&mut state, &mut next, &tabs, native_cut(1999));
    assert!(
        next.native_terminals.is_empty(),
        "a later exit proof belongs to the next source window"
    );
    assert_eq!(state.native_pending.len(), 1);
    let carried = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        origins: Vec::new(),
        origins_incomplete: false,
        native: Some(NativeCarry {
            pid: Some(host.pid),
            start: Some(host.start),
            seq: state.native_seq,
            lost_origins: 0,
            pending: state.native_pending.values().cloned().collect(),
            queue_lost: 0,
            tracking_cut: None,
            tracking: false,
            outstanding: false,
            awaiting_observation: false,
            observer: None,
            source_lost: false,
            cpu_cut: state.native_cpu_cut.into(),
        }),
    })
    .expect("an undelivered final must survive the actual TOML-compatible handoff encoding");
    let carried: Carry = aterm_json::from_value(carried).unwrap();
    state.native_pending.clear();
    for pending in carried.native.unwrap().pending {
        state
            .native_pending
            .insert((pending.host_pid, pending.host_start, pending.seq), pending);
    }
    // A failed later RPC does not erase already received original final generations.
    deliver_native(&mut state, &mut next, &tabs, native_cut(2000));
    assert_eq!(next.native_terminals.len(), 1);
    assert_eq!(next.native_terminals[0].tab, Some(1));
    assert_eq!(next.native_terminals[0].process.start, Some(2));
    let first_window = next.native_terminals[0].timing;
    assert_eq!(first_window.previous, native_cut(1999));
    assert_eq!(first_window.current, native_cut(2000));
    assert_eq!(first_window.exited_ns, Some(2000));
    let encoded = encode_carry(Carry {
        boot: [7; 16],
        jobs: Vec::new(),
        origins: Vec::new(),
        origins_incomplete: false,
        native: carry_native(&state, 0, 0),
    })
    .unwrap();
    let carried: Carry = aterm_json::from_value(encoded).unwrap();
    let mut transferred = State::default();
    restore_native(&mut transferred, carried.native.unwrap(), &|_| None);
    assert_eq!(transferred.native_cpu_cut, native_cut(2000));
    let mut after_handoff = Observation::default();
    deliver_native(
        &mut transferred,
        &mut after_handoff,
        &tabs,
        native_cut(4000),
    );
    assert_eq!(
        after_handoff.native_terminals[0].timing, first_window,
        "handoff cannot move a historical final into a later CPU window"
    );
    assert_eq!(
        next.native_terminals[0].process.uid,
        Some(0),
        "OS identity survives the native wire and handoff without substituting our UID"
    );
    assert_eq!(next.procs.as_ref().unwrap()[0].start, Some(3));
    let mut retained = Observation::default();
    deliver_native(&mut state, &mut retained, &tabs, native_cut(3000));
    assert_eq!(
        retained.native_terminals.len(),
        1,
        "delivery retains custody until a whole model sweep adopts it"
    );
    assert_eq!(
        retained.native_terminals[0].timing, first_window,
        "redelivery retains the first eligible receipt window"
    );
    native_model_pending_into(&mut state, false);
    let mut again = Observation::default();
    native.terminals.clear();
    apply_native(&mut state, &mut again, native, &tabs);
    deliver_native(&mut state, &mut again, &tabs, native_cut(3000));
    assert!(
        again.native_terminals.is_empty(),
        "the same terminal cursor is consumed once"
    );
}
