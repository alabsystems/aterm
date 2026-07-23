// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the production draft-journal serializer.

#![cfg(test)]

use aterm_buffer::Seq;
use aterm_spec::derive::{Model, native_draft_journal_model};
use aterm_spec::interp::{State, admits};

use crate::document_store::{DocumentId, DocumentStore, TextEdit};
use crate::native_document_io::{
    AtomicSaveProof, AtomicSaveResult, JournalAppendProof, JournalAppendResult, JournalGeneration,
    ObservedFileVersion, SaveReducer, SaveReduction,
};
use crate::native_document_journal::{
    DocumentJournalStore, JournalCompletion, JournalEffect, execute_journal_append,
    execute_journal_rewrite,
};

#[derive(Clone, Copy)]
struct Protocol {
    origin: Seq,
    inflight: i64,
    target: Seq,
    generation: i64,
    file_durable: Seq,
    baseline: Seq,
    checkpoint_ready: bool,
    stale_rejected: i64,
}

fn relative(seq: Seq, origin: Seq) -> i64 {
    i64::try_from(seq.0.saturating_sub(origin.0)).expect("bounded conformance sequence")
}

fn projection(
    model: &Model,
    store: &DocumentStore,
    journals: &DocumentJournalStore,
    document: DocumentId,
    protocol: Protocol,
) -> State {
    let head = store.snapshot(document).unwrap().seq;
    let durable = journals.durable_seq(document).unwrap();
    let mut state = model.init_state();
    state.insert("edit_seq", relative(head, protocol.origin));
    state.insert("desired_seq", relative(head, protocol.origin));
    state.insert("durable_seq", relative(durable, protocol.origin));
    state.insert("inflight", protocol.inflight);
    state.insert("target_seq", relative(protocol.target, protocol.origin));
    state.insert("generation", protocol.generation);
    state.insert(
        "file_durable_seq",
        relative(protocol.file_durable, protocol.origin),
    );
    state.insert("baseline_seq", relative(protocol.baseline, protocol.origin));
    state.insert("checkpoint_ready", i64::from(protocol.checkpoint_ready));
    state.insert("stale_rejected", protocol.stale_rejected);
    state.insert("stale_accepted", 0);
    state.insert("unsafe_prune", 0);
    state
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        admits(model, before, after),
        Some(action),
        "shipping transition must be admitted specifically as {action}"
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

fn append(store: &mut DocumentStore, document: DocumentId, text: &str) {
    let snapshot = store.snapshot(document).unwrap();
    let outcome = store.transact(
        document,
        snapshot.seq,
        vec![TextEdit {
            range: snapshot.text.len()..snapshot.text.len(),
            insert: text.to_string(),
        }],
    );
    assert!(matches!(
        outcome,
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
}

#[test]
fn production_serializer_conforms_and_negative_controls_are_rejected() {
    let model = native_draft_journal_model();
    let root =
        std::env::temp_dir().join(format!("aterm-journal-conformance-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = DocumentStore::new();
    let uri = "file:///tmp/journal-conformance.md";
    let document = store.open(uri.to_string(), "base".to_string());
    let disk = store.snapshot(document).unwrap();
    let decision = journals.inspect_open(uri, disk.text.as_bytes()).unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();
    let mut protocol = Protocol {
        origin: disk.seq,
        inflight: 0,
        target: disk.seq,
        generation: 0,
        file_durable: disk.seq,
        baseline: disk.seq,
        checkpoint_ready: false,
        stale_rejected: 0,
    };

    // A genuine committed edit becomes the latest desired journal head.
    let before = projection(&model, &store, &journals, document, protocol);
    append(&mut store, document, "-one");
    journals
        .observe_commit(&store.snapshot(document).unwrap())
        .unwrap();
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "Edit");

    // The real reducer admits one in-flight record and freezes its exact target.
    let before = after;
    let JournalEffect::Append {
        path,
        key,
        plan: first,
    } = journals.next_effect(document).unwrap().unwrap()
    else {
        panic!("journal record expected")
    };
    protocol.inflight = 1;
    protocol.target = first.target_seq;
    protocol.generation += 1;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "BeginJournal");

    // A rapid edit while fsync is pending replaces desired, not the frozen plan.
    let before = after;
    append(&mut store, document, "-two");
    journals
        .observe_commit(&store.snapshot(document).unwrap())
        .unwrap();
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "Edit");

    let before = after;
    let result = execute_journal_append(&path, key, &first);
    assert!(matches!(
        journals.complete_append(document, first.generation, result),
        JournalCompletion::Durable { seq, .. } if seq == first.target_seq
    ));
    protocol.inflight = 0;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "AcceptJournal");

    // Catch-up plans directly to the latest desired head. A wrong generation is
    // a genuine stale decision and changes no durable reducer state.
    let before = after;
    let JournalEffect::Append {
        path,
        key,
        plan: latest,
    } = journals.next_effect(document).unwrap().unwrap()
    else {
        panic!("catch-up record expected")
    };
    protocol.inflight = 1;
    protocol.target = latest.target_seq;
    protocol.generation += 1;
    let after_begin = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after_begin, "BeginJournal");

    let before_stale = after_begin;
    let stale = journals.complete_append(
        document,
        JournalGeneration(latest.generation.0 + 1),
        JournalAppendResult::Committed(JournalAppendProof {
            appended_len: latest.bytes.len(),
            encoded_fingerprint: latest.encoded_fingerprint,
            file_synced: true,
            renamed_over_journal: true,
            directory_synced: true,
        }),
    );
    assert_eq!(stale, JournalCompletion::Stale);
    protocol.stale_rejected += 1;
    let after_stale = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before_stale, &after_stale, "RejectStaleProof");

    // Negative control: accepting that stale proof as future durability is not a
    // Buggy=0 transition and violates both exact-proof invariants.
    let mut stale_accept = before_stale.clone();
    stale_accept.insert("durable_seq", before_stale["desired_seq"] + 1);
    stale_accept.insert("stale_accepted", 1);
    assert_eq!(admits(&model, &before_stale, &stale_accept), None);
    assert!(!model.check_invariant("NeverAckFuture", &stale_accept));
    assert!(!model.check_invariant("StaleProofIsNoOp", &stale_accept));

    let before = after_stale;
    let result = execute_journal_append(&path, key, &latest);
    assert!(matches!(
        journals.complete_append(document, latest.generation, result),
        JournalCompletion::Durable { seq, .. } if seq == latest.target_seq
    ));
    protocol.inflight = 0;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "AcceptJournal");

    // The accepted atomic file-save proof arms a checkpoint baseline.
    let saved = store.snapshot(document).unwrap();
    let before = after;
    let mut save = SaveReducer::new(
        document,
        ObservedFileVersion::observed(b"older", Some(7), Some(1)),
    );
    let save_plan = save.begin(&saved).unwrap();
    let checkpoint = match save.complete(
        save_plan.generation,
        AtomicSaveResult::Committed(AtomicSaveProof {
            observed: ObservedFileVersion::observed(&save_plan.bytes, Some(7), Some(2)),
            temporary_synced: true,
            renamed_over_target: true,
            directory_synced: true,
        }),
    ) {
        SaveReduction::Durable(checkpoint) => checkpoint,
        other => panic!("exact atomic-save proof was not accepted: {other:?}"),
    };
    journals
        .request_checkpoint(checkpoint, saved.text.clone())
        .unwrap();
    protocol.file_durable = saved.seq;
    protocol.checkpoint_ready = true;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "ProveFileSave");

    // A newer edit remains in the rewritten journal beyond the saved baseline.
    let before = after;
    append(&mut store, document, "-newer");
    journals
        .observe_commit(&store.snapshot(document).unwrap())
        .unwrap();
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "Edit");

    // Negative control: pruning through the newer draft without another file
    // proof is forbidden and the executable model rejects it.
    let mut unsafe_prune = after.clone();
    unsafe_prune.insert("baseline_seq", after["desired_seq"]);
    unsafe_prune.insert("unsafe_prune", 1);
    assert_eq!(admits(&model, &after, &unsafe_prune), None);
    assert!(!model.check_invariant("PruneOnlyAfterFileDurable", &unsafe_prune));
    assert!(!model.check_invariant("NoUnsafePrune", &unsafe_prune));

    let before = after;
    let JournalEffect::Rewrite { path, plan } = journals.next_effect(document).unwrap().unwrap()
    else {
        panic!("proof-gated checkpoint rewrite expected")
    };
    protocol.inflight = 2;
    protocol.target = plan.target_seq;
    protocol.baseline = saved.seq;
    protocol.checkpoint_ready = false;
    protocol.generation += 1;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "BeginCheckpoint");

    let before = after;
    let result = execute_journal_rewrite(&path, &plan);
    assert!(matches!(
        journals.complete_rewrite(document, plan.generation, result),
        JournalCompletion::Durable { seq, .. } if seq == plan.target_seq
    ));
    protocol.inflight = 0;
    let after = projection(&model, &store, &journals, document, protocol);
    assert_transition(&model, &before, &after, "AcceptCheckpoint");
    assert_eq!(
        journals.durable_seq(document),
        Some(store.snapshot(document).unwrap().seq)
    );
    let _ = std::fs::remove_dir_all(root);
}
