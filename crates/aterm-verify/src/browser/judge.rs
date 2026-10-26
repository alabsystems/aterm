// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Browser lane verdicts: GPU frames and offscreen readback against CPU frames, worker frames
//! against the GPU page's, a synchronous `init`, and the glow scene's ramps against the quads
//! they stand for.

use aterm_json::Value;

/// Maximum RGBA8 byte delta allowed between CPU and GPU frames of one grid.
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

/// Global scope and canvas constructor names where GPU frames were drawn.
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
    /// Differential frames (`spec.diff`) from the GPU page.
    pub diff: Vec<Frame>,
    /// Offscreen readback (`render_offscreen()` + `rgba()`) of the last GPU frame.
    pub readback: Option<Frame>,
    pub timings: Value,
    /// The GPU page's `init` report; the CPU page has none.
    pub init: Option<Init>,
    /// Where the GPU page drew; the CPU page says nothing.
    pub drawn: Option<Where>,
    /// The GPU page's glow scene runs (`spec.glow`); empty where none ran.
    pub glow: Vec<GlowRun>,
}

/// One run of the GPU page's glow scene: a glow style, a page theme, bloom
/// on or off, and the glow's beam runs laid as ramps or as the quads they
/// stand for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GlowRun {
    pub style: String,
    pub page: String,
    pub bloom: bool,
    pub ramps: bool,
    /// Each presented frame's digest.
    pub digests: Vec<String>,
    /// Each frame's glow ramps, the lit rows they carry, and the additive
    /// stream's ramps (the bloom's and the crown's item list).
    pub laid: Vec<[u64; 3]>,
    /// The last frame, whole.
    pub last: Frame,
}

fn parse_glow(v: &Value) -> Result<Vec<GlowRun>, String> {
    let Some(runs) = v.get("glow") else {
        return Ok(Vec::new());
    };
    let runs = runs.as_array().ok_or("the page's `glow` is not an array")?;
    runs.iter()
        .enumerate()
        .map(|(i, run)| {
            let field = |k: &str| {
                run.get(k)
                    .ok_or_else(|| format!("glow run {i} has no `{k}`"))
            };
            let flag = |k: &str| {
                field(k)?
                    .as_bool()
                    .ok_or_else(|| format!("glow run {i}'s `{k}` is not a bool"))
            };
            let list = |k: &str| {
                field(k)?
                    .as_array()
                    .ok_or_else(|| format!("glow run {i}'s `{k}` is not an array"))
            };
            let digests = list("digests")?
                .iter()
                .map(|d| d.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("glow run {i} has a digest that is not a string"))?;
            let laid = list("laid")?
                .iter()
                .map(|l| match l.as_array()?.as_slice() {
                    [ramps, lit, add] => Some([ramps.as_u64()?, lit.as_u64()?, add.as_u64()?]),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    format!("glow run {i} has a `laid` entry that is not [ramps, lit, additive]")
                })?;
            let last = parse_frames(std::slice::from_ref(field("last")?), "glow frame")?
                .pop()
                .ok_or_else(|| format!("glow run {i} has no last frame"))?;
            Ok(GlowRun {
                style: field("style")?
                    .as_str()
                    .ok_or_else(|| format!("glow run {i}'s `style` is not a string"))?
                    .to_string(),
                page: field("page")?
                    .as_str()
                    .ok_or_else(|| format!("glow run {i}'s `page` is not a string"))?
                    .to_string(),
                bloom: flag("bloom")?,
                ramps: flag("ramps")?,
                digests,
                laid,
                last,
            })
        })
        .collect()
}

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
        glow: parse_glow(v)?,
    })
}

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

/// Count red glyph pixels (`r > 140 && g < 90 && b < 90`) in `rgba`.
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

/// Judge the GPU page's `frames` against the CPU page's within [`TOLERANCE`], including `init`, readback, and negative-control checks.
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

/// Compare a later backend's session against the first backend's: CPU frames must be byte-identical, and GPU frame deltas are reported.
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
/// The scope and canvases the worker draws in.
pub(crate) const WORKER: [&str; 3] = [
    "DedicatedWorkerGlobalScope",
    "OffscreenCanvas",
    "OffscreenCanvas",
];

/// Check whether `answer` drew in `want` (`[scope, canvas, diff_canvas]`), returning a failure message on mismatch.
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

/// Compare the worker's run against the GPU page's run in the same session, requiring byte-identical frames and readback on their expected canvases.
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

/// Judge the GPU page's glow scene: in each of `styles` on each of `pages`
/// with bloom on and off, the run laying ramps must lay some and present
/// every one of its `frames` byte-identical to the run laying quads, which
/// lays none. Its controls: the glow must change the frame (the last frame
/// is not the prompt's), bloom must change it on some page (bloom on is not
/// bloom off; a light page dims the bloom by design, so one page may not),
/// and some run with bloom on must lay ramps in the additive stream, the
/// ones the bloom and its crown read as their item list. Each `art` page
/// (beside the plain page it names) draws fx art of its own in every frame,
/// so there the ramps ride the fx stream the art opens; its control: the
/// art must change the quads run's last frame from the plain page's.
pub(crate) fn glow(
    gpu: &Answer,
    styles: &[&str],
    pages: &[&str],
    art: &[(&str, &str)],
    frames: usize,
) -> Verdict {
    let mut v = Verdict::default();
    let mut bloomed = Vec::new();
    let mut read = Vec::new();
    let run = |style: &str, page: &str, bloom: bool, ramps: bool| {
        gpu.glow
            .iter()
            .find(|r| r.style == style && r.page == page && r.bloom == bloom && r.ramps == ramps)
    };
    for (style, page) in styles
        .iter()
        .flat_map(|s| pages.iter().map(move |p| (*s, *p)))
    {
        let mut lasts = Vec::new();
        for bloom in [true, false] {
            let what = format!(
                "glow [{style}, {page} page, bloom {}]",
                if bloom { "on" } else { "off" }
            );
            let (r, q) = (
                run(style, page, bloom, true),
                run(style, page, bloom, false),
            );
            let (Some(r), Some(q)) = (r, q) else {
                v.failures
                    .push(format!("{what}: the page sent no ramps and quads runs"));
                continue;
            };
            let before = v.failures.len();
            for (who, x) in [("ramps", r), ("quads", q)] {
                if x.digests.len() != frames || x.laid.len() != frames {
                    v.failures.push(format!(
                        "{what}: the {who} run answered {} frames ({} laid counts), the lane typed {frames}",
                        x.digests.len(),
                        x.laid.len()
                    ));
                }
            }
            let ramp_frames = r.laid.iter().filter(|[_, lit, _]| *lit > 0).count();
            let (most, lit) = r
                .laid
                .iter()
                .fold((0, 0), |(m, l), &[n, k, _]| (m.max(n), l.max(k)));
            let added = r.laid.iter().filter(|[_, _, add]| *add > 0).count();
            if ramp_frames == 0 {
                v.failures.push(format!(
                    "{what}: the ramps run laid no lit ramp: it does not reach the route it holds"
                ));
            }
            if q.laid.iter().any(|[n, _, _]| *n > 0) {
                v.failures
                    .push(format!("{what}: the quads run laid ramps: it is no oracle"));
            }
            let differ: Vec<String> = r
                .digests
                .iter()
                .zip(&q.digests)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(k, _)| k.to_string())
                .collect();
            if !differ.is_empty() {
                v.failures.push(format!(
                    "{what}: the ramps differ from the quads on frames {}",
                    differ.join(", ")
                ));
            }
            if r.last != q.last {
                v.failures.push(format!(
                    "{what}: the last frame's ramps differ from its quads ({})",
                    moved(&q.last, &r.last)
                ));
            }
            if r.digests.first() == r.digests.last() {
                v.failures.push(format!(
                    "NEGATIVE CONTROL: {what}: the last frame is the prompt's: the glow drew nothing"
                ));
            }
            if v.failures.len() == before {
                v.lines.push(format!(
                    "{what}: {frames} frames byte-identical to the quads, ramps laid on {ramp_frames} \
                     (up to {most} ramps, {lit} lit rows; additive ramps on {added})"
                ));
            }
            if bloom && added > 0 {
                read.push(format!("{style} on the {page} page ({added} frames)"));
            }
            lasts.push(&r.last);
        }
        if let [on, off] = lasts[..]
            && on != off
        {
            bloomed.push(format!("{style} on the {page} page"));
        }
    }
    for (style, &(with, without)) in styles.iter().flat_map(|s| art.iter().map(move |a| (*s, a))) {
        for bloom in [true, false] {
            let (Some(a), Some(b)) = (
                run(style, with, bloom, false),
                run(style, without, bloom, false),
            ) else {
                continue;
            };
            if a.last == b.last {
                v.failures.push(format!(
                    "NEGATIVE CONTROL: glow [{style}, {with} page, bloom {}]: the last frame is \
                     the {without} page's: the art drew nothing",
                    if bloom { "on" } else { "off" }
                ));
            }
        }
    }
    if read.is_empty() {
        v.failures.push(
            "NEGATIVE CONTROL: glow: no run with bloom on laid ramps in the additive stream, so \
             no item list was read by the bloom or its crown"
                .to_string(),
        );
    } else {
        v.lines.push(format!(
            "glow: the bloom and its crown read the additive stream's ramps in {}",
            read.join(", ")
        ));
    }
    if bloomed.is_empty() {
        v.failures.push(
            "NEGATIVE CONTROL: glow: bloom on drew the frame bloom off drew on every page, so \
             no ramp was held through the bloom"
                .to_string(),
        );
    } else {
        v.lines.push(format!(
            "glow: bloom changed the last frame in {}",
            bloomed.join(", ")
        ));
    }
    v
}

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

    fn answer(frames: Vec<Frame>) -> Answer {
        Answer {
            module: "m".to_string(),
            adapter: "ANGLE (test)".to_string(),
            user_agent: String::new(),
            drawn: None,
            readback: Some(frame(2)),
            frames,
            diff: Vec::new(),
            glow: Vec::new(),
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

    /// The glow style of [`glowing`]'s runs.
    const STYLE: &str = "rainbow kitty";

    /// A glow scene answer: in [`STYLE`] on each page, bloom on and off, a
    /// ramps run laying a ramp on every frame after the prompt (one of them
    /// additive) and a quads run laying none, every frame the same; a page
    /// named `… art` draws art its plain page does not.
    fn glowing(pages: &[&str]) -> Answer {
        let mut a = answer(Vec::new());
        for &page in pages {
            for bloom in [true, false] {
                for ramps in [true, false] {
                    let laid = (0..3u64)
                        .map(|k| {
                            if ramps && k > 0 {
                                [2, 4 * k, 1]
                            } else {
                                [0, 0, 0]
                            }
                        })
                        .collect();
                    let mut last = frame(2);
                    last.rgba[6] = u8::from(bloom);
                    last.rgba[10] ^= if page.ends_with(" art") { 0x40 } else { 0 };
                    a.glow.push(GlowRun {
                        style: STYLE.to_string(),
                        page: page.to_string(),
                        bloom,
                        ramps,
                        digests: ["prompt", "one", "two"].map(str::to_string).to_vec(),
                        laid,
                        last,
                    });
                }
            }
        }
        a
    }

    #[test]
    fn glow_ramps_byte_identical_to_their_quads_pass() {
        let pages = ["dark", "light", "dark art"];
        let art = [("dark art", "dark")];
        let v = glow(&glowing(&pages), &[STYLE], &pages, &art, 3);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
        assert_eq!(v.lines.len(), 8, "{:?}", v.lines);
        // A page whose bloom draws nothing passes while another blooms.
        let mut a = glowing(&["dark", "light"]);
        for r in a.glow.iter_mut().filter(|r| r.page == "light") {
            r.last.rgba[6] = 0;
        }
        let v = glow(&a, &[STYLE], &["dark", "light"], &[], 3);
        assert!(v.failures.is_empty(), "{:?}", v.failures);
    }

    #[test]
    fn glow_ramps_that_differ_from_their_quads_fail_and_each_control_bites() {
        let pages = ["dark", "dark art"];
        let fails = |a: &Answer, says: &str| {
            let v = glow(a, &[STYLE], &pages, &[("dark art", "dark")], 3);
            assert!(
                v.failures.iter().any(|f| f.contains(says)),
                "{says}: {:?}",
                v.failures
            );
        };
        let mut a = glowing(&pages);
        a.glow[0].digests[1] = "moved".to_string();
        fails(&a, "differ from the quads on frames 1");
        let mut a = glowing(&pages);
        a.glow[0].last.rgba[0] ^= 1;
        fails(&a, "the last frame's ramps differ from its quads");
        let mut a = glowing(&pages);
        a.glow[0].laid = vec![[0, 0, 0]; 3];
        fails(&a, "laid no lit ramp");
        let mut a = glowing(&pages);
        a.glow[1].laid[2] = [1, 3, 0];
        fails(&a, "it is no oracle");
        let mut a = glowing(&pages);
        for r in a.glow.iter_mut().filter(|r| r.bloom) {
            for l in &mut r.laid {
                l[2] = 0;
            }
        }
        fails(&a, "no item list was read by the bloom or its crown");
        let mut a = glowing(&pages);
        a.glow[0].style = "comet".to_string();
        fails(&a, "sent no ramps and quads runs");
        let mut a = glowing(&pages);
        for r in &mut a.glow {
            r.digests[2] = "prompt".to_string();
        }
        fails(&a, "the glow drew nothing");
        let mut a = glowing(&pages);
        for r in &mut a.glow {
            r.last.rgba[6] = 0;
        }
        fails(&a, "bloom on drew the frame bloom off drew on every page");
        let mut a = glowing(&pages);
        a.glow.retain(|r| r.bloom);
        fails(&a, "sent no ramps and quads runs");
        let mut a = glowing(&pages);
        for r in a.glow.iter_mut().filter(|r| r.page == "dark art") {
            r.last.rgba[10] ^= 0x40;
        }
        fails(&a, "the art drew nothing");
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
