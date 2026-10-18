// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lane's verdict: each GPU frame against the CPU frame of the same
//! content, within crates/aterm-gpu-web/tests/cpu_gpu_parity.rs's tolerance,
//! never blank, and a control that the comparison tells frames apart; and
//! the GPU module's `init` synchronous, its throw still rejecting an async
//! caller that awaits it (wasm-platform WP.5).

use aterm_json::Value;

/// cpu_gpu_parity.rs's tolerance: the largest RGBA8 byte delta between the
/// CPU and GPU frames of one grid (round-vs-floor coverage rounding at glyph
/// edges).
pub(crate) const TOLERANCE: u8 = 8;

/// One frame a page handed back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// What the GPU page saw of `init`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Init {
    /// `gpu_ready`, read as `init(canvas)` returned, before anything awaited.
    pub ready_on_return: bool,
    /// An async fn awaiting `init` on a canvas with no WebGL2 rejected.
    pub probe_rejected: bool,
    /// `typeof` what it rejected with: init's `Err` crosses as a string.
    pub probe_thrown: String,
    pub probe_error: String,
}

/// What one page answered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    pub module: String,
    pub adapter: String,
    pub frames: Vec<Frame>,
    /// The GPU page's differential frames (`spec.diff`); the CPU page has
    /// none.
    pub diff: Vec<Frame>,
    /// The GPU page's `render_offscreen()` + `rgba()` of its last frame; the
    /// CPU page has none.
    pub readback: Option<Frame>,
    pub timings: Value,
    /// The GPU page's `init` report; the CPU page has none.
    pub init: Option<Init>,
}

/// The GPU page's `init` report, if the answer carries one.
fn parse_init(v: &Value) -> Result<Option<Init>, String> {
    let Some(init) = v.get("init") else {
        return Ok(None);
    };
    let flag = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("the page's init report has no `{k}`"))
    };
    let probe = init
        .get("probe")
        .ok_or("the page's init report has no `probe`")?;
    let text = |k: &str| {
        probe
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("the page's init probe has no `{k}`"))
    };
    Ok(Some(Init {
        ready_on_return: flag(init, "ready_on_return")?,
        probe_rejected: flag(probe, "rejected")?,
        probe_thrown: text("thrown")?,
        probe_error: text("error")?,
    }))
}

/// `window.atermLane`'s answer, its frames decoded and sized.
pub(crate) fn parse(v: &Value) -> Result<Answer, String> {
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("the page's answer has no `{k}`"))
    };
    let frames = parse_frames(
        v.get("frames")
            .and_then(Value::as_array)
            .ok_or("the page's answer has no `frames`")?,
        "frame",
    )?;
    let diff = match v.get("diff") {
        None => Vec::new(),
        Some(d) => parse_frames(
            d.as_array().ok_or("the page's `diff` is not an array")?,
            "differential frame",
        )?,
    };
    let readback = match v.get("readback") {
        None => None,
        Some(r) => parse_frames(std::slice::from_ref(r), "readback")?.pop(),
    };
    Ok(Answer {
        module: text("module")?,
        adapter: text("adapter")?,
        frames,
        diff,
        readback,
        timings: v.get("timings").cloned().unwrap_or(Value::Null),
        init: parse_init(v)?,
    })
}

/// A page's frames, each decoded and sized; `what` names one in an error.
fn parse_frames(frames: &[Value], what: &str) -> Result<Vec<Frame>, String> {
    frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let dim = |k: &str| {
                f.get(k)
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| format!("{what} {i} has no `{k}`"))
            };
            let (width, height) = (dim("width")?, dim("height")?);
            let rgba = f
                .get("rgba")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{what} {i} has no `rgba`"))
                .and_then(|s| {
                    aterm_codec::base64::decode(s)
                        .map_err(|e| format!("{what} {i}'s rgba is not base64: {e}"))
                })?;
            if width == 0
                || height == 0
                || Some(rgba.len()) != width.checked_mul(height).and_then(|n| n.checked_mul(4))
            {
                return Err(format!(
                    "{what} {i} is {width}x{height} but carries {} bytes",
                    rgba.len()
                ));
            }
            Ok(Frame {
                width,
                height,
                rgba,
            })
        })
        .collect()
}

/// The largest per-byte difference between two equal-length buffers.
pub(crate) fn max_byte_delta(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Pixels of the red glyph the first line draws (cpu_gpu_parity.rs's test:
/// red above 140, green and blue under 90).
pub(crate) fn red_pixels(rgba: &[u8]) -> usize {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|[r, g, b, _]| *r > 140 && *g < 90 && *b < 90)
        .count()
}

/// What the comparison found: the lines it reports and its failures.
#[derive(Debug, Default)]
pub(crate) struct Verdict {
    pub lines: Vec<String>,
    pub failures: Vec<String>,
}

/// Judge the GPU page's `frames` frames against the CPU page's.
///
/// Each GPU frame must have the CPU frame's size, lie within [`TOLERANCE`]
/// of it, and show the red glyph. NEGATIVE CONTROL: each GPU frame after the
/// first must lie OUTSIDE the tolerance of the CPU frame before it (one more
/// line of content), so a comparison that cannot tell two frames apart, or
/// a GPU page that stopped presenting, cannot pass.
pub(crate) fn judge(cpu: &Answer, gpu: &Answer, frames: usize) -> Verdict {
    let mut v = Verdict::default();
    for (who, a) in [("cpu", cpu), ("gpu", gpu)] {
        if a.frames.len() != frames {
            v.failures.push(format!(
                "the {who} page answered {} frames, the lane fed {frames}",
                a.frames.len()
            ));
        }
    }
    if gpu.adapter.is_empty() {
        v.failures
            .push("the GPU page reports no adapter: init(canvas) acquired none".to_string());
    }
    judge_init(gpu.init.as_ref(), &mut v);
    judge_readback(cpu, gpu, &mut v);
    for (k, (c, g)) in cpu.frames.iter().zip(&gpu.frames).enumerate() {
        if (c.width, c.height) != (g.width, g.height) {
            v.failures.push(format!(
                "frame {k}: the GPU canvas is {}x{}, the CPU frame {}x{}",
                g.width, g.height, c.width, c.height
            ));
            continue;
        }
        let delta = max_byte_delta(&c.rgba, &g.rgba);
        let red = red_pixels(&g.rgba);
        v.lines.push(format!(
            "frame {k}: {}x{}, GPU vs CPU max byte delta {delta} (tolerance {TOLERANCE}), {red} \
             red glyph pixels on the GPU frame",
            g.width, g.height
        ));
        if delta > TOLERANCE {
            v.failures.push(format!(
                "frame {k}: the WebGL2 frame differs from the CPU frame by {delta} > {TOLERANCE}"
            ));
        }
        if red == 0 {
            v.failures.push(format!(
                "frame {k}: no red glyph pixels on the GPU frame (a blank or wrong present)"
            ));
        }
        if k > 0 {
            let prev = &cpu.frames[k - 1];
            let control = if prev.rgba.len() == g.rgba.len() {
                max_byte_delta(&prev.rgba, &g.rgba)
            } else {
                u8::MAX
            };
            if control <= TOLERANCE {
                v.failures.push(format!(
                    "NEGATIVE CONTROL: GPU frame {k} lies within {TOLERANCE} of CPU frame {} \
                     ({control}): the comparison does not tell the frames apart",
                    k - 1
                ));
            }
        }
    }
    if v.failures.is_empty() && frames > 1 {
        v.lines.push(format!(
            "negative control held: each GPU frame after the first differs from the CPU frame \
             before it by more than {TOLERANCE}"
        ));
    }
    v
}

/// The harness readback (`render_offscreen()` + `rgba()` after the last
/// frame) holds the last frame's content: within [`TOLERANCE`] of the CPU's
/// last frame, at its size. NEGATIVE CONTROL: outside it of the CPU's frame
/// before, so a readback of a stale or blank offscreen cannot pass.
fn judge_readback(cpu: &Answer, gpu: &Answer, v: &mut Verdict) {
    let Some(rb) = gpu.readback.as_ref() else {
        v.failures
            .push("the GPU page sent no render_offscreen() readback".to_string());
        return;
    };
    let n = cpu.frames.len();
    let Some(last) = cpu.frames.last() else {
        return;
    };
    if (rb.width, rb.height) != (last.width, last.height) {
        v.failures.push(format!(
            "readback: render_offscreen() read {}x{}, the CPU frame is {}x{}",
            rb.width, rb.height, last.width, last.height
        ));
        return;
    }
    let delta = max_byte_delta(&last.rgba, &rb.rgba);
    v.lines.push(format!(
        "readback: render_offscreen() + rgba() of the last frame, max byte delta {delta} \
         against the CPU frame (tolerance {TOLERANCE})"
    ));
    if delta > TOLERANCE {
        v.failures.push(format!(
            "readback: render_offscreen() differs from the CPU frame by {delta} > {TOLERANCE}"
        ));
    }
    if let Some(before) = n.checked_sub(2).map(|k| &cpu.frames[k])
        && before.rgba.len() == rb.rgba.len()
        && max_byte_delta(&before.rgba, &rb.rgba) <= TOLERANCE
    {
        v.failures.push(
            "NEGATIVE CONTROL: the readback lies within the tolerance of the CPU frame before \
             the last: it does not show the last frame"
                .to_string(),
        );
    }
}

/// `init` (WP.5): the GPU is ready when `init(canvas)` returns, and an
/// `init` that throws inside an awaited call rejects its async caller with
/// the thrown string.
fn judge_init(init: Option<&Init>, v: &mut Verdict) {
    let before = v.failures.len();
    let Some(init) = init else {
        v.failures
            .push("the GPU page sent no init report".to_string());
        return;
    };
    if !init.ready_on_return {
        v.failures.push(
            "init(canvas) returned before the GPU was ready: init is not synchronous".to_string(),
        );
    }
    if !init.probe_rejected {
        v.failures.push(
            "an async fn awaiting init on a canvas with no WebGL2 did not reject".to_string(),
        );
    } else if init.probe_thrown != "string" {
        v.failures.push(format!(
            "an async fn awaiting init on a canvas with no WebGL2 rejected with a value of type \
             {}, not init's thrown string: {}",
            init.probe_thrown, init.probe_error
        ));
    }
    if v.failures.len() == before {
        v.lines.push(format!(
            "init(canvas): synchronous (gpu_ready as it returned); awaited on a canvas with no \
             WebGL2 it rejected its async caller with \"{}\"",
            init.probe_error
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: [u8; 4] = [30, 30, 46, 255];
    const RED: [u8; 4] = [205, 49, 49, 255];
    const WHITE: [u8; 4] = [224, 224, 224, 255];

    /// A 4x4 frame: a red pixel, then one white pixel per line fed.
    fn frame(lines: usize) -> Frame {
        let mut rgba: Vec<u8> = BG.repeat(16);
        rgba[..4].copy_from_slice(&RED);
        for l in 0..lines {
            let at = 4 * (4 * (l + 1) % 16);
            rgba[at..at + 4].copy_from_slice(&WHITE);
        }
        Frame {
            width: 4,
            height: 4,
            rgba,
        }
    }

    /// A page's answer; its readback is the run's last frame, as a GPU page
    /// whose offscreen holds what it presented reads it.
    fn answer(frames: Vec<Frame>) -> Answer {
        Answer {
            module: "m".to_string(),
            adapter: "ANGLE (test)".to_string(),
            readback: Some(frame(2)),
            frames,
            diff: Vec::new(),
            timings: Value::Null,
            init: Some(Init {
                ready_on_return: true,
                probe_rejected: true,
                probe_thrown: "string".to_string(),
                probe_error: "create canvas surface failed".to_string(),
            }),
        }
    }

    fn run() -> Vec<Frame> {
        (0..3).map(frame).collect()
    }

    #[test]
    fn frames_within_the_tolerance_pass_with_the_control_held() {
        let mut gpu = run();
        for f in &mut gpu {
            f.rgba[5] += TOLERANCE;
        }
        let v = judge(&answer(run()), &answer(gpu), 3);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(
            v.lines
                .last()
                .is_some_and(|l| l.contains("negative control held"))
        );
    }

    #[test]
    fn a_frame_past_the_tolerance_fails_and_names_itself() {
        let mut gpu = run();
        gpu[2].rgba[5] += TOLERANCE + 1;
        let v = judge(&answer(run()), &answer(gpu), 3);
        assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
        assert!(v.failures[0].starts_with("frame 2: the WebGL2 frame differs"));
    }

    /// The readback must show the last frame: one off by more than the
    /// tolerance fails, a stale one (the frame before) trips the control, and
    /// a page that sent none fails.
    #[test]
    fn a_readback_that_misses_the_last_frame_fails() {
        let mut off = answer(run());
        if let Some(rb) = off.readback.as_mut() {
            rb.rgba[5] += TOLERANCE + 1;
        }
        let v = judge(&answer(run()), &off, 3);
        assert!(
            v.failures[0].starts_with("readback: render_offscreen() differs"),
            "{:?}",
            v.failures
        );
        let mut stale = answer(run());
        stale.readback = Some(frame(1));
        let v = judge(&answer(run()), &stale, 3);
        assert!(
            v.failures
                .iter()
                .any(|f| f.starts_with("NEGATIVE CONTROL: the readback")),
            "{:?}",
            v.failures
        );
        let mut none = answer(run());
        none.readback = None;
        let v = judge(&answer(run()), &none, 3);
        assert!(v.failures[0].contains("no render_offscreen() readback"));
    }

    #[test]
    fn a_blank_present_fails_the_red_check() {
        let mut gpu = run();
        gpu[1].rgba[..4].copy_from_slice(&BG);
        let v = judge(&answer(run()), &answer(gpu), 3);
        assert!(
            v.failures
                .iter()
                .any(|f| f.starts_with("frame 1: no red glyph")),
            "{:?}",
            v.failures
        );
    }

    /// A GPU page stuck on its first frame matches no later CPU frame within
    /// the tolerance, and the control names the stall.
    #[test]
    fn a_page_that_stops_presenting_trips_the_negative_control() {
        let cpu = answer(run());
        let gpu = answer(vec![frame(0), frame(0), frame(1)]);
        let v = judge(&cpu, &gpu, 3);
        assert!(
            v.failures
                .iter()
                .any(|f| f.starts_with("NEGATIVE CONTROL: GPU frame 2")),
            "{:?}",
            v.failures
        );
    }

    #[test]
    fn missing_frames_a_missing_adapter_and_a_size_mismatch_fail() {
        let mut gpu = answer(vec![frame(0)]);
        gpu.adapter.clear();
        let v = judge(&answer(run()), &gpu, 3);
        assert!(
            v.failures
                .iter()
                .any(|f| f.contains("gpu page answered 1 frames"))
        );
        assert!(v.failures.iter().any(|f| f.contains("no adapter")));
        let mut wide = frame(0);
        wide.width = 8;
        wide.rgba = wide.rgba.repeat(2);
        let v = judge(&answer(vec![frame(0)]), &answer(vec![wide]), 1);
        assert!(
            v.failures
                .iter()
                .any(|f| f.contains("the GPU canvas is 8x4, the CPU frame 4x4")),
            "{:?}",
            v.failures
        );
    }

    /// An init that came back unfinished, a probe that resolved, and one that
    /// rejected with something other than init's string each fail.
    #[test]
    fn an_init_that_is_not_synchronous_or_does_not_reject_fails() {
        let judged = |edit: fn(&mut Init)| {
            let mut gpu = answer(run());
            if let Some(init) = gpu.init.as_mut() {
                edit(init);
            }
            judge(&answer(run()), &gpu, 3).failures
        };
        assert!(judged(|_| {}).is_empty());
        assert!(judged(|i| i.ready_on_return = false)[0].contains("not synchronous"));
        assert!(judged(|i| i.probe_rejected = false)[0].contains("did not reject"));
        assert!(judged(|i| i.probe_thrown = "object".to_string())[0].contains("of type object"));
        let mut gpu = answer(run());
        gpu.init = None;
        assert!(judge(&answer(run()), &gpu, 3).failures[0].contains("no init report"));
    }

    #[test]
    fn an_answer_parses_its_frames_and_refuses_a_short_one() {
        let rgba = aterm_codec::base64::encode(&[1, 2, 3, 4, 5, 6, 7, 8]).expect("encode");
        let v = aterm_json::json!({
            "module": "aterm-wasm", "adapter": "cpu", "timings": {"total_ms": 1.5},
            "frames": [{"width": 2, "height": 1, "rgba": rgba}]
        });
        let a = parse(&v).expect("parses");
        assert_eq!(a.frames[0].rgba, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(a.module, "aterm-wasm");
        let short = aterm_json::json!({
            "module": "m", "adapter": "a", "frames": [{"width": 3, "height": 1, "rgba": rgba}]
        });
        assert_eq!(
            parse(&short),
            Err("frame 0 is 3x1 but carries 8 bytes".to_string())
        );
        assert!(parse(&aterm_json::json!({"module": "m"})).is_err());
        let gpu = aterm_json::json!({
            "module": "aterm-gpu-web", "adapter": "ANGLE", "frames": [],
            "init": {"ready_on_return": true,
                     "probe": {"rejected": true, "thrown": "string", "error": "e"}}
        });
        assert_eq!(
            parse(&gpu).map(|a| a.init),
            Ok(answer(vec![]).init.map(|i| Init {
                probe_error: "e".to_string(),
                ..i
            }))
        );
        let no_probe = aterm_json::json!({
            "module": "m", "adapter": "a", "frames": [], "init": {"ready_on_return": true}
        });
        assert!(parse(&no_probe).is_err());
    }
}
