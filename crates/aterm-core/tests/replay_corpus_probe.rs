// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Adversarial determinism corpus replay and [`aterm_models_session::coalesce_model`] tests.

use aterm_core::terminal::{ClockReading, Terminal};

#[rustfmt::skip]
include!("support/replay_corpus_data.rs");

const ROWS: u16 = 12;
const COLS: u16 = 40;

#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(), // captured once, reused so deltas are zero
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
        let by_record_again = fold(records, c).checkpoint();

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

#[test]
fn every_corpus_record_conforms_to_coalesce_model() {
    use aterm_models_session::coalesce_model;
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
