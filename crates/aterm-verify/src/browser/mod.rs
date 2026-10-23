// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify browser`: both web modules in real headless browsers.
//!
//! The modules are `verify wasm-smoke`'s dev build ([`crate::wasm_smoke::
//! build_dev`], a STOCK EXCEPTION: Trust has no wasm32 std), bound for a page
//! by `verify glue`. [`mod@page`] runs the wasm lanes' drivers in the same
//! Chromium.
pub(crate) mod cdp;
pub(crate) mod chromium;
pub(crate) mod firefox;
pub(crate) mod judge;
pub(crate) mod page;
pub(crate) mod recorded;
pub(crate) mod serve;
pub(crate) mod webkit;

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand, spawn, spawn_with_two_fds};

use crate::ladder::Report;
use crate::perf::Status;
use crate::{Ctx, EnvSnapshot, exit};

const LABEL: &str =
    "browser (both web modules in headless browsers: a WebGL2 boot, GPU vs CPU pixels)";

/// `cpu_gpu_parity.rs`'s grid.
const ROWS: u16 = 6;
const COLS: u16 = 12;
/// `cpu_gpu_parity.rs`'s font size.
const PX: f32 = 18.0;
/// `cpu_gpu_parity.rs`'s web theme: fg, bg, cursor, selection (`0x00RRGGBB`).
const THEME: [u32; 4] = [0x00E0_E0E0, 0x001E_1E2E, 0x00FF_FFFF, 0x0030_4060];
/// `cpu_gpu_parity.rs`'s content, one line per frame.
const FRAMES: [&str; 4] = [
    "\x1b[31mRR\x1b[0m\r\n",
    "\x1b[44m  \x1b[0m\r\n",
    "\x1b[7mXX\x1b[0m\r\n",
    "ab\r\n",
];

/// Recorded-frame differential grid dimensions.
const DIFF_ROWS: u16 = 8;
const DIFF_COLS: u16 = 40;
/// Recorded-frame differential content, one line per frame.
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

/// GPU page frames keyed by recording name (`parity-<k>`, then `diff-<k>`).
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
    /// Time from navigation to the load event over DevTools.
    load_ms: Option<f64>,
    /// Wait time for the page's evaluation or posted answer.
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
    /// WebKit, driven by its pages; it keeps a window.
    WebKit(webkit::WebKit),
}

impl Engine {
    /// The session's name: its directory, and the tag on its lines.
    const fn name(&self) -> &'static str {
        match self {
            Engine::Chromium(_, angle) => angle.name(),
            Engine::Firefox(_) => "firefox",
            Engine::WebKit(_) => "webkit",
        }
    }

    /// What the session's rows add to the lane's label.
    fn label(&self) -> String {
        match self {
            Engine::Chromium(_, angle) => format!("WebGL2 on ANGLE {}", angle.name()),
            Engine::Firefox(_) => "Firefox".to_string(),
            Engine::WebKit(_) => "WebKit".to_string(),
        }
    }

    /// Why `gpu` was not drawn by this engine, if it fell back or mismatched.
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
            Engine::WebKit(_) => (!webkit::is_webkit(&gpu.user_agent)).then(|| {
                format!(
                    "the browser answered as \"{}\", not WebKit: no webkit session ran",
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
            Engine::WebKit(webkit) => drive_webkit(webkit, site, work, profile),
        }
    }
}

/// Every session's name ([`Engine::name`]), as `browser-drive --engine` takes it.
pub(crate) const ENGINES: [&str; 4] = ["metal", "swiftshader", "firefox", "webkit"];

/// WebKit session participation in a run.
enum WebKitRow {
    /// Run it on the WebKit found; without one it is NOT RUN.
    Run(Result<webkit::WebKit, String>),
    /// Named skip under `--no-windows`, optionally carrying the missing-WebKit fix.
    Skip(Option<&'static str>),
    /// Not asked for (`browser-drive` without `--engine webkit`): no row.
    Off,
}

/// Build, bind and serve both modules once, then boot the pages in each browser session.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let t0 = Instant::now();
    let (chromium, firefox, webkit) = (chromium::find(), firefox::find(), webkit::find());
    let webkit_runs = webkit.is_ok() && !ctx.no_windows;
    if let (Err(c), Err(f), false) = (&chromium, &firefox, webkit_runs) {
        let w = match &webkit {
            Err(w) => format!("; {w}"),
            Ok(_) => String::new(),
        };
        r.cannot_run(format!(
            "{LABEL} — {c}; {f}{w} (fix: targo --unverified verify provision)"
        ));
        return;
    }
    let webkit = if ctx.no_windows {
        WebKitRow::Skip(webkit.is_err().then(webkit::fix))
    } else {
        WebKitRow::Run(webkit)
    };
    let work = crate::plan::cells_dir(ctx).join("browser");
    let site = work.join("site");
    let Some(cost) = build_site(ctx, r, &site, t0) else {
        return;
    };
    let built = Built {
        site,
        work,
        cost: Some(cost),
        scratch: ctx.scratch.clone(),
        ceiling: ctx.child_ceiling,
    };
    sessions(r, &built, &chromium, &firefox, webkit, &[]);
}

/// Build both wasm modules and assemble the site in `site`.
fn build_site(ctx: &Ctx, r: &mut Report, site: &Path, t0: Instant) -> Option<Cost> {
    let rustup = match crate::perf::wasm::build_tools(false) {
        Ok(rustup) => rustup,
        Err(refused) => {
            match refused.status {
                Status::Skip { why, fix } | Status::CouldNotRun { why, fix } => {
                    r.cannot_run(format!("{LABEL} — {why} (fix: {fix})"));
                }
                Status::Fail(findings) => {
                    r.fail(LABEL);
                    r.raw(findings.join("\n"));
                }
                _ => r.cannot_run(format!("{LABEL} — its tools answered no verdict")),
            }
            return None;
        }
    };
    let modules = crate::wasm_smoke::build_dev(ctx, r, &rustup, "browser", LABEL)?;
    let built_s = t0.elapsed().as_secs_f64();
    let t_bind = Instant::now();
    if let Err(why) = assemble(&ctx.root, &modules, site) {
        r.fail(format!("{LABEL} — {why}"));
        return None;
    }
    Some(Cost {
        built_s,
        bind_ms: ms(t_bind),
    })
}

/// Boot the pages in each requested browser session ([`ENGINES`]).
fn sessions(
    r: &mut Report,
    built: &Built,
    chromium: &Result<chromium::Browser, String>,
    firefox: &Result<firefox::Firefox, String>,
    webkit: WebKitRow,
    wanted: &[String],
) {
    let wants = |name: &str| wanted.is_empty() || wanted.iter().any(|w| w == name);
    for name in wanted {
        let here = chromium::angle_backends().iter().any(|a| a.name() == name);
        if name != "firefox" && name != "webkit" && !here {
            r.cannot_run(format!(
                "{LABEL}, WebGL2 on ANGLE {name} — this host's Chromium sessions are {}",
                chromium::angle_backends()
                    .iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ));
        }
    }
    let angles: Vec<chromium::Angle> = chromium::angle_backends()
        .iter()
        .copied()
        .filter(|a| wants(a.name()))
        .collect();
    let mut first: Option<(&'static str, Session)> = None;
    let mut run_one = |r: &mut Report, engine: Engine| {
        let session = session(r, built, &engine, first.as_ref());
        if first.is_none() {
            first = session.map(|s| (engine.name(), s));
        }
    };
    match chromium {
        _ if angles.is_empty() => {}
        Ok(browser) => {
            r.raw(format!(
                "browser: {} ({}), WebGL2 on ANGLE {}",
                browser.path.display(),
                browser.source,
                angles
                    .iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(" then ")
            ));
            for &angle in &angles {
                run_one(r, Engine::Chromium(browser.clone(), angle));
            }
        }
        Err(why) => r.cannot_run(format!(
            "{LABEL}, Chromium — {why} (fix: {})",
            chromium::FETCH_FIX
        )),
    }
    if wants("firefox") {
        match firefox {
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
    match webkit {
        WebKitRow::Off => {}
        WebKitRow::Skip(missing) => {
            r.skip(format!("{LABEL}, {}", webkit::no_windows_skip(missing)));
        }
        WebKitRow::Run(Ok(w)) => {
            r.raw(format!("webkit: {} ({})", w.path.display(), w.source));
            run_one(r, Engine::WebKit(w));
        }
        WebKitRow::Run(Err(why)) => {
            r.cannot_run(format!("{LABEL}, WebKit — {why} (fix: {})", webkit::fix()));
        }
    }
}

/// What building the modules and binding them cost.
#[derive(Clone, Copy)]
struct Cost {
    built_s: f64,
    bind_ms: f64,
}

/// Shared state across browser sessions in a run.
struct Built {
    site: PathBuf,
    work: PathBuf,
    cost: Option<Cost>,
    scratch: PathBuf,
    ceiling: Option<Duration>,
}

/// Files and directories laid by [`assemble`].
const SITE_FILES: [&str; 9] = [
    "cpu.html",
    "gpu.html",
    "worker.html",
    "lane.mjs",
    "worker.mjs",
    "spec.json",
    "font.ttf",
    "cpu",
    "gpu",
];

/// First entry of [`SITE_FILES`] missing from `site`, if any.
fn site_lacks(site: &Path) -> Option<&'static str> {
    SITE_FILES.into_iter().find(|f| !site.join(f).exists())
}

/// Render `r` and return its exit status (`0`, `1`, or `3`).
fn finish(r: &Report) -> i32 {
    print!("{}", r.render());
    let t = crate::ladder::tally(std::slice::from_ref(r));
    if !t.gate_failures.is_empty() {
        exit::FAILED
    } else if !t.could_not_run.is_empty() || r.outcomes().next().is_none() {
        exit::COULD_NOT_RUN
    } else {
        exit::PASS
    }
}

/// Entry point for `verify browser-site DIR`.
pub fn site_main(args: &[String], cwd: &Path) -> i32 {
    let [dir] = args else {
        eprintln!("verify browser-site: takes one argument, the directory to lay the site in");
        return exit::USAGE;
    };
    let site = cwd.join(dir);
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify browser-site: COULD NOT RUN — {} is not inside a git checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    let scratch = match crate::mktemp_dir("atv") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("verify browser-site: COULD NOT RUN — no scratch directory: {e}");
            return exit::COULD_NOT_RUN;
        }
    };
    let ctx = Ctx::new(
        root,
        crate::scope::Scope::workspace(),
        EnvSnapshot::capture(),
        scratch.clone(),
    );
    let mut r = Report::new("browser-site");
    if let Some(cost) = build_site(&ctx, &mut r, &site, Instant::now()) {
        r.pass(format!(
            "{LABEL}: the site laid in {} (built in {:.1} s, bound in {:.0} ms)",
            site.display(),
            cost.built_s,
            cost.bind_ms
        ));
    }
    let _ = std::fs::remove_dir_all(&scratch);
    finish(&r)
}

/// `verify browser-drive SITE OUT [--engine NAME]...`.
pub fn drive_main(args: &[String]) -> i32 {
    let usage = |why: String| {
        eprintln!(
            "verify browser-drive: {why} (verify browser-drive SITE OUT [--engine {}]...)",
            ENGINES.join("|")
        );
        exit::USAGE
    };
    let mut paths = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--engine" {
            let Some(name) = it.next().filter(|n| ENGINES.contains(&n.as_str())) else {
                return usage("--engine names a session".to_string());
            };
            if !wanted.contains(name) {
                wanted.push(name.clone());
            }
        } else if a.starts_with('-') {
            return usage(format!("unknown option `{a}`"));
        } else {
            paths.push(PathBuf::from(a));
        }
    }
    let [site, work] = &paths[..] else {
        return usage("takes a site and an output directory".to_string());
    };
    if let Some(missing) = site_lacks(site) {
        eprintln!(
            "verify browser-drive: COULD NOT RUN — {} holds no {missing}: lay it with `verify \
             browser-site`",
            site.display()
        );
        return exit::COULD_NOT_RUN;
    }
    let scratch = match crate::mktemp_dir("atv") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("verify browser-drive: COULD NOT RUN — no scratch directory: {e}");
            return exit::COULD_NOT_RUN;
        }
    };
    let built = Built {
        site: site.clone(),
        work: work.clone(),
        cost: None,
        scratch: scratch.clone(),
        ceiling: Some(crate::exec::DEFAULT_CHILD_CEILING),
    };
    let mut r = Report::new(format!("browser-drive ({})", site.display()));
    let webkit = if wanted.iter().any(|w| w == "webkit") {
        WebKitRow::Run(webkit::find())
    } else {
        WebKitRow::Off
    };
    sessions(
        &mut r,
        &built,
        &chromium::find(),
        &firefox::find(),
        webkit,
        &wanted,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    finish(&r)
}

/// Run and judge one browser session and record its rows in `r`.
fn session(
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
    let profile = built.scratch.join(format!("browser-profile-{name}"));
    let driven = crate::exec::bounded(built.ceiling, move || {
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
        "build_and_tools_s": built.cost.map_or(Value::Null, |c| json!((c.built_s * 1e3).round() / 1e3)),
        "bind_ms": built.cost.map_or(Value::Null, |c| json!(c.bind_ms)),
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

/// Judge one GPU run against CPU frames and the wgpu-WebGL `recording`.
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

/// Step keys summed to compute untimed page overhead.
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

/// Recording frame name (`parity-<k>` or `diff-<k>`) for frame index `k`.
fn frame_name(k: usize) -> String {
    match k.checked_sub(FRAMES.len()) {
        None => format!("parity-{k}"),
        Some(d) => format!("diff-{d}"),
    }
}

/// Format per-step readback timings (`finish`, `copy`, `getImageData`, `base64`).
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

/// Format a single-line timing summary for page `p`.
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

/// Assemble the browser test site and wasm glue in `site`.
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
        crate::wasm_glue::bind(&modules.join(module), &site.join(dir))?;
    }
    Ok(())
}

/// Running browser process killed and reaped on drop.
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

/// Launch Chromium on `angle` and drive the three pages over DevTools.
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
    let ((cmd_read, cmd_write), (out_read, out_write)) = (pipe()?, pipe()?);
    let mut cmd = LaneCommand::new(&browser.path);
    cmd.args(chromium::launch_args(browser, profile, angle, &[]))
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

/// Launch Firefox on a fresh profile and drive the pages via HTTP POST.
fn drive_firefox(
    firefox: &firefox::Firefox,
    site: &Path,
    work: &Path,
    profile: &Path,
) -> Result<Session, Stop> {
    if let Some(why) =
        firefox::display_missing(std::env::consts::OS, std::env::var_os("DISPLAY").as_deref())
    {
        return Err(Stop::CouldNotRun(why));
    }
    firefox::make_profile(profile).map_err(Stop::CouldNotRun)?;
    let home = ScratchHome(firefox::home_of(profile));
    let pages = drive_by_posts("Firefox", &firefox.path, site, work, &home, |url, home| {
        (
            firefox::launch_args(profile, url),
            firefox::launch_env(std::env::vars_os(), home),
        )
    })?;
    drop(home);
    let product = firefox::product(&pages.gpu.answer.user_agent);
    Ok(pages.session(product))
}

/// Launch WebKit on a fresh profile and drive the pages via HTTP POST.
fn drive_webkit(
    webkit: &webkit::WebKit,
    site: &Path,
    work: &Path,
    profile: &Path,
) -> Result<Session, Stop> {
    webkit::make_profile(profile).map_err(Stop::CouldNotRun)?;
    let home = ScratchHome(webkit::home_of(profile));
    let frameworks = webkit::frameworks_dir(&webkit.path);
    let pages = drive_by_posts("WebKit", &webkit.path, site, work, &home, |url, home| {
        (
            webkit::launch_args(profile, url),
            webkit::launch_env(std::env::vars_os(), home, &frameworks),
        )
    })?;
    drop(home);
    let product = webkit::product(&pages.gpu.answer.user_agent);
    Ok(pages.session(product))
}

/// The three answers a browser driven by its pages posted.
struct PostedPages {
    cpu: Page,
    gpu: Page,
    worker: Result<Page, Stop>,
    served: usize,
}

impl PostedPages {
    fn session(self, product: String) -> Session {
        Session {
            product,
            ready_ms: None,
            cpu: self.cpu,
            gpu: self.gpu,
            worker: self.worker,
            served: self.served,
        }
    }
}

/// Serve `site`, launch `browser`, and collect the three posted page answers.
fn drive_by_posts(
    what: &str,
    browser: &Path,
    site: &Path,
    work: &Path,
    home: &ScratchHome,
    launch: impl FnOnce(&str, &Path) -> (Vec<OsString>, Vec<(OsString, OsString)>),
) -> Result<PostedPages, Stop> {
    let log = work.join("browser.log");
    let (out, err) = log_stdio(&log)?;
    let mut server = serve::Server::start(site).map_err(Stop::CouldNotRun)?;
    let url = format!("{}/cpu.html?post=1&next=gpu,worker", server.origin());
    let (args, env) = launch(&url, &home.0);
    let mut cmd = LaneCommand::new(browser);
    cmd.args(args)
        .env_exactly(env)
        .stdin(ChildStdio::Null)
        .stdout(out)
        .stderr(err);
    let child = spawn(cmd)
        .map_err(|e| Stop::CouldNotRun(format!("could not start {}: {e}", browser.display())))?;
    let mut running = Running(child);
    let mut since = Instant::now();
    let mut take = |kind: &str| -> Result<Page, Stop> {
        let body =
            server
                .answer(kind, || running.ended().is_none())
                .map_err(|why| match running.ended() {
                    Some(status) if kind == "cpu" => Stop::CouldNotRun(format!(
                        "{what} exited ({status}) before the cpu page answered\n{}",
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
    Ok(PostedPages {
        cpu,
        gpu,
        worker,
        served,
    })
}

/// Scratch home directory removed ([`firefox::remove_home`]) on drop.
struct ScratchHome(PathBuf);

impl Drop for ScratchHome {
    fn drop(&mut self) {
        firefox::remove_home(&self.0);
    }
}

/// Parse a posted page answer or error payload.
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

/// Open `<origin>/<kind>.html` in a fresh DevTools target, run its lane, and close the tab.
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

/// One pinned browser `verify provision` places in the browser cache ([`chromium::host_cache_root`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pin {
    /// What it is, for the run's lines.
    pub what: String,
    /// The version the lane pins.
    pub version: String,
    /// Where its archive is served.
    pub url: String,
    /// The archive's sha256.
    pub sha256: &'static str,
    /// The archive's size.
    pub bytes: u64,
    /// Its directory under the cache root.
    pub dir: PathBuf,
    /// Its executable, relative to `dir`.
    pub binary: PathBuf,
}

/// The pin of `engine` for `(os, arch)` under the browser cache `root`, where the lane pins one for that host.
pub(crate) fn pin_for(
    engine: crate::provision::Engine,
    os: &str,
    arch: &str,
    root: &Path,
) -> Option<Pin> {
    use crate::provision::Engine;
    match engine {
        Engine::Chromium => {
            let platform = chromium::platform_for(os, arch)?;
            let pin = chromium::PINNED.iter().find(|p| p.platform == platform)?;
            Some(Pin {
                what: format!("chrome-headless-shell {}", chromium::PIN_VERSION),
                version: chromium::PIN_VERSION.to_string(),
                url: chromium::archive_url(platform),
                sha256: pin.sha256,
                bytes: pin.bytes,
                dir: chromium::pinned_dir(root, platform),
                binary: chromium::pinned_binary(Path::new(""), platform),
            })
        }
        Engine::Firefox => {
            let platform = firefox::platform_for(os, arch)?;
            let pin = firefox::PINNED.iter().find(|p| p.platform == platform)?;
            Some(Pin {
                what: format!(
                    "Playwright's Firefox build {} (Firefox {})",
                    firefox::BUILD,
                    firefox::VERSION
                ),
                version: firefox::BUILD.to_string(),
                url: firefox::archive_url(pin),
                sha256: pin.sha256,
                bytes: pin.bytes,
                dir: firefox::pinned_dir(root, pin),
                binary: PathBuf::from(pin.binary),
            })
        }
        Engine::Webkit => {
            let platform = webkit::platform_for(os, arch)?;
            let pin = webkit::PINNED.iter().find(|p| p.platform == platform)?;
            Some(Pin {
                what: format!(
                    "Playwright's WebKit build {} (WebKit {})",
                    webkit::BUILD,
                    webkit::VERSION
                ),
                version: webkit::BUILD.to_string(),
                url: webkit::archive_url(pin),
                sha256: pin.sha256,
                bytes: pin.bytes,
                dir: webkit::pinned_dir(root, pin),
                binary: PathBuf::from(pin.binary),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `readback_split` sums each step and `timing_line` reports untimed overhead.
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
        assert!(
            timing_line("gpu", &page).ends_with("7.0 in no timed step)"),
            "{}",
            timing_line("gpu", &page)
        );
        assert!(readback_split(&json!({})).is_none());
    }

    /// `site_lacks` and `drive_main` reject incomplete sites and invalid CLI arguments.
    #[test]
    fn a_drive_needs_a_laid_site_and_named_sessions() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let site = dir.path().join("site");
        std::fs::create_dir_all(&site).expect("site");
        assert_eq!(site_lacks(&site), Some("cpu.html"));
        for f in SITE_FILES {
            if f == "cpu" || f == "gpu" {
                std::fs::create_dir_all(site.join(f)).expect("glue dir");
            } else {
                std::fs::write(site.join(f), "x").expect("file");
            }
        }
        assert_eq!(site_lacks(&site), None);
        std::fs::remove_dir_all(site.join("gpu")).expect("rm gpu");
        assert_eq!(site_lacks(&site), Some("gpu"));
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let site_arg = site.display().to_string();
        assert_eq!(drive_main(&s(&[&site_arg])), exit::USAGE);
        assert_eq!(
            drive_main(&s(&[&site_arg, "/o", "--engine", "safari"])),
            exit::USAGE
        );
        assert_eq!(drive_main(&s(&[&site_arg, "/o", "--fast"])), exit::USAGE);
        for engine in ["firefox", "webkit"] {
            assert_eq!(
                drive_main(&s(&[&site_arg, "/o", "--engine", engine])),
                exit::COULD_NOT_RUN,
                "{engine}: a site without its gpu glue"
            );
        }
        assert_eq!(site_main(&s(&[]), dir.path()), exit::USAGE);
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
