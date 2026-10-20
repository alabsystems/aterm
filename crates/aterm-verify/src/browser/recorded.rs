// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The recorded-frame differential (docs/AUDIT-third-party-2026-10-15.md,
//! wasm-platform WP.8): every frame the GPU page presents, held to the frame
//! the wgpu-WebGL module presented for the same content in the same browser.
//! That module's frames were recorded before WP.8 replaced wgpu's WebGL2
//! backend, and are committed in `crates/aterm-verify/browser/wgpu-webgl/`
//! (its README says how and where).
//!
//! A recording is a directory: `recording.json` (the module, the browser,
//! the WebGL2 adapter, and each frame's name, size and the SHA-256 of its
//! RGBA8 bytes) and one RGBA8 PNG per frame. Every session writes the
//! frames it saw to `<work>/<backend>/gpu-frames/` in that layout
//! ([`write`]), so a run's frames can be looked at, and the recording was
//! made by copying one (then `<work>/gpu-frames/`: the lane ran Metal alone).
//!
//! [`judge`]: on the recording's adapter each frame must be byte-identical
//! to its recorded twin; on another adapter, within the CPU parity tolerance
//! ([`super::judge::TOLERANCE`]), and the verdict says which. NEGATIVE
//! CONTROL: each frame after the first of a sequence must lie outside that
//! tolerance of the recorded frame before it, so a comparison that cannot
//! tell frames apart, or a page that stopped presenting, cannot pass.

use std::path::{Path, PathBuf};

use aterm_json::{Value, json};

use super::judge::{Frame, TOLERANCE, Verdict, max_byte_delta};

/// The committed recording of the wgpu-WebGL module, beside this crate.
pub(crate) fn wgpu_webgl_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("browser/wgpu-webgl")
}

/// The manifest's file name.
const MANIFEST: &str = "recording.json";

/// The largest frame side a recording may hold (the PNG decode's bound).
const MAX_SIDE: u32 = 4096;

/// What one session recorded: who drew the frames, and the frames by name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recording {
    pub module: String,
    pub browser: String,
    pub adapter: String,
    pub frames: Vec<(String, Frame)>,
}

/// The lowercase hex SHA-256 of `bytes`.
fn sha256(bytes: &[u8]) -> String {
    aterm_digest::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `frame` as an RGBA8 PNG.
fn png(frame: &Frame) -> Result<Vec<u8>, String> {
    let (w, h) = (
        u32::try_from(frame.width).map_err(|_| "a frame wider than u32".to_string())?,
        u32::try_from(frame.height).map_err(|_| "a frame taller than u32".to_string())?,
    );
    let mut out = Vec::new();
    let mut enc = aterm_png::Encoder::new(&mut out, w, h);
    enc.set_color(aterm_png::ColorType::Rgba);
    enc.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = enc
        .write_header()
        .map_err(|e| format!("png header: {e:?}"))?;
    writer
        .write_image_data(&frame.rgba)
        .map_err(|e| format!("png data: {e:?}"))?;
    writer.finish().map_err(|e| format!("png end: {e:?}"))?;
    Ok(out)
}

/// Write `rec` into `dir` (emptied first): the manifest and one PNG per
/// frame.
pub(crate) fn write(dir: &Path, rec: &Recording) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let mut listed = Vec::new();
    for (name, frame) in &rec.frames {
        let path = dir.join(format!("{name}.png"));
        std::fs::write(&path, png(frame)?)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        listed.push(json!({
            "name": name,
            "width": frame.width,
            "height": frame.height,
            "sha256": sha256(&frame.rgba),
        }));
    }
    // One field, and one frame, per line, so a re-recording diffs by frame.
    let ser = |v: &Value| {
        aterm_json::to_string(v).map_err(|e| format!("could not serialize the manifest: {e}"))
    };
    let mut text = String::from("{\n");
    for (k, val) in [
        ("module", &rec.module),
        ("browser", &rec.browser),
        ("adapter", &rec.adapter),
    ] {
        text += &format!("  {}: {},\n", ser(&json!(k))?, ser(&json!(val))?);
    }
    text += "  \"frames\": [\n";
    let rows: Vec<String> = listed
        .iter()
        .map(|f| ser(f).map(|s| format!("    {s}")))
        .collect::<Result<_, _>>()?;
    text += &rows.join(",\n");
    text += "\n  ]\n}\n";
    let path = dir.join(MANIFEST);
    std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Read the recording in `dir`: every frame's PNG decoded and held to the
/// manifest's size and digest.
pub(crate) fn read(dir: &Path) -> Result<Recording, String> {
    let path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let v: Value = aterm_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let text_of = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{} has no `{k}`", path.display()))
    };
    let mut frames = Vec::new();
    for (i, f) in v
        .get("frames")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{} has no `frames`", path.display()))?
        .iter()
        .enumerate()
    {
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .ok_or_else(|| format!("{}: frame {i} has no plain `name`", path.display()))?;
        let dim = |k: &str| {
            f.get(k)
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("{}: frame {name} has no `{k}`", path.display()))
        };
        let (width, height) = (dim("width")?, dim("height")?);
        let digest = f
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{}: frame {name} has no `sha256`", path.display()))?;
        let png_path = dir.join(format!("{name}.png"));
        let bytes = std::fs::read(&png_path)
            .map_err(|e| format!("could not read {}: {e}", png_path.display()))?;
        let (rgba, w, h) = aterm_png::decode_rgba8(&bytes, MAX_SIDE)
            .ok_or_else(|| format!("{} is not an RGBA8 PNG", png_path.display()))?;
        if (w, h) != (width, height) {
            return Err(format!(
                "{} is {w}x{h}, the manifest says {width}x{height}",
                png_path.display()
            ));
        }
        if sha256(&rgba) != digest {
            return Err(format!(
                "{}'s pixels do not hash to the manifest's sha256",
                png_path.display()
            ));
        }
        frames.push((
            name.to_string(),
            Frame {
                width,
                height,
                rgba,
            },
        ));
    }
    Ok(Recording {
        module: text_of("module")?,
        browser: text_of("browser")?,
        adapter: text_of("adapter")?,
        frames,
    })
}

/// A frame name's sequence and index: `diff-3` is (`diff`, 3).
fn sequence(name: &str) -> Option<(&str, usize)> {
    let (seq, k) = name.rsplit_once('-')?;
    Some((seq, k.parse().ok()?))
}

/// Judge `run` (frames by name, drawn on `adapter`) against `rec`.
pub(crate) fn judge(rec: &Recording, adapter: &str, run: &[(String, &Frame)]) -> Verdict {
    let mut v = Verdict::default();
    let exact = adapter == rec.adapter;
    let tolerance = if exact { 0 } else { TOLERANCE };
    let names =
        |frames: &mut dyn Iterator<Item = &String>| -> Vec<String> { frames.cloned().collect() };
    let (want, got) = (
        names(&mut rec.frames.iter().map(|(n, _)| n)),
        names(&mut run.iter().map(|(n, _)| n)),
    );
    if want != got {
        v.failures.push(format!(
            "the run drew frames {got:?}, the recording holds {want:?}"
        ));
        return v;
    }
    v.lines.push(if exact {
        format!(
            "recorded-frame differential against the {} recording (\"{}\"): on the recording's \
             adapter, so every frame must be byte-identical",
            rec.module, rec.adapter
        )
    } else {
        format!(
            "recorded-frame differential against the {} recording: this run's adapter \
             \"{adapter}\" is not the recording's \"{}\", so frames are held within {TOLERANCE}, \
             not byte for byte",
            rec.module, rec.adapter
        )
    });
    for ((name, recorded), (_, frame)) in rec.frames.iter().zip(run) {
        if (recorded.width, recorded.height) != (frame.width, frame.height) {
            v.failures.push(format!(
                "{name}: the canvas is {}x{}, the recorded frame {}x{}",
                frame.width, frame.height, recorded.width, recorded.height
            ));
            continue;
        }
        let delta = max_byte_delta(&recorded.rgba, &frame.rgba);
        let differing = recorded
            .rgba
            .iter()
            .zip(&frame.rgba)
            .filter(|(a, b)| a != b)
            .count();
        v.lines.push(format!(
            "{name}: {}x{}, max byte delta {delta} against the recording ({differing} bytes \
             differ)",
            frame.width, frame.height
        ));
        if delta > tolerance {
            v.failures.push(format!(
                "{name}: differs from the recorded frame by {delta} > {tolerance} \
                 ({differing} bytes)"
            ));
        }
        let Some((seq, k)) = sequence(name).filter(|(_, k)| *k > 0) else {
            continue;
        };
        let before = format!("{seq}-{}", k - 1);
        let Some((_, prev)) = rec.frames.iter().find(|(n, _)| *n == before) else {
            continue;
        };
        let control = if prev.rgba.len() == frame.rgba.len() {
            max_byte_delta(&prev.rgba, &frame.rgba)
        } else {
            u8::MAX
        };
        if control <= tolerance {
            v.failures.push(format!(
                "NEGATIVE CONTROL: {name} lies within {tolerance} of the recorded {before} \
                 ({control}): the differential does not tell the frames apart"
            ));
        }
    }
    if v.failures.is_empty() {
        v.lines.push(format!(
            "negative control held: each frame after a sequence's first differs from the \
             recorded frame before it by more than {tolerance}"
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3x2 frame whose pixel `lit` is white on black.
    fn frame(lit: usize) -> Frame {
        let mut rgba = [0, 0, 0, 255].repeat(6);
        rgba[4 * lit..4 * lit + 3].copy_from_slice(&[255, 255, 255]);
        Frame {
            width: 3,
            height: 2,
            rgba,
        }
    }

    fn recording() -> Recording {
        Recording {
            module: "aterm-gpu-web".to_string(),
            browser: "HeadlessChrome/1".to_string(),
            adapter: "ANGLE (test)".to_string(),
            frames: vec![
                ("parity-0".to_string(), frame(0)),
                ("parity-1".to_string(), frame(1)),
                ("diff-0".to_string(), frame(2)),
                ("diff-1".to_string(), frame(3)),
            ],
        }
    }

    fn run_of(rec: &Recording) -> Vec<(String, &Frame)> {
        rec.frames.iter().map(|(n, f)| (n.clone(), f)).collect()
    }

    /// A recording survives its own round trip through PNG and the manifest,
    /// and a pixel edited in a PNG is refused by its digest.
    #[test]
    fn a_recording_round_trips_and_a_tampered_png_is_refused() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let rec = recording();
        write(dir.path(), &rec).expect("write");
        assert_eq!(read(dir.path()).expect("read"), rec);
        let mut other = frame(2);
        other.rgba[0] = 7;
        std::fs::write(dir.path().join("parity-1.png"), png(&other).expect("png")).expect("plant");
        let err = read(dir.path()).expect_err("a tampered frame");
        assert!(err.contains("do not hash"), "{err}");
    }

    /// On the recording's adapter the frames must match byte for byte; one
    /// byte off fails and names its frame, while another adapter's run is
    /// held within the tolerance and says so.
    #[test]
    fn the_recording_s_adapter_is_held_byte_for_byte_another_within_the_tolerance() {
        let rec = recording();
        let v = judge(&rec, &rec.adapter, &run_of(&rec));
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(v.lines[0].contains("byte-identical"), "{:?}", v.lines);
        let mut off = recording();
        off.frames[2].1.rgba[1] = 1;
        let v = judge(&rec, &rec.adapter, &run_of(&off));
        assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
        assert!(
            v.failures[0].starts_with("diff-0: differs"),
            "{:?}",
            v.failures
        );
        let v = judge(&rec, "SwiftShader", &run_of(&off));
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(v.lines[0].contains("within 8"), "{:?}", v.lines);
    }

    /// A page stuck on a sequence's first frame trips the negative control,
    /// and a run with other frames than the recording's is refused by name.
    #[test]
    fn a_stalled_page_trips_the_control_and_a_missing_frame_is_named() {
        let rec = recording();
        let mut stalled = recording();
        stalled.frames[1].1 = frame(0);
        let v = judge(&rec, &rec.adapter, &run_of(&stalled));
        assert!(
            v.failures
                .iter()
                .any(|f| f.starts_with("NEGATIVE CONTROL: parity-1")),
            "{:?}",
            v.failures
        );
        let short: Vec<(String, &Frame)> = run_of(&rec).into_iter().take(3).collect();
        let v = judge(&rec, &rec.adapter, &short);
        assert!(
            v.failures[0].contains("the recording holds"),
            "{:?}",
            v.failures
        );
    }
}
