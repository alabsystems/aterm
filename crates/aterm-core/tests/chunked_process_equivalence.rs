// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Chunked feeding is equivalent to one `process()` of the whole burst.
//!
//! The PTY reader feeds a burst in 8 or 16 KiB slices and releases the terminal lock
//! between them (aterm-gui's `ingest_chunk_width`); that is sound only if the grid and the
//! concatenated per-chunk `take_response()` replies match one call's. One byte at a time
//! takes the per-glyph writer (`print`) where a coalesced run takes the batched one
//! (`print_unicode_bulk`), so this is also the batched-vs-per-char oracle.

use aterm_core::terminal::Terminal;

/// 안녕하세요 — a pure-Hangul sample. Its lead syllable selects the wide-run batcher on a
/// coalesced feed and the per-glyph writer on a fragmented one.
const HANGUL: &str = "\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}";

/// A >8 KiB corpus of the surfaces most sensitive to chunk splits: CSI/SGR (truecolor
/// too), CJK and Hangul runs, scrolls, wide glyphs at the last column (autowrap + BCE tail
/// blank), DECSLRM spans entered from outside, and DSR queries that report the cursor.
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
            // Right-edge wrap: `++` leaves the cursor on the last column, then a wide run.
            // The glyph cannot fit, so the writer blanks the tail cell (BCE) and wraps,
            // in the same column whether the run arrives coalesced or per syllable.
            v.extend_from_slice(b"\r\n\x1b[78G++");
            v.extend_from_slice(HANGUL.as_bytes());
        }
        if i % 5 == 0 {
            // DECSLRM with the cursor outside the span: `CSI 11 ; 41 s` sets columns
            // 11-41 and homes the cursor to column 1, left of the span. A wide run from
            // there crosses into the span mid-row, where a row limit derived once per
            // row diverges from one re-derived per glyph; the DSR after it compares the
            // cursor column too.
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

/// Feed `data` to a fresh terminal in `chunk`-byte slices, draining replies per chunk, as
/// the reader does. Returns the visible grid text and the concatenated replies.
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

    // Every chunking must reproduce the reference exactly: 1 byte (splits every
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

/// RIS does not retract the replies earlier bytes generated: they are owed to
/// the PTY peer, so a query and an `ESC c` in one slice answer exactly as when
/// a slice boundary falls between them.
#[test]
fn ris_keeps_the_replies_queries_before_it_generated() {
    let data = b"\x1b[c\x1b[>c\x1b[6n\x1bc";
    let (whole_grid, whole_resp) = feed(data, data.len());
    assert!(
        whole_resp.ends_with(b"\x1b[1;1R"),
        "the burst answers its DSR: {whole_resp:?}"
    );
    for chunk in 1..data.len() {
        assert_eq!(
            feed(data, chunk),
            (whole_grid.clone(), whole_resp.clone()),
            "chunk size {chunk}"
        );
    }
}

/// A run of 30 `한` from column 1 under a DECSLRM span of columns 11-41: the batched
/// writer wraps at the right margin like the per-glyph one, which re-derives the
/// margin-clamped row limit before every syllable, though the cursor starts outside the span.
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
    // Non-vacuity: the run wrapped inside the margin span, so a grid that silently
    // dropped Hangul cannot "match".
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
        Some(
            format!(
                "\x1bP>|aterm({})\x1b\\",
                aterm_types::version::app_version()
            )
            .as_bytes()
        )
    );
}

/// Everything a read boundary could change: each row's `len` and text, every cell that is
/// not a default blank or has an extras entry, the cursor and its deferred wrap, and the
/// replies. Truecolor is compared as resolved (`fg_rgb_at`): the per-glyph writer keeps it
/// in the extras map, the run writers in the dense ring.
fn state(t: &Terminal, resp: &[u8]) -> String {
    use std::fmt::Write as _;
    let g = t.grid();
    let c = t.cursor();
    let mut s = String::new();
    let _ = writeln!(
        s,
        "cursor=({},{}) wrap={} resp={:?}",
        c.row,
        c.col,
        g.pending_wrap(),
        String::from_utf8_lossy(resp)
    );
    for row in 0..g.rows() {
        let len = g.row(row).map_or(0, aterm_core::grid::row::Row::len);
        let _ = writeln!(s, "row {row} len={len} text={:?}", t.row_text(row.into()));
        for col in 0..g.cols() {
            let Some(cell) = g.cell(row, col) else {
                continue;
            };
            let extra = g.cell_extra(row, col).map(|e| {
                (
                    e.combining().to_vec(),
                    e.complex_char().map(ToString::to_string),
                    e.hyperlink().map(ToString::to_string),
                    e.hyperlink_id().map(ToString::to_string),
                    e.underline_color(),
                    e.underline_color_index(),
                    e.images().len(),
                )
            });
            // An entry holding only truecolor bytes resolves like the ring.
            let extra = extra.filter(|x| *x != Default::default());
            if cell.is_empty() && extra.is_none() {
                continue;
            }
            // A complex cell's `char_data` is an overflow index (allocation
            // order, not content): show the cluster it resolves to instead.
            let glyph = if cell.is_complex() {
                g.complex_char_str_at(row, col).unwrap_or_default()
            } else {
                cell.char().to_string()
            };
            let _ = writeln!(
                s,
                "  {col}: {glyph:?} {:?} {:?} has_extras={} fg={:?} bg={:?} {extra:?}",
                cell.flags(),
                cell.colors(),
                cell.has_extras(),
                cell.fg_needs_overflow().then(|| g.fg_rgb_at(row, col)),
                cell.bg_needs_overflow().then(|| g.bg_rgb_at(row, col)),
            );
        }
    }
    s
}

/// `chunks` fed in order to a fresh `rows`x`cols` terminal; its [`state`].
fn fold(rows: u16, cols: u16, chunks: &[&[u8]]) -> (Terminal, String) {
    let mut t = Terminal::new(rows, cols);
    let mut resp = Vec::new();
    for ch in chunks {
        t.process(ch);
        if let Some(r) = t.take_response() {
            resp.extend_from_slice(&r);
        }
    }
    let s = state(&t, &resp);
    (t, s)
}

/// Every visible cell whose extras entry holds a mark, a cluster, a link, an underline
/// colour or an image carries HAS_EXTRAS (aterm-grid's `HAS_EXTRAS ⇔ entry`); an unclaimed
/// entry is an orphan a later write would adopt. Truecolor bytes alone are owned through
/// the cell's RGB marker instead.
fn assert_no_orphan_extras(t: &Terminal, what: &str) {
    let g = t.grid();
    for row in 0..g.rows() {
        for col in 0..g.cols() {
            let Some(extra) = g.cell_extra(row, col) else {
                continue;
            };
            let claimable = !extra.combining().is_empty()
                || extra.complex_char().is_some()
                || extra.hyperlink().is_some()
                || extra.underline_color().is_some()
                || extra.underline_color_index().is_some()
                || !extra.images().is_empty();
            assert!(
                !claimable || g.cell(row, col).is_some_and(|c| c.has_extras()),
                "{what}: ({row},{col}) holds an extras entry its cell does not claim: \
                 {extra:?}"
            );
        }
    }
}

/// `input` fed whole, one byte at a time, and cut once at every byte: the
/// same [`state`] each way, and no orphaned extras entry in any of them.
/// Returns the whole feed's terminal for the caller's own expectations.
fn assert_chunk_invariant(rows: u16, cols: u16, input: &[u8]) -> Terminal {
    let shown = String::from_utf8_lossy(input).escape_debug().to_string();
    let (whole_t, whole) = fold(rows, cols, &[input]);
    assert_no_orphan_extras(&whole_t, &format!("{shown} whole"));
    let bytes: Vec<&[u8]> = input.chunks(1).collect();
    let (byte_t, bytewise) = fold(rows, cols, &bytes);
    assert_no_orphan_extras(&byte_t, &format!("{shown} bytewise"));
    assert_eq!(
        whole, bytewise,
        "{shown}: one byte at a time diverges from one read"
    );
    for cut in 1..input.len() {
        let (a, b) = input.split_at(cut);
        let (cut_t, split) = fold(rows, cols, &[a, b]);
        assert_no_orphan_extras(&cut_t, &format!("{shown} cut at {cut}"));
        assert_eq!(
            whole, split,
            "{shown}: a read boundary at byte {cut} diverges"
        );
    }
    whole_t
}

fn row_texts(t: &Terminal) -> Vec<String> {
    (0..usize::from(t.grid().rows()))
        .map(|r| t.row_text(r).unwrap_or_default())
        .collect()
}

/// DECSLRM homes the cursor to column 1, left of the span. An ASCII run from there wraps
/// at the right margin as soon as it enters the span (xterm `dotext`), then continues at
/// the left margin, whichever writer the run reaches.
#[test]
fn ascii_run_entering_decslrm_from_outside_wraps_at_the_right_margin() {
    let alphabet = "abcdefghijklmnopqrstuvwxyz";
    let splat = "z".repeat(70);
    let cases: [(&str, String); 6] = [
        ("blast", format!("\x1b[?69h\x1b[3;9s{}", &alphabet[..11])),
        (
            "styled",
            format!("\x1b[?69h\x1b[3;9s\x1b[1m{}", &alphabet[..11]),
        ),
        (
            "extras",
            format!("\x1b[?69h\x1b[3;9s\x1b[38;2;1;2;3m{}", &alphabet[..11]),
        ),
        ("splat", format!("\x1b[?69h\x1b[3;9s{splat}")),
        ("rep", "\x1b[?69h\x1b[3;9sa\x1b[10b".to_string()),
        (
            "right of span",
            format!("\x1b[?69h\x1b[3;9s\x1b[1;11H{alphabet}"),
        ),
    ];
    for (name, input) in cases {
        let input = format!("{input}\x1b[6n");
        let t = assert_chunk_invariant(6, 12, input.as_bytes());
        // Non-vacuity: the run did wrap at the right margin (column 9), so no
        // row past the first holds text right of it.
        let rows = row_texts(&t);
        for (r, text) in rows.iter().enumerate().skip(1) {
            assert!(
                text.trim_end().chars().count() <= 9,
                "{name}: row {r} prints past the right margin: {rows:?}"
            );
        }
        if name != "right of span" {
            assert_eq!(
                rows[0].trim_end().chars().count(),
                9,
                "{name}: the first row ends at the right margin: {rows:?}"
            );
        }
    }
    // On a double-width row the margin span ends at the row's half width as
    // well, glyph by glyph as for a run: the double-width row holds at most
    // its six columns.
    for input in [
        "\x1b[?69h\x1b[3;s\x1b#3aaaaaa\x1b[6n",
        "\x1b[?69h\x1b[3;9s\x1b#6abcdefghij\x1b[6n",
    ] {
        let t = assert_chunk_invariant(6, 12, input.as_bytes());
        let first = row_texts(&t)[0].trim_end().chars().count();
        assert!(
            first <= 6,
            "{input:?}: the double-width row holds {first} columns: {:?}",
            row_texts(&t)
        );
    }
}

/// A cell an operation blanks or overwrites outside the writers' stale-entry probe drops
/// its extras with it, so a later extras-style write at that column never adopts an old
/// combining mark, skin tone or link; every grid keeps each extras entry claimed.
#[test]
fn a_blanked_cell_keeps_no_extras_for_a_later_write_to_adopt() {
    // (name, rows, cols, input, the rows it must show, the row-0 columns that
    // keep a link of their own).
    type Case<'a> = (&'a str, u16, u16, &'a str, &'a [&'a str], &'a [u16]);
    let cases: [Case; 19] = [
        (
            "vs16 over an NFD mark",
            1,
            12,
            "cafe\u{301}\r\x1b[2C\u{2764}\u{FE0F}\r\x1b[38;2;200;100;50mcaf\x1b[38;2;10;20;30mX",
            &["cafX"],
            &[],
        ),
        (
            "vs16 over a mark, curly underline",
            1,
            10,
            "\x1b[4:3mAB\u{301}\r\u{2764}\u{FE0F}\rhe",
            &["he"],
            &[],
        ),
        (
            "ich pushes a toned emoji off the edge",
            2,
            12,
            "\x1b[1;10H\u{1F44D}\u{1F3FD}\x1b[1;4H\x1b[58;5;3m\x1b[2@\x1b[1;12Hz",
            &["           z"],
            &[],
        ),
        (
            "narrow over a toned emoji's right half",
            1,
            8,
            "\u{1F44D}\u{1F3FD}\x1b[1;2Hx\x1b[1;1H\x1b[38;2;1;2;3my",
            &["yx"],
            &[],
        ),
        (
            "narrow over a marked CJK's right half",
            1,
            8,
            "\u{4E2D}\u{301}\x1b[1;2Hx\x1b[1;1H\x1b[38;2;1;2;3my",
            &["yx"],
            &[],
        ),
        (
            "plain mark onto the blanked head",
            1,
            8,
            "\u{4E2D}\u{301}\x1b[1;2Hx\x1b[1;1Hy\u{300}",
            &["y\u{300}x"],
            &[],
        ),
        (
            "linked CJK, then a truecolor glyph on its blanked half",
            1,
            8,
            "\x1b]8;;http://old.example/\x1b\\\u{4E2D}\x1b]8;;\x1b\\\rh\x1b[38;2;1;2;3me",
            &["he"],
            &[],
        ),
        (
            "vs15 narrows a linked emoji",
            1,
            8,
            "\x1b]8;;http://old.example/\x1b\\\u{231A}\x1b]8;;\x1b\\\u{FE0E}\x1b[1;2H\x1b[38;2;1;2;3mq",
            &["\u{231A}\u{FE0E}q"],
            &[0],
        ),
        (
            "el 0 from a marked CJK's right half",
            1,
            8,
            "\u{4E2D}\u{301}\x1b[1;2H\x1b[K\x1b[1;1H\x1b[38;2;1;2;3my",
            &["y"],
            &[],
        ),
        (
            "el 1 to a linked CJK's left half",
            1,
            8,
            "\x1b]8;;http://old.example/\x1b\\\u{4E2D}\x1b]8;;\x1b\\\x1b[1;1H\x1b[1K\x1b[1;2H\x1b[38;2;1;2;3mq",
            &[" q"],
            &[],
        ),
        (
            "dch at a marked CJK's right half",
            1,
            8,
            "\u{4E2D}\u{301}AB\x1b[1;2H\x1b[P\x1b[1;1H\x1b[38;2;1;2;3my",
            &["yAB"],
            &[],
        ),
        (
            "a wide glyph wraps off a marked last column",
            2,
            12,
            "\x1b[1;12He\u{301}\x1b[1;12H\u{4E2D}\x1b[1;12H\x1b[38;2;1;2;3mq",
            &["           q", "\u{4E2D}"],
            &[],
        ),
        (
            "decsel clears a marked cell",
            1,
            8,
            "e\u{301}\r\x1b[?K\x1b[38;2;1;2;3mq",
            &["q"],
            &[],
        ),
        (
            "ech at a marked CJK's right half",
            1,
            8,
            "\u{4E2D}\u{301}\x1b[1;2H\x1b[X\x1b[1;1H\x1b[38;2;1;2;3my",
            &["y"],
            &[],
        ),
        (
            "decera bisects a marked CJK",
            1,
            8,
            "\u{4E2D}\u{301}\x1b[1;2;1;2$z\x1b[1;1H\x1b[38;2;1;2;3my",
            &["y"],
            &[],
        ),
        (
            "deccra copies a marked head its source edge cut",
            2,
            8,
            "\u{4E2D}\u{301}\x1b[1;1;1;1;1;2;1;1$v\x1b[2;1H\x1b[38;2;1;2;3mq",
            &["\u{4E2D}\u{301}", "q"],
            &[],
        ),
        // A double-width row's line is its first half: ICH and DCH shift its
        // cells there, with its extras, and leave the hidden half alone.
        (
            "dch on a double-width row, a mark DECIC pushed into the hidden half",
            1,
            12,
            "abcdef\u{301}\x1b#6\x1b[1;6H\x1b[2'}\x1b[1;6H\x1b[P\x1b#5\x1b[1;8H\x1b[38;2;1;2;3mq",
            &["abcde  q"],
            &[],
        ),
        (
            "ich on a double-width row, a mark DECIC pushed into the hidden half",
            1,
            12,
            "abcdef\u{301}\x1b#6\x1b[1;6H\x1b[2'}\x1b[1;1H\x1b[@\x1b#5\x1b[1;8H\x1b[38;2;1;2;3mq",
            &[" abcde q"],
            &[],
        ),
        // DECDWL blanks the hidden half so nothing reappears at DECSWL; ICH
        // pushes nothing into it either.
        (
            "ich on a double-width row drops what it pushes past the half",
            1,
            12,
            "abcdef\x1b#6\x1b[1;1H\x1b[@\x1b#5",
            &[" abcde"],
            &[],
        ),
    ];
    for (name, rows, cols, input, want, linked) in cases {
        let t = assert_chunk_invariant(rows, cols, input.as_bytes());
        let got = row_texts(&t);
        for (r, w) in want.iter().enumerate() {
            assert_eq!(
                got[r].trim_end(),
                *w,
                "{name}: row {r} shows an adopted mark or link: {got:?}"
            );
        }
        for row in 0..rows {
            for col in 0..cols {
                if row == 0 && linked.contains(&col) {
                    continue;
                }
                assert_eq!(
                    t.hyperlink_at(row, col),
                    None,
                    "{name}: ({row},{col}) still links the old target"
                );
            }
        }
    }
}

/// A wide pair a rectangle scroll (RI at the top of a DECSLRM span) copied into a row is
/// a wide pair to every writer: a later run over one half blanks the other, in one read
/// (the bulk writers, gated by the row's `HAS_WIDE_CHARS` hint) or a byte at a time.
#[test]
fn a_rectangle_scroll_keeps_the_rows_wide_pairs_for_the_bulk_writers() {
    for (input, want) in [
        // ASCII over a copied pair's continuation: the head goes too.
        (
            "\x1b[?69h\x1b[3;9s\u{4E2D}\u{6587}\u{C548}\u{B155}\x1bM\x1b[2;4Hxyz",
            "   xyz\u{B155}",
        ),
        // ASCII over a copied head, then a toned emoji over the next head: each
        // continuation goes.
        (
            "\x1b[?69h\x1b[3;9s\u{4E2D}\u{6587}\u{C548}\u{B155}\x1bM\x1b[2;3Hb\x1b[?69l\u{1F44D}\u{1F3FD}",
            "  b\u{1F44D}\u{1F3FD} \u{B155}",
        ),
        // The same through a margined IL and a margined scroll up.
        (
            "\x1b[?69h\x1b[3;9s\u{4E2D}\u{6587}\u{C548}\u{B155}\x1b[1;3H\x1b[L\x1b[2;4Hxyz",
            "   xyz\u{B155}",
        ),
    ] {
        let t = assert_chunk_invariant(6, 12, input.as_bytes());
        assert_eq!(
            row_texts(&t)[1].trim_end(),
            want,
            "{input:?}: {:?}",
            row_texts(&t)
        );
    }
}

/// REP repeats the last graphic character received (ECMA-48 §8.3.103, xterm's
/// `lastchar`): the skin-tone modifier or the flag's second regional indicator,
/// whichever writer laid the emoji out — the bulk run or one character at a
/// time — and whatever pen is current.
#[test]
fn rep_after_a_folded_emoji_repeats_the_same_character_on_every_path() {
    for input in [
        "\u{1F44D}\u{1F3FD}\x1b[b|",
        "\u{1F44D}\u{1F3FD}\x1b[3b|",
        "\u{1F1FA}\u{1F1F8}\x1b[b|",
        "\x1b[38;2;1;2;3m\u{1F44D}\u{1F3FD}\x1b[b|",
        "\x1b[38;2;1;2;3m\u{1F1FA}\u{1F1F8}\x1b[b|",
        "x\u{1F44D}\u{1F3FD}\x1b[b\x1b[6n",
    ] {
        assert_chunk_invariant(3, 20, input.as_bytes());
    }
    // And the default pen agrees with an extras pen (the bulk writer falls
    // back to per-character writes under one).
    let (plain, _) = fold(3, 20, &["\u{1F1FA}\u{1F1F8}\x1b[b|".as_bytes()]);
    let (pen, _) = fold(
        3,
        20,
        &["\x1b[38;2;1;2;3m\u{1F1FA}\u{1F1F8}\x1b[b|".as_bytes()],
    );
    assert_eq!(row_texts(&plain), row_texts(&pen));
    assert_eq!(
        row_texts(&plain)[0].trim_end(),
        "\u{1F1FA}\u{1F1F8}\u{1F1F8}|",
        "REP repeats the flag's second regional indicator"
    );
}

/// A printed blank is written text whichever writer printed it (the splat of 4+ identical
/// bytes in a run over 64 bytes, REP of a space, one at a time): it counts toward the
/// row's `len`, so trailing spaces, a mark on one, copy across a soft wrap and a prompt's
/// cursor through a rewrap match for every read boundary.
#[test]
fn printed_blanks_count_toward_the_row_len_on_every_writer() {
    let digits: String = (0..70).map(|i| char::from(b'0' + (i % 10) as u8)).collect();
    for (rows, cols, input) in [
        (1u16, 80u16, format!("{digits}    ")),
        (1, 80, format!("{digits}G    \u{301}")),
        (
            6,
            20,
            "hello world 1234    next row text and more padding to 64+ bytes!".to_string(),
        ),
    ] {
        let t = assert_chunk_invariant(rows, cols, input.as_bytes());
        // Non-vacuity: the whole feed kept the printed blanks.
        assert!(
            t.row_text(0)
                .is_some_and(|s| s.ends_with(' ') || s.contains("    ")),
            "{input:?}: printed blanks dropped: {:?}",
            row_texts(&t)
        );
    }
    // REP of a space writes the same line as the spaces printed one by one.
    let (_, rep) = fold(1, 80, &[b"x \x1b[9b"]);
    let (_, typed) = fold(1, 80, &[b"x          "]);
    assert_eq!(rep, typed, "REP's blanks are written like printed ones");

    // A `read -p` prompt ending in printed spaces, then a width change and back: the
    // cursor stays after the spaces, so typed text lands after them whatever the read
    // boundaries were.
    let prompt = "Please enter the name of the project you would like to configure:    ";
    let typed_after = |chunks: &[&[u8]]| {
        let (mut t, _) = fold(3, 80, chunks);
        t.resize(3, 40);
        t.resize(3, 80);
        t.process(b"Bob");
        (row_texts(&t), (t.cursor().row, t.cursor().col))
    };
    let whole = typed_after(&[prompt.as_bytes()]);
    let bytes: Vec<&[u8]> = prompt.as_bytes().chunks(1).collect();
    assert_eq!(
        whole,
        typed_after(&bytes),
        "the read boundaries moved the cursor"
    );
    assert_eq!(
        whole.0[0],
        format!("{prompt}Bob"),
        "typed text after the spaces"
    );

    // A mark with nothing printed before it in the row's text lands on a blank
    // cell; that cell is written, so the frame draws the mark.
    let (mut t, _) = fold(1, 20, &["ab\x1b[3C\u{301}".as_bytes()]);
    assert_eq!(t.row_text(0).as_deref(), Some("ab   \u{301}"));
    assert!(
        t.cell_frame(1, 20).combining[0]
            .iter()
            .any(|(col, marks)| *col == 4 && marks.contains(&'\u{301}')),
        "the frame draws a mark on a cell nothing else wrote"
    );

    // A mark on a printed blank is part of the line and of the frame.
    let (mut t, _) = fold(1, 80, &[format!("{digits}G    \u{301}").as_bytes()]);
    assert!(
        t.row_text(0).is_some_and(|s| s.ends_with(" \u{301}")),
        "the mark on the last printed blank is row text: {:?}",
        t.row_text(0)
    );
    let frame = t.cell_frame(1, 80);
    assert!(
        frame.combining[0]
            .iter()
            .any(|(col, marks)| *col == 74 && marks.contains(&'\u{301}')),
        "the render frame draws the mark on the blank: {:?}",
        frame.combining[0]
    );
}

/// VT52 direct cursor addressing consumes exactly its two address bytes, so where a read
/// ends cannot move the cursor. No writer folds printable ASCII after a ZWJ (UAX #29 GB11
/// joins a ZWJ only to Extended_Pictographic), so `a`ZWJ`xy` lays out the same under IRM,
/// DECAWM off, a national charset and the bulk path.
#[test]
fn vt52_address_and_ascii_after_zwj_lay_out_the_same_on_every_path() {
    assert_chunk_invariant(6, 12, "\x1b[?2l\x1bY%(a\u{200D}\x1bY%)_y".as_bytes());
    assert_chunk_invariant(6, 12, "\x1b[?2l\x1bY%(ab\x1bY%)cdef\x1bY  gh".as_bytes());

    let (bulk, _) = fold(1, 12, &["a\u{200D}xy\x1b[6n".as_bytes()]);
    let reference = row_texts(&bulk);
    assert_eq!(bulk.cursor().col, 3, "x and y each open a cell after a ZWJ");
    for mode in ["\x1b[4h", "\x1b[?7l", "\x1b(A"] {
        let (t, _) = fold(1, 12, &[format!("{mode}a\u{200D}xy").as_bytes()]);
        assert_eq!(t.cursor().col, 3, "{mode:?}: ASCII after a ZWJ folded");
        assert_eq!(row_texts(&t), reference, "{mode:?}");
    }
    // A printed ASCII glyph ends the ZWJ continuation whichever writer
    // printed it: an emoji written back next to the ZWJ cell opens its own.
    let (bulk, _) = fold(1, 12, &["a\u{200D}x\x1b[1;2H\u{1F600}".as_bytes()]);
    let (per_char, _) = fold(1, 12, &["\x1b[?7la\u{200D}x\x1b[1;2H\u{1F600}".as_bytes()]);
    assert_eq!(row_texts(&bulk), row_texts(&per_char));
    assert_eq!(bulk.cursor().col, 3, "the emoji opened its own cell");
}

/// Seeded random differential over the surfaces above, interleaved with text for every
/// writer: each input gives the same [`state`] whole, one byte at a time and cut at a few
/// random points, with every extras entry claimed. 4,000 inputs because the
/// rectangle-scroll wide pair first shows at input 2,162.
#[test]
fn random_streams_are_read_boundary_invariant() {
    const TOKENS: &[&str] = &[
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[?7l",
        "\x1b[?7h",
        "\x1b(0",
        "\x1b(B",
        "\x1b(A",
        "\x0e",
        "\x0f",
        "\x1b[?69h\x1b[3;9s",
        "\x1b[?69l",
        "\x1b[2;5r",
        "\x1b[r",
        "\x1b[?6h",
        "\x1b[?6l",
        "\x1b[?2l",
        "\x1b<",
        "\x1bY##",
        "\x1bY",
        "\x1b[1;31m",
        "\x1b[0m",
        "\x1b[7m",
        "\x1b[38;2;1;2;3m",
        "\x1b[4:3m",
        "\x1b[58;5;3m",
        "\x1b]8;;http://x/\x1b\\",
        "\x1b]8;;\x1b\\",
        "\x1b[3b",
        "\x1b[b",
        "\x1b[2@",
        "\x1b[2P",
        "\x1b[2'}",
        "\x1b[2'~",
        "\x1b[3X",
        "\x1b[K",
        "\x1b[1K",
        "\x1b[?K",
        "\x1b[1;11H",
        "\x1b[1;12H",
        "\x1b[6;12H",
        "\x1b[H",
        "\x1b[5G",
        "\r",
        "\n",
        "\x08",
        "\t",
        "\x1b7",
        "\x1b8",
        "\x1bD",
        "\x1bM",
        "\x1b#6",
        "\x1b#3",
        "\x1b#5",
    ];
    const TEXT: &[&str] = &[
        "a",
        "hello",
        "abcdefghijklmnop",
        "\u{4E2D}\u{6587}",
        "\u{C548}\u{B155}",
        "e\u{301}",
        "\u{301}",
        "\u{200D}",
        "\u{1F468}\u{200D}\u{1F469}",
        "\u{FE0F}",
        "\u{FE0E}",
        "\u{2764}\u{FE0F}",
        "\u{231A}",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F44D}\u{1F3FD}",
        "\u{1F3FB}",
        "a\u{200D}b",
        "    ",
        "ab    cd====ef\u{4E2D}gh    ij____kl  mn--------op qr st uv wx yz 0123456789 ABCDEFG    ",
    ];
    let mut state_word = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move |n: usize| {
        state_word ^= state_word << 13;
        state_word ^= state_word >> 7;
        state_word ^= state_word << 17;
        usize::try_from(state_word % n as u64).unwrap_or(0)
    };
    for case in 0..4000 {
        let mut input = Vec::new();
        for _ in 0..3 + next(20) {
            if next(10) < 4 {
                input.extend_from_slice(TOKENS[next(TOKENS.len())].as_bytes());
            } else {
                input.extend_from_slice(TEXT[next(TEXT.len())].as_bytes());
            }
        }
        let shown = String::from_utf8_lossy(&input).escape_debug().to_string();
        let (whole_t, whole) = fold(6, 12, &[&input]);
        assert_no_orphan_extras(&whole_t, &format!("case {case} {shown}"));
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(
            whole,
            fold(6, 12, &bytes).1,
            "case {case}: {shown} one byte at a time"
        );
        for _ in 0..3 {
            let cut = next(input.len() + 1);
            let (a, b) = input.split_at(cut);
            assert_eq!(
                whole,
                fold(6, 12, &[a, b]).1,
                "case {case}: {shown} cut at {cut}"
            );
        }
    }
}
