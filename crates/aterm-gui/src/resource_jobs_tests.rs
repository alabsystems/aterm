// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn carried(root: ProcessKey, member: ProcessKey) -> aterm_json::Value {
    aterm_json::to_value(Carry {
        boot: [7; 16],
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
    let complete = observed.procs.is_some();
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
        ..Default::default()
    });
    publish_into(
        state,
        complete.then(|| snapshot.delegated.clone()),
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
            assert!(
                partial.aterm.is_none(),
                "{root_live}/{failure}/fresh={fresh_available} cannot publish reduced process totals"
            );
            assert!(partial.delegated.is_empty());
            assert_eq!(state.ledger.bindings(), old_bindings);
            assert_eq!(state.usage, old_usage);
            assert_eq!(state.sampled, old_age);
            assert!(!state.current_complete);
            let recovered = sampled(&mut state, &mut vitals, &healthy(4), &lookup, 4, epoch);
            let count = if root_live { 2 } else { 1 };
            assert!(state.current_complete);
            assert_eq!(state.ledger.bindings(), old_bindings);
            assert_eq!(state.usage.len(), 1);
            assert_eq!(state.usage[0].procs, count);
            assert_eq!(recovered.aterm.as_ref().unwrap().procs, count);
            assert_eq!(recovered.tab(1).unwrap().scope.procs, count);
            assert_eq!(
                state.sampled,
                Some(epoch + std::time::Duration::from_secs(4))
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
