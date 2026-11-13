// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Lumen on paper is a tracer, not Off: the trail gallery's typing scene replayed through the
// shipping effects pipeline and the real renderer, the host's post passes on, each kept frame
// read against its clean twin.

use crate::trail_letters_read::{
    DEFAULT_LIGHT, Ev, GITHUB_LIGHT, Kept, Look, ROWS, Replay, TYPING_TEXT, gallery_row_px, typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::pipeline::EffectsPipeline;

/// The last key of the typing scene, ms.
const LAST_KEY: u64 = 1_000 + 60 * (TYPING_TEXT.len() as u64 - 1);

/// The replay's frame step, ms (`trail_letters_read`'s).
const FRAME_MS: u64 = 16;

/// How long after the last key the law reads the tracer, ms: the replay's frame 132 ms after
/// it, the first on or past this.
const AFTER: u64 = 120;

/// How long the replay runs after the last key, ms, and so the latest frame the later control
/// reads: [`TAIL`]'s key comes 300 ms before the last, and a paper Lumen spark lives 0.9 s at
/// most (`LUMEN_PAPER_LIFE_MAX`).
const RUN: u64 = 600;

/// The cells behind the caret a row must hold no ink over to count as clear ([`clear_rows`]):
/// the second to the eighth, `rown fo` of the scene's `brown fox`.
const BEHIND: std::ops::Range<usize> = 2..9;

/// The cell behind the caret the tracer's strength is read in: the fourth, the space between
/// the scene's `brown` and `fox`, the one cell of [`BEHIND`] that holds no letter. No ink covers
/// its light.
const BLANK: usize = 4;

/// The cell behind the caret the tracer's tail is read in: the sixth, the letter typed five
/// keys before the last (`w` of the scene's `brown`).
const TAIL: usize = 6;

/// The least share of [`BLANK`]'s light the tracer lays in [`TAIL`], over the rows no letter
/// inks ([`clear_rows`]), as a fraction's denominator: an eighth.
const TAIL_SHARE: u64 = 8;

/// The least light paper Lumen lays in [`BLANK`] on each look, levels a pixel: [`light_in`]
/// over the whole cell, divided by its area. Each look's tracer lays its own light against its
/// own page, so each has its own floor, set halfway between the least the shipped tracer lays
/// there and the most a tracer drawn at 0.9 of its coverage lays, over every face and size the
/// law's doc names, to the nearest half level.
const STRENGTH: [(Look, f64); 2] = [(GITHUB_LIGHT, 36.0), (DEFAULT_LIGHT, 42.5)];

/// How far `f` lies off its clean twin `c`: its largest channel difference (`trail-contrast`'s
/// `diff_vs_clean` measure, with no threshold).
fn off_clean(f: u32, c: u32) -> u32 {
    [16u32, 8, 0]
        .iter()
        .map(|&sh| ((f >> sh) & 0xff).abs_diff((c >> sh) & 0xff))
        .max()
        .unwrap_or(0)
}

/// The pixel rows of the caret's row in `k`.
fn caret_row(k: &Kept) -> std::ops::Range<usize> {
    let y0 = k.top + k.caret.0 * k.cell.1;
    y0..y0 + k.cell.1
}

/// The pixel columns of the cell `back` cells behind the caret in `k`.
fn cell_cols(k: &Kept, back: usize) -> std::ops::Range<usize> {
    let x0 = k.left + k.caret.1.saturating_sub(back) * k.cell.0;
    x0..x0 + k.cell.0
}

/// The rows of the caret's row in `k` its clean twin lays no ink on in [`BEHIND`]'s cells: one
/// colour from the eighth cell back to the second.
fn clear_rows(k: &Kept) -> Vec<usize> {
    let x0 = cell_cols(k, BEHIND.end - 1).start;
    let x1 = cell_cols(k, BEHIND.start).end;
    caret_row(k)
        .filter(|&y| {
            let page = k.clean[y * k.w + x0];
            (x0..x1).all(|x| k.clean[y * k.w + x] == page)
        })
        .collect()
}

/// The light the effects laid in `k` on `rows` of the cell `back` cells behind the caret: each
/// pixel's [`off_clean`], summed. No pixel counts whole or not at all by which side of a
/// threshold it falls on: a soft edge the pixel grid lays a level brighter or dimmer moves the
/// sum by that level.
fn light_in(k: &Kept, back: usize, rows: impl IntoIterator<Item = usize>) -> u64 {
    let cols = cell_cols(k, back);
    rows.into_iter()
        .flat_map(|y| cols.clone().map(move |x| y * k.w + x))
        .map(|i| u64::from(off_clean(k.px[i], k.clean[i])))
        .sum()
}

/// What the law reads in a frame: the light in [`BLANK`] and that cell's area, and over the
/// rows no letter inks ([`clear_rows`]) the light in [`TAIL`] and in [`BLANK`].
struct Read {
    blank: u64,
    area: u64,
    tail: u64,
    base: u64,
}

impl Read {
    fn of(k: &Kept) -> Self {
        let clear = clear_rows(k);
        Self {
            blank: light_in(k, BLANK, caret_row(k)),
            area: (k.cell.0 * k.cell.1) as u64,
            tail: light_in(k, TAIL, clear.iter().copied()),
            base: light_in(k, BLANK, clear),
        }
    }

    /// [`BLANK`]'s light, levels a pixel.
    fn strength(&self) -> f64 {
        self.blank as f64 / self.area as f64
    }

    /// Whether the tracer lays `floor` levels a pixel or more in [`BLANK`].
    fn strong(&self, floor: f64) -> bool {
        self.blank as f64 >= floor * self.area as f64
    }

    /// Whether the tracer reaches [`TAIL`]: it lays light in [`BLANK`]'s clear rows, and at
    /// least a [`TAIL_SHARE`]th of that in [`TAIL`]'s.
    fn reaches(&self) -> bool {
        self.base > 0 && self.tail * TAIL_SHARE >= self.base
    }

    /// [`TAIL`]'s light over [`BLANK`]'s in the clear rows.
    fn share(&self) -> f64 {
        self.tail as f64 / self.base.max(1) as f64
    }
}

/// The typing scene under `style` on `look`, the post passes on, `controls` applied, in the
/// window's face at the size that lays the trail gallery's row ([`gallery_row_px`]): the
/// replay's frames from the first on or past [`AFTER`] ms after the last key, the law's, to the
/// last on or before `until` ms after it, the law's frame at least.
fn typed_frames(
    style: &str,
    enabled: bool,
    look: Look,
    until: u64,
    controls: impl Fn(&mut EffectsPipeline),
) -> Vec<Kept> {
    let mut r = Replay::sized_at(style, look, TrailColor::Automatic, ROWS, gallery_row_px());
    if !enabled {
        let spec = crate::trail_letters_read::Spec::of(style);
        r.fx.set_cursor_glow(
            false,
            spec.trail,
            TrailColor::Automatic,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            look.cursor,
        );
        if spec.tall {
            r.fx.set_ribbon_shape(true, spec.flat);
        }
    }
    r.post = true;
    controls(&mut r.fx);
    let law = (LAST_KEY + AFTER).div_ceil(FRAME_MS) * FRAME_MS;
    r.keep = (law..=(LAST_KEY + until).max(law))
        .step_by(FRAME_MS as usize)
        .collect();
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let _ = r.run(&ev, RUN);
    assert_eq!(
        r.kept.len(),
        r.keep.len(),
        "fixture: every frame asked for is kept"
    );
    r.kept
}

/// Lumen on paper keeps a tracer behind the caret when the typing stops. Its sparks lived as
/// briefly as the dark face's (about a key and a tenth of a second), so its paper tracer was two
/// cells long and gone 120 ms after the last key: the gallery's `t120` and `t470` frames of Lumen
/// on paper were Off's, pixel for pixel (composed r2's three judges, O9(b)). 120 ms after the last
/// key ([`AFTER`]) it is still as strong four cells behind the caret, in [`BLANK`], as [`STRENGTH`]
/// holds, where Off lays nothing, and it still reaches the sixth cell back: over the rows no letter
/// inks, the light it lays in [`TAIL`] is at least an eighth of the light it lays in [`BLANK`]
/// ([`TAIL_SHARE`]). Controls: `set_lumen_paper_tracer` off gives it the dark face's brief sparks
/// again, which lay no light in either cell and fail both reads; and as the tracer fades, the first
/// of the replay's later frames (to [`RUN`]) that fails the tail read still holds half the light
/// [`BLANK`]'s clear rows hold at the law's frame, so the tail read says no to a frame with light
/// to weigh it against. That control bounds how long a spark may live: under the shipped cap
/// (`LUMEN_PAPER_LIFE_MAX`, 0.9 s) every `LUMEN_PAPER_CHAIN_KEYS` passes it, and so do caps of
/// 0.95 and 1.0 s, but at 1.1 s or more no frame to [`RUN`] fails the tail read, and the control
/// goes red while both of the law's reads pass, so a longer cap needs [`RUN`] raised with it. It
/// catches a [`TAIL_SHARE`] loosened to a 64th, not one loosened to a 32nd: the eighth's
/// strictness rests on the named mutants below.
///
/// Both reads are light, not a count of pixels past a threshold, and both are taken where the
/// scene's letters lay no ink. The tracer's band is soft at its edges, and where a row's height
/// lays them on the pixel grid moves such a count by a pixel row; and the letters lie over the
/// band, and how much of its light they take, and in which cells, the face decides. The law's first
/// read (eb2905f8cb, on macOS's SF Mono at 22 px, 14 x 26 px cells) counted the pixels more than 12
/// levels off from two to eight cells back, against a floor of a cell and a half's worth; its
/// second (a0816efd1c) a cell's worth, and the sixth cell's light over the third's (`f`) on every
/// row. Under twelve faces at 16 to 28 px (JetBrains Mono, bundled and installed, DejaVu Sans Mono,
/// Liberation Mono, Nimbus Mono PS, Noto Mono, Noto Sans Mono, Ubuntu Mono, Ubuntu Sans Mono, Tlwg
/// Mono, Tlwg Typo and Mitra) the shipped tracer counts 1.373 to 1.775 cells' worth and lays 0.108
/// to 0.160 of the third cell's light in the sixth; with `LUMEN_PAPER_END_LIT` at 1.0 in place of
/// 0.75 (3ad53d99af's mutant) it counts 1.30 to 1.67 and lays up to 0.116, so neither read tells
/// the two apart on every face. [`BLANK`] and the clear rows hold still: shipped, [`BLANK`] reads
/// 37.73 to 38.09 levels a pixel on GitHub Light and 44.40 to 44.88 on Default Light, and
/// [`TAIL`]'s share 0.143 to 0.146 on both.
///
/// Over the same faces and sizes a tracer drawn at 0.9 of its coverage lays 33.95 to 34.31 and
/// 39.99 to 40.43 levels a pixel in [`BLANK`], and main's count refused it at the test's own frame
/// (445 px on GitHub Light, under 468). [`STRENGTH`] lies halfway between it and the shipped
/// tracer, which clears it by 4.2 levels or more on every lit pixel of [`BLANK`], where the dimmer
/// one falls 4.2 or more short. A tracer drawn at 0.95 lies on it (35.83 to 36.22 and 42.29 to
/// 42.75; 35.92 at the test's own frame, a fail), and so does a band reaching 0.19 of the cell
/// height either side of its spine in place of 0.20 (35.86 to 36.22 and 42.20 to 42.63; 35.95);
/// one reaching 0.17 (32.1 to 32.6 and 37.7 to 38.3) or 0.14 (26.3 to 27.0 and 31.0 to 31.8) fails
/// on every face and size. [`TAIL_SHARE`] fails `LUMEN_PAPER_END_LIT` at 1.0 (0.103 to 0.106)
/// and at 0.9 (0.115 to 0.118) and passes it at 0.5 (0.170 to 0.173); `LUMEN_PAPER_CHAIN_KEYS` at
/// 6.5 in place of 7.0 fails it (0.042 to 0.045), and at 6.0 fails both reads (23.2 to 27.7
/// levels a pixel; a share of 0.006 or less). The shipped tracer clears [`TAIL_SHARE`] by 1.39
/// levels or more on every lit pixel of the two cells' clear rows, each moved against it, and
/// END_LIT 1.0 falls 1.52 or more short. The tail is read on the tracer's fade: its share is 0.086
/// to 0.089 a frame later (148 ms after the last key), so the shipped tracer clears an eighth by
/// about 5 ms, and one that fades sooner fails.
///
/// One that lingers passes, the later control with it. With `LUMEN_PAPER_CHAIN_KEYS` at 7.2 to 10
/// the law passes on every face and size, and the first frame to fail the tail read comes 164 to
/// 292 ms after the last key in place of the shipped tracer's 148, with 0.89 or more of the law's
/// frame's light in [`BLANK`]'s clear rows. At 12 and at 15 (a spark life of 0.82 s and the 0.9 s
/// cap, `LUMEN_PAPER_LIFE_MAX`) it comes at 388 and 468 ms with 0.78 or more, and the law passes on
/// the bundled face and on every frame but five of Default Light's, Tlwg Mono at 16 to 18 px and
/// Tlwg Typo at 16 and 17, where the effects lay 37.1 to 41.5 levels a pixel in [`BLANK`]: no ink
/// lies there, but the letter guard, reading the neighbouring letters' rasters, cuts its light when
/// the tracer is that bright (48.5 or more with the guard off); for the shipped tracer the guard
/// takes nothing from [`BLANK`] (Linux arm64, 2026-11-07). Not measured on macOS, including
/// whether the guard reaches [`BLANK`] beside SF Mono's letters.
#[test]
fn lumen_on_paper_keeps_a_tracer_when_the_typing_stops() {
    for (look, floor) in STRENGTH {
        let off = typed_frames("off", false, look, AFTER, |_| {}).remove(0);
        let frames = typed_frames("lumen", true, look, RUN, |_| {});
        let brief = typed_frames("lumen", true, look, AFTER, |fx| {
            fx.set_lumen_paper_tracer(false)
        })
        .remove(0);
        let lumen = &frames[0];
        let (cw, ch) = lumen.cell;
        let blank_cols = cell_cols(lumen, BLANK);
        let blank_is_clear = caret_row(lumen).all(|y| {
            let page = lumen.clean[y * lumen.w + blank_cols.start];
            blank_cols
                .clone()
                .all(|x| lumen.clean[y * lumen.w + x] == page)
        });
        let off_light =
            light_in(&off, BLANK, caret_row(&off)) + light_in(&off, TAIL, caret_row(&off));
        let (read, brief) = (Read::of(lumen), Read::of(&brief));
        // The first frame after the law's that the tail read says no to.
        let faded = frames[1..]
            .iter()
            .map(|k| (k.ms - LAST_KEY, Read::of(k)))
            .find(|(_, r)| !r.reaches());
        assert!(
            blank_is_clear,
            "{} ({cw} x {ch} px cells): fixture: the cell {BLANK} back holds no letter",
            look.name
        );
        assert_eq!(
            off_light, 0,
            "{} ({cw} x {ch} px cells): fixture: Off lays no light {BLANK} and {TAIL} cells \
             behind the caret",
            look.name
        );
        assert!(
            read.strong(floor),
            "{} ({cw} x {ch} px cells): {AFTER} ms after the last key paper Lumen lays {:.2} \
             levels a pixel in the cell {BLANK} back, under {floor}",
            look.name,
            read.strength()
        );
        assert!(
            read.reaches(),
            "{} ({cw} x {ch} px cells): {AFTER} ms after the last key, over the rows no letter \
             inks, paper Lumen lays {} in the cell {TAIL} back, {:.3} of the {} it lays \
             {BLANK} back, under an eighth",
            look.name,
            read.tail,
            read.share(),
            read.base
        );
        assert!(
            !brief.strong(floor) && !brief.reaches(),
            "{} ({cw} x {ch} px cells): control: with the dark face's brief sparks paper Lumen \
             lays {:.2} levels a pixel in the cell {BLANK} back, not under {floor}, or lays \
             {} in the cell {TAIL} back, not under an eighth of the {} it lays {BLANK} back",
            look.name,
            brief.strength(),
            brief.tail,
            brief.base
        );
        assert!(
            faded.as_ref().is_some_and(|(_, f)| f.base * 2 >= read.base),
            "{} ({cw} x {ch} px cells): control: no frame to {RUN} ms after the last key fails \
             the tail read while the rows no letter inks {BLANK} back keep half the {} they hold \
             {AFTER} ms after it: {}",
            look.name,
            read.base,
            faded.map_or_else(
                || "every one passes it (a spark that lives past LUMEN_PAPER_LIFE_MAX's 0.9 s \
                    needs RUN raised)"
                    .to_owned(),
                |(ms, f)| format!(
                    "the first that fails it, {ms} ms after the last key, holds {} there and {} \
                     {TAIL} back",
                    f.base, f.tail
                )
            )
        );
    }
}
