// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Burst-slicing certificate (v0.5.2 perf pass).
//!
//! The PTY reader now slices a 64 KiB read into <=8 KiB chunks, RELEASING the single
//! terminal mutex between chunks (`aterm-gui/src/spawn.rs`), so a keystroke typed
//! during an output flood is no longer starved waiting on the whole burst's
//! `process()` (~75-185 us). That lock-hold reduction is a FREE win only if feeding the
//! VT engine in chunks is BYTE-IDENTICAL to one `process()` of the whole burst — and
//! the reader accumulates per-chunk `take_response()` replies exactly as one final
//! `take_response()` would.
//!
//! The parser is a streaming state machine, so the equivalence holds; this test
//! discharges that obligation CONCRETELY (not by an abstract model): over a >8 KiB
//! corpus whose SGR/CSI escape sequences, multibyte CJK runs, and DSR query replies
//! straddle the chunk boundaries, the final visible grid AND the concatenated query
//! responses are identical for a whole-burst feed vs every chunking — down to one byte
//! at a time (which splits every escape sequence), the real 8 KiB slice, and off-by-one
//! around it. If a future change makes the engine sensitive to read boundaries, this
//! reddens.

use aterm_core::terminal::Terminal;

/// A >8 KiB deterministic corpus mixing the surfaces most sensitive to chunk splits:
/// multi-byte CSI/SGR sequences, multibyte UTF-8 (CJK) runs, truecolor SGR, newlines
/// (scroll), and DSR cursor-position queries whose reply depends on the cursor position
/// when processed.
fn corpus() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..400u32 {
        v.extend_from_slice(b"\x1b[1;32mhello\x1b[0m \x1b[3mitalic\x1b[0m ");
        v.extend_from_slice("\u{4f60}\u{597d}\u{4e16}\u{754c}".as_bytes()); // 你好世界
        v.extend_from_slice(b" \x1b[38;2;200;100;50mtruecolor\x1b[0m");
        if i % 7 == 0 {
            v.extend_from_slice(b"\x1b[6n"); // DSR — cursor-position report (emits a reply)
        }
        v.extend_from_slice(b"\r\n");
    }
    assert!(
        v.len() > 8 * 1024,
        "corpus ({} B) must exceed the 8 KiB slice size",
        v.len()
    );
    v
}

/// Feed `data` to a fresh terminal in `chunk`-byte slices, draining replies per chunk
/// and accumulating them in order — exactly the reader's sliced shape. Returns the
/// final visible grid text and the concatenated replies.
fn feed(data: &[u8], chunk: usize) -> (String, Vec<u8>) {
    let mut t = Terminal::new(24, 80);
    let mut resp = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let end = (off + chunk).min(data.len());
        t.process(&data[off..end]);
        if let Some(r) = t.take_response() {
            resp.extend_from_slice(&r);
        }
        off = end;
    }
    (t.visible_content(), resp)
}

#[test]
fn chunked_process_is_byte_identical_to_whole_burst() {
    let data = corpus();
    // The reference: one `process()` of the whole burst, one `take_response()`.
    let (whole_grid, whole_resp) = feed(&data, data.len());
    assert!(
        !whole_resp.is_empty(),
        "corpus must exercise the reply path (DSR), else the response check is vacuous"
    );

    // Every chunking must reproduce the reference EXACTLY: 1 byte (splits every
    // sequence), small odd sizes, and the real 8 KiB slice +/- 1.
    for &chunk in &[1usize, 7, 64, 8191, 8192, 8193] {
        let (grid, resp) = feed(&data, chunk);
        assert_eq!(
            grid, whole_grid,
            "visible grid diverges at chunk size {chunk} — slicing is NOT byte-identical"
        );
        assert_eq!(
            resp, whole_resp,
            "concatenated replies diverge at chunk size {chunk}"
        );
    }
}
