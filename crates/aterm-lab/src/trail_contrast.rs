// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-contrast <png>`: the WCAG contrast of the cells of an aterm capture,
//! as JSON. Every check of a cursor trail's legibility, caret or demo is
//! measured with this probe and the frames [`crate::trail_gallery`] takes, so
//! results compare across changes. The frame is decoded by the workspace's own
//! PNG crate (`aterm_png`: every colour type and depth, interlaced or not).
//!
//! THE GRID. A cell's box is `origin + (col * cw, row * ch)`, sized `cw x ch`.
//! It comes from `--origin X,Y --cell WxH`, or from a `dims` reply (`--dims
//! FILE`): origin = (pad, head + pad_top), cell = (cell_w, cell_h). A reply
//! with chrome rows, offsets or bands is refused, since the origin is then not
//! known; pass `--origin` for such a frame.
//!
//! THE CELLS. `--find TEXT` measures each glyph of every occurrence of TEXT in
//! the frame's screen text; `--cells ROW:COL[-COL]` and `--row ROW` name cells
//! directly. With none of them, every non-blank glyph of the screen text is
//! measured. The caret's own cell, and the `--newest N` glyph cells left of it
//! on its row (default 3; a blank cell or a space ends the run), are always
//! reported when the caret is known. A glyph's cells are counted with the
//! terminal grid's own width table (`aterm_grapheme::char_width`).
//!
//! SIDECARS. `trail-gallery` writes, beside `<stem>.<frame>.png`:
//! `<stem>.dims.txt` (the `dims` reply), `<stem>.<frame>.cursor.txt`
//! (`cursor`), `<stem>.<frame>.text.txt` (`text`) and
//! `<stem>.<frame>.clean.png` (`image clean`: the same frame with every cursor
//! effect suppressed). They are read when present; `--dims`, `--text`,
//! `--cursor`, `--clean` and `--caret` name other files or override them.
//!
//! THE MEASURE. With a clean twin, a glyph's ink is the median colour of the
//! pixels that are glyph core in the clean frame (at least [`CORE_SHARE`] of
//! the way from the clean ground to the clean glyph's extreme, in luminance),
//! and its ground is the median colour of the pixels that are pure ground in
//! the clean frame; both are read from the measured frame, so a halo or a
//! ribbon under the glyph counts against it. `clean_contrast` is the same
//! measure on the clean frame, the glyph's own legibility with no effect
//! drawn. Without a twin, the ground is the cell's median colour and the ink
//! its fourth most contrasting pixel (robust to three stray spark pixels);
//! that `extreme` method is coarser and says so in `method`. A cell whose
//! clean (or measured) extreme is under [`BLANK_BELOW`] against its ground
//! holds no glyph: `blank`.
//!
//! THE CARET is the cursor cell's pixels that differ from the page colour (the
//! frame's most common colour, or `--page RRGGBB`): their median colour, how
//! much of the cell they cover, and that colour's contrast against the page.
//! `diff_vs_clean` counts the pixels (any channel off by more than
//! [`DIFF_DELTA`]) the effects changed outside the caret cell, and their
//! bounding box.
//!
//! The report is the JSON of `tools/visual-judge/trail-contrast.py`, which
//! this verb replaces, key for key: one line per key, one per measured cell.
//!
//! EXIT: 0; 1 when a threshold fails (`--min` for the selected cells,
//! `--newest-min` for the glyphs left of the caret, `--caret-min` for the
//! caret against the page), each failure named in `failures`; 2 on a usage,
//! file or decoding error.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::instance::{take_flag, take_value, take_values};
use crate::paths::from_caller;
use crate::pyjson::{J, Style};
use crate::{Fail, Res};

/// A glyph's core: at least this share of the way from the clean ground to
/// the clean glyph's extreme, in luminance.
pub const CORE_SHARE: f64 = 0.70;
/// A cell whose extreme is under this ratio against its ground holds no glyph.
pub const BLANK_BELOW: f64 = 1.5;
/// A pixel the effects changed: a channel off by more than this.
pub const DIFF_DELTA: u8 = 12;
/// A caret pixel: at least this ratio against the page.
pub const CARET_DIFF: f64 = 1.1;
/// The widest and tallest frame decoded.
const MAX_SIDE: u32 = 1 << 14;

/// The verb's own help (`trail-contrast --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-contrast <png> [--find TEXT]... [--cells ROW:COL[-COL]]... [--row N]...
         [--newest N] [--min X] [--newest-min X] [--caret-min X] [--dims FILE | --origin X,Y --cell WxH]
         [--text FILE] [--clean PNG | --no-clean] [--cursor FILE | --caret ROW,COL] [--page RRGGBB]

The WCAG contrast of the cells of an aterm capture, as JSON: the grid (from the frame's <stem>.dims.txt sidecar, --dims,
or --origin and --cell), the page colour, one line per measured glyph cell (--find: each glyph of every occurrence of
TEXT in the screen text; --cells and --row: cells by number; none: every glyph on screen), the caret cell against the
page with the --newest N (3) glyphs left of it, and diff_vs_clean, the pixels the effects changed outside the caret cell.
With the frame's clean twin (<stem>.<frame>.clean.png, `image clean`) a glyph's ink and ground are the pixels that are
glyph core and pure ground in the twin, read from the frame, so a halo or a ribbon under the glyph counts against it;
clean_contrast is the same glyph with no effect drawn. Without one (--no-clean) the coarser `extreme` method is used.
The sidecars trail-gallery writes (.text.txt, .cursor.txt, .clean.png, .dims.txt) are read when present.

exit: 0; 1 a threshold failed (--min: a selected cell, --newest-min: a glyph left of the caret, --caret-min: the caret
against the page; each named in `failures`); 2 a usage, file or decoding error
";

/// An sRGB colour.
pub type Rgb = [u8; 3];

/// WCAG 2's linearised channel, for every 8-bit value.
fn linear_table() -> &'static [f64; 256] {
    static T: OnceLock<[f64; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0.0; 256];
        for (v, slot) in (0u8..=255).zip(t.iter_mut()) {
            let c = f64::from(v) / 255.0;
            *slot = if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
        }
        t
    })
}

/// WCAG 2 relative luminance of an sRGB colour.
#[must_use]
pub fn luminance(rgb: Rgb) -> f64 {
    let l = linear_table();
    0.2126 * l[usize::from(rgb[0])]
        + 0.7152 * l[usize::from(rgb[1])]
        + 0.0722 * l[usize::from(rgb[2])]
}

/// WCAG 2 contrast ratio between two sRGB colours (1.0 to 21.0).
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `#rrggbb`.
#[must_use]
pub fn hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn parse_hex(text: &str) -> Result<Rgb, String> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    let bad = || format!("not an RRGGBB colour: {}", crate::util::py_repr(text));
    if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let ch = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| bad());
    Ok([ch(0)?, ch(2)?, ch(4)?])
}

/// `x` rounded to `places` decimals as Python's `round` does: the correctly
/// rounded decimal, read back.
#[must_use]
pub fn round_to(x: f64, places: usize) -> f64 {
    format!("{x:.places$}").parse().unwrap_or(x)
}

/// Each channel's median (the upper one of an even count), or `None` for no pixels.
fn median(px: &[Rgb]) -> Option<Rgb> {
    if px.is_empty() {
        return None;
    }
    let mid = px.len() / 2;
    let mut out = [0u8; 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut ch: Vec<u8> = px.iter().map(|p| p[i]).collect();
        ch.sort_unstable();
        *slot = ch[mid];
    }
    Some(out)
}

/// The most common colour, the first seen among equals; `None` for no pixels.
fn mode(px: impl IntoIterator<Item = Rgb>) -> Option<Rgb> {
    let mut counts: HashMap<Rgb, (usize, usize)> = HashMap::new();
    for (i, p) in px.into_iter().enumerate() {
        counts.entry(p).or_insert((0, i)).0 += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1)))
        .map(|(p, _)| p)
}

/// The first colour of `px` that contrasts most with `against`.
fn most_contrasting(px: &[Rgb], against: Rgb) -> Option<Rgb> {
    let mut best: Option<(f64, Rgb)> = None;
    for &p in px {
        let c = contrast(p, against);
        if best.is_none_or(|(b, _)| c > b) {
            best = Some((c, p));
        }
    }
    best.map(|(_, p)| p)
}

/// A decoded frame: its colours, row-major (alpha dropped).
#[derive(Clone, Debug)]
pub struct Image {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    px: Vec<Rgb>,
}

/// A pixel box: `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxBox {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl PxBox {
    fn holds(self, x: i64, y: i64) -> bool {
        self.x0 <= x && x < self.x1 && self.y0 <= y && y < self.y1
    }
}

impl Image {
    /// Read and decode the PNG at `path`.
    ///
    /// # Errors
    /// The file cannot be read, is not a PNG, or does not decode.
    pub fn read(path: &Path) -> Result<Image, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Image::decode(path, &bytes)
    }

    /// Decode PNG `bytes` (named `path` in messages).
    ///
    /// # Errors
    /// Not a PNG, or one `aterm_png` refuses.
    pub fn decode(path: &Path, bytes: &[u8]) -> Result<Image, String> {
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(format!("{} is not a PNG", path.display()));
        }
        let (rgba, width, height) = aterm_png::decode_rgba8(bytes, MAX_SIDE).ok_or_else(|| {
            format!(
                "{}: the PNG does not decode (corrupt, or a side past {MAX_SIDE} px)",
                path.display()
            )
        })?;
        let px = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        Ok(Image {
            path: path.to_path_buf(),
            width,
            height,
            px,
        })
    }

    /// A frame from its colours, row-major.
    ///
    /// # Panics
    /// `px` does not hold `width * height` colours.
    #[cfg(test)]
    #[must_use]
    pub fn from_pixels(width: usize, height: usize, px: Vec<Rgb>) -> Image {
        assert_eq!(px.len(), width * height, "a {width}x{height} frame");
        Image {
            path: PathBuf::from("<memory>"),
            width,
            height,
            px,
        }
    }

    fn at(&self, x: usize, y: usize) -> Rgb {
        self.px[y * self.width + x]
    }

    /// The colours of `b`, clipped to the frame, row-major.
    fn region(&self, b: PxBox) -> Vec<Rgb> {
        let clip = |v: i64, hi: usize| usize::try_from(v.max(0)).unwrap_or(0).min(hi);
        let (x0, x1) = (clip(b.x0, self.width), clip(b.x1, self.width));
        let (y0, y1) = (clip(b.y0, self.height), clip(b.y1, self.height));
        let mut out = Vec::with_capacity(x1.saturating_sub(x0) * y1.saturating_sub(y0));
        for y in y0..y1 {
            for x in x0..x1 {
                out.push(self.at(x, y));
            }
        }
        out
    }

    /// The frame's most common colour (black for an empty frame).
    #[must_use]
    pub fn most_common(&self) -> Rgb {
        mode(self.px.iter().copied()).unwrap_or([0, 0, 0])
    }
}

/// Where the cells are, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub ox: i64,
    pub oy: i64,
    pub cw: i64,
    pub ch: i64,
    pub rows: i64,
    pub cols: i64,
    /// `dims` or `args`.
    pub source: &'static str,
}

impl Grid {
    /// The box of `span` cells from `row`, `col`.
    #[must_use]
    pub fn cell_box(&self, row: i64, col: i64, span: i64) -> PxBox {
        let (x0, y0) = (self.ox + col * self.cw, self.oy + row * self.ch);
        PxBox {
            x0,
            y0,
            x1: x0 + span * self.cw,
            y1: y0 + self.ch,
        }
    }

    fn holds(&self, row: i64, col: i64) -> bool {
        (0..self.rows).contains(&row) && (0..self.cols).contains(&col)
    }

    fn describe(&self) -> J {
        J::obj([
            ("origin", J::Arr(vec![J::Int(self.ox), J::Int(self.oy)])),
            ("cell", J::Arr(vec![J::Int(self.cw), J::Int(self.ch)])),
            ("rows", J::Int(self.rows)),
            ("cols", J::Int(self.cols)),
            ("source", J::s(self.source)),
        ])
    }
}

/// The grid a `dims` reply (`OK <rows> <cols> <px_w> <px_h> key=value…`) names.
///
/// # Errors
/// Not a dims reply, a field missing, or chrome, offsets or bands that leave
/// the origin unknown.
pub fn grid_from_dims(text: &str) -> Result<Grid, String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let not_dims = || {
        let head: String = text.chars().take(80).collect();
        format!("not a dims reply: {}", crate::util::py_repr(&head))
    };
    if tokens.len() < 5 || tokens[0] != "OK" {
        return Err(not_dims());
    }
    let rows: i64 = tokens[1].parse().map_err(|_| not_dims())?;
    let cols: i64 = tokens[2].parse().map_err(|_| not_dims())?;
    let mut facts: HashMap<&str, &str> = HashMap::new();
    for t in &tokens[5..] {
        if let Some((k, v)) = t.split_once('=') {
            facts.insert(k, v);
        }
    }
    for key in [
        "tab_rows",
        "offset_x",
        "offset_y",
        "band_left",
        "band_top",
        "crop_left",
        "crop_top",
    ] {
        if let Some(v) = facts.get(key)
            && !matches!(*v, "0" | "none")
        {
            return Err(format!(
                "dims has {key}={v}, so the grid origin is not known; pass --origin and --cell"
            ));
        }
    }
    let num = |k: &str, default: Option<&str>| -> Result<i64, String> {
        facts
            .get(k)
            .copied()
            .or(default)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| "dims reply lacks pad/pad_top/cell_w/cell_h".to_string())
    };
    Ok(Grid {
        ox: num("pad", None)?,
        oy: num("head", Some("0"))? + num("pad_top", None)?,
        cw: num("cell_w", None)?,
        ch: num("cell_h", None)?,
        rows,
        cols,
        source: "dims",
    })
}

fn parse_pair(text: &str, sep: char, what: &str) -> Result<(i64, i64), String> {
    let lower = text.to_lowercase();
    let parts: Vec<&str> = lower.split(sep).collect();
    if parts.len() != 2 {
        return Err(format!(
            "{what} wants A{sep}B, got {}",
            crate::util::py_repr(text)
        ));
    }
    match (parts[0].trim().parse(), parts[1].trim().parse()) {
        (Ok(a), Ok(b)) => Ok((a, b)),
        _ => Err(format!(
            "{what} wants integers, got {}",
            crate::util::py_repr(text)
        )),
    }
}

/// A glyph of the screen text: where it sits and how many cells it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub row: i64,
    pub col: i64,
    pub ch: char,
    pub width: i64,
}

/// The cells `c` takes on the grid (0 for a mark that joins the glyph before it).
fn cell_width(c: char) -> i64 {
    i64::try_from(aterm_grapheme::char_width(c)).unwrap_or(1)
}

/// Every glyph of the screen text, by row and column.
#[must_use]
pub fn screen_cells(lines: &[String]) -> Vec<Glyph> {
    let mut out = Vec::new();
    for (row, line) in (0i64..).zip(lines) {
        let mut col = 0;
        for ch in line.chars() {
            let width = cell_width(ch);
            if width == 0 {
                continue;
            }
            out.push(Glyph {
                row,
                col,
                ch,
                width,
            });
            col += width;
        }
    }
    out
}

/// Each glyph of every occurrence of `needle` (occurrences may overlap).
#[must_use]
pub fn find_cells(lines: &[String], needle: &str) -> Vec<Glyph> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    for (row, line) in (0i64..).zip(lines) {
        let mut from = 0;
        while let Some(at) = line[from..].find(needle).map(|i| from + i) {
            let mut col: i64 = line[..at].chars().map(cell_width).sum();
            for ch in needle.chars() {
                let width = cell_width(ch);
                if width > 0 {
                    out.push(Glyph {
                        row,
                        col,
                        ch,
                        width,
                    });
                    col += width;
                }
            }
            from = at + line[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// One cell's measure, as the report's fields.
fn measure_cell(
    frame: &Image,
    clean: Option<&Image>,
    grid: &Grid,
    row: i64,
    col: i64,
    span: i64,
) -> Vec<(String, J)> {
    let kv = |pairs: Vec<(&str, J)>| -> Vec<(String, J)> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    };
    let b = grid.cell_box(row, col, span);
    let pixels = frame.region(b);
    if pixels.is_empty() {
        return kv(vec![("outside", J::Bool(true))]);
    }
    let Some(clean) = clean else {
        let bed = median(&pixels).unwrap_or_default();
        let mut ranked: Vec<(f64, Rgb)> = pixels.iter().map(|&p| (contrast(p, bed), p)).collect();
        // Stable and descending, so equals keep their order (Python's `sorted(reverse=True)`).
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        let ink = ranked[3.min(ranked.len() - 1)].1;
        let ratio = contrast(ink, bed);
        if ratio < BLANK_BELOW {
            return kv(vec![("blank", J::Bool(true)), ("method", J::s("extreme"))]);
        }
        return kv(vec![
            ("method", J::s("extreme")),
            ("ink", J::s(hex(ink))),
            ("ground", J::s(hex(bed))),
            ("contrast", J::Float(round_to(ratio, 2))),
        ]);
    };
    let clean_px = clean.region(b);
    let clean_ground = mode(clean_px.iter().copied()).unwrap_or_default();
    let extreme = most_contrasting(&clean_px, clean_ground).unwrap_or(clean_ground);
    if contrast(extreme, clean_ground) < BLANK_BELOW {
        return kv(vec![("blank", J::Bool(true)), ("method", J::s("mask"))]);
    }
    let ground_luma = luminance(clean_ground);
    let reach = CORE_SHARE * (luminance(extreme) - ground_luma).abs();
    let (mut ink, mut bed, mut clean_ink) = (Vec::new(), Vec::new(), Vec::new());
    for (&c, &p) in clean_px.iter().zip(&pixels) {
        if c == clean_ground {
            bed.push(p);
        } else if (luminance(c) - ground_luma).abs() >= reach {
            ink.push(p);
            clean_ink.push(c);
        }
    }
    let core_px = i64::try_from(ink.len()).unwrap_or(i64::MAX);
    // The extreme pixel is core and the ground is the box's mode: neither set is empty.
    let (ink, bed) = (
        median(&ink).unwrap_or_default(),
        median(&bed).unwrap_or_default(),
    );
    let clean_ink = median(&clean_ink).unwrap_or_default();
    kv(vec![
        ("method", J::s("mask")),
        ("ink", J::s(hex(ink))),
        ("ground", J::s(hex(bed))),
        ("contrast", J::Float(round_to(contrast(ink, bed), 2))),
        (
            "clean_contrast",
            J::Float(round_to(contrast(clean_ink, clean_ground), 2)),
        ),
        ("core_px", J::Int(core_px)),
    ])
}

fn measure_caret(frame: &Image, grid: &Grid, row: i64, col: i64, page: Rgb) -> J {
    let pixels = frame.region(grid.cell_box(row, col, 1));
    let lit: Vec<Rgb> = pixels
        .iter()
        .copied()
        .filter(|&p| contrast(p, page) >= CARET_DIFF)
        .collect();
    // A cell's pixel count is far below 2^52: the division is exact enough.
    let coverage = lit.len() as f64 / pixels.len().max(1) as f64;
    let mut out = J::obj([
        ("row", J::Int(row)),
        ("col", J::Int(col)),
        ("coverage", J::Float(round_to(coverage, 3))),
    ]);
    match median(&lit) {
        None => {
            out.set("colour", J::Null);
            out.set("contrast", J::Null);
            out.set("peak_contrast", J::Null);
        }
        Some(colour) => {
            let peak = lit
                .iter()
                .map(|&p| contrast(p, page))
                .fold(f64::MIN, f64::max);
            out.set("colour", J::s(hex(colour)));
            out.set("contrast", J::Float(round_to(contrast(colour, page), 2)));
            out.set("peak_contrast", J::Float(round_to(peak, 2)));
        }
    }
    out
}

/// The pixels the effects changed (a channel off by more than [`DIFF_DELTA`]),
/// and those of them outside the caret's box with their bounding box.
#[must_use]
pub fn diff_vs_clean(frame: &Image, clean: &Image, caret: Option<PxBox>) -> J {
    let (mut total, mut outside) = (0i64, 0i64);
    let mut bbox: Option<[i64; 4]> = None;
    for y in 0..frame.height.min(clean.height) {
        for x in 0..frame.width.min(clean.width) {
            let (a, b) = (frame.at(x, y), clean.at(x, y));
            if (0..3).all(|i| a[i].abs_diff(b[i]) <= DIFF_DELTA) {
                continue;
            }
            total += 1;
            let (xi, yi) = (
                i64::try_from(x).unwrap_or(i64::MAX),
                i64::try_from(y).unwrap_or(i64::MAX),
            );
            if caret.is_some_and(|c| c.holds(xi, yi)) {
                continue;
            }
            outside += 1;
            bbox = Some(bbox.map_or([xi, yi, xi, yi], |b| {
                [b[0].min(xi), b[1].min(yi), b[2].max(xi), b[3].max(yi)]
            }));
        }
    }
    J::obj([
        ("px", J::Int(total)),
        ("outside_caret_px", J::Int(outside)),
        (
            "outside_caret_bbox",
            bbox.map_or(J::Null, |b| J::Arr(b.iter().map(|&v| J::Int(v)).collect())),
        ),
    ])
}

/// `<stem>.<frame>.png` -> `<stem>.<frame><suffix>` (`per_frame`), else
/// `<stem><suffix>`; only when that file exists.
#[must_use]
pub fn sidecar(png: &Path, suffix: &str, per_frame: bool) -> Option<PathBuf> {
    let name = png.file_name()?.to_str()?;
    let base = name.strip_suffix(".png")?;
    let base = if per_frame {
        base
    } else {
        base.split_once('.')?.0
    };
    let path = png.with_file_name(format!("{base}{suffix}"));
    path.exists().then_some(path)
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// A caller's path made absolute and normalised (Python's `os.path.abspath`).
fn abspath(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let joined = from_caller(p);
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// What `trail-contrast` was asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub png: String,
    pub dims: Option<String>,
    pub origin: Option<String>,
    pub cell: Option<String>,
    pub text: Option<String>,
    pub clean: Option<String>,
    pub no_clean: bool,
    pub cursor: Option<String>,
    pub caret: Option<String>,
    pub page: Option<String>,
    pub find: Vec<String>,
    pub cells: Vec<String>,
    pub rows: Vec<i64>,
    pub newest: i64,
    pub min: Option<f64>,
    pub newest_min: Option<f64>,
    pub caret_min: Option<f64>,
}

fn number<T: std::str::FromStr>(flag: &str, v: &str, kind: &str) -> Result<T, Fail> {
    v.trim()
        .parse()
        .map_err(|_| Fail::usage(format!("trail-contrast: {flag} takes {kind}, not '{v}'")))
}

/// Read `trail-contrast`'s arguments.
///
/// # Errors
/// A usage error (exit 2): an unknown option, a flag with no or a malformed
/// value, no frame or more than one.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let find = take_values(&mut a, "--find")?;
    let cells = take_values(&mut a, "--cells")?;
    let rows = take_values(&mut a, "--row")?
        .iter()
        .map(|v| number("--row", v, "a row number"))
        .collect::<Result<Vec<i64>, Fail>>()?;
    let float = |a: &mut Vec<String>, flag: &str| -> Result<Option<f64>, Fail> {
        take_value(a, flag)?
            .map(|v| number(flag, &v, "a ratio"))
            .transpose()
    };
    let out = Args {
        find,
        cells,
        rows,
        dims: take_value(&mut a, "--dims")?,
        origin: take_value(&mut a, "--origin")?,
        cell: take_value(&mut a, "--cell")?,
        text: take_value(&mut a, "--text")?,
        clean: take_value(&mut a, "--clean")?,
        no_clean: take_flag(&mut a, "--no-clean"),
        cursor: take_value(&mut a, "--cursor")?,
        caret: take_value(&mut a, "--caret")?,
        page: take_value(&mut a, "--page")?,
        newest: take_value(&mut a, "--newest")?
            .map(|v| number("--newest", &v, "a count"))
            .transpose()?
            .unwrap_or(3),
        min: float(&mut a, "--min")?,
        newest_min: float(&mut a, "--newest-min")?,
        caret_min: float(&mut a, "--caret-min")?,
        png: String::new(),
    };
    if let Some(flag) = a.iter().find(|x| x.starts_with('-') && x.len() > 1) {
        return Err(Fail::usage(format!(
            "trail-contrast: unknown option '{flag}' (trail-contrast --help)"
        )));
    }
    match a.as_slice() {
        [png] => Ok(Args {
            png: png.clone(),
            ..out
        }),
        [] => Err(Fail::usage(
            "trail-contrast: name the frame to measure (trail-contrast --help)",
        )),
        [_, extra, ..] => Err(Fail::usage(format!(
            "trail-contrast: one frame at a time, not also '{extra}'"
        ))),
    }
}

/// A measured selector: what asked for it, where, and the glyph there.
struct Target {
    selector: String,
    row: i64,
    col: i64,
    ch: Option<char>,
    width: i64,
}

/// The report for `a`, and whether a threshold failed.
///
/// # Errors
/// A file that cannot be read or decoded, a grid that does not fit the frame,
/// or a selector the frame cannot answer.
pub fn report(a: &Args) -> Result<J, String> {
    let png = PathBuf::from(from_caller(&a.png));
    let frame = Image::read(&png)?;
    let (width, height) = (
        i64::try_from(frame.width).unwrap_or(i64::MAX),
        i64::try_from(frame.height).unwrap_or(i64::MAX),
    );

    let grid = if a.origin.is_some() || a.cell.is_some() {
        let (Some(origin), Some(cell)) = (&a.origin, &a.cell) else {
            return Err("--origin and --cell go together".into());
        };
        let (ox, oy) = parse_pair(origin, ',', "--origin")?;
        let (cw, ch) = parse_pair(cell, 'x', "--cell")?;
        if cw <= 0 || ch <= 0 {
            return Err("--cell must be positive".into());
        }
        Grid {
            ox,
            oy,
            cw,
            ch,
            rows: (height - oy).div_euclid(ch),
            cols: (width - ox).div_euclid(cw),
            source: "args",
        }
    } else {
        let dims = a
            .dims
            .as_deref()
            .map(|d| PathBuf::from(from_caller(d)))
            .or_else(|| sidecar(&png, ".dims.txt", false))
            .ok_or("no grid: pass --dims FILE or --origin X,Y --cell WxH")?;
        grid_from_dims(&read_text(&dims)?)?
    };
    let (right, bottom) = (grid.ox + grid.cols * grid.cw, grid.oy + grid.rows * grid.ch);
    if right > width || bottom > height {
        return Err(format!(
            "the {}x{} grid ends at {right},{bottom}, outside the {width}x{height} frame",
            grid.cols, grid.rows
        ));
    }

    let clean = if a.no_clean {
        None
    } else {
        let path = a
            .clean
            .as_deref()
            .map(|c| PathBuf::from(from_caller(c)))
            .or_else(|| sidecar(&png, ".clean.png", true));
        match path {
            Some(p) => {
                let c = Image::read(&p)?;
                if (c.width, c.height) != (frame.width, frame.height) {
                    return Err(format!(
                        "the clean twin is {}x{}, the frame {}x{}",
                        c.width, c.height, frame.width, frame.height
                    ));
                }
                Some(c)
            }
            None => None,
        }
    };

    let text_path = a
        .text
        .as_deref()
        .map(|t| PathBuf::from(from_caller(t)))
        .or_else(|| sidecar(&png, ".text.txt", true));
    let lines: Option<Vec<String>> = match text_path {
        Some(p) => Some(read_text(&p)?.split('\n').map(str::to_string).collect()),
        None => None,
    };
    let glyphs = lines.as_deref().map(screen_cells).unwrap_or_default();
    let by_cell: HashMap<(i64, i64), (char, i64)> = glyphs
        .iter()
        .map(|g| ((g.row, g.col), (g.ch, g.width)))
        .collect();
    // The right halves of wide glyphs. A cell that is neither a glyph nor one of
    // these is blank: the `text` reply trims each line's trailing spaces.
    let wide_tails: HashSet<(i64, i64)> = glyphs
        .iter()
        .filter(|g| g.width == 2)
        .map(|g| (g.row, g.col + 1))
        .collect();

    let mut caret_facts: Vec<(String, J)> = Vec::new();
    let caret = if let Some(c) = &a.caret {
        Some(parse_pair(c, ',', "--caret")?)
    } else {
        let path = a
            .cursor
            .as_deref()
            .map(|c| PathBuf::from(from_caller(c)))
            .or_else(|| sidecar(&png, ".cursor.txt", true));
        match path {
            None => None,
            Some(p) => {
                // `OK <row> <col> <visible> <shape>`
                let reply = read_text(&p)?;
                let words: Vec<&str> = reply.split_whitespace().collect();
                if words.len() >= 3 && words[0] == "OK" {
                    let unreadable = || format!("{}: not a cursor reply", p.display());
                    let at = (
                        words[1].parse().map_err(|_| unreadable())?,
                        words[2].parse().map_err(|_| unreadable())?,
                    );
                    if words.len() >= 5 {
                        caret_facts.push(("visible".into(), J::Bool(words[3] == "1")));
                        caret_facts.push(("shape".into(), J::s(words[4])));
                    }
                    Some(at)
                } else {
                    None
                }
            }
        }
    };

    let page = match &a.page {
        Some(p) => parse_hex(p)?,
        None => frame.most_common(),
    };

    let mut targets: Vec<Target> = Vec::new();
    for needle in &a.find {
        let Some(lines) = &lines else {
            return Err(
                "--find needs the frame's screen text (--text, or a .text.txt sidecar)".into(),
            );
        };
        let found = find_cells(lines, needle);
        if found.is_empty() {
            return Err(format!(
                "{} is not on the screen",
                crate::util::py_repr(needle)
            ));
        }
        targets.extend(found.into_iter().map(|g| Target {
            selector: format!("find:{needle}"),
            row: g.row,
            col: g.col,
            ch: Some(g.ch),
            width: g.width,
        }));
    }
    for spec in &a.cells {
        let parsed = (|| {
            let (row, span) = spec.split_once(':')?;
            let (first, last) = span.split_once('-').unwrap_or((span, span));
            let digits = |s: &str| {
                (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse::<i64>().ok())
                    .flatten()
            };
            Some((digits(row)?, digits(first)?, digits(last)?))
        })();
        let Some((row, first, last)) = parsed else {
            return Err(format!(
                "--cells wants ROW:COL or ROW:COL-COL, got {}",
                crate::util::py_repr(spec)
            ));
        };
        for col in first..=last {
            let (ch, width) = by_cell
                .get(&(row, col))
                .map_or((None, 1), |&(c, w)| (Some(c), w));
            targets.push(Target {
                selector: format!("cells:{spec}"),
                row,
                col,
                ch,
                width,
            });
        }
    }
    for &row in &a.rows {
        if lines.is_none() {
            targets.extend((0..grid.cols).map(|col| Target {
                selector: format!("row:{row}"),
                row,
                col,
                ch: None,
                width: 1,
            }));
        } else {
            targets.extend(
                glyphs
                    .iter()
                    .filter(|g| g.row == row && !g.ch.is_whitespace())
                    .map(|g| Target {
                        selector: format!("row:{row}"),
                        row: g.row,
                        col: g.col,
                        ch: Some(g.ch),
                        width: g.width,
                    }),
            );
        }
    }
    if a.find.is_empty() && a.cells.is_empty() && a.rows.is_empty() {
        if lines.is_none() {
            return Err(
                "nothing to measure: pass --find, --cells or --row, or give the screen text".into(),
            );
        }
        targets.extend(
            glyphs
                .iter()
                .filter(|g| !g.ch.is_whitespace())
                .map(|g| Target {
                    selector: "screen".into(),
                    row: g.row,
                    col: g.col,
                    ch: Some(g.ch),
                    width: g.width,
                }),
        );
    }

    let mut cells = Vec::with_capacity(targets.len());
    let mut measured: Vec<f64> = Vec::new();
    for t in &targets {
        if !grid.holds(t.row, t.col) {
            return Err(format!(
                "cell {},{} is outside the {}x{} grid",
                t.row, t.col, grid.cols, grid.rows
            ));
        }
        let mut entry = vec![
            ("selector".to_string(), J::s(t.selector.clone())),
            ("row".to_string(), J::Int(t.row)),
            ("col".to_string(), J::Int(t.col)),
        ];
        if let Some(c) = t.ch {
            entry.push(("char".into(), J::s(c.to_string())));
        }
        entry.extend(measure_cell(
            &frame,
            clean.as_ref(),
            &grid,
            t.row,
            t.col,
            t.width,
        ));
        let cell = J::Obj(entry);
        if let Some(c) = cell.get("contrast").and_then(J::as_f64) {
            measured.push(c);
        }
        cells.push(cell);
    }

    let mut out = J::obj([
        ("image", J::s(abspath(&a.png))),
        ("size", J::Arr(vec![J::Int(width), J::Int(height)])),
        ("grid", grid.describe()),
        ("page", J::s(hex(page))),
        (
            "clean",
            clean
                .as_ref()
                .map_or(J::Null, |c| J::s(abspath(&c.path.to_string_lossy()))),
        ),
    ]);
    let min_contrast = measured.iter().copied().reduce(f64::min);
    let low_cells = a
        .min
        .map_or(0, |m| measured.iter().filter(|&&c| c < m).count());
    out.set("cells", J::Arr(cells));
    out.set("min_contrast", min_contrast.map_or(J::Null, J::Float));

    let mut caret_box = None;
    let mut caret_report = J::Null;
    let mut newest_contrasts: Vec<Option<f64>> = Vec::new();
    if let Some((row, col)) = caret
        && grid.holds(row, col)
    {
        let mut c = measure_caret(&frame, &grid, row, col, page);
        for (k, v) in caret_facts {
            c.set(&k, v);
        }
        let mut newest = Vec::new();
        let mut at = col - 1;
        let want = usize::try_from(a.newest.max(0)).unwrap_or(0);
        while at >= 0 && newest.len() < want {
            if wide_tails.contains(&(row, at)) {
                // The right half of a wide glyph: measure from its left cell.
                at -= 1;
                continue;
            }
            let (ch, width) = by_cell
                .get(&(row, at))
                .map_or((None, 1), |&(c, w)| (Some(c), w));
            if lines.is_some() && ch.is_none_or(char::is_whitespace) {
                break;
            }
            let mut entry = vec![
                ("row".to_string(), J::Int(row)),
                ("col".to_string(), J::Int(at)),
            ];
            if let Some(c) = ch {
                entry.push(("char".into(), J::s(c.to_string())));
            }
            entry.extend(measure_cell(&frame, clean.as_ref(), &grid, row, at, width));
            let entry = J::Obj(entry);
            if entry.get("blank").is_some() {
                break;
            }
            newest_contrasts.push(entry.get("contrast").and_then(J::as_f64));
            newest.push(entry);
            at -= 1;
        }
        c.set("newest", J::Arr(newest));
        caret_report = c;
        caret_box = Some(grid.cell_box(row, col, 1));
    }
    let caret_ratio = caret_report.get("contrast").and_then(J::as_f64);
    let had_caret = !matches!(caret_report, J::Null);
    out.set("caret", caret_report);
    if let Some(clean) = &clean {
        out.set("diff_vs_clean", diff_vs_clean(&frame, clean, caret_box));
    }

    let mut failures = Vec::new();
    if let Some(m) = a.min
        && low_cells > 0
    {
        failures.push(J::s(format!(
            "{low_cells} measured glyph cell(s) under {m:.2}:1"
        )));
    }
    if let Some(m) = a.newest_min {
        let low = newest_contrasts
            .iter()
            .filter(|c| c.unwrap_or(0.0) < m)
            .count();
        if newest_contrasts.is_empty() || low > 0 {
            failures.push(J::s(format!(
                "{low} of {} newest glyph cell(s) under {m:.2}:1",
                newest_contrasts.len()
            )));
        }
    }
    if let Some(m) = a.caret_min {
        let ratio = if had_caret { caret_ratio } else { None };
        if ratio.is_none_or(|r| r < m) {
            let said = ratio.map_or_else(|| "None".to_string(), crate::pyjson::py_float);
            failures.push(J::s(format!("caret {said} under {m:.2}:1")));
        }
    }
    out.set("failures", J::Arr(failures));
    Ok(out)
}

/// A value as compact JSON, non-ASCII kept (`json.dumps(v, ensure_ascii=False)`).
fn compact(v: &J) -> String {
    v.dump(Style {
        indent: None,
        compact: false,
        ascii: false,
    })
}

/// The report as JSON with one line per key and one per measured cell, so it
/// reads, diffs and greps: the report and its caret open up, cell lists put
/// one cell on a line, and everything else is written compact.
#[must_use]
pub fn render(v: &J, depth: usize) -> String {
    let indent = " ".repeat(depth + 1);
    match v {
        J::Obj(m) if depth == 0 || v.get("newest").is_some() => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, item)| {
                    format!(
                        "{indent}{}: {}",
                        J::s(k.as_str()).py(),
                        render(item, depth + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{}}}", items.join(",\n"), " ".repeat(depth))
        }
        J::Arr(a) if !a.is_empty() && a.iter().all(|i| matches!(i, J::Obj(_))) => {
            let items: Vec<String> = a
                .iter()
                .map(|i| format!("{indent}{}", compact(i)))
                .collect();
            format!("[\n{}\n{}]", items.join(",\n"), " ".repeat(depth))
        }
        _ => compact(v),
    }
}

/// `trail-contrast`: print the report; exit 1 when a threshold failed.
///
/// # Errors
/// A usage, file or decoding error (exit 2).
pub fn cmd_trail_contrast(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let out = report(&a).map_err(|e| Fail::usage(format!("trail-contrast: {e}")))?;
    println!("{}", render(&out, 0));
    let failed = out
        .get("failures")
        .and_then(J::as_arr)
        .is_some_and(|f| !f.is_empty());
    Ok(i32::from(failed))
}

#[cfg(test)]
#[path = "trail_contrast_tests.rs"]
mod tests;
