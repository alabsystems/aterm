// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify browser`: both web modules in real headless browsers
//! (docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.5 and WP.8: "one
//! real WebGL2 boot").
//!
//! The modules are `verify wasm-smoke`'s dev build ([`crate::wasm_smoke::
//! build_dev`], a STOCK EXCEPTION: Trust has no wasm32 std), bound for a page
//! by `verify glue --flavor web`. A loopback server ([`serve`]) serves them
//! with the pages in `crates/aterm-verify/browser/` and the bundled font.
//! Each browser session is two rows, the pages' and the worker's:
//! - Chromium ([`chromium`]) runs headless with its DevTools on two pipes
//!   ([`cdp`]), once per ANGLE backend WebGL2 can run on: on macOS Metal,
//!   then SwiftShader (Chromium's software WebGL); elsewhere SwiftShader. A
//!   session whose GPU page reports another backend's adapter (a browser
//!   that fell back) is NOT RUN, never a pass.
//! - Firefox ([`firefox`]) runs headless and is driven by its pages: each
//!   POSTs its answer to the server and opens the next. A session whose
//!   pages report another browser's user agent is NOT RUN.
//!
//! Three pages, each fed crates/aterm-gpu-web/tests/cpu_gpu_parity.rs's grid,
//! theme, px, font and content, one line per frame:
//! - `cpu.html` boots aterm-wasm and hands back each frame's `rgba()`;
//! - `gpu.html` boots aterm-gpu-web, `await init(canvas)` (aterm-gpu's GL
//!   arm on WebGL2 through the module's `aterm_gl` imports, wasm-platform
//!   WP.8), presents each frame with `render()` and hands back the canvas.
//!   `init` is synchronous: the page reports whether the GPU was ready as
//!   the call returned, and whether an `init` that throws (on a canvas
//!   already holding a 2D context) rejects the async fn awaiting it;
//! - `worker.html` transfers its canvas to a dedicated worker, which runs the
//!   gpu page's lane there through `init_offscreen(canvas)` (a site that
//!   renders off the main thread).
//!
//! [`judge`] holds each GPU frame to the CPU frame within that test's
//! tolerance, with a negative control, and the GPU page's `init` report to
//! both answers. The GPU page then draws a wider grid of attributes, colours,
//! sprites and an image on a second canvas, and [`recorded`] holds every GPU
//! frame to the frames the wgpu-WebGL module drew for the same content
//! (`browser/wgpu-webgl/`, recorded before WP.8): byte for byte on the
//! recording's adapter (ANGLE Metal on an M4 Pro), within the CPU tolerance
//! on another. The worker's run is held to all of that too, and to the gpu
//! page's frames of the same session byte for byte ([`judge::worker`]). A
//! session after the first is also held to the first's CPU frames, which
//! must not move (the CPU module draws in wasm), and its GPU frames' distance
//! from the first's is reported. Each session's GPU frames are written to
//! `<target>/verify/cells/browser/<session>/gpu-frames/` (the worker's to
//! `worker-frames/`), and the timings each page measured, each frame's
//! readback timed apart from its render, are printed and written to
//! `<target>/verify/cells/browser/<session>/timings.json`.
//!
//! No browser or no stable wasm32 std is COULD NOT RUN with the fix, never a
//! pass. In no set.

pub(crate) mod cdp;
pub(crate) mod chromium;
pub(crate) mod firefox;
pub(crate) mod judge;
pub(crate) mod recorded;
pub(crate) mod serve;

pub use chromium::fetch_main;

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand, spawn, spawn_with_two_fds};

use crate::Ctx;
use crate::ladder::Report;
use crate::perf::Status;
use crate::plan::{Lane, lane_dir};

const LABEL: &str =
    "browser (both web modules in headless browsers: a WebGL2 boot, GPU vs CPU pixels)";

/// cpu_gpu_parity.rs's grid.
const ROWS: u16 = 6;
const COLS: u16 = 12;
/// cpu_gpu_parity.rs's font size.
const PX: f32 = 18.0;
/// cpu_gpu_parity.rs's web theme: fg, bg, cursor, selection (`0x00RRGGBB`).
const THEME: [u32; 4] = [0x00E0_E0E0, 0x001E_1E2E, 0x00FF_FFFF, 0x0030_4060];
/// cpu_gpu_parity.rs's content, one line per frame: every frame shows the
/// red glyph, and each adds a line the one before it lacks.
const FRAMES: [&str; 4] = [
    "\x1b[31mRR\x1b[0m\r\n",
    "\x1b[44m  \x1b[0m\r\n",
    "\x1b[7mXX\x1b[0m\r\n",
    "ab\r\n",
];

/// The recorded-frame differential's grid (`recorded`): wider than the
/// parity grid, so a line holds a run of each attribute.
const DIFF_ROWS: u16 = 8;
const DIFF_COLS: u16 = 40;
/// Its content, one line per frame: weights and styles, every underline
/// shape and the other decorations, indexed and true colours, box drawing,
/// blocks and braille (drawn as sprites), a hyperlink, a sixel image (the
/// image plane), then a cursor shape change.
const DIFF_FRAMES: [&str; 7] = [
    "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[2mdim\x1b[0m \x1b[7mreverse\x1b[0m \x1b[1;3mbi\x1b[0m\r\n",
    "\x1b[4mund\x1b[0m \x1b[4:2mdbl\x1b[0m \x1b[4:3mcurl\x1b[0m \x1b[4:4mdot\x1b[0m \x1b[9mstrike\x1b[0m \x1b[53mover\x1b[0m\r\n",
    "\x1b[38;5;196m196\x1b[0m \x1b[48;5;28m bg28 \x1b[0m \x1b[38;2;255;170;0;48;2;0;60;120m true \x1b[0m \x1b[58;2;0;255;0;4mul\x1b[0m\r\n",
    "\u{250c}\u{2500}\u{252c}\u{2510} \u{256d}\u{256e} \u{2580}\u{2584}\u{2588}\u{258c}\u{2590} \u{2591}\u{2592}\u{2593} \u{28ff}\u{2847}\r\n",
    "\x1b]8;;https://example.invalid/\x1b\\link\x1b]8;;\x1b\\ \x1b[8mhidden\x1b[0m end\r\n",
    "\x1bPq#1;2;100;20;20#2;2;20;100;20#1~~~~~~~~#2~~~~~~~~-#2~~~~~~~~#1~~~~~~~~\x1b\\\r\n",
    "\x1b[4 qcursor",
];

/// The pages, beside this crate.
fn pages_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("browser")
}

/// What `window.atermLane` is handed: the grid, theme, px and frames.
fn spec() -> Value {
    json!({
        "rows": ROWS,
        "cols": COLS,
        "px": PX,
        "theme": THEME.to_vec(),
        "frames": FRAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "diff": {
            "rows": DIFF_ROWS,
            "cols": DIFF_COLS,
            "frames": DIFF_FRAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        },
    })
}

/// The GPU page's frames by recording name: `parity-<k>` (the parity
/// grid's), then `diff-<k>`.
fn named_gpu_frames(gpu: &judge::Answer) -> Vec<(String, &judge::Frame)> {
    let named = |seq: &str, frames: &'_ [judge::Frame]| -> Vec<(String, usize)> {
        (0..frames.len())
            .map(|k| (format!("{seq}-{k}"), k))
            .collect()
    };
    named("parity", &gpu.frames)
        .into_iter()
        .map(|(n, k)| (n, &gpu.frames[k]))
        .chain(
            named("diff", &gpu.diff)
                .into_iter()
                .map(|(n, k)| (n, &gpu.diff[k])),
        )
        .collect()
}

/// Why a session, or a page of one, ended without answers.
enum Stop {
    /// The machine, not the tree: the browser did not start or answer.
    CouldNotRun(String),
    /// A finding about the tree: a page failed or hung.
    Fail(String),
}

/// One page's answer and what it cost.
struct Page {
    answer: judge::Answer,
    /// From the navigation to the load event, where the lane drives the page
    /// over the DevTools protocol.
    load_ms: Option<f64>,
    /// The lane's wait for the answer: the evaluation, or for a page that
    /// posts its answer, the time since the page before it answered.
    evaluate_ms: f64,
}

/// What a session gathered.
struct Session {
    product: String,
    /// When the browser answered the DevTools protocol, where it speaks it.
    ready_ms: Option<f64>,
    cpu: Page,
    gpu: Page,
    /// The worker page, judged on its own row: its failure ends only that row.
    worker: Result<Page, Stop>,
    served: usize,
}

fn ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// A browser session the lane runs.
#[derive(Clone)]
enum Engine {
    /// Chromium with WebGL2 on one ANGLE backend, over the DevTools protocol.
    Chromium(chromium::Browser, chromium::Angle),
    /// Firefox, driven by its pages.
    Firefox(firefox::Firefox),
}

impl Engine {
    /// The session's name: its directory, and the tag on its lines.
    fn name(&self) -> &'static str {
        match self {
            Engine::Chromium(_, angle) => angle.name(),
            Engine::Firefox(_) => "firefox",
        }
    }

    /// What the session's rows add to the lane's label.
    fn label(&self) -> String {
        match self {
            Engine::Chromium(_, angle) => format!("WebGL2 on ANGLE {}", angle.name()),
            Engine::Firefox(_) => "Firefox".to_string(),
        }
    }

    /// Why a GPU answer was not drawn by this engine, if it was not: a
    /// Chromium that fell back to another backend, or a Firefox executable
    /// that is another browser.
    fn not_drawn(&self, gpu: &judge::Answer) -> Option<String> {
        match self {
            Engine::Chromium(_, angle) => (!angle.drew(&gpu.adapter)).then(|| {
                let name = angle.name();
                format!(
                    "the browser gave WebGL2 on \"{}\", not ANGLE {name}: no {name} session ran",
                    gpu.adapter
                )
            }),
            Engine::Firefox(_) => (!firefox::is_firefox(&gpu.user_agent)).then(|| {
                format!(
                    "the browser answered as \"{}\", not Firefox: no firefox session ran",
                    gpu.user_agent
                )
            }),
        }
    }

    /// Boot the pages in this engine's browser, on the private `profile`.
    fn drive(&self, site: &Path, work: &Path, profile: &Path) -> Result<Session, Stop> {
        match self {
            Engine::Chromium(browser, angle) => {
                drive_chromium(browser, site, work, profile, *angle)
            }
            Engine::Firefox(firefox) => drive_firefox(firefox, site, work, profile),
        }
    }
}

/// Build, bind and serve both modules once, then boot the pages in each
/// browser session: Chromium on each ANGLE backend
/// ([`chromium::angle_backends`]), then Firefox. Two rows per session.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let t0 = Instant::now();
    let (chromium, firefox) = (chromium::find(), firefox::find());
    if let (Err(c), Err(f)) = (&chromium, &firefox) {
        r.cannot_run(format!(
            "{LABEL} — {c}; {f} (fix: targo --unverified verify fetch-browser)"
        ));
        return;
    }
    let rustup = match crate::perf::wasm::build_tools(false) {
        Ok(rustup) => rustup,
        Err(refused) => {
            match refused.status {
                Status::Skip { why, fix } => {
                    r.cannot_run(format!("{LABEL} — {why} (fix: {fix})"));
                }
                Status::Fail(findings) => {
                    r.fail(LABEL);
                    r.raw(findings.join("\n"));
                }
                _ => r.cannot_run(format!("{LABEL} — its tools answered no verdict")),
            }
            return;
        }
    };
    let Some(modules) = crate::wasm_smoke::build_dev(ctx, r, &rustup, "browser", LABEL) else {
        return;
    };
    let built_s = t0.elapsed().as_secs_f64();
    let work = lane_dir(ctx, Lane::CellsTarget)
        .unwrap_or_else(|| ctx.state_dir().join("cells"))
        .join("browser");
    let site = work.join("site");
    let t_bind = Instant::now();
    if let Err(why) = assemble(&ctx.root, &modules, &site) {
        r.fail(format!("{LABEL} — {why}"));
        return;
    }
    let built = Built {
        site,
        work,
        built_s,
        bind_ms: ms(t_bind),
    };
    let mut first: Option<(&'static str, Session)> = None;
    let mut run_one = |r: &mut Report, engine: Engine| {
        let session = session(ctx, r, &built, &engine, first.as_ref());
        if first.is_none() {
            first = session.map(|s| (engine.name(), s));
        }
    };
    match &chromium {
        Ok(browser) => {
            r.raw(format!(
                "browser: {} ({}), WebGL2 on ANGLE {}",
                browser.path.display(),
                browser.source,
                chromium::angle_backends()
                    .iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(" then ")
            ));
            for &angle in chromium::angle_backends() {
                run_one(r, Engine::Chromium(browser.clone(), angle));
            }
        }
        Err(why) => r.cannot_run(format!(
            "{LABEL}, Chromium — {why} (fix: {})",
            chromium::FETCH_FIX
        )),
    }
    match &firefox {
        Ok(f) => {
            r.raw(format!("firefox: {} ({})", f.path.display(), f.source));
            run_one(r, Engine::Firefox(f.clone()));
        }
        Err(why) => r.cannot_run(format!(
            "{LABEL}, Firefox — {why} (fix: {})",
            firefox::fix()
        )),
    }
}

/// What every session shares: the served site, the lane's directory and
/// what building and binding cost.
struct Built {
    site: PathBuf,
    work: PathBuf,
    built_s: f64,
    bind_ms: f64,
}

/// One session: the pages booted in `engine`'s browser, judged, their frames
/// and timings written to `<work>/<session>/`, and its two rows (the pages',
/// then the worker's). A session after the first is also held to `first`'s
/// CPU frames (the CPU module draws in wasm, so they must not move with the
/// browser), and its GPU frames' distance from `first`'s is reported. The
/// session, when one ran.
fn session(
    ctx: &Ctx,
    r: &mut Report,
    built: &Built,
    engine: &Engine,
    first: Option<&(&'static str, Session)>,
) -> Option<Session> {
    let name = engine.name();
    let label = format!("{LABEL}, {}", engine.label());
    let worker_label = format!("{LABEL}, init_offscreen in a worker, {}", engine.label());
    let say = |text: String| format!("browser [{name}]: {text}");
    let dir = built.work.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        r.cannot_run(format!("{label} — could not create {}: {e}", dir.display()));
        return None;
    }
    let (session_engine, session_site, session_dir) =
        (engine.clone(), built.site.clone(), dir.clone());
    let profile = ctx.scratch.join(format!("browser-profile-{name}"));
    let driven = crate::exec::bounded(ctx.child_ceiling, move || {
        session_engine.drive(&session_site, &session_dir, &profile)
    });
    let not_run = format!("{worker_label} — the session's pages did not run (above)");
    let session = match driven {
        Ok(Ok(s)) => s,
        Ok(Err(Stop::CouldNotRun(why))) => {
            r.cannot_run(format!("{label} — {why}"));
            r.cannot_run(not_run);
            return None;
        }
        Ok(Err(Stop::Fail(why))) => {
            r.fail(label);
            r.raw(why);
            r.cannot_run(not_run);
            return None;
        }
        Err(why) => {
            r.fail(label);
            r.raw(format!("the browser session {}", why.sentence()));
            r.cannot_run(not_run);
            return None;
        }
    };
    let gpu = &session.gpu.answer;
    let timings = json!({
        "browser": session.product,
        "engine": name,
        "adapter": gpu.adapter,
        "user_agent": gpu.user_agent,
        "build": "dev (wasm32, rustup stable)",
        "build_and_tools_s": (built.built_s * 1e3).round() / 1e3,
        "bind_ms": built.bind_ms,
        "browser_ready_ms": session.ready_ms.map_or(Value::Null, |v| json!(v)),
        "requests_served": session.served,
        "cpu": page_timings(&session.cpu),
        "gpu": page_timings(&session.gpu),
        "worker": match &session.worker {
            Ok(p) => page_timings(p),
            Err(Stop::CouldNotRun(why) | Stop::Fail(why)) => json!({"error": why}),
        },
    });
    let ready = session
        .ready_ms
        .map_or_else(String::new, |v| format!(" ready in {v:.0} ms;"));
    r.raw(say(format!(
        "{}{ready} GPU adapter {}",
        session.product, gpu.adapter
    )));
    r.raw(say(timing_line("cpu page (aterm-wasm)", &session.cpu)));
    r.raw(say(timing_line("gpu page (aterm-gpu-web)", &session.gpu)));
    if let Ok(w) = &session.worker {
        r.raw(say(timing_line("worker (aterm-gpu-web)", w)));
    }
    let record = dir.join("timings.json");
    match aterm_json::to_string(&timings).map(|t| std::fs::write(&record, t + "\n")) {
        Ok(Ok(())) => r.raw(say(format!("timings in {}", record.display()))),
        _ => r.raw(say(format!("could not write {}", record.display()))),
    }
    let recording = recorded::read(&recorded::wgpu_webgl_dir());
    let mut failures = held(r, &say, &session, gpu, &recording, &dir.join("gpu-frames"));
    if let Some((before, earlier)) = first {
        let across = judge::across(
            before,
            &earlier.cpu.answer.frames,
            &session.cpu.answer.frames,
            &named_gpu_frames(&earlier.gpu.answer),
            &named_gpu_frames(gpu),
        );
        for line in &across.lines {
            r.raw(say(line.clone()));
        }
        failures.extend(across.failures);
    }
    if !failures.is_empty() {
        r.fail(label);
        r.raw(failures.join("\n"));
    } else if let Some(why) = engine.not_drawn(gpu) {
        r.cannot_run(format!("{label} — {why}"));
    } else {
        r.pass(label);
    }
    match &session.worker {
        Err(Stop::CouldNotRun(why)) => r.cannot_run(format!("{worker_label} — {why}")),
        Err(Stop::Fail(why)) => {
            r.fail(worker_label);
            r.raw(why.clone());
        }
        Ok(w) => {
            let say_worker = |text: String| say(format!("worker: {text}"));
            let mut failures = held(
                r,
                &say_worker,
                &session,
                &w.answer,
                &recording,
                &dir.join("worker-frames"),
            );
            let same = judge::worker(gpu, &w.answer);
            for line in &same.lines {
                r.raw(say_worker(line.clone()));
            }
            failures.extend(same.failures);
            if !failures.is_empty() {
                r.fail(worker_label);
                r.raw(failures.join("\n"));
            } else if let Some(why) = engine.not_drawn(&w.answer) {
                r.cannot_run(format!("{worker_label} — {why}"));
            } else {
                r.pass(worker_label);
            }
        }
    }
    Some(session)
}

/// One GPU run of `session` (the gpu page's, or its worker's) judged: against
/// the session's CPU frames, its frames written to `frames_dir`, then held
/// to the wgpu-WebGL `recording`. Its lines go to `r` through `say`; the
/// failures.
fn held(
    r: &mut Report,
    say: &dyn Fn(String) -> String,
    session: &Session,
    gpu: &judge::Answer,
    recording: &Result<recorded::Recording, String>,
    frames_dir: &Path,
) -> Vec<String> {
    let verdict = judge::judge(&session.cpu.answer, gpu, FRAMES.len());
    for line in &verdict.lines {
        r.raw(say(line.clone()));
    }
    let mut failures = verdict.failures;
    // The recorded-frame differential: this run's GPU frames, written where
    // they can be looked at (and recorded from), then held to the
    // wgpu-WebGL module's recording.
    let named = named_gpu_frames(gpu);
    let seen = recorded::Recording {
        module: gpu.module.clone(),
        browser: session.product.clone(),
        adapter: gpu.adapter.clone(),
        frames: named
            .iter()
            .map(|(n, f)| (n.clone(), (*f).clone()))
            .collect(),
    };
    match recorded::write(frames_dir, &seen) {
        Ok(()) => r.raw(say(format!("its GPU frames in {}", frames_dir.display()))),
        Err(why) => r.raw(say(format!("could not write the GPU frames: {why}"))),
    }
    match recording {
        Ok(rec) => {
            let diff = recorded::judge(rec, &gpu.adapter, &named);
            for line in &diff.lines {
                r.raw(say(line.clone()));
            }
            failures.extend(diff.failures);
        }
        Err(why) => failures.push(format!("the wgpu-WebGL recording: {why}")),
    }
    failures
}

fn page_timings(p: &Page) -> Value {
    json!({
        "module": p.answer.module,
        "load_ms": p.load_ms.map_or(Value::Null, |v| json!(v)),
        "evaluate_ms": p.evaluate_ms,
        "page": p.answer.timings,
    })
}

/// The page timings that time a step of their own (a number, or one per
/// frame): what a page's total less them spent on nothing it timed.
const TIMED: [&str; 15] = [
    "font_fetch_ms",
    "module_init_ms",
    "construct_ms",
    "probe_ms",
    "gpu_init_ms",
    "frame_ms",
    "finish_ms",
    "copy_ms",
    "read_ms",
    "encode_ms",
    "frame_wait_ms",
    "readback_ms",
    "free_ms",
    "diff_setup_ms",
    "diff_frame_ms",
];

/// `t[key]`'s milliseconds: the number, or the sum of the list.
fn timed(t: &Value, key: &str) -> Option<f64> {
    match t.get(key)? {
        Value::Array(each) => Some(each.iter().filter_map(Value::as_f64).sum()),
        v => v.as_f64(),
    }
}

/// The frame a per-frame timing's entry `k` is: `parity-<k>`, then
/// `diff-<k>`.
fn frame_name(k: usize) -> String {
    match k.checked_sub(FRAMES.len()) {
        None => format!("parity-{k}"),
        Some(d) => format!("diff-{d}"),
    }
}

/// The readbacks' cost: each presented frame's gl.finish(), its copy onto a
/// 2D canvas, its getImageData (where the copy's read of the GPU's frame is
/// waited for; Chromium's finish() does not wait) and its base64, summed
/// apart, with the frame each of the first and third took longest on.
fn readback_split(t: &Value) -> Option<String> {
    let most = |key: &str| -> Option<String> {
        let each: Vec<f64> = t
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        let (at, v) = each
            .iter()
            .copied()
            .enumerate()
            .fold(
                (0, 0.0_f64),
                |best, (k, v)| if v > best.1 { (k, v) } else { best },
            );
        Some(format!(
            "{:.1} ms (most {v:.1} on {})",
            each.iter().sum::<f64>(),
            frame_name(at)
        ))
    };
    let frames = t.get("read_ms")?.as_array()?.len();
    Some(format!(
        "readbacks of {frames} frames: finish {}, copy {:.1} ms, getImageData {}, base64 {:.1} \
         ms",
        most("finish_ms")?,
        timed(t, "copy_ms")?,
        most("read_ms")?,
        timed(t, "encode_ms")?
    ))
}

/// One page's timings on one line: what the page measured, in its order,
/// and what of its total it timed no step of.
fn timing_line(who: &str, p: &Page) -> String {
    let t = &p.answer.timings;
    let num = |k: &str| t.get(k).and_then(Value::as_f64);
    let mut parts = Vec::new();
    if let Some(load) = p.load_ms {
        parts.push(format!("load {load:.1} ms"));
    }
    for (k, name) in [
        ("module_init_ms", "module"),
        ("construct_ms", "construct"),
        ("gpu_init_ms", "init"),
    ] {
        if let Some(v) = num(k) {
            parts.push(format!("{name} {v:.1} ms"));
        }
    }
    if let Some(frames) = t.get("frame_ms").and_then(Value::as_array) {
        let each: Vec<String> = frames
            .iter()
            .filter_map(Value::as_f64)
            .map(|v| format!("{v:.1}"))
            .collect();
        parts.push(format!("frames {} ms", each.join("/")));
    }
    parts.extend(readback_split(t));
    // A worker's total is its own; the page's adds starting it.
    let own = num("worker_total_ms").or_else(|| num("total_ms"));
    if let Some(v) = own {
        let accounted: f64 = TIMED.iter().filter_map(|k| timed(t, k)).sum();
        let wait = num("frame_wait_ms").map_or_else(String::new, |w| {
            format!("{w:.1} of them waiting on animation frames, ")
        });
        parts.push(format!(
            "total {v:.1} ms ({wait}{:.1} in no timed step)",
            v - accounted
        ));
    }
    if let Some(trip) = t
        .pointer("/page/worker_round_trip_ms")
        .and_then(Value::as_f64)
    {
        parts.push(format!(
            "{trip:.1} ms from the page's transfer to the answer"
        ));
    }
    format!("{who}: {}", parts.join(", "))
}

/// The site directory: the pages, the lane's spec (for a page that fetches
/// it), the bundled font, the CPU module's glue in `cpu/` and the GPU
/// module's in `gpu/`.
fn assemble(root: &Path, modules: &Path, site: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(site);
    std::fs::create_dir_all(site)
        .map_err(|e| format!("could not create {}: {e}", site.display()))?;
    let font = root.join("crates/aterm-render/assets/DejaVuSansMono.ttf");
    let copies = [
        "cpu.html",
        "gpu.html",
        "worker.html",
        "lane.mjs",
        "worker.mjs",
    ]
    .map(|p| (pages_dir().join(p), site.join(p)))
    .into_iter()
    .chain([(font, site.join("font.ttf"))]);
    for (from, to) in copies {
        std::fs::copy(&from, &to)
            .map_err(|e| format!("could not copy {} to {}: {e}", from.display(), to.display()))?;
    }
    let spec_path = site.join("spec.json");
    aterm_json::to_string(&spec())
        .map_err(|e| format!("could not encode the spec: {e}"))
        .and_then(|text| {
            std::fs::write(&spec_path, text)
                .map_err(|e| format!("could not write {}: {e}", spec_path.display()))
        })?;
    for (module, dir) in [("aterm_wasm.wasm", "cpu"), ("aterm_gpu_web.wasm", "gpu")] {
        crate::wasm_glue::bind(
            &modules.join(module),
            &site.join(dir),
            crate::wasm_glue::Flavor::Web,
        )?;
    }
    Ok(())
}

/// The browser, killed (by its pid) and reaped on drop if it is still
/// running.
struct Running(LaneChild);

impl Running {
    /// How the browser ended, if it has.
    fn ended(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(status.to_string()),
            Err(e) => Some(format!("could not be waited for: {e}")),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The last lines of the browser's log, for a refusal.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    format!("the browser's log ({}):\n{tail}", log.display())
}

/// The browser's log at `log`, as its stdout and stderr.
fn log_stdio(log: &Path) -> Result<(ChildStdio, ChildStdio), Stop> {
    let out = std::fs::File::create(log)
        .map_err(|e| Stop::CouldNotRun(format!("could not create {}: {e}", log.display())))?;
    let err = out
        .try_clone()
        .map_err(|e| Stop::CouldNotRun(format!("could not share {}: {e}", log.display())))?;
    Ok((
        ChildStdio::Fd(OwnedFd::from(out)),
        ChildStdio::Fd(OwnedFd::from(err)),
    ))
}

/// Launch Chromium on `angle`, serve `site`, boot the three pages over the
/// DevTools protocol and close the browser.
fn drive_chromium(
    browser: &chromium::Browser,
    site: &Path,
    work: &Path,
    profile: &Path,
    angle: chromium::Angle,
) -> Result<Session, Stop> {
    let _ = std::fs::remove_dir_all(profile);
    std::fs::create_dir_all(profile)
        .map_err(|e| Stop::CouldNotRun(format!("could not create {}: {e}", profile.display())))?;
    let log = work.join("browser.log");
    let (out, err) = log_stdio(&log)?;
    let pipe = || std::io::pipe().map_err(|e| Stop::CouldNotRun(format!("no pipe: {e}")));
    // The browser reads commands on fd 3 and writes on fd 4.
    let ((cmd_read, cmd_write), (out_read, out_write)) = (pipe()?, pipe()?);
    let mut cmd = LaneCommand::new(&browser.path);
    cmd.args(chromium::launch_args(browser, profile, angle))
        .stdin(ChildStdio::Null)
        .stdout(out)
        .stderr(err);
    let t0 = Instant::now();
    let child = spawn_with_two_fds(cmd, OwnedFd::from(cmd_read), OwnedFd::from(out_write))
        .map_err(|e| {
            Stop::CouldNotRun(format!("could not start {}: {e}", browser.path.display()))
        })?;
    let mut running = Running(child);
    let mut cdp = cdp::Cdp::new(cmd_write, out_read);
    let version = cdp
        .call(None, "Browser.getVersion", json!({}))
        .map_err(|e| {
            Stop::CouldNotRun(format!(
                "the browser did not answer: {e}\n{}",
                log_tail(&log)
            ))
        })?;
    let ready_ms = ms(t0);
    let product = version
        .get("product")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let server = serve::Server::start(site).map_err(Stop::CouldNotRun)?;
    let cpu = page(&mut cdp, &server.origin(), "cpu")?;
    let gpu = page(&mut cdp, &server.origin(), "gpu")?;
    let worker = page(&mut cdp, &server.origin(), "worker");
    let served = server.served();
    drop(server);
    // Asked to close, the browser exits; one that does not is killed on drop.
    let _ = cdp.call(None, "Browser.close", json!({}));
    let waited = Instant::now();
    while waited.elapsed() < cdp::HANG_DETECTOR {
        if running.ended().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(running);
    Ok(Session {
        product,
        ready_ms: Some(ready_ms),
        cpu,
        gpu,
        worker,
        served,
    })
}

/// Launch Firefox on a fresh profile, under a scratch home, at the cpu page, which runs its lane,
/// POSTs its answer and opens the gpu page, which opens the worker page;
/// take the three answers and kill the browser by its pid.
fn drive_firefox(
    firefox: &firefox::Firefox,
    site: &Path,
    work: &Path,
    profile: &Path,
) -> Result<Session, Stop> {
    firefox::make_profile(profile).map_err(Stop::CouldNotRun)?;
    let log = work.join("browser.log");
    let (out, err) = log_stdio(&log)?;
    let mut server = serve::Server::start(site).map_err(Stop::CouldNotRun)?;
    let url = format!("{}/cpu.html?post=1&next=gpu,worker", server.origin());
    let home = ScratchHome(firefox::home_of(profile));
    let mut cmd = LaneCommand::new(&firefox.path);
    cmd.args(firefox::launch_args(profile, &url))
        .env_exactly(firefox::launch_env(std::env::vars_os(), &home.0))
        .stdin(ChildStdio::Null)
        .stdout(out)
        .stderr(err);
    let child = spawn(cmd).map_err(|e| {
        Stop::CouldNotRun(format!("could not start {}: {e}", firefox.path.display()))
    })?;
    let mut running = Running(child);
    let mut since = Instant::now();
    let mut take = |kind: &str| -> Result<Page, Stop> {
        let body =
            server
                .answer(kind, || running.ended().is_none())
                .map_err(|why| match running.ended() {
                    // A browser gone before the first page answered never ran it.
                    Some(status) if kind == "cpu" => Stop::CouldNotRun(format!(
                        "Firefox exited ({status}) before the cpu page answered\n{}",
                        log_tail(&log)
                    )),
                    _ => Stop::Fail(format!("the {kind} page: {why}\n{}", log_tail(&log))),
                })?;
        let evaluate_ms = ms(since);
        since = Instant::now();
        posted(kind, &body).map(|answer| Page {
            answer,
            load_ms: None,
            evaluate_ms,
        })
    };
    let cpu = take("cpu")?;
    let gpu = take("gpu")?;
    let worker = take("worker");
    let served = server.served();
    drop(server);
    drop(running);
    drop(home);
    Ok(Session {
        product: firefox::product(&gpu.answer.user_agent),
        ready_ms: None,
        cpu,
        gpu,
        worker,
        served,
    })
}

/// Firefox's scratch home, removed ([`firefox::remove_home`]) when dropped:
/// declared before the browser's [`Running`], it outlives the browser on
/// every path out of [`drive_firefox`].
struct ScratchHome(PathBuf);

impl Drop for ScratchHome {
    fn drop(&mut self) {
        firefox::remove_home(&self.0);
    }
}

/// A posted answer: `{ answer }`, parsed, or the page's `{ error }` as a
/// finding.
fn posted(kind: &str, body: &[u8]) -> Result<judge::Answer, Stop> {
    let fail = |what: String| Stop::Fail(format!("the {kind} page: {what}"));
    let v: Value =
        aterm_json::from_slice(body).map_err(|e| fail(format!("its answer is not JSON: {e}")))?;
    if let Some(error) = v.get("error") {
        return Err(fail(error.as_str().unwrap_or("failed").to_string()));
    }
    let answer = v
        .get("answer")
        .ok_or_else(|| fail("it posted no answer".to_string()))?;
    judge::parse(answer).map_err(fail)
}

/// Open `<origin>/<kind>.html` in a fresh tab, wait for its load, run its
/// lane and close the tab: the parsed answer. A page that throws or hangs is
/// a finding, its console printed with it.
fn page(cdp: &mut cdp::Cdp, origin: &str, kind: &str) -> Result<Page, Stop> {
    let fail = |what: String| Stop::Fail(format!("the {kind} page: {what}"));
    let target = cdp
        .call(None, "Target.createTarget", json!({"url": "about:blank"}))
        .map_err(Stop::CouldNotRun)?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
        )
        .map_err(Stop::CouldNotRun)?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for method in ["Page.enable", "Runtime.enable"] {
        cdp.call(Some(&session), method, json!({}))
            .map_err(Stop::CouldNotRun)?;
    }
    let t0 = Instant::now();
    let nav = cdp
        .call(
            Some(&session),
            "Page.navigate",
            json!({"url": format!("{origin}/{kind}.html")}),
        )
        .map_err(fail)?;
    if let Some(err) = nav.get("errorText").and_then(Value::as_str) {
        return Err(fail(format!("navigation failed: {err}")));
    }
    cdp.wait_event(&session, "Page.loadEventFired")
        .map_err(fail)?;
    let load_ms = ms(t0);
    let expression = format!(
        "window.atermLane({}, {})",
        aterm_json::to_string(&kind).unwrap_or_default(),
        aterm_json::to_string(&spec()).unwrap_or_default()
    );
    let t0 = Instant::now();
    let result = cdp.call(
        Some(&session),
        "Runtime.evaluate",
        json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
    );
    let evaluate_ms = ms(t0);
    let console = cdp.take_console(&session);
    let _ = cdp.call(None, "Target.closeTarget", json!({"targetId": target_id}));
    let with_console = |why: String| {
        if console.is_empty() {
            why
        } else {
            format!("{why}\n  its console:\n    {}", console.join("\n    "))
        }
    };
    let answer = result
        .and_then(|r| cdp::evaluated(&r))
        .and_then(|v| judge::parse(&v))
        .map_err(|e| fail(with_console(e)))?;
    Ok(Page {
        answer,
        load_ms: Some(load_ms),
        evaluate_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The readback split sums each step apart and names the frame whose
    /// read took longest, and the total counts what no step timed.
    #[test]
    fn a_page_s_timings_split_its_readbacks_from_its_frames() {
        let t = json!({
            "frame_ms": [1.0, 2.0, 1.0, 1.0],
            "finish_ms": [3.0, 0.0, 0.0, 0.0, 0.0],
            "copy_ms": [0.5, 0.5, 0.5, 0.5, 1.0],
            "read_ms": [1.0, 1.0, 1.0, 1.0, 9.0],
            "encode_ms": [0.25, 0.25, 0.25, 0.25, 1.0],
            "frame_wait_ms": 10.0,
            "total_ms": 43.0,
        });
        assert_eq!(timed(&t, "read_ms"), Some(13.0));
        assert_eq!(timed(&t, "frame_wait_ms"), Some(10.0));
        assert_eq!(timed(&t, "probe_ms"), None);
        let split = readback_split(&t).expect("split");
        assert!(
            split.contains(
                "finish 3.0 ms (most 3.0 on parity-0), copy 3.0 ms, getImageData 13.0 ms (most \
                 9.0 on diff-0)"
            ),
            "{split}"
        );
        let page = Page {
            answer: judge::Answer {
                module: "m".to_string(),
                adapter: "a".to_string(),
                user_agent: String::new(),
                frames: Vec::new(),
                diff: Vec::new(),
                readback: None,
                timings: t,
                init: None,
                drawn: None,
            },
            load_ms: None,
            evaluate_ms: 0.0,
        };
        // 43 less 5 (frames), 3 (finishes), 3 (copies), 13 (reads), 2
        // (base64) and 10 (waits).
        assert!(
            timing_line("gpu", &page).ends_with("7.0 in no timed step)"),
            "{}",
            timing_line("gpu", &page)
        );
        assert!(readback_split(&json!({})).is_none());
    }

    /// A posted answer parses, and a page's error is a finding naming it.
    #[test]
    fn a_posted_answer_or_error_is_read_back() {
        let ok = br#"{"answer": {"module": "aterm-wasm", "adapter": "cpu", "frames": []}}"#;
        assert!(matches!(posted("cpu", ok), Ok(a) if a.module == "aterm-wasm"));
        let failed = br#"{"error": "init_offscreen(canvas): no WebGL2"}"#;
        assert!(matches!(
            posted("worker", failed),
            Err(Stop::Fail(why)) if why == "the worker page: init_offscreen(canvas): no WebGL2"
        ));
        assert!(matches!(posted("gpu", b"{}"), Err(Stop::Fail(why)) if why.contains("no answer")));
        assert!(
            matches!(posted("gpu", b"<html>"), Err(Stop::Fail(why)) if why.contains("not JSON"))
        );
    }
}
