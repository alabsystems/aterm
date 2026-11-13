// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline, native-pixel review of every indexed video frame. No glyph or continuity verdict.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use aterm_json::{Value, json};

use crate::instance::take_value;
use crate::system_state_sheet::{self as sheet, Tile};
use crate::trail_contrast::{Grid, Image, PxBox, grid_from_dims};
use crate::{Fail, Res};

const INDEX_BYTES: usize = 4 * 1024 * 1024;
const DIMS_BYTES: usize = 16 * 1024;
const FRAME_BYTES: usize = 96 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
const PAGE_FRAMES: usize = 8;
const ROI_PIXELS: usize = 1024 * 1024;

const HELP: &str = "video-sheet INDEX --dims FILE --rows A:B --cols C:D --before-us US --out NEWDIR [--expect-text TEXT]\n\
Offline native-pixel atlas of ALL indexed frames, including delta=0 and the tail.\n\
Rows/columns are zero-based inclusive. BEFORE means t_us < US on the video's GUI clock.\n\
Each page retains source pixel scale; no OCR, glyph, continuity or latency verdict is made.\n\
Manifest fp is retained but not validated; keep the producer's sealed, byte-identical recording provenance.\n\
Bounds: index 4 MiB, dims 16 KiB, 8192 entries, PNG 96 MiB/4096 px per side, ROI 1 Mi-pixel/1024 px high, 8 frames/page.\n\
Input files must be the user's single-link regular files, with no symlinks in their paths.\n\
Output must be a new directory under an existing parent. Coverage errors are retained in coverage.json.\n";

struct Options {
    index: PathBuf,
    dims: PathBuf,
    rows: (i64, i64),
    cols: (i64, i64),
    before_us: u64,
    out: PathBuf,
    expected: Option<String>,
}

fn required(args: &mut Vec<String>, name: &str) -> Result<String, Fail> {
    take_value(args, name)?.ok_or_else(|| Fail::usage(format!("missing {name}\n{HELP}")))
}

fn range(value: &str) -> Result<(i64, i64), Fail> {
    let parsed = value
        .split_once(':')
        .and_then(|(a, b)| Some((a.parse::<i64>().ok()?, b.parse::<i64>().ok()?)));
    parsed
        .filter(|&(a, b)| a >= 0 && b >= a)
        .ok_or_else(|| Fail::usage(format!("invalid inclusive range {value}")))
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, Fail> {
        let mut args = args.to_vec();
        let path = |s: &str| PathBuf::from(crate::paths::from_caller(s));
        let dims = path(&required(&mut args, "--dims")?);
        let rows = range(&required(&mut args, "--rows")?)?;
        let cols = range(&required(&mut args, "--cols")?)?;
        let before_us = required(&mut args, "--before-us")?
            .parse::<u64>()
            .map_err(|_| Fail::usage("--before-us must be an unsigned integer"))?;
        let out = path(&required(&mut args, "--out")?);
        let expected = take_value(&mut args, "--expect-text")?;
        if expected
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 16 * 1024)
        {
            return Err(Fail::usage("--expect-text must hold 1..16384 UTF-8 bytes"));
        }
        if args.len() != 1 || args[0].starts_with('-') {
            return Err(Fail::usage(HELP));
        }
        Ok(Self {
            index: path(&args[0]),
            dims,
            rows,
            cols,
            before_us,
            out,
            expected,
        })
    }
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let (file, meta) = crate::sys::scratch_file_with_meta(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .ok_or_else(|| format!("{}: missing file", path.display()))?;
    let size = meta.len();
    if size > limit as u64 {
        return Err(format!("{}: exceeds {limit} bytes", path.display()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0).saturating_add(1));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{}: grew past {limit} bytes", path.display()));
    }
    Ok(bytes)
}

fn dims_number(text: &str, name: &str) -> Result<i64, String> {
    dims_number_in(text, name, 1)
}

fn dims_number_in(text: &str, name: &str, low: i64) -> Result<i64, String> {
    let prefix = format!("{name}=");
    let mut values = text
        .split_whitespace()
        .filter_map(|s| s.strip_prefix(&prefix));
    let number = values.next().and_then(|s| s.parse::<i64>().ok());
    if values.next().is_some() {
        return Err(format!("duplicate dims {name}"));
    }
    number
        .filter(|n| (low..=4096).contains(n))
        .ok_or_else(|| format!("invalid dims {name}"))
}

fn crop_box(
    grid: Grid,
    rows: (i64, i64),
    cols: (i64, i64),
    w: i64,
    h: i64,
) -> Result<PxBox, String> {
    let right = grid
        .cols
        .checked_mul(grid.cw)
        .and_then(|n| grid.ox.checked_add(n));
    let bottom = grid
        .rows
        .checked_mul(grid.ch)
        .and_then(|n| grid.oy.checked_add(n));
    if grid.cw <= 0
        || grid.ch <= 0
        || grid.ox < 0
        || grid.oy < 0
        || right.is_none_or(|n| n > w)
        || bottom.is_none_or(|n| n > h)
        || rows.0 < 0
        || rows.1 < rows.0
        || rows.1 >= grid.rows
        || cols.0 < 0
        || cols.1 < cols.0
        || cols.1 >= grid.cols
    {
        return Err("ROI or dims grid is outside the full frame".into());
    }
    let first = grid.cell_box(rows.0, cols.0, 1);
    let last = grid.cell_box(rows.1, cols.1, 1);
    let b = PxBox {
        x0: first.x0,
        y0: first.y0,
        x1: last.x1,
        y1: last.y1,
    };
    let area = usize::try_from(b.x1 - b.x0).ok().and_then(|a| {
        usize::try_from(b.y1 - b.y0)
            .ok()
            .and_then(|b| a.checked_mul(b))
    });
    if area.is_none_or(|n| n == 0 || n > ROI_PIXELS) || b.y1 - b.y0 > 1024 {
        return Err("ROI exceeds 1 Mi-pixel or 1024 pixels high".into());
    }
    Ok(b)
}

fn num(value: &Value, name: &str) -> Result<u64, String> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing/invalid {name}"))
}

fn frame_name(value: &Value) -> Result<&str, String> {
    let file = value
        .get("file")
        .and_then(Value::as_str)
        .ok_or("missing frame file")?;
    let number = file
        .strip_prefix("frame_")
        .and_then(|s| s.strip_suffix(".png"));
    if file.len() > 64
        || !number.is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("frame file is not a simple sibling frame_<digits>.png name".into());
    }
    Ok(file)
}

fn metadata_reasons(
    index: &Value,
    frames: &[Value],
    inputs: &[Value],
    w: i64,
    h: i64,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let meta = index.get("meta");
    let field = |name| meta.and_then(|m| m.get(name));
    if field("mode").and_then(Value::as_str) != Some("swapchain-tap") {
        reasons.push("meta.mode must be swapchain-tap".into());
    }
    for name in ["half_res", "head_truncated", "resized_early_stop"] {
        if field(name).and_then(Value::as_bool) != Some(false) {
            reasons.push(format!("meta.{name} must be false"));
        }
    }
    if !field("fps_cap").is_some_and(Value::is_null) {
        reasons.push("meta.fps_cap must be null".into());
    }
    for name in [
        "dropped_frames",
        "ring_skipped",
        "evicted_frames",
        "decimated_frames",
        "unlogged_inputs",
        "unlogged_other_window",
        "unlogged_over_cap",
    ] {
        if field(name).and_then(Value::as_u64) != Some(0) {
            reasons.push(format!("meta.{name} must be zero"));
        }
    }
    for (name, expected) in [
        ("w", w as u64),
        ("h", h as u64),
        ("frames_written", frames.len() as u64),
        ("inputs_logged", inputs.len() as u64),
    ] {
        if field(name).and_then(Value::as_u64) != Some(expected) {
            reasons.push(format!("meta.{name} disagrees with imported data"));
        }
    }
    if !field("device_px")
        .and_then(Value::as_array)
        .is_some_and(|size| {
            size.len() == 2
                && size[0].as_u64() == Some(w as u64)
                && size[1].as_u64() == Some(h as u64)
        })
    {
        reasons.push("meta.device_px must match the full-resolution frame dimensions".into());
    }
    if field("keys_requested").and_then(Value::as_bool).is_none() {
        reasons.push("missing/invalid meta.keys_requested".into());
    }
    if field("requested_ms")
        .and_then(Value::as_u64)
        .is_none_or(|n| n == 0)
    {
        reasons.push("missing/invalid meta.requested_ms".into());
    }
    if frames.is_empty() {
        reasons.push("no indexed frames".into());
    } else {
        let ends = frames
            .first()
            .and_then(|v| num(v, "t_us").ok())
            .zip(frames.last().and_then(|v| num(v, "t_us").ok()));
        let declared = field("covered_us").and_then(Value::as_array);
        if !ends.zip(declared).is_some_and(|((first, last), declared)| {
            declared.len() == 2
                && declared[0].as_u64() == Some(first)
                && declared[1].as_u64() == Some(last)
        }) {
            reasons.push("meta.covered_us disagrees with the full frame manifest".into());
        }
    }
    reasons
}

fn flush(
    out: &crate::sys::NewOutputDir,
    tiles: &mut Vec<Tile>,
    pages: &mut Vec<String>,
) -> Result<(), String> {
    if tiles.is_empty() {
        return Ok(());
    }
    let name = format!("sheet-{:04}.png", pages.len() + 1);
    let png = sheet::native_png(
        "Video frames at source pixel scale",
        "BEFORE is a clock bound; visual result NOT JUDGED",
        tiles,
    )?;
    out.write(&name, &png).map_err(|e| e.to_string())?;
    pages.push(name);
    tiles.clear();
    Ok(())
}

fn run(options: &Options) -> Result<Value, String> {
    let index: Value = aterm_json::from_slice(&read(&options.index, INDEX_BYTES)?)
        .map_err(|e| format!("invalid index: {e}"))?;
    let frames = index
        .get("frames")
        .and_then(Value::as_array)
        .ok_or("index.frames must be an array")?;
    if frames.len() > MAX_ENTRIES {
        return Err("frame manifest exceeds 8192 entries".into());
    }
    let text = String::from_utf8(read(&options.dims, DIMS_BYTES)?).map_err(|e| e.to_string())?;
    let (w, h) = (
        dims_number(&text, "frame_w")?,
        dims_number(&text, "frame_h")?,
    );
    // Bound the origin addition before the shared parser computes head + pad_top.
    for name in ["pad", "pad_top", "head"] {
        let _ = dims_number_in(&text, name, 0)?;
    }
    for name in ["cell_w", "cell_h"] {
        let _ = dims_number(&text, name)?;
    }
    for name in [
        "tab_rows",
        "offset_x",
        "offset_y",
        "band_left",
        "band_top",
        "crop_left",
        "crop_top",
    ] {
        if dims_number_in(&text, name, 0)? != 0 {
            return Err(format!("unsupported nonzero dims {name}"));
        }
    }
    let roi = crop_box(grid_from_dims(&text)?, options.rows, options.cols, w, h)?;
    let inputs = index.get("inputs").and_then(Value::as_array);
    let empty = Vec::new();
    let mut reasons = metadata_reasons(&index, frames, inputs.unwrap_or(&empty), w, h);
    if inputs.is_none() {
        reasons.push("index.inputs must be an array".into());
    }
    let inputs = inputs.unwrap_or(&empty);
    let mut last_input = None;
    for input in inputs {
        match num(input, "t_us") {
            Ok(t) if last_input.is_none_or(|previous| t >= previous) => last_input = Some(t),
            _ => reasons.push("input timestamps are missing or out of order".into()),
        }
    }
    if let Some(expected) = &options.expected {
        let actual: Option<String> = inputs
            .iter()
            .map(|input| {
                let s = input.get("ch")?.as_str()?;
                (s.chars().count() == 1).then_some(s)
            })
            .collect();
        if actual.as_deref() != Some(expected.as_str()) {
            reasons.push("input ledger does not equal --expect-text".into());
        }
        if index
            .get("meta")
            .and_then(|m| m.get("keys_requested"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            reasons.push("--expect-text requires keys_requested=true".into());
        }
    }
    let parent = options
        .index
        .parent()
        .ok_or("index has no parent directory")?;
    let out = crate::sys::NewOutputDir::create(&options.out).map_err(|e| e.to_string())?;
    let mut rows = Vec::with_capacity(frames.len());
    let (mut decoded, mut before, mut after) = (0usize, 0usize, 0usize);
    let mut tiles = Vec::with_capacity(PAGE_FRAMES);
    let mut pages = Vec::new();
    let mut names = HashSet::new();
    let (mut previous_seq, mut previous_time) = (None, None);
    let (mut first_before, mut last_before) = (None, None);
    for (ordinal, entry) in frames.iter().enumerate() {
        let classified = entry
            .get("t_us")
            .and_then(Value::as_u64)
            .map(|t| t < options.before_us);
        let mut location = None;
        let checked = (|| -> Result<(), String> {
            let n = num(entry, "n")?;
            let seq = num(entry, "seq")?;
            let t = num(entry, "t_us")?;
            let _delta = num(entry, "delta")?;
            let file = frame_name(entry)?;
            if n != ordinal as u64 + 1
                || seq == 0
                || previous_seq.is_some_and(|v| seq <= v)
                || previous_time.is_some_and(|v| t < v)
                || !names.insert(file.to_string())
            {
                return Err("frame order/number/name is duplicated or inconsistent".into());
            }
            previous_seq = Some(seq);
            previous_time = Some(t);
            if index
                .get("meta")
                .and_then(|m| m.get("half_res"))
                .and_then(Value::as_bool)
                != Some(false)
            {
                return Err("native atlas requires known full-resolution frames".into());
            }
            let path = parent.join(file);
            let frame = Image::decode_opaque(&path, &read(&path, FRAME_BYTES)?)?;
            if (frame.width, frame.height) != (w as usize, h as usize) {
                return Err("PNG dimensions differ from full-frame dims".into());
            }
            let crop = frame.crop(roi)?;
            location = Some((format!("sheet-{:04}.png", pages.len() + 1), tiles.len() + 1));
            tiles.push(Tile {
                frame: crop,
                caption: format!(
                    "N{n} SEQ{seq} T_US {t} {}",
                    if t < options.before_us {
                        "BEFORE"
                    } else {
                        "AFTER"
                    }
                ),
                verdicts: vec![],
            });
            decoded += 1;
            if t < options.before_us {
                before += 1;
                first_before.get_or_insert(t);
                last_before = Some(t);
            } else {
                after += 1;
            }
            Ok(())
        })();
        let error = checked.err();
        if let Some(error) = &error {
            reasons.push(format!("frame entry {}: {error}", ordinal + 1));
        }
        rows.push(json!({"ordinal":ordinal + 1,"entry":entry,"before_cutoff":classified,
            "sheet":location.as_ref().map(|p|p.0.as_str()),"tile":location.as_ref().map(|p|p.1),"error":error}));
        if tiles.len() == PAGE_FRAMES {
            flush(&out, &mut tiles, &mut pages)?;
        }
    }
    flush(&out, &mut tiles, &mut pages)?;
    if before == 0 {
        reasons.push("no decoded pre-cutoff frames".into());
    }
    if let Some(first_input) = inputs.first().and_then(|input| num(input, "t_us").ok())
        && first_before.is_none_or(|t| t >= first_input)
    {
        reasons.push("pre-cutoff frames do not include a frame before the first input".into());
    }
    if let Some(last_input) = inputs.last().and_then(|input| num(input, "t_us").ok())
        && last_before.is_none_or(|t| t <= last_input)
    {
        reasons.push("pre-cutoff frames stop before the last logged input".into());
    }
    let report = json!({
        "coverage":if reasons.is_empty(){"all indexed frames represented"}else{"inconclusive"},
        "visual_not_judged":true,"glyph_prefix_not_judged":true,"pixel_scale":1,
        "manifest_fp_validated":false,
        "integrity_scope":"Enumeration and decoding only; manifest fp is not recomputed. Producer seals and byte-identical import provenance are separate receipts.",
        "index":options.index.to_string_lossy().into_owned(),"dims":options.dims.to_string_lossy().into_owned(),
        "roi_source":"external dims, not per-frame terminal/glyph metadata",
        "roi_pixels":{"x0":roi.x0,"y0":roi.y0,"x1":roi.x1,"y1":roi.y1},
        "before_us_exclusive":options.before_us,"frames_manifest":frames.len(),"frames_decoded":decoded,
        "frames_before_cutoff":before,"frames_at_or_after_cutoff":after,"frames_errors":frames.len()-decoded,
        "expected_text":options.expected,"recording_meta":index.get("meta"),"pages":pages,
        "reasons":reasons,"frames":rows,
        "limits":"Coverage is of indexed captures, not whole real-time pixel continuity, glyph latency, compositor visibility or scanout. AFTER frames may include capture-perturbed work."
    });
    let bytes = aterm_json::to_vec(&report).map_err(|e| e.to_string())?;
    out.write("coverage.json", &bytes)
        .map_err(|e| e.to_string())?;
    Ok(report)
}

/// Build an offline atlas without contacting a lab or opening a window.
/// # Errors
/// Bad options, invalid inputs/ROI, or output that cannot be created or written.
pub fn cmd(args: &[String]) -> Res {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    let options = Options::parse(args)?;
    let report = run(&options).map_err(Fail::die)?;
    let inconclusive = report
        .get("reasons")
        .and_then(Value::as_array)
        .is_none_or(|v| !v.is_empty());
    println!(
        "video-sheet: {} decoded frames; visual NOT JUDGED; {}",
        report["frames_decoded"],
        options.out.display()
    );
    Ok(i32::from(inconclusive))
}

#[cfg(test)]
#[path = "video_sheet_tests.rs"]
mod tests;
