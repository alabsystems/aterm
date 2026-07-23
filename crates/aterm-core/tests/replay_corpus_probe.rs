// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Asserting gate: runs the adversarial determinism corpora (machine-generated
//! by a hazard sweep that targeted real bulk-vs-single write-path divergences)
//! and asserts that, under the fixed clock, the screen state is a PURE FUNCTION
//! of the output-byte log — every corpus folds to the identical checkpoint
//! whether fed per original record, all at once, or one byte at a time. Two of
//! these corpora (`wide_wrap_tail`, `zwj_emoji_join`) reproduced genuine engine
//! bugs that are now fixed; this gate locks the fixes in.

use aterm_core::terminal::{ClockReading, Terminal};

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

        // Near-exhaustive differential check: EVERY single cut point [..k][k..]
        // must also fold to the reference. This is the metamorphic guard that
        // catches the bulk-vs-single lane divergence class for this byte log.
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
