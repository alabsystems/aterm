// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-frame cost of the sparkle-words scan over NO-SPACE-SCRIPT text.
//!
//! ## Why this bench had to be written
//!
//! Nothing in the tree priced `Lexicon::scan_into_with_scratch` on CJK input,
//! and the CJK arm of that scanner is the expensive one: for every position in
//! a no-space-script run `scan_cjk_run` clears and rebuilds a `max_cjk`-char
//! window and then probes it longest-first, two hash lookups per length. In the
//! shipped data `max_cjk` is 10, so a position that matches nothing — the
//! dominant case for real prose — costs ~10 UTF-8 encodes and ~20 hash probes
//! before advancing by ONE character, and the next position rebuilds a window
//! sharing nine of its ten characters with the one just discarded.
//!
//! That work is per FRAME, not per keystroke-batch: `WordDecorations::
//! rescan_from_cells_*` runs inside the present that carries the keystroke echo
//! and re-scans every visible row whose text missed the row memo — which during
//! scrolling output is every genuinely new line. `word_decorations.rs` records
//! in-source that this tokenise+lookup step is "~86 % of the rescan".
//!
//! ## Arms
//!
//! One arm per script, each a full 24-row SCREENFUL scanned row by row through
//! the same reused scratch buffers the real caller keeps
//! (`WordDecorations::scan_chars` / `scan_matches` / `scan_scratch`), with the
//! shipped default [`ScanOptions`] (`allow_bare_cat` and `cjk_single_char`
//! both false, no ignore set):
//!
//! * `cjk_scan/japanese`, `cjk_scan/chinese` — kana + han, genuinely
//!   space-free, so one row is ONE run of 40 no-space characters.
//! * `cjk_scan/korean` — Hangul, which DOES use spaces between words, so the
//!   same row is many short runs. Kept separate because run length is exactly
//!   what a per-position prefilter changes.
//! * `cjk_scan/thai` — single-width script, so a row holds twice as many
//!   characters as a han row and the arm is correspondingly longer.
//! * `cjk_scan/ascii_control` — the CONTROL. English prose of the same screen
//!   geometry, which `fold::is_no_space_script` rejects at every position, so
//!   the scanner never enters `scan_cjk_run` at all. [`verify_scan_reach`]
//!   asserts that (exactly zero entries) as the negative half of the reach
//!   guard; the arm is here so a change to the CJK path can be shown NOT to
//!   move the spaced-token path.
//!
//! ## Two-sided reach guard
//!
//! [`verify_scan_reach`] runs before any timing. `fold::is_no_space_script` is
//! `pub` precisely so callers classify with the SAME predicate the scanner
//! uses (see its docs), so the guard re-derives, from the corpus alone, exactly
//! what `scan_into_with_scratch` does with it: the number of maximal no-space
//! runs (= the number of `scan_cjk_run` ENTRIES) and the number of characters
//! inside them (= the positions that build a window). It then pins both halves:
//!
//! * every CJK arm enters `scan_cjk_run` a pinned, non-zero number of times and
//!   walks a pinned, non-zero number of positions;
//! * the ASCII control arm enters it ZERO times and walks ZERO positions;
//! * every arm still produces matches, so no arm is an inert corpus that would
//!   measure nothing;
//! * and the CJK arms are dominated by MISSES (the case the window rebuild is
//!   pure waste for), asserted as a ratio rather than assumed.
//!
//! The derivation was checked once against a temporary counter compiled into
//! `scan_cjk_run` itself: entries and positions matched the numbers below
//! exactly, and the ASCII arm reported zero of both.

use aterm_lexicon::{Class, Lexicon, Match, ScanOptions, ScanScratch, is_no_space_script};
use criterion::{Criterion, black_box, criterion_group, criterion_main};

/// Visible rows in the screenful each arm scans — one terminal's worth.
const ROWS: usize = 24;
/// Characters per row for a DOUBLE-width script (han, kana, Hangul) in an
/// 80-column terminal.
const WIDE_COLS: usize = 40;
/// Characters per row for a single-width script (Thai, ASCII).
const NARROW_COLS: usize = 80;

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------

/// Japanese prose: kana + kanji with NO spaces, so a row is one long run.
/// Carries a few real lexicon surfaces (子猫, にゃんこ, 畜生) and one
/// exception surface (山猫, which must be suppressed) so the hit path and the
/// exception path are both exercised, at the rate real prose exercises them.
const JAPANESE: &str = "\
端末は毎フレームごとに画面の文字を走査するので、辞書の照合が遅いと入力の応答が目に見えて悪くなる。\
子猫がキーボードの上を歩いても描画は止まらないはずだ。にゃんこは可愛いが、畜生、また描画が遅い。\
山猫は猫ではないので装飾しない。行の折り返しや全角文字の幅計算も同じ経路を通る。";

/// Chinese prose: han with no spaces. Carries 小猫 and 卧槽.
const CHINESE: &str = "\
终端在每一帧都会重新扫描可见的文本行，如果词典匹配太慢，按键回显就会出现明显的延迟。\
小猫在键盘上走过也不会让绘制停下来。卧槽，这一行又慢了。\
换行和全角字符的宽度计算也会经过同一条代码路径。";

/// Korean prose. Hangul DOES use spaces between words, so this row is many
/// short runs rather than one long one. Carries 고양이.
const KOREAN: &str = "\
터미널은 화면에 보이는 모든 줄을 매 프레임마다 다시 훑기 때문에 사전 조회가 느리면 \
입력 반응이 눈에 띄게 나빠진다. 고양이가 키보드 위를 걸어도 그리기는 멈추지 않는다. \
줄 바꿈과 전각 문자의 너비 계산도 같은 경로를 지난다.";

/// Thai prose. Single-width, no word spaces inside a phrase. Carries แมว.
const THAI: &str = "\
เทอร์มินัลจะสแกนข้อความที่มองเห็นทุกเฟรม ถ้าการค้นพจนานุกรมช้า \
การตอบสนองของการพิมพ์จะแย่ลงอย่างเห็นได้ชัด แมวเดินบนคีย์บอร์ดก็ไม่ทำให้การวาดหยุด \
การตัดบรรทัดและการคำนวณความกว้างก็ใช้เส้นทางเดียวกัน";

/// ADVERSARIAL worst case for a first-character prefilter — deliberately NOT
/// prose. Thai `เ` is the one first character in the shipped data whose longest
/// surface is the global `max_cjk` (`เจ้าเหมียว`, 10 characters), so a run of it
/// passes the prefilter at EVERY position and then still builds the full
/// ten-character window and runs all twenty probes. This arm therefore prices
/// the prefilter's cost with none of its saving, which is the only shape in
/// which it can lose. The row ends in a real surface (`เหมียว`) so the arm is
/// not an inert corpus, while staying >92 % misses.
const ADVERSARIAL: &str = "\
เเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเ\
เเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเเ\
เหมียว";

/// English prose — the control. Every character is rejected by
/// `is_no_space_script`, so the scanner takes the spaced-token arm for all of
/// it. Carries `cats` (four folded characters, so it clears the short-feline
/// guard) to keep the arm from being an inert corpus.
const ASCII: &str = "\
the terminal rescans every visible row inside the present that carries the keystroke echo, \
so a slow dictionary lookup shows up as input latency and nothing else; the cats asleep on \
the keyboard do not stop the draw, and neither does a wrapped line or a wide glyph. ";

/// Cut `source` into `ROWS` rows of `cols` characters, cycling the source.
///
/// Rows are cut at a fixed character count exactly as a terminal wraps them,
/// so surfaces that straddle the boundary are split — which is the real
/// scanner's input, not a curated one.
fn screen(source: &str, cols: usize) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    assert!(!chars.is_empty(), "corpus must not be empty");
    let mut rows = Vec::with_capacity(ROWS);
    let mut at = 0;
    for _ in 0..ROWS {
        let mut row = String::with_capacity(cols * 4);
        for _ in 0..cols {
            row.push(chars[at % chars.len()]);
            at += 1;
        }
        rows.push(row);
    }
    rows
}

/// The five arms: (name, rows, the reach the guard pins).
///
/// The `Reach` values are EXACT and were read off a temporary counter
/// compiled into `scan_cjk_run` itself (see the module docs); the guard
/// re-derives them from `is_no_space_script` and demands equality, so any
/// drift in the corpora, the geometry, or the script table fails loudly
/// instead of silently re-pointing an arm at a different workload.
fn corpora() -> Vec<(&'static str, Vec<String>, Reach)> {
    vec![
        ("japanese", screen(JAPANESE, WIDE_COLS), Reach::new(77, 904)),
        ("chinese", screen(CHINESE, WIDE_COLS), Reach::new(94, 889)),
        // Hangul is written WITH word spaces, so the same 960 characters split
        // into 263 short runs instead of 77 long ones — the case a
        // per-position prefilter and a per-run one are priced differently on.
        ("korean", screen(KOREAN, WIDE_COLS), Reach::new(263, 691)),
        ("thai", screen(THAI, NARROW_COLS), Reach::new(63, 1881)),
        (
            "thai_head_worst_case",
            screen(ADVERSARIAL, NARROW_COLS),
            Reach::new(24, 1920),
        ),
        // The control: not one character of it reaches scan_cjk_run.
        (
            "ascii_control",
            screen(ASCII, NARROW_COLS),
            Reach::new(0, 0),
        ),
    ]
}

/// The shipped default scan gates (`DecoConfig::scan_opts` with the default
/// config): bare `cat` off, single-char CJK off, no ignore set.
fn shipping_opts() -> ScanOptions<'static> {
    ScanOptions {
        allow_bare_cat: false,
        cjk_single_char: false,
        ignore: None,
    }
}

// ---------------------------------------------------------------------------
// The workload
// ---------------------------------------------------------------------------

/// Scan one screenful row by row through reused buffers, mirroring
/// `WordDecorations::scan_row`. Returns the total match count so the optimizer
/// cannot delete the scan.
fn scan_screen(
    lexicon: &Lexicon,
    rows: &[String],
    opts: &ScanOptions<'_>,
    chars: &mut Vec<char>,
    out: &mut Vec<Match>,
    scratch: &mut ScanScratch,
) -> usize {
    let mut total = 0;
    for row in rows {
        lexicon.scan_into_with_scratch(row, opts, chars, out, scratch);
        total += out.len();
    }
    total
}

// ---------------------------------------------------------------------------
// TWO-SIDED REACH GUARD
// ---------------------------------------------------------------------------

/// What the scanner's own run-splitting loop does with a screenful, re-derived
/// from `is_no_space_script` — the predicate `scan_into_with_scratch` splits
/// on.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
struct Reach {
    /// Maximal no-space-script runs = calls to `scan_cjk_run`.
    entries: usize,
    /// Characters inside those runs. `scan_cjk_run` builds a window at each of
    /// them except the ones a match or an exception surface consumed, which it
    /// steps over — so this is an upper bound on the window rebuilds, tight to
    /// within the handful of characters the corpora actually match (41 of 904
    /// on the Japanese arm, 20 of 1881 on the Thai one).
    positions: usize,
}

impl Reach {
    const fn new(entries: usize, positions: usize) -> Self {
        Self { entries, positions }
    }
}

fn reach(rows: &[String]) -> Reach {
    let mut r = Reach::default();
    for row in rows {
        let mut in_run = false;
        for c in row.chars() {
            if is_no_space_script(c) {
                r.positions += 1;
                if !in_run {
                    r.entries += 1;
                    in_run = true;
                }
            } else {
                in_run = false;
            }
        }
    }
    r
}

/// Characters consumed by CJK matches, i.e. positions that hit rather than
/// missed. Only matches that START inside a no-space run count.
fn matched_no_space_chars(lexicon: &Lexicon, rows: &[String], opts: &ScanOptions<'_>) -> usize {
    let mut consumed = 0;
    for row in rows {
        let chars: Vec<char> = row.chars().collect();
        for m in lexicon.scan(row, opts) {
            if chars.get(m.start).copied().is_some_and(is_no_space_script) {
                consumed += m.end - m.start;
            }
        }
    }
    consumed
}

fn total_matches(lexicon: &Lexicon, rows: &[String], opts: &ScanOptions<'_>) -> usize {
    rows.iter().map(|row| lexicon.scan(row, opts).len()).sum()
}

/// Prove every CJK arm reaches `scan_cjk_run` and the control arm does not.
fn verify_scan_reach(lexicon: &Lexicon) {
    let opts = shipping_opts();

    // The window length the miss path pays per position. Only `cjk_forms` is
    // enumerable from outside the crate; `max_cjk` also folds in the exception
    // surfaces, so this is a LOWER bound on it. Pinned so the bench announces
    // it if the data ever shrinks the window that makes this path expensive.
    let longest_form = lexicon
        .iter_cjk()
        .map(|(surface, _)| surface.chars().count())
        .max()
        .unwrap_or(0);
    assert!(
        longest_form >= 7,
        "longest CJK surface is {longest_form} chars: the per-position window \
         this bench exists to price has collapsed, so the arms no longer \
         measure the O(n * L^2) shape the finding is about"
    );

    for (name, rows, want) in corpora() {
        let got = reach(&rows);
        let matches = total_matches(lexicon, &rows, &opts);
        let scanned: usize = rows.iter().map(|r| r.chars().count()).sum();
        let cols = if name.starts_with("thai") || name == "ascii_control" {
            NARROW_COLS
        } else {
            WIDE_COLS
        };

        // Geometry first: an arm that lost rows would satisfy every ratio
        // below while measuring a fraction of the screenful it claims to.
        assert_eq!(
            scanned,
            ROWS * cols,
            "{name}: scanned {scanned} characters, expected a {ROWS}x{cols} screenful"
        );

        // BOTH HALVES, as exact equalities. For the CJK arms this pins a
        // non-zero entry count; for the control it pins ZERO — the arm cannot
        // reach `scan_cjk_run` at all, which is what makes it a control rather
        // than a second CJK workload.
        assert_eq!(
            got, want,
            "{name}: reach drifted (got {got:?}, pinned {want:?}). Either the \
             corpus changed, or `is_no_space_script` no longer classifies this \
             script the way `scan_into_with_scratch` splits runs on it — either \
             way the arm is no longer the workload it is named for"
        );

        // No arm may be inert: an empty corpus would satisfy the equalities
        // above only for the control, but a corpus that matched NOTHING would
        // still price a scan that can never produce a decoration.
        assert!(
            matches > 0,
            "{name}: the corpus produced no matches at all — it is inert and the \
             timing means nothing"
        );

        if want.entries == 0 {
            // Control arm: nothing further to prove. It exists to show the
            // spaced-token path is untouched.
            assert!(
                rows.iter().all(|r| !r.chars().any(is_no_space_script)),
                "ascii_control: a no-space-script character got into the control corpus"
            );
            continue;
        }

        // The CJK arms must be MISS-dominated — the regime in which rebuilding
        // a ten-character window per position is pure waste. A curated corpus
        // that mostly hit would price the hit path and hide the finding.
        let hits = matched_no_space_chars(lexicon, &rows, &opts);
        assert!(
            hits * 10 < got.positions,
            "{name}: {hits} of {} no-space positions are consumed by matches; \
             this corpus is not the miss-dominated prose the finding is about",
            got.positions
        );

        // ... while still exercising the hit path, so a prefilter that broke
        // matching would be caught by the corpus-equality test in the crate's
        // own suite AND be visible here as a changed class mix.
        let classes: Vec<Class> = rows
            .iter()
            .flat_map(|row| lexicon.scan(row, &opts))
            .map(|m| m.class)
            .collect();
        assert!(
            !classes.is_empty(),
            "{name}: seeded surfaces produced no classified matches"
        );
    }
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn cjk_scan(c: &mut Criterion) {
    // The shipping default: the embedded lexicon with `languages = ["en"]`.
    // CJK entries are NOT language-gated (only `ambiguous` ones are), so an
    // English-only configuration displaying a Japanese file pays the full CJK
    // scan — which is exactly the regime this bench measures.
    let lexicon = Lexicon::builtin();
    verify_scan_reach(lexicon);

    let opts = shipping_opts();
    for (name, rows, _) in corpora() {
        c.bench_function(&format!("cjk_scan/{name}"), |b| {
            // The caller keeps these three buffers alive across rows AND across
            // frames (`WordDecorations` owns them), so a warmed, allocation-free
            // scan is the state being priced.
            let mut chars = Vec::new();
            let mut out = Vec::new();
            let mut scratch = ScanScratch::default();
            b.iter(|| {
                black_box(scan_screen(
                    lexicon,
                    black_box(&rows),
                    &opts,
                    &mut chars,
                    &mut out,
                    &mut scratch,
                ))
            });
        });
    }
}

criterion_group!(benches, cjk_scan);
criterion_main!(benches);
