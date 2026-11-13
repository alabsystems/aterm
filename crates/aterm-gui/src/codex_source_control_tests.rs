// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
// Ledger metadata only: this does not construct a qualified source Peer.
pub(super) fn caller() -> Caller {
    let pid = std::process::id();
    let process = Process::observe(pid).unwrap();
    Caller {
        caller: slot::Caller {
            sid: "replay-fixture".into(),
            pid,
            birth: process.birth,
        },
        process,
        start: restart_stamp::kernel_start(pid).unwrap(),
    }
}
fn request(counter: u64) -> Request {
    Request::parse(&format!(
        "member-open schema=1 request=1234567890abcdef{counter:016x} operation={counter:032x}"
    ))
    .unwrap()
}
#[test]
fn retirement_fence_never_holds_progress_while_waiting_for_context_preparation() {
    let caller = caller();
    let request = Request::parse(&format!(
        "member-preload-use schema=1 request=1234567890abcdef0000000000000001 operation={} member={} preload_sequence=0123456789abcdef0000000000000001 use_member={} use_operation=11111111111111110000000000000001 capability={}",
        "a".repeat(32),"b".repeat(32),"b".repeat(32),"c".repeat(64),
    )).unwrap();
    let mut jobs = Jobs::default();
    let (job, _) = jobs.admit(9901, &caller, &request).unwrap();
    let producer = job.prepared_context.lock().unwrap();
    let (arrived, arrival) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel();
    let member = request.member.clone().unwrap();
    let operation = request.operation.clone();
    let fence = std::thread::spawn(move || {
        let captures = jobs.fence_member_preload_at(&caller, &member, &operation, || {
            arrived.send(()).unwrap();
            resumed.recv().unwrap();
        });
        drop(captures);
    });
    arrival.recv().unwrap();
    // This is the producer's actual lock order. A fence which took progress
    // first would fail this assertion instead of hanging the test worker.
    assert!(matches!(
        *job.progress
            .try_lock()
            .expect("fence must not hold progress before acquiring context"),
        Progress::Waiting
    ));
    resume.send(()).unwrap();
    drop(producer);
    fence.join().unwrap();
    assert!(matches!(
        *job.progress.lock().unwrap(),
        Progress::Complete(Err(registry::Error::Stale))
    ));
}
#[test]
fn peer_hello_is_metadata_only_on_actual_socket_even_after_app_drain() {
    use std::os::fd::AsRawFd;
    let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
    let _fd = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let _direct = crate::control::DirectPeer::enter();
    let lifetime = account_work::Lifetime::default();
    let registry = registry::Registry::for_app(&lifetime);
    let drained = lifetime.pause().unwrap().wait();
    let line = format!(
        "member-peer schema=1 request=0123456789abcdef0000000000000001 operation={}",
        "f".repeat(32)
    );
    let request = Request::parse(&line).unwrap();
    for extra in [
        format!(" member={}", "a".repeat(32)),
        " source=1".into(),
        " open_kind=ordinary".into(),
    ] {
        assert!(Request::parse(&format!("{line}{extra}")).is_none());
    }
    let id = u64::MAX - 88003;
    let response = serve_peer_identified(request.clone(), &registry, id, || {
        identify_for_session("source-peer-metadata")
    });
    let body = response.strip_prefix("OK codex-auth member-peer ").unwrap();
    let value: aterm_json::Value = aterm_json::from_str(body.trim()).unwrap();
    assert!(value["member"].is_null());
    assert_eq!(
        value["session"]["process_pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    assert!(hex(value["session"]["epoch"].as_str().unwrap(), 32));
    assert_eq!(value.as_object().unwrap().len(), 5);
    assert!(lifetime.enter().is_none());
    assert_eq!(
        serve_peer_identified(request, &registry, id, || identify_for_session(
            "source-peer-metadata"
        )),
        response
    );
    let changed = Request::parse(&line.replace(&"f".repeat(32), &"e".repeat(32))).unwrap();
    assert_eq!(
        serve_peer_identified(changed, &registry, id, || identify_for_session(
            "source-peer-metadata"
        )),
        "ERR codex-auth request changed\n"
    );
    registry.connection_closed(id);
    connection_closed(id);
    drained.resume().unwrap();
}
fn target_ready_line(counter: u64) -> String {
    format!(
        "member-ready-target schema=1 preload_sequence=0123456789abcdef0000000000000001 request=1234567890abcdef{counter:016x} operation={} member={} assignment={} context={} assignment_generation=9 registration={} credential_generation=13 digest={} account={} user={} auth_generation=3 owner_generation=5 source_generation=7 login_generation=11",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(32),
        "f".repeat(64),
        "1".repeat(64),
        "2".repeat(64)
    )
}
#[test]
fn target_ready_grammar_keeps_loaded_reader_unavailable_and_actual_generation_distinct() {
    let manager = target_ready_line(1);
    assert_eq!(
        Request::parse(&manager).unwrap().name(),
        "member-ready-target"
    );
    let auth = " auth_generation=3 owner_generation=5 source_generation=7 login_generation=11";
    let reader = manager
        .replace("member-ready-target", "member-reader-ready-target")
        .replace(auth, "");
    assert_eq!(
        Request::parse(&reader).unwrap().name(),
        "member-reader-ready-target"
    );
    let unavailable = format!(
        "member-unavailable-target schema=1 preload_sequence=0123456789abcdef0000000000000001 request=1234567890abcdef0000000000000001 operation={} member={} assignment={} context={} assignment_generation=9 registration={}",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(32)
    );
    assert_eq!(
        Request::parse(&unavailable).unwrap().name(),
        "member-unavailable-target"
    );
    for invalid in [
        manager.replace(" preload_sequence=0123456789abcdef0000000000000001", ""),
        manager.replace(
            "preload_sequence=0123456789abcdef0000000000000001",
            "preload_sequence=0123456789abcdef0000000000000000",
        ),
        manager.replace(" assignment_generation=9", ""),
        manager.replace("assignment_generation=9", "assignment_generation=0"),
        manager.replace("assignment_generation=9", "assignment_generation=09"),
        manager.replace("credential_generation=13", "credential_generation=0"),
        manager.replace(auth, ""),
        format!("{reader}{auth}"),
        format!("{unavailable} credential_generation=13"),
        format!("{manager} source_path=/tmp/auth.json"),
    ] {
        assert!(Request::parse(&invalid).is_none());
    }
}
#[test]
fn owned_retirement_requires_exact_sequence_and_no_reader_has_no_sequence() {
    let line = format!(
        "member-retire-owned schema=1 request=1234567890abcdef0000000000000001 operation={} member={} preload_operation={} preload_sequence=0123456789abcdef0000000000000001 registration=none",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
    );
    let parsed = Request::parse(&line).unwrap();
    assert!(matches!(
        &parsed.action,
        Action::RetireOwned {
            registration: None,
            ..
        }
    ));
    let caller = caller();
    let response = completed_envelope(&parsed, &caller, "epoch", parsed.member.as_deref());
    assert_eq!(
        response["preload_sequence"].as_str(),
        Some("0123456789abcdef0000000000000001")
    );
    for invalid in [
        line.replace(" preload_sequence=0123456789abcdef0000000000000001", ""),
        line.replace(
            "preload_sequence=0123456789abcdef0000000000000001",
            "preload_sequence=0123456789abcdef0000000000000000",
        ),
        format!("{line} context={}", "d".repeat(64)),
    ] {
        assert!(Request::parse(&invalid).is_none());
    }
    let unrelated = format!(
        "member-unrelated schema=1 request=1234567890abcdef0000000000000002 operation={} member={} kind=api_key",
        "a".repeat(32),
        "b".repeat(32)
    );
    assert!(Request::parse(&unrelated).is_some());
    assert!(
        Request::parse(&format!(
            "{unrelated} preload_sequence=0123456789abcdef0000000000000001"
        ))
        .is_none()
    );
}
#[test]
fn accepted_member_fence_closes_only_matching_queued_readiness_and_never_running_work() {
    // Queue-state control only. No native custody or positive source
    // admission is constructed by these transport metadata fixtures.
    let caller = caller();
    let queued = Request::parse(&target_ready_line(1)).unwrap();
    let running = Request::parse(&target_ready_line(2)).unwrap();
    let other =
        Request::parse(&target_ready_line(3).replace(&"b".repeat(32), &"9".repeat(32))).unwrap();
    let mut jobs = Jobs::default();
    let (queued_job, _) = jobs.admit(1, &caller, &queued).unwrap();
    let (running_job, _) = jobs.admit(1, &caller, &running).unwrap();
    running_job.target_accounted.store(true, Ordering::Release);
    assert!(jobs.reserve_ready(running_job.clone()).1);
    let (other_job, _) = jobs.admit(1, &caller, &other).unwrap();
    let released = jobs.fence_member_preload(
        &caller,
        queued.member.as_deref().unwrap(),
        &queued.operation,
    );
    drop(released);
    assert!(matches!(
        *queued_job.progress.lock().unwrap(),
        Progress::Complete(Err(registry::Error::Stale))
    ));
    assert!(matches!(
        *running_job.progress.lock().unwrap(),
        Progress::Running { .. }
    ));
    assert!(matches!(
        *other_job.progress.lock().unwrap(),
        Progress::Waiting
    ));
    running_job.finish(Err(registry::Error::Stale));
}
#[test]
fn target_ready_cannot_queue_before_actual_native_capture() {
    let caller = caller();
    let request = Request::parse(&target_ready_line(1)).unwrap();
    let mut jobs = Jobs::default();
    let (job, started) = jobs.admit(1, &caller, &request).unwrap();
    assert!(!started);
    assert!(!jobs.reserve_ready(job).1);
    assert_eq!(jobs.cleanup_running.load(Ordering::Acquire), 0);
    assert_eq!(jobs.running.load(Ordering::Acquire), 0);
}
#[test]
fn target_ready_negative_uses_actual_socket_identity_without_enter_or_foreground() {
    use std::os::fd::AsRawFd;
    let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
    let _fd = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let _direct = crate::control::DirectPeer::enter();
    let lifetime = account_work::Lifetime::default();
    let registry = Arc::new(registry::Registry::for_app(&lifetime));
    let drained = lifetime.pause().unwrap().wait();
    assert!(lifetime.enter().is_none());
    let id = u64::MAX - 88002;
    let line = ready_control::serve_identified(
        Request::parse(&target_ready_line(1)).unwrap(),
        &registry,
        id,
        || identify_for_session("source-ready-kernel-negative"),
    );
    // No native target/SDK image was admitted. Exact native Stale, rather
    // than foreground/enter refusal, proves only this negative boundary.
    assert_eq!(line, "ERR codex-auth stale\n");
    registry.connection_closed(id);
    connection_closed(id);
    drained.resume().unwrap();
}

#[test]
fn quiescence_preserves_the_distinction_between_reader_and_manager() {
    let reader = format!(
        "member-quiesce schema=1 request=1234567890abcdef0000000000000001 operation={} member={} capability={} registration={} credential_generation=1 digest={} role=reader",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(64),
        "d".repeat(32),
        "e".repeat(64),
    );
    assert!(matches!(
        Request::parse(&reader).unwrap().action,
        Action::Quiesce { auth: None, .. }
    ));
    let generations =
        " auth_generation=1 owner_generation=2 source_generation=3 login_generation=4";
    let manager = reader.replace("role=reader", "role=manager");
    assert!(Request::parse(&manager).is_none());
    assert!(Request::parse(&format!("{reader}{generations}")).is_none());
    assert!(matches!(
        Request::parse(&format!("{manager}{generations}"))
            .unwrap()
            .action,
        Action::Quiesce { auth: Some(_), .. }
    ));
    assert!(
        Request::parse(&reader.replace("credential_generation=1", "credential_generation=0"))
            .is_none()
    );
}

#[test]
fn census_cursor_is_complete_and_cannot_supply_source_authority() {
    let line = format!(
        "member-census schema=1 request=1234567890abcdef0000000000000001 operation={} member={} capability={}",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(64),
    );
    assert!(Request::parse(&line).is_some());
    let revision = format!(" revision={}", "d".repeat(64));
    let after = format!(" after={}", "e".repeat(32));
    assert!(Request::parse(&format!("{line}{revision}{after}")).is_some());
    for changed in [
        format!("{line}{revision}"),
        format!("{line}{after}"),
        format!("{line} source_path=/tmp/auth.json"),
        line.replace(&"c".repeat(64), &"c".repeat(63)),
    ] {
        assert!(Request::parse(&changed).is_none());
    }
}

#[test]
fn reservation_uses_closed_assignment_fields_and_requires_loaded_member_identity() {
    let line = format!(
        "member-reserve schema=1 request=1234567890abcdef0000000000000001 operation={} member={} pool_revision={} expected_assignment=0 selection=auto",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(64),
    );
    let request = Request::parse(&line).unwrap();
    assert!(matches!(request.action, Action::Reserve(_)));
    for changed in [
        line.replace("expected_assignment=0", "expected_assignment=00"),
        line.replace("selection=auto", "selection=unknown"),
        line.replace(&format!(" member={}", "b".repeat(32)), ""),
        format!("{line} source_path=/tmp/auth.json"),
        format!("{line} selection=auto"),
    ] {
        assert!(Request::parse(&changed).is_none());
    }
}

#[test]
fn completed_requests_do_not_exhaust_worker_capacity_and_evicted_ids_never_execute() {
    let caller = caller();
    let mut jobs = Jobs::default();
    for counter in 1..=192 {
        let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
        assert!(start);
        job.finish(Ok("fixture".into()));
    }
    jobs.reap();
    let connection = &jobs.connections[&1];
    assert!(connection.active.is_empty());
    assert_eq!(connection.recent.len(), RECENT_PER_CONNECTION);
    assert_eq!(connection.highwater, 192);
    assert!(matches!(
        jobs.admit(1, &caller, &request(1)),
        Err(AdmissionError::Stale)
    ));
    let mut changed = request(1);
    changed.operation = "f".repeat(32);
    assert!(matches!(
        jobs.admit(1, &caller, &changed),
        Err(AdmissionError::Stale)
    ));
}
#[test]
fn busy_retry_retains_exact_body_and_old_active_requests_survive_newer_counters() {
    let caller = caller();
    let mut jobs = Jobs::default();
    let original = request(1);
    let (job, start) = jobs.admit(1, &caller, &original).unwrap();
    assert!(start);
    let (same, start) = jobs.admit(1, &caller, &original).unwrap();
    assert!(!start && Arc::ptr_eq(&job, &same));
    job.finish(Err(registry::Error::Busy));
    for counter in 2..=32 {
        let (next, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
        assert!(start);
        next.finish(Ok("fixture".into()));
    }
    let mut changed = original.clone();
    changed.operation = "f".repeat(32);
    assert!(matches!(
        jobs.admit(1, &caller, &changed),
        Err(AdmissionError::Changed)
    ));
    let (same, start) = jobs.admit(1, &caller, &original).unwrap();
    assert!(start && Arc::ptr_eq(&job, &same));
    same.finish(Ok("result".into()));
    assert!(!jobs.admit(1, &caller, &original).unwrap().1);
}
#[test]
fn capacity_refuses_before_acceptance_and_eof_cannot_resurrect_completed_workers() {
    let caller = caller();
    let mut jobs = Jobs::default();
    let mut first = None;
    for counter in 1..=MAX_PENDING as u64 {
        let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
        assert_eq!(start, counter <= MAX_RUNNING as u64);
        if counter == 1 {
            first = Some(job);
        }
    }
    assert!(matches!(
        jobs.admit(1, &caller, &request(257)),
        Err(AdmissionError::Capacity)
    ));
    assert_eq!(jobs.connections[&1].highwater, 256);
    let first = first.unwrap();
    first.finish(Ok("fixture".into()));
    assert!(jobs.admit(1, &caller, &request(257)).unwrap().1);
    drop(jobs.closed(1));
    first.finish(Ok("late".into()));
    assert!(jobs.connections.is_empty());
    assert!(jobs.admit(2, &caller, &request(1)).unwrap().1);
}
#[test]
fn eof_does_not_release_running_capacity_before_the_old_worker_finishes() {
    let caller = caller();
    let mut jobs = Jobs::default();
    let mut workers = Vec::new();
    for counter in 1..=MAX_RUNNING as u64 {
        let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
        assert!(start);
        workers.push(job);
    }
    drop(jobs.closed(1));
    let (next, start) = jobs.admit(2, &caller, &request(1)).unwrap();
    assert!(!start);
    assert_eq!(jobs.running.load(Ordering::Acquire), MAX_RUNNING);
    workers.pop().unwrap().finish(Ok("retired".into()));
    let (same, start) = jobs.admit(2, &caller, &request(1)).unwrap();
    assert!(start && Arc::ptr_eq(&same, &next));
    assert_eq!(jobs.running.load(Ordering::Acquire), MAX_RUNNING);
    for worker in workers {
        worker.finish(Ok("retired".into()));
    }
    next.finish(Ok("retired".into()));
    assert_eq!(jobs.running.load(Ordering::Acquire), 0);
}
#[test]
fn sequence_gaps_prefix_changes_and_exhaustion_fail_closed() {
    let caller = caller();
    let mut jobs = Jobs::default();
    jobs.admit(1, &caller, &request(10)).unwrap();
    assert!(matches!(
        jobs.admit(1, &caller, &request(9)),
        Err(AdmissionError::Stale)
    ));
    let mut changed = request(11);
    changed.sequence.prefix ^= 1;
    assert!(matches!(
        jobs.admit(1, &caller, &changed),
        Err(AdmissionError::Stale)
    ));
    assert!(jobs.admit(1, &caller, &request(u64::MAX)).is_ok());
    assert!(Sequence::parse("1234567890abcdef0000000000000000").is_none());
}
#[test]
fn private_member_requests_separate_request_nonce_from_snapshot_and_refuse_extra_authority() {
    let base = format!(
        "schema=1 request={} operation={}",
        "a".repeat(32),
        "b".repeat(32)
    );
    assert!(Request::parse(&format!("member-open {base}")).is_some());
    for suffix in [
        " member=cc",
        " path=/auth.json",
        " launch=secret",
        " schema=1",
    ] {
        assert!(Request::parse(&format!("member-open {base}{suffix}")).is_none());
    }
    let ready = format!(
        "member-ready {base} preload_sequence=0123456789abcdef0000000000000001 member={} registration={} credential_generation=3 digest={} account={} user={} auth_generation=7 owner_generation=11 source_generation=13 login_generation=17",
        "c".repeat(32),
        "d".repeat(32),
        "e".repeat(64),
        "f".repeat(64),
        "0".repeat(64)
    );
    assert!(Request::parse(&ready).is_some());
    assert!(
        Request::parse(&ready.replace("credential_generation=3", "credential_generation=0"))
            .is_none()
    );
    assert!(Request::parse(&ready.replace("auth_generation=7", "auth_generation=07")).is_none());
    assert!(Request::parse(&format!("{ready} boundary={}", "1".repeat(32))).is_none());
    let updated = ready.replace(&"a".repeat(32), &"2".repeat(32));
    let first = Request::parse(&ready).unwrap();
    let next = Request::parse(&updated).unwrap();
    assert!(first.request != next.request && first.operation == next.operation);
}
#[test]
fn direct_reader_ready_never_invents_or_accepts_manager_counters() {
    let reader = format!(
        "member-reader-ready schema=1 preload_sequence=0123456789abcdef0000000000000001 request={} operation={} member={} registration={} credential_generation=3 digest={} account={} user={}",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
        "d".repeat(32),
        "e".repeat(64),
        "f".repeat(64),
        "0".repeat(64)
    );
    let parsed = Request::parse(&reader).unwrap();
    assert_eq!(parsed.name(), "member-reader-ready");
    assert!(matches!(
        parsed.action,
        Action::ReaderReady { generation: 3, .. }
    ));
    assert!(Request::parse(&reader.replacen("member-reader-ready", "member-ready", 1)).is_none());
    for extra in [
        " auth_generation=0 owner_generation=0 source_generation=0 login_generation=0",
        " path=/auth.json",
        " credential_generation=3",
    ] {
        assert!(Request::parse(&format!("{reader}{extra}")).is_none());
    }
    for generation in ["0", "03"] {
        assert!(
            Request::parse(&reader.replace(
                "credential_generation=3",
                &format!("credential_generation={generation}")
            ))
            .is_none()
        );
    }
}

#[test]
fn unrelated_modes_and_unloaded_retirement_do_not_claim_a_source() {
    let base = format!(
        "schema=1 request={} operation={} member={}",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32)
    );
    assert!(matches!(
        Request::parse(&format!("member-retire {base} registration=none"))
            .unwrap()
            .action,
        Action::Retire { registration: None }
    ));
    assert!(Request::parse(&format!("member-unrelated {base} kind=api_key")).is_some());
    assert!(Request::parse(&format!("member-unrelated {base} kind=chatgpt")).is_none());
    assert!(
        Request::parse(&format!(
            "member-unrelated {base} kind=api_key credential_generation=0"
        ))
        .is_none()
    );
}
fn use_request(counter: u64, verb: &str) -> Request {
    let extra = if verb == "member-use-end" {
        format!("capability={}", "c".repeat(64))
    } else {
        format!("registration={}", "b".repeat(32))
    };
    Request::parse(&format!("{verb} schema=1 request=1234567890abcdef{counter:016x} operation=aaaaaaaaaaaaaaaa0000000000000001 member={} {extra}", "a".repeat(32))).unwrap()
}

#[test]
fn use_wire_is_closed_and_semantic_nonce_is_distinct_from_rpc_sequence() {
    for verb in ["member-use-begin", "member-use-end", "member-use-close"] {
        let request = use_request(7, verb);
        assert_eq!(request.name(), "member-use");
        assert_eq!(request.cleanup(), verb != "member-use-begin");
        let extra = if verb == "member-use-end" {
            format!("capability={}", "c".repeat(64))
        } else {
            format!("registration={}", "b".repeat(32))
        };
        let line = format!(
            "{verb} schema=1 request=1234567890abcdef0000000000000007 operation=aaaaaaaaaaaaaaaa0000000000000001 member={} {extra}",
            "a".repeat(32)
        );
        assert!(Request::parse(&format!("{line} unexpected=1")).is_none());
        assert!(
            Request::parse(&line.replace(
                "aaaaaaaaaaaaaaaa0000000000000001",
                "aaaaaaaaaaaaaaaa0000000000000000"
            ))
            .is_none()
        );
        assert!(Request::parse(&format!("{line} {extra}")).is_none());
    }
}

#[test]
fn full_begin_worker_budget_cannot_starve_release_and_replay_body_is_shared() {
    let caller = caller();
    let mut jobs = Jobs::default();
    jobs.running.store(MAX_RUNNING, Ordering::Release);
    let (begin, start) = jobs
        .admit(1, &caller, &use_request(1, "member-use-begin"))
        .unwrap();
    assert!(!start);
    let release_request = use_request(2, "member-use-end");
    let (release, start) = jobs.admit(1, &caller, &release_request).unwrap();
    assert!(!start, "native preparation precedes worker admission");
    use_control::prepare_before_waiting(&release, || {
        Ok(registry::UseCommand::Settled(registry::UseProgress::Closed))
    })
    .unwrap();
    let (_, start) = jobs.reserve_ready(Arc::clone(&release));
    assert!(start);
    assert_eq!(jobs.cleanup_running.load(Ordering::Acquire), 1);
    assert!(matches!(
        jobs.admit(1, &caller, &use_request(2, "member-use-close")),
        Err(AdmissionError::Changed)
    ));
    release.finish(Ok("closed-fixture".into()));
    assert_eq!(jobs.cleanup_running.load(Ordering::Acquire), 0);
    assert!(use_control::prepare_before_waiting(&begin, || Err(registry::Error::Busy)).is_err());
    assert!(
        begin.prepared_use.lock().unwrap().is_none(),
        "a backend refusal cannot become an accepted Waiting receipt"
    );
}

#[test]
fn use_closed_frame_contains_no_fabricated_registration_or_revision() {
    let request = use_request(1, "member-use-close");
    let line = use_control::response(
        &request,
        &caller(),
        "test-epoch",
        registry::UseProgress::Closed,
    )
    .unwrap();
    let body = line.strip_prefix("OK codex-auth member-use ").unwrap();
    let value: aterm_json::Value = aterm_json::from_str(body).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 7);
    assert_eq!(
        object.get("state").and_then(aterm_json::Value::as_str),
        Some("closed")
    );
    assert!(object.get("capability").unwrap().is_null());
    assert!(
        !object.contains_key("registration")
            && !object.contains_key("generation")
            && !object.contains_key("digest")
    );
}
#[test]
fn full_begin_pending_table_still_accepts_release_without_partitioning_rpc_replay() {
    let caller = caller();
    let mut jobs = Jobs::default();
    for counter in 1..=MAX_PENDING as u64 {
        assert!(
            !jobs
                .admit(1, &caller, &use_request(counter, "member-use-begin"))
                .unwrap()
                .1
        );
    }
    assert!(matches!(
        jobs.admit(
            1,
            &caller,
            &use_request(MAX_PENDING as u64 + 1, "member-use-begin")
        ),
        Err(AdmissionError::Capacity)
    ));
    let counter = MAX_PENDING as u64 + 2;
    let request = use_request(counter, "member-use-close");
    let (release, _) = jobs.admit(1, &caller, &request).unwrap();
    use_control::prepare_before_waiting(&release, || {
        Ok(registry::UseCommand::Settled(registry::UseProgress::Closed))
    })
    .unwrap();
    assert!(jobs.reserve_ready(Arc::clone(&release)).1);
    assert!(matches!(
        jobs.admit(1, &caller, &use_request(counter, "member-use-end")),
        Err(AdmissionError::Changed)
    ));
    assert!(matches!(
        jobs.admit(
            1,
            &caller,
            &use_request(MAX_PENDING as u64 + 1, "member-use-begin")
        ),
        Err(AdmissionError::Stale)
    ));
}
#[test]
fn actual_direct_socket_cleanup_reaches_retained_identity_without_foreground_or_enter() {
    use std::os::fd::AsRawFd;
    let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
    let _fd = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    assert!(matches!(
        identify_for_session("source-kernel-negative"),
        Err(registry::Error::Denied)
    ));
    let _direct = crate::control::DirectPeer::enter();
    let actual = identify_for_session("source-kernel-negative").unwrap();
    assert_eq!(actual.caller.pid, std::process::id());
    let lifetime = account_work::Lifetime::default();
    let registry = Arc::new(registry::Registry::for_app(&lifetime));
    let drained = lifetime.pause().unwrap().wait();
    assert!(lifetime.enter().is_none());
    let id = u64::MAX - 88001;
    let line =
        use_control::serve_identified(use_request(1, "member-use-end"), &registry, id, || {
            identify_for_session("source-kernel-negative")
        });
    // The real test binary has NO admitted source member/image. Reaching
    // retained-member Stale proves cleanup skipped generic foreground and
    // ordinary enter; it grants no positive Source/Peer authority.
    assert_eq!(line, "ERR codex-auth stale\n");
    registry.connection_closed(id);
    connection_closed(id);
    drained.resume().unwrap();
}
