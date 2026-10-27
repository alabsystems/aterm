// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WebAssembly renderer performance lane (`aterm-wasm` and `aterm-gpu-web`).
//!
//! STOCK EXCEPTION (Trust lacks a wasm32 std: `error[E0463]` can't find crate
//! for `core`): the build rides rustup's `stable`, resolved to its own
//! `cargo`/`rustc` by path, since a stock or rerouted `cargo` earlier on PATH
//! ignores `RUSTUP_TOOLCHAIN`. It runs from a neutral cwd so no repo
//! config (vendored source replacement, Trust `-Z` flags) applies.

use aterm_uds::output::CommandOutput as _;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_json::{Value, json};
use aterm_uds::spawnfd::command;

use super::LaneRun;
use super::floors::{self, FloorLane};
use super::workspace_root;
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

fn answers(program: &Path, args: &[&str]) -> bool {
    command(program)
        .args(args)
        .output_owned()
        .is_ok_and(|o| o.status.success())
}

/// Resolves rustup and optional `wasm-opt` for building the WebAssembly modules.
pub(crate) fn build_tools(wasm_opt: bool) -> Result<PathBuf, LaneRun> {
    build_tools_with(aterm_uds::host::tool("rustup").locate(), wasm_opt)
}

/// [`build_tools`] with the `rustup` row's answer `found`: where it found none, COULD NOT
/// RUN saying where the row looked and naming its provider, `verify provision`, once.
fn build_tools_with(
    found: Result<PathBuf, aterm_uds::host::Absent>,
    wasm_opt: bool,
) -> Result<PathBuf, LaneRun> {
    let rustup = match found {
        Ok(rustup) => rustup,
        Err(absent) => {
            return Err(LaneRun::could_not_run(
                absent.why,
                format!("{}, with stable's wasm32 std", absent.fix),
            ));
        }
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
        return Err(LaneRun::could_not_run(
            "no stable wasm32-unknown-unknown target",
            "rustup target add wasm32-unknown-unknown --toolchain stable",
        ));
    }
    if wasm_opt && !answers(&wasm_opt_program(), &["--version"]) {
        return Err(LaneRun::could_not_run(
            "wasm-opt not found; the lane applies the shipping -O3 pass and never measures \
             pre-opt modules",
            aterm_uds::host::tool("wasm-opt").remedy(),
        ));
    }
    Ok(rustup)
}

/// WASM floors: the shipped modules in the browser, every gated key against its floor, plus the
/// module-size ceilings.
pub(crate) fn gate_wasm(record: bool) -> LaneRun {
    let root = workspace_root();
    let browser = match wasm_page::browser() {
        Ok(browser) => browser,
        Err(refused) => return refused.verdict("the wasm bench"),
    };
    let rustup = match build_tools(true) {
        Ok(rustup) => rustup,
        Err(verdict) => return verdict,
    };
    let (product, json) = match build_and_bench(&root, &rustup, &browser) {
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
    let rustup = match build_tools(false) {
        Ok(rustup) => rustup,
        Err(verdict) => return verdict,
    };
    let mut record = || -> Result<(), Refused> {
        let built = build_modules(&root, &rustup, "dev", None).map_err(Refused::Failed)?;
        let site = root.join("target/wasm-smoke");
        bind_modules(&built, &site, false).map_err(Refused::Failed)?;
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
        std::fs::write(&answers, text)
            .map_err(|e| failed(format!("could not write {}: {e}", answers.display())))?;
        let _ = writeln!(log, "  wasm-replay: recorded {}", answers.display());
    }
    let text = std::fs::read_to_string(&answers)
        .map_err(|e| failed(format!("could not read {}: {e}", answers.display())))?;
    let (planted, hash) = plant_frame_hash(&text)
        .ok_or_else(|| failed(format!("{}: no frame hash to plant", answers.display())))?;
    for (name, contents) in [("answers.json", &text), ("answers.planted.json", &planted)] {
        let path = site.join(name);
        std::fs::write(&path, contents)
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
    rustup: &Path,
    profile: &str,
    rustflags: Option<&str>,
) -> Result<PathBuf, String> {
    // STOCK EXCEPTION: Trust lacks a wasm32 std.
    let stable = crate::rustup::Toolchain::resolve(rustup, "stable")?;
    let neutral = std::env::temp_dir().join(format!("aterm-wasm-bench-{}", std::process::id()));
    std::fs::create_dir_all(&neutral)
        .map_err(|e| format!("could not create {}: {e}", neutral.display()))?;
    eprintln!("  building aterm-wasm + aterm-gpu-web (wasm32, {profile}) on stable");
    let mut build = stable.cargo();
    let target = root.join("target");
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
    let dir = if profile == "dev" { "debug" } else { profile };
    Ok(root.join("target/wasm32-unknown-unknown").join(dir))
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
    rustup: &Path,
    browser: &wasm_page::Browser,
) -> Result<(String, Value), Refused> {
    let failed = Refused::Failed;
    let built = build_modules(root, rustup, "release", Some("-C target-feature=+simd128"))
        .map_err(failed)?;
    let site = root.join("target/wasm-bench");
    bind_modules(&built, &site, true).map_err(failed)?;
    wasm_page::lay_drivers(root, &site).map_err(failed)?;
    let mut page = Page::start(browser, &site, &std::env::temp_dir())?;
    let product = page.product().to_string();
    eprintln!("  wasm-bench: in {product}");
    let ran = page.run(
        &format!("{}/bench.mjs", wasm_page::DRIVERS),
        &json!({"cpu": "../cpu/aterm_wasm.js", "gpu": "../gpu/aterm_gpu_web.js",
                "font": format!("../{}", wasm_page::FONT)}),
    )?;
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
    for (key, module) in [
        ("wasm_cpu_module_bytes", "cpu/aterm_wasm_bg.wasm"),
        ("wasm_gpu_module_bytes", "gpu/aterm_gpu_web_bg.wasm"),
    ] {
        let path = site.join(module);
        let bytes = std::fs::metadata(&path)
            .map_err(|e| failed(format!("could not read {}: {e}", path.display())))?
            .len();
        fields.insert(key.to_string(), json!(bytes));
    }
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

    /// WITHOUT A RUSTUP THE WASM BUILDS END COULD NOT RUN NAMING THE WORKSPACE PROVIDER: where
    /// the `rustup` row finds none (no stamped copy, nothing on PATH), the lanes that build
    /// the web modules (wasm-smoke among them) refuse the machine, saying where the row looked
    /// and naming `verify provision` (its provider) once, never Homebrew, even with a keg on
    /// disk. Negative control: a rustup that is found gets past this refusal to the next
    /// question (here, a stand-in with no wasm32 std).
    #[test]
    #[cfg(unix)]
    fn with_no_rustup_the_wasm_builds_could_not_run_naming_verify_provision() {
        use crate::perf::Status;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let keg = tmp.path().join("opt/homebrew/opt/rustup/bin/rustup");
        std::fs::create_dir_all(keg.parent().unwrap()).unwrap();
        std::fs::write(&keg, "#!/bin/false\n").unwrap();
        let row = aterm_uds::host::tool("rustup");
        let lookup = aterm_uds::host::Lookup {
            named: None,
            tooling: Some(tmp.path().join("tooling.noindex")),
            path: Some(tmp.path().join("empty").into_os_string()),
        };
        let Err(refused) = build_tools_with(row.locate_in(&lookup), false) else {
            panic!("no rustup built the modules");
        };
        let Status::CouldNotRun { why, fix } = refused.status else {
            panic!("{:?}", refused.status);
        };
        assert!(
            why.starts_with("no rustup (looked in $ATERM_TEST_RUSTUP"),
            "{why}"
        );
        assert_eq!(fix, format!("{}, with stable's wasm32 std", row.remedy()));
        let said = format!("{why} {fix}");
        assert_eq!(said.matches("verify provision").count(), 1, "{said}");
        assert!(!said.contains("brew"), "{said}");

        let Err(next) = build_tools_with(Ok(PathBuf::from("/usr/bin/false")), false) else {
            panic!("a rustup with no wasm32 std built the modules");
        };
        assert!(
            matches!(&next.status, Status::CouldNotRun { why, .. }
                if why == "no stable wasm32-unknown-unknown target"),
            "{:?}",
            next.status
        );
    }

    #[test]
    fn the_wasm_module_ceilings_hold_absolutely() {
        assert!(wasm_module_fences(&sizes(4_703_802, 4_901_631)).is_empty());
        assert!(wasm_module_fences(&sizes(5_886_685, 6_133_315)).is_empty());
        let cpu_over = wasm_module_fences(&sizes(5_886_686, 6_133_315));
        assert_eq!(cpu_over.len(), 1);
        assert!(cpu_over[0].starts_with("wasm_cpu_module_bytes 5886686 B > ceiling"));
        let gpu_over = wasm_module_fences(&sizes(5_886_685, 6_133_316));
        assert_eq!(gpu_over.len(), 1);
        assert!(gpu_over[0].starts_with("wasm_gpu_module_bytes 6133316 B > ceiling"));
        assert_eq!(wasm_module_fences(&sizes(5_886_686, 6_133_316)).len(), 2);
        assert_eq!(
            wasm_module_fences(&json("{\"wasm_cpu_module_bytes\":1}")),
            ["wasm_gpu_module_bytes missing from the bench report"]
        );
        assert_eq!(
            wasm_module_fences(&json("{\"wasm_gpu_module_bytes\":1}")),
            ["wasm_cpu_module_bytes missing from the bench report"]
        );
    }

    /// The floors are judged and recorded only in the pinned shell: another
    /// browser's run is not judged and keeps no samples for the trend, and a
    /// record in it could not run. NEGATIVE CONTROL: the pinned product is
    /// judged.
    #[cfg(unix)]
    #[test]
    fn the_floors_are_judged_only_in_the_pinned_shell() {
        let pinned = crate::browser::chromium::pinned_product();
        assert!(floors_engine(&pinned, false).is_ok() && floors_engine(&pinned, true).is_ok());
        let other = "HeadlessChrome/1.0.0.0";
        let run = floors_engine(other, false).expect_err("another browser");
        assert!(
            matches!(&run.status, super::super::Status::NotJudged(why)
                if why.contains(&pinned) && why.contains(other)),
            "{run:?}"
        );
        assert!(run.samples.is_empty());
        let record = floors_engine(other, true).expect_err("no record");
        assert!(
            matches!(&record.status, super::super::Status::CouldNotRun { why, .. }
                if why.contains("records in the pinned shell only")),
            "{record:?}"
        );
    }

    #[test]
    fn each_gated_keys_samples_print_in_lane_order() {
        let report = json(
            "{\"samples\":{\"wasm_gpu_frame_build_fps\":[3,4.5],\
             \"wasm_cpu_ingest_mixed_mbps\":[1,2],\"ungated\":[9]}}",
        );
        assert_eq!(
            sample_lines(&report),
            [
                "wasm_cpu_ingest_mixed_mbps: n=2 [1 2]",
                "wasm_gpu_frame_build_fps: n=2 [3 4.5]"
            ]
        );
        assert!(sample_lines(&json("{}")).is_empty());
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
