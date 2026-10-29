// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-hue <path>...`: the owner's hue rulings read off a trail gallery's frames against their clean twins, as JSON.
//!
//! Two rulings name a style's colour family: Fire is never green, and Sparkle's shower stays in the gold family.
//! Each frame `trail-gallery` photographs keeps its clean twin (`<stem>.<frame>.clean.png`, the same screen with the
//! trail off) and its `trail status` row (`resolved=` names the style that drew it). The verb counts, per frame of
//! Fire or Sparkle, the pixels the trail turned to a hue its ruling forbids: lit (a channel off its twin's), saturated
//! enough to read as a hue, outside the caret's cell, the word icons and the companion's bodies, and not the twin's own
//! hue (a teal page the glow only lifted keeps its hue, and is not the trail's colour). A pixel reads as a hue only
//! when a viewer can see one in it: its CIELAB chroma is [`CHROMA_MIN`] or more (looks10 round 2: the dim edges where
//! a flame or the gold rail fades into Solarized Dark's teal are tinted greys, C* 9 to 17, less colourful than the
//! page itself, and read as no hue at 1x). The check looks10's judges ran by hand (`greenlit.py`), now a tool
//! (looks10 WIDEN).

use std::path::{Path, PathBuf};

use crate::instance::{take_value, take_values};
use crate::paths::from_caller;
use crate::pyjson::J;
use crate::trail_companion::{Span, frames_under, status_box, status_boxes, status_field};
use crate::trail_contrast::{
    Image, PxBox, Rgb, caret_cell, compact, grid_from_dims, read_text, sidecar,
};
use crate::{Fail, Res};

/// A channel off its clean twin's by more than this marks a pixel the trail drew.
pub const LIT: u8 = 24;
/// The least HSV saturation that reads as a hue rather than a grey.
pub const SAT_MIN: f64 = 0.2;
/// The least HSV value that reads as a hue rather than black.
pub const VAL_MIN: f64 = 0.12;
/// The least CIELAB chroma (C*ab, D65) at which a viewer names a colour by its hue rather than as a tinted grey: the
/// visibility floor. Solarized Dark's own teal page is C* 14.5; an olive flame fringe a viewer sees as green is 28.
pub const CHROMA_MIN: f64 = 20.0;
/// A lit pixel within this many degrees of its twin's own hue (a saturated twin) keeps the page's colour.
pub const OWN_HUE: f64 = 12.0;
/// Green as the eye names it, degrees `[from, to)`: what Fire never is.
pub const GREEN: (f64, f64) = (66.0, 180.0);
/// The gold family, degrees `[from, to)`: amber through gold to yellow, where Sparkle's shower stays.
pub const GOLD: (f64, f64) = (20.0, 66.0);

/// The verb's own help (`trail-hue --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-hue <path>... [--min-px N] [--max N] [--list all|hits]

The hue rulings, as JSON: Fire is never green, and Sparkle's shower stays in the gold family. Every frame under each
<path> (a trail-gallery directory, walked, or one frame's <stem>.<frame>.png) whose `trail status` sidecar
(<stem>.<frame>.status.txt) names resolved=fire or resolved=sparkle is read against its clean twin
(<stem>.<frame>.clean.png); other styles are counted in skipped. A pixel counts when the trail lit it (a channel off
the twin's by more than 24), it reads as a hue a viewer can see (HSV saturation 0.2 or more, value 0.12 or more, and
CIELAB chroma 20 or more: a less colourful pixel reads as a tinted grey), it lies outside the
caret's cell (<stem>.<frame>.cursor.txt on the grid of <stem>.dims.txt), the word icons (word_icons=) and the
companion's bodies (pet_body=, head_body=), it is not the twin's own hue (a saturated twin within 12 degrees), and its
hue is one the style's ruling forbids: green (66..180 degrees) for Fire, anything outside gold (20..66) for Sparkle.
A frame is off its family when N or more pixels count (--min-px, default 1); off_family counts those frames.
--list hits (the default) lists them, all lists every frame read.

exit: 0; 1 off_family is over --max N; 2 a usage, file or parse error
";

/// A colour's HSV: hue in degrees `[0, 360)`, saturation and value in `[0, 1]`.
#[must_use]
pub fn hsv(rgb: Rgb) -> (f64, f64, f64) {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d <= 1e-9 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let s = if mx > 0.0 { d / mx } else { 0.0 };
    (h * 60.0, s, mx)
}

/// A colour's CIELAB chroma C*ab (sRGB, D65 white).
#[must_use]
pub fn lab_chroma(rgb: Rgb) -> f64 {
    let lin = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = rgb.map(lin);
    let x = (0.412_4 * r + 0.357_6 * g + 0.180_5 * b) / 0.950_47;
    let y = 0.212_6 * r + 0.715_2 * g + 0.072_2 * b;
    let z = (0.019_3 * r + 0.119_2 * g + 0.950_5 * b) / 1.088_83;
    let f = |t: f64| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (500.0 * (fx - fy)).hypot(200.0 * (fy - fz))
}

/// The angle between two hues, degrees `[0, 180]`.
#[must_use]
pub fn hue_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// A style's hue ruling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ruling {
    /// Fire is never green.
    FireNeverGreen,
    /// Sparkle's shower stays in the gold family.
    SparkleGold,
}

impl Ruling {
    /// The ruling for the style a status's `resolved=` names, if it has one.
    #[must_use]
    pub fn of_style(style: &str) -> Option<Ruling> {
        match style.trim_matches('"') {
            "fire" => Some(Ruling::FireNeverGreen),
            "sparkle" => Some(Ruling::SparkleGold),
            _ => None,
        }
    }

    /// Whether the ruling forbids `hue` (degrees).
    #[must_use]
    pub fn forbids(self, hue: f64) -> bool {
        let within = |(from, to): (f64, f64)| (from..to).contains(&hue);
        match self {
            Ruling::FireNeverGreen => within(GREEN),
            Ruling::SparkleGold => !within(GOLD),
        }
    }

    /// The style the ruling is for.
    #[must_use]
    pub const fn style(self) -> &'static str {
        match self {
            Ruling::FireNeverGreen => "fire",
            Ruling::SparkleGold => "sparkle",
        }
    }
}

/// What one frame's trail drew off its family.
#[derive(Clone, Debug, PartialEq)]
pub struct Off {
    /// The pixels that count.
    pub px: i64,
    /// Their bounding box (end exclusive), when any.
    pub bbox: Option<PxBox>,
    /// Their hues in 10-degree bins.
    pub bins: [i64; 36],
}

impl Default for Off {
    fn default() -> Off {
        Off {
            px: 0,
            bbox: None,
            bins: [0; 36],
        }
    }
}

/// Whether `(x, y)` lies in any of `masks` (window px, end exclusive).
fn masked(masks: &[Span], x: i64, y: i64) -> bool {
    masks
        .iter()
        .any(|&(x0, x1, y0, y1)| (x0..x1).contains(&x) && (y0..y1).contains(&y))
}

/// The pixels of `frame` its trail turned to a hue `ruling` forbids, read against its `clean` twin, outside `masks`.
#[must_use]
pub fn judge(frame: &Image, clean: &Image, ruling: Ruling, masks: &[Span]) -> Off {
    judge_seen(frame, clean, ruling, masks, CHROMA_MIN)
}

/// [`judge`] with the visibility floor at `chroma_min` (CIELAB C*ab): a pixel less colourful reads as no hue.
#[must_use]
pub fn judge_seen(
    frame: &Image,
    clean: &Image,
    ruling: Ruling,
    masks: &[Span],
    chroma_min: f64,
) -> Off {
    let mut off = Off::default();
    for y in 0..frame.height.min(clean.height) {
        for x in 0..frame.width.min(clean.width) {
            let (a, b) = (frame.at(x, y), clean.at(x, y));
            if (0..3).all(|i| a[i].abs_diff(b[i]) <= LIT) {
                continue;
            }
            let (xi, yi) = (
                i64::try_from(x).unwrap_or(i64::MAX),
                i64::try_from(y).unwrap_or(i64::MAX),
            );
            if masked(masks, xi, yi) {
                continue;
            }
            let (h, s, v) = hsv(a);
            if s < SAT_MIN || v < VAL_MIN || !ruling.forbids(h) || lab_chroma(a) < chroma_min {
                continue;
            }
            let (hc, sc, vc) = hsv(b);
            if sc >= SAT_MIN && vc >= VAL_MIN && hue_gap(h, hc) <= OWN_HUE {
                continue;
            }
            off.px += 1;
            off.bins[((h / 10.0) as usize).min(35)] += 1;
            off.bbox = Some(off.bbox.map_or(
                PxBox {
                    x0: xi,
                    y0: yi,
                    x1: xi + 1,
                    y1: yi + 1,
                },
                |b| PxBox {
                    x0: b.x0.min(xi),
                    y0: b.y0.min(yi),
                    x1: b.x1.max(xi + 1),
                    y1: b.y1.max(yi + 1),
                },
            ));
        }
    }
    off
}

/// One frame read.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRead {
    /// The frame's path.
    pub png: PathBuf,
    /// The ruling its style keeps.
    pub ruling: Ruling,
    /// What its trail drew off that family.
    pub off: Off,
}

/// The masks a frame's sidecars name: the caret's cell, the word icons and the companion's bodies.
fn masks_of(png: &Path, status: &str) -> Result<Vec<Span>, String> {
    let at = |e: String| format!("{}: {e}", png.display());
    let mut masks: Vec<Span> = status_boxes(status, "word_icons")
        .map_err(at)?
        .unwrap_or_default();
    for key in ["pet_body", "head_body"] {
        masks.extend(status_box(status, key).map_err(at)?);
    }
    let (caret, _) = caret_cell(None, sidecar(png, ".cursor.txt", true))?;
    if let Some((row, col)) = caret
        && let Some(dims) = sidecar(png, ".dims.txt", false)
    {
        let grid = grid_from_dims(&read_text(&dims)?)?;
        let b = grid.cell_box(row, col, 1);
        masks.push((b.x0, b.x1, b.y0, b.y1));
    }
    Ok(masks)
}

/// Read the frame at `png`: `Ok(None)` when it has no `trail status` sidecar or clean twin, or its style keeps no
/// hue ruling.
/// # Errors
/// A sidecar or a PNG that cannot be read, decoded or parsed.
pub fn read_frame(png: &Path) -> Result<Option<FrameRead>, String> {
    let Some(status_path) = sidecar(png, ".status.txt", true) else {
        return Ok(None);
    };
    let status = read_text(&status_path)?;
    let Some(ruling) = status_field(&status, "resolved").and_then(Ruling::of_style) else {
        return Ok(None);
    };
    let Some(clean_path) = sidecar(png, ".clean.png", true) else {
        return Ok(None);
    };
    let (frame, clean) = (Image::read(png)?, Image::read(&clean_path)?);
    let masks = masks_of(png, &status)?;
    Ok(Some(FrameRead {
        png: png.to_path_buf(),
        ruling,
        off: judge(&frame, &clean, ruling, &masks),
    }))
}

fn frame_json(f: &FrameRead, root: &Path) -> J {
    let name = f
        .png
        .strip_prefix(root)
        .unwrap_or(&f.png)
        .to_string_lossy()
        .into_owned();
    let mut bins: Vec<(usize, i64)> = f
        .off
        .bins
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, n)| n > 0)
        .collect();
    bins.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    J::obj([
        ("frame", J::s(name)),
        ("style", J::s(f.ruling.style())),
        ("px", J::Int(f.off.px)),
        (
            "bbox",
            f.off.bbox.map_or(J::Null, |b| {
                J::Arr(vec![J::Int(b.x0), J::Int(b.x1), J::Int(b.y0), J::Int(b.y1)])
            }),
        ),
        (
            "hues",
            J::Arr(
                bins.iter()
                    .take(4)
                    .map(|&(k, n)| {
                        J::Arr(vec![J::Int(i64::try_from(k * 10).unwrap_or(0)), J::Int(n)])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// What `trail-hue` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub paths: Vec<String>,
    pub min_px: i64,
    pub max: Option<i64>,
    pub list_all: bool,
}

/// # Errors
/// An unknown flag, a malformed number, or no path.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut rest = args.to_vec();
    let int = |v: Option<String>, flag: &str| -> Result<Option<i64>, Fail> {
        v.map(|v| {
            v.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| Fail::usage(format!("{flag} wants a count, got {v}")))
        })
        .transpose()
    };
    let min_px = int(take_value(&mut rest, "--min-px")?, "--min-px")?
        .unwrap_or(1)
        .max(1);
    let max = int(take_value(&mut rest, "--max")?, "--max")?;
    let list_all = match take_values(&mut rest, "--list")?.last().map(String::as_str) {
        None | Some("hits") => false,
        Some("all") => true,
        Some(v) => return Err(Fail::usage(format!("--list wants all or hits, got {v}"))),
    };
    if let Some(flag) = rest.iter().find(|a| a.starts_with("--")) {
        return Err(Fail::usage(format!("trail-hue: unknown flag {flag}")));
    }
    if rest.is_empty() {
        return Err(Fail::usage(
            "trail-hue: name a gallery directory or a frame",
        ));
    }
    Ok(Args {
        paths: rest,
        min_px,
        max,
        list_all,
    })
}

/// The report over every frame `a` names.
/// # Errors
/// A path that cannot be walked, or a frame whose sidecars or PNGs cannot be read.
pub fn report(a: &Args) -> Result<J, String> {
    let (mut frames, mut skipped, mut off_family) = (0i64, 0i64, 0i64);
    // (frames read, pixels off, frames off) per ruling: Fire, then Sparkle.
    let mut by = [(0i64, 0i64, 0i64); 2];
    let mut listed = Vec::new();
    for p in &a.paths {
        let root = PathBuf::from(from_caller(p));
        let base = if root.is_dir() {
            root.clone()
        } else {
            root.parent().map_or_else(PathBuf::new, Path::to_path_buf)
        };
        for png in frames_under(&root)? {
            let Some(f) = read_frame(&png)? else {
                skipped += 1;
                continue;
            };
            frames += 1;
            let k = usize::from(f.ruling == Ruling::SparkleGold);
            let off = f.off.px >= a.min_px;
            by[k].0 += 1;
            by[k].1 += f.off.px;
            by[k].2 += i64::from(off);
            off_family += i64::from(off);
            if a.list_all || off {
                listed.push(frame_json(&f, &base));
            }
        }
    }
    let mut failures = Vec::new();
    if let Some(max) = a.max
        && off_family > max
    {
        failures.push(J::s(format!("off_family {off_family} > --max {max}")));
    }
    Ok(J::obj([
        ("frames", J::Int(frames)),
        ("skipped", J::Int(skipped)),
        ("off_family", J::Int(off_family)),
        ("min_px", J::Int(a.min_px)),
        ("fire_frames", J::Int(by[0].0)),
        ("fire_green_px", J::Int(by[0].1)),
        ("fire_green_frames", J::Int(by[0].2)),
        ("sparkle_frames", J::Int(by[1].0)),
        ("sparkle_off_gold_px", J::Int(by[1].1)),
        ("sparkle_off_gold_frames", J::Int(by[1].2)),
        ("listed", J::Arr(listed)),
        ("failures", J::Arr(failures)),
    ]))
}

/// The report with one listed frame per line.
#[must_use]
pub fn render(v: &J) -> String {
    let J::Obj(m) = v else {
        return compact(v);
    };
    let items: Vec<String> = m
        .iter()
        .map(|(k, item)| match item {
            J::Arr(a) if !a.is_empty() && k == "listed" => {
                let rows: Vec<String> = a.iter().map(|i| format!("  {}", compact(i))).collect();
                format!(" {}: [\n{}\n ]", J::s(k.as_str()).py(), rows.join(",\n"))
            }
            _ => format!(" {}: {}", J::s(k.as_str()).py(), compact(item)),
        })
        .collect();
    format!("{{\n{}\n}}", items.join(",\n"))
}

/// `trail-hue`: print the report; exit 1 when it is over `--max`.
/// # Errors
/// A usage, file or parse error (exit 2).
pub fn cmd_trail_hue(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let out = report(&a).map_err(|e| Fail::usage(format!("trail-hue: {e}")))?;
    println!("{}", render(&out));
    let failed = out
        .get("failures")
        .and_then(J::as_arr)
        .is_some_and(|f| !f.is_empty());
    Ok(i32::from(failed))
}

#[cfg(test)]
#[path = "trail_hue_tests.rs"]
mod tests;
