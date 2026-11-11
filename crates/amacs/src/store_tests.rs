// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn open() -> (DocumentStore, DocumentId) {
    let mut store = DocumentStore::for_test();
    let id = store.open("mem://readme".into(), "hello\nworld".into());
    (store, id)
}

#[test]
fn canonical_uri_reuses_one_document() {
    let (mut store, id) = open();
    let again = store.open("mem://readme".into(), "ignored".into());
    assert_eq!(again, id);
    assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "hello\nworld");
}

/// A read-only document refuses every transaction, a replacement included, keeps its text and
/// sequence, and stays read-only when its URI is opened again.
/// NEGATIVE CONTROL: with `transact` checking only `directory`, the edit commits:
/// `` read-only: Committed { .. } ``.
/// A concealed document keeps what it first concealed for its life: a second call cannot
/// narrow it, and reopening its URI keeps it. A header is concealed only where every cell is.
#[test]
fn a_document_s_concealment_is_set_once_and_never_narrows() {
    let (mut store, id) = open();
    assert_eq!(store.concealed(id), None);
    store.set_concealed(id, Concealed::All).unwrap();
    store
        .set_concealed(id, Concealed::Columns(vec!["password".into()]))
        .unwrap();
    assert_eq!(store.concealed(id), Some(&Concealed::All));
    assert_eq!(store.open("mem://readme".into(), "again".into()), id);
    assert_eq!(store.concealed(id), Some(&Concealed::All));
    let columns = Concealed::Columns(vec!["password".into(), "number".into()]);
    assert!(columns.column("password") && columns.column("number"));
    assert!(!columns.column("addr") && !columns.column("=password"));
    assert!(!columns.headers() && Concealed::All.headers() && Concealed::All.column("addr"));
    assert_eq!(
        store.set_concealed(DocumentId(NonZeroU64::new(99).unwrap()), Concealed::All),
        Err(DocumentError::UnknownDocument)
    );
}

#[test]
fn a_read_only_document_refuses_every_change() {
    let (mut store, id) = open();
    assert!(!store.read_only(id));
    store.set_read_only(id).unwrap();
    assert!(store.read_only(id));
    let base = store.snapshot(id).unwrap().seq;
    let outcome = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 0..5,
            insert: "secret".into(),
        }],
    );
    assert!(
        matches!(
            outcome,
            DocumentTxnOutcome::Rejected(DocumentError::ReadOnly)
        ),
        "read-only: {outcome:?}"
    );
    assert!(matches!(
        store.replace_text(id, base, "replaced"),
        DocumentTxnOutcome::Rejected(DocumentError::ReadOnly)
    ));
    let snapshot = store.snapshot(id).unwrap();
    assert_eq!(snapshot.text.as_ref(), "hello\nworld");
    assert_eq!(snapshot.seq, base);
    assert_eq!(store.open("mem://readme".into(), "again".into()), id);
    assert!(store.read_only(id), "reopening keeps it read-only");
    assert_eq!(
        store.set_read_only(DocumentId(NonZeroU64::new(99).unwrap())),
        Err(DocumentError::UnknownDocument)
    );
}

#[test]
fn multi_edit_commits_one_seq_and_surface_matches_projection() {
    let (mut store, id) = open();
    let base = store.snapshot(id).unwrap().seq;
    let outcome = store.transact(
        id,
        base,
        vec![
            TextEdit {
                range: 0..5,
                insert: "goodbye".into(),
            },
            TextEdit {
                range: 6..11,
                insert: "moon".into(),
            },
        ],
    );
    let DocumentTxnOutcome::Committed { seq, .. } = outcome else {
        panic!("expected commit");
    };
    assert_eq!(seq.0, base.0 + 1, "one transaction is one spine event");
    assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "goodbye\nmoon");
    assert_eq!(store.surface_text(id).as_deref(), Some("goodbye\nmoon"));
}

#[test]
fn commit_publishes_the_same_seq_to_every_attached_controller() {
    let (mut store, id) = open();
    let markdown = DocumentViewId(11);
    let editor = DocumentViewId(12);
    store.attach_view(id, markdown).unwrap();
    store.attach_view(id, editor).unwrap();
    let base = store.snapshot(id).unwrap().seq;
    let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 5..5,
            insert: "!".into(),
        }],
    ) else {
        panic!("expected commit");
    };
    assert_eq!(store.observed_seq(id, markdown), Some(seq));
    assert_eq!(store.observed_seq(id, editor), Some(seq));
    assert_eq!(store.snapshot(id).unwrap().seq, seq);
}

#[test]
fn stale_or_invalid_transactions_change_nothing() {
    let (mut store, id) = open();
    let base = store.snapshot(id).unwrap().seq;
    let first = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 0..5,
            insert: "hi".into(),
        }],
    );
    assert!(matches!(first, DocumentTxnOutcome::Committed { .. }));
    let before = store.snapshot(id).unwrap();
    assert_eq!(
        store.transact(
            id,
            base,
            vec![TextEdit {
                range: 0..2,
                insert: "no".into(),
            }]
        ),
        DocumentTxnOutcome::Conflict {
            current: before.seq
        }
    );
    assert!(matches!(
        store.transact(
            id,
            before.seq,
            vec![TextEdit {
                range: 1..99,
                insert: String::new(),
            }]
        ),
        DocumentTxnOutcome::Rejected(DocumentError::InvalidRange)
    ));
    assert_eq!(store.snapshot(id).unwrap().text, before.text);
}

#[test]
fn final_view_of_dirty_shared_document_waits_for_durable_ack() {
    let (mut store, id) = open();
    let markdown = DocumentViewId(1);
    let editor = DocumentViewId(2);
    store.attach_view(id, markdown).unwrap();
    store.attach_view(id, editor).unwrap();
    let base = store.snapshot(id).unwrap().seq;
    let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 5..5,
            insert: "!".into(),
        }],
    ) else {
        panic!("commit");
    };

    assert!(matches!(
        store.prepare_close(id, &[editor]).unwrap(),
        DocumentCloseReadiness::Ready { .. }
    ));
    store.commit_detach(id, &[editor]).unwrap();
    assert!(matches!(
        store.prepare_close(id, &[markdown]).unwrap(),
        DocumentCloseReadiness::Pending { requested } if requested == seq
    ));
    assert_eq!(
        store.commit_detach(id, &[markdown]),
        Err(DocumentError::CloseNotReady)
    );
    assert!(matches!(
        store.checkpoint_ack(id, seq).unwrap(),
        DocumentCloseReadiness::Ready { .. }
    ));
    store.commit_detach(id, &[markdown]).unwrap();
    assert_eq!(store.view_count(id), Some(0));
}

#[test]
fn failed_checkpoint_keeps_open_documents_editable() {
    for attached in [false, true] {
        let (mut store, id) = open();
        assert_eq!(store.failed_checkpoint_seq(id), None);
        if attached {
            store.attach_view(id, DocumentViewId(7)).unwrap();
        }
        let phase = store.phase(id);
        assert_eq!(
            phase,
            Some(if attached {
                DocumentPhase::Active
            } else {
                DocumentPhase::Suspended
            })
        );
        let checkpoint = store.checkpoint_seq(id);
        let base = store.snapshot(id).unwrap().seq;
        let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
            id,
            base,
            vec![TextEdit {
                range: 0..0,
                insert: "x".into(),
            }],
        ) else {
            panic!("the open document accepts an edit");
        };

        assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(store.phase(id), phase);
        assert_eq!(store.checkpoint_seq(id), checkpoint);
        assert_eq!(store.view_count(id), Some(usize::from(attached)));
        let DocumentTxnOutcome::Committed { seq: later, .. } = store.transact(
            id,
            seq,
            vec![TextEdit {
                range: 0..0,
                insert: "y".into(),
            }],
        ) else {
            panic!("a failed ordinary checkpoint cannot freeze editing");
        };
        assert!(later > seq);
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "yxhello\nworld");
        assert_eq!(store.phase(id), phase);
        assert_eq!(store.dirty(id), Some(true));
        assert_eq!(store.checkpoint_seq(id), checkpoint);
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(
            store.checkpoint_ack(id, base).unwrap(),
            DocumentCloseReadiness::Pending { requested: later }
        );
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(
            store.checkpoint_ack(id, seq).unwrap(),
            DocumentCloseReadiness::Pending { requested: later }
        );
        assert_eq!(store.failed_checkpoint_seq(id), None);
        assert_eq!(store.dirty(id), Some(true));
        assert_eq!(
            store.checkpoint_ack(id, later).unwrap(),
            DocumentCloseReadiness::Ready { requested: later }
        );
        assert_eq!(store.phase(id), phase);
        assert_eq!(store.dirty(id), Some(false));
    }
}

#[test]
fn asking_whether_a_close_is_ready_starts_no_close() {
    let (mut store, id) = open();
    let (first, second) = (DocumentViewId(7), DocumentViewId(8));
    store.attach_view(id, first).unwrap();
    store.attach_view(id, second).unwrap();
    let base = store.snapshot(id).unwrap().seq;
    let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 0..0,
            insert: "x".into(),
        }],
    ) else {
        panic!("commit");
    };
    assert_eq!(
        store.close_ready(id, &[first]),
        Ok(true),
        "another view stays"
    );
    assert_eq!(store.close_ready(id, &[first, second]), Ok(false));
    assert_eq!(store.phase(id), Some(DocumentPhase::Active));
    assert_eq!(
        store.close_ready(id, &[DocumentViewId(9)]),
        Err(DocumentError::UnknownView)
    );
    store.checkpoint_ack(id, seq).unwrap();
    assert_eq!(store.close_ready(id, &[first, second]), Ok(true));
    assert_eq!(store.phase(id), Some(DocumentPhase::Active));
}

#[test]
fn failed_checkpoint_preserves_views_until_retry() {
    let (mut store, id) = open();
    let view = DocumentViewId(7);
    store.attach_view(id, view).unwrap();
    let base = store.snapshot(id).unwrap().seq;
    let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
        id,
        base,
        vec![TextEdit {
            range: 0..0,
            insert: "x".into(),
        }],
    ) else {
        panic!("commit");
    };
    assert_eq!(
        store.prepare_close(id, &[view]).unwrap(),
        DocumentCloseReadiness::Pending { requested: seq }
    );
    assert_eq!(
        store.phase(id),
        Some(DocumentPhase::Closing { requested: seq })
    );
    let late_edit = || {
        vec![TextEdit {
            range: 0..0,
            insert: "late".into(),
        }]
    };
    assert_eq!(
        store.transact(id, seq, late_edit()),
        DocumentTxnOutcome::Rejected(DocumentError::Closing)
    );
    for _ in 0..2 {
        assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(
            store.phase(id),
            Some(DocumentPhase::Blocked { requested: seq })
        );
        assert_eq!(
            store.prepare_close(id, &[view]).unwrap(),
            DocumentCloseReadiness::Blocked { requested: seq }
        );
        assert_eq!(
            store.transact(id, seq, late_edit()),
            DocumentTxnOutcome::Rejected(DocumentError::Closing)
        );
        assert_eq!(store.snapshot(id).unwrap().seq, seq);
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "xhello\nworld");
        assert_eq!(store.checkpoint_seq(id), Some(base));
        assert_eq!(store.view_count(id), Some(1));
    }
    store.abort_close(id).unwrap();
    assert_eq!(store.phase(id), Some(DocumentPhase::Active));
    assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
    assert_eq!(
        store.prepare_close(id, &[view]).unwrap(),
        DocumentCloseReadiness::Pending { requested: seq }
    );
    assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
    assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
    assert_eq!(
        store.checkpoint_retry(id).unwrap(),
        DocumentCloseReadiness::Pending { requested: seq }
    );
    assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
    assert_eq!(
        store.phase(id),
        Some(DocumentPhase::Closing { requested: seq })
    );
    assert_eq!(
        store.commit_detach(id, &[view]),
        Err(DocumentError::CloseNotReady)
    );
    assert_eq!(
        store.checkpoint_ack(id, seq).unwrap(),
        DocumentCloseReadiness::Ready { requested: seq }
    );
    assert_eq!(store.failed_checkpoint_seq(id), None);
    assert_eq!(
        store.transact(id, seq, late_edit()),
        DocumentTxnOutcome::Rejected(DocumentError::Closing)
    );
    store.commit_detach(id, &[view]).unwrap();
    assert_eq!(store.view_count(id), Some(0));
    assert_eq!(store.phase(id), Some(DocumentPhase::Suspended));
    assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "xhello\nworld");
}

/// An ack past head is refused: `NativeSaveIntentLatch` relies on this for `durable <= head`.
#[test]
fn checkpoint_ack_past_head_is_refused_without_moving_the_checkpoint() {
    let (mut store, id) = open();
    let head = store.snapshot(id).unwrap().seq;
    let before = store.checkpoint_seq(id);
    assert_eq!(store.checkpoint_fail(id).unwrap(), head);
    assert_eq!(store.failed_checkpoint_seq(id), Some(head));
    // Even a clean document's failed save remains evidence: the previously
    // durable head is not a new acknowledgement of that failure.
    let older = Seq(head
        .0
        .checked_sub(1)
        .expect("opening appends a spine event"));
    assert_eq!(
        store.checkpoint_ack(id, older).unwrap(),
        DocumentCloseReadiness::Ready { requested: head }
    );
    assert_eq!(store.checkpoint_seq(id), before);
    assert_eq!(store.failed_checkpoint_seq(id), Some(head));
    assert_eq!(
        store.checkpoint_ack(id, Seq(head.0 + 1)),
        Err(DocumentError::CheckpointAheadOfHead)
    );
    assert_eq!(store.checkpoint_seq(id), before);
    assert_eq!(store.failed_checkpoint_seq(id), Some(head));
    assert_eq!(
        store.checkpoint_ack(id, head).unwrap(),
        DocumentCloseReadiness::Ready { requested: head }
    );
    assert_eq!(store.failed_checkpoint_seq(id), None);
}

#[test]
fn batch_detach_is_atomic_across_documents() {
    let mut store = DocumentStore::for_test();
    let first = store.open("mem://batch/first".into(), "one".into());
    let second = store.open("mem://batch/second".into(), "two".into());
    let first_view = DocumentViewId(31);
    let second_view = DocumentViewId(32);
    store.attach_view(first, first_view).unwrap();
    store.attach_view(second, second_view).unwrap();
    let first_seq = match store.transact(
        first,
        store.snapshot(first).unwrap().seq,
        vec![TextEdit {
            range: 3..3,
            insert: "!".into(),
        }],
    ) {
        DocumentTxnOutcome::Committed { seq, .. } => seq,
        other => panic!("first edit failed: {other:?}"),
    };
    let second_seq = match store.transact(
        second,
        store.snapshot(second).unwrap().seq,
        vec![TextEdit {
            range: 3..3,
            insert: "!".into(),
        }],
    ) {
        DocumentTxnOutcome::Committed { seq, .. } => seq,
        other => panic!("second edit failed: {other:?}"),
    };
    store.prepare_close(first, &[first_view]).unwrap();
    store.prepare_close(second, &[second_view]).unwrap();
    store.checkpoint_ack(first, first_seq).unwrap();

    let batch = vec![(first, vec![first_view]), (second, vec![second_view])];
    assert_eq!(
        store.commit_detach_batch(&batch),
        Err(DocumentError::CloseNotReady)
    );
    assert_eq!(store.view_count(first), Some(1));
    assert_eq!(store.view_count(second), Some(1));

    store.checkpoint_ack(second, second_seq).unwrap();
    store.commit_detach_batch(&batch).unwrap();
    assert_eq!(store.view_count(first), Some(0));
    assert_eq!(store.view_count(second), Some(0));

    let shared = store.open("mem://batch/shared".into(), "three".into());
    let shared_a = DocumentViewId(33);
    let shared_b = DocumentViewId(34);
    store.attach_view(shared, shared_a).unwrap();
    store.attach_view(shared, shared_b).unwrap();
    let shared_seq = match store.transact(
        shared,
        store.snapshot(shared).unwrap().seq,
        vec![TextEdit {
            range: 5..5,
            insert: "!".into(),
        }],
    ) {
        DocumentTxnOutcome::Committed { seq, .. } => seq,
        other => panic!("shared edit failed: {other:?}"),
    };
    let split_batch = vec![(shared, vec![shared_a]), (shared, vec![shared_b])];
    assert_eq!(
        store.commit_detach_batch(&split_batch),
        Err(DocumentError::CloseNotReady)
    );
    assert_eq!(store.phase(shared), Some(DocumentPhase::Active));
    assert_eq!(store.view_count(shared), Some(2));
    store.checkpoint_ack(shared, shared_seq).unwrap();
    store
        .commit_detach_batch(&[(shared, vec![shared_a, shared_a]), (shared, vec![shared_b])])
        .unwrap();
    assert_eq!(store.view_count(shared), Some(0));
}

#[test]
fn central_view_detach_is_idempotent_and_fails_closed_when_dirty() {
    let (mut store, id) = open();
    let view = DocumentViewId(41);
    store.attach_view(id, view).unwrap();
    let seq = match store.transact(
        id,
        store.snapshot(id).unwrap().seq,
        vec![TextEdit {
            range: 0..0,
            insert: "dirty".into(),
        }],
    ) {
        DocumentTxnOutcome::Committed { seq, .. } => seq,
        other => panic!("edit failed: {other:?}"),
    };
    assert_eq!(
        store.detach_view_if_ready(id, view),
        Err(DocumentError::CloseNotReady)
    );
    assert_eq!(store.view_count(id), Some(1));
    store.checkpoint_ack(id, seq).unwrap();
    assert_eq!(store.detach_view_if_ready(id, view), Ok(true));
    assert_eq!(store.detach_view_if_ready(id, view), Ok(false));
}

#[test]
fn replacing_the_text_keeps_positions_outside_the_change_on_their_text() {
    let before = "a = 1\nb = 2\nc = 3\nd = 4\n";
    let after = "a = 1\nc = 3\nd = 4\n";
    let caret = before.find("c = 3").unwrap();
    let later = before.find("d = 4").unwrap() + 2;

    let mut store = DocumentStore::for_test();
    let id = store.open("mem://config".into(), before.into());
    let base = store.snapshot(id).unwrap().seq;
    let DocumentTxnOutcome::Committed { deltas, .. } = store.replace_text(id, base, after) else {
        panic!("replace_text did not commit");
    };
    assert_eq!(store.snapshot(id).unwrap().text.as_ref(), after);
    assert_eq!(rebase_position(0, &deltas), 0, "before the change");
    assert_eq!(
        rebase_position(caret, &deltas),
        after.find("c = 3").unwrap(),
        "the line after a deleted one keeps its caret"
    );
    assert_eq!(
        rebase_position(later, &deltas),
        after.find("d = 4").unwrap() + 2
    );

    // The control: the whole-text edit this replaces sends every position to the end.
    let whole = [EditDelta {
        old: 0..before.len(),
        inserted_len: after.len(),
    }];
    assert_eq!(rebase_position(caret, &whole), after.len());
}

#[test]
fn replacing_with_the_same_text_commits_nothing() {
    let (mut store, id) = open();
    let base = store.snapshot(id).unwrap().seq;
    let outcome = store.replace_text(id, base, "hello\nworld");
    assert!(
        matches!(&outcome, DocumentTxnOutcome::Committed { seq, deltas, .. } if *seq == base && deltas.is_empty()),
        "{outcome:?}"
    );
}

#[test]
fn rebase_positions_tracks_atomic_deltas() {
    let deltas = vec![
        EditDelta {
            old: 2..4,
            inserted_len: 5,
        },
        EditDelta {
            old: 8..9,
            inserted_len: 0,
        },
    ];
    assert_eq!(rebase_position(1, &deltas), 1);
    assert_eq!(rebase_position(3, &deltas), 7);
    assert_eq!(rebase_position(10, &deltas), 12);

    let coordinates_stay_in_the_pre_transaction_snapshot = vec![
        EditDelta {
            old: 0..0,
            inserted_len: 10,
        },
        EditDelta {
            old: 10..12,
            inserted_len: 0,
        },
    ];
    assert_eq!(
        rebase_position(6, &coordinates_stay_in_the_pre_transaction_snapshot),
        16
    );
    let huge = (isize::MAX as usize) + 5;
    let past_isize = vec![EditDelta {
        old: 0..2,
        inserted_len: huge,
    }];
    assert_eq!(rebase_position(3, &past_isize), huge + 1);
}
