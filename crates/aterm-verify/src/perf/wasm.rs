// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm` lane: build the two SHIPPED renderer modules (`aterm-wasm` on
//! the CPU canvas path, `aterm-gpu-web` on the WebGL path) for
//! `wasm32-unknown-unknown` with the shipping `+simd128` flag, bind them for
//! node with `verify glue`'s generator (neither module has wasm-bindgen:
//! docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.7 and WP.8), apply
//! the shipping `wasm-opt -O3` pass, and drive their real exports with
//! `crates/aterm-verify/perf/wasm/bench.mjs` under node.
//!
//! STOCK EXCEPTION (Trust lacks a wasm32 std: error[E0463] can't find crate
//! for `core`): the build rides rustup's `stable`, resolved to its own
//! `cargo`/`rustc` by path, since a stock or rerouted `cargo` earlier on PATH
//! ignores `RUSTUP_TOOLCHAIN`. It runs from a neutral cwd so no repo
//! config (vendored source replacement, Trust `-Z` flags) applies.
//!
//! A box without node, the stable wasm32 target or `wasm-opt` SKIPs with the
//! fix; a failure on an equipped box fails.
//!
//! Honest seams, on purpose: the GPU scenarios time the wasm-side frame build
//! only (node has no WebGL, and bench.mjs asserts the seam is where it
//! errors), and heap growth is an RSS proxy (wasm linear memory never
//! shrinks).

use aterm_uds::output::CommandOutput as _;
use std::path::{Path, PathBuf};

use aterm_json::Value;
use aterm_uds::spawnfd::command;

use super::LaneRun;
use super::floors::{self, FloorLane};
use super::workspace_root;
// The shipping `wasm-opt` pass: pre-opt numbers are not the product's, so the
// lane never measures without it.
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
              aterm-gpu-web) driven under node by crates/aterm-verify/perf/wasm/bench.mjs. All \
              BIGGER-IS-BETTER. Re-record with targo --unverified verify perf --record \
              wasm.",
    cli: "wasm",
};

/// The CPU module's size ceiling in bytes: ABSOLUTE and lower-is-better, on
/// the post-`wasm-opt -O3` `aterm_wasm_bg.wasm` (`wasm_cpu_module_bytes`). The
/// measured 5,351,532 B + 10% (docs/DESIGN-host-boundary-2026-08-30.md §7
/// decision 5). A trip is the owner's raise-or-shrink call, never a re-record.
pub(crate) const WASM_CPU_MODULE_BYTES_CAP: f64 = 5_886_685.0;

/// The GPU module's size ceiling in bytes, on the post-`wasm-opt -O3`
/// `aterm_gpu_web_bg.wasm` (`wasm_gpu_module_bytes`), the same rule as the
/// CPU module's: the measured 4,901,631 B (2026-10-19) + 10%.
pub(crate) const WASM_GPU_MODULE_BYTES_CAP: f64 = 5_391_794.0;

/// The node driver, beside this crate.
fn bench_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/wasm/bench.mjs")
}

/// Does `program args` run and exit 0?
fn answers(program: &Path, args: &[&str]) -> bool {
    command(program)
        .args(args)
        .output_owned()
        .is_ok_and(|o| o.status.success())
}

/// What a wasm lane needs before it builds, found or refused: node, then
/// [`build_tools`]. `Err` is the lane's verdict: a SKIP with the fix.
pub(crate) fn lane_tools(wasm_opt: bool) -> Result<PathBuf, LaneRun> {
    if !answers(Path::new("node"), &["--version"]) {
        return Err(LaneRun::skip(
            "node not found; the lane runs the wasm under node",
            aterm_uds::host::tool("node").remedy(),
        ));
    }
    build_tools(wasm_opt)
}

/// What building the two modules needs, found or refused: rustup (whose
/// `stable` carries the wasm32 std), and `wasm-opt` when `wasm_opt` asks for
/// it: rustup's path, or a SKIP with the fix. Binding needs nothing more:
/// `verify glue`'s generator runs in process.
pub(crate) fn build_tools(wasm_opt: bool) -> Result<PathBuf, LaneRun> {
    let Some(rustup) = crate::rustup::find_rustup() else {
        return Err(LaneRun::skip(
            format!("no rustup ({})", crate::rustup::RUSTUP_LOOKED),
            format!(
                "{}, with stable's wasm32 std",
                aterm_uds::host::tool("rustup").remedy()
            ),
        ));
    };
    let has_target = command(&rustup)
        .args(["target", "list", "--installed", "--toolchain", "stable"])
        .output_owned()
        .is_ok_and(|o| {
            crate::rustup::lists_target(
                &String::from_utf8_lossy(&o.stdout),
                "wasm32-unknown-unknown",
            )
        });
    if !has_target {
        return Err(LaneRun::skip(
            "no stable wasm32-unknown-unknown target",
            "rustup target add wasm32-unknown-unknown --toolchain stable",
        ));
    }
    if wasm_opt && !answers(&wasm_opt_program(), &["--version"]) {
        return Err(LaneRun::skip(
            "wasm-opt not found; the lane applies the shipping -O3 pass and never measures \
             pre-opt modules",
            aterm_uds::host::tool("wasm-opt").remedy(),
        ));
    }
    Ok(rustup)
}

/// WASM floors: the shipped modules under node, every gated key against its
/// floor, plus the module-size ceilings.
pub(crate) fn gate_wasm(record: bool) -> LaneRun {
    let root = workspace_root();
    let rustup = match lane_tools(true) {
        Ok(rustup) => rustup,
        Err(verdict) => return verdict,
    };
    let json = match build_and_bench(&root, &rustup) {
        Ok(json) => json,
        Err(e) => return LaneRun::fail(e),
    };
    let mut run = floors::judge_keyed(&WASM_LANE, &json, record);
    let findings = wasm_module_fences(&json);
    if findings.is_empty() {
        run.fence_held(WASM_LANE.cli);
    }
    for finding in findings {
        run.add_failure(finding);
    }
    run
}

/// The CPU module's boundary replay recorded again (`verify glue
/// --record-replay`): both modules built in the DEV profile and bound as the
/// `wasm-smoke` check binds them, then [`replay`] with `record`, which
/// rewrites the answers and checks them back. Progress goes to `log`.
pub(crate) fn record_replay(log: &mut String) -> LaneRun {
    let root = workspace_root();
    let rustup = match lane_tools(false) {
        Ok(rustup) => rustup,
        Err(verdict) => return verdict,
    };
    let mut record = || -> Result<(), String> {
        let built = build_modules(&root, &rustup, "dev", None)?;
        let out = root.join("target/wasm-smoke");
        bind_modules(&built, &out, false)?;
        bind_cpu_web(&built, &out)?;
        replay(&root, &out, true, log)
    };
    match record() {
        Ok(()) => LaneRun::pass(),
        Err(e) => LaneRun::fail(e),
    }
}

/// The CPU module's ES module glue into `out/cpu-web`, the second flavor
/// [`replay`] drives.
pub(crate) fn bind_cpu_web(built: &Path, out: &Path) -> Result<(), String> {
    crate::wasm_glue::bind(
        &built.join("aterm_wasm.wasm"),
        &out.join("cpu-web"),
        crate::wasm_glue::Flavor::Web,
    )
    .map(drop)
}

/// Run `node args…`, its stderr into `log`; `Err` unless it exits 0.
fn node_script(args: &[&std::ffi::OsStr], log: &mut String) -> Result<(), String> {
    let run = node_output(args)?;
    for line in String::from_utf8_lossy(&run.stderr).lines() {
        log.push_str("  ");
        log.push_str(line);
        log.push('\n');
    }
    if run.status.success() {
        Ok(())
    } else {
        Err(format!(
            "node {} exited {:?}",
            Path::new(args.first().copied().unwrap_or_default()).display(),
            run.status.code()
        ))
    }
}

fn node_output(args: &[&std::ffi::OsStr]) -> Result<std::process::Output, String> {
    command("node")
        .args(args)
        .output_owned()
        .map_err(|e| format!("could not run node: {e}"))
}

/// The CPU module's recorded boundary answers (`perf/wasm/replay.mjs`).
pub(crate) fn replay_answers(root: &Path) -> PathBuf {
    root.join("crates/aterm-wasm/tests/fixtures/abi_replay.json")
}

/// The CPU module's boundary replay: `replay.mjs` drives the debug module
/// through the CommonJS glue in `out/cpu` and the ES module glue in
/// `out/cpu-web`, and each run must give the recorded answers
/// ([`replay_answers`]); `record` rewrites them from the CommonJS run first.
/// The answers were recorded on the glue wasm-bindgen 0.2.108 wrote, so a
/// module bound another way that reproduces them carries every value across
/// the boundary as that one did.
///
/// NEGATIVE CONTROL: the same run against a copy of the answers with one
/// frame's hash changed must fail and name that answer, so a replay that
/// compares nothing cannot pass.
pub(crate) fn replay(
    root: &Path,
    out: &Path,
    record: bool,
    log: &mut String,
) -> Result<(), String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/wasm/replay.mjs");
    let answers = replay_answers(root);
    let (cpu, cpu_web) = (out.join("cpu"), out.join("cpu-web"));
    if record {
        node_script(
            &[
                script.as_os_str(),
                "record".as_ref(),
                answers.as_os_str(),
                cpu.as_os_str(),
                "nodejs".as_ref(),
            ],
            log,
        )?;
    }
    for (dir, flavor) in [(&cpu, "nodejs"), (&cpu_web, "web")] {
        node_script(
            &[
                script.as_os_str(),
                "check".as_ref(),
                answers.as_os_str(),
                dir.as_os_str(),
                flavor.as_ref(),
            ],
            log,
        )?;
    }
    let text = std::fs::read_to_string(&answers)
        .map_err(|e| format!("could not read {}: {e}", answers.display()))?;
    let (planted, hash) = plant_frame_hash(&text)
        .ok_or_else(|| format!("{}: no frame hash to plant", answers.display()))?;
    let copy = out.join("abi_replay.planted.json");
    std::fs::write(&copy, planted)
        .map_err(|e| format!("could not write {}: {e}", copy.display()))?;
    let run = node_output(&[
        script.as_os_str(),
        "check".as_ref(),
        copy.as_os_str(),
        cpu.as_os_str(),
        "nodejs".as_ref(),
    ])?;
    let stderr = String::from_utf8_lossy(&run.stderr);
    if run.status.success() || !stderr.contains(&hash) || !stderr.contains(" 1 differ") {
        return Err(format!(
            "NEGATIVE CONTROL: the replay against answers with one frame hash planted \
             ({hash}) did not fail on exactly that answer:\n{stderr}"
        ));
    }
    log.push_str("  wasm-replay: negative control held: a planted frame hash fails the replay\n");
    Ok(())
}

/// `answers` with the first frame's `rgba` hash changed in its first digit:
/// the planted text and the planted hash.
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

/// Build both modules for wasm32 on rustup's stable in `profile`, with
/// `rustflags` for the wasm32 target only; the directory the `.wasm` files
/// land in.
fn build_modules(
    root: &Path,
    rustup: &Path,
    profile: &str,
    rustflags: Option<&str>,
) -> Result<PathBuf, String> {
    // STOCK EXCEPTION: Trust lacks a wasm32 std (module doc).
    let stable = crate::rustup::Toolchain::resolve(rustup, "stable")?;
    let neutral = std::env::temp_dir().join(format!("aterm-wasm-bench-{}", std::process::id()));
    std::fs::create_dir_all(&neutral)
        .map_err(|e| format!("could not create {}: {e}", neutral.display()))?;
    eprintln!("  building aterm-wasm + aterm-gpu-web (wasm32, {profile}) on stable");
    let mut build = stable.cargo();
    let target = root.join("target");
    // The intermediates go where Spotlight never looks; the `.wasm` files are
    // uplifted to `<target>/wasm32-unknown-unknown/<profile>/` as before.
    if let Some(value) =
        crate::fallback_build_dir(&crate::EnvSnapshot::capture(), &neutral, &target)
    {
        build.arg("--config").arg(value);
    }
    build
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(&neutral)
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
    let built = build.status();
    let _ = std::fs::remove_dir_all(&neutral);
    match built {
        Ok(s) if s.success() => {}
        Ok(s) => return Err(format!("the stable wasm32 build exited {:?}", s.code())),
        Err(e) => return Err(format!("could not run {}: {e}", stable.cargo.display())),
    }
    // cargo names the dev profile's directory `debug`.
    let dir = if profile == "dev" { "debug" } else { profile };
    Ok(root.join("target/wasm32-unknown-unknown").join(dir))
}

/// Bind both modules for node (CommonJS) into `out/cpu` and `out/gpu` with
/// `verify glue`'s generator, then apply the shipping `wasm-opt` pass when
/// `optimise`.
pub(crate) fn bind_modules(built: &Path, out: &Path, optimise: bool) -> Result<(), String> {
    for (module, dir) in [("aterm_wasm.wasm", "cpu"), ("aterm_gpu_web.wasm", "gpu")] {
        crate::wasm_glue::bind(
            &built.join(module),
            &out.join(dir),
            crate::wasm_glue::Flavor::Nodejs,
        )?;
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

/// The wasm-opt the lane runs: the resolver's (`ATERM_TEST_WASM_OPT`, the pinned
/// binaryen `verify provision` places, then PATH), else the bare name, whose failure to
/// start names it.
fn wasm_opt_program() -> PathBuf {
    aterm_uds::host::find_tool("wasm-opt").unwrap_or_else(|| "wasm-opt".into())
}

/// Build, bind, optimise and bench; the bench's JSON report.
fn build_and_bench(root: &Path, rustup: &Path) -> Result<Value, String> {
    let built = build_modules(root, rustup, "release", Some("-C target-feature=+simd128"))?;
    let out = root.join("target/wasm-bench");
    bind_modules(&built, &out, true)?;
    // --expose-gc: the bench collects before its heap reads, so the search
    // memory proxy is not dominated by dead JS garbage.
    let bench = command("node")
        .arg("--expose-gc")
        .arg(bench_script())
        .arg(&out)
        .output_owned()
        .map_err(|e| format!("could not run node: {e}"))?;
    let stderr = String::from_utf8_lossy(&bench.stderr);
    for line in stderr.lines().filter(|l| l.contains("wasm-bench:")) {
        eprintln!("  {line}");
    }
    if !bench.status.success() {
        return Err(format!(
            "bench.mjs exited {:?}\n{stderr}",
            bench.status.code()
        ));
    }
    floors::last_json_line(&String::from_utf8_lossy(&bench.stdout))
        .ok_or_else(|| "bench.mjs printed no JSON report".to_string())
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

/// The module-size ceilings, CPU then GPU: a finding for each one breached or
/// missing (an unmeasured size is not a small one).
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
                 shrink the module — an owner call, not a re-record",
                v - cap
            )),
            None => findings.push(format!("{key} missing from the bench report")),
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        aterm_json::from_str(text).expect("test JSON parses")
    }

    fn sizes(cpu: u64, gpu: u64) -> Value {
        json(&format!(
            "{{\"wasm_cpu_module_bytes\":{cpu},\"wasm_gpu_module_bytes\":{gpu}}}"
        ))
    }

    #[test]
    fn the_wasm_module_ceilings_hold_absolutely() {
        assert!(wasm_module_fences(&sizes(4_703_802, 4_901_631)).is_empty());
        assert!(wasm_module_fences(&sizes(5_886_685, 5_391_794)).is_empty());
        let cpu_over = wasm_module_fences(&sizes(5_886_686, 5_391_794));
        assert_eq!(cpu_over.len(), 1);
        assert!(cpu_over[0].starts_with("wasm_cpu_module_bytes 5886686 B > ceiling"));
        let gpu_over = wasm_module_fences(&sizes(5_886_685, 5_391_795));
        assert_eq!(gpu_over.len(), 1);
        assert!(gpu_over[0].starts_with("wasm_gpu_module_bytes 5391795 B > ceiling"));
        assert_eq!(wasm_module_fences(&sizes(5_886_686, 5_391_795)).len(), 2);
        assert_eq!(
            wasm_module_fences(&json("{\"wasm_cpu_module_bytes\":1}")),
            ["wasm_gpu_module_bytes missing from the bench report"]
        );
        assert_eq!(
            wasm_module_fences(&json("{\"wasm_gpu_module_bytes\":1}")),
            ["wasm_cpu_module_bytes missing from the bench report"]
        );
    }

    #[test]
    fn a_planted_frame_hash_changes_one_digit_of_the_first_frame() {
        let a = "0".repeat(64);
        let b = "f".repeat(64);
        let text = format!("[\"x\",{{\"rgba\":\"{a}\"}}],\n[\"y\",{{\"rgba\":\"{b}\"}}]");
        let (planted, hash) = plant_frame_hash(&text).expect("a frame to plant");
        assert_eq!(hash, format!("1{}", &a[1..]));
        assert_eq!(planted, text.replacen(&a, &hash, 1));
        assert!(plant_frame_hash("{\"answers\": []}").is_none());
    }
}
