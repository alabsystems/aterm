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

/// Where the GPU frames were drawn: the global scope's and the canvases'
/// constructor names (`Window` and `HTMLCanvasElement` on the gpu page,
/// `DedicatedWorkerGlobalScope` and `OffscreenCanvas` in its worker).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Where {
    pub scope: String,
    pub canvas: String,
    pub diff_canvas: String,
}

/// What one page answered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    pub module: String,
    pub adapter: String,
    /// `navigator.userAgent` where the page ran; empty if it sent none.
    pub user_agent: String,
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
    /// Where the GPU page drew; the CPU page says nothing.
    pub drawn: Option<Where>,
}

/// The GPU page's `where` report, if the answer carries one.
fn parse_where(v: &Value) -> Result<Option<Where>, String> {
    let Some(w) = v.get("where") else {
        return Ok(None);
    };
    let text = |k: &str| {
        w.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("the page's `where` has no `{k}`"))
    };
    Ok(Some(Where {
        scope: text("scope")?,
        canvas: text("canvas")?,
        diff_canvas: text("diff_canvas")?,
    }))
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
        user_agent: text("user_agent").unwrap_or_default(),
        frames,
        diff,
        readback,
        timings: v.get("timings").cloned().unwrap_or(Value::Null),
        init: parse_init(v)?,
        drawn: parse_where(v)?,
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
            .push("the GPU run reports no adapter: init acquired none".to_string());
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

/// A later backend's session against the first backend's: the CPU module
/// draws in wasm whatever WebGL2 runs on, so its frames must be
/// byte-identical across backends (each backend's GPU frames are judged
/// against them); the GPU frames' distance from the first backend's is
/// reported, frame by frame, as the measure of what the backend moved.
pub(crate) fn across(
    first: &str,
    first_cpu: &[Frame],
    cpu: &[Frame],
    first_gpu: &[(String, &Frame)],
    gpu: &[(String, &Frame)],
) -> Verdict {
    let mut v = Verdict::default();
    if first_cpu.len() != cpu.len() {
        v.failures.push(format!(
            "the CPU page answered {} frames, under {first} {}",
            cpu.len(),
            first_cpu.len()
        ));
    }
    let moved: Vec<String> = first_cpu
        .iter()
        .zip(cpu)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(k, (a, b))| {
            let delta = if a.rgba.len() == b.rgba.len() {
                format!("max byte delta {}", max_byte_delta(&a.rgba, &b.rgba))
            } else {
                format!("{}x{} against {}x{}", b.width, b.height, a.width, a.height)
            };
            format!("CPU frame {k} differs from the {first} session's ({delta})")
        })
        .collect();
    if moved.is_empty() && v.failures.is_empty() {
        v.lines.push(format!(
            "the CPU module's {} frames are byte-identical to the {first} session's",
            cpu.len()
        ));
    }
    v.failures.extend(moved);
    let each: Vec<String> = first_gpu
        .iter()
        .zip(gpu)
        .filter(|((n, a), (m, b))| n == m && a.rgba.len() == b.rgba.len())
        .map(|((n, a), (_, b))| {
            let differing = a.rgba.iter().zip(&b.rgba).filter(|(x, y)| x != y).count();
            format!("{n} {} ({differing})", max_byte_delta(&a.rgba, &b.rgba))
        })
        .collect();
    v.lines.push(format!(
        "GPU frames against the {first} session's, max byte delta (bytes that differ): {}",
        each.join(", ")
    ));
    v
}

/// The scope and canvases the gpu page draws in.
pub(crate) const DOCUMENT: [&str; 3] = ["Window", "HTMLCanvasElement", "HTMLCanvasElement"];
/// The scope and canvases its worker draws in: the page's canvas
/// transferred to it, then an OffscreenCanvas of its own.
pub(crate) const WORKER: [&str; 3] = [
    "DedicatedWorkerGlobalScope",
    "OffscreenCanvas",
    "OffscreenCanvas",
];

/// Whether `answer` says it drew in `want` (scope, canvas, differential
/// canvas); the failure naming what it drew in, if not.
pub(crate) fn drawn_in(who: &str, answer: &Answer, want: [&str; 3]) -> Option<String> {
    let Some(w) = answer.drawn.as_ref() else {
        return Some(format!(
            "the {who} sent no `where`: it cannot show where it drew"
        ));
    };
    let got = [w.scope.as_str(), w.canvas.as_str(), w.diff_canvas.as_str()];
    (got != want).then(|| {
        format!(
            "the {who} drew in {} on {} then {}, not {} on {} then {}",
            got[0], got[1], got[2], want[0], want[1], want[2]
        )
    })
}

/// The worker's run (init_offscreen on a transferred OffscreenCanvas, then
/// one of its own) against the gpu page's (init on the document's canvases)
/// in the same session: the same module on the same adapter issues the same
/// GL calls, so every frame and the harness readback must be byte-identical.
/// NEGATIVE CONTROL: each must have drawn where it says it did, so a worker
/// page that drew on the document, or a gpu page in a worker, cannot pass as
/// the other.
pub(crate) fn worker(gpu: &Answer, worker: &Answer) -> Verdict {
    let mut v = Verdict::default();
    v.failures.extend(drawn_in("gpu page", gpu, DOCUMENT));
    v.failures.extend(drawn_in("worker", worker, WORKER));
    if worker.adapter != gpu.adapter {
        v.failures.push(format!(
            "the worker drew on \"{}\", the gpu page on \"{}\"",
            worker.adapter, gpu.adapter
        ));
    }
    let pairs = [
        ("parity", &gpu.frames, &worker.frames),
        ("diff", &gpu.diff, &worker.diff),
    ];
    let mut counted = 0;
    for (seq, page, off) in pairs {
        if page.len() != off.len() {
            v.failures.push(format!(
                "the worker drew {} {seq} frames, the gpu page {}",
                off.len(),
                page.len()
            ));
            continue;
        }
        for (k, (a, b)) in page.iter().zip(off.iter()).enumerate() {
            counted += 1;
            if a != b {
                v.failures.push(format!(
                    "{seq}-{k}: the worker's frame differs from the gpu page's ({})",
                    moved(a, b)
                ));
            }
        }
    }
    match (&gpu.readback, &worker.readback) {
        (Some(a), Some(b)) if a != b => v.failures.push(format!(
            "readback: the worker's render_offscreen() differs from the gpu page's ({})",
            moved(a, b)
        )),
        (Some(_), None) => v
            .failures
            .push("the worker sent no render_offscreen() readback".to_string()),
        _ => {}
    }
    if v.failures.is_empty() {
        v.lines.push(format!(
            "the worker (init_offscreen on a transferred OffscreenCanvas, then one of its own) \
             drew its {counted} frames and its readback byte-identical to the gpu page's"
        ));
    }
    v
}

/// How frame `b` moved from `a`: the largest byte delta and how many bytes
/// differ, or the two sizes.
fn moved(a: &Frame, b: &Frame) -> String {
    if a.rgba.len() == b.rgba.len() {
        let differing = a.rgba.iter().zip(&b.rgba).filter(|(x, y)| x != y).count();
        format!(
            "max byte delta {}, {differing} bytes",
            max_byte_delta(&a.rgba, &b.rgba)
        )
    } else {
        format!("{}x{} against {}x{}", b.width, b.height, a.width, a.height)
    }
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
        v.failures
            .push("init returned before the GPU was ready: init is not synchronous".to_string());
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
            "init: synchronous (gpu_ready as it returned); awaited on a canvas with no \
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
            user_agent: String::new(),
            drawn: None,
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

    /// Across backends the CPU frames must not move: identical ones pass
    /// and report the GPU frames' distance, and one changed byte fails and
    /// names its frame.
    #[test]
    fn a_cpu_frame_that_moves_with_the_backend_fails() {
        fn refs(frames: &[Frame]) -> Vec<(String, &Frame)> {
            frames
                .iter()
                .enumerate()
                .map(|(k, f)| (format!("parity-{k}"), f))
                .collect()
        }
        let (a, b) = (run(), run());
        let mut gpu = run();
        gpu[1].rgba[5] += 3;
        let first_gpu = run();
        let v = across("metal", &a, &b, &refs(&first_gpu), &refs(&gpu));
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(
            v.lines
                .iter()
                .any(|l| l.ends_with("parity-0 0 (0), parity-1 3 (1), parity-2 0 (0)")),
            "{:?}",
            v.lines
        );
        let mut c = run();
        c[2].rgba[5] += 1;
        let v = across("metal", &a, &c, &refs(&first_gpu), &refs(&gpu));
        assert_eq!(
            v.failures,
            ["CPU frame 2 differs from the metal session's (max byte delta 1)"]
        );
    }

    /// A gpu page and its worker in the same session draw the same bytes:
    /// identical runs pass, one byte moved fails and names its frame, and a
    /// run drawn somewhere other than it says trips the control.
    #[test]
    fn a_worker_must_draw_where_it_says_and_what_the_page_drew() {
        let place = |w: [&str; 3]| {
            Some(Where {
                scope: w[0].to_string(),
                canvas: w[1].to_string(),
                diff_canvas: w[2].to_string(),
            })
        };
        let at = |w| {
            let mut a = answer(run());
            a.diff = vec![frame(0), frame(1)];
            a.drawn = place(w);
            a
        };
        let (page, off) = (at(DOCUMENT), at(WORKER));
        let v = worker(&page, &off);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert!(v.lines[0].contains("its 5 frames"), "{:?}", v.lines);
        let mut moved = at(WORKER);
        moved.diff[1].rgba[5] += 1;
        assert_eq!(
            worker(&page, &moved).failures,
            ["diff-1: the worker's frame differs from the gpu page's (max byte delta 1, 1 bytes)"]
        );
        // The control: a "worker" that drew on the document's canvas.
        let mut on_page = at(WORKER);
        on_page.drawn = place(["Window", "HTMLCanvasElement", "OffscreenCanvas"]);
        let v = worker(&page, &on_page);
        assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
        assert!(
            v.failures[0].starts_with("the worker drew in Window"),
            "{:?}",
            v.failures
        );
        let mut silent = at(WORKER);
        silent.drawn = None;
        assert!(worker(&page, &silent).failures[0].contains("sent no `where`"));
        let mut elsewhere = at(WORKER);
        elsewhere.adapter = "SwiftShader".to_string();
        assert!(worker(&page, &elsewhere).failures[0].contains("the worker drew on"));
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
        let placed = aterm_json::json!({
            "module": "m", "adapter": "a", "frames": [], "user_agent": "Firefox/155.0",
            "where": {"scope": "DedicatedWorkerGlobalScope", "canvas": "OffscreenCanvas",
                      "diff_canvas": "OffscreenCanvas"}
        });
        let a = parse(&placed).expect("parses");
        assert_eq!(a.user_agent, "Firefox/155.0");
        assert_eq!(drawn_in("worker", &a, WORKER), None);
        let half = aterm_json::json!({
            "module": "m", "adapter": "a", "frames": [], "where": {"scope": "Window"}
        });
        assert!(parse(&half).is_err());
    }
}
