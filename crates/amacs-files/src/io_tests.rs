// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use amacs::store::{DocumentStore, DocumentTxnOutcome, TextEdit};
use aterm_models_native::{config_file_commit_cas_model, native_file_watch_model};
use aterm_spec::interp::admits;

fn snapshot(text: &str) -> (DocumentStore, DocumentId, DocumentSnapshot) {
    let mut store = crate::test_document_store();
    let id = store.open("file:///draft.md".to_string(), text.to_string());
    let snapshot = store.snapshot(id).expect("document snapshot");
    (store, id, snapshot)
}

fn changed_snapshot(
    store: &mut DocumentStore,
    id: DocumentId,
    prior: &DocumentSnapshot,
    range: Range<usize>,
    insert: &str,
) -> DocumentSnapshot {
    let outcome = store.transact(
        id,
        prior.seq,
        vec![TextEdit {
            range,
            insert: insert.to_string(),
        }],
    );
    assert!(matches!(outcome, DocumentTxnOutcome::Committed { .. }));
    store.snapshot(id).expect("updated snapshot")
}

fn save_proof(plan: &SavePlan, stamp: u64) -> AtomicSaveProof {
    AtomicSaveProof {
        observed: ObservedFileVersion::observed(&plan.file_bytes, Some(7), Some(stamp)),
        temporary_synced: true,
        renamed_over_target: true,
        directory_synced: true,
    }
}

fn append_proof(plan: &JournalAppendPlan) -> JournalAppendProof {
    let (_, tail) = encode_journal_image(&[JournalRecord {
        document_raw: 1,
        base_seq: plan.target_seq,
        seq: plan.target_seq,
        payload: JournalPayload::Snapshot(String::new()),
    }])
    .unwrap();
    JournalAppendProof {
        record_len: plan.bytes.len(),
        encoded_fingerprint: plan.encoded_fingerprint,
        published: tail,
        file_synced: true,
        landing: JournalLanding::Appended,
    }
}

#[test]
fn conflict_detection_checks_existence_identity_content_and_mtime() {
    let base = ObservedFileVersion::observed(b"old", Some(3), Some(10));
    assert_eq!(detect_version_conflict(base, base), None);
    assert_eq!(
        detect_version_conflict(base, ObservedFileVersion::missing()),
        Some(VersionConflict::Existence)
    );
    assert_eq!(
        detect_version_conflict(
            base,
            ObservedFileVersion::observed(b"old", Some(4), Some(10))
        ),
        Some(VersionConflict::Identity)
    );
    assert_eq!(
        detect_version_conflict(
            base,
            ObservedFileVersion::observed(b"new", Some(3), Some(10))
        ),
        Some(VersionConflict::Content)
    );
    assert_eq!(
        detect_version_conflict(
            base,
            ObservedFileVersion::observed(b"old", Some(3), Some(11))
        ),
        Some(VersionConflict::Metadata)
    );
}

#[test]
fn file_watch_reducer_reloads_only_clean_idle_documents_and_conforms() {
    let baseline = ObservedFileVersion::observed(b"old", Some(3), Some(10));
    let observed = ObservedFileVersion::observed(b"new", Some(3), Some(11));
    let reduce = |document_dirty, save_in_flight| {
        reduce_file_watch(FileWatchInput {
            baseline,
            observed,
            document_dirty,
            save_in_flight,
        })
    };
    assert!(matches!(
        reduce(false, false),
        FileWatchReduction::ReloadClean {
            change: VersionConflict::Content
        }
    ));
    assert!(matches!(
        reduce(true, false),
        FileWatchReduction::ConflictDirty {
            change: VersionConflict::Content
        }
    ));
    assert!(matches!(
        reduce(false, true),
        FileWatchReduction::DeferredSaving {
            change: VersionConflict::Content
        }
    ));
    assert_eq!(
        reduce_file_watch(FileWatchInput {
            baseline,
            observed: baseline,
            document_dirty: true,
            save_in_flight: true,
        }),
        FileWatchReduction::Unchanged
    );

    let equivalent_metadata = ObservedFileVersion::observed(b"old", Some(3), Some(11));
    let equivalent_identity = ObservedFileVersion::observed(b"old", Some(4), Some(11));
    for equivalent in [equivalent_metadata, equivalent_identity] {
        assert!(matches!(
            reduce_file_watch(FileWatchInput {
                baseline,
                observed: equivalent,
                document_dirty: true,
                save_in_flight: false,
            }),
            FileWatchReduction::RebindEquivalent { .. }
        ));
        assert!(matches!(
            reduce_file_watch(FileWatchInput {
                baseline,
                observed: equivalent,
                document_dirty: true,
                save_in_flight: true,
            }),
            FileWatchReduction::DeferredSaving { .. }
        ));
    }
    assert!(matches!(
        reduce_file_watch(FileWatchInput {
            baseline,
            observed,
            document_dirty: true,
            save_in_flight: false,
        }),
        FileWatchReduction::ConflictDirty {
            change: VersionConflict::Content
        }
    ));

    assert_ne!(reduce(true, false), reduce(false, false));
    assert_ne!(reduce(false, true), reduce(false, false));

    let model = native_file_watch_model();
    for changed in [false, true] {
        for equivalent in [false, true] {
            for document_dirty in [false, true] {
                for save_in_flight in [false, true] {
                    let observed = if !changed {
                        baseline
                    } else if equivalent {
                        equivalent_metadata
                    } else {
                        observed
                    };
                    let reduction = reduce_file_watch(FileWatchInput {
                        baseline,
                        observed,
                        document_dirty,
                        save_in_flight,
                    });
                    let verdict = match reduction {
                        FileWatchReduction::Unchanged => 1,
                        FileWatchReduction::ReloadClean { .. } => 2,
                        FileWatchReduction::ConflictDirty { .. } => 3,
                        FileWatchReduction::DeferredSaving { .. } => 4,
                        FileWatchReduction::RebindEquivalent { .. } => 5,
                    };
                    let mut before = model.init_state();
                    before.insert("changed", i64::from(changed));
                    before.insert("equivalent", i64::from(equivalent));
                    before.insert("dirty", i64::from(document_dirty));
                    before.insert("saving", i64::from(save_in_flight));
                    let mut after = before.clone();
                    after.insert("verdict", verdict);
                    assert_eq!(admits(&model, &before, &after), Some("Resolve"));
                    assert!(model.check_invariant("PriorityIsDeterministic", &after));
                }
            }
        }
    }

    let mut before = model.init_state();
    before.insert("changed", 1);
    before.insert("equivalent", 0);
    before.insert("dirty", 1);
    before.insert("saving", 1);
    let mut inverted = before.clone();
    inverted.insert("verdict", 3);
    assert_eq!(admits(&model, &before, &inverted), None);
    assert!(!model.check_invariant("PriorityIsDeterministic", &inverted));
}

#[test]
fn stale_save_completion_cannot_ack_superseding_plan() {
    let (mut store, id, first) = snapshot("a");
    let mut reducer = SaveReducer::new(id, ObservedFileVersion::missing());
    let old = reducer.begin(&first).unwrap();
    let second = changed_snapshot(&mut store, id, &first, 1..1, "b");
    let current = reducer.begin(&second).unwrap();
    assert_eq!(
        reducer.complete(
            old.generation,
            AtomicSaveResult::Committed(save_proof(&old, 1))
        ),
        SaveReduction::Stale
    );
    assert_eq!(reducer.pending(), Some(&current));
    let reduction = reducer.complete(
        current.generation,
        AtomicSaveResult::Committed(save_proof(&current, 2)),
    );
    assert_eq!(
        reduction,
        SaveReduction::Durable(DurableCheckpoint {
            document: id,
            seq: second.seq,
            source: DurableSource::AtomicFile,
        })
    );
}

#[test]
fn incomplete_or_mismatched_atomic_proof_fails_closed() {
    let (_store, id, snapshot) = snapshot("draft");
    let mut reducer = SaveReducer::new(id, ObservedFileVersion::missing());
    let plan = reducer.begin(&snapshot).unwrap();
    let mut proof = save_proof(&plan, 1);
    proof.directory_synced = false;
    let reduced = reducer.complete(plan.generation, AtomicSaveResult::Committed(proof));
    assert!(matches!(
        reduced,
        SaveReduction::Failed {
            stage: AtomicSaveStage::VerifyProof,
            ..
        }
    ));
    assert!(!matches!(reducer.phase(), SavePhase::Saved { .. }));
}

#[test]
fn published_unverified_requires_explicit_reconciliation_before_retry() {
    let (_store, id, snapshot) = snapshot("draft");
    let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
    let visible = ObservedFileVersion::observed(b"draft", Some(2), Some(2));
    let mut reducer = SaveReducer::new(id, original);
    let plan = reducer.begin(&snapshot).unwrap();
    let reduced = reducer.complete(
        plan.generation,
        AtomicSaveResult::PublishedUnverified {
            stage: AtomicSaveStage::SyncDirectory,
            observed: Some(visible),
            message: "directory sync failed".to_string(),
            cause: None,
        },
    );
    assert!(matches!(
        reduced,
        SaveReduction::ReconcileRequired {
            expected,
            observed: Some(actual),
            stage: AtomicSaveStage::SyncDirectory,
            ..
        } if expected == original && actual == visible
    ));
    assert_eq!(
        reducer.observed(),
        original,
        "old baseline must not advance"
    );
    assert_eq!(reducer.begin(&snapshot), Err(SaveError::ReconcileRequired));

    let model = config_file_commit_cas_model();
    let begun = model.successors("BeginManual", &model.init_state())[0].clone();
    let locked = model.successors("LockManual", &begun)[0].clone();
    let indeterminate = model.successors("ResolveManualIndeterminate", &locked)[0].clone();
    assert_eq!(indeterminate["manual_phase"], 5);
    assert_eq!(indeterminate["manual_committed"], 0);
    let rejected = model.successors("RetryIndeterminate", &indeterminate);
    assert_eq!(rejected, vec![indeterminate.clone()]);
    assert!(model.check_invariant("IndeterminateDoesNotClaimDurability", &indeterminate));
    let claimed = aterm_spec::interp::with_buggy(&model, 1)
        .successors("ResolveManualIndeterminate", &locked)[0]
        .clone();
    assert_eq!(claimed["manual_committed"], 1);
    assert_eq!(aterm_spec::interp::admits(&model, &locked, &claimed), None);
    assert!(!model.check_invariant("IndeterminateDoesNotClaimDurability", &claimed));

    reducer.accept_observation(visible).unwrap();
    assert_eq!(reducer.begin(&snapshot).unwrap().expected, visible);
}

#[test]
fn external_change_becomes_explicit_conflict_until_user_accepts_observation() {
    let (_store, id, snapshot) = snapshot("ours");
    let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
    let external = ObservedFileVersion::observed(b"theirs", Some(1), Some(2));
    let mut reducer = SaveReducer::new(id, original);
    let plan = reducer.begin(&snapshot).unwrap();
    assert_eq!(
        reducer.complete(
            plan.generation,
            AtomicSaveResult::Conflict {
                observed: external,
                equivalent_rebind_allowed: true,
            }
        ),
        SaveReduction::Conflict(VersionConflict::Content)
    );
    assert_eq!(reducer.observed(), original);
    assert_eq!(
        reducer.begin(&snapshot),
        Err(SaveError::ConflictRequiresReconcile),
        "a stale conflict baseline cannot authorize a blind retry"
    );
    reducer.accept_observation(external).unwrap();
    assert_eq!(reducer.observed(), external);
    assert_eq!(reducer.begin(&snapshot).unwrap().expected, external);
}

#[test]
fn equivalent_generation_save_conflict_rebinds_without_authorizing_stale_write() {
    let (_store, id, snapshot) = snapshot("ours");
    let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
    let replaced = ObservedFileVersion::observed(b"base", Some(2), Some(2));

    let mut unvalidated = SaveReducer::new(id, original);
    let unvalidated_plan = unvalidated.begin(&snapshot).unwrap();
    assert_eq!(
        unvalidated.complete(
            unvalidated_plan.generation,
            AtomicSaveResult::Conflict {
                observed: original,
                equivalent_rebind_allowed: false,
            }
        ),
        SaveReduction::Conflict(VersionConflict::Identity),
        "same bytes cannot rebind an invalidated path or symlink capability"
    );
    assert_eq!(unvalidated.observed(), original);

    let mut reducer = SaveReducer::new(id, original);
    let stale_plan = reducer.begin(&snapshot).unwrap();
    assert_eq!(
        stale_plan.preflight(replaced),
        Err(VersionConflict::Identity)
    );
    assert_eq!(
        reducer.complete(
            stale_plan.generation,
            AtomicSaveResult::Conflict {
                observed: replaced,
                equivalent_rebind_allowed: true,
            }
        ),
        SaveReduction::ReboundEquivalent(VersionConflict::Identity)
    );
    assert_eq!(reducer.observed(), replaced);
    assert_eq!(reducer.phase(), &SavePhase::Idle);

    let rebound_plan = reducer.begin(&snapshot).unwrap();
    assert_eq!(rebound_plan.expected, replaced);
    assert_eq!(rebound_plan.preflight(replaced), Ok(()));

    let model = native_file_watch_model();
    let mut before = model.init_state();
    before.insert("changed", 1);
    before.insert("equivalent", 1);
    before.insert("dirty", 1);
    before.insert("saving", 0);
    let mut after = before.clone();
    after.insert("verdict", 5);
    assert_eq!(admits(&model, &before, &after), Some("Resolve"));
    assert!(model.check_invariant("PriorityIsDeterministic", &after));

    let mut unsafe_conflict = before.clone();
    unsafe_conflict.insert("verdict", 3);
    assert_eq!(admits(&model, &before, &unsafe_conflict), None);
    assert!(!model.check_invariant("PriorityIsDeterministic", &unsafe_conflict));
}

#[test]
fn record_round_trip_preserves_snapshot_and_unicode_delta() {
    let (_store, id, snapshot) = snapshot("aλc");
    let first = JournalRecord::snapshot(&snapshot);
    let second = JournalRecord::delta(
        id,
        snapshot.seq,
        Seq(snapshot.seq.0 + 1),
        vec![JournalEdit {
            range: 1..3,
            insert: "β".to_string(),
        }],
    )
    .unwrap();
    let bytes = encode_journal(&[first.clone(), second.clone()]).unwrap();
    assert_eq!(decode_journal(&bytes).unwrap(), vec![first, second]);
}

#[test]
fn recovery_replays_incremental_records_to_latest_durable_seq() {
    let (_store, id, snapshot) = snapshot("one two");
    let seq2 = Seq(snapshot.seq.0 + 1);
    let seq3 = Seq(snapshot.seq.0 + 2);
    let records = vec![
        JournalRecord::snapshot(&snapshot),
        JournalRecord::delta(
            id,
            snapshot.seq,
            seq2,
            vec![JournalEdit {
                range: 0..3,
                insert: "ONE".to_string(),
            }],
        )
        .unwrap(),
        JournalRecord::delta(
            id,
            seq2,
            seq3,
            vec![JournalEdit {
                range: 7..7,
                insert: "!".to_string(),
            }],
        )
        .unwrap(),
    ];
    let recovered = recover_journal(id, &encode_journal(&records).unwrap()).unwrap();
    assert_eq!(recovered.durable_seq, seq3);
    assert_eq!(recovered.text, "ONE two!");
    assert_eq!(recovered.record_count, 3);
}

#[test]
fn format_1_checksum_corruption_rejects_the_whole_journal_not_a_valid_prefix() {
    let (_store, id, snapshot) = snapshot("draft");
    let first = JournalRecord::snapshot(&snapshot);
    let second = JournalRecord::delta(
        id,
        snapshot.seq,
        Seq(snapshot.seq.0 + 1),
        vec![JournalEdit {
            range: 5..5,
            insert: "!".to_string(),
        }],
    )
    .unwrap();
    let mut bytes = encode_journal_v1(&[first, second]).unwrap();
    let last_payload_byte = bytes.len() - JOURNAL_CHECKSUM_LEN - 1;
    bytes[last_payload_byte] ^= 0x40;
    assert_eq!(decode_journal(&bytes), Err(JournalError::ChecksumMismatch));
    assert_eq!(
        recover_journal(id, &bytes),
        Err(JournalError::ChecksumMismatch)
    );
}

#[test]
fn truncated_and_unknown_version_records_are_rejected() {
    let (_store, _id, snapshot) = snapshot("draft");
    let bytes = encode_journal_record(&JournalRecord::snapshot(&snapshot)).unwrap();
    assert_eq!(
        decode_journal(&bytes[..bytes.len() - 1]),
        Err(JournalError::Truncated)
    );
    let mut future = bytes;
    future[4..6].copy_from_slice(&(JOURNAL_VERSION + 1).to_le_bytes());
    assert_eq!(
        decode_journal(&future),
        Err(JournalError::UnsupportedVersion)
    );
}

#[test]
fn recovery_rejects_wrong_document_sequence_gap_and_utf8_split() {
    let (mut store, id_a, snapshot_a) = snapshot("λx");
    let id_b = store.open("file:///other".to_string(), String::new());
    assert_eq!(
        recover_journal(
            id_b,
            &encode_journal(&[JournalRecord::snapshot(&snapshot_a)]).unwrap()
        ),
        Err(JournalError::WrongDocument)
    );

    let gap = JournalRecord {
        document_raw: id_a.get(),
        base_seq: snapshot_a.seq,
        seq: Seq(snapshot_a.seq.0 + 1),
        payload: JournalPayload::Delta(vec![JournalEdit {
            range: 1..2,
            insert: "z".to_string(),
        }]),
    };
    let bytes = encode_journal(&[JournalRecord::snapshot(&snapshot_a), gap]).unwrap();
    assert_eq!(
        recover_journal(id_a, &bytes),
        Err(JournalError::InvalidEdit)
    );

    let missing = JournalRecord::delta(
        id_a,
        Seq(snapshot_a.seq.0 + 1),
        Seq(snapshot_a.seq.0 + 2),
        vec![JournalEdit {
            range: 0..0,
            insert: "z".to_string(),
        }],
    )
    .unwrap();
    let bytes = encode_journal(&[JournalRecord::snapshot(&snapshot_a), missing]).unwrap();
    assert_eq!(
        recover_journal(id_a, &bytes),
        Err(JournalError::InvalidSequence)
    );
}

#[test]
fn journal_reducer_serializes_appends_and_rejects_stale_completion() {
    let (mut store, id, first) = snapshot("a");
    let mut reducer = DraftJournalReducer::new(id, Seq(0));
    let initial = reducer.plan_snapshot(&first).unwrap();
    assert_eq!(reducer.plan_snapshot(&first), Err(JournalPlanError::Busy));
    assert_eq!(
        reducer.complete(
            JournalGeneration(initial.generation.0 + 99),
            JournalAppendResult::Committed(append_proof(&initial))
        ),
        JournalReduction::Stale
    );
    assert_eq!(reducer.durable_seq(), Seq(0));
    assert_eq!(
        reducer.complete(
            initial.generation,
            JournalAppendResult::Committed(append_proof(&initial))
        ),
        JournalReduction::Durable(DurableCheckpoint {
            document: id,
            seq: first.seq,
            source: DurableSource::DraftJournal,
        })
    );

    let second = changed_snapshot(&mut store, id, &first, 1..1, "b");
    let delta = reducer
        .plan_delta(
            first.seq,
            second.seq,
            vec![JournalEdit {
                range: 1..1,
                insert: "b".to_string(),
            }],
        )
        .unwrap();
    assert_eq!(delta.base_durable, first.seq);
}

#[test]
fn journal_durability_requires_matching_synced_append_proof() {
    let (_store, id, snapshot) = snapshot("draft");
    let mut reducer = DraftJournalReducer::new(id, Seq(0));
    let plan = reducer.plan_snapshot(&snapshot).unwrap();
    let mut proof = append_proof(&plan);
    proof.file_synced = false;
    assert!(matches!(
        reducer.complete(plan.generation, JournalAppendResult::Committed(proof)),
        JournalReduction::Failed {
            stage: JournalStage::VerifyProof,
            ..
        }
    ));
    assert_eq!(reducer.durable_seq(), Seq(0));

    // A compaction is durable only once renamed over the journal with its folder synced.
    for (renamed_over_journal, directory_synced) in [(false, true), (true, false)] {
        let plan = reducer.plan_snapshot(&snapshot).unwrap();
        let mut proof = append_proof(&plan);
        proof.landing = JournalLanding::Rewritten {
            renamed_over_journal,
            directory_synced,
        };
        assert!(matches!(
            reducer.complete(plan.generation, JournalAppendResult::Committed(proof)),
            JournalReduction::Failed {
                stage: JournalStage::VerifyProof,
                ..
            }
        ));
        assert_eq!(reducer.durable_seq(), Seq(0));
    }
    let plan = reducer.plan_snapshot(&snapshot).unwrap();
    let mut proof = append_proof(&plan);
    proof.landing = JournalLanding::Rewritten {
        renamed_over_journal: true,
        directory_synced: true,
    };
    assert!(matches!(
        reducer.complete(plan.generation, JournalAppendResult::Committed(proof)),
        JournalReduction::Durable { .. }
    ));
    assert_eq!(reducer.durable_seq(), snapshot.seq);
}

#[test]
fn empty_delta_and_oversized_edit_sets_are_bounded() {
    let (_store, id, snapshot) = snapshot("x");
    assert_eq!(
        JournalRecord::delta(id, snapshot.seq, Seq(snapshot.seq.0 + 1), vec![]),
        Err(JournalError::InvalidEdit)
    );
    let edits = (0..=MAX_JOURNAL_EDITS)
        .map(|_| JournalEdit {
            range: 0..0,
            insert: String::new(),
        })
        .collect();
    assert_eq!(
        JournalRecord::delta(id, snapshot.seq, Seq(snapshot.seq.0 + 1), edits),
        Err(JournalError::TooManyEdits)
    );
}

/// Every string of up to 8 characters over `a`, CR and LF, with and without
/// a byte order mark: the held text encodes back to exactly the file's
/// bytes; the file is held as CRLF exactly when it has a line feed and each
/// one has a CR before it; and then exactly one CR goes per line feed.
#[test]
fn held_text_encodes_back_to_the_files_bytes() {
    let mut files = vec![String::new()];
    let mut level = vec![String::new()];
    for _ in 0..8 {
        level = level
            .iter()
            .flat_map(|prefix| ["a", "\r", "\n"].map(|next| format!("{prefix}{next}")))
            .collect();
        files.extend(level.iter().cloned());
    }
    assert_eq!(files.len(), 9841);
    for file in &files {
        let line_feeds = file.matches('\n').count();
        let every_break_is_crlf = line_feeds > 0 && file.matches("\r\n").count() == line_feeds;
        for bom in [false, true] {
            let held = Decoded::hold(file.clone(), bom);
            assert_eq!(held.encoding.bom, bom);
            let on_disk = [if bom { &UTF8_BOM[..] } else { &[] }, file.as_bytes()].concat();
            assert_eq!(
                encode_for_disk(&held.text, held.encoding),
                on_disk,
                "{file:?}, bom {bom}"
            );
            if every_break_is_crlf {
                assert_eq!(held.encoding.eol, LineEnding::CrLf, "{file:?}");
                assert_eq!(held.text.len() + line_feeds, file.len(), "{file:?}");
                assert_eq!(held.text.matches('\n').count(), line_feeds, "{file:?}");
            } else {
                assert_eq!(held.encoding.eol, LineEnding::Lf, "{file:?}");
                assert_eq!(&held.text, file, "{file:?} is held byte for byte");
            }
        }
        if line_feeds > 0 {
            let crlf = DiskEncoding {
                bom: false,
                eol: LineEnding::CrLf,
            };
            let saved = String::from_utf8(encode_for_disk(file, crlf)).unwrap();
            assert_eq!(
                Decoded::hold(saved, false),
                Decoded {
                    text: file.clone(),
                    encoding: crlf
                },
                "{file:?}"
            );
        }
    }
}

/// A plan carries the buffer's text and, apart from it, the bytes the file
/// gets; a plain document's are one allocation.
#[test]
fn a_save_plan_encodes_the_text_with_the_mapping_it_was_read_with() {
    let crlf = DiskEncoding {
        bom: true,
        eol: LineEnding::CrLf,
    };
    let (mut store, id, first) = snapshot("a\nb\n");
    let read = ObservedFileVersion::observed(b"\xef\xbb\xbfa\r\nb\r\n", Some(7), Some(1))
        .with_encoding(crlf);
    let mut reducer = SaveReducer::new(id, read);
    let plan = reducer.begin(&first).unwrap();
    assert_eq!(&*plan.bytes, b"a\nb\n");
    assert_eq!(&*plan.file_bytes, b"\xef\xbb\xbfa\r\nb\r\n");
    assert_eq!(plan.desired, ContentFingerprint::of(&plan.file_bytes));
    assert!(matches!(
        reducer.complete(
            plan.generation,
            AtomicSaveResult::Committed(save_proof(&plan, 2))
        ),
        SaveReduction::Durable(_)
    ));
    assert_eq!(reducer.observed().encoding(), crlf);

    let second = changed_snapshot(&mut store, id, &first, 0..first.text.len(), "ab");
    let plan = reducer.begin(&second).unwrap();
    assert_eq!(&*plan.file_bytes, b"\xef\xbb\xbfab");
    assert!(matches!(
        reducer.complete(
            plan.generation,
            AtomicSaveResult::Committed(save_proof(&plan, 3))
        ),
        SaveReduction::Durable(_)
    ));
    assert_eq!(
        reducer.observed().encoding(),
        crlf,
        "no line break left, still CRLF"
    );

    let (_store, id, plain) = snapshot("x\n");
    let mut reducer = SaveReducer::new(id, ObservedFileVersion::missing());
    let plan = reducer.begin(&plain).unwrap();
    assert!(Arc::ptr_eq(&plan.bytes, &plan.file_bytes));
}

/// `journal_edit_in_order` is proved for one edit (SPECS.md); this holds
/// the loop that threads each edit's end into the next check.
#[test]
fn a_delta_takes_sorted_disjoint_forward_edits_only() {
    let (_store, id, snapshot) = snapshot("abcd");
    let delta = |ranges: &[(usize, usize)]| {
        let edits = ranges
            .iter()
            .map(|&(start, end)| JournalEdit {
                range: start..end,
                insert: String::new(),
            })
            .collect();
        JournalRecord::delta(id, snapshot.seq, Seq(snapshot.seq.0 + 1), edits).map(|_| ())
    };
    assert_eq!(
        delta(&[(2, 3)]),
        Ok(()),
        "the first edit may start anywhere"
    );
    assert_eq!(
        delta(&[(0, 1), (1, 2), (2, 2), (2, 4)]),
        Ok(()),
        "edits may touch"
    );
    assert_eq!(delta(&[(1, 2), (0, 1)]), Err(JournalError::InvalidEdit));
    assert_eq!(delta(&[(0, 2), (1, 3)]), Err(JournalError::InvalidEdit));
    assert_eq!(delta(&[(3, 1)]), Err(JournalError::InvalidEdit), "backward");
}
