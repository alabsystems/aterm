// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Runs the adversarial determinism corpora and asserts that, under a fixed
//! clock, the screen state is a pure function of the output-byte log: every
//! corpus folds to the same checkpoint whether fed per record, all at once, or
//! one byte at a time.

use aterm_core::terminal::{ClockReading, Terminal};

// The corpus records are generated and their layout is their content (xtask's
// `FMT_SWEEP_EXCLUSIONS`). A trustfmt from Trust `77d28e49f5` on follows a
// literal `include!` and formats the file it names; the skip keeps it out.
#[rustfmt::skip]
include!("support/replay_corpus_data.rs");

const ROWS: u16 = 12;
const COLS: u16 = 40;

fn clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(), // CLOCK-EXEMPT: captured once, reused so deltas are zero
        wall_ms: Some(0),
    }
}

fn fold(chunks: &[&[u8]], c: ClockReading) -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    for ch in chunks {
        t.process_at(ch, c);
    }
    t
}

#[test]
fn every_corpus_folds_chunk_independently() {
    let c = clock();
    let mut diverged = Vec::new();
    for (name, records) in CORPORA {
        let flat: Vec<u8> = records.iter().flat_map(|r| r.iter().copied()).collect();
        let by_record = fold(records, c).checkpoint();
        let one_shot = fold(&[&flat[..]], c).checkpoint();
        let per_byte_chunks: Vec<&[u8]> = flat.iter().map(std::slice::from_ref).collect();
        let per_byte = fold(&per_byte_chunks, c).checkpoint();
        // Re-run the reference chunking: a second fold must be bit-identical
        // (no rng / hashmap-iteration-order / global-state leak into output).
        let by_record_again = fold(records, c).checkpoint();

        // Every single cut point [..k][k..] must also fold to the reference
        // (catches bulk-vs-single write-lane divergence).
        let all_cuts_ok =
            (1..flat.len()).all(|k| fold(&[&flat[..k], &flat[k..]], c).checkpoint() == by_record);

        let ok = one_shot == by_record
            && per_byte == by_record
            && by_record_again == by_record
            && all_cuts_ok;
        println!(
            "{name:24} one_shot=={} per_byte=={} stable=={} all_cuts=={}",
            one_shot == by_record,
            per_byte == by_record,
            by_record_again == by_record,
            all_cuts_ok
        );
        if !ok {
            diverged.push(*name);
        }
    }
    assert!(
        diverged.is_empty(),
        "checkpoint() must be a pure function of the byte log regardless of chunk \
         boundaries; these corpora diverged: {diverged:?}"
    );
}

/// Tier-1 bind of `aterm_spec::derive::coalesce_model()`: the two write lanes,
/// compared after every parser-ground prefix rather than only at the end.
///
/// The single lane feeds one byte per `process_at` (the per-glyph writer); the
/// bulk lane feeds each record whole (the batched writers). A model element is
/// a run of records ending at parser ground, where a checkpoint is defined.
/// After each element `diverged` is projected from both lanes' real
/// `checkpoint()`s and every element must be the model's `Emit` at `Buggy = 0`,
/// so a divergence at any grounded prefix is rejected, even one a later record
/// would overwrite.
///
/// Negative control: a diverging record, projected from a real prefix, is
/// rejected by the committed model and admitted by `Buggy = 1` at `SKIPAT`.
#[test]
fn every_corpus_record_conforms_to_coalesce_model() {
    use aterm_spec::derive::coalesce_model;
    use aterm_spec::{interp, verify};
    use std::collections::BTreeMap;

    let m = coalesce_model();
    let c = clock();
    let st = |seq: usize, diverged: bool| -> BTreeMap<&'static str, i64> {
        [
            ("seq", i64::try_from(seq).expect("small corpus")),
            ("diverged", i64::from(diverged)),
        ]
        .into_iter()
        .collect()
    };
    let mut elements_checked = 0usize;
    for (name, records) in CORPORA {
        let max_seq = i64::try_from(records.len()).expect("small corpus");
        let mut single = Terminal::new(ROWS, COLS);
        let mut bulk = Terminal::new(ROWS, COLS);
        let mut state = st(0, false);
        let mut elements = 0usize;
        for (k, record) in records.iter().enumerate() {
            for byte in record.iter() {
                single.process_at(std::slice::from_ref(byte), c);
            }
            bulk.process_at(record, c);
            assert_eq!(
                single.parser_is_ground(),
                bulk.parser_is_ground(),
                "{name} record {k}: the lanes disagree on where the parser stands"
            );
            if !bulk.parser_is_ground() {
                continue; // mid-sequence: this element continues into the next record
            }
            elements += 1;
            let next = st(elements, single.checkpoint() != bulk.checkpoint());
            let (ok, why) = verify::validate_transition_tiered(
                &m,
                &[("MaxSeq", max_seq)],
                &state,
                &next,
                Some("Emit"),
                "coalesce lane conformance",
            );
            assert!(
                ok,
                "{name} record {k}: the bulk and single lanes diverged ({state:?} -> {next:?})\n{why}"
            );
            state = next;
        }
        assert!(
            bulk.parser_is_ground(),
            "{name}: every corpus ends at parser-ground"
        );
        elements_checked += elements;
    }
    assert!(
        elements_checked > CORPORA.len(),
        "the corpora were checked at intermediate prefixes, not only at their ends"
    );

    // Negative control from a real prefix of the first corpus.
    let (_, records) = CORPORA[0];
    let prev = st(1, false);
    let diverging = st(2, true);
    let mut single = Terminal::new(ROWS, COLS);
    let mut bulk = Terminal::new(ROWS, COLS);
    for b in records[0] {
        single.process_at(std::slice::from_ref(b), c);
    }
    bulk.process_at(records[0], c);
    assert_eq!(
        single.checkpoint(),
        bulk.checkpoint(),
        "the control's prefix is a real agreeing one"
    );
    let overrides = [("MaxSeq", 4), ("SKIPAT", 2)];
    let (ok, _) = verify::validate_transition_tiered(
        &m,
        &overrides,
        &prev,
        &diverging,
        Some("Emit"),
        "coalesce negative control",
    );
    assert!(!ok, "the committed model must reject a diverging record");
    assert!(
        interp::with_consts(&interp::with_buggy(&m, 1), &overrides)
            .successors("Emit", &prev)
            .contains(&diverging),
        "Buggy = 1 admits the divergence at SKIPAT"
    );
    assert!(!m.check_invariant("LanesAgree", &diverging));
}
