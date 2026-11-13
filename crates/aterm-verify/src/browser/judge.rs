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
    // The styles and pages some ramps run reached the route on, and the
    // runs that lay no ramp at all (a style drawn by its quads alone there,
    // as Lumen and Fire are on paper): each style and each page must reach
    // the route somewhere.
    let (mut ramped_styles, mut ramped_pages, mut rampless) = (Vec::new(), Vec::new(), Vec::new());
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
                rampless.push(what.trim_start_matches("glow ").to_string());
            } else {
                ramped_styles.push(style);
                ramped_pages.push(page);
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
    for style in styles.iter().filter(|s| !ramped_styles.contains(s)) {
        v.failures.push(format!(
            "glow [{style}]: its ramps runs laid no lit ramp on any page: it does not reach the \
             route it holds"
        ));
    }
    for page in pages.iter().filter(|p| !ramped_pages.contains(p)) {
        v.failures.push(format!(
            "glow [{page} page]: no style's ramps run laid a lit ramp: the page does not reach \
             the route it holds"
        ));
    }
    if !rampless.is_empty() {
        v.lines.push(format!(
            "glow: laid no ramp, drawn by its quads alone: {}",
            rampless.join(", ")
        ));
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
#[path = "judge_tests.rs"]
mod tests;
