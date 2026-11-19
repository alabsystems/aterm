// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A record as the kernel lays it out: `rows` rows after the header, which names `len`.
fn record(len: u16, rows: &[[u64; 5]]) -> [u8; BUFFER_BYTES] {
    let mut buf = [0u8; BUFFER_BYTES];
    buf[..2].copy_from_slice(&len.to_ne_bytes());
    for (i, row) in rows.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            let at = HEADER_BYTES + i * ROW_BYTES + j * 8;
            buf[at..at + 8].copy_from_slice(&v.to_ne_bytes());
        }
    }
    buf
}

/// An M-series shape (two levels, P then E) decodes field by field and sums by level. The
/// field-by-field `assert_eq` is what holds the word order: the first row's five words are
/// distinct (asserted first), so a decoder that read any two of them in the other order would
/// decode another row and fail it.
#[test]
fn thread_counts_decode_two_levels_field_by_field() {
    let rows = [[900, 1_200, 30, 7, 5_000], [100, 400, 12, 3, 800]];
    let mut words = rows[0].to_vec();
    words.sort_unstable();
    words.dedup();
    assert_eq!(words.len(), 5, "the first row's words are distinct");
    let buf = record(2, &rows);
    let got = decode(&buf, HEADER_BYTES + 2 * ROW_BYTES).expect("a two-level record");
    assert_eq!(got.levels().len(), 2);
    let expected = LevelCounts {
        instructions: 900,
        cycles: 1_200,
        user_time_mach: 30,
        system_time_mach: 7,
        energy_nj: 5_000,
    };
    assert_eq!(got.levels()[0], expected);
    assert_eq!(
        got.total(),
        LevelCounts {
            instructions: 1_000,
            cycles: 1_600,
            user_time_mach: 42,
            system_time_mach: 10,
            energy_nj: 5_800,
        }
    );
}

/// A header with no row, more rows than the buffer holds, or rows past what the kernel wrote,
/// is refused, never read as zeros or as the buffer's stale tail. The control is the same
/// record with `written` exactly covering its rows, which decodes.
#[test]
fn thread_counts_decode_refuses_short_and_empty_records() {
    let one = record(1, &[[5, 6, 7, 8, 9]]);
    assert!(decode(&one, HEADER_BYTES + ROW_BYTES).is_some(), "control");
    assert_eq!(decode(&one, HEADER_BYTES + ROW_BYTES - 1), None);
    assert_eq!(decode(&one, 0), None);
    assert_eq!(decode(&record(0, &[]), BUFFER_BYTES), None);
    let over = record(5, &[[1; 5]; 4]);
    assert_eq!(decode(&over, BUFFER_BYTES), None);
    assert_eq!(decode(&one[..HEADER_BYTES + 8], BUFFER_BYTES), None);
    assert_eq!(decode(&[], 0), None);
}

/// Counters saturate rather than wrap when summed.
#[test]
fn thread_counts_total_saturates() {
    let buf = record(2, &[[u64::MAX, 1, 1, 1, 1], [2, 1, 1, 1, 1]]);
    let total = decode(&buf, BUFFER_BYTES).expect("decodes").total();
    assert_eq!(total.instructions, u64::MAX);
    assert_eq!(total.cycles, 2);
}

/// An inert live read of this process's own threads: some thread has a row with retired
/// instructions. Nothing is started or loaded.
#[cfg(target_os = "macos")]
#[test]
fn thread_counts_live_read_answers_for_this_process() {
    let cut = crate::self_cpu::read(std::time::Instant::now()).expect("a self cut");
    let counted: Vec<ThreadCounts> = cut
        .threads
        .iter()
        .filter_map(|t| read(std::process::id(), t.id))
        .collect();
    assert!(
        counted
            .iter()
            .any(|c| c.total().instructions > 0 && c.levels().len() <= MAX_LEVELS),
        "no thread of this process answered PROC_PIDTHREADCOUNTS"
    );
}
