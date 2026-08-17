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
//!
//! One byte at a time is not merely a parser stress: a lone decoded multibyte char
//! dispatches `print` (the per-glyph writer) while a coalesced run dispatches
//! `print_unicode_bulk` (the batched writer). So this certificate is ALSO the oracle
//! for batched-vs-per-char grid equivalence in the wide-glyph write path, and the
//! corpus below carries the cases that path is sensitive to: Hangul (the whole
//! Korean-text lead), a wide glyph landing on the last column (wrap + BCE tail blank),
//! and DECSLRM with the cursor parked OUTSIDE the margin span (where the batched
//! writer used to keep an unclamped row limit for the whole row — see
//! `hangul_run_under_decslrm_matches_per_char_writes`).

use aterm_core::terminal::Terminal;

/// 안녕하세요 — a pure-Hangul sample. Its lead syllable selects the wide-run
/// batcher on a coalesced feed and the per-glyph writer on a fragmented one, so
/// it is the sample that makes the two paths race for the same grid.
const HANGUL: &str = "\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}";

/// A >8 KiB deterministic corpus mixing the surfaces most sensitive to chunk splits:
/// multi-byte CSI/SGR sequences, multibyte UTF-8 (CJK and Hangul) runs, truecolor SGR,
/// newlines (scroll), wide glyphs landing on the last column (autowrap + BCE tail
/// blank), DECSLRM margin spans entered from OUTSIDE, and DSR cursor-position queries
/// whose reply depends on the cursor position when processed.
fn corpus() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..400u32 {
        v.extend_from_slice(b"\x1b[1;32mhello\x1b[0m \x1b[3mitalic\x1b[0m ");
        v.extend_from_slice("\u{4f60}\u{597d}\u{4e16}\u{754c}".as_bytes()); // 你好世界
        // Pure Hangul, and Hangul adjacent to Hanja: the first leads the wide run,
        // the second is folded into a CJK-led run by the extension predicate.
        v.extend_from_slice(HANGUL.as_bytes());
        v.extend_from_slice(" \u{6F22}\u{C790}\u{6F22}\u{C790}".as_bytes()); // 漢자漢자
        v.extend_from_slice(b" \x1b[38;2;200;100;50mtruecolor\x1b[0m");
        if i % 3 == 0 {
            // RIGHT-EDGE WRAP: park the cursor two columns short of the 80-column
            // right edge, fill them, then start a wide run at the last column. The
            // glyph cannot fit, so the writer must blank the skipped tail cell with
            // a BCE blank and wrap — in the same column whether the run arrives
            // coalesced or one syllable at a time.
            v.extend_from_slice(b"\r\n\x1b[78G++");
            v.extend_from_slice(HANGUL.as_bytes());
        }
        if i % 5 == 0 {
            // DECSLRM with the cursor OUTSIDE the span. `CSI ? 69 h` arms
            // DECLRMM, `CSI 11 ; 41 s` sets the span to columns 11-41 (1-based)
            // and homes the cursor to column 1 — LEFT of margins.left. A wide run
            // started there crosses INTO the span mid-row, which is precisely
            // where a row limit derived once per row diverges from one re-derived
            // per glyph. The DSR right after it compares the resulting cursor
            // column byte-for-byte, on top of the grid comparison.
            v.extend_from_slice(b"\x1b[?69h\x1b[11;41s\x1b[4;1H");
            for _ in 0..8 {
                v.extend_from_slice(HANGUL.as_bytes());
            }
            v.extend_from_slice(b"\x1b[6n");
            // The other side of the span: cursor RIGHT of margins.right.
            v.extend_from_slice(b"\x1b[6;60H");
            v.extend_from_slice("\u{4e16}\u{754c}\u{4e16}\u{754c}".as_bytes());
            v.extend_from_slice(b"\x1b[6n\x1b[?69l"); // DECLRMM off resets the margins
        }
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

/// The minimal, human-readable form of the divergence the corpus now covers,
/// pinned on its own so a failure names the cause instead of "the corpus moved".
///
/// A run of 30 `한` starting at column 1 with a DECSLRM span of columns 11-41:
/// the per-glyph writer re-derives the margin-clamped row limit before every
/// syllable, so it wraps at the right margin (20 syllables on the row, the rest
/// resuming at the left margin); the batched writer used to derive that limit
/// ONCE, while the cursor was still outside the span, and so ran all 30 past the
/// right margin on one row. Coalesced input hits the batched writer, one byte at
/// a time hits the per-glyph one — identical bytes, and they must fold to an
/// identical grid.
#[test]
fn hangul_run_under_decslrm_matches_per_char_writes() {
    let mut v = Vec::new();
    v.extend_from_slice(b"\x1b[?69h"); // DECLRMM on
    v.extend_from_slice(b"\x1b[11;41s"); // DECSLRM: columns 11-41 (1-based)
    v.extend_from_slice(b"\x1b[1;1H"); // CUP to column 1 — LEFT of margins.left
    for _ in 0..30 {
        v.extend_from_slice("\u{D55C}".as_bytes()); // 한
    }
    v.extend_from_slice(b"\x1b[6n"); // and the cursor column must agree too

    let (whole, whole_resp) = feed(&v, v.len());
    let (per_char, per_char_resp) = feed(&v, 1);
    assert_eq!(
        whole, per_char,
        "batched wide-run write diverges from per-glyph writes under DECSLRM"
    );
    assert_eq!(
        whole_resp, per_char_resp,
        "cursor position after the run diverges under DECSLRM"
    );
    // Non-vacuity: the run must actually have wrapped inside the margin span,
    // i.e. the reference itself puts syllables on more than one row. Without
    // this, a grid that silently stopped rendering Hangul would still "match".
    let rows_with_hangul = whole.lines().filter(|l| l.contains('\u{D55C}')).count();
    assert!(
        rows_with_hangul >= 2,
        "corpus must wrap at the right margin to exercise the clamp, got:\n{whole}"
    );
}

#[test]
fn xtversion_reports_the_shared_application_identity() {
    let mut terminal = Terminal::new(24, 80);
    terminal.process(b"\x1b[>0q");
    assert_eq!(
        terminal.take_response().as_deref(),
        Some(format!("\x1bP>|aterm({})\x1b\\", aterm_types::version::APP_VERSION).as_bytes())
    );
}
