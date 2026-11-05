// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless projection controls for typed delivered outcomes and retained identity.

use super::*;
use crate::allocation_monitor_host::{Phase, Presentation, Reading};
use crate::native_app::ViewMotionCx;
use crate::native_ui::{CompiledUi, LogicalRect, UiKey};
use aterm_capacity::{
    cohort_namespace::Namespace,
    host_protocol::{Epoch, Generation, Job, Phase as JobPhase},
    leadership::Fence,
};
use atpkg::build_admit::service::{
    Gap, Owner,
    monitor::{EventKind, Observation, Progress, RecordSummary},
};
use std::{collections::BTreeSet, os::unix::fs::DirBuilderExt as _};

fn observation() -> Observation {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace = Namespace::create_private(world.path(), &home).unwrap();
    let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
    let owner = Owner::fenced(fence, 1, 4).unwrap();
    let source = owner.monitor_source().unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (client, mut exchange) = source.pair(deadline).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(move || -> std::io::Result<()> {
            while exchange.step()? == Progress::Pending {
                exchange.wait(deadline)?;
            }
            Ok(())
        });
        let result = client.read_until(deadline);
        server.join().unwrap().unwrap();
        result.unwrap()
    })
}

fn app(observation: Observation, recent: Vec<RecordSummary>) -> AllocationApp {
    AllocationApp {
        monitor: Some(Arc::new(Presentation {
            requested: 1,
            desired_generation: Some(observation.snapshot.generation),
            phase: Phase::Observed,
            last: Some(Reading {
                attachment: 1,
                observation: Arc::new(observation),
                last_gap: None,
                recent: recent.into(),
                omitted_terminals: 0,
            }),
        })),
        ..AllocationApp::default()
    }
}

fn compiled(app: &AllocationApp) -> CompiledUi {
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, 500.0, 400.0),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.view(&AllocationViewState::default(), &cx)
        .compile(cx.viewport)
        .unwrap()
}

fn epoch(sequence: u64) -> Epoch {
    Epoch {
        sequence,
        backing: 4,
        roots: 2,
        reserved_bytes: 3 * 1024 * 1024 * 1024,
        opened: true,
        retired: false,
        helper_exited: false,
        control_eof: false,
        endpoint_cleaned: false,
        settled_units: 0,
        uncertain_units: 0,
        remaining_units: 4,
        completed_roots: 0,
        failed_roots: 0,
        faulted: false,
        unbound: false,
        not_spawned: false,
        cancellation_started: false,
        failure: None,
        cache_work: Vec::new(),
    }
}

#[test]
fn cache_work_keeps_exact_run_unavailability_and_original_attachment_in_the_pane() {
    use aterm_capacity::{
        cache_work::{Availability, Denominators, Summary, UnavailableReason, Work},
        host_protocol::RootCacheWork,
    };
    let mut observation = observation();
    let mut historical = epoch(1);
    historical.roots = 3;
    historical.completed_roots = 3;
    historical.retired = true;
    historical.helper_exited = true;
    historical.control_eof = true;
    historical.endpoint_cleaned = true;
    historical.settled_units = 4;
    historical.remaining_units = 0;
    let run = "20261105T103732783Z-1ab3ba0487a929a0";
    let measured = Summary {
        run_id: run.into(),
        availability: Availability::Complete,
        denominators: Denominators {
            registered_units: 7,
            run_script_units: 2,
            observed_units: 1,
            units_without_attempts: 6,
            attempts_started: 2,
            attempts_finished: 2,
            captured_attempts: 1,
            uncacheable_attempts: 1,
        },
        totals: Some(Work {
            entries: 3,
            input_bytes: 41,
            bytes_hashed: 17,
            metadata_reads: 12,
            resolution_steps: 11,
            link_reads: 0,
            observations: 15,
            revalidations: 7,
        }),
    };
    let legacy = Summary {
        run_id: "20261105T103732784Z-1ab3ba0487a929a0".into(),
        availability: Availability::UnavailableLegacyLog,
        denominators: Denominators {
            registered_units: 7,
            run_script_units: 2,
            units_without_attempts: 7,
            ..Denominators::default()
        },
        totals: None,
    };
    historical.cache_work = vec![
        RootCacheWork {
            root: 0,
            summary: Some(measured),
            unavailable: None,
        },
        RootCacheWork {
            root: 1,
            summary: Some(legacy),
            unavailable: None,
        },
        RootCacheWork {
            root: 2,
            summary: None,
            unavailable: Some(UnavailableReason::QueryFailed),
        },
    ];
    observation.snapshot.epochs = vec![historical];
    observation.snapshot.encode().unwrap();
    let mut app = app(observation, Vec::new());
    let ui = compiled(&app);
    let label = |key: &str| {
        &ui.semantic(&UiKey::new(format!("allocation/monitor/epoch/1/{key}")))
            .unwrap()
            .label
    };
    assert!(label("root/0/cache/availability").contains(run));
    assert!(label("root/0/cache/availability").contains("build-script-inputs-v1 · complete"));
    assert_eq!(
        label("root/0/cache/work"),
        "Inputs: 17 bytes hashed · 12 metadata reads · 7 revalidations"
    );
    assert!(label("root/0/cache/coverage").contains("1 observed of 2 script units"));
    assert!(label("root/1/cache/availability").contains("legacy log unavailable"));
    assert!(label("root/2/cache/availability").contains("report query failed"));
    for root in [1, 2] {
        assert!(
            ui.semantic(&UiKey::new(format!(
                "allocation/monitor/epoch/1/root/{root}/cache/work"
            )))
            .is_none()
        );
    }
    let mut pending = (**app.monitor.as_ref().unwrap()).clone();
    pending.requested = 2;
    pending.desired_generation = Some(Generation([9; 16]));
    pending.phase = Phase::Queued;
    app.monitor = Some(Arc::new(pending));
    let rows = app.rows(Instant::now());
    assert!(
        rows.iter()
            .any(|(key, label)| key == "monitor/retained" && label.contains("attachment 1"))
    );
    assert!(rows.iter().any(
        |(key, label)| key == "monitor/epoch/1/root/0/cache/availability" && label.contains(run)
    ));
    let absent = cache_work_rows(&epoch(2));
    assert_eq!(absent.len(), 1);
    assert!(absent[0].1.contains("no completed-root report recorded"));
    assert!(!absent[0].1.contains("0 bytes"));
}

#[test]
fn epochs_preserve_failed_builds_and_uncertain_backing_after_transport_retirement() {
    let mut observation = observation();
    let mut retired = epoch(2);
    retired.retired = true;
    retired.helper_exited = true;
    retired.control_eof = true;
    retired.endpoint_cleaned = true;
    retired.settled_units = 1;
    retired.uncertain_units = 1;
    retired.remaining_units = 2;
    retired.completed_roots = 2;
    retired.failed_roots = 1;
    let mut retiring = epoch(1);
    retiring.retired = true;
    observation.snapshot.epochs = vec![retired, retiring];
    // Exercise the public projection's invariants before rendering synthetic
    // states; resource cleanup never upgrades the original build outcome.
    observation.snapshot.encode().unwrap();
    let app = app(observation, Vec::new());
    let rows = app.rows(Instant::now());
    let index = |key: &str| rows.iter().position(|(found, _)| found == key).unwrap();
    assert!(index("monitor/epoch/1/budget") < index("monitor/epoch/2/budget"));
    assert!(index("monitor/epoch/2/backing") < index("monitor/completions"));
    let ui = compiled(&app);
    let label = |key: &str| {
        &ui.semantic(&UiKey::new(format!("allocation/monitor/{key}")))
            .unwrap()
            .label
    };
    assert_eq!(
        label("epoch/2/budget"),
        "Epoch 2 · CPU backing N=4 · root slots R=2 · original admitted memory peak 3221225472 bytes (3.0 GiB)"
    );
    assert_eq!(
        label("epoch/2/roots"),
        "Roots: 2 completed of 2 slots · 1 failed"
    );
    assert_eq!(
        label("epoch/1/transport"),
        "Transport: opened yes · Retired yes · helper exited no · output ended no"
    );
    assert_eq!(
        label("epoch/1/backing"),
        "CPU backing: 0 returned · 4 remaining · 0 uncertain · group drain + endpoint cleanup pending"
    );
    assert_eq!(
        label("epoch/2/backing"),
        "CPU backing: 1 returned · 2 remaining · 1 uncertain · group drain + endpoint cleanup confirmed"
    );
    assert_eq!(
        ui.semantic(&UiKey::new("allocation/monitor/epochs"))
            .unwrap()
            .role,
        SemanticRole::Heading
    );
}

#[test]
fn unlaunched_cancellation_returns_resources_without_inventing_build_success() {
    let mut observation = observation();
    let mut cancelled = epoch(1);
    cancelled.opened = false;
    cancelled.cancellation_started = true;
    cancelled.endpoint_cleaned = true;
    cancelled.settled_units = 4;
    cancelled.remaining_units = 0;
    let mut not_spawned = epoch(2);
    not_spawned.opened = false;
    not_spawned.not_spawned = true;
    observation.snapshot.epochs = vec![cancelled, not_spawned];
    observation.snapshot.encode().unwrap();
    let ui = compiled(&app(observation, Vec::new()));
    let label = |key: &str| {
        &ui.semantic(&UiKey::new(format!("allocation/monitor/{key}")))
            .unwrap()
            .label
    };
    assert_eq!(
        label("epoch/1/roots"),
        "Roots: 0 completed of 2 slots · 0 failed · launch cancellation started"
    );
    assert_eq!(
        label("epoch/1/backing"),
        "CPU backing: 4 returned · 0 remaining · 0 uncertain · unlaunched cleanup confirmed"
    );
    assert_eq!(
        label("epoch/2/roots"),
        "Roots: 0 completed of 2 slots · 0 failed · no child spawned; cancellation pending"
    );
    assert_eq!(
        label("epoch/1/transport"),
        "Transport: opened no · Retired no · helper exited no · output ended no"
    );
    assert!(
        ui.semantic(&UiKey::new("allocation/monitor/completions-empty"))
            .is_some(),
        "CPU repayment supplies no delivered build completion"
    );
}

#[test]
fn typed_outcomes_preserve_each_success_and_cancellation_flag_in_semantics() {
    let mut recent = Vec::new();
    let mut expected = Vec::new();
    for (succeeded, cancelled) in [(false, false), (false, true), (true, false), (true, true)] {
        let revision = 10 + recent.len() as u64;
        recent.push(RecordSummary {
            revision,
            attempt: Some(7),
            kind: EventKind::Finished {
                succeeded,
                cancelled,
            },
        });
        expected.push((
            revision,
            format!(
                "Session attempt 7 · Finished · {}{} · completion revision {revision}",
                if succeeded {
                    "success established"
                } else {
                    "success not established"
                },
                if cancelled {
                    " · cancellation requested"
                } else {
                    ""
                }
            ),
        ));
    }
    for (revision, attempt, kind, label) in [
        (
            14,
            Some(8),
            EventKind::Failed,
            "Session attempt 8 · Owner-reported failure · completion revision 14",
        ),
        (
            15,
            None,
            EventKind::Failed,
            "Session service failure · history revision 15",
        ),
        (
            16,
            None,
            EventKind::Finished {
                succeeded: false,
                cancelled: true,
            },
            "Session outcome · Finished · success not established · cancellation requested · history revision 16",
        ),
    ] {
        recent.push(RecordSummary {
            revision,
            attempt,
            kind,
        });
        expected.push((revision, label.into()));
    }
    let app = app(observation(), recent);
    let ui = compiled(&app);
    for (revision, label) in expected {
        let key = UiKey::new(format!("allocation/monitor/completion/{revision}"));
        assert_eq!(ui.semantic(&key).unwrap().label, label);
    }
    let completion_keys: BTreeSet<_> = ui
        .semantics
        .iter()
        .filter(|node| {
            node.key
                .as_str()
                .starts_with("allocation/monitor/completion/")
        })
        .map(|node| &node.key)
        .collect();
    assert_eq!(
        completion_keys.len(),
        7,
        "reused attempts retain distinct revisions"
    );
    let header = ui
        .semantic(&UiKey::new("allocation/monitor/completions"))
        .unwrap();
    assert_eq!(header.role, SemanticRole::Heading);
    assert_eq!(header.label, "Recent delivered outcomes");
}

#[test]
fn retained_outcomes_keep_original_identity_and_distinguish_local_eviction_from_gap() {
    let mut observation = observation();
    let generation = observation.snapshot.generation;
    // This control supplies a synthetic live row to the pure presentation; the
    // real source above supplies its original receipt and attachment identity.
    observation.snapshot.jobs.push(Job {
        attempt: 2,
        phase: JobPhase::Waiting,
        reserved_bytes: 1024,
        failure: None,
    });
    observation.snapshot.epochs.push(epoch(3));
    let mut app = app(
        observation,
        vec![RecordSummary {
            revision: 20,
            attempt: Some(1),
            kind: EventKind::Finished {
                succeeded: true,
                cancelled: false,
            },
        }],
    );
    let monitor = Arc::make_mut(app.monitor.as_mut().unwrap());
    monitor.requested = 2;
    monitor.desired_generation = Some(Generation(generation.0.map(|byte| byte ^ 0xff)));
    monitor.phase = Phase::Reading;
    let last = monitor.last.as_mut().unwrap();
    last.last_gap = Some(Gap { first: 3, last: 8 });
    last.omitted_terminals = 5;

    let rows = app.rows(Instant::now());
    let row = |key: &str| &rows.iter().find(|(found, _)| found == key).unwrap().1;
    assert_eq!(
        row("monitor/retained"),
        "Retained observation from attachment 1; attachment 2 has no observation"
    );
    assert_eq!(
        row("monitor/source"),
        &format!(
            "Source generation: {} · snapshot revision 0 · attachment 1",
            aterm_uds::rand::hex_encode(&generation.0)
        )
    );
    assert_eq!(
        row("monitor/history-gap"),
        "Latest reported history gap: revisions 3–8 unavailable"
    );
    assert_eq!(
        row("monitor/completions-omitted"),
        "Earlier delivered outcomes omitted locally: 5"
    );
    let index = |key: &str| rows.iter().position(|(found, _)| found == key).unwrap();
    assert!(index("monitor/attempt/2") < index("monitor/completions"));
    assert!(index("monitor/epoch/3/budget") < index("monitor/completions"));
    assert!(index("monitor/completions") < index("monitor/completion/20"));
    assert_eq!(
        rows.len(),
        rows.iter()
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>()
            .len()
    );
    let ui = compiled(&app);
    for key in [
        "monitor/retained",
        "monitor/source",
        "monitor/history-gap",
        "monitor/completions-omitted",
        "monitor/epoch/3/budget",
        "monitor/completion/20",
    ] {
        assert_eq!(
            &ui.semantic(&UiKey::new(format!("allocation/{key}")))
                .unwrap()
                .label,
            row(key)
        );
    }
}

#[test]
fn nonterminal_records_do_not_invent_outcomes_and_empty_history_stays_explicit() {
    for kind in [
        EventKind::Submitted,
        EventKind::IntentDelivered,
        EventKind::Rejected,
        EventKind::AdmissionRefused,
        EventKind::NativeObservation,
        EventKind::StateChanged,
        EventKind::OwnerClosed,
    ] {
        assert!(
            completion_row(&RecordSummary {
                revision: 1,
                attempt: Some(1),
                kind,
            })
            .is_none()
        );
    }
    let app = app(observation(), Vec::new());
    let ui = compiled(&app);
    assert_eq!(
        ui.semantic(&UiKey::new("allocation/monitor/completions-empty"))
            .unwrap()
            .label,
        "No delivered outcomes retained"
    );
    assert!(
        ui.semantic(&UiKey::new("allocation/monitor/completions-omitted"))
            .is_none()
    );
}
