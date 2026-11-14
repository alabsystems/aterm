// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-film <out_dir> [style]`: a cursor trail judged in motion, frame by frame, against three laws.
//!
//! A film is one scene (a scripted line of typing) replayed with no window: `aterm-render`'s `Replay` harness runs
//! the shipping effects pipeline and the real renderer on its own clock, the host's post passes on, and its
//! `trail_film::film` test writes every 16 ms frame as `trail-gallery` writes a real one (`film.<ms>.png`, the clean
//! twin, and the text, cursor and status sidecars, one `film.dims.txt`). This verb runs that test, then reads each
//! film:
//!
//! - FLASH, [`Flicker`]: every pixel of every frame, the caret's cell and the pet's box included, by WCAG 2.3.1's
//!   general flash ([`Turns`]); the worst pixel's layer by `trail-census`'s sort ([`Scene`]).
//! - CONTRAST, [`ContrastRead`]: every lettered cell and the caret of every frame, by `trail-contrast`'s report.
//! - PET, [`judge_pet`]: every frame's companion, by `trail-companion`'s frame reader, and its alpha and pose from
//!   the status row.
//! - LINE, [`LineTrack`]: the cells typed along the caret's row that have a vivid line under them, a reading and
//!   not a law (most styles draw no line).
//!
//! The scratch probe this grew from (never committed) counted something else: a pixel's rises off its clean twin in
//! the caret's rows. [`TwinRises`] keeps that count beside the law so its readings can be set against this verb's.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Stdio;

use crate::instance::{take_flag, take_value};
use crate::paths::{from_caller, paths};
use crate::pyjson::J;
use crate::trail_census::{Layer, Scene};
use crate::trail_companion::{SHAVE, Span, frames_under, read_frame, status_field};
use crate::trail_contrast::{
    Grid, Image, LEGIBLE, PxBox, Rgb, caret_cell, clean_twin, frame_dims, grid_from_dims, judged,
    lowered_under, luminance, read_text, sidecar,
};
use crate::{Fail, Res};

/// WCAG 2.3.1's general flash: a pair of opposing changes in relative luminance of this much or more (a tenth of
/// the maximum, 1.0)...
///
/// The law is the brand guide's, word for word (`BRAND.md` section 7: "A flash is a pair of opposing changes in
/// relative luminance of 10 % or more where the darker state is below 0.80 (WCAG 2.3.1)"), in linear light on
/// every theme. The design that asked for this verb wanted a perceptual threshold, so that a dark page's lights
/// could not pass by construction; the guide was written after it and names this one, so the perceptual count is
/// printed beside the law ([`LIGHTNESS_SWING`]) and fails nothing. It reads 2 to 6 times the law's pixels on a
/// dark page: whoever tightens the law for dark pages changes the guide's sentence first.
pub const FLASH_SWING: f64 = 0.10;
/// ...where the darker state's relative luminance is below this.
pub const FLASH_DARK_BELOW: f64 = 0.80;
/// The most flashes a pixel may make in any one second (the brand guide's bound, 3.0 Hz). Two opposing turns make
/// a flash, so the bound is on turns: a pixel that turns more than twice this in a second flashes faster than
/// 3 Hz, its seventh turn being the fourth flash begun ([`pixel_read`]).
pub const FLASH_MOST: usize = 3;
/// The second a pixel's flashes are counted in, ms.
pub const SECOND_MS: u32 = 1_000;
/// The lightness reading's swing: a tenth of CIE L*'s range. On a white page that is the law's own swing (relative
/// luminance 1.0 to 0.79 is L* 100 to 91); on a dark page it is a change a person sees that the law's tenth of
/// linear light never reaches (a dark theme's page at 0.006 lit to 0.03 is 16 L*). A reading, not a law
/// ([`FLASH_SWING`] says why).
pub const LIGHTNESS_SWING: f64 = 0.10;
/// The scratch probe's rise off the clean twin, and the distance a pixel must come back under before it can rise
/// again, in relative luminance.
pub const TWIN_RISE: f64 = 0.10;
pub const TWIN_REARM: f64 = 0.03;
/// The rows each side of the caret's the scratch probe read.
pub const TWIN_ROWS: i64 = 2;
/// The caret's bar: a mark, not words (WCAG 1.4.11, 3:1).
pub const CARET_BAR: f64 = 3.0;
/// The least time a pose is shown, ms.
pub const POSE_MS: u64 = 100;
/// A fall, then a rise, of the pet's alpha by this much or more is a blink.
pub const ALPHA_DIP: u8 = 26;
/// A line's pixel is vivid at this chroma (its greatest channel less its least, of 255) or more. The flagship's
/// rail under an open row, the line this reading asks for on every row, reads a median of 188 to 207 and a tenth
/// of its pixels under 151 to 173 on its four pixel rows, dark and GitHub Light (the typing scene, bar caret); the
/// dim foot band it left on a row with letters under it read a median of 62 to 130.
pub const LINE_CHROMA: u8 = 140;
/// ...and at a relative luminance within this: the rail's ink is solved to 0.20 to 0.54 on a dark page and 0.14 to
/// 0.54 on paper, and its pixels under an open row read 0.12 to 0.44 on the dark page and 0.16 to 0.59 on GitHub
/// Light's (a twentieth of them under, a twentieth over). The foot band read 0.02 to 0.07 on dark (a median, row
/// by row) and 0.68 to 0.75 on GitHub Light.
pub const LINE_LUMA: (f64, f64) = (0.12, 0.60);
/// A cell has a line under it when half its pixel columns or more hold this many vivid pixels, one over the
/// other, that the effects drew (they differ from the clean twin's): a line a pixel thick is a hairline.
pub const LINE_THICK: usize = 2;
/// Where a cell's line is looked for, in cell heights: from this far down the cell (under the x-height's body,
/// over the baseline) ...
pub const LINE_BAND_FROM: f64 = 0.70;
/// ...to this far under its row's bottom, where an open row's rail lies (2 to 6 px under a 26 px row).
pub const LINE_BAND_UNDER: f64 = 0.30;
/// A typed cell is judged once it is this old, ms: the line is laid from the hand over a few frames.
pub const LINE_SETTLE_MS: u64 = 96;
/// ...on the frames up to this long after the last key, ms: the reading is of the row while it is typed on.
pub const LINE_LIVE_MS: u64 = 250;
/// ...when the run of typing holds this many such cells or more.
pub const LINE_CELLS: usize = 8;
/// A cell has ink of its own under the baseline when its clean twin holds a pixel off the cell's most common
/// colour this far down the cell or further: the last 0.12 of a cell's height, where a line inside the row lies
/// (a 26 px row's letters end on its 21st pixel row and its descenders on its 25th). A line parts round such
/// ink, as a typeset underline skips a descender, so these cells are also counted apart.
pub const LINE_FOOT_FROM: f64 = 0.88;
/// The flagship: the style a run with none named films.
pub const FLAGSHIP: &str = "rainbow";
/// The pet a run with none named films beside it: the shipped default.
pub const FLAGSHIP_PET: &str = aterm_effects::cursor_pet::DEFAULT_PET;

/// The themes a film is photographed on: `aterm-render`'s `trail_film::THEMES`.
pub const THEMES: [&str; 6] = [
    "dark",
    "github-light",
    "default-light",
    "solarized-dark",
    "dim-grey",
    "paper-grey",
];
/// The caret shapes: `trail_film::CARETS`.
pub const CARETS: [&str; 3] = ["block", "bar", "underline"];
/// The scenes and what each types: `trail_film::SCENES`. The first [`DEFAULT_SCENES`] are a run's own. The pages
/// and lines of the resident scenes are `trail_scenes`'s, which the replay mounts too.
pub const SCENES: [(&str, &str); 25] = [
    (
        "typing",
        "24 keys at 60 ms on a blank page's prompt, then the fade",
    ),
    ("burst", "67 keys at 50 ms, then a 4.6 s rest"),
    (
        "context",
        "the typing line between a powerline row and a selected row, lettered rows past both",
    ),
    (
        "enter",
        "the typing line on the last row of a full page, Enter, and the answer scrolling it",
    ),
    (
        "backspace",
        "the typing line, then 12 Backspaces 50 ms apart and the fade",
    ),
    (
        "bottom-prompt",
        "a shell prompt on the last row of a full page, output on the row over it: 48 keys at 60 ms, held",
    ),
    (
        "composer",
        "Claude Code's input box (a grey rule, the prompt, a rule, two grey rows) under its transcript: the same 48 keys",
    ),
    (
        "jump",
        "a 48-key line, then Home and End: two moves of the whole line along its row",
    ),
    (
        "enter-long",
        "a 40-cell command on the last row of a full page, Enter, and the answer scrolling it",
    ),
    (
        "wall-69",
        "the burst's 69-cell line on the last row of a full page 69 columns wide: no cell past it",
    ),
    ("wall-70", "the same on 70 columns: one cell past the line"),
    ("wall-71", "the same on 71 columns: two cells past the line"),
    ("wall-72", "the same on 72 columns"),
    ("wall-73", "the same on 73 columns"),
    (
        "coloured-prompt",
        "a command typed as fish paints it: each word in its syntax ink, a grey suggestion past the caret",
    ),
    (
        "verdict",
        "true, false, sleep 3, sleep 3; false and a sleep that Ctrl-C ends, each between shell-integration marks",
    ),
    (
        "long-build",
        "a build entered on the last row: forty lines 100 ms apart, then exit 0 and the next prompt",
    ),
    (
        "steam",
        "the typing line at 50 ms between lettered rows, then 12 erases",
    ),
    ("cjk", "a line with four wide glyphs"),
    ("wrapped", "a line typed on past the margin"),
    ("fold-rest", "a burst whose last three keys cross the wrap"),
    (
        "last-row",
        "a burst along the last row under a full page, Enter, and the typing line",
    ),
    (
        "move-up",
        "a command entered, then the program moves the caret up onto a lettered row",
    ),
    ("split", "the burst in a 6-row pane"),
    (
        "focus-wrap",
        "a 32-column composer on the alternate screen, typed across its wrap",
    ),
];
/// The scenes a run films when none is named.
pub const DEFAULT_SCENES: usize = 5;

/// The planted star's side, px ([`plant_star`]).
pub const STAR_SIDE: i64 = 3;
/// The planted star's half period, ms: lit for one, dark for the next, 8 turns a second (4 flashes, over
/// [`FLASH_MOST`]).
pub const STAR_HALF_MS: u64 = 125;

/// Where [`plant_star`] paints in a `width` x `height` frame: a square one pixel in from its bottom-right corner,
/// in the window's padding past the last cell.
#[must_use]
pub fn star_box(width: usize, height: usize) -> PxBox {
    let (x1, y1) = (
        i64::try_from(width).unwrap_or(i64::MAX) - 1,
        i64::try_from(height).unwrap_or(i64::MAX) - 1,
    );
    PxBox {
        x0: x1 - STAR_SIDE,
        y0: y1 - STAR_SIDE,
        x1,
        y1,
    }
}

/// A planted star (`--plant-star`): the judge's own proof that it sees a flash. A [`STAR_SIDE`] square
/// ([`star_box`]) painted into the frame the flash law reads at `ms`, on the frames of every other
/// [`STAR_HALF_MS`] from 0: white on a dark page, black on a light one, the pixel's own colour between. It is
/// never painted into the clean twin, so the page cannot excuse it, and the trail-contrast, line and pet
/// readings read the frame as it was written.
pub fn plant_star(frame: &mut Image, ms: u64) {
    if (ms / STAR_HALF_MS) % 2 == 1 {
        return;
    }
    let b = star_box(frame.width, frame.height);
    let (Ok(x), Ok(y)) = (usize::try_from(b.x0), usize::try_from(b.y0)) else {
        return;
    };
    if x >= frame.width || y >= frame.height {
        return;
    }
    let lit = if luminance(frame.at(x, y)) < 0.5 {
        [255, 255, 255]
    } else {
        [0, 0, 0]
    };
    frame.paint(b, lit);
}

/// What a planted star read: its box and how many of its pixels the flash law put over the bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StarRead {
    pub at: PxBox,
    pub over: usize,
}

/// The verb's own help (`trail-film --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-film <out_dir> [style] [--pet off|cat|dog] [--theme NAME[,NAME]...|both]
         [--caret NAME[,NAME]...|both] [--scene NAME[,NAME]...|--all-scenes] [--no-replay] [--report-only]
         [--plant-star]
       targo --unverified lab trail-film --list

A cursor trail judged in motion. Each film is one scene (a scripted line of typing) replayed with no window through
the shipping effects and the real renderer, every 16 ms frame written under
<out_dir>/<style>-<pet>/<theme>_<caret>_<scene>/ as trail-gallery writes a frame (film.<ms>.png, .clean.png, .text.txt,
.cursor.txt, .status.txt, film.dims.txt), so trail-contrast, trail-companion and trail-census read one too. The
style (a cursor_trail_style) defaults to \"rainbow\", --pet (a cursor_pet) to cat, --theme and --caret to both (dark and github-light; block and bar),
--scene to typing, burst, context, enter and backspace. --no-replay judges the films already there. --list names
the themes, carets and scenes. --plant-star checks the judge itself: it paints a 3 px square one pixel in from
each frame's bottom-right corner, lit every other eighth of a second (8 turns a second, white on a dark page, black
on a light one), into the frame the flash law reads and not its clean twin. A STAR line says how many of its 9 px
the law put over the bound, a film where it missed one breaks (star:), and the flash_* counts include the star's.

The two rows a person types on are the scenes bottom-prompt (a shell prompt on the last row of a full page) and
composer (Claude Code's input box). jump, enter-long, wall-69 to wall-73, coloured-prompt, verdict and long-build
are filmed on a shell's full page too; each starts typing at 2 s, once the pet has come to the caret.

Per film, four lines and the laws it breaks:
FLASH     every pixel of every frame, by WCAG 2.3.1: a flash is a pair of opposing changes in relative luminance
          of 0.10 or more where the darker state is below 0.80. A change is a turn, two turns a flash.
          flash_turns_most is the most turns one pixel makes in any second and flash_most the whole flashes in
          them. flash_px_over3 is the pixels that flash faster than 3 Hz: more than 6 turns in a second less one
          frame step (a turn is seen on the frame after it, so a 3.0 Hz light can show 7 turns in 992 ms of 16 ms
          frames and is not over; one at 3.1 Hz is). flash_cells_over3 is the cells that hold such a pixel, worst
          the pixel with the most turns, their times and the layers trail-census sorts it into on those frames
          (ribbon, rail, star, pet, caret, other, none; a meteor is not a layer it knows). pet_*, caret_*,
          icon_*, page_* and trail_* are the same count apart by place, and a place is a box, not what drew
          the pixel: a pixel is the trail's own when fewer than half its turns fell in the pet's drawn box,
          the caret's cell or a word icon's box (sparkle words are on, as in the app), else it is the place's
          that held the most of them. The pet's box holds a turn on the frame it held the pixel and on the
          frame after, when the pixel goes back to the page as the pet moves on; the caret's cell and an icon's
          box hold their own frame's turns alone. Trail light under a pet that stands on it counts as the pet's.
          A pixel no box holds and over the bound is the page's own when the page alone (its clean twin, by the
          same law) is over the bound there and the pixel's turns left once each of the page's has taken one
          within a frame of it are not: text scrolling up the page. Under the bound it is the page's when the
          page's turns take half of its busiest second's. trail_worst's layers say what the census made of it.
          The law is on the effects: flash_px_over3 less page_px_over3. Two readings are not the law:
          lightness_* counts the same turns in CIE L* (a swing of 10, no ceiling), which a dark page's lights
          reach and the law's tenth of linear light does not; twin_* is the earlier scratch probe's count (rises of
          0.10 off the clean twin in the caret's rows and two each side, the caret's cell left out), with the
          pixels of it in the pet's box and the trail's own apart.
CONTRAST  trail-contrast's report of every frame: contrast_floor the lowest letter (local contrast), lowered the
          readings the effects took under 4.5:1 (a letter the page itself draws lower keeps 97% of its own),
          caret_floor the caret's lowest reading.
LINE      the cells typed along the caret's row (the caret's steps of one or two columns; a jump, a Backspace or
          a new row starts the run again) that have a vivid line under them: half a cell's pixel columns or more
          with 2 px, one over the other, that the effects drew, of chroma 140 or more (greatest channel less
          least) and relative luminance 0.12 to 0.60, between 0.70 of the way down the cell and 0.30 of a cell
          under its row. Those are an open row's rail (chroma about 200, luminance 0.16 to 0.57); the dim foot
          band of a row with letters under it is not. A cell is judged from 96 ms after its key, on the frames up
          to 250 ms after the last key, once the run holds 8. line_cells_lit is the share on the frame that
          judged the most cells, the whole line typed (the least such frame; none: no frame was judged), and
          line_at its cells and time. line_free_lit is that frame's share of the cells with no ink of their own
          under the baseline (none in the last 0.12 of the cell in the clean twin): a line parts round a
          descender as a typeset underline does, so g, j, p, q and y are not lit by a line that is whole.
          line_least is the least share of any judged frame, line_mean the mean, line_frames the frames judged.
          A reading, not a law: most styles draw no line.
PET       trail-companion's reading of every frame: companion_on_letters and companion_on_caret the frames whose
          body or flying head meets a lettered cell or the caret's by a pixel or more; alpha_dip the times the
          pet's alpha fell by a tenth or more and rose again (a leave and return through the window's edge is not
          one); min_pose_dwell_ms the shortest time one pose stayed, poses_under_100ms how many stayed less.

laws: flash_px_over3 - page_px_over3 = 0; lowered = 0 and caret_floor >= 3.0; companion_on_letters = 0, companion_on_caret = 0,
alpha_dip = 0, poses_under_100ms = 0 and no frame unread.
exit: 0; 1 a film breaks a law (--report-only: 0); 2 a usage error, or a film that could not be made or read
";

/// How a pixel's value turned over a film: its confirmed rises and falls of [`Turns::swing`] or more, each taken
/// from the last extreme (a zigzag), so a slow drift across many frames is one change and a wobble under the
/// swing is none.
///
/// A fall is measured from the highest value since the last turn, whatever the pixel did between: 1.0, 0.85,
/// 0.88, 0.79 is one fall of 0.21, not a last step of 0.09. The step down to 0.85 was no turn only because its
/// darker state was not yet under [`Turns::dark_below`], and the rise to 0.88 is a wobble under the swing; the
/// change the eye is given is the whole of it, from 1.0 to 0.79. Measuring from the last local extreme instead
/// would lose every change that arrives in steps, a fade included.
pub struct Turns {
    swing: f64,
    dark_below: f64,
    /// Per pixel: [`UNSEEN`], `0` no turn yet, `1` the last turn was a rise, `-1` a fall.
    dir: Vec<i8>,
    /// Per pixel, the lowest value since the last turn (before any: since the film began).
    lo: Vec<f64>,
    /// Per pixel, the highest.
    hi: Vec<f64>,
    /// The times of each pixel's turns, ms.
    at: HashMap<u32, Vec<u32>>,
}

const UNSEEN: i8 = 2;

/// One reading of a film's pixels: the most whole counts one pixel has in a second (`most`) and the most events
/// (`turns`; two of the law's make one count), the pixels over the bound, and the worst pixel (its index, its
/// events in its busiest second and their times).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PixelRead {
    pub most: usize,
    pub turns: usize,
    pub over: Vec<u32>,
    pub worst: Option<(u32, usize, Vec<u32>)>,
}

/// The most of `times` (sorted, ms) in any [`SECOND_MS`], and the index that second starts at.
#[must_use]
pub fn most_in_a_second(times: &[u32]) -> (usize, usize) {
    most_within(times, SECOND_MS)
}

/// The most of `times` (sorted, ms) in any `window` ms (half open: an event `window` after the first is out),
/// and the index that window starts at.
#[must_use]
pub fn most_within(times: &[u32], window: u32) -> (usize, usize) {
    let mut best = (0, 0);
    for (i, &t) in times.iter().enumerate() {
        let n = times[i..].iter().take_while(|&&u| u < t + window).count();
        if n > best.0 {
            best = (n, i);
        }
    }
    best
}

/// `events` read per pixel; `per` of them make one count (two turns make a flash).
///
/// A pixel is over the bound when more than `per` times [`FLASH_MOST`] of its events fall in one second: its
/// counts are then faster than 3 a second, though the whole ones among them are still 3 (seven turns are three
/// flashes and the fourth begun, a steady 3.1 to 3.9 Hz). The second is cut short by `step_ms`, the film's frame
/// step, for the law's turns: a turn is seen on the frame after it happened, so two turns a second apart can be
/// read up to a step closer, and a light at the bound itself (3.0 Hz: a turn every 166.7 ms) shows seven turns
/// in 992 ms of 16 ms frames. Cut by a step, that light keeps the law at every phase and one at 3.1 Hz breaks it.
fn pixel_read(events: &HashMap<u32, Vec<u32>>, per: usize, step_ms: u32) -> PixelRead {
    let mut out = PixelRead::default();
    for (&px, times) in events {
        let (n, from) = most_in_a_second(times);
        if over_bound(times, per, step_ms) {
            out.over.push(px);
        }
        let better = out
            .worst
            .as_ref()
            .is_none_or(|w| (n, std::cmp::Reverse(px)) > (w.1, std::cmp::Reverse(w.0)));
        if n >= per && better {
            out.worst = Some((px, n, times[from..from + n].to_vec()));
        }
        out.most = out.most.max(n / per);
        out.turns = out.turns.max(n);
    }
    out.over.sort_unstable();
    out
}

impl Turns {
    /// `px` pixels, a turn a change of `swing` or more whose darker state is under `dark_below`.
    #[must_use]
    pub fn new(px: usize, swing: f64, dark_below: f64) -> Self {
        Self {
            swing,
            dark_below,
            dir: vec![UNSEEN; px],
            lo: vec![0.0; px],
            hi: vec![0.0; px],
            at: HashMap::new(),
        }
    }

    /// Pixel `i` reads `v` at `ms`.
    pub fn see(&mut self, i: usize, v: f64, ms: u32) {
        let (lo, hi) = (self.lo[i], self.hi[i]);
        let turned = match self.dir[i] {
            UNSEEN => {
                (self.lo[i], self.hi[i], self.dir[i]) = (v, v, 0);
                0
            }
            0 => {
                (self.lo[i], self.hi[i]) = (lo.min(v), hi.max(v));
                if self.hi[i] - self.lo[i] >= self.swing && self.lo[i] < self.dark_below {
                    if v > lo { 1 } else { -1 }
                } else {
                    0
                }
            }
            1 if v > hi => {
                self.hi[i] = v;
                0
            }
            1 if hi - v >= self.swing && v < self.dark_below => -1,
            -1 if v < lo => {
                self.lo[i] = v;
                0
            }
            -1 if v - lo >= self.swing && lo < self.dark_below => 1,
            _ => 0,
        };
        if turned != 0 {
            (self.dir[i], self.lo[i], self.hi[i]) = (turned, v, v);
            self.at
                .entry(u32::try_from(i).unwrap_or(u32::MAX))
                .or_default()
                .push(ms);
        }
    }

    /// The film's flashes: two opposing turns make one. `step_ms` is the film's frame step ([`pixel_read`]).
    #[must_use]
    pub fn read(&self, step_ms: u32) -> PixelRead {
        pixel_read(&self.at, 2, step_ms)
    }
}

/// The scratch probe's count: a pixel rises when it stands [`TWIN_RISE`] or more off its clean twin, and can rise
/// again once it has come back within [`TWIN_REARM`].
pub struct TwinRises {
    up: Vec<bool>,
    at: HashMap<u32, Vec<u32>>,
}

impl TwinRises {
    #[must_use]
    pub fn new(px: usize) -> Self {
        Self {
            up: vec![false; px],
            at: HashMap::new(),
        }
    }

    /// Pixel `i` stands `off` its twin at `ms`.
    pub fn see(&mut self, i: usize, off: f64, ms: u32) {
        if !self.up[i] && off >= TWIN_RISE {
            self.up[i] = true;
            self.at
                .entry(u32::try_from(i).unwrap_or(u32::MAX))
                .or_default()
                .push(ms);
        } else if self.up[i] && off < TWIN_REARM {
            self.up[i] = false;
        }
    }

    /// The film's rises, each one a count, in whole seconds as the scratch probe counted them.
    #[must_use]
    pub fn read(&self) -> PixelRead {
        pixel_read(&self.at, 1, 0)
    }
}

/// Where a pixel flashes: in the pet's drawn box, in the caret's cell, in a word icon's box, on the page itself (its
/// clean twin turned with it), or in none of them, which is the trail's own.
pub const PLACES: [&str; 5] = ["pet", "caret", "icon", "page", "trail"];
/// The page's own among [`PLACES`]: the program's output, not the effects'.
pub const PAGE: usize = 3;
/// The trail's own among [`PLACES`].
pub const TRAIL: usize = 4;

/// A frame's boxes: the pet's body and the flying head's, the caret's cell, and the word icons on glass.
pub type Boxes = (Vec<Span>, Option<PxBox>, Vec<Span>);

/// `turns` (each pixel's events, ms; `per` of them make one count, read on frames `step_ms` apart as
/// [`pixel_read`] reads them) apart by [`PLACES`].
///
/// A place is a box, not what drew the pixel. Of the events of a pixel's busiest second, some fell on frames
/// where it lay in a pet box (`boxes`, by the frame's time), some where it lay in the caret's cell and some in a
/// word icon's box. The pixel is the trail's own only when those together are fewer than half: a pixel the pet
/// crossed on three turns and the caret on two of seven is not the trail's, though neither box alone held half.
/// Otherwise it is the place's that held the most (the earlier of [`PLACES`] on a tie). A box is a sprite's
/// bounds, so the trail under a pet that stands on it counts as the pet's while it stands there. A body's box
/// also holds the turns on the frame after it held the pixel (`step_ms` on): a sprite that moves off a pixel
/// turns it on the first frame its box no longer holds it, the cat's coat giving way to the page. The caret's
/// cell and an icon's box are held on their own frame alone.
///
/// A pixel no box holds is the page's own ([`the_page_s`]) over the bound only when the page alone (`page`, its
/// clean twin's turns by the same law) is over the bound in every second the pixel is and the effects' turns are
/// in none: a build log's text scrolling up the page flashes with no effect's help. Under it, when the page's turns take half its busiest
/// second's (a reading, not the law: a letter typed onto the page is not the trail's). The boxes are read
/// first: the clean twin holds the caret, so a caret's flash read as the page's would leave the law.
#[must_use]
pub fn by_place(
    turns: &HashMap<u32, Vec<u32>>,
    (per, step_ms): (usize, u32),
    width: usize,
    boxes: &HashMap<u32, Boxes>,
    page: &HashMap<u32, Vec<u32>>,
) -> [PixelRead; 5] {
    let mut parts: [HashMap<u32, Vec<u32>>; 5] = Default::default();
    for (&px, times) in turns {
        let (n, from) = most_in_a_second(times);
        let (x, y) = (
            (px as usize % width.max(1)) as i64,
            (px as usize / width.max(1)) as i64,
        );
        let within = |spans: &[Span]| {
            spans
                .iter()
                .any(|&(x0, x1, y0, y1)| x0 <= x && x < x1 && y0 <= y && y < y1)
        };
        let mut held = [0usize; PAGE];
        for t in &times[from..from + n] {
            let Some((bodies, cell, icons)) = boxes.get(t) else {
                continue;
            };
            let left = t
                .checked_sub(step_ms)
                .and_then(|was| boxes.get(&was))
                .is_some_and(|(before, _, _)| within(before));
            if within(bodies) || left {
                held[0] += 1;
            } else if cell.is_some_and(|c| c.holds(x, y)) {
                held[1] += 1;
            } else if within(icons) {
                held[2] += 1;
            }
        }
        let boxed: usize = held.iter().sum();
        let place = if 2 * boxed >= n {
            // The first of the most: `max_by_key` keeps the last, so the places are read from the end.
            (0..PAGE).rev().max_by_key(|&p| held[p]).unwrap_or(TRAIL)
        } else if page
            .get(&px)
            .is_some_and(|paged| the_page_s(times, paged, (per, step_ms)))
        {
            PAGE
        } else {
            TRAIL
        };
        parts[place].insert(px, times.clone());
    }
    parts.map(|p| pixel_read(&p, per, step_ms))
}

/// Whether a pixel's turns (`times`) are the page's own (`paged`, its clean twin's turns). Over the bound, only when
/// the turns the page's take ([`taken`], each the one turn within a frame step it matches) are over the bound in
/// every second the pixel is ([`over_wherever`]), and what is left of `times` is over it in none: a page under the
/// bound is no excuse for effects that carry it over, not even where the page is over the bound at some other
/// moment, and one turn of the page excuses one turn, not the three a frame each side of it can hold. Under the
/// bound the place is a reading, not the law: the page's when it matches half the turns of the pixel's busiest
/// second or more, so a letter typed onto the page is not read as the trail's.
#[must_use]
fn the_page_s(times: &[u32], paged: &[u32], (per, step_ms): (usize, u32)) -> bool {
    if over_bound(times, per, step_ms) {
        let (mut page, mut left) = (Vec::new(), Vec::new());
        for (&t, page_s) in times.iter().zip(taken(times, paged, step_ms)) {
            if page_s { page.push(t) } else { left.push(t) }
        }
        return over_wherever(times, &page, per, step_ms) && !over_bound(&left, per, step_ms);
    }
    let (n, from) = most_in_a_second(times);
    let left = unmatched(&times[from..from + n], paged, step_ms).len();
    2 * (n - left) >= n
}

/// Whether `part` (sorted, ms; some of `times`, sorted) is over the bound in every second `times` is, the seconds
/// cut short by a frame step as [`over_bound`] cuts them.
#[must_use]
fn over_wherever(times: &[u32], part: &[u32], per: usize, step_ms: u32) -> bool {
    let window = SECOND_MS.saturating_sub(step_ms).max(1);
    let most = per * FLASH_MOST;
    let within = |v: &[u32], from: u32| {
        v.partition_point(|&t| t < from.saturating_add(window)) - v.partition_point(|&t| t < from)
    };
    // What a second holds changes only where a turn leaves it or comes into it: read every second that starts
    // just after a turn or that ends just after one, and the one at each turn.
    times
        .iter()
        .flat_map(|&t| [t, t + 1, (t + 1).saturating_sub(window)])
        .all(|from| within(times, from) <= most || within(part, from) > most)
}

/// `times` (sorted, ms) less those `paged` (sorted) matches one for one ([`taken`]).
#[must_use]
fn unmatched(times: &[u32], paged: &[u32], step_ms: u32) -> Vec<u32> {
    times
        .iter()
        .zip(taken(times, paged, step_ms))
        .filter_map(|(&t, page_s)| (!page_s).then_some(t))
        .collect()
}

/// Which of `times` (sorted, ms) `paged` (sorted) takes, one for one: each of `times` in turn takes the earliest of
/// `paged` not yet taken within `step_ms` of it. The windows are all one width, so taking the earliest leaves the
/// most for the rest.
#[must_use]
fn taken(times: &[u32], paged: &[u32], step_ms: u32) -> Vec<bool> {
    let mut j = 0;
    times
        .iter()
        .map(|&t| {
            while j < paged.len() && paged[j].saturating_add(step_ms) < t {
                j += 1;
            }
            let page_s = j < paged.len() && paged[j] <= t.saturating_add(step_ms);
            j += usize::from(page_s);
            page_s
        })
        .collect()
}

/// Whether `events` (sorted, ms; `per` make one count) are over the bound, as [`pixel_read`] reads one pixel's.
fn over_bound(events: &[u32], per: usize, step_ms: u32) -> bool {
    most_within(events, SECOND_MS.saturating_sub(step_ms).max(1)).0 > per * FLASH_MOST
}

/// CIE L* of a relative luminance, as a share of its range.
#[must_use]
pub fn lightness(y: f64) -> f64 {
    if y > 216.0 / 24_389.0 {
        1.16 * y.cbrt() - 0.16
    } else {
        y * 24_389.0 / 2_700.0
    }
}

/// A film's pixels over time: the law's flashes, the lightness reading and the scratch probe's, and the law's
/// turns of the page itself (the clean twins, [`Flicker::page`]).
pub struct Flicker {
    width: usize,
    height: usize,
    last: Option<Image>,
    last_page: Option<Image>,
    pub law: Turns,
    pub light: Turns,
    pub twin: TwinRises,
    pub page: Turns,
}

impl Flicker {
    /// A film of `width` x `height` frames.
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        let px = width * height;
        Self {
            width,
            height,
            last: None,
            last_page: None,
            law: Turns::new(px, FLASH_SWING, FLASH_DARK_BELOW),
            light: Turns::new(px, LIGHTNESS_SWING, f64::INFINITY),
            twin: TwinRises::new(px),
            page: Turns::new(px, FLASH_SWING, FLASH_DARK_BELOW),
        }
    }

    /// The page itself at `ms`: a frame's clean twin, read by the law where it changed since the last one.
    /// # Errors
    /// A twin that is not the film's size.
    pub fn page(&mut self, ms: u64, clean: &Image) -> Result<(), String> {
        if (clean.width, clean.height) != (self.width, self.height) {
            return Err(format!(
                "the clean twin at {ms} ms is not the film's {}x{}",
                self.width, self.height
            ));
        }
        let ms = u32::try_from(ms).unwrap_or(u32::MAX);
        for y in 0..self.height {
            for x in 0..self.width {
                let p = clean.at(x, y);
                if self.last_page.as_ref().is_some_and(|l| l.at(x, y) == p) {
                    continue;
                }
                self.page.see(y * self.width + x, luminance(p), ms);
            }
        }
        self.last_page = Some(clean.clone());
        Ok(())
    }

    /// The film's next frame, at `ms`. With its clean twin, the grid and the caret's cell, the scratch probe's
    /// count reads it too.
    /// # Errors
    /// A frame or a twin that is not the film's size.
    pub fn frame(
        &mut self,
        ms: u64,
        frame: &Image,
        twin: Option<(&Image, &Grid, (i64, i64))>,
    ) -> Result<(), String> {
        let fits = |i: &Image| (i.width, i.height) == (self.width, self.height);
        if !fits(frame) || twin.is_some_and(|(clean, ..)| !fits(clean)) {
            return Err(format!(
                "the frame at {ms} ms is not the film's {}x{}",
                self.width, self.height
            ));
        }
        let ms = u32::try_from(ms).unwrap_or(u32::MAX);
        for y in 0..self.height {
            for x in 0..self.width {
                let p = frame.at(x, y);
                // A pixel that kept its colour turned nothing.
                if self.last.as_ref().is_some_and(|l| l.at(x, y) == p) {
                    continue;
                }
                let lum = luminance(p);
                let i = y * self.width + x;
                self.law.see(i, lum, ms);
                self.light.see(i, lightness(lum), ms);
            }
        }
        if let Some((clean, grid, (row, col))) = twin {
            let clip = |v: i64, hi: usize| usize::try_from(v.max(0)).unwrap_or(0).min(hi);
            for r in (row - TWIN_ROWS).max(0)..=(row + TWIN_ROWS).min(grid.rows - 1) {
                for c in (0..grid.cols).filter(|&c| (r, c) != (row, col)) {
                    let b = grid.cell_box(r, c, 1);
                    for y in clip(b.y0, self.height)..clip(b.y1, self.height) {
                        for x in clip(b.x0, self.width)..clip(b.x1, self.width) {
                            let (p, q) = (frame.at(x, y), clean.at(x, y));
                            let off = if p == q {
                                0.0
                            } else {
                                (luminance(p) - luminance(q)).abs()
                            };
                            self.twin.see(y * self.width + x, off, ms);
                        }
                    }
                }
            }
        }
        self.last = Some(frame.clone());
        Ok(())
    }
}

/// A low reading: its ratio, its frame's time, and the cell it was read in.
#[derive(Clone, Debug, PartialEq)]
pub struct Low {
    pub ratio: f64,
    pub ms: u64,
    pub row: i64,
    pub col: i64,
    pub ch: String,
}

/// A film's letters and caret, from `trail-contrast`'s report of each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContrastRead {
    /// The lowest letter of the film.
    pub floor: Option<Low>,
    /// The readings the effects took under [`LEGIBLE`], the frames that hold one, and the lowest.
    pub lowered: usize,
    pub lowered_frames: usize,
    pub lowered_floor: Option<Low>,
    /// The caret's lowest reading and its frame's time.
    pub caret_floor: Option<(f64, u64)>,
    /// The frames whose caret the report could not read (hidden, or no pixel of it off the page).
    pub caret_unread: usize,
}

impl ContrastRead {
    /// The frame at `ms`, by its `trail-contrast` report.
    pub fn note(&mut self, ms: u64, report: &J) {
        let mut lowered = 0;
        for cell in report.get("cells").and_then(J::as_arr).unwrap_or(&[]) {
            let Some(ratio) = judged(cell) else {
                continue;
            };
            let low = || Low {
                ratio,
                ms,
                row: cell.get("row").and_then(J::as_i64).unwrap_or(-1),
                col: cell.get("col").and_then(J::as_i64).unwrap_or(-1),
                ch: cell
                    .get("char")
                    .and_then(J::as_str)
                    .unwrap_or_default()
                    .to_string(),
            };
            if self.floor.as_ref().is_none_or(|f| ratio < f.ratio) {
                self.floor = Some(low());
            }
            if lowered_under(cell, LEGIBLE) {
                lowered += 1;
                if self.lowered_floor.as_ref().is_none_or(|f| ratio < f.ratio) {
                    self.lowered_floor = Some(low());
                }
            }
        }
        self.lowered += lowered;
        self.lowered_frames += usize::from(lowered > 0);
        match report
            .get("caret")
            .and_then(|c| c.get("contrast"))
            .and_then(J::as_f64)
        {
            Some(ratio) if self.caret_floor.is_none_or(|(f, _)| ratio < f) => {
                self.caret_floor = Some((ratio, ms));
            }
            Some(_) => {}
            None => self.caret_unread += 1,
        }
    }
}

/// Whether a line's pixel is vivid ([`LINE_CHROMA`], [`LINE_LUMA`]).
#[must_use]
pub fn vivid(p: Rgb) -> bool {
    let chroma = p
        .iter()
        .max()
        .map_or(0, |hi| hi - p.iter().min().unwrap_or(hi));
    let y = luminance(p);
    chroma >= LINE_CHROMA && (LINE_LUMA.0..=LINE_LUMA.1).contains(&y)
}

/// Whether the cell `(row, col)` of `frame` has a vivid line under it that `clean`, its twin with no effects,
/// has not ([`LINE_THICK`], [`LINE_BAND_FROM`]).
#[must_use]
pub fn cell_lined(frame: &Image, clean: &Image, grid: &Grid, (row, col): (i64, i64)) -> bool {
    let b = grid.cell_box(row, col, 1);
    let clip = |v: i64, hi: usize| usize::try_from(v.max(0)).unwrap_or(0).min(hi);
    let ch = grid.ch as f64;
    let from = b.y0 + (LINE_BAND_FROM * ch).floor() as i64;
    let to = b.y1 + (LINE_BAND_UNDER * ch).ceil() as i64;
    let (y0, y1) = (clip(from, frame.height), clip(to, frame.height));
    let (x0, x1) = (clip(b.x0, frame.width), clip(b.x1, frame.width));
    let lined = (x0..x1)
        .filter(|&x| {
            let mut run = 0;
            (y0..y1).any(|y| {
                let p = frame.at(x, y);
                run = if p != clean.at(x, y) && vivid(p) {
                    run + 1
                } else {
                    0
                };
                run >= LINE_THICK
            })
        })
        .count();
    x1 > x0 && 2 * lined >= x1 - x0
}

/// Whether the cell `(row, col)` of `clean`, a frame with no effects, has ink of its own under the baseline
/// ([`LINE_FOOT_FROM`]).
#[must_use]
pub fn cell_footed(clean: &Image, grid: &Grid, (row, col): (i64, i64)) -> bool {
    let b = grid.cell_box(row, col, 1);
    let Some(ground) = crate::trail_contrast::mode(clean.region(b)) else {
        return false;
    };
    let foot = PxBox {
        y0: b.y0 + (LINE_FOOT_FROM * grid.ch as f64).floor() as i64,
        ..b
    };
    clean.region(foot).iter().any(|&p| p != ground)
}

/// A film's line: of the cells typed along the caret's row, the share with a vivid line under them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineRead {
    /// The frames judged.
    pub frames: usize,
    /// The frame that judged the most cells, the least of them by share: its lit cells, its judged cells and
    /// its time.
    pub full: Option<(usize, usize, u64)>,
    /// That frame's cells with no ink of their own under the baseline ([`cell_footed`]): the lit and all.
    pub free: (usize, usize),
    /// The frame with the least share.
    pub least: Option<(usize, usize, u64)>,
    /// Every judged frame's lit cells and judged cells, summed.
    pub lit: usize,
    pub cells: usize,
}

impl LineRead {
    /// The share of the frame that judged the most cells, of 1.
    #[must_use]
    pub fn share(&self) -> Option<f64> {
        self.full.map(|(lit, cells, _)| lit as f64 / cells as f64)
    }

    /// The least share of any judged frame, of 1.
    #[must_use]
    pub fn least_share(&self) -> Option<f64> {
        self.least.map(|(lit, cells, _)| lit as f64 / cells as f64)
    }
}

/// A film's typing, followed frame by frame for [`LineRead`].
#[derive(Clone, Debug, Default)]
pub struct LineTrack {
    /// The caret's cell on the last frame.
    caret: Option<(i64, i64)>,
    /// The run's cells on the caret's row, each with the time the caret left it.
    typed: Vec<(i64, u64)>,
    pub read: LineRead,
}

impl LineTrack {
    /// The film's next frame, at `ms`, its clean twin and its caret's cell.
    ///
    /// A step of the caret one or two columns on along its row types the cells it leaves (two: a wide glyph);
    /// any other move of it starts the run again, so a Home, a Backspace, an Enter and a click judge nothing.
    pub fn frame(&mut self, ms: u64, frame: &Image, clean: &Image, grid: &Grid, caret: (i64, i64)) {
        match self.caret {
            Some(last) if last == caret => {}
            Some((row, col)) if row == caret.0 && (1..=2).contains(&(caret.1 - col)) => {
                self.typed.extend((col..caret.1).map(|c| (c, ms)));
            }
            _ => self.typed.clear(),
        }
        self.caret = Some(caret);
        let Some(&(_, newest)) = self.typed.last() else {
            return;
        };
        let settled: Vec<i64> = self
            .typed
            .iter()
            .filter(|&&(_, at)| at + LINE_SETTLE_MS <= ms)
            .map(|&(c, _)| c)
            .collect();
        if settled.len() < LINE_CELLS || ms > newest + LINE_LIVE_MS {
            return;
        }
        let lined: Vec<i64> = settled
            .iter()
            .copied()
            .filter(|&c| cell_lined(frame, clean, grid, (caret.0, c)))
            .collect();
        let (lit, cells) = (lined.len(), settled.len());
        let r = &mut self.read;
        r.frames += 1;
        r.lit += lit;
        r.cells += cells;
        // The lesser share, by cross product.
        if r.least.is_none_or(|(l, n, _)| lit * n < l * cells) {
            r.least = Some((lit, cells, ms));
        }
        if r.full
            .is_none_or(|(l, n, _)| cells > n || (cells == n && lit < l))
        {
            r.full = Some((lit, cells, ms));
            let free = |cols: &[i64]| {
                cols.iter()
                    .filter(|&&c| !cell_footed(clean, grid, (caret.0, c)))
                    .count()
            };
            r.free = (free(&lined), free(&settled));
        }
    }
}

/// One frame's pet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PetShot {
    pub ms: u64,
    /// The alpha the frame put the pet on glass at (`0`: none drawn).
    pub alpha: u8,
    /// The pose the frame drew.
    pub pose: String,
    /// Whether the pet's drawn body meets the grid's left or right edge.
    pub at_edge: bool,
    /// The lettered cells its body or the flying head meets, and whether either meets the caret's cell or a
    /// word icon.
    pub letters: usize,
    pub on_caret: bool,
    pub on_icons: bool,
    /// Whether a body's letters or caret could not be read.
    pub unread: bool,
}

/// A film's pet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PetRead {
    /// The frames with the pet on glass.
    pub on_glass: usize,
    /// The frames whose body or head meets a lettered cell, the caret's cell, a word icon; the first such frame.
    pub on_letters: usize,
    pub on_caret: usize,
    pub on_icons: usize,
    pub first_over: Option<u64>,
    /// Each blink: when the alpha began to fall, when it had risen again, and the least it reached.
    pub blinks: Vec<(u64, u64, u8)>,
    /// The shortest pose: its time on glass, its name and when it began; and the poses under [`POSE_MS`].
    pub min_dwell: Option<(u64, String, u64)>,
    pub short_poses: usize,
    pub unread: usize,
}

/// The pet over a film's `shots`, in time order.
///
/// A blink is a fall of the alpha by [`ALPHA_DIP`] or more from its last peak and a rise by as much from the
/// trough: the fade in at a film's start and a fade out with no return are neither. A fall to nothing and back is
/// a walk off the glass and on again, not a blink, when the body met the grid's edge both as it left and as it
/// came back. A pose's time runs from its first frame to the first frame that draws another (or no pet); the
/// film's first and last frames cut their poses short, so those are not timed.
#[must_use]
pub fn judge_pet(shots: &[PetShot]) -> PetRead {
    let mut out = PetRead::default();
    for s in shots {
        out.on_glass += usize::from(s.alpha > 0);
        out.on_letters += usize::from(s.letters > 0);
        out.on_caret += usize::from(s.on_caret);
        out.on_icons += usize::from(s.on_icons);
        out.unread += usize::from(s.unread);
        if (s.letters > 0 || s.on_caret) && out.first_over.is_none() {
            out.first_over = Some(s.ms);
        }
    }
    // The alpha's zigzag: `peak` the last high and its time, `trough` the low under it once it has dipped;
    // `left_by_edge` whether the last frame that drew the pet before it was gone drew it at the grid's edge.
    let mut peak: Option<(u8, u64)> = None;
    let mut trough: Option<u8> = None;
    let mut left_by_edge = false;
    let mut before: Option<&PetShot> = None;
    for s in shots {
        if let Some(b) = before.filter(|b| b.alpha > 0 && s.alpha == 0) {
            left_by_edge = b.at_edge;
        }
        match (peak, trough) {
            (None, _) => peak = Some((s.alpha, s.ms)),
            (Some((p, _)), None) => {
                if s.alpha >= p {
                    peak = Some((s.alpha, s.ms));
                } else if p - s.alpha >= ALPHA_DIP {
                    trough = Some(s.alpha);
                }
            }
            (Some((_, from)), Some(t)) => {
                if s.alpha <= t {
                    trough = Some(s.alpha);
                } else if s.alpha - t >= ALPHA_DIP {
                    if !(t == 0 && left_by_edge && s.at_edge) {
                        out.blinks.push((from, s.ms, t));
                    }
                    peak = Some((s.alpha, s.ms));
                    trough = None;
                }
            }
        }
        before = Some(s);
    }
    let mut i = 0;
    while i < shots.len() {
        let mut j = i + 1;
        while j < shots.len()
            && shots[j].pose == shots[i].pose
            && (shots[j].alpha > 0) == (shots[i].alpha > 0)
        {
            j += 1;
        }
        if shots[i].alpha > 0 && i > 0 && j < shots.len() {
            let dwell = shots[j].ms - shots[i].ms;
            out.short_poses += usize::from(dwell < POSE_MS);
            if out.min_dwell.as_ref().is_none_or(|m| dwell < m.0) {
                out.min_dwell = Some((dwell, shots[i].pose.clone(), shots[i].ms));
            }
        }
        i = j;
    }
    out
}

/// The flash line's layer names: `trail-census`'s, by the words a trail's parts go by.
const fn layer_word(layer: Layer) -> &'static str {
    match layer {
        Layer::None => "none",
        Layer::Caret => "caret",
        Layer::Pet => "pet",
        Layer::Star => "star",
        Layer::Band => "ribbon",
        Layer::RailLip => "rail",
        Layer::Other => "other",
    }
}

/// One film, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub frames: usize,
    pub last_ms: u64,
    /// The film's frame step, ms: the least time between two of its frames.
    pub step_ms: u32,
    pub width: usize,
    pub grid: Option<Grid>,
    pub flash: PixelRead,
    pub light: PixelRead,
    pub twin: PixelRead,
    /// The law's flashes apart by [`PLACES`] ([`by_place`]), and the scratch probe's rises.
    pub places: [PixelRead; 5],
    pub twin_places: [PixelRead; 5],
    /// The layers `trail-census` sorts the worst pixel into on the frames it turned on, most frames first; and
    /// the worst pixel of the trail's own.
    pub layers: Layers,
    pub trail_layers: Layers,
    /// Whether rainbow kitty v2 had a meteor live on one of those frames.
    pub meteor_live: bool,
    pub contrast: ContrastRead,
    pub line: LineRead,
    pub pet: PetRead,
    /// The planted star's reading, where one was planted ([`plant_star`]).
    pub star: Option<StarRead>,
}

impl Reading {
    /// A pixel index as `(x, y)` and its cell `(row, col)` (`-1` off the grid).
    fn place(&self, px: u32) -> ((usize, usize), (i64, i64)) {
        let (x, y) = (
            px as usize % self.width.max(1),
            px as usize / self.width.max(1),
        );
        let cell = self.grid.map_or((-1, -1), |g| {
            let (dx, dy) = (x as i64 - g.ox, y as i64 - g.oy);
            if dx < 0 || dy < 0 || dx >= g.cols * g.cw || dy >= g.rows * g.ch {
                (-1, -1)
            } else {
                (dy / g.ch, dx / g.cw)
            }
        });
        ((x, y), cell)
    }

    /// The cells that hold one of `over`'s pixels.
    fn cells(&self, over: &[u32]) -> usize {
        let mut cells: Vec<(i64, i64)> = over.iter().map(|&p| self.place(p).1).collect();
        cells.sort_unstable();
        cells.dedup();
        cells.len()
    }

    /// The laws the film breaks, one line each.
    #[must_use]
    pub fn broken(&self) -> Vec<String> {
        let mut out = Vec::new();
        // The page's own flashes are the program's (a build log scrolling), not the effects'.
        let effects = self.flash.over.len() - self.places[PAGE].over.len();
        if effects > 0 {
            let worst = if self.places[PAGE].over.is_empty() {
                self.flash.turns
            } else {
                (0..PLACES.len())
                    .filter(|&p| p != PAGE)
                    .map(|p| self.places[p].turns)
                    .max()
                    .unwrap_or(0)
            };
            out.push(format!(
                "flash: {effects} px flash faster than {FLASH_MOST} a second (over {} turns in one), the worst {worst} turns",
                2 * FLASH_MOST,
            ));
        }
        let c = &self.contrast;
        if c.lowered > 0 {
            let worst = c.lowered_floor.as_ref().map_or(0.0, |l| l.ratio);
            out.push(format!(
                "contrast: the effects took {} letter readings in {} frames under {LEGIBLE}:1, the lowest {worst:.2}",
                c.lowered, c.lowered_frames
            ));
        }
        if let Some((ratio, ms)) = c.caret_floor.filter(|&(r, _)| r < CARET_BAR) {
            out.push(format!(
                "contrast: the caret reads {ratio:.2}:1 at {ms} ms, under {CARET_BAR}:1"
            ));
        }
        let p = &self.pet;
        if p.on_letters > 0 || p.on_caret > 0 {
            out.push(format!(
                "pet: on a lettered cell in {} frames and on the caret's cell in {}, first at {} ms",
                p.on_letters,
                p.on_caret,
                p.first_over.unwrap_or(0)
            ));
        }
        if let Some(&(from, to, least)) = p.blinks.first() {
            out.push(format!(
                "pet: its alpha dipped and came back {} times, first from {from} ms to {to} ms (down to {least})",
                p.blinks.len()
            ));
        }
        if p.short_poses > 0 {
            let (dwell, pose, at) = p.min_dwell.clone().unwrap_or_default();
            out.push(format!(
                "pet: {} poses shown under {POSE_MS} ms, the shortest {pose} for {dwell} ms at {at} ms",
                p.short_poses
            ));
        }
        if p.unread > 0 {
            out.push(format!(
                "pet: {} frames whose letters or caret were not read",
                p.unread
            ));
        }
        if let Some(star) = self
            .star
            .filter(|s| s.over < (STAR_SIDE * STAR_SIDE) as usize)
        {
            out.push(format!(
                "star: the planted star flashed in {} of its {} px: the flash law missed a flash it was given",
                star.over,
                STAR_SIDE * STAR_SIDE
            ));
        }
        out
    }

    /// The film's report: its FLASH, CONTRAST, LINE and PET lines.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        let f = &self.flash;
        let _ = write!(
            s,
            "  FLASH    flash_most={} flash_turns_most={} flash_px_over3={} flash_cells_over3={}",
            f.most,
            f.turns,
            f.over.len(),
            self.cells(&f.over)
        );
        let worst = |read: &PixelRead, layers: &[(&'static str, usize)]| {
            let Some((px, _, times)) = &read.worst else {
                return "none".to_string();
            };
            let ((x, y), (row, col)) = self.place(*px);
            let times: Vec<String> = times.iter().map(u32::to_string).collect();
            let layers: Vec<String> = layers
                .iter()
                .map(|(name, n)| format!("{name}:{n}"))
                .collect();
            format!(
                "px:{x},{y};cell:{row},{col};turns_ms:{};layer:{}",
                times.join(","),
                if layers.is_empty() {
                    "unread".to_string()
                } else {
                    layers.join(",")
                }
            )
        };
        let _ = write!(s, " worst={}", worst(f, &self.layers));
        if self.meteor_live {
            s.push_str(" meteor_live=true");
        }
        s.push_str("\n          ");
        for (name, read) in PLACES.iter().zip(&self.places) {
            let _ = write!(
                s,
                " {name}_most={} {name}_turns_most={} {name}_px_over3={}",
                read.most,
                read.turns,
                read.over.len()
            );
        }
        let _ = write!(
            s,
            " trail_worst={}",
            worst(&self.places[TRAIL], &self.trail_layers)
        );
        let _ = write!(
            s,
            "\n           lightness_most={} lightness_px_over3={} twin_most={} twin_px_over3={} twin_cells_over3={} \
             twin_pet_px_over3={} twin_trail_most={} twin_trail_px_over3={}",
            self.light.most,
            self.light.over.len(),
            self.twin.most,
            self.twin.over.len(),
            self.cells(&self.twin.over),
            self.twin_places[0].over.len(),
            self.twin_places[TRAIL].most,
            self.twin_places[TRAIL].over.len()
        );
        let c = &self.contrast;
        let low = |l: &Option<Low>| {
            l.as_ref().map_or_else(
                || "none".to_string(),
                |l| {
                    format!(
                        "{:.2};ms:{};cell:{},{};char:{}",
                        l.ratio, l.ms, l.row, l.col, l.ch
                    )
                },
            )
        };
        let _ = write!(
            s,
            "\n  CONTRAST contrast_floor={} lowered={} lowered_frames={} lowered_floor={} caret_floor={} caret_unread={}",
            low(&c.floor),
            c.lowered,
            c.lowered_frames,
            low(&c.lowered_floor),
            c.caret_floor
                .map_or_else(|| "none".to_string(), |(r, ms)| format!("{r:.2};ms:{ms}")),
            c.caret_unread
        );
        let l = &self.line;
        let pct =
            |lit: usize, cells: usize| format!("{:.1}%", 100.0 * lit as f64 / cells.max(1) as f64);
        let at = |f: Option<(usize, usize, u64)>| {
            f.map_or_else(
                || "none".to_string(),
                |(lit, cells, ms)| format!("lit:{lit};cells:{cells};ms:{ms}"),
            )
        };
        let _ = write!(
            s,
            "\n  LINE     line_cells_lit={} line_at={} line_free_lit={} line_least={} line_mean={} line_frames={}",
            l.share().map_or_else(
                || "none".to_string(),
                |share| format!("{:.1}%", 100.0 * share)
            ),
            at(l.full),
            if l.full.is_none() {
                "none".to_string()
            } else {
                format!(
                    "{};lit:{};cells:{}",
                    pct(l.free.0, l.free.1),
                    l.free.0,
                    l.free.1
                )
            },
            l.least_share().map_or_else(
                || "none".to_string(),
                |share| format!("{:.1}%;{}", 100.0 * share, at(l.least))
            ),
            if l.frames == 0 {
                "none".to_string()
            } else {
                pct(l.lit, l.cells)
            },
            l.frames
        );
        let p = &self.pet;
        let _ = write!(
            s,
            "\n  PET      on_glass={} companion_on_letters={} companion_on_caret={} on_icons={} alpha_dip={} \
             min_pose_dwell_ms={} poses_under_100ms={} unread={}",
            p.on_glass,
            p.on_letters,
            p.on_caret,
            p.on_icons,
            p.blinks.len(),
            p.min_dwell.as_ref().map_or_else(
                || "none".to_string(),
                |(d, pose, at)| format!("{d};pose:{pose};ms:{at}")
            ),
            p.short_poses,
            p.unread
        );
        if let Some(star) = self.star {
            let _ = write!(
                s,
                "\n  STAR     planted_at={},{} star_px_over3={} of {}",
                star.at.x0,
                star.at.y0,
                star.over,
                STAR_SIDE * STAR_SIDE
            );
        }
        s
    }
}

/// A film frame's time, from its name (`film.<ms>.png`).
fn frame_ms(png: &Path) -> Option<u64> {
    png.file_name()?
        .to_str()?
        .strip_prefix("film.")?
        .strip_suffix(".png")?
        .parse()
        .ok()
}

/// The frame at `png` as its sidecars name its pet.
/// With it, the frame's pet boxes (the body's and the flying head's) and its word icons' boxes.
fn pet_shot(png: &Path, ms: u64, grid: &Grid) -> Result<(PetShot, Vec<Span>, Vec<Span>), String> {
    let read =
        read_frame(png, SHAVE)?.ok_or_else(|| format!("{}: no status sidecar", png.display()))?;
    let status = sidecar(png, ".status.txt", true)
        .map(|p| read_text(&p))
        .transpose()?
        .unwrap_or_default();
    let alpha = match status_field(&status, "pet_alpha") {
        Some(a) => a
            .parse()
            .map_err(|_| format!("{}: pet_alpha={a} is not 0..=255", png.display()))?,
        // A gallery's status row has no alpha: a body named is a body drawn.
        None if read.body.is_some() => u8::MAX,
        None => 0,
    };
    let edges = (grid.ox, grid.ox + grid.cols * grid.cw);
    let met = |m: &crate::trail_companion::Meets| (m.letters.len(), m.caret_px > 0, m.icon_px > 0);
    let (body, head) = (met(&read.meets), met(&read.head_meets));
    let shot = PetShot {
        ms,
        alpha: if read.body.is_some() { alpha } else { 0 },
        pose: status_field(&status, "pet_pose")
            .unwrap_or_default()
            .to_string(),
        at_edge: read
            .body
            .is_some_and(|b: Span| b.0 <= edges.0 || b.1 >= edges.1),
        letters: body.0 + head.0,
        on_caret: body.1 || head.1,
        on_icons: body.2 || head.2,
        unread: !read.judged(),
    };
    let icons = crate::trail_companion::status_boxes(&status, "word_icons")?.unwrap_or_default();
    Ok((
        shot,
        read.body.into_iter().chain(read.head).collect(),
        icons,
    ))
}

/// A pixel's census layers by name, each with the frames it was sorted into it.
type Layers = Vec<(&'static str, usize)>;

/// The layers `trail-census` sorts the pixel `at` into on the frames of `times`, most frames first, and whether
/// a meteor was live on one.
fn layers_on(
    frames: &[(u64, PathBuf)],
    grid: Grid,
    at: (usize, usize),
    times: &[u32],
) -> Result<(Layers, bool), String> {
    let mut tally: Layers = Vec::new();
    let mut meteor_live = false;
    for &t in times {
        let Some((_, png)) = frames.iter().find(|(ms, _)| *ms == u64::from(t)) else {
            continue;
        };
        let (layer, meteor) = layer_of(png, grid, at)?;
        meteor_live |= meteor;
        let word = layer_word(layer);
        match tally.iter_mut().find(|(w, _)| *w == word) {
            Some((_, n)) => *n += 1,
            None => tally.push((word, 1)),
        }
    }
    tally.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    Ok((tally, meteor_live))
}

/// The layer `trail-census` sorts the pixel `(x, y)` of the frame at `png` into, and whether its status row names
/// a live meteor.
fn layer_of(png: &Path, grid: Grid, (x, y): (usize, usize)) -> Result<(Layer, bool), String> {
    let frame = Image::read(png)?;
    let twin = sidecar(png, ".clean.png", true)
        .ok_or_else(|| format!("{}: no clean twin", png.display()))?;
    let clean = clean_twin(&frame, &twin)?;
    let (caret, _) = caret_cell(None, sidecar(png, ".cursor.txt", true))?;
    let caret_box = caret
        .filter(|&(r, c)| grid.holds(r, c))
        .map(|(r, c)| grid.cell_box(r, c, 1));
    let status = sidecar(png, ".status.txt", true)
        .map(|p| read_text(&p))
        .transpose()?
        .unwrap_or_default();
    let mut pets = Vec::new();
    for key in ["pet_body", "head_body"] {
        if let Some((x0, x1, y0, y1)) = crate::trail_companion::status_box(&status, key)? {
            pets.push(PxBox { x0, y0, x1, y1 });
        }
    }
    let scene = Scene::new(&frame, &clean, grid, clean.most_common(), caret_box, &pets);
    let meteor = status_field(&status, "v2_meteors").is_some_and(|n| n != "0");
    Ok((scene.layer_at(x as i64, y as i64).0, meteor))
}

/// The film under `dir` (its `film.<ms>.png` frames and their sidecars), read.
/// # Errors
/// No frame there, or a frame or sidecar that cannot be read.
pub fn judge_film(dir: &Path, star: bool) -> Result<Reading, String> {
    let frames: Vec<(u64, PathBuf)> = frames_under(dir)?
        .into_iter()
        .filter_map(|p| frame_ms(&p).map(|ms| (ms, p)))
        .collect();
    let Some((_, first)) = frames.first() else {
        return Err(format!("{}: no film.<ms>.png frame", dir.display()));
    };
    let dims = frame_dims(first).ok_or_else(|| format!("{}: no film.dims.txt", dir.display()))?;
    let grid = grid_from_dims(&read_text(&dims)?)?;
    let mut out = Reading {
        frames: frames.len(),
        last_ms: frames.last().map_or(0, |f| f.0),
        step_ms: frames
            .windows(2)
            .map(|w| u32::try_from(w[1].0 - w[0].0).unwrap_or(u32::MAX))
            .min()
            .unwrap_or(0),
        grid: Some(grid),
        ..Reading::default()
    };
    let mut flicker: Option<Flicker> = None;
    let mut twin: Option<(u64, Image)> = None;
    let mut line = LineTrack::default();
    let mut shots = Vec::with_capacity(frames.len());
    let mut boxes: HashMap<u32, Boxes> = HashMap::new();
    for (ms, png) in &frames {
        let mut frame = Image::read(png)?;
        if star {
            plant_star(&mut frame, *ms);
        }
        // A film links a frame's twin to the last one written while the page is the same.
        let twin_path = sidecar(png, ".clean.png", true)
            .ok_or_else(|| format!("{}: no clean twin", png.display()))?;
        let inode = std::fs::metadata(&twin_path)
            .map_err(|e| format!("cannot read {}: {e}", twin_path.display()))?
            .ino();
        let new_page = twin.as_ref().is_none_or(|(i, _)| *i != inode);
        if new_page {
            twin = Some((inode, clean_twin(&frame, &twin_path)?));
        }
        let clean = twin.as_ref().map(|(_, c)| c);
        let (caret, _) = caret_cell(None, sidecar(png, ".cursor.txt", true))?;
        let flicker = flicker.get_or_insert_with(|| Flicker::new(frame.width, frame.height));
        if let Some(clean) = clean.filter(|_| new_page) {
            flicker.page(*ms, clean)?;
        }
        out.width = frame.width;
        flicker.frame(*ms, &frame, clean.zip(caret).map(|(c, at)| (c, &grid, at)))?;
        if let Some((clean, at)) = clean.zip(caret) {
            line.frame(*ms, &frame, clean, &grid, at);
        }
        let report = crate::trail_contrast::report(&crate::trail_contrast::Args {
            png: png.to_string_lossy().into_owned(),
            ..Default::default()
        })?;
        out.contrast.note(*ms, &report);
        let (shot, bodies, icons) = pet_shot(png, *ms, &grid)?;
        shots.push(shot);
        let cell = caret
            .filter(|&(r, c)| grid.holds(r, c))
            .map(|(r, c)| grid.cell_box(r, c, 1));
        boxes.insert(
            u32::try_from(*ms).unwrap_or(u32::MAX),
            (bodies, cell, icons),
        );
    }
    if let Some(flicker) = &flicker {
        out.flash = flicker.law.read(out.step_ms);
        out.places = by_place(
            &flicker.law.at,
            (2, out.step_ms),
            out.width,
            &boxes,
            &flicker.page.at,
        );
        // The scratch probe reads rises off the clean twin: the page's own turns are not in it.
        out.twin_places = by_place(&flicker.twin.at, (1, 0), out.width, &boxes, &HashMap::new());
        out.light = flicker.light.read(out.step_ms);
        out.twin = flicker.twin.read();
        if star {
            let at = star_box(flicker.width, flicker.height);
            let width = i64::try_from(flicker.width).unwrap_or(i64::MAX);
            let over = (at.y0..at.y1)
                .flat_map(|y| (at.x0..at.x1).map(move |x| y * width + x))
                .filter_map(|i| u32::try_from(i).ok())
                .filter(|i| out.flash.over.binary_search(i).is_ok())
                .count();
            out.star = Some(StarRead { at, over });
        }
    }
    out.line = line.read;
    out.pet = judge_pet(&shots);
    if let Some((px, _, times)) = out.flash.worst.clone() {
        let (layers, meteor) = layers_on(&frames, grid, out.place(px).0, &times)?;
        (out.layers, out.meteor_live) = (layers, meteor);
    }
    if let Some((px, _, times)) = out.places[TRAIL].worst.clone() {
        let (layers, meteor) = layers_on(&frames, grid, out.place(px).0, &times)?;
        out.trail_layers = layers;
        out.meteor_live |= meteor;
    }
    Ok(out)
}

/// What `trail-film` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub out: String,
    pub style: String,
    /// The `cursor_pet` beside it.
    pub pet: String,
    pub themes: Vec<String>,
    pub carets: Vec<String>,
    pub scenes: Vec<String>,
    pub replay: bool,
    pub report_only: bool,
    /// `--plant-star`: [`plant_star`] in every frame judged.
    pub plant_star: bool,
}

impl Args {
    /// The replay's word for the style and its pet (`aterm-render`'s `trail_letters_read::Spec`).
    #[must_use]
    pub fn word(&self) -> String {
        if self.pet == "off" {
            self.style.clone()
        } else {
            format!("{} + {}", self.style, self.pet)
        }
    }

    /// The run's folder under the output directory: `<style>-<pet>`.
    #[must_use]
    pub fn folder(&self) -> String {
        format!("{}-{}", style_slug(&self.style), self.pet)
    }
}

/// The comma list `value` of `known` names, each checked and kept once; `first` when none is given, and for the
/// word `both` where `both` says the flag takes it.
fn names(
    flag: &str,
    value: Option<String>,
    known: &[&str],
    first: &[&str],
    both: bool,
) -> Result<Vec<String>, Fail> {
    let Some(value) = value else {
        return Ok(first.iter().map(|n| (*n).to_string()).collect());
    };
    let mut out: Vec<String> = Vec::new();
    for word in value.split(',') {
        let each: Vec<&str> = if both && word == "both" {
            first.to_vec()
        } else {
            vec![word]
        };
        for name in each {
            if !known.contains(&name) {
                return Err(Fail::usage(format!(
                    "trail-film: {flag} takes {}, not {name:?}",
                    known.join(", ")
                )));
            }
            if !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    Ok(out)
}

/// The pets a film can keep: the walkers and `off`. The replay films the pipeline, which
/// keeps those; a flier's flight is the app's (`lab trail-gallery --extra
/// 'cursor_pet = "flying cat"'` photographs it).
fn filmed_pets() -> String {
    aterm_effects::cursor_pet::PETS
        .iter()
        .filter(|row| !row.flies)
        .map(|row| row.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// # Errors
/// An unknown flag or name, or no output directory.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut rest = args.to_vec();
    let pet = take_value(&mut rest, "--pet")?.unwrap_or_else(|| FLAGSHIP_PET.to_string());
    let Some(pet) = aterm_effects::cursor_pet::row(&pet)
        .filter(|row| !row.flies)
        .map(|row| row.name)
    else {
        return Err(Fail::usage(format!(
            "trail-film: --pet takes {}, not {pet:?}",
            filmed_pets()
        )));
    };
    let themes = names(
        "--theme",
        take_value(&mut rest, "--theme")?,
        &THEMES,
        &THEMES[..2],
        true,
    )?;
    let carets = names(
        "--caret",
        take_value(&mut rest, "--caret")?,
        &CARETS,
        &CARETS[..2],
        true,
    )?;
    let known: Vec<&str> = SCENES.iter().map(|s| s.0).collect();
    let all = take_flag(&mut rest, "--all-scenes");
    let scene = take_value(&mut rest, "--scene")?;
    if all && scene.is_some() {
        return Err(Fail::usage("trail-film: --scene or --all-scenes, not both"));
    }
    let scenes = if all {
        known.iter().map(|s| (*s).to_string()).collect()
    } else {
        names("--scene", scene, &known, &known[..DEFAULT_SCENES], false)?
    };
    let replay = !take_flag(&mut rest, "--no-replay");
    let report_only = take_flag(&mut rest, "--report-only");
    let plant_star = take_flag(&mut rest, "--plant-star");
    if let Some(flag) = rest.iter().find(|a| a.starts_with("--")) {
        return Err(Fail::usage(format!("trail-film: unknown flag {flag}")));
    }
    let mut rest = rest.into_iter();
    let out = rest
        .next()
        .ok_or_else(|| Fail::usage("trail-film: name the directory the films go in"))?;
    let style = rest.next().unwrap_or_else(|| FLAGSHIP.to_string());
    if aterm_effects::cursor_glow::spellings::row(&style).is_none() {
        let now = aterm_effects::cursor_glow::spellings::retired(&style).map_or_else(
            || {
                format!(
                    "the styles are {}",
                    aterm_effects::cursor_glow::spellings::TRAIL_STYLE_NAMES.join(", ")
                )
            },
            |r| match r.pet {
                Some(pet) => format!("that name is retired: {:?} --pet {pet}", r.style),
                None => format!("that name is retired: {:?}", r.style),
            },
        );
        return Err(Fail::usage(format!(
            "trail-film: {style:?} is no cursor_trail_style; {now}"
        )));
    }
    if let Some(extra) = rest.next() {
        return Err(Fail::usage(format!(
            "trail-film: one style a run, not {extra:?} too (quote a style of several words)"
        )));
    }
    Ok(Args {
        out,
        style,
        pet: pet.to_string(),
        themes,
        carets,
        scenes,
        replay,
        report_only,
        plant_star,
    })
}

/// A style's folder name: its letters and digits, the rest dashes.
#[must_use]
pub fn style_slug(style: &str) -> String {
    style
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// `--list`: the names a run takes.
#[must_use]
pub fn list() -> String {
    let mut s = format!(
        "style   a cursor_trail_style; default {FLAGSHIP:?}\n\
         pet     {} (--pet; default {FLAGSHIP_PET})\n\
         theme   {} (both: {} and {})\n\
         caret   {} (both: {} and {})\n\
         scene\n",
        filmed_pets(),
        THEMES.join(", "),
        THEMES[0],
        THEMES[1],
        CARETS.join(", "),
        CARETS[0],
        CARETS[1],
    );
    for (i, (name, what)) in SCENES.iter().enumerate() {
        let mark = if i < DEFAULT_SCENES { "*" } else { " " };
        let _ = writeln!(s, "  {mark} {name:<15} {what}");
    }
    s.push_str("  * filmed when no --scene is named\n");
    s
}

/// The films `a` names, as `(theme, caret, scene, folder)`.
fn films(a: &Args, root: &Path) -> Vec<(String, String, String, PathBuf)> {
    let mut out = Vec::new();
    for theme in &a.themes {
        for caret in &a.carets {
            for scene in &a.scenes {
                let dir = root.join(format!("{theme}_{caret}_{scene}"));
                out.push((theme.clone(), caret.clone(), scene.clone(), dir));
            }
        }
    }
    out
}

/// Replay `a`'s films into their folders under `root`: each folder's earlier film removed, then
/// `aterm-render`'s `trail_film::film` run once for them all.
fn replay(a: &Args, root: &Path, films: &[(String, String, String, PathBuf)]) -> Result<(), Fail> {
    let cannot = |what: &str, p: &Path, e: std::io::Error| {
        Fail::usage(format!("trail-film: cannot {what} {}: {e}", p.display()))
    };
    for (.., dir) in films {
        std::fs::create_dir_all(dir).map_err(|e| cannot("create", dir, e))?;
        for entry in std::fs::read_dir(dir).map_err(|e| cannot("read", dir, e))? {
            let path = entry.map_err(|e| cannot("read", dir, e))?.path();
            let ours = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("film."));
            if ours && path.is_file() {
                std::fs::remove_file(&path).map_err(|e| cannot("remove", &path, e))?;
            }
        }
    }
    let targo = crate::measure::targo();
    let mut cmd = aterm_uds::spawnfd::command(&targo);
    cmd.args([
        "--unverified",
        "test",
        "-p",
        "aterm-render",
        "--test",
        "render_suite",
        "--",
        "--ignored",
        "--exact",
        "trail_film::film",
        "--nocapture",
    ])
    .env("TRAIL_FILM_OUT", root)
    .env("TRAIL_FILM_STYLE", a.word())
    .env("TRAIL_FILM_THEMES", a.themes.join(","))
    .env("TRAIL_FILM_CARETS", a.carets.join(","))
    .env("TRAIL_FILM_SCENES", a.scenes.join(","))
    .current_dir(&paths().repo)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit());
    let ran = cmd
        .output()
        .map_err(|e| Fail::usage(format!("trail-film: {}: {e}", targo.to_string_lossy())))?;
    crate::stop::check()?;
    if !ran.status.success() {
        print!("{}", String::from_utf8_lossy(&ran.stdout));
        return Err(Fail::usage(format!(
            "trail-film: the replay (`targo --unverified test -p aterm-render --test render_suite -- --ignored \
             --exact trail_film::film`) exited {}",
            ran.status
        )));
    }
    Ok(())
}

/// `trail-film`: replay the films, print each one's reading, and exit 1 when one breaks a law.
/// # Errors
/// A usage error, or a film that could not be made or read (exit 2).
pub fn cmd_trail_film(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    if args.iter().any(|a| a == "--list") {
        print!("{}", list());
        return Ok(0);
    }
    let a = parse_args(args)?;
    let root = PathBuf::from(from_caller(&a.out)).join(a.folder());
    let films = films(&a, &root);
    if a.replay {
        replay(&a, &root, &films)?;
    }
    // Each film is read on its own thread, a few at a time; the report keeps the films' order.
    let workers = aterm_cores::available_parallelism().map_or(2, |n| n.get().min(6));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: std::sync::Mutex<Vec<Option<Result<Reading, String>>>> =
        std::sync::Mutex::new(vec![None; films.len()]);
    std::thread::scope(|s| {
        for _ in 0..workers.min(films.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((.., dir)) = films.get(i) else {
                        return;
                    };
                    let read = judge_film(dir, a.plant_star);
                    if let Ok(mut slots) = slots.lock() {
                        slots[i] = Some(read);
                    }
                }
            });
        }
    });
    let readings = slots
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut broke = 0;
    for ((theme, caret, scene, dir), read) in films.iter().zip(readings) {
        let read = read
            .unwrap_or_else(|| Err("its reader stopped".to_string()))
            .map_err(|e| Fail::usage(format!("trail-film: {}: {e}", dir.display())))?;
        println!(
            "film {:?} pet={} theme={theme} caret={caret} scene={scene} frames={} last_ms={} dir={}",
            a.style,
            a.pet,
            read.frames,
            read.last_ms,
            dir.display()
        );
        println!("{}", read.render());
        let broken = read.broken();
        broke += usize::from(!broken.is_empty());
        for law in broken {
            println!("  BROKEN   {law}");
        }
    }
    println!(
        "trail-film: {broke} of {} films break a law{}",
        films.len(),
        if a.report_only && broke > 0 {
            " (--report-only)"
        } else {
            ""
        }
    );
    Ok(i32::from(broke > 0 && !a.report_only))
}

#[cfg(test)]
#[path = "trail_film_tests.rs"]
mod tests;
