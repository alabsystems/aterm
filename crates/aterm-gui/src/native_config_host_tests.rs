// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn document() -> DocumentId {
    let mut store = crate::test_document_store();
    store.open(
        "document:test".to_string(),
        "manual diagnostics".to_string(),
    )
}

fn assets() -> Arc<crate::app_config::ConfigAssetCatalog> {
    crate::app_config::ConfigAssetCatalog::empty()
}

/// NEGATIVE CONTROL: the `Buggy = 1` model mutant leaves `pending_revision = 2` on `RequestThird`, which projects to `(1, 2)` rather than `(2, 2)`.
#[test]
fn shipping_lane_conforms_to_latest_revision_model() {
    let document = document();
    let (mut lane, receiver) = HostDiagnosticsLane::test_pair();
    let model = aterm_models_misc::manual_config_diagnostics_lane_model();
    let mut state = model.init_state();
    let key_for = |model_revision: i64| match model_revision {
        1 => (1, 1),
        2 => (1, 2),
        3 => (2, 2),
        other => panic!("unexpected model revision {other}"),
    };
    let pending_key = |lane: &HostDiagnosticsLane| {
        lane.pending
            .as_ref()
            .map(|request| (request.revision, request.analysis_generation))
    };

    lane.request(
        document,
        1,
        1,
        true,
        Arc::from("theme = \"One\"\n"),
        assets(),
    );
    assert!(model.fire("RequestFirst", &mut state));
    assert_eq!(
        pending_key(&lane),
        state
            .get("pending_revision")
            .copied()
            .filter(|revision| *revision > 0)
            .map(key_for)
    );
    lane.request(
        document,
        1,
        2,
        true,
        Arc::from("theme = \"One\"\n"),
        assets(),
    );
    assert!(model.fire("RequestSecond", &mut state));
    assert_eq!(
        pending_key(&lane),
        state
            .get("pending_revision")
            .copied()
            .filter(|revision| *revision > 0)
            .map(key_for)
    );
    lane.request(
        document,
        2,
        2,
        true,
        Arc::from("theme = \"Three\"\n"),
        assets(),
    );
    assert!(model.fire("RequestThird", &mut state));
    assert_eq!(
        pending_key(&lane),
        state
            .get("pending_revision")
            .copied()
            .filter(|revision| *revision > 0)
            .map(key_for)
    );

    let first = receiver.recv().unwrap();
    assert!(model.fire("WorkerTakes", &mut state));
    assert_eq!(
        (first.revision, first.analysis_generation),
        key_for(state["active_revision"])
    );
    assert!(model.fire("WorkerCompletes", &mut state));
    lane.worker_drained();
    assert!(model.fire("DispatchLatestPending", &mut state));
    assert_eq!(
        pending_key(&lane),
        state
            .get("pending_revision")
            .copied()
            .filter(|revision| *revision > 0)
            .map(key_for)
    );
    assert!(model.fire("RejectStale", &mut state));
    let latest = receiver.recv().unwrap();
    assert!(model.fire("WorkerTakes", &mut state));
    assert_eq!(
        (latest.revision, latest.analysis_generation),
        key_for(state["active_revision"])
    );
    assert_eq!(latest.source.as_ref(), "theme = \"Three\"\n");
    assert!(model.fire("WorkerCompletes", &mut state));
    assert!(model.fire("AcceptCurrent", &mut state));
    assert_eq!(state.get("published_revision"), Some(&3));
    assert!(receiver.try_recv().is_err());

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut lost = buggy.init_state();
    for action in ["RequestFirst", "RequestSecond", "RequestThird"] {
        assert!(buggy.fire(action, &mut lost));
    }
    assert_eq!(key_for(lost["pending_revision"]), (1, 2));
    assert!(!buggy.check_invariant("LatestRequestRemainsRepresented", &lost));
    assert!(!buggy.check_invariant("PendingSlotNamesLatest", &lost));
}

#[test]
fn oversized_revision_drops_obsolete_pending_host_work() {
    let document = document();
    let (mut lane, receiver) = HostDiagnosticsLane::test_pair();
    lane.request(
        document,
        1,
        1,
        true,
        Arc::from("theme = \"One\"\n"),
        assets(),
    );
    lane.request(
        document,
        2,
        1,
        true,
        Arc::from("theme = \"Two\"\n"),
        assets(),
    );
    lane.request(
        document,
        3,
        1,
        true,
        Arc::<str>::from("x".repeat(MAX_CONFIG_ANALYSIS_BYTES + 1)),
        assets(),
    );

    assert_eq!(receiver.recv().unwrap().revision, 1);
    lane.worker_drained();
    assert!(receiver.try_recv().is_err());
}
