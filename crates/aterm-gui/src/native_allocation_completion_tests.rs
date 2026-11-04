// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless projection controls for typed delivered outcomes and retained identity.

use super::*;
use crate::allocation_monitor_host::{Phase, Presentation, Reading};
use crate::native_app::ViewMotionCx;
use crate::native_ui::{CompiledUi, LogicalRect, UiKey};
use aterm_capacity::{
    cohort_namespace::Namespace,
    host_protocol::{Generation, Job, Phase as JobPhase},
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
