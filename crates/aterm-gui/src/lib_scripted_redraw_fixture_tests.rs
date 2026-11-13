// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId};

/// Render galleries on every test run. An explicit export directory keeps the images;
/// otherwise the returned guard removes an isolated temporary directory at test end.
pub(crate) fn gallery_directory(
    variable: &str,
) -> (std::path::PathBuf, Option<aterm_tempfile::TempDir>) {
    if let Some(path) = std::env::var_os(variable).filter(|path| !path.is_empty()) {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).expect("gallery export directory");
        (path, None)
    } else {
        let scratch = aterm_tempfile::tempdir().expect("isolated gallery directory");
        (scratch.path().to_path_buf(), Some(scratch))
    }
}

/// Puts window `wid`, made with no OS window and not yet composed, at display `scale`, as a
/// headless `--scale` launch lays it out before its first frame: the launch's scale (which
/// [`App::unattached_window_scale`] gives a window made later), the window's own, and its
/// metrics at that scale (its font size at `scale`'s device px, its pads and head scaled from
/// its own), held at the frame its grid takes there. The grid keeps its rows and columns, so
/// the first composition and capture draw its cells at `scale`'s pixels: a render at that
/// scale, not a resampling of one. A window already composed is refused: its next composition
/// would cross its layout's coordinate boundary, which re-grids its cursor effects and its pet
/// as a live change of display scale does and a cold launch never does.
pub(crate) fn at_scale(app: &mut App, wid: WindowId, scale: f64) {
    app.headless_scale = scale;
    let ws = app.windows.get_mut(&wid).expect("window");
    assert!(ws.os_window.is_none(), "a window with no OS window");
    assert!(
        ws.last_layout_coordinate_space.is_none(),
        "a window not yet composed, as a cold --scale launch lays out its first frame"
    );
    let (from, m, grid) = (ws.scale, ws.metrics, (ws.rows, ws.cols));
    let size = crate::app_window::font_size_of_device_px(m.font_px, from);
    let at = |px: usize| super::logical_to_device_px(px as f32 / from as f32, scale);
    ws.scale = scale;
    ws.metrics = super::MetricsView::applied(
        super::font_px_for_scale(size, scale),
        at(m.pad),
        at(m.pad_top),
        at(m.head),
    );
    app.apply_window_scale(wid);
    let px = app.window_frame_px_for(wid, grid.0, grid.1);
    app.note_headless_px_for(wid, px, grid);
    let ws = &app.windows[&wid];
    assert_eq!(
        (ws.rows, ws.cols),
        grid,
        "the grid keeps its cells at {scale}x"
    );
}

/// One capture of a window ([`Drawn::of`]): its pixels, and the grid, cells and frame its
/// metrics laid them out in.
pub(crate) struct Drawn {
    /// The frame's width and height in pixels.
    pub(crate) size: (usize, usize),
    /// The frame's RGBA bytes, row by row.
    pub(crate) rgba: Vec<u8>,
    grid: (u16, u16),
    scale: f64,
    font_px: f32,
    /// The size of a cell and of the frame, as the window's metrics give them.
    cell: (usize, usize),
    frame: (usize, usize),
    /// The pixel of frame cell (0, 0).
    origin: (usize, usize),
    /// The frame cells, `(row, col)`, that hold a letter or a digit.
    letters: Vec<(usize, usize)>,
}

/// What a glyph inks in its cell ([`Drawn::ink`]), each pixel read by its largest channel's
/// distance from the cell's ground (its most common colour), against the farthest pixel's.
#[derive(Clone, Copy)]
struct Ink {
    /// The width and height of the box round the pixels at least half as far.
    size: (usize, usize),
    /// The pixels at least a quarter as far (the glyph touches them), and of them those under
    /// three quarters (partial: an antialiased edge's).
    touched: usize,
    partial: usize,
}

/// How a fake 2x frame takes each cell from the 1x frame ([`Drawn::with_1x_cells`]).
#[derive(Clone, Copy)]
enum OneX {
    /// The 1x cell scaled to the 2x cell, each pixel the 1x pixel its top left corner falls
    /// in.
    Nearest,
    /// The 1x cell scaled to the 2x cell, each pixel blended from the four 1x pixels round its
    /// centre.
    Bilinear,
    /// The 1x cell at its own size at the 2x cell's origin, the rest of the 2x cell the 1x
    /// cell's ground.
    Unscaled,
}

impl Drawn {
    /// `(w, h, rgba)`, the capture of window `wid` just taken, with the layout its metrics
    /// give it: the frame is the one its grid takes with the shared backend tuned to the
    /// window, as a redraw tunes it, whatever the capture drew at.
    pub(crate) fn of(app: &mut App, wid: WindowId, (w, h, rgba): (usize, usize, Vec<u8>)) -> Self {
        app.apply_window_scale(wid);
        let ws = &app.windows[&wid];
        let frame = app.window_frame_px_for(wid, ws.rows, ws.cols);
        let letters = ws
            .input_scratch
            .cells
            .iter()
            .enumerate()
            .flat_map(|(row, cells)| {
                cells
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| cell.ch.is_alphanumeric())
                    .map(move |(col, _)| (row, col))
            })
            .collect();
        Drawn {
            size: (w, h),
            rgba,
            grid: (ws.rows, ws.cols),
            scale: ws.scale,
            font_px: ws.metrics.font_px,
            cell: app.win_cell_size(wid),
            frame: (frame.width as usize, frame.height as usize),
            origin: (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid)),
            letters,
        }
    }

    /// This frame with each pixel on an even row and column doubled over its 2 × 2 block
    /// (nearest neighbour), in this frame's layout: a doubling of the frame at half its size,
    /// as the galleries made their `-2x` crops before they drew at 2x. [`drawn_at_2x`]
    /// refuses it ([`Not2x::Doubled`]).
    fn doubled(&self) -> Drawn {
        let (w, h) = self.size;
        let mut rgba = self.rgba.clone();
        for y in 0..h {
            for x in 0..w {
                let at = ((y & !1) * w + (x & !1)) * 4;
                rgba.copy_within(at..at + 4, (y * w + x) * 4);
            }
        }
        Drawn {
            rgba,
            letters: self.letters.clone(),
            ..*self
        }
    }

    /// This frame with each of its cells drawn from `one`'s same cell as `how` says, the
    /// pixels outside the cells kept: the 1x glyph rasters drawn in this frame's layout, as a
    /// renderer that kept them at 2x would draw them. [`drawn_at_2x`] refuses each
    /// ([`Not2x::Unscaled`], [`Not2x::Resampled`]).
    fn with_1x_cells(&self, one: &Drawn, how: OneX) -> Drawn {
        let (rows, cols) = (
            self.cells().0.min(one.cells().0),
            self.cells().1.min(one.cells().1),
        );
        let ((aw, ah), (bw, bh)) = (one.cell, self.cell);
        let mut rgba = self.rgba.clone();
        for row in 0..rows {
            for col in 0..cols {
                let ((ax, _), (ay, _)) = one.cell_rect((row, col));
                let ((bx, bx1), (by, by1)) = self.cell_rect((row, col));
                let ground = one.ground((row, col));
                // Where a 2x pixel's centre falls in the 1x cell: the 1x pixels either side
                // of it, and how far it lies from the first.
                let around = |d: usize, from: usize, to: usize| {
                    let at = ((d as f32 + 0.5) * from as f32 / to as f32 - 0.5)
                        .clamp(0.0, (from - 1) as f32);
                    let lo = at.floor() as usize;
                    (lo, (lo + 1).min(from - 1), at - lo as f32)
                };
                for y in by..by1 {
                    for x in bx..bx1 {
                        let (dx, dy) = (x - bx, y - by);
                        let p = match how {
                            OneX::Nearest => one.px(ax + dx * aw / bw, ay + dy * ah / bh),
                            OneX::Unscaled if dx < aw && dy < ah => one.px(ax + dx, ay + dy),
                            OneX::Unscaled => ground,
                            OneX::Bilinear => {
                                let (x0, x1, fx) = around(dx, aw, bw);
                                let (y0, y1, fy) = around(dy, ah, bh);
                                let at = |x: usize, y: usize| one.px(ax + x, ay + y);
                                let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
                                std::array::from_fn(|i| {
                                    let top = f32::from(a[i]) * (1.0 - fx) + f32::from(b[i]) * fx;
                                    let low = f32::from(c[i]) * (1.0 - fx) + f32::from(d[i]) * fx;
                                    (top * (1.0 - fy) + low * fy).round() as u8
                                })
                            }
                        };
                        let at = (y * self.size.0 + x) * 4;
                        rgba[at..at + 4].copy_from_slice(&p);
                    }
                }
            }
        }
        Drawn {
            rgba,
            letters: self.letters.clone(),
            ..*self
        }
    }

    /// The RGBA bytes of the rect `x0..x1` by `y0..y1` of the frame.
    pub(crate) fn crop(&self, (x0, x1): (usize, usize), (y0, y1): (usize, usize)) -> Vec<u8> {
        let w = self.size.0;
        (y0..y1)
            .flat_map(|y| &self.rgba[(y * w + x0) * 4..(y * w + x1) * 4])
            .copied()
            .collect()
    }

    /// Pixel `(x, y)`'s RGBA.
    fn px(&self, x: usize, y: usize) -> [u8; 4] {
        let at = (y * self.size.0 + x) * 4;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }

    /// The rows and columns of whole cells the frame holds from its origin.
    fn cells(&self) -> (usize, usize) {
        let ((w, h), (x, y), (cw, ch)) = (self.size, self.origin, self.cell);
        (h.saturating_sub(y) / ch, w.saturating_sub(x) / cw)
    }

    /// The pixels `(x0..x1, y0..y1)` of frame cell `(row, col)`, cut to the frame.
    fn cell_rect(&self, (row, col): (usize, usize)) -> ((usize, usize), (usize, usize)) {
        let ((w, h), (cw, ch)) = (self.size, self.cell);
        let (x0, y0) = (self.origin.0 + col * cw, self.origin.1 + row * ch);
        ((x0.min(w), (x0 + cw).min(w)), (y0.min(h), (y0 + ch).min(h)))
    }

    /// Frame cell `at`'s ground: its most common colour.
    fn ground(&self, at: (usize, usize)) -> [u8; 4] {
        let ((x0, x1), (y0, y1)) = self.cell_rect(at);
        let mut pixels: Vec<[u8; 4]> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| self.px(x, y)))
            .collect();
        pixels.sort_unstable();
        pixels
            .chunk_by(|a, b| a == b)
            .max_by_key(|run| run.len())
            .map_or([0; 4], |run| run[0])
    }

    /// What frame cell `at`'s glyph inks, or `None` where every pixel is its ground.
    fn ink(&self, at: (usize, usize)) -> Option<Ink> {
        let ((x0, x1), (y0, y1)) = self.cell_rect(at);
        let ground = self.ground(at);
        let far = |x: usize, y: usize| {
            let p = self.px(x, y);
            (0..3)
                .map(|i| u32::from(p[i].abs_diff(ground[i])))
                .max()
                .unwrap_or(0)
        };
        let pixels = || (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y)));
        let top = pixels()
            .map(|(x, y)| far(x, y))
            .max()
            .filter(|&top| top > 0)?;
        let (mut xs, mut ys) = ((usize::MAX, 0), (usize::MAX, 0));
        let (mut touched, mut partial) = (0, 0);
        for (x, y) in pixels() {
            let d = far(x, y);
            if 2 * d >= top {
                xs = (xs.0.min(x), xs.1.max(x));
                ys = (ys.0.min(y), ys.1.max(y));
            }
            if 4 * d >= top {
                touched += 1;
                partial += usize::from(4 * d < 3 * top);
            }
        }
        Some(Ink {
            size: (xs.1 + 1 - xs.0, ys.1 + 1 - ys.0),
            touched,
            partial,
        })
    }

    /// Whether frame cell `at` holds a 2 × 2 block of pixels, on an even row and column of
    /// the frame, that is not one colour. A nearest-neighbour doubling has none: each such
    /// block is one pixel of the frame it doubles.
    fn split_at(&self, at: (usize, usize)) -> bool {
        let ((x0, x1), (y0, y1)) = self.cell_rect(at);
        (y0.next_multiple_of(2)..y1.saturating_sub(1))
            .step_by(2)
            .any(|y| {
                (x0.next_multiple_of(2)..x1.saturating_sub(1))
                    .step_by(2)
                    .any(|x| {
                        let p = self.px(x, y);
                        p != self.px(x + 1, y)
                            || p != self.px(x, y + 1)
                            || p != self.px(x + 1, y + 1)
                    })
            })
    }
}

/// What [`drawn_at_2x`] reads of a 2x frame against its 1x frame, over the frame cells that
/// hold a letter or a digit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At2x {
    /// The letter and digit cells, and those of them a glyph inks at both scales.
    letters: usize,
    inked: usize,
    /// The letter cells with a 2 × 2 block of pixels, on an even row and column, that is not
    /// one colour.
    split: usize,
    /// The inked cells whose glyph's box at 2x is at least 1.8 times its 1x box across and
    /// down.
    grown: usize,
    /// `(partial, touched)` over the inked cells' glyphs ([`Ink`]), at 1x and at 2x.
    edges: [(usize, usize); 2],
}

impl At2x {
    /// The tab-separated names of [`At2x::row`]'s fields.
    pub(crate) const HEAD: &str = "frame\tletter cells\tinked\tsplit\tgrown\t\
                                   partial share 1x\tpartial share 2x\t2x over 1x";

    /// This reading of `frame` as a tab-separated row: its counts, and the share of the
    /// pixels the glyphs touch that are partial at 1x, at 2x, and the second over the first.
    pub(crate) fn row(&self, frame: &str) -> String {
        let share = |(partial, touched): (usize, usize)| partial as f64 / touched.max(1) as f64;
        let [one, two] = self.edges.map(share);
        format!(
            "{frame}\t{}\t{}\t{}\t{}\t{one:.4}\t{two:.4}\t{:.3}",
            self.letters,
            self.inked,
            self.split,
            self.grown,
            two / one
        )
    }

    /// Whether half or more of the letter cells hold no 2 × 2 block, on even pixels, that is
    /// not one colour ([`Not2x::Doubled`]).
    pub(crate) const fn doubled(&self) -> bool {
        2 * self.split <= self.letters
    }

    /// Whether half or more of the inked letter cells ink a box under 1.8 times their 1x box
    /// across or down ([`Not2x::Unscaled`]).
    pub(crate) const fn unscaled(&self) -> bool {
        2 * self.grown <= self.inked
    }

    /// Whether the glyphs' partial pixels are not under nine tenths of their share at 1x
    /// ([`Not2x::Resampled`]).
    pub(crate) const fn resampled(&self) -> bool {
        let [(p1, t1), (p2, t2)] = self.edges;
        10 * p2 * t1 >= 9 * p1 * t2
    }
}

/// Why a frame is not its scene rendered at 2x ([`drawn_at_2x`]).
#[derive(Debug)]
pub(crate) enum Not2x {
    /// Its grid, scale, font, frame, cells or letter cells are not the 2x layout of the 1x
    /// frame's scene.
    Layout(String),
    /// Half or more of its letter cells hold no 2 × 2 block, on even pixels, that is not one
    /// colour: a nearest-neighbour doubling of a frame half its size.
    Doubled(At2x),
    /// Half or more of its inked letter cells ink a box under 1.8 times their 1x box across
    /// or down: glyphs at their 1x size.
    Unscaled(At2x),
    /// Its glyphs' partial pixels are not under nine tenths of their share at 1x. A render at
    /// 2x keeps an edge about a pixel across while a glyph's area grows fourfold, so the share
    /// of its pixels that are partial falls; a resampling of 1x pixels widens each edge with
    /// the glyph, so the share stays (a nearest resampling copies each pixel into its block,
    /// 1.000 of it) or grows (bilinear, 1.974). How far a render's share falls depends on its
    /// 1x edges, not only on its scale: a hinted face sets its 1x stems on whole pixels with
    /// faint fringes, which dark ink on paper leaves under the quarter of the ink a glyph
    /// touches, so its 1x share is already low. Windows' Iosevka Term NL, hinted, reads a 2x
    /// share of 0.29 on Aterm Night and on Aterm Day, against 1x shares of 0.44 and 0.35:
    /// 0.650 of it and 0.812 to 0.843. Unhinted, the same frames read 0.556 to 0.567
    /// (`quota_row_gallery`, 2026-11-11).
    Resampled(At2x),
}

impl Not2x {
    /// What [`drawn_at_2x`] read of the frame's pixels, unless its layout refused it first.
    pub(crate) const fn reading(&self) -> Option<&At2x> {
        match self {
            Self::Layout(_) => None,
            Self::Doubled(at) | Self::Unscaled(at) | Self::Resampled(at) => Some(at),
        }
    }
}

impl std::fmt::Display for Not2x {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Layout(why) => f.write_str(why),
            Self::Doubled(at) => write!(
                f,
                "{} of its {} letter cells hold a 2 x 2 block of pixels that is not one \
                 colour: a doubling, not a render",
                at.split, at.letters
            ),
            Self::Unscaled(at) => write!(
                f,
                "{} of its {} inked letter cells ink a box at least 1.8 times their 1x box: \
                 glyphs at their 1x size, not a render",
                at.grown, at.inked
            ),
            Self::Resampled(at) => {
                let [(p1, t1), (p2, t2)] = at.edges;
                write!(
                    f,
                    "{p2} of the {t2} pixels its glyphs touch are partial, against {p1} of \
                     {t1} at 1x: not under nine tenths of that share, a resampling of 1x \
                     glyphs, not a render"
                )
            }
        }
    }
}

/// Whether `two` is the scene `one` showed rendered again at 2x, each drawn on a window of
/// its own (the second put [`at_scale`] 2 before its first frame): what it reads ([`At2x`]),
/// or why not ([`Not2x`]). It must have the same grid, at the window's 2x metrics (the font
/// size of `one` at twice its device px), in the frame those metrics give that grid, with
/// cells at least twice the 1x size less a pixel and the same letter and digit cells. Its
/// pixels must be a render's, read cell by cell against `one`'s: more than half of the letter
/// cells hold a 2 × 2 block, on even pixels, that is not one colour (a doubling has none),
/// more than half of those a glyph inks at both scales ink a box at least 1.8 times the 1x one
/// across and down (1x glyph rasters in 2x cells do not), and the share of the pixels the
/// glyphs touch that are partial is under nine tenths of the 1x share (a resampling of 1x
/// glyphs widens each edge with the glyph, so it keeps that share or raises it).
pub(crate) fn drawn_at_2x(one: &Drawn, two: &Drawn) -> Result<At2x, Not2x> {
    let layout = |why: String| Err(Not2x::Layout(why));
    let size = crate::app_window::font_size_of_device_px(one.font_px, one.scale);
    let font_px = super::font_px_for_scale(size, 2.0);
    if two.grid != one.grid {
        return layout(format!("its grid is {:?}, not {:?}", two.grid, one.grid));
    }
    if two.scale != 2.0 || two.font_px != font_px {
        return layout(format!(
            "its window is at {}x with a {} px font, not 2x with {font_px} px",
            two.scale, two.font_px
        ));
    }
    for (what, drawn) in [("1x", one), ("2x", two)] {
        if drawn.size != drawn.frame {
            return layout(format!(
                "its {what} frame is {:?}, not the {:?} its grid takes",
                drawn.size, drawn.frame
            ));
        }
    }
    if two.cell.0 < 2 * one.cell.0 - 1 || two.cell.1 < 2 * one.cell.1 - 1 {
        return layout(format!(
            "its cells are {:?}, under twice the 1x {:?}",
            two.cell, one.cell
        ));
    }
    if two.letters != one.letters || two.letters.is_empty() {
        return layout(format!(
            "its {} letter and digit cells are not the 1x frame's {}",
            two.letters.len(),
            one.letters.len()
        ));
    }
    let mut at = At2x {
        letters: two.letters.len(),
        inked: 0,
        split: two
            .letters
            .iter()
            .filter(|&&cell| two.split_at(cell))
            .count(),
        grown: 0,
        edges: [(0, 0); 2],
    };
    for &cell in &two.letters {
        let (Some(a), Some(b)) = (one.ink(cell), two.ink(cell)) else {
            continue;
        };
        at.inked += 1;
        at.grown += usize::from(5 * b.size.0 >= 9 * a.size.0 && 5 * b.size.1 >= 9 * a.size.1);
        for (edge, ink) in at.edges.iter_mut().zip([a, b]) {
            edge.0 += ink.partial;
            edge.1 += ink.touched;
        }
    }
    if at.doubled() {
        Err(Not2x::Doubled(at))
    } else if at.unscaled() {
        Err(Not2x::Unscaled(at))
    } else if at.resampled() {
        Err(Not2x::Resampled(at))
    } else {
        Ok(at)
    }
}

/// CONTROL: the fakes of a scene's 2x frame `two` are each refused by [`drawn_at_2x`] and
/// break their own rule, whichever rule it names first: `two` doubled from half its size
/// ([`At2x::doubled`]), and `one`'s 1x cells drawn in `two`'s unscaled ([`At2x::unscaled`])
/// and resampled to them, nearest and bilinear ([`At2x::resampled`]). A fake that breaks an
/// earlier rule as well still counts: whether the nearest fake splits more than half its
/// cells depends on how many letters sit in even grid columns (an odd-width 2x cell starts
/// on an even pixel only there), not on how it was drawn.
pub(crate) fn refuse_fakes(one: &Drawn, two: &Drawn) {
    let fakes = [
        (
            "a doubling of the 2x frame",
            two.doubled(),
            "doubled",
            At2x::doubled as fn(&At2x) -> bool,
        ),
        (
            "the 1x cells unscaled in the 2x cells",
            two.with_1x_cells(one, OneX::Unscaled),
            "unscaled",
            At2x::unscaled,
        ),
        (
            "the 1x cells nearest-resampled to the 2x cells",
            two.with_1x_cells(one, OneX::Nearest),
            "resampled",
            At2x::resampled,
        ),
        (
            "the 1x cells bilinear-resampled to the 2x cells",
            two.with_1x_cells(one, OneX::Bilinear),
            "resampled",
            At2x::resampled,
        ),
    ];
    for (what, fake, rule, breaks) in fakes {
        let at = match drawn_at_2x(one, &fake) {
            Ok(at) => panic!("CONTROL: {what} passes as a 2x render ({})", at.row(what)),
            Err(why) => *why.reading().unwrap_or_else(|| {
                panic!("CONTROL: {what} is refused for its layout, not read: {why}")
            }),
        };
        assert!(
            breaks(&at),
            "CONTROL: {what} keeps the {rule} rule ({})",
            at.row(what)
        );
    }
}

/// The real redraw path with every decorative effect off, so equal content is equal pixels.
pub(crate) fn scripted_app() -> App {
    scripted_app_with_sink(std::sync::Arc::new(aterm_session::sink::SinkWriter::new(
        -1,
    )))
}

/// [`scripted_app`] whose session writes to `sink`, so input through the seam reaches a PTY a test holds.
pub(crate) fn scripted_app_with_sink(sink: std::sync::Arc<aterm_session::sink::SinkWriter>) -> App {
    let mut app = App::headless_for_test_with_sink(sink);
    app.config.serious_mode = Some(true);
    app.config.cursor_trail = Some(false);
    app.config.cursor_trail_style = Some("off".into());
    app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
        enabled: Some(false),
        ..Default::default()
    });
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    let ws = app.windows.get_mut(&WindowId(0)).expect("window");
    ws.present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    app
}

pub(crate) fn surface(app: &mut App) -> &mut crate::present::scripted::ScriptedPresenter {
    match app
        .windows
        .get_mut(&WindowId(0))
        .and_then(|ws| ws.present.as_mut())
    {
        Some(crate::PresentTarget::Cpu { surface }) => {
            surface.scripted_mut().expect("scripted surface")
        }
        _ => panic!("the fixture presents through its scripted CPU surface"),
    }
}

pub(crate) fn glass(app: &mut App) -> ((usize, usize), Vec<u32>) {
    let s = surface(app);
    (s.size(), s.glass().to_vec())
}

/// Run `capture` without the window's surface.
pub(crate) fn headless<R>(app: &mut App, capture: impl FnOnce(&mut App) -> R) -> R {
    let target = app
        .windows
        .get_mut(&WindowId(0))
        .and_then(|ws| ws.present.take());
    let out = capture(app);
    if let Some(ws) = app.windows.get_mut(&WindowId(0)) {
        ws.present = target;
    }
    out
}

pub(crate) fn feed(app: &App, bytes: &[u8]) {
    let term = app
        .front_terminal(WindowId(0))
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&term).process(bytes);
}

pub(crate) fn has_damage(app: &App) -> bool {
    let term = app
        .front_terminal(WindowId(0))
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&term).has_damage()
}
