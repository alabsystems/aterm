// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gaps of the rainbow line under typed words, read on glass: where a person types at a
//! shell's prompt on the last row of a full page, the flagship's vivid line parts round the ink
//! of each descender and ends before a bar or an underline caret, and no other vivid light of
//! the trail lies in those gaps of the line's pixel rows. A film of the shipping effects and the
//! real renderer ([`film_replay`], the film `lab trail-film` judges), read against its clean
//! twin.
//!
//! The rows are the line's own ([`ROW_RAIL_CH`] of the row at its bottom), not every row under
//! the baseline: the flagship's band is a wash behind the letters, a descender's included, as
//! the light glass is, and its foot fades through the rows over the line (on GitHub Light's
//! paper the comet's mint read 141 of a 140 spread there beside a `q`, a hair over the bar this
//! check gives the line). What lies in the line's rows is the line's, and a band laid there is
//! what this check was written for.
//!
//! The ribbon's own tests count the quads it lays (`aterm-effects`'
//! `the_body_s_band_parts_where_the_line_does`); this one reads pixels, so a layer those
//! tests do not name cannot fill a gap unseen. It was written when one did: the rail was
//! parted and the underline's body, a band in the same pixel rows, was not.

use crate::trail_film::{Take, film_replay};
use crate::trail_letters_read::*;
use aterm_effects::rainbow_kitty::ribbon::ROW_RAIL_CH;
use std::collections::HashMap;

/// The flagship.
const STYLE: &str = "rainbow + cat";
/// The underline's body, whose band lies in the line's rows.
const UNDERLINE_BODY: &str = "underline rainbow + cat";
/// The judge's ring round a letter's ink, px (`aterm-effects`' `LETTER_GUARD_RING`).
const RING: usize = 2;
/// The least spread of a pixel's channels that reads as the vivid line (`lab trail-film`'s
/// `line_cells_lit` counts the same).
const VIVID: u32 = 140;
/// How far a clean pixel's channel stands off the page to be ink.
const INK: u32 = 40;

/// What the frames of one film hold after its line is typed, summed.
#[derive(Debug, Default)]
struct Gaps {
    frames: usize,
    /// Cells with ink under the baseline, at most in one frame.
    descenders: usize,
    /// Vivid pixels of the line's rows under cells with no such ink.
    line: usize,
    /// Vivid pixels of the line's rows in a descender's columns and its ring.
    in_gaps: usize,
    /// Vivid pixels of the line's rows in the ring before the caret's cell.
    before_caret: usize,
}

fn spread(p: u32) -> u32 {
    let c = [(p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF];
    c.iter().max().expect("three") - c.iter().min().expect("three")
}

fn off(p: u32, q: u32) -> u32 {
    (0..3)
        .map(|i| ((p >> (8 * i)) & 0xFF).abs_diff((q >> (8 * i)) & 0xFF))
        .max()
        .expect("three")
}

/// Film [`Take::BottomPrompt`] in `style` on `look` under the caret `caret` sets and read the
/// typed row's line in every frame from 100 ms to 400 ms after the last key.
fn gaps(style: &str, look: Look, caret: &[u8]) -> Gaps {
    let take = Take::BottomPrompt;
    let (mut ev, tail) = take.events(look);
    ev.insert(0, (0, Ev::Out(caret.to_vec())));
    let (rows, cols) = take.grid();
    let last_key = ev
        .iter()
        .filter(|e| matches!(e.1, Ev::Key(..)))
        .map(|e| e.0)
        .max()
        .expect("the scene types");
    let first = (last_key + 100).div_ceil(16);
    let mut r = film_replay(style, look, rows, cols);
    r.keep = (first..first + 19).map(|k| k * 16).collect();
    let _ = r.run(&ev, tail);
    let mut out = Gaps::default();
    for k in &r.kept {
        let (cw, ch) = k.cell;
        let y1 = k.top + (k.caret.0 + 1) * ch;
        // The pixel rows under the baseline, where a descender's ink is: a 26 px row's letters
        // end on its 21st. The line lies in the last of them.
        let foot = y1 - ch * 5 / 26..y1;
        let line_rows = y1 - ((ROW_RAIL_CH * ch as f32).floor() as usize).max(1)..y1;
        let at = |px: &[u32], x: usize, y: usize| px[y * k.w + x] & 0x00FF_FFFF;
        let mut seen: HashMap<u32, usize> = HashMap::new();
        for y in foot.clone() {
            for x in 0..k.w {
                *seen.entry(at(&k.clean, x, y)).or_default() += 1;
            }
        }
        let page = *seen.iter().max_by_key(|e| e.1).expect("a row of pixels").0;
        let vivid = |x0: usize, x1: usize| {
            line_rows
                .clone()
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let p = at(&k.px, x, y);
                    p != at(&k.clean, x, y) && spread(p) >= VIVID
                })
                .count()
        };
        let mut descenders = 0;
        for col in 0..k.caret.1 {
            let x0 = k.left + col * cw;
            let ink: Vec<usize> = (x0..x0 + cw)
                .filter(|&x| foot.clone().any(|y| off(at(&k.clean, x, y), page) > INK))
                .collect();
            match (ink.first(), ink.last()) {
                (Some(&lo), Some(&hi)) => {
                    descenders += 1;
                    out.in_gaps += vivid(lo - RING, hi + RING + 1);
                }
                _ => out.line += vivid(x0, x0 + cw),
            }
        }
        let caret_x = k.left + k.caret.1 * cw;
        out.before_caret += vivid(caret_x - RING, caret_x);
        out.descenders = out.descenders.max(descenders);
        out.frames += 1;
    }
    out
}

/// At a shell's prompt on the last row of a full page, on GitHub Light and on a dark page, with
/// a bar and with an underline caret, in the flagship and in the underline's body: the vivid
/// line lies under the typed words, and no vivid pixel of the trail lies in the line's rows
/// within the judge's ring of a descender's ink or of the caret's cell.
///
/// Negative control: with the underline's body laid whole
/// (`ribbon::with_body_band_whole`), as it was, GitHub Light's film holds its band's vivid
/// pixels in the descenders' gaps and before the bar caret.
#[test]
fn the_line_under_typed_words_leaves_its_gaps_empty() {
    const BAR: &[u8] = b"\x1b[6 q";
    const UNDERLINE: &[u8] = b"\x1b[4 q";
    let films = [
        (STYLE, "github-light", GITHUB_LIGHT, "bar", BAR),
        (STYLE, "github-light", GITHUB_LIGHT, "underline", UNDERLINE),
        (STYLE, "dark", DARK, "bar", BAR),
        (STYLE, "dark", DARK, "underline", UNDERLINE),
        (UNDERLINE_BODY, "github-light", GITHUB_LIGHT, "bar", BAR),
        (UNDERLINE_BODY, "dark", DARK, "bar", BAR),
    ];
    let (reads, whole) = std::thread::scope(|s| {
        let held: Vec<_> = films
            .iter()
            .map(|&(style, _, look, _, caret)| s.spawn(move || gaps(style, look, caret)))
            .collect();
        let control = s.spawn(|| {
            aterm_effects::rainbow_kitty::ribbon::with_body_band_whole(|| {
                gaps(UNDERLINE_BODY, GITHUB_LIGHT, BAR)
            })
        });
        let reads: Vec<Gaps> = held
            .into_iter()
            .map(|h| h.join().expect("a film"))
            .collect();
        (reads, control.join().expect("the control's film"))
    });
    for ((style, theme, _, caret, _), read) in films.iter().zip(&reads) {
        eprintln!("line_gaps bottom-prompt {style}, {theme} {caret}: {read:?}");
    }
    for ((style, theme, _, caret, _), read) in films.iter().zip(&reads) {
        let theme = format!("{style}, {theme}");
        assert!(
            read.frames == 19 && read.descenders >= 5,
            "{theme}, {caret} caret: read {} frames and {} descenders of the pangram's five",
            read.frames,
            read.descenders
        );
        assert!(
            read.line > 1_000 * read.frames,
            "{theme}, {caret} caret: only {} vivid px under the typed words in {} frames, so \
             there is no line to part",
            read.line,
            read.frames
        );
        assert_eq!(
            (read.in_gaps, read.before_caret),
            (0, 0),
            "{theme}, {caret} caret: vivid px of the trail in the descenders' gaps and before \
             the caret, over {} frames",
            read.frames
        );
    }
    eprintln!("line_gaps control (the underline body's band whole, github-light, bar): {whole:?}");
    assert!(
        whole.in_gaps > 20 * whole.frames && whole.before_caret > 0,
        "control: with the underline body's band whole GitHub Light's film reads {} vivid px \
         in the descenders' gaps and {} before the caret over {} frames, so the reading does \
         not see the band",
        whole.in_gaps,
        whole.before_caret,
        whole.frames
    );
}
