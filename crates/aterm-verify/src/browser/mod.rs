// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify browser`: both web modules in a real headless Chromium, driven
//! over the DevTools protocol (docs/AUDIT-third-party-2026-10-15.md,
//! wasm-platform WP.5 and WP.8: "one real WebGL2 boot").
//!
//! The modules are `verify wasm-smoke`'s dev build ([`crate::wasm_smoke::
//! build_dev`], a STOCK EXCEPTION: Trust has no wasm32 std), bound for a page
//! by `verify glue --flavor web`. A loopback server ([`serve`]) serves them
//! with the pages in `crates/aterm-verify/browser/` and the bundled font.
//! The browser ([`chromium`]) runs headless with WebGL2 on ANGLE (Metal on
//! macOS, SwiftShader elsewhere) and its DevTools on two pipes ([`cdp`]).
//!
//! Two pages, each fed crates/aterm-gpu-web/tests/cpu_gpu_parity.rs's grid,
//! theme, px, font and content, one line per frame:
//! - `cpu.html` boots aterm-wasm and hands back each frame's `rgba()`;
//! - `gpu.html` boots aterm-gpu-web, `await init(canvas)` (aterm-gpu's GL
//!   arm on WebGL2 through the module's `aterm_gl` imports, wasm-platform
//!   WP.8), presents each frame with `render()` and hands back the canvas.
//!   `init` is synchronous: the page reports whether the GPU was ready as
//!   the call returned, and whether an `init` that throws (on a canvas
//!   already holding a 2D context) rejects the async fn awaiting it.
//!
//! [`judge`] holds each GPU frame to the CPU frame within that test's
//! tolerance, with a negative control, and the GPU page's `init` report to
//! both answers. The GPU page then draws a wider grid of attributes, colours,
//! sprites and an image on a second canvas, and [`recorded`] holds every GPU
//! frame to the frames the wgpu-WebGL module drew for the same content
//! (`browser/wgpu-webgl/`, recorded before WP.8): byte for byte on the
//! recording's adapter. Each run's GPU frames are written to
//! `<target>/verify/cells/browser/gpu-frames/`. The timings each page
//! measured are printed and written to
//! `<target>/verify/cells/browser/timings.json`.
//!
//! No browser or no stable wasm32 std is COULD NOT RUN with the fix, never a
//! pass. In no set.

pub(crate) mod cdp;
pub(crate) mod chromium;
pub(crate) mod judge;
pub(crate) mod recorded;
pub(crate) mod serve;

pub use chromium::fetch_main;

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand, spawn_with_two_fds};

use crate::Ctx;
use crate::ladder::Report;
use crate::perf::Status;
use crate::plan::{Lane, lane_dir};

const LABEL: &str =
    "browser (both web modules in headless Chromium: a WebGL2 boot, GPU vs CPU pixels)";

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

/// Why a session ended without answers.
enum Stop {
    /// The machine, not the tree: the browser did not start or answer.
    CouldNotRun(String),
    /// A finding about the tree: a page failed or hung.
    Fail(String),
}

/// One page's answer and what it cost.
struct Page {
    answer: judge::Answer,
    load_ms: f64,
    evaluate_ms: f64,
}

/// What a session gathered.
struct Session {
    product: String,
    ready_ms: f64,
    cpu: Page,
    gpu: Page,
    served: usize,
}

fn ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// Build, bind and serve both modules, boot both pages, judge; one row.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let t0 = Instant::now();
    let browser = match chromium::find() {
        Ok(b) => b,
        Err(why) => {
            r.cannot_run(format!("{LABEL} — {why} (fix: {})", chromium::FETCH_FIX));
            return;
        }
    };
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
    let bind_ms = ms(t_bind);
    r.raw(format!(
        "browser: {} ({}), WebGL2 on ANGLE {}",
        browser.path.display(),
        browser.source,
        chromium::angle_backend()
    ));
    let (session_browser, session_site, session_work) =
        (browser.clone(), site.clone(), work.clone());
    let profile = ctx.scratch.join("browser-profile");
    let driven = crate::exec::bounded(ctx.child_ceiling, move || {
        drive(&session_browser, &session_site, &session_work, &profile)
    });
    let session = match driven {
        Ok(Ok(s)) => s,
        Ok(Err(Stop::CouldNotRun(why))) => {
            r.cannot_run(format!("{LABEL} — {why}"));
            return;
        }
        Ok(Err(Stop::Fail(why))) => {
            r.fail(LABEL);
            r.raw(why);
            return;
        }
        Err(why) => {
            r.fail(LABEL);
            r.raw(format!("the browser session {}", why.sentence()));
            return;
        }
    };
    let verdict = judge::judge(&session.cpu.answer, &session.gpu.answer, FRAMES.len());
    let timings = json!({
        "browser": session.product,
        "browser_path": browser.path.display().to_string(),
        "angle": chromium::angle_backend(),
        "adapter": session.gpu.answer.adapter,
        "build": "dev (wasm32, rustup stable)",
        "build_and_tools_s": (built_s * 1e3).round() / 1e3,
        "bind_ms": bind_ms,
        "browser_ready_ms": session.ready_ms,
        "requests_served": session.served,
        "cpu": page_timings(&session.cpu),
        "gpu": page_timings(&session.gpu),
    });
    r.raw(format!(
        "browser: {} ready in {:.0} ms; GPU adapter {}",
        session.product, session.ready_ms, session.gpu.answer.adapter
    ));
    r.raw(timing_line("cpu page (aterm-wasm)", &session.cpu));
    r.raw(timing_line("gpu page (aterm-gpu-web)", &session.gpu));
    let record = work.join("timings.json");
    match aterm_json::to_string(&timings).map(|t| std::fs::write(&record, t + "\n")) {
        Ok(Ok(())) => r.raw(format!("browser: timings in {}", record.display())),
        _ => r.raw(format!("browser: could not write {}", record.display())),
    }
    for line in &verdict.lines {
        r.raw(format!("browser: {line}"));
    }
    let mut failures = verdict.failures;
    // The recorded-frame differential: this run's GPU frames, written where
    // they can be looked at (and recorded from), then held to the
    // wgpu-WebGL module's recording.
    let named = named_gpu_frames(&session.gpu.answer);
    let seen = recorded::Recording {
        module: session.gpu.answer.module.clone(),
        browser: session.product.clone(),
        adapter: session.gpu.answer.adapter.clone(),
        frames: named
            .iter()
            .map(|(n, f)| (n.clone(), (*f).clone()))
            .collect(),
    };
    let frames_dir = work.join("gpu-frames");
    match recorded::write(&frames_dir, &seen) {
        Ok(()) => r.raw(format!(
            "browser: this run's GPU frames in {}",
            frames_dir.display()
        )),
        Err(why) => r.raw(format!("browser: could not write the GPU frames: {why}")),
    }
    match recorded::read(&recorded::wgpu_webgl_dir()) {
        Ok(rec) => {
            let diff = recorded::judge(&rec, &session.gpu.answer.adapter, &named);
            for line in &diff.lines {
                r.raw(format!("browser: {line}"));
            }
            failures.extend(diff.failures);
        }
        Err(why) => failures.push(format!("the wgpu-WebGL recording: {why}")),
    }
    if failures.is_empty() {
        r.pass(LABEL);
    } else {
        r.fail(LABEL);
        r.raw(failures.join("\n"));
    }
}

fn page_timings(p: &Page) -> Value {
    json!({
        "module": p.answer.module,
        "load_ms": p.load_ms,
        "evaluate_ms": p.evaluate_ms,
        "page": p.answer.timings,
    })
}

/// One page's timings on one line: what the page measured, in its order.
fn timing_line(who: &str, p: &Page) -> String {
    let t = &p.answer.timings;
    let num = |k: &str| t.get(k).and_then(Value::as_f64);
    let mut parts = vec![format!("load {:.1} ms", p.load_ms)];
    for (k, name) in [
        ("module_init_ms", "module"),
        ("construct_ms", "construct"),
        ("gpu_init_ms", "init(canvas)"),
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
    if let Some(v) = num("total_ms") {
        let wait = num("frame_wait_ms").map_or_else(String::new, |w| {
            format!(" ({w:.1} of them waiting on animation frames)")
        });
        parts.push(format!("total {v:.1} ms{wait}"));
    }
    format!("browser: {who}: {}", parts.join(", "))
}

/// The site directory: the pages, the bundled font, the CPU module's glue
/// in `cpu/` and the GPU module's in `gpu/`.
fn assemble(root: &Path, modules: &Path, site: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(site);
    std::fs::create_dir_all(site)
        .map_err(|e| format!("could not create {}: {e}", site.display()))?;
    let font = root.join("crates/aterm-render/assets/DejaVuSansMono.ttf");
    let copies = ["cpu.html", "gpu.html", "lane.mjs"]
        .map(|p| (pages_dir().join(p), site.join(p)))
        .into_iter()
        .chain([(font, site.join("font.ttf"))]);
    for (from, to) in copies {
        std::fs::copy(&from, &to)
            .map_err(|e| format!("could not copy {} to {}: {e}", from.display(), to.display()))?;
    }
    for (module, dir) in [("aterm_wasm.wasm", "cpu"), ("aterm_gpu_web.wasm", "gpu")] {
        crate::wasm_glue::bind(
            &modules.join(module),
            &site.join(dir),
            crate::wasm_glue::Flavor::Web,
        )?;
    }
    Ok(())
}

/// The browser, killed and reaped on drop if it is still running.
struct Running(LaneChild);

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

/// Launch the browser, serve `site`, boot both pages and close the browser.
fn drive(
    browser: &chromium::Browser,
    site: &Path,
    work: &Path,
    profile: &Path,
) -> Result<Session, Stop> {
    let _ = std::fs::remove_dir_all(profile);
    std::fs::create_dir_all(profile)
        .map_err(|e| Stop::CouldNotRun(format!("could not create {}: {e}", profile.display())))?;
    let log = work.join("browser.log");
    let log_file = std::fs::File::create(&log)
        .map_err(|e| Stop::CouldNotRun(format!("could not create {}: {e}", log.display())))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| Stop::CouldNotRun(format!("could not share {}: {e}", log.display())))?;
    let pipe = || std::io::pipe().map_err(|e| Stop::CouldNotRun(format!("no pipe: {e}")));
    // The browser reads commands on fd 3 and writes on fd 4.
    let ((cmd_read, cmd_write), (out_read, out_write)) = (pipe()?, pipe()?);
    let mut cmd = LaneCommand::new(&browser.path);
    cmd.args(chromium::launch_args(browser, profile))
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Fd(OwnedFd::from(log_file)))
        .stderr(ChildStdio::Fd(OwnedFd::from(log_err)));
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
    let served = server.served();
    drop(server);
    // Asked to close, the browser exits; one that does not is killed on drop.
    let _ = cdp.call(None, "Browser.close", json!({}));
    let waited = Instant::now();
    while waited.elapsed() < cdp::HANG_DETECTOR {
        if !matches!(running.0.try_wait(), Ok(None)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(running);
    Ok(Session {
        product,
        ready_ms,
        cpu,
        gpu,
        served,
    })
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
        load_ms,
        evaluate_ms,
    })
}
