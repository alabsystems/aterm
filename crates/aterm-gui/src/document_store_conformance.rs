// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the native document mutation/publication and close
//! protocols. These tests drive the genuine Surface-backed [`DocumentStore`],
//! project independently observed store state into the drift-free models, and
//! ask the executable model whether each real transition is admitted.

#![cfg(test)]

use aterm_buffer::Seq;
use aterm_spec::derive::{Model, native_close_plan_model, native_document_publication_model};
use aterm_spec::interp::{State, admits};

use crate::document_store::{
    DocumentCloseReadiness, DocumentError, DocumentId, DocumentPhase, DocumentStore,
    DocumentTxnOutcome, DocumentViewId, TextEdit, rebase_position,
};

#[derive(Clone, Copy)]
struct PendingTxn {
    active: bool,
    base: Seq,
    /// Selection-anchor VERSION observed by the controller. The byte position is
    /// separately transformed through the real returned deltas below.
    anchor_seq: Seq,
}

fn relative(seq: Seq, baseline: Seq) -> i64 {
    i64::try_from(seq.0.saturating_sub(baseline.0)).expect("bounded test sequence")
}

fn publication_projection(
    model: &Model,
    store: &DocumentStore,
    document: DocumentId,
    editor: DocumentViewId,
    markdown: DocumentViewId,
    baseline: Seq,
    pending: PendingTxn,
) -> State {
    let snapshot = store.snapshot(document).expect("live document");
    let editor_seen = store
        .observed_seq(document, editor)
        .expect("attached Editor view");
    let markdown_seen = store
        .observed_seq(document, markdown)
        .expect("attached Markdown view");
    let mut state = model.init_state();
    state.insert("edit_seq", relative(snapshot.seq, baseline));
    state.insert("snapshot_seq", relative(snapshot.seq, baseline));
    state.insert("editor_seen", relative(editor_seen, baseline));
    state.insert("markdown_seen", relative(markdown_seen, baseline));
    state.insert("anchor_seq", relative(pending.anchor_seq, baseline));
    state.insert("txn_active", i64::from(pending.active));
    state.insert("txn_base", relative(pending.base, baseline));
    // These are defect witnesses, not duplicated sources of real state. A real
    // conflict/atomic publication is checked below from before/after snapshots;
    // the explicit corrupted projections flip these witnesses and must fail.
    state.insert("stale_write", 0);
    state.insert("partial_publish", 0);
    state
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        admits(model, before, after),
        Some(action),
        "real transition must be admitted specifically as {action}"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

fn append_text(
    store: &mut DocumentStore,
    document: DocumentId,
    suffix: &str,
) -> DocumentTxnOutcome {
    let snapshot = store.snapshot(document).expect("live document");
    let end = snapshot.text.len();
    store.transact(
        document,
        snapshot.seq,
        vec![TextEdit {
            range: end..end,
            insert: suffix.to_string(),
        }],
    )
}

fn committed(outcome: DocumentTxnOutcome) -> (Seq, Vec<crate::document_store::EditDelta>) {
    match outcome {
        DocumentTxnOutcome::Committed { seq, deltas, .. } => (seq, deltas),
        other => panic!("expected committed document transaction, got {other:?}"),
    }
}

#[test]
fn surface_occ_publication_conforms_and_rejects_corrupted_projection() {
    let model = native_document_publication_model();
    let mut store = DocumentStore::new();
    let document = store.open("mem://conformance/publication".into(), "alpha".into());
    let markdown = DocumentViewId(101);
    let editor = DocumentViewId(102);
    store.attach_view(document, markdown).unwrap();
    store.attach_view(document, editor).unwrap();
    let baseline = store.snapshot(document).unwrap().seq;

    // Editor begins at the current immutable snapshot. The transaction base is
    // deliberately retained while two independent writers commit rapidly.
    let mut pending = PendingTxn {
        active: true,
        base: baseline,
        anchor_seq: baseline,
    };
    let mut anchor_position = 2usize;

    for suffix in ["-one", "-two"] {
        let before = publication_projection(
            &model, &store, document, editor, markdown, baseline, pending,
        );
        let (seq, deltas) = committed(append_text(&mut store, document, suffix));
        let previous_anchor = anchor_position;
        anchor_position = rebase_position(anchor_position, &deltas);
        assert!(
            anchor_position >= previous_anchor,
            "returned delta transforms the controller anchor"
        );
        pending.anchor_seq = seq;
        let after = publication_projection(
            &model, &store, document, editor, markdown, baseline, pending,
        );
        assert_transition(&model, &before, &after, "OtherCommit");

        // Negative control: a router that publishes the commit only to Editor
        // cannot masquerade as the real transition and violates the same derived
        // invariant. This state is built independently from the router decision.
        let mut editor_only = after.clone();
        editor_only.insert("markdown_seen", before["markdown_seen"]);
        editor_only.insert("partial_publish", 1);
        assert_eq!(admits(&model, &before, &editor_only), None);
        assert!(!model.check_invariant("MarkdownCurrent", &editor_only));
        assert!(!model.check_invariant("PublishIsAtomic", &editor_only));
    }

    // The original Editor request is now stale. The genuine mutation lane must
    // return Conflict and change neither canonical text nor either observer.
    let before_snapshot = store.snapshot(document).unwrap();
    let editor_before = store.observed_seq(document, editor);
    let markdown_before = store.observed_seq(document, markdown);
    let before_reject = publication_projection(
        &model, &store, document, editor, markdown, baseline, pending,
    );
    let stale_outcome = store.transact(
        document,
        pending.base,
        vec![TextEdit {
            range: 0..1,
            insert: "X".into(),
        }],
    );
    assert_eq!(
        stale_outcome,
        DocumentTxnOutcome::Conflict {
            current: before_snapshot.seq
        }
    );
    assert_eq!(store.snapshot(document).unwrap().text, before_snapshot.text);
    assert_eq!(store.observed_seq(document, editor), editor_before);
    assert_eq!(store.observed_seq(document, markdown), markdown_before);
    pending.active = false;
    let after_reject = publication_projection(
        &model, &store, document, editor, markdown, baseline, pending,
    );
    assert_transition(&model, &before_reject, &after_reject, "RejectStale");

    // Negative control: blind stale acceptance advances every data lane but
    // explicitly records the forbidden stale write. It is not a valid Buggy=0
    // transition and the invariant catches it.
    let mut blind_stale = before_reject.clone();
    for key in [
        "edit_seq",
        "snapshot_seq",
        "editor_seen",
        "markdown_seen",
        "anchor_seq",
    ] {
        blind_stale.insert(key, before_reject[key] + 1);
    }
    blind_stale.insert("txn_active", 0);
    blind_stale.insert("stale_write", 1);
    assert_eq!(admits(&model, &before_reject, &blind_stale), None);
    assert!(!model.check_invariant("StaleTxnIsNoOp", &blind_stale));

    // A fresh Editor transaction commits cleanly and is published to Markdown
    // before this synchronous call returns.
    let fresh = store.snapshot(document).unwrap().seq;
    pending = PendingTxn {
        active: true,
        base: fresh,
        anchor_seq: fresh,
    };
    let before_clean = publication_projection(
        &model, &store, document, editor, markdown, baseline, pending,
    );
    let (seq, deltas) = committed(append_text(&mut store, document, "-clean"));
    anchor_position = rebase_position(anchor_position, &deltas);
    assert!(anchor_position <= store.snapshot(document).unwrap().text.len());
    pending.active = false;
    pending.anchor_seq = seq;
    let after_clean = publication_projection(
        &model, &store, document, editor, markdown, baseline, pending,
    );
    assert_transition(&model, &before_clean, &after_clean, "CommitClean");
    assert_eq!(store.observed_seq(document, editor), Some(seq));
    assert_eq!(store.observed_seq(document, markdown), Some(seq));
}

#[derive(Clone, Copy)]
struct CloseProjection {
    baseline: Seq,
    markdown: DocumentViewId,
    editor: DocumentViewId,
    frozen_requested: Option<Seq>,
    other_leaf_ready: bool,
}

fn close_projection(
    model: &Model,
    store: &DocumentStore,
    document: DocumentId,
    projection: CloseProjection,
) -> State {
    let head = store.snapshot(document).expect("live document").seq;
    let checkpoint = store.checkpoint_seq(document).expect("live document");
    let markdown_live = store.observed_seq(document, projection.markdown).is_some();
    let editor_live = store.observed_seq(document, projection.editor).is_some();
    let live_views = usize::from(markdown_live) + usize::from(editor_live);
    let actual_phase = store.phase(document).expect("live document");
    let phase = match actual_phase {
        DocumentPhase::Active => 0,
        DocumentPhase::Closing { .. } => 1,
        DocumentPhase::Blocked { .. } => 2,
        DocumentPhase::Suspended if live_views == 0 && projection.frozen_requested.is_some() => 3,
        DocumentPhase::Suspended => 0,
    };
    let requested = match actual_phase {
        DocumentPhase::Closing { requested } | DocumentPhase::Blocked { requested } => requested,
        DocumentPhase::Suspended => projection.frozen_requested.unwrap_or(projection.baseline),
        DocumentPhase::Active => projection.baseline,
    };
    let document_ready = i64::from(phase > 0 && checkpoint >= requested);
    let mut state = model.init_state();
    state.insert("phase", phase);
    state.insert("edit_seq", relative(head, projection.baseline));
    state.insert("requested_seq", relative(requested, projection.baseline));
    state.insert("checkpoint_seq", relative(checkpoint, projection.baseline));
    state.insert("markdown_views", i64::from(markdown_live));
    state.insert("editor_views", i64::from(editor_live));
    state.insert("document_ready", document_ready);
    state.insert("other_leaf_ready", i64::from(projection.other_leaf_ready));
    state.insert("any_leaf_detached", i64::from(phase == 3));
    state
}

#[test]
fn last_markdown_after_editor_close_conforms_to_durable_atomic_ordering() {
    let model = native_close_plan_model();
    let mut store = DocumentStore::new();
    let document = store.open("mem://conformance/close".into(), "draft".into());
    let markdown = DocumentViewId(201);
    let editor = DocumentViewId(202);
    store.attach_view(document, markdown).unwrap();
    store.attach_view(document, editor).unwrap();
    let baseline = store.snapshot(document).unwrap().seq;
    let mut projection = CloseProjection {
        baseline,
        markdown,
        editor,
        frozen_requested: None,
        other_leaf_ready: false,
    };

    // A real edit makes the document dirty and conforms to the close model's
    // only Open-phase mutation.
    let before_edit = close_projection(&model, &store, document, projection);
    let (dirty_seq, _) = committed(append_text(&mut store, document, "!"));
    let after_edit = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_edit, &after_edit, "Edit");

    // Editor is non-final because Markdown still references the same document.
    assert_eq!(
        store.prepare_close(document, &[editor]).unwrap(),
        DocumentCloseReadiness::Ready {
            requested: dirty_seq
        }
    );
    let before_editor_detach = close_projection(&model, &store, document, projection);
    store.commit_detach(document, &[editor]).unwrap();
    let after_editor_detach = close_projection(&model, &store, document, projection);
    assert_transition(
        &model,
        &before_editor_detach,
        &after_editor_detach,
        "CloseEditorNonFinal",
    );
    assert_eq!(store.view_count(document), Some(1));

    // Markdown is now the final view: the genuine store freezes the mandatory
    // sequence and refuses detach until a durable checkpoint reaches it.
    let before_final = close_projection(&model, &store, document, projection);
    let readiness = store.prepare_close(document, &[markdown]).unwrap();
    let DocumentCloseReadiness::Pending { requested } = readiness else {
        panic!("dirty final Markdown view must wait, got {readiness:?}");
    };
    projection.frozen_requested = Some(requested);
    let after_final = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_final, &after_final, "BeginFinalClose");

    let before_refused = close_projection(&model, &store, document, projection);
    assert_eq!(
        store.commit_detach(document, &[markdown]),
        Err(DocumentError::CloseNotReady)
    );
    assert_eq!(
        close_projection(&model, &store, document, projection),
        before_refused,
        "refused close changes no projected state"
    );

    // Negative control: a coordinator that detaches the leaf here is rejected by
    // the executable Next relation and violates both atomicity and durability.
    let mut early_detach = before_refused.clone();
    early_detach.insert("phase", 3);
    early_detach.insert("markdown_views", 0);
    early_detach.insert("any_leaf_detached", 1);
    assert_eq!(admits(&model, &before_refused, &early_detach), None);
    assert!(!model.check_invariant("AtomicTreeClose", &early_detach));
    assert!(!model.check_invariant("NoSilentLoss", &early_detach));

    // Persistence failure blocks the plan and preserves the view. Retry returns
    // to Closing but cannot fabricate a durable acknowledgement.
    let before_fail = close_projection(&model, &store, document, projection);
    assert_eq!(store.checkpoint_fail(document).unwrap(), requested);
    let after_fail = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_fail, &after_fail, "FailCheckpoint");
    assert_eq!(store.view_count(document), Some(1));

    let before_retry = close_projection(&model, &store, document, projection);
    assert_eq!(
        store.checkpoint_retry(document).unwrap(),
        DocumentCloseReadiness::Pending { requested }
    );
    let after_retry = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_retry, &after_retry, "RetryCheckpoint");
    assert_eq!(after_retry["document_ready"], 0);
    assert_eq!(
        store.commit_detach(document, &[markdown]),
        Err(DocumentError::CloseNotReady),
        "Retry is not a fake Ack"
    );

    // The other leaf's independently obtained Ready verdict joins the plan; it
    // is intentionally separate from document durability.
    let before_other_ready = close_projection(&model, &store, document, projection);
    projection.other_leaf_ready = true;
    let after_other_ready = close_projection(&model, &store, document, projection);
    assert_transition(
        &model,
        &before_other_ready,
        &after_other_ready,
        "ReadyOtherLeaf",
    );

    let before_ack = close_projection(&model, &store, document, projection);
    assert_eq!(
        store.checkpoint_ack(document, requested).unwrap(),
        DocumentCloseReadiness::Ready { requested }
    );
    let after_ack = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_ack, &after_ack, "AckCheckpoint");

    let before_commit = close_projection(&model, &store, document, projection);
    store.commit_detach(document, &[markdown]).unwrap();
    let after_commit = close_projection(&model, &store, document, projection);
    assert_transition(&model, &before_commit, &after_commit, "CommitClose");
    assert_eq!(store.view_count(document), Some(0));
    assert!(model.check_invariant("NoSilentLoss", &after_commit));
    assert!(model.check_invariant("AtomicTreeClose", &after_commit));
}
