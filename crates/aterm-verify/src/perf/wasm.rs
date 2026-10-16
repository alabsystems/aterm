// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm` lane: build the two SHIPPED renderer modules (`aterm-wasm` on
//! the CPU canvas path, `aterm-gpu-web` on the WebGL path) for
//! `wasm32-unknown-unknown` with the shipping `+simd128` flag, bind them for
//! node (the CPU module with `verify glue`'s generator, the GPU module with
//! the pinned wasm-bindgen), apply the shipping `wasm-opt -O3` pass, and
//! drive their real exports with `crates/aterm-verify/perf/wasm/bench.mjs`
//! under node.
//!
//! STOCK EXCEPTION (Trust lacks a wasm32 std: error[E0463] can't find crate
//! for `core`): the build rides rustup's `stable`, resolved to its own
//! `cargo`/`rustc` by path, since a stock or rerouted `cargo` earlier on PATH
//! ignores `RUSTUP_TOOLCHAIN`. It runs from a neutral cwd so no repo
//! config (vendored source replacement, Trust `-Z` flags) applies.
//!
//! A box without node, the stable wasm32 target, `wasm-opt` or the pinned
//! `wasm-bindgen` SKIPs with the fix; a failure on an equipped box fails.
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

/// The wasm-bindgen CLI this lane binds the GPU module (`aterm-gpu-web`)
/// with; the CPU module has no wasm-bindgen and binds with `verify glue`'s generator
/// (docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.7). The CLI refuses
/// a crate built against another version, so it must equal the
/// `wasm-bindgen` version `Cargo.lock` builds aterm-gpu-web against; the lane
/// checks that before it builds anything.
pub(crate) const WASM_BINDGEN_PIN: &str = "0.2.108";

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

/// The `wasm-opt` pass the shipping pipeline applies: `-O3` (speed, matching
/// the cargo opt-3 build) with the shipping feature flags. Pre-opt numbers are
/// not the product's, so the lane never measures without it.
const WASM_OPT_FLAGS: [&str; 7] = [
    "-O3",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--enable-reference-types",
    "--enable-simd",
];

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

/// The version `wasm-bindgen --version` reports (`wasm-bindgen 0.2.108`'s
/// second field), compared exactly: a substring test reads 0.2.1080 as 0.2.108.
fn bindgen_version(program: &Path, path_env: Option<&std::ffi::OsStr>) -> Option<String> {
    let mut cmd = command(program);
    cmd.arg("--version");
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    let out = cmd.output_owned().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
}

/// The `wasm-bindgen` version `Cargo.lock` builds the modules against.
pub(crate) fn locked_bindgen_version(lockfile: &str) -> Option<String> {
    let lock: aterm_toml::Value = aterm_toml::from_str(lockfile).ok()?;
    lock.get("package")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(aterm_toml::Value::as_str) == Some("wasm-bindgen"))?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

/// The pinned wasm-bindgen: on PATH (with rustup's directory appended, where
/// a `cargo install`ed one lands) at exactly the pin, else in this repo's
/// tooling cache.
fn find_bindgen(root: &Path, path_env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let on_path = std::env::split_paths(path_env.unwrap_or_default())
        .map(|d| d.join("wasm-bindgen"))
        .find(|p| p.is_file());
    let cache = root.join(format!(
        "target/tooling/wasm-bindgen-{WASM_BINDGEN_PIN}/bin/wasm-bindgen"
    ));
    on_path
        .into_iter()
        .chain([cache])
        .find(|p| bindgen_version(p, path_env).as_deref() == Some(WASM_BINDGEN_PIN))
}

/// What a wasm lane needs before it builds, found or refused: rustup (whose
/// `stable` carries the wasm32 std) and the pinned wasm-bindgen. Node is
/// checked too, and `wasm-opt` when `wasm_opt` asks for it. `Err` is the
/// lane's verdict: a SKIP with the fix, or a FAIL on a lock off the pin.
pub(crate) fn lane_tools(root: &Path, wasm_opt: bool) -> Result<(PathBuf, PathBuf), LaneRun> {
    if !answers(Path::new("node"), &["--version"]) {
        return Err(LaneRun::skip(
            "node not found; the lane runs the wasm under node",
            "install node (brew install node)",
        ));
    }
    let Some(rustup) = crate::rustup::find_rustup() else {
        return Err(LaneRun::skip(
            format!("no rustup ({})", crate::rustup::RUSTUP_LOOKED),
            "install rustup (https://rustup.rs) and the stable wasm32 target",
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
    if wasm_opt && !answers(Path::new("wasm-opt"), &["--version"]) {
        return Err(LaneRun::skip(
            "wasm-opt not found; the lane applies the shipping -O3 pass and never measures \
             pre-opt modules",
            "install binaryen (brew install binaryen)",
        ));
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    match locked_bindgen_version(&lock) {
        Some(v) if v == WASM_BINDGEN_PIN => {}
        other => {
            return Err(LaneRun::fail(format!(
                "Cargo.lock builds wasm-bindgen {}, WASM_BINDGEN_PIN is {WASM_BINDGEN_PIN}: \
                 move the const (crates/aterm-verify/src/perf/wasm.rs) to the locked version",
                other.as_deref().unwrap_or("<none>")
            )));
        }
    }
    let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(dir) = rustup.parent() {
        path_dirs.push(dir.to_path_buf());
    }
    let path_env = std::env::join_paths(path_dirs).ok();
    let Some(bindgen) = find_bindgen(root, path_env.as_deref()) else {
        let tooling = root.join(format!("target/tooling/wasm-bindgen-{WASM_BINDGEN_PIN}"));
        return Err(LaneRun::skip(
            format!(
                "wasm-bindgen {WASM_BINDGEN_PIN} is not on PATH, beside rustup or in target/tooling"
            ),
            format!(
                "cd / && targo --unverified install wasm-bindgen-cli --version \
                 {WASM_BINDGEN_PIN} --locked --root '{}'",
                tooling.display()
            ),
        ));
    };
    Ok((rustup, bindgen))
}

/// WASM floors: the shipped modules under node, every gated key against its
/// floor, plus the module-size ceiling.
pub(crate) fn gate_wasm(record: bool) -> LaneRun {
    let root = workspace_root();
    let (rustup, bindgen) = match lane_tools(&root, true) {
        Ok(tools) => tools,
        Err(verdict) => return verdict,
    };
    eprintln!("  wasm-bindgen: {} ({WASM_BINDGEN_PIN})", bindgen.display());
    let json = match build_and_bench(&root, &rustup, &bindgen) {
        Ok(json) => json,
        Err(e) => return LaneRun::fail(e),
    };
    let mut run = floors::judge_keyed(&WASM_LANE, &json, record);
    match wasm_module_fence(&json) {
        Some(finding) => run.add_failure(finding),
        None => run.fence_held(WASM_LANE.cli),
    }
    run
}

/// The CPU module's boundary replay recorded again (`verify glue
/// --record-replay`): both modules built in the DEV profile and bound as the
/// `wasm-smoke` check binds them, then [`replay`] with `record`, which
/// rewrites the answers and checks them back. Progress goes to `log`.
pub(crate) fn record_replay(log: &mut String) -> LaneRun {
    let root = workspace_root();
    let (rustup, bindgen) = match lane_tools(&root, false) {
        Ok(tools) => tools,
        Err(verdict) => return verdict,
    };
    let mut record = || -> Result<(), String> {
        let built = build_modules(&root, &rustup, "dev", None)?;
        let out = root.join("target/wasm-smoke");
        bind_modules(&bindgen, &built, &out, false)?;
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
    build
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", root.join("target"))
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

/// Bind both modules for node (CommonJS) into `out/cpu` and `out/gpu`, the
/// CPU module with `verify glue`'s generator and the GPU module with
/// wasm-bindgen (`--target nodejs`), then apply the shipping `wasm-opt` pass
/// when `optimise`.
pub(crate) fn bind_modules(
    bindgen: &Path,
    built: &Path,
    out: &Path,
    optimise: bool,
) -> Result<(), String> {
    crate::wasm_glue::bind(
        &built.join("aterm_wasm.wasm"),
        &out.join("cpu"),
        crate::wasm_glue::Flavor::Nodejs,
    )?;
    run_step(
        command(bindgen)
            .args(["--target", "nodejs", "--out-dir"])
            .arg(out.join("gpu"))
            .arg(built.join("aterm_gpu_web.wasm")),
        "wasm-bindgen",
    )?;
    if optimise {
        for bound in [
            out.join("cpu/aterm_wasm_bg.wasm"),
            out.join("gpu/aterm_gpu_web_bg.wasm"),
        ] {
            run_step(
                command("wasm-opt")
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

/// Build, bind, optimise and bench; the bench's JSON report.
fn build_and_bench(root: &Path, rustup: &Path, bindgen: &Path) -> Result<Value, String> {
    let built = build_modules(root, rustup, "release", Some("-C target-feature=+simd128"))?;
    let out = root.join("target/wasm-bench");
    bind_modules(bindgen, &built, &out, true)?;
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

/// The module-size ceiling: `Some(finding)` when it is breached or the size
/// is missing (an unmeasured size is not a small one).
pub(crate) fn wasm_module_fence(json: &Value) -> Option<String> {
    let key = "wasm_cpu_module_bytes";
    let cap = WASM_CPU_MODULE_BYTES_CAP;
    match floors::number(json, key) {
        Some(v) if v <= cap => {
            eprintln!(
                "    {key}: GREEN — {v:.0} B <= ceiling {cap:.0} B ({:.0} B of headroom)",
                cap - v
            );
            None
        }
        Some(v) => Some(format!(
            "{key} {v:.0} B > ceiling {cap:.0} B by {:.0} B (WASM_CPU_MODULE_BYTES_CAP): raise \
             the ceiling or shrink the module — an owner call, not a re-record",
            v - cap
        )),
        None => Some(format!("{key} missing from the bench report")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        aterm_json::from_str(text).expect("test JSON parses")
    }

    #[test]
    fn the_wasm_module_ceiling_holds_absolutely() {
        assert!(wasm_module_fence(&json("{\"wasm_cpu_module_bytes\":5351532}")).is_none());
        assert!(wasm_module_fence(&json("{\"wasm_cpu_module_bytes\":5886685}")).is_none());
        assert!(wasm_module_fence(&json("{\"wasm_cpu_module_bytes\":5886686}")).is_some());
        assert!(wasm_module_fence(&json("{\"wasm_gpu_module_bytes\":1}")).is_some());
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

    #[test]
    fn the_locked_version_is_read_from_the_lockfile_table() {
        let lock = "version = 4\n\n[[package]]\nname = \"wasm-bindgen-macro\"\nversion = \"9.9.9\"\n\n\
                    [[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.108\"\n";
        assert_eq!(locked_bindgen_version(lock).as_deref(), Some("0.2.108"));
        assert_eq!(locked_bindgen_version("version = 4\n"), None);
    }

    /// Only a wasm-bindgen that answers exactly the pin is found: a version
    /// the pin is a prefix of, and one that does not answer, are not.
    #[cfg(unix)]
    #[test]
    fn only_a_bindgen_answering_exactly_the_pin_is_found() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-wasm-")
            .tempdir()
            .expect("scratch");
        let cache = dir.path().join(format!(
            "target/tooling/wasm-bindgen-{WASM_BINDGEN_PIN}/bin/wasm-bindgen"
        ));
        std::fs::create_dir_all(cache.parent().expect("parent")).expect("mkdir");
        let no_path = Some(std::ffi::OsStr::new(""));
        for (body, found) in [
            (format!("echo 'wasm-bindgen {WASM_BINDGEN_PIN}0'"), false),
            ("exit 1".to_string(), false),
            (format!("echo 'wasm-bindgen {WASM_BINDGEN_PIN}'"), true),
        ] {
            // Replaced, not rewritten in place: a running text file is busy.
            let _ = std::fs::remove_file(&cache);
            std::fs::write(&cache, format!("#!/bin/sh\n{body}\n")).expect("stub");
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            assert_eq!(
                find_bindgen(dir.path(), no_path),
                found.then(|| cache.clone()),
                "{body}"
            );
        }
    }
}
