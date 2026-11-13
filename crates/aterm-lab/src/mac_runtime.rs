// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Q4: release-product runtime rows in a disposable macOS guest. A committed source snapshot,
//! its submodule, and the resolved compiler are transported as data; the guest builds its own
//! verifier once. The bundle is never executed on the host. Every take retains its complete
//! stdout, stderr, exit status, guest resource usage, and host load, including failed takes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{Stdio, command};

use crate::mac::{Vm, not_run};
use crate::paths::paths;
use crate::util::shell_quote as q;
use crate::{Fail, Res, stop};

mod count_compare;
mod measure_costs;

pub(crate) const COUNT_USAGE: &str = "  count-ranks --record DIR [--candidate DIR]\n                Recompute B14's exact count ranks from retained Q4 logs; no product is run.\n                Candidate is five later default-policy takes on the same bundle and guest shape.\n                Relative directories are resolved from this checkout's root.\n  settle-ranks --record DIR\n                Recompute the matched idle-RSS setup experiment, including every row's verdicts.\n  wait-ranks --record DIR\n                Recompute the matched Count-readiness experiments, including setup spans.\n";

pub(crate) fn settle_ranks(args: &[String]) -> Res {
    let [flag, dir] = args else {
        return Err(Fail::usage(COUNT_USAGE));
    };
    if flag != "--record" {
        return Err(Fail::usage(COUNT_USAGE));
    }
    println!(
        "{}",
        count_compare::settle_records(&paths().repo.join(dir))?
    );
    Ok(0)
}

pub(crate) fn wait_ranks(args: &[String]) -> Res {
    let [flag, dir] = args else {
        return Err(Fail::usage(COUNT_USAGE));
    };
    if flag != "--record" {
        return Err(Fail::usage(COUNT_USAGE));
    }
    println!("{}", count_compare::wait_records(&paths().repo.join(dir))?);
    Ok(0)
}

fn count_args(args: &[String], repo: &Path) -> Result<(PathBuf, Option<PathBuf>), Fail> {
    match args {
        [flag, record] if flag == "--record" => Ok((repo.join(record), None)),
        [flag, record, next, candidate] if flag == "--record" && next == "--candidate" => {
            Ok((repo.join(record), Some(repo.join(candidate))))
        }
        _ => Err(Fail::usage(COUNT_USAGE)),
    }
}

pub(crate) fn count_ranks(args: &[String]) -> Res {
    if args == ["--help"] {
        print!("{COUNT_USAGE}");
        return Ok(0);
    }
    let (record, candidate) = count_args(args, &paths().repo)?;
    println!("{}", count_compare::records(&record, candidate.as_deref())?);
    Ok(0)
}

pub(crate) const USAGE: &str = "\
  runtime --app BUNDLE --record DIR [--lane measure|cursor|rewind|rewind-control|cat|compare|fabric|floors] [--image REF]
      [--baseline BUNDLE]
      [--cpus N] [--memory-gib N] [--disk-gb N]
      [--display window|headless]
                Q4: five takes of this committed checkout's verify measure --app BUNDLE, or a
                perf lane, inside one disposable macOS guest. Copies the resolved Trust compiler
                and builds the verifier in the guest; no release product runs on the host.
                Uses the image's admin account without sudo; requires SIP on and performs no
                privileged guest preparation. Inputs and builds stay in that account's home.
                With --baseline, measure/cursor/cat warm both images and alternate their order
                for five paired takes in the same guest, retaining each arm separately.
                Default bounds: 8 vCPUs, 32 GiB RAM, 60 decimal GB disk; --cpus, --memory-gib
                and --disk-gb name larger or smaller fixed bounds, recorded with every run.
                The shipped fabric link needs --memory-gib 48 --disk-gb 80. The clone and builds are
                deleted at the end, including failure. One of the account's two guest slots is
                held through boot and teardown. DIR keeps JSON rows and raw logs (never a
                cached qualification); a missing image is pulled automatically. --lane compare
                compares the supplied bundle with itself as an instrument/noise control.
                Window is the default: one owned Tart window under the shared display lock.
                Headless is a presentation-clock negative control; cursor runs only the GUI
                cursor-pacing burst, to qualify that clock before the full matrix.
                Qualified count rows run together; timing rows run alone. The sequential
                count experiment was retired after B14 qualification.
                rewind runs only recording throughput; rewind-control runs the identical plan
                with recording off in both arms, an A/A noise control with no recording verdict.
";

// The pinned image's login account owns all runtime inputs and build products.
const ROOT: &str = "/Users//admin/.ubd/runtime.noindex";
const TAKES: u32 = 5;
const MEMORY_MIB: u32 = 32768;
const CPUS: u32 = 8;
const DISK_GB: u32 = 60;
// The first Q4 base pulled on m118, 2026-11-06. Pin the image rather than its mutable tag.
const IMAGE: &str = "ghcr.io/cirruslabs/macos-golden-gate-base@sha256:14f4fc32a778763ccae1a4d543ea0f177720c570dd7f859eda10b9e0020c7d05";
// A build or a complete perf compare may take much longer than a single measurement row.
const BUILD: Duration = Duration::from_secs(7200);
const TAKE: Duration = Duration::from_secs(3600);

#[derive(Debug, PartialEq, Eq)]
struct Ask {
    app: PathBuf,
    baseline: Option<PathBuf>,
    record: PathBuf,
    lane: String,
    image: String,
    cpus: u32,
    memory_mib: u32,
    disk_gb: u32,
    graphics: bool,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let mut app = None;
    let mut baseline = None;
    let mut record = None;
    let mut lane = "measure".to_owned();
    let mut image = std::env::var("LAB_MAC_IMAGE").unwrap_or_else(|_| IMAGE.into());
    let mut cpus = CPUS;
    let mut memory_mib = MEMORY_MIB;
    let mut disk_gb = DISK_GB;
    let mut graphics = true;
    let mut at = 0;
    while at < args.len() {
        let flag = &args[at];
        at += 1;
        let value = args
            .get(at)
            .ok_or_else(|| Fail::usage(format!("{flag} needs a value")))?;
        at += 1;
        match flag.as_str() {
            "--app" if app.is_none() => app = Some(PathBuf::from(value)),
            "--baseline" if baseline.is_none() => baseline = Some(PathBuf::from(value)),
            "--record" if record.is_none() => record = Some(PathBuf::from(value)),
            "--lane"
                if matches!(
                    value.as_str(),
                    "measure"
                        | "cursor"
                        | "rewind"
                        | "rewind-control"
                        | "cat"
                        | "compare"
                        | "fabric"
                        | "floors"
                ) =>
            {
                lane.clone_from(value)
            }
            "--image" => image.clone_from(value),
            "--display" if matches!(value.as_str(), "window" | "headless") => {
                graphics = value == "window"
            }
            "--cpus" | "--memory-gib" | "--disk-gb" => {
                let n = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| Fail::usage(format!("{flag} needs a positive integer")))?;
                match flag.as_str() {
                    "--cpus" => cpus = n,
                    "--disk-gb" => disk_gb = n,
                    _ => {
                        memory_mib = n
                            .checked_mul(1024)
                            .ok_or_else(|| Fail::usage("--memory-gib is too large"))?
                    }
                }
            }
            _ => {
                return Err(Fail::usage(format!(
                    "runtime: unknown/duplicate option or lane {flag} {value}"
                )));
            }
        }
    }
    if baseline.is_some() && !matches!(lane.as_str(), "measure" | "cursor" | "cat") {
        return Err(Fail::usage("--baseline supports measure, cursor and cat"));
    }
    Ok(Ask {
        app: app.ok_or_else(|| Fail::usage("runtime needs --app BUNDLE"))?,
        baseline,
        record: record.ok_or_else(|| Fail::usage("runtime needs --record DIR"))?,
        lane,
        image,
        cpus,
        memory_mib,
        disk_gb,
        graphics,
    })
}

fn checked(program: &str, args: &[String], cwd: &Path) -> Result<String, Fail> {
    let out = command(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(Fail::die(format!(
            "{program}: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn load() -> String {
    checked(
        "/usr/sbin/sysctl",
        &["-n".into(), "vm.loadavg".into()],
        &paths().repo,
    )
    .unwrap_or_else(|e| e.msg.unwrap_or_else(|| "load unavailable".into()))
}

fn sampled<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
    std::thread::scope(|scope| {
        let (end, until) = std::sync::mpsc::channel::<()>();
        let reader = scope.spawn(move || {
            let mut readings = vec![load()];
            while matches!(
                until.recv_timeout(Duration::from_secs(1)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                readings.push(load());
            }
            readings.push(load());
            readings
        });
        let result = run();
        drop(end);
        (result, reader.join().unwrap_or_default())
    })
}

fn max_load(readings: &[String]) -> Option<f64> {
    readings
        .iter()
        .filter_map(|s| {
            s.trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
                .split_whitespace()
                .next()?
                .parse::<f64>()
                .ok()
        })
        .filter(|v| v.is_finite())
        .max_by(f64::total_cmp)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    crate::util::write_fresh(path, bytes).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write_json(path: &Path, value: &Value) -> Result<(), Fail> {
    let mut bytes = aterm_json::to_string(value)
        .map_err(|e| format!("JSON: {e}"))?
        .into_bytes();
    bytes.push(b'\n');
    write(path, &bytes)
}

/// Timing drivers belong to the console's app band. An Interactive job is still a daemon
/// task: neither its driver nor its subjects can take the UI role the instrument requests.
/// launchd owns the job; the result file is written only after the measured child exits.
fn app_job(root: &Path, phase: &str, script: &str) -> Value {
    let dir = root.join(phase);
    let path = |name: &str| q(&dir.join(name).display().to_string());
    let program = format!(
        "printf started > {}; /bin/sh -c {}; result=$?; printf '%s\\n' \"$result\" > {}; /bin/mv {} {}",
        path("started"),
        q(script),
        path("exit.tmp"),
        path("exit.tmp"),
        path("exit")
    );
    json!({
        "Label": format!("systems.alab.aterm.runtime.{phase}"),
        "ProgramArguments": ["/bin/sh", "-c", program],
        "ProcessType": "App", "RunAtLoad": true,
        "StandardOutPath": dir.join("stdout").display().to_string(),
        "StandardErrorPath": dir.join("stderr").display().to_string()
    })
}

fn app_exec(
    vm: &Vm,
    ask: &Ask,
    phase: &str,
    script: &str,
    limit: Duration,
) -> Result<std::process::Output, Fail> {
    let job = app_job(&Path::new(ROOT).join("driver"), phase, script);
    write_json(&ask.record.join(format!("{phase}.driver.json")), &job)?;
    let encoded = aterm_json::to_string(&job).map_err(|e| format!("driver JSON: {e}"))?;
    let dir = format!("{ROOT}/driver/{phase}");
    let label = job["Label"].as_str().expect("job label");
    let command = format!(
        "set -eu; mkdir -p {dir}; printf '%s' {} > {dir}/job.json; \
         /usr/bin/plutil -convert xml1 -o {dir}/job.plist {dir}/job.json; \
         uid=$(id -u); /bin/launchctl bootstrap gui/$uid {dir}/job.plist; \
         count=0; while ! test -f {dir}/exit; do \
           if ! test -f {dir}/started && test \"$count\" -ge 60; then \
             /bin/launchctl print gui/$uid/{label} >&2; exit 3; \
           fi; count=$((count + 1)); sleep 1; \
         done; \
         /bin/launchctl print gui/$uid/{label} > {dir}/launchd.txt 2>&1 || true; \
         /bin/launchctl bootout gui/$uid/{label} >/dev/null 2>&1 || true; \
         cat {dir}/stdout; cat {dir}/stderr >&2; exit \"$(cat {dir}/exit)\"",
        q(&encoded)
    );
    let out = vm.exec(&command, limit)?;
    let state = vm.exec(&format!("cat {dir}/launchd.txt"), crate::mac::STEP)?;
    write(
        &ask.record.join(format!("{phase}.launchd.txt")),
        &state.stdout,
    )?;
    Ok(out)
}

/// `time -lp`'s own fields, not the program's numbers. Missing resource readings stay null.
fn usage(stderr: &str) -> Value {
    let mut fields = aterm_json::Map::new();
    for line in stderr.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() == 2 && matches!(words[0], "real" | "user" | "sys") {
            if let Ok(n) = words[1].parse::<f64>() {
                fields.insert(words[0].into(), json!(n));
            }
        } else if words.get(1..) == Some(&["maximum", "resident", "set", "size"][..])
            && let Ok(n) = words[0].parse::<u64>()
        {
            fields.insert("peak_rss_bytes".into(), json!(n));
        }
    }
    Value::Object(fields)
}

fn spread(values: &[f64]) -> Value {
    if values.len() != TAKES as usize || values.iter().any(|v| !v.is_finite()) {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({"takes": values, "min": sorted[0], "median": sorted[2], "max": sorted[4],
        "range": sorted[4] - sorted[0]})
}

fn experiment_exit(codes: &[Option<i32>], paired: bool) -> i32 {
    if codes.iter().any(|c| matches!(c, None | Some(1))) {
        1
    } else if codes.len() != (TAKES as usize + 1) * if paired { 2 } else { 1 }
        || codes.iter().any(|c| *c != Some(0))
    {
        3
    } else {
        0
    }
}

fn disk_room(df: &str, disk_gb: u32) -> bool {
    // df -g reports GiB; Tart 2.40.1's resizeRawDisk/resizeASIFDisk take decimal GB.
    crate::mac_nux::df_free_gb(df)
        .and_then(|gib| gib.checked_mul(1 << 30))
        .is_some_and(|bytes| bytes >= (u64::from(disk_gb) + 30) * 1_000_000_000)
}

fn verifier_log(stdout: &str) -> Option<&str> {
    stdout.lines().find_map(|line| {
        let path = line.strip_prefix("log: ")?;
        let relative = path.strip_prefix(&format!("{ROOT}/target.noindex/verify/"))?;
        if !relative_member(relative) {
            return None;
        }
        let file = Path::new(relative).file_name()?.to_str()?;
        let (time, pid) = file.strip_suffix(".log")?.split_once('-')?;
        (!time.is_empty()
            && !pid.is_empty()
            && time.bytes().chain(pid.bytes()).all(|b| b.is_ascii_digit()))
        .then_some(path)
    })
}

fn ledger_member(path: &str) -> bool {
    path.starts_with("docs/measured/") && relative_member(path)
}

fn relative_member(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// stdout contains the verifier's summary. Its row values live in the log; perf also writes
/// structured records beside its source. Bring those back before the disposable disk goes.
fn collect(vm: &Vm, ask: &Ask, name: &str, stdout: &str) -> Result<Value, Fail> {
    let mut artifacts = aterm_json::Map::new();
    if let Some(path) = verifier_log(stdout) {
        let log = vm.sh(&format!("cat -- {}", q(path)), crate::mac::STEP)?;
        let file = format!("{name}.verifier.log");
        write(&ask.record.join(&file), log.as_bytes())?;
        artifacts.insert("verifier_log".into(), json!(file));
    } else if matches!(ask.lane.as_str(), "measure" | "cursor") {
        return Err(not_run("guest measure named no row-level log to retain"));
    }
    if !matches!(ask.lane.as_str(), "measure" | "cursor") {
        let listed = vm.sh(
            &format!(
                "cd {ROOT}/source && git ls-files --others --exclude-standard -- docs/measured"
            ),
            crate::mac::STEP,
        )?;
        let files: Vec<_> = listed.lines().collect();
        if files.iter().any(|f| !ledger_member(f)) {
            return Err(not_run("guest perf returned a path outside its ledger"));
        }
        if !files.is_empty() {
            let args = files.iter().map(|f| q(f)).collect::<Vec<_>>().join(" ");
            let out = vm.exec(
                &format!("tar -C {ROOT}/source -cf - -- {args}"),
                crate::mac::STEP,
            )?;
            if !out.status.success() {
                return Err(not_run("guest perf records could not be archived"));
            }
            let file = format!("{name}.records.tar");
            write(&ask.record.join(&file), &out.stdout)?;
            artifacts.insert("perf_records".into(), json!(file));
        }
    }
    Ok(Value::Object(artifacts))
}

fn lane_args(lane: &str) -> Vec<String> {
    let bin = format!("{ROOT}/aterm.app/Contents/MacOS/aterm");
    match lane {
        "measure" => vec![
            "measure".into(),
            "--app".into(),
            format!("{ROOT}/aterm.app"),
        ],
        "cursor" => vec![
            "measure".into(),
            "--app".into(),
            format!("{ROOT}/aterm.app"),
            "--cursor-pacing".into(),
        ],
        "cat" => vec![
            "perf".into(),
            "cat".into(),
            "--binary".into(),
            bin,
            "--dev-smoke".into(),
        ],
        "compare" => vec![
            "perf".into(),
            "compare".into(),
            "--baseline".into(),
            bin.clone(),
            "--candidate".into(),
            bin,
            "--allow-identical".into(),
            "--dev-smoke".into(),
        ],
        "fabric" => vec![
            "perf".into(),
            "fabric".into(),
            "--profile".into(),
            "shipped".into(),
        ],
        "floors" => vec!["perf".into(), "--binary".into(), bin],
        _ => unreachable!("parse admits only known lanes"),
    }
}

/// Cargo names executable artifacts, including under Trust's per-unit build layout. Refuse
/// missing or conflicting paths instead of selecting an arbitrary executable from the cache.
fn rewind_command(artifacts: &str, control: bool) -> Result<String, Fail> {
    let mut executables =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for line in artifacts.lines() {
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"].as_str() != Some("compiler-artifact") {
            continue;
        }
        if let (Some(name), Some(exe)) = (v["target"]["name"].as_str(), v["executable"].as_str())
            && exe.starts_with(&format!("{ROOT}/target.noindex/"))
            && relative_member(exe.trim_start_matches('/'))
        {
            executables
                .entry(name.into())
                .or_default()
                .insert(exe.into());
        }
    }
    let one = |name: &str| -> Result<&str, Fail> {
        let paths = executables
            .get(name)
            .filter(|p| p.len() == 1)
            .ok_or_else(|| {
                not_run(format!(
                    "focused rewind needs exactly one Cargo executable for {name}"
                ))
            })?;
        Ok(paths.first().expect("one path").as_str())
    };
    let bench = one("rewind")?;
    let producer = one("fake-tui")?;
    let subject = format!("{ROOT}/aterm.app/Contents/MacOS/aterm");
    if control {
        Ok(format!(
            "{} --throughput-control {} {} {ROOT}/tmp",
            q(bench),
            q(&subject),
            q(producer)
        ))
    } else {
        Ok(format!(
            "env ATERM_REWIND_BIN={} CARGO_BIN_EXE_fake-tui={} {} --filter recording_keeps_parse_throughput",
            q(&subject),
            q(producer),
            q(bench)
        ))
    }
}

fn environment(jobs: u32) -> String {
    // This is the guest's compiler, not an override of the host's flags or toolchain.
    format!(
        "export PATH={ROOT}/toolchain/bin:/usr/bin:/bin:/usr/sbin:/sbin; \
        export TRUST_STAGE2_BIN={ROOT}/toolchain/bin; \
        export RUSTC={ROOT}/toolchain/bin/trustc; \
        export CARGO_TARGET_DIR={ROOT}/target.noindex; \
        export CARGO_BUILD_JOBS={jobs}; \
        export TMPDIR={ROOT}/tmp/; cd {ROOT}/source; "
    )
}

fn take_order(paired: bool) -> Vec<(u32, &'static str)> {
    (1..=TAKES)
        .flat_map(|take| {
            let arms = if !paired {
                vec!["candidate"]
            } else if take % 2 == 1 {
                vec!["baseline", "candidate"]
            } else {
                vec!["candidate", "baseline"]
            };
            arms.into_iter().map(move |arm| (take, arm))
        })
        .collect()
}

fn arm_command(command: &str, arm: &str) -> String {
    if arm == "baseline" {
        command.replace(
            &format!("{ROOT}/aterm.app"),
            &format!("{ROOT}/baseline.app"),
        )
    } else {
        command.to_owned()
    }
}

/// Run the lane and retain a refusal as a record too: unavailable input is never a pass.
pub(crate) fn run(args: &[String]) -> Res {
    if args == ["--help"] || args == ["help"] {
        print!("{USAGE}");
        return Ok(0);
    }
    stop::install();
    let ask = parse(args)?;
    std::fs::create_dir(&ask.record)
        .map_err(|e| format!("{} (must be a new directory): {e}", ask.record.display()))?;
    let result = run_in(&ask);
    if let Err(e) = &result {
        write_json(
            &ask.record.join("incomplete.json"),
            &json!({"schema": "aterm-mac-runtime-v1", "complete": false, "reason": e.msg}),
        )?;
    }
    result
}

fn resolve_tools(
    root: &Path,
    home: &Path,
    prefix: &Path,
    path: &std::ffi::OsStr,
    stage: Option<&Path>,
    before: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<aterm_toolchain::Toolchain, Fail> {
    let lock = atpkg::trust_lock::found_for(root, Some(prefix));
    let tools = aterm_toolchain::Toolchain::discover_with_store_guarded(
        stage,
        &aterm_toolchain::rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home),
        Some(prefix),
        path,
        aterm_toolchain::pinned_channel(root).as_deref(),
        lock.as_ref(),
        before,
    )
    .map_err(not_run)?;
    if let Some(why) = &tools.lock_refusal {
        return Err(not_run(why));
    }
    if !tools.have_targo() {
        return Err(not_run(
            "the source snapshot's Trust compiler is unavailable",
        ));
    }
    Ok(tools)
}

fn run_in(ask: &Ask) -> Res {
    let repo = &paths().repo;
    let dirty = checked("git", &["status".into(), "--porcelain".into()], repo)?;
    if !dirty.is_empty() {
        return Err(Fail::usage(
            "runtime measures committed code: commit the checkout first",
        ));
    }
    let app = std::fs::canonicalize(&ask.app).map_err(|e| format!("{}: {e}", ask.app.display()))?;
    let bin = app.join("Contents/MacOS/aterm");
    let app_sha = crate::windows::sha256_file(&bin)?;
    let baseline = ask
        .baseline
        .as_ref()
        .map(|path| -> Result<_, Fail> {
            let app =
                std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let sha = crate::windows::sha256_file(&app.join("Contents/MacOS/aterm"))?;
            Ok((app, sha))
        })
        .transpose()?;
    let source = checked("git", &["rev-parse".into(), "HEAD".into()], repo)?;
    let submodule = checked(
        "git",
        &["rev-parse".into(), "HEAD".into()],
        &repo.join("vendor/astream"),
    )?;
    let home = Path::new(&paths().real_home);
    let prefix = aterm_toolchain::atpkg_prefix(
        home,
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
    );
    let mut compiler_leases = Vec::new();
    let tool = resolve_tools(
        repo,
        home,
        &prefix,
        &std::env::var_os("PATH").unwrap_or_default(),
        std::env::var_os("TRUST_STAGE2_BIN")
            .as_deref()
            .map(Path::new),
        &mut |dir| {
            if let Some(lease) =
                aterm_toolchain::lease::take_for_dir(&prefix, dir, "lab mac runtime")
                    .map_err(|e| format!("compiler lease: {e}"))?
            {
                compiler_leases.push(lease);
            }
            Ok(())
        },
    )?;
    let compiler = std::fs::canonicalize(
        tool.stage2_dir
            .parent()
            .ok_or_else(|| not_run("Trust sysroot has no parent"))?,
    )
    .map_err(|e| format!("Trust sysroot: {e}"))?;
    if !compiler.join("bin/trustc").is_file() {
        return Err(not_run("the resolved Trust sysroot has no bin/trustc"));
    }
    let tart = crate::mac::tart_to_run()?;
    write_json(
        &ask.record.join("inputs.json"),
        &json!({
            "schema": "aterm-mac-runtime-v1", "source_commit": source, "astream_commit": submodule,
            "bundle_sha256": app_sha, "baseline_bundle_sha256": baseline.as_ref().map(|(_, sha)| sha), "image": ask.image, "lane": ask.lane,
            "driver": "gui-launch-agent/App", "count_policy": "qualified", "privilege": "guest-user",
            "compiler": compiler.display().to_string(), "compiler_lock_override": tool.lock_override,
            "guest": {"cpus": ask.cpus, "memory_mib": ask.memory_mib, "disk_gb": ask.disk_gb, "graphics": ask.graphics}
        }),
    )?;
    // The disk bound includes the guest's entire build cache, not just the app and image.
    let df = checked(
        "/bin/df",
        &["-g".into(), "/System/Volumes/Data".into()],
        repo,
    )?;
    if !disk_room(&df, ask.disk_gb) {
        return Err(not_run(format!(
            "runtime needs {} GB free ({} GB guest plus 30 GB host margin): {df}",
            u64::from(ask.disk_gb) + 30,
            ask.disk_gb
        )));
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("mac-runtime-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| format!("runtime scratch: {e}"))?;
    let share = scratch.path().join("share");
    std::fs::create_dir(&share).map_err(|e| format!("share: {e}"))?;
    for (dir, name) in [
        (repo.clone(), "source.tar"),
        (repo.join("vendor/astream"), "astream.tar"),
    ] {
        checked(
            "git",
            &[
                "archive".into(),
                "--format=tar".into(),
                format!("--output={}", share.join(name).display()),
                "HEAD".into(),
            ],
            &dir,
        )?;
    }
    checked(
        "/usr/bin/tar",
        &[
            "-cf".into(),
            share.join("toolchain.tar").display().to_string(),
            "-C".into(),
            compiler.display().to_string(),
            ".".into(),
        ],
        repo,
    )?;
    checked(
        "/usr/bin/ditto",
        &[
            "-c".into(),
            "-k".into(),
            "--keepParent".into(),
            app.display().to_string(),
            share.join("app.zip").display().to_string(),
        ],
        repo,
    )?;
    if let Some((app, _)) = &baseline {
        checked(
            "/usr/bin/ditto",
            &[
                "-c".into(),
                "-k".into(),
                "--keepParent".into(),
                app.display().to_string(),
                share.join("baseline.zip").display().to_string(),
            ],
            repo,
        )?;
    }
    let base =
        crate::mac::existing_prepared_base(&tart, &ask.image)?.unwrap_or_else(|| ask.image.clone());
    let df = checked(
        "/bin/df",
        &["-g".into(), "/System/Volumes/Data".into()],
        repo,
    )?;
    if !disk_room(&df, ask.disk_gb) {
        return Err(not_run(format!(
            "after image preparation, runtime needs {} GB free: {df}",
            u64::from(ask.disk_gb) + 30
        )));
    }
    let guest = crate::vm_admit::Guest {
        lane: "mac-runtime",
        vm: "mac-runtime",
        memory: u64::from(ask.memory_mib) << 20,
        cpus: ask.cpus,
    };
    let mut admitted = crate::vm_admit::admit(&guest, true)?;
    let mut vm = Vm::clone_from(&tart, &base, scratch.path(), &share, false)?;
    vm.graphics = ask.graphics;
    checked(
        &tart,
        &[
            "set".into(),
            vm.name.clone(),
            "--cpu".into(),
            ask.cpus.to_string(),
            "--memory".into(),
            ask.memory_mib.to_string(),
            "--disk-size".into(),
            ask.disk_gb.to_string(),
        ],
        repo,
    )?;
    vm.boot()?;
    let sip = vm.sh("csrutil status", crate::mac::STEP)?;
    if !crate::mac::sip_on(&sip) {
        return Err(not_run(
            "runtime requires a SIP-on image; it performs no privileged guest preparation",
        ));
    }
    if let Some(pid) = vm.pid() {
        admitted.credit("mac-runtime", &[pid]);
    }
    let mut setup = format!(
        "set -eu; test \"$HOME\" = /Users//admin; mkdir -p {ROOT}/source {ROOT}/toolchain {ROOT}/tmp; \
        tar -xf {share}/source.tar -C {ROOT}/source; \
        tar -xf {share}/astream.tar -C {ROOT}/source/vendor/astream; \
        tar -xf {share}/toolchain.tar -C {ROOT}/toolchain; \
        ditto -x -k {share}/app.zip {ROOT}/unpack; \
        mv {ROOT}/unpack/{app_name} {ROOT}/aterm.app; \
        {env} git init -q; git add .; git -c user.name='Andrew Yates' -c user.email='lab@localhost' commit -qm 'Q4 snapshot {source}'; \
        test \"$(shasum -a 256 {ROOT}/aterm.app/Contents/MacOS/aterm | cut -d ' ' -f 1)\" = {app_sha}; \
        {ROOT}/toolchain/bin/trustc -Vv; \
        {ROOT}/toolchain/bin/targo --config {build_dir_config} --unverified build -p aterm-verify; ",
        share = q(crate::mac::GUEST_SHARE),
        app_name = q(app
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Fail::usage("bundle name is not UTF-8"))?),
        env = environment(admitted.jobs),
        build_dir_config = q(&format!(
            "build.build-dir='{}'",
            aterm_toolchain::build_cache::target_build_path(Path::new(&format!(
                "{ROOT}/target.noindex"
            )))
            .display()
        ))
    );
    if let Some((app, sha)) = &baseline {
        setup.push_str(&format!(
            "ditto -x -k {share}/baseline.zip {ROOT}/baseline-unpack; \
             mv {ROOT}/baseline-unpack/{name} {ROOT}/baseline.app; \
             test \"$(shasum -a 256 {ROOT}/baseline.app/Contents/MacOS/aterm | cut -d ' ' -f 1)\" = {sha}; ",
            share=q(crate::mac::GUEST_SHARE),
            name=q(app.file_name().and_then(|name| name.to_str()).ok_or_else(|| Fail::usage("baseline bundle name is not UTF-8"))?),
            sha=q(sha),
        ));
    }
    let setup_out = vm.exec(&setup, BUILD)?;
    write(&ask.record.join("setup.stdout"), &setup_out.stdout)?;
    write(&ask.record.join("setup.stderr"), &setup_out.stderr)?;
    if !setup_out.status.success() {
        return Err(not_run(format!(
            "guest verifier build failed: {}; see setup.stderr",
            setup_out.status
        )));
    }
    // Build measurement benches before taking time: the five takes use the same warm cache.
    let prebuild = format!(
        "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; {ROOT}/target.noindex/debug/aterm-verify measure --app {ROOT}/aterm.app --compile-only --admit now",
        environment(admitted.jobs)
    );
    if matches!(ask.lane.as_str(), "measure" | "cursor") {
        let out = vm.exec(&prebuild, BUILD)?;
        write(&ask.record.join("prebuild.stdout"), &out.stdout)?;
        write(&ask.record.join("prebuild.stderr"), &out.stderr)?;
        if !out.status.success() {
            return Err(not_run(
                "guest measurement prebuild failed; see prebuild.stderr",
            ));
        }
    }
    let focused = matches!(ask.lane.as_str(), "rewind" | "rewind-control");
    let command = if focused {
        let out = vm.exec(&format!("{} {ROOT}/toolchain/bin/targo --unverified bench --profile test -p aterm-conformance --bench rewind --no-run --message-format=json", environment(admitted.jobs)), BUILD)?;
        write(&ask.record.join("prebuild.stdout"), &out.stdout)?;
        write(&ask.record.join("prebuild.stderr"), &out.stderr)?;
        if !out.status.success() {
            return Err(not_run("focused rewind build failed; see prebuild.stderr"));
        }
        rewind_command(
            &String::from_utf8_lossy(&out.stdout),
            ask.lane == "rewind-control",
        )?
    } else {
        format!(
            "{ROOT}/target.noindex/debug/aterm-verify {}",
            lane_args(&ask.lane)
                .iter()
                .map(|a| q(a))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let untimed = format!(
        "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; {command}",
        environment(admitted.jobs)
    );
    // Populate every lane's compiler and corpus caches before measuring. This run is retained,
    // too; a red warm-up is never erased by five later green takes.
    println!(
        "mac runtime: {} warm-up (not one of the five takes)",
        ask.lane
    );
    let paired = baseline.is_some();
    let mut codes = Vec::new();
    let mut warmups = Vec::new();
    for arm in if paired {
        vec!["baseline", "candidate"]
    } else {
        vec!["candidate"]
    } {
        let name = if paired {
            format!("warmup-{arm}")
        } else {
            "warmup".into()
        };
        let warm = app_exec(&vm, ask, &name, &arm_command(&untimed, arm), BUILD)?;
        write(&ask.record.join(format!("{name}.stdout")), &warm.stdout)?;
        write(&ask.record.join(format!("{name}.stderr")), &warm.stderr)?;
        let artifacts = collect(&vm, ask, &name, &String::from_utf8_lossy(&warm.stdout))?;
        let row = json!({"arm": arm, "exit": warm.status.code(), "success": warm.status.success(), "artifacts": artifacts});
        write_json(&ask.record.join(format!("{name}.json")), &row)?;
        warmups.push(row);
        codes.push(warm.status.code());
    }
    let invocation = format!(
        "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; /usr/bin/time -lp {command}",
        environment(admitted.jobs)
    );
    let mut rows = Vec::new();
    let mut real = Vec::new();
    let mut cpu = Vec::new();
    for (take, arm) in take_order(paired) {
        stop::check()?;
        let sha = if arm == "baseline" {
            &baseline.as_ref().expect("paired baseline").1
        } else {
            &app_sha
        };
        let name = if paired {
            format!("take-{take}-{arm}")
        } else {
            format!("take-{take}")
        };
        let selected = arm_command(&invocation, arm);
        println!(
            "mac runtime: {} take {take}/{TAKES} {arm}, bundle {sha}",
            ask.lane
        );
        let (out, loads) = sampled(|| app_exec(&vm, ask, &name, &selected, TAKE));
        let out = out?;
        codes.push(out.status.code());
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let artifacts = collect(&vm, ask, &name, &stdout)?;
        write(&ask.record.join(format!("{name}.stdout")), &out.stdout)?;
        write(&ask.record.join(format!("{name}.stderr")), &out.stderr)?;
        let resource = usage(&stderr);
        if let Some(n) = resource["real"].as_f64() {
            real.push(n);
        }
        if let (Some(u), Some(s)) = (resource["user"].as_f64(), resource["sys"].as_f64()) {
            cpu.push(u + s);
        }
        rows.push(
            json!({"take": take, "arm": arm, "bundle_sha256": sha, "command": selected, "exit": out.status.code(), "success": out.status.success(),
            "usage": resource, "artifacts": artifacts, "host_load_samples": loads, "host_max_load_1m": max_load(&loads),
            "stdout": stdout.as_ref(), "stderr": stderr.as_ref()}),
        );
        write_json(
            &ask.record.join(format!("{name}.json")),
            rows.last().expect("just pushed"),
        )?;
    }
    let exit = experiment_exit(&codes, paired);
    write_json(
        &ask.record.join("rows.json"),
        &json!({"schema": if paired { "aterm-mac-runtime-paired-v1" } else { "aterm-mac-runtime-v1" },
        "complete": true, "all_exits_zero": exit == 0, "exit": exit,
        "warmup_exit": if paired { None } else { codes[0] },
        "warmup_artifacts": if paired { Value::Null } else { warmups[0]["artifacts"].clone() },
        "warmups": warmups, "takes_per_arm": TAKES, "source_commit": source, "astream_commit": submodule,
        "bundle_sha256": app_sha, "baseline_bundle_sha256": baseline.as_ref().map(|(_, sha)| sha), "image": ask.image, "lane": ask.lane,
        "driver": "gui-launch-agent/App", "count_policy": "qualified",
        "guest": {"cpus": ask.cpus, "memory_mib": ask.memory_mib, "disk_gb": ask.disk_gb, "graphics": ask.graphics},
        "granted_share": admitted.jobs,
        "command": command, "wall_s": spread(&real), "cpu_s": spread(&cpu), "rows": rows}),
    )?;
    drop(vm); // Stop the VM and delete the complete disk, its builds, and scratch identities.
    Ok(exit)
}

#[cfg(test)]
#[path = "mac_runtime_tests.rs"]
mod tests;
