// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the native document mutation/publication and close
//! protocols. These tests drive the genuine Surface-backed [`DocumentStore`] (and,
//! for publication, the shipping Editor controller over it),
//! project independently observed store state into the drift-free models, and
//! ask the executable model whether each real transition is admitted.

#![cfg(test)]

use aterm_buffer::Seq;
use aterm_spec::derive::{Model, native_close_plan_model, native_document_publication_model};
use aterm_spec::interp::{State, admits};

use crate::document_store::{
    DocumentCloseReadiness, DocumentError, DocumentId, DocumentPhase, DocumentStore,
    DocumentTxnOutcome, DocumentViewId, TextEdit,
};
use crate::native_editor::{EditorBufferView, EditorError, EditorWorkspace, Selection};

#[derive(Clone, Copy)]
struct PendingTxn {
    active: bool,
    /// The Editor transaction's base: the Editor view's anchor sequence when it
    /// began, read off the shipping view.
    base: Seq,
}

fn relative(seq: Seq, baseline: Seq) -> i64 {
    i64::try_from(seq.0.saturating_sub(baseline.0)).expect("bounded test sequence")
}

/// Every text the document has published, with the sequence that published it,
/// from the baseline on — built from the edits THIS TEST made, never read back
/// from the store. The immutable snapshot's generation is read off its text
/// against this record. `DocumentSnapshot::seq` is the Surface head by
/// construction, so projecting `snapshot_seq` from it would make
/// `SnapshotCurrent` true by construction, and so would recording the store's
/// own text after each commit: a projection cache left on the old text would be
/// recorded as the new one. Built independently, a stale cache resolves to the
/// older sequence it still carries.
type TextHistory = Vec<(Seq, String)>;

fn publication_projection(
    model: &Model,
    store: &DocumentStore,
    document: DocumentId,
    editor: &EditorBufferView,
    markdown: DocumentViewId,
    pending: PendingTxn,
    history: &TextHistory,
) -> State {
    let baseline = history.first().expect("the baseline text is recorded").0;
    let snapshot = store.snapshot(document).expect("live document");
    let snapshot_text_seq = history
        .iter()
        .rev()
        .find(|(_, text)| *text == *snapshot.text)
        .map(|(seq, _)| *seq)
        .expect("the snapshot carries a text this test published");
    let editor_seen = store
        .observed_seq(document, editor.document_view)
        .expect("attached Editor view");
    let markdown_seen = store
        .observed_seq(document, markdown)
        .expect("attached Markdown view");
    let mut state = model.init_state();
    state.insert("edit_seq", relative(snapshot.seq, baseline));
    state.insert("snapshot_seq", relative(snapshot_text_seq, baseline));
    state.insert("editor_seen", relative(editor_seen, baseline));
    state.insert("markdown_seen", relative(markdown_seen, baseline));
    // The shipping Editor view's own anchor version, which the host moves
    // through `observe_external` and the Editor's commit through `observe_own`.
    state.insert("anchor_seq", relative(editor.anchor_seq(), baseline));
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
    store.attach_view(document, markdown).unwrap();
    // The Editor controller is the shipping one: its view, its anchor rebase
    // and its own commits.
    let mut workspace = EditorWorkspace::new();
    let mut editor = workspace
        .attach(&mut store, document, DocumentViewId(102))
        .unwrap();
    let mut caret = 2;
    editor.selections = vec![Selection::caret(caret)];
    let baseline = store.snapshot(document).unwrap().seq;
    let mut text = String::from("alpha");
    let mut history: TextHistory = vec![(baseline, text.clone())];

    // Editor begins at the current immutable snapshot. The transaction base is
    // deliberately retained — as the view it was built from — while the other
    // controller commits twice.
    let retained = editor.clone();
    let mut pending = PendingTxn {
        active: true,
        base: retained.anchor_seq(),
    };

    for prefix in ["one-", "two-"] {
        let before = publication_projection(
            &model, &store, document, &editor, markdown, pending, &history,
        );
        // The other controller inserts ahead of the Editor's caret, and the host
        // rebases the Editor through the commit's deltas, as it does for a disk
        // refresh.
        let snapshot = store.snapshot(document).unwrap();
        let (seq, deltas) = committed(store.transact(
            document,
            snapshot.seq,
            vec![TextEdit {
                range: 0..0,
                insert: prefix.to_string(),
            }],
        ));
        text.insert_str(0, prefix);
        history.push((seq, text.clone()));
        let unrebased = editor.clone();
        editor.observe_external(seq, &deltas);
        caret += prefix.len();
        assert_eq!(
            editor.primary_selection(),
            &Selection::caret(caret),
            "the returned deltas carry the caret past the insertion"
        );
        let after = publication_projection(
            &model, &store, document, &editor, markdown, pending, &history,
        );
        assert_transition(&model, &before, &after, "OtherCommit");

        // Negative control: the commit published only to its author. An Editor
        // the host did not rebase keeps its old anchor version — projected from
        // that real view — which no healthy step leaves behind; the mutant's
        // step leaves the snapshot on the old text as well.
        let missed = publication_projection(
            &model, &store, document, &unrebased, markdown, pending, &history,
        );
        assert_eq!(missed["anchor_seq"], before["anchor_seq"]);
        assert_eq!(admits(&model, &before, &missed), None);
        assert!(!model.check_invariant("AnchorsTransformed", &missed));
        let author_only =
            aterm_spec::interp::with_buggy(&model, 1).successors("OtherCommit", &before)[0].clone();
        assert_eq!(author_only["snapshot_seq"], before["snapshot_seq"]);
        assert_eq!(author_only["anchor_seq"], missed["anchor_seq"]);
        assert_eq!(admits(&model, &before, &author_only), None);
        for law in ["SnapshotCurrent", "EditorCurrent", "AnchorsTransformed"] {
            assert!(!model.check_invariant(law, &author_only), "{law}");
        }

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

    // The original Editor request is now stale. The shipping Editor refuses its
    // retained view before any transaction, the store's own lane answers
    // Conflict to its base, and neither changes canonical text or an observer.
    let before_snapshot = store.snapshot(document).unwrap();
    let editor_before = store.observed_seq(document, editor.document_view);
    let markdown_before = store.observed_seq(document, markdown);
    let before_reject = publication_projection(
        &model, &store, document, &editor, markdown, pending, &history,
    );
    let mut stale_view = retained;
    assert!(matches!(
        workspace.insert_text(&mut store, &mut stale_view, "X"),
        Err(EditorError::StaleView { view, current })
            if view == pending.base && current == before_snapshot.seq
    ));
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
    assert_eq!(
        store.observed_seq(document, editor.document_view),
        editor_before
    );
    assert_eq!(store.observed_seq(document, markdown), markdown_before);
    pending.active = false;
    let after_reject = publication_projection(
        &model, &store, document, &editor, markdown, pending, &history,
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

    // A fresh transaction commits through the shipping Editor at its caret and
    // is published to Markdown before this synchronous call returns.
    pending = PendingTxn {
        active: true,
        base: editor.anchor_seq(),
    };
    let before_clean = publication_projection(
        &model, &store, document, &editor, markdown, pending, &history,
    );
    workspace
        .insert_text(&mut store, &mut editor, "-clean")
        .unwrap();
    let (seq, _) = workspace
        .take_last_commit(document)
        .expect("the Editor's own commit");
    text.insert_str(caret, "-clean");
    history.push((seq, text.clone()));
    pending.active = false;
    let after_clean = publication_projection(
        &model, &store, document, &editor, markdown, pending, &history,
    );
    assert_transition(&model, &before_clean, &after_clean, "CommitClean");
    assert_eq!(editor.anchor_seq(), seq);
    assert_eq!(
        store.observed_seq(document, editor.document_view),
        Some(seq)
    );
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

    // Negative control: Closing freezes the head. The genuine mutation lane
    // refuses an edit here; a lane that admitted it would move the head past
    // the frozen request, which the healthy model does not admit.
    assert_eq!(
        append_text(&mut store, document, "late"),
        DocumentTxnOutcome::Rejected(DocumentError::Closing)
    );
    assert_eq!(
        close_projection(&model, &store, document, projection),
        after_final
    );
    let late_edit =
        aterm_spec::interp::with_buggy(&model, 1).successors("Edit", &after_final)[0].clone();
    assert_eq!(admits(&model, &after_final, &late_edit), None);
    assert!(!model.check_invariant("FrozenFinalSequence", &late_edit));

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

    // Negative control: the acknowledgement of an OLDER generation (the clean
    // baseline) leaves the plan Pending in the genuine store. Read as covering
    // the request, it would close the document below its frozen sequence.
    assert_eq!(
        store.checkpoint_ack(document, baseline).unwrap(),
        DocumentCloseReadiness::Pending { requested }
    );
    assert_eq!(
        close_projection(&model, &store, document, projection),
        before_ack
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let stale_ack = buggy.successors("AckCheckpoint", &before_ack)[0].clone();
    assert_eq!(stale_ack["checkpoint_seq"], before_ack["checkpoint_seq"]);
    assert_eq!(admits(&model, &before_ack, &stale_ack), None);
    let closed_short = buggy.successors("CommitClose", &stale_ack)[0].clone();
    assert!(!model.check_invariant("NoSilentLoss", &closed_short));

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
