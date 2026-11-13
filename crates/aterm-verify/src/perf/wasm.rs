// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WebAssembly renderer performance lane (`aterm-wasm` and `aterm-gpu-web`).
//!
//! STOCK EXCEPTION (Trust lacks a wasm32 std: `error[E0463]` can't find crate
//! for `core`): the build rides the checkout's stock pin
//! ([`crate::rustup::stock_pin`], the compiler the site's /terminal engine ships from),
//! resolved to its own `cargo`/`rustc` by path, since a stock or rerouted `cargo` earlier on
//! PATH ignores `RUSTUP_TOOLCHAIN`. It runs from a neutral cwd so no repo
//! config (vendored source replacement, Trust `-Z` flags) applies.

use aterm_uds::output::CommandOutput as _;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::exec::check_command as command;
use aterm_json::{Value, json};

use super::LaneRun;
use super::floors::{self, FloorLane};
use super::workspace_root;
use crate::rustup::Toolchain;
use crate::wasm_page::{self, Page, Refused};

use crate::wasm_glue::WASM_OPT_FLAGS;

pub(crate) const WASM_LANE: FloorLane = FloorLane {
    lane: "wasm",
    example: "",
    keys: &[
        "wasm_cpu_ingest_mixed_mbps",
        "wasm_cpu_ingest_replog_mbps",
        "wasm_cpu_ingest_str_ascii_mbps",
        "wasm_cpu_ingest_str_nonascii_mbps",
        "wasm_cpu_scroll_present_fps",
        "wasm_cpu_typing_present_fps",
        "wasm_cpu_search_build_klps",
        "wasm_cpu_search_query_qps",
        "wasm_cpu_restore_hz",
        "wasm_cpu_uniform_flood_present_fps",
        "wasm_cpu_scrolled_stream_present_fps",
        "wasm_cpu_styled_scrub_present_fps",
        "wasm_gpu_ingest_mixed_mbps",
        "wasm_gpu_frame_build_fps",
        "wasm_gpu_scroll_frame_build_fps",
    ],
    baseline_file: "perf-baseline-wasm.json",
    comment: "aterm WASM-BENCH baseline: the SHIPPED wasm modules (CPU aterm-wasm + GPU \
              aterm-gpu-web) driven in the pinned chrome-headless-shell by \
              crates/aterm-verify/perf/wasm/bench.mjs. All BIGGER-IS-BETTER. Re-record with \
              targo --unverified verify perf --record wasm.",
    cli: "wasm",
};

/// CPU module size ceiling in bytes after `wasm-opt -O3`.
pub(crate) const WASM_CPU_MODULE_BYTES_CAP: f64 = 5_886_685.0;

/// GPU module size ceiling in bytes after `wasm-opt -O3`.
pub(crate) const WASM_GPU_MODULE_BYTES_CAP: f64 = 6_133_315.0;

fn module_build_plan(
    env: &crate::EnvSnapshot,
    source: &Path,
    artifacts: &Path,
) -> Result<crate::NativeBuildPlan, String> {
    let configured = env
        .cargo_layout
        .iter()
        .find(|(name, _)| *name == "CARGO_TARGET_DIR")
        .map(|(_, value)| value.as_os_str())
        .or_else(|| env.cargo_target_dir.as_deref().map(Path::as_os_str));
    match configured {
        Some(path) if path.is_empty() => Err("CARGO_TARGET_DIR is empty".into()),
        Some(path) => Ok(crate::NativeBuildPlan::for_target(
            env,
            source,
            &artifacts.join(path),
        )),
        None => Ok(crate::NativeBuildPlan::for_default_target(
            env, source, artifacts,
        )),
    }
}

struct BuiltModules {
    target: PathBuf,
    directory: PathBuf,
}

fn run_module_build(
    plan: &crate::NativeBuildPlan,
    profile: &str,
    build: impl FnOnce() -> Result<(), String>,
) -> Result<BuiltModules, String> {
    plan.prepare().map_err(|error| error.to_string())?;
    build()?;
    let target = plan.target().to_path_buf();
    let profile = if profile == "dev" { "debug" } else { profile };
    let directory = target.join(WASM32).join(profile);
    Ok(BuiltModules { target, directory })
}

fn answers(program: &Path, args: &[&str]) -> bool {
    command(program)
        .args(args)
        .output_owned()
        .is_ok_and(|o| o.status.success())
}

/// Resolves the stock pin of the checkout at `repo`, with its wasm32 std, and optional
/// `wasm-opt` for building the WebAssembly modules.
pub(crate) fn build_tools(repo: &Path, wasm_opt: bool) -> Result<Toolchain, LaneRun> {
    build_tools_with(aterm_uds::host::tool("rustup").locate(), repo, wasm_opt)
}

/// [`build_tools`] with the `rustup` row's answer `found`: where it found none, COULD NOT
/// RUN saying where the row looked and naming its provider, `verify provision`, once; where
/// rustup holds no stock pin with a wasm32 std, COULD NOT RUN naming the same.
fn build_tools_with(
    found: Result<PathBuf, aterm_uds::host::Absent>,
    repo: &Path,
    wasm_opt: bool,
) -> Result<Toolchain, LaneRun> {
    let rustup = match found {
        Ok(rustup) => rustup,
        Err(absent) => {
            return Err(LaneRun::could_not_run(
                absent.why,
                format!("{}, with the stock pin's wasm32 std", absent.fix),
            ));
        }
    };
    let toolchain = crate::rustup::pinned(rustup.as_path(), repo)
        .and_then(|stock| stock.serving(WASM32, false).cloned())
        .map_err(|unready| LaneRun::could_not_run(unready.why, unready.fix))?;
    if wasm_opt && !answers(&wasm_opt_program(), &["--version"]) {
        return Err(LaneRun::could_not_run(
            "wasm-opt not found; the lane applies the shipping -O3 pass and never measures \
             pre-opt modules",
            aterm_uds::host::tool("wasm-opt").remedy(),
        ));
    }
    Ok(toolchain)
}

/// The target every web module is built for.
const WASM32: &str = "wasm32-unknown-unknown";

/// WASM floors: the shipped modules in the browser, every gated key against its floor, plus the
/// module-size ceilings.
pub(crate) fn gate_wasm(record: bool) -> LaneRun {
    let root = workspace_root();
    let browser = match wasm_page::browser() {
        Ok(browser) => browser,
        Err(refused) => return refused.verdict("the wasm bench"),
    };
    let toolchain = match build_tools(&root, true) {
        Ok(toolchain) => toolchain,
        Err(verdict) => return verdict,
    };
    let (product, json) = match build_and_bench(&root, &toolchain, &browser) {
        Ok(ran) => ran,
        Err(refused) => return refused.verdict("the wasm bench"),
    };
    let mut run = match floors_engine(&product, record) {
        Ok(()) => floors::judge_keyed(&WASM_LANE, &json, record),
        Err(other) => other,
    };
    let findings = wasm_module_fences(&json);
    if findings.is_empty() {
        run.fence_held(WASM_LANE.cli);
    }
    for finding in findings {
        run.add_failure(finding);
    }
    run
}

/// Static module ceilings need compilation and optimization, with no browser
/// execution or timing admission. The ordinary wasm lane also judges floors.
pub(crate) fn gate_wasm_size() -> LaneRun {
    let root = workspace_root();
    let toolchain = match build_tools(&root, true) {
        Ok(toolchain) => toolchain,
        Err(verdict) => return verdict,
    };
    let sizes = (|| {
        let built = build_modules(
            &root,
            &toolchain,
            "release",
            Some("-C target-feature=+simd128"),
        )?;
        let site = built.target.join("wasm-size");
        bind_modules(&built.directory, &site, true)?;
        module_sizes(&site)
    })();
    let (cpu, gpu) = match sizes {
        Ok(sizes) => sizes,
        Err(why) => return LaneRun::fail(why),
    };
    let report = json!({"wasm_cpu_module_bytes": cpu, "wasm_gpu_module_bytes": gpu});
    let mut run = LaneRun::pass();
    let findings = wasm_module_fences(&report);
    if findings.is_empty() {
        run.fence_held("wasm module sizes");
    }
    for finding in findings {
        run.add_failure(finding);
    }
    run
}

fn module_sizes(site: &Path) -> Result<(u64, u64), String> {
    let bytes = |module: &str| {
        let path = site.join(module);
        std::fs::symlink_metadata(&path)
            .map(|meta| meta.len())
            .map_err(|e| format!("could not read {}: {e}", path.display()))
    };
    Ok((
        bytes("cpu/aterm_wasm_bg.wasm")?,
        bytes("gpu/aterm_gpu_web_bg.wasm")?,
    ))
}

fn floors_engine(product: &str, record: bool) -> Result<(), LaneRun> {
    let pinned = pinned_product();
    if product == pinned {
        return Ok(());
    }
    let why = format!("the floors were recorded in {pinned}, and this run's browser is {product}");
    Err(if record {
        LaneRun::could_not_run(
            format!("--record wasm records in the pinned shell only: {why}"),
            format!("unset ATERM_BROWSER; {}", wasm_page::browser_fix()),
        )
    } else {
        LaneRun::not_judged(why)
    })
}

/// The pinned shell's product string, where this host drives one.
#[cfg(unix)]
fn pinned_product() -> String {
    crate::browser::chromium::pinned_product()
}

#[cfg(not(unix))]
fn pinned_product() -> String {
    String::new()
}

/// Re-records and verifies the CPU module boundary replay fixture.
pub(crate) fn record_replay(log: &mut String) -> LaneRun {
    let root = workspace_root();
    let browser = match wasm_page::browser() {
        Ok(browser) => browser,
        Err(refused) => return refused.verdict("the replay"),
    };
    let toolchain = match build_tools(&root, false) {
        Ok(toolchain) => toolchain,
        Err(verdict) => return verdict,
    };
    let mut record = || -> Result<(), Refused> {
        let built = build_modules(&root, &toolchain, "dev", None).map_err(Refused::Failed)?;
        let site = built.target.join("wasm-smoke");
        bind_modules(&built.directory, &site, false).map_err(Refused::Failed)?;
        wasm_page::lay_drivers(&root, &site).map_err(Refused::Failed)?;
        let mut page = Page::start(&browser, &site, &std::env::temp_dir())?;
        replay(&root, &site, &mut page, true, log)
    };
    match record() {
        Ok(()) => LaneRun::pass(),
        Err(refused) => refused.verdict("the replay"),
    }
}

/// Path to the CPU module's recorded boundary replay fixture.
pub(crate) fn replay_answers(root: &Path) -> PathBuf {
    root.join("crates/aterm-wasm/tests/fixtures/abi_replay.json")
}

/// Runs the CPU module boundary replay and its negative control in `page`.
pub(crate) fn replay(
    root: &Path,
    site: &Path,
    page: &mut Page,
    record: bool,
    log: &mut String,
) -> Result<(), Refused> {
    let failed = Refused::Failed;
    let answers = replay_answers(root);
    let driver = format!("{}/replay.mjs", wasm_page::DRIVERS);
    let run = |page: &mut Page, answers: Option<&str>, log: &mut String| {
        let ran = page.run(
            &driver,
            &json!({"glue": "../cpu/aterm_wasm.js", "font": format!("../{}", wasm_page::FONT),
                    "answers": answers.map(|a| format!("../{a}"))}),
        )?;
        for line in &ran.console {
            let _ = writeln!(log, "  {line}");
        }
        Ok::<_, Refused>(ran)
    };
    if record {
        let ran = run(page, None, log)?;
        let text = ran
            .value
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| failed("replay.mjs answered no answers to record".to_string()))?;
        crate::write_fresh(&answers, text)
            .map_err(|e| failed(format!("could not write {}: {e}", answers.display())))?;
        let _ = writeln!(log, "  wasm-replay: recorded {}", answers.display());
    }
    let text = crate::read_regular_text(&answers)
        .map_err(|e| failed(format!("could not read {}: {e}", answers.display())))?;
    let (planted, hash) = plant_frame_hash(&text)
        .ok_or_else(|| failed(format!("{}: no frame hash to plant", answers.display())))?;
    for (name, contents) in [("answers.json", &text), ("answers.planted.json", &planted)] {
        let path = site.join(name);
        crate::write_fresh(&path, contents)
            .map_err(|e| failed(format!("could not write {}: {e}", path.display())))?;
    }
    let differ = |ran: &wasm_page::Ran| ran.value.get("failures").and_then(Value::as_u64);
    let ran = run(page, Some("answers.json"), log)?;
    if differ(&ran) != Some(0) {
        return Err(failed(format!(
            "the replay's answers differ from {} (review the change, then `targo \
             --unverified verify glue --record-replay`)",
            answers.display()
        )));
    }
    let mut control = String::new();
    let ran = run(page, Some("answers.planted.json"), &mut control)?;
    if differ(&ran) != Some(1) || !control.contains(&hash) {
        return Err(failed(format!(
            "NEGATIVE CONTROL: the replay against answers with one frame hash planted \
             ({hash}) did not fail on exactly that answer:\n{control}"
        )));
    }
    log.push_str("  wasm-replay: negative control held: a planted frame hash fails the replay\n");
    Ok(())
}

fn plant_frame_hash(answers: &str) -> Option<(String, String)> {
    const KEY: &str = "\"rgba\":\"";
    let at = answers.find(KEY)? + KEY.len();
    let hash = answers.get(at..at + 64)?;
    let first = hash.chars().next()?;
    let planted_first = if first == '0' { '1' } else { '0' };
    let planted = format!("{planted_first}{}", &hash[1..]);
    Some((
        format!("{}{planted}{}", &answers[..at], &answers[at + 64..]),
        planted,
    ))
}

fn build_modules(
    root: &Path,
    toolchain: &Toolchain,
    profile: &str,
    rustflags: Option<&str>,
) -> Result<BuiltModules, String> {
    // STOCK EXCEPTION: Trust lacks a wasm32 std.
    let neutral = aterm_tempfile::Builder::new()
        .prefix("aterm-wasm-bench-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| format!("could not create neutral build directory: {e}"))?;
    eprintln!(
        "  building aterm-wasm + aterm-gpu-web (wasm32, {profile}) on {}",
        toolchain.name
    );
    let mut build = toolchain.cargo();
    let env = crate::EnvSnapshot::capture();
    let plan = module_build_plan(&env, neutral.path(), root)?;
    plan.apply_to_command(&mut build)
        .map_err(|error| error.to_string())?;
    if let Some(value) = plan.build_dir() {
        build.arg("--config").arg(value);
    }
    build
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .current_dir(neutral.path())
        .args([
            "build",
            "--profile",
            profile,
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            "aterm-wasm",
            "-p",
            "aterm-gpu-web",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"));
    if let Some(flags) = rustflags {
        build.env("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS", flags);
    }
    if std::env::var_os("CARGO_NET_OFFLINE").is_none() {
        build.env("CARGO_NET_OFFLINE", "false");
    }
    let built = run_module_build(&plan, profile, || match build.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "the {} wasm32 build exited {:?}",
            toolchain.name,
            status.code()
        )),
        Err(error) => Err(format!(
            "could not run {}: {error}",
            toolchain.cargo.display()
        )),
    });
    drop(neutral);
    built
}

/// Bind both modules into `out/cpu` and `out/gpu` with `verify glue`'s generator, then apply the
/// shipping `wasm-opt` pass when `optimise`.
pub(crate) fn bind_modules(built: &Path, out: &Path, optimise: bool) -> Result<(), String> {
    for (module, dir) in [("aterm_wasm.wasm", "cpu"), ("aterm_gpu_web.wasm", "gpu")] {
        crate::wasm_glue::bind(&built.join(module), &out.join(dir))?;
    }
    if optimise {
        for bound in [
            out.join("cpu/aterm_wasm_bg.wasm"),
            out.join("gpu/aterm_gpu_web_bg.wasm"),
        ] {
            run_step(
                command(wasm_opt_program())
                    .args(WASM_OPT_FLAGS)
                    .arg("-o")
                    .arg(&bound)
                    .arg(&bound),
                "wasm-opt",
            )?;
        }
    }
    Ok(())
}

fn wasm_opt_program() -> PathBuf {
    aterm_uds::host::find_tool("wasm-opt").unwrap_or_else(|| "wasm-opt".into())
}

fn build_and_bench(
    root: &Path,
    toolchain: &Toolchain,
    browser: &wasm_page::Browser,
) -> Result<(String, Value), Refused> {
    let failed = Refused::Failed;
    let built = build_modules(
        root,
        toolchain,
        "release",
        Some("-C target-feature=+simd128"),
    )
    .map_err(failed)?;
    let site = built.target.join("wasm-bench");
    bind_modules(&built.directory, &site, true).map_err(failed)?;
    wasm_page::lay_drivers(root, &site).map_err(failed)?;
    let mut page = Page::start(browser, &site, &std::env::temp_dir())?;
    let product = page.product().to_string();
    eprintln!("  wasm-bench: in {product}");
    let _entry = super::measurement_entry().map_err(Refused::Timing)?;
    let ran = page.run(
        &format!("{}/bench.mjs", wasm_page::DRIVERS),
        &json!({"cpu": "../cpu/aterm_wasm.js", "gpu": "../gpu/aterm_gpu_web.js",
                "font": format!("../{}", wasm_page::FONT)}),
    )?;
    _entry
        .timing_admission()
        .map_err(|why| Refused::Timing(why.to_string()))?;
    for line in ran.console.iter().filter(|l| l.contains("wasm-bench:")) {
        eprintln!("  {line}");
    }
    for line in sample_lines(&ran.value) {
        eprintln!("    {line}");
    }
    let mut report = ran.value;
    let Value::Object(fields) = &mut report else {
        return Err(failed(format!("bench.mjs answered no report: {report:?}")));
    };
    let (cpu, gpu) = module_sizes(&site).map_err(failed)?;
    fields.insert("wasm_cpu_module_bytes".into(), json!(cpu));
    fields.insert("wasm_gpu_module_bytes".into(), json!(gpu));
    Ok((product, report))
}

fn sample_lines(report: &Value) -> Vec<String> {
    WASM_LANE
        .keys
        .iter()
        .filter_map(|key| {
            let samples = report.get("samples")?.get(key)?.as_array()?;
            let shown: Vec<String> = samples
                .iter()
                .filter_map(Value::as_f64)
                .map(|v| format!("{v}"))
                .collect();
            Some(format!("{key}: n={} [{}]", shown.len(), shown.join(" ")))
        })
        .collect()
}

fn run_step(cmd: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<(), String> {
    let out = cmd
        .output_owned()
        .map_err(|e| format!("could not run {what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// The module-size ceilings, CPU then GPU.
pub(crate) fn wasm_module_fences(json: &Value) -> Vec<String> {
    let fences = [
        (
            "wasm_cpu_module_bytes",
            WASM_CPU_MODULE_BYTES_CAP,
            "WASM_CPU_MODULE_BYTES_CAP",
        ),
        (
            "wasm_gpu_module_bytes",
            WASM_GPU_MODULE_BYTES_CAP,
            "WASM_GPU_MODULE_BYTES_CAP",
        ),
    ];
    let mut findings = Vec::new();
    for (key, cap, name) in fences {
        match floors::number(json, key) {
            Some(v) if v <= cap => eprintln!(
                "    {key}: GREEN — {v:.0} B <= ceiling {cap:.0} B ({:.0} B of headroom)",
                cap - v
            ),
            Some(v) => findings.push(format!(
                "{key} {v:.0} B > ceiling {cap:.0} B by {:.0} B ({name}): raise the ceiling or \
                 shrink the module, and record the call with what grew in decision 5 of \
                 docs/DESIGN-host-boundary-2026-08-30.md — never a re-record",
                v - cap
            )),
            None => findings.push(format!("{key} missing from the bench report")),
        }
    }
    findings
}

#[cfg(test)]
#[path = "wasm_tests.rs"]
mod tests;
