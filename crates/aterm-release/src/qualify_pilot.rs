// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local contention experiments. Builds release products without executing or publishing them.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::BufRead as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub(crate) struct Options {
    verify: PathBuf,
    before: Option<PathBuf>,
    mode: String,
}

pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let mut verify = None;
    let mut mode = None;
    let mut before = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--verify" if verify.is_none() => verify = Some(PathBuf::from(value)),
            "--before-verify" if before.is_none() => before = Some(PathBuf::from(value)),
            "--mode"
                if mode.is_none()
                    && matches!(
                        value.as_str(),
                        "alone"
                            | "shared"
                            | "chain"
                            | "tree"
                            | "series"
                            | "qualify-before"
                            | "qualify-after"
                            | "qualify-series"
                    ) =>
            {
                mode = Some(value)
            }
            _ => return Err(format!("invalid qualify-pilot option {flag} {value}")),
        }
    }
    let options = Options {
        verify: verify.ok_or("qualify-pilot requires --verify PATH")?,
        before,
        mode: mode.ok_or("qualify-pilot requires --mode alone|shared|chain|tree|series|qualify-before|qualify-after|qualify-series")?,
    };
    if (options.mode == "series") != options.before.is_some() {
        return Err("only series requires --before-verify PATH".into());
    }
    Ok(options)
}

/// Unix nice is a relative CPU priority, not a background task band or a core cap: lower
/// priority work can use all idle cores. It changes neither the shared FIFO nor memory claims.
pub(crate) fn invocation(program: &Path, lower: bool) -> (PathBuf, Vec<OsString>) {
    if lower && cfg!(unix) {
        (
            PathBuf::from("/usr/bin/nice"),
            vec!["-n".into(), "10".into(), program.as_os_str().to_owned()],
        )
    } else {
        (program.to_owned(), Vec::new())
    }
}

fn descendants(rows: &[aterm_sysprobe::procs::ProcessReading], root: u32) -> BTreeSet<u32> {
    let mut ids = BTreeSet::from([root]);
    loop {
        let before = ids.len();
        for row in rows {
            if ids.contains(&row.ppid) {
                ids.insert(row.pid);
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}

fn sample(root: u32, elapsed: f64, global: &mut BTreeMap<(u32, u64), u64>) -> aterm_json::Value {
    let reading_started = Instant::now();
    let sweep = aterm_sysprobe::procs::processes();
    let initial = global.is_empty();
    let mut global_delta = 0_u64;
    if let Some(sweep) = &sweep {
        for row in &sweep.procs {
            if let (Some(start), Some(cpu)) = (row.start, row.cpu_ns) {
                let before = global.entry((row.pid, start)).or_default();
                global_delta = global_delta.saturating_add(cpu.saturating_sub(*before));
                *before = cpu;
            }
        }
    }
    let rows = sweep.as_ref().map(|sweep| {
        let ids = descendants(&sweep.procs, root);
        sweep
            .procs
            .iter()
            .filter(|r| ids.contains(&r.pid) && r.pid != root)
            .map(|r| {
                aterm_json::json!({
                    "pid":r.pid, "start":r.start, "ppid":r.ppid, "name":r.name,
                    "cpu_ns":r.cpu_ns, "runnable_ns":r.runnable_ns,
                    "footprint":r.footprint,
                    "p_cpu_ns":r.cores.map(|c| c.p_cpu_ns),
                    "instructions":r.cores.map(|c| c.instructions),
                    "p_instructions":r.cores.map(|c| c.p_instructions),
                    "nice":r.scheduling.map(|s| observation(s.nice)),
                    "qos_cpu_ns":r.scheduling.and_then(|s| s.qos_cpu_ns).map(observation),
                })
            })
            .collect::<Vec<_>>()
    });
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    aterm_json::json!({"at_unix_s":epoch,"elapsed_s":elapsed+reading_started.elapsed().as_secs_f64(), "global_cpu_delta_ns":(!initial && sweep.is_some()).then_some(global_delta), "load":aterm_sysprobe::load_average(), "truncated":sweep.as_ref().map(|s| s.truncated), "members":rows})
}

#[derive(Default)]
struct Counters {
    cpu: u64,
    runnable: u64,
    performance: u64,
    qos: [u64; 7],
    runnable_missing: bool,
}

fn compiler_counters(pid: u64, counters: &BTreeMap<(u64, u64), Counters>) -> Option<&Counters> {
    let mut matches = counters
        .iter()
        .filter(|((candidate, _), _)| *candidate == pid);
    let (_, found) = matches.next()?;
    // A reused numeric PID cannot be joined to an event without its process birth identity.
    matches.next().is_none().then_some(found)
}

fn field(value: &aterm_json::Value, name: &str) -> u64 {
    value[name].as_u64().unwrap_or(0)
}

fn branch(
    row: &aterm_json::Value,
    rows: &BTreeMap<u64, &aterm_json::Value>,
    verifier: &str,
) -> &'static str {
    let mut found = "other";
    let mut current = row;
    let mut seen = BTreeSet::new();
    while seen.insert(field(current, "pid")) {
        match current["name"].as_str() {
            Some(name) if name == verifier => found = "tree",
            Some("targo") => found = "native",
            Some("cargo") => found = "compat",
            _ => {}
        }
        match rows.get(&field(current, "ppid")) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    found
}

/// Summarize a closed take. Sampling is a lower bound; wait4 remains the branch CPU total.
pub(crate) fn summarize(root: &Path) -> Result<(), String> {
    let result: aterm_json::Value = aterm_json::from_str(
        &crate::durable::read_text(&root.join("result.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let input = crate::durable::open_regular_file(&root.join("processes.jsonl"))
        .map_err(|e| e.to_string())?;
    let mut previous = BTreeMap::<(u64, u64), Counters>::new();
    let verifier = result["verify"]
        .as_str()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .unwrap_or("aterm-verify");
    let mut totals = BTreeMap::<String, Counters>::new();
    let mut bins = BTreeMap::<(u64, String), f64>::new();
    let mut nice = BTreeMap::<String, BTreeSet<i64>>::new();
    let mut last = 0.0_f64;
    let mut max_interval = 0.0_f64;
    let mut max_cores = 0.0_f64;
    let mut max_load = 0.0_f64;
    let mut unavailable = 0_u64;
    let mut unread_process_rows = 0_u64;
    let mut max_live_compilers = 0_usize;
    let mut max_progressing_compilers = 0_usize;
    let mut over_width_s = 0.0;
    let mut started_unix_s = None;
    let mut truncated_samples = 0_u64;
    for line in std::io::BufReader::new(input).lines() {
        let value: aterm_json::Value =
            aterm_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let at = value["elapsed_s"]
            .as_f64()
            .ok_or("sample lacks elapsed time")?;
        let interval = at - last;
        if started_unix_s.is_none() {
            started_unix_s = value["at_unix_s"].as_f64();
        }
        truncated_samples += u64::from(value["truncated"].as_bool() == Some(true));
        max_interval = max_interval.max(interval);
        if let Some(load) = value["load"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(aterm_json::Value::as_f64)
        {
            max_load = max_load.max(load);
        }
        let Some(members) = value["members"].as_array() else {
            unavailable += 1;
            last = at;
            continue;
        };
        let rows: BTreeMap<_, _> = members.iter().map(|row| (field(row, "pid"), row)).collect();
        let mut busy = 0.0;
        let mut live_compilers = 0_usize;
        let mut progressing_compilers = 0_usize;
        for row in members {
            if row["cpu_ns"].as_u64().is_none() || row["start"].as_u64().is_none() {
                unread_process_rows += 1;
                continue;
            }
            let role = branch(row, &rows, verifier);
            let key = (field(row, "pid"), field(row, "start"));
            let old = previous.entry(key).or_default();
            let now = Counters {
                cpu: field(row, "cpu_ns"),
                runnable: row["runnable_ns"].as_u64().unwrap_or(old.runnable),
                performance: row["p_cpu_ns"].as_u64().unwrap_or(old.performance),
                qos: std::array::from_fn(|i| {
                    row["qos_cpu_ns"]["value"]
                        .as_array()
                        .and_then(|a| a.get(i))
                        .and_then(aterm_json::Value::as_u64)
                        .unwrap_or(0)
                }),
                runnable_missing: old.runnable_missing || row["runnable_ns"].as_u64().is_none(),
            };
            let delta = now.cpu.saturating_sub(old.cpu) as f64 / 1e9;
            if matches!(
                row["name"].as_str(),
                Some("rustc" | "trustc" | "tippy-driver" | "clippy-driver")
            ) {
                live_compilers += 1;
                if last > 0.0 && delta > interval * 0.1 {
                    progressing_compilers += 1;
                }
            }
            let sum = totals.entry(role.to_owned()).or_default();
            sum.cpu += now.cpu.saturating_sub(old.cpu);
            sum.runnable += now.runnable.saturating_sub(old.runnable);
            sum.performance += now.performance.saturating_sub(old.performance);
            sum.runnable_missing |= now.runnable_missing;
            for i in 0..7 {
                sum.qos[i] += now.qos[i].saturating_sub(old.qos[i]);
            }
            if let Some(n) = row["nice"]["value"].as_i64() {
                nice.entry(role.to_owned()).or_default().insert(n);
            }
            busy += delta;
            *bins
                .entry(((at / 600.0) as u64, role.to_owned()))
                .or_default() += delta;
            *old = now;
        }
        if last > 0.0 && interval > 0.0 {
            max_cores = max_cores.max(busy / interval);
        }
        max_live_compilers = max_live_compilers.max(live_compilers);
        max_progressing_compilers = max_progressing_compilers.max(progressing_compilers);
        if result["slots"]
            .as_u64()
            .is_some_and(|slots| live_compilers as u64 > slots)
        {
            over_width_s += interval.max(0.0);
        }
        last = at;
    }
    let roles: Vec<_> = totals.into_iter().map(|(role,c)| aterm_json::json!({"role":role,"cpu_s":c.cpu as f64/1e9,"runnable_s":(!c.runnable_missing).then_some(c.runnable as f64/1e9),"waiting_for_cpu_s":(!c.runnable_missing).then_some(c.runnable.saturating_sub(c.cpu) as f64/1e9),"performance_cpu_s":c.performance as f64/1e9,"qos_cpu_s":c.qos.map(|n| n as f64/1e9),"nice_values":nice.get(&role).map(|s|s.iter().copied().collect::<Vec<_>>())})).collect();
    let bins: Vec<_> = bins.into_iter().map(|((bin,role),cpu)| aterm_json::json!({"window_start_s":bin*600,"role":role,"cpu_s":cpu,"mean_cores":cpu/(last-(bin*600) as f64).clamp(1.0,600.0)})).collect();
    let mut units = Vec::new();
    let mut artifacts = Vec::new();
    for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        let counted = crate::buildplan::units_of(
            crate::durable::open_regular_file(&root.join(format!("{triple}.messages.jsonl")))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        artifacts.push(aterm_json::json!({"triple":triple,"compiled":counted.compiled,"fresh":counted.fresh,"unknown":counted.unknown}));
        let file =
            crate::durable::open_regular_file(&root.join(format!("{triple}.messages.jsonl")))
                .map_err(|e| e.to_string())?;
        let mut starts = BTreeMap::<(u64, u64), aterm_json::Value>::new();
        for line in std::io::BufReader::new(file).lines() {
            let value: aterm_json::Value = aterm_json::from_str(&line.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let message = &value["message"];
            if message["event"].as_str() != Some("release-slice-unit") {
                continue;
            }
            let key = (
                field(message, "pid"),
                message["queued_unix_s"]
                    .as_f64()
                    .ok_or("unit has no queued time")?
                    .to_bits(),
            );
            match message["phase"].as_str() {
                Some("started") => {
                    starts.insert(key, message.clone());
                }
                Some("ended") => {
                    let start = starts.remove(&key).ok_or("ended unit lacks its start")?;
                    let begin = start["at_unix_s"].as_f64().ok_or("unit lacks start time")?;
                    let end = message["at_unix_s"].as_f64().ok_or("unit lacks end time")?;
                    let queued = start["queued_unix_s"]
                        .as_f64()
                        .ok_or("unit lacks queue time")?;
                    let counters = compiler_counters(field(&start, "compiler_pid"), &previous);
                    units.push(aterm_json::json!({"triple":triple,"package":message["package"],"unit":message["unit"],"started_unix_s":begin,"ended_unix_s":end,"pre_spawn_s":begin-queued,"memory_waiting_s":start["memory_waiting_s"],"wall_s":end-begin,"cpu_s":message["cpu_s"],"compiler_pid":start["compiler_pid"],"sampled_compiler_cpu_s":counters.map(|c|c.cpu as f64/1e9),"sampled_compiler_cpu_wait_s":counters.filter(|c|!c.runnable_missing).map(|c|c.runnable.saturating_sub(c.cpu) as f64/1e9),"success":message["success"]}));
                }
                _ => return Err("unknown unit timeline phase".into()),
            }
        }
        if !starts.is_empty() {
            return Err(format!("{triple} has {} unfinished units", starts.len()));
        }
    }
    let critical: Vec<_> = units
        .iter()
        .filter(|u| {
            matches!(
                u["package"].as_str(),
                Some(
                    "aterm-mesh-core"
                        | "aterm-link"
                        | "aterm-mesh"
                        | "aterm-agent"
                        | "aterm-gui"
                        | "aterm"
                )
            ) && u["unit"].as_str() != Some("build_script_build bin")
        })
        .cloned()
        .collect();
    let summary = aterm_json::json!({"result":result,"sample_started_unix_s":started_unix_s,"artifacts":artifacts,"sampled_roles":roles,"bins":bins,"critical_units":critical,"sampled_max_cores":max_cores,"max_live_compilers":max_live_compilers,"max_progressing_compilers":max_progressing_compilers,"compiler_count_over_slots_s":over_width_s,"max_sample_interval_s":max_interval,"max_load1":max_load,"unavailable_samples":unavailable,"truncated_samples":truncated_samples,"unread_process_rows":unread_process_rows,"completed_units":units.len()});
    crate::durable::write_fresh(&root.join("summary.json"), summary.to_string())
        .map_err(|e| e.to_string())?;
    println!("qualify-summary: {summary}");
    Ok(())
}

fn observation<T: serde::Serialize>(read: Result<T, i32>) -> aterm_json::Value {
    match read {
        Ok(value) => aterm_json::json!({"value":value,"errno":null}),
        Err(errno) => aterm_json::json!({"value":null,"errno":errno}),
    }
}

#[derive(Clone, Copy)]
enum TreeWork {
    CompileTests,
    Lint,
    Qualify,
}

impl TreeWork {
    fn label(self) -> &'static str {
        match self {
            Self::CompileTests => "test",
            Self::Lint => "lint",
            Self::Qualify => "qualify",
        }
    }
    fn arguments(self) -> Vec<&'static str> {
        match self {
            Self::CompileTests => vec!["test", "--all", "--as-release", "--compile-only"],
            Self::Lint => vec!["release", "--tree", "--checks", "lint"],
            Self::Qualify => vec!["release", "--tree"],
        }
    }
}

fn tree(
    repo: &Path,
    root: &Path,
    opts: &Options,
    pool: &aterm_capacity::Jobserver,
) -> Result<Vec<aterm_json::Value>, String> {
    if opts.mode == "alone" {
        return Ok(Vec::new());
    }
    std::thread::scope(|scope| {
        let work = if matches!(opts.mode.as_str(), "qualify-before" | "qualify-after") {
            vec![TreeWork::Qualify]
        } else {
            vec![TreeWork::CompileTests, TreeWork::Lint]
        };
        let handles: Vec<_> = work.into_iter().map(|work| scope.spawn(move || {
            let check = work.label();
            let (program, prefix) = invocation(&opts.verify, opts.mode == "chain");
            let mut cmd = aterm_uds::spawnfd::command(program);
            cmd.args(prefix).args(work.arguments());
            cmd.current_dir(repo)
                .env(aterm_capacity::RELEASE_RUN_ENV, "1")
                .env("CARGO_TARGET_DIR", root.join(format!("tree-{check}.noindex")))
                .arg("--log").arg(root.join(format!("tree-{check}.log")))
                .arg("--events").arg(root.join(format!("tree-{check}.events")));
            pool.hand_to(&mut cmd);
            let output = crate::durable::create_fresh(&root.join(format!("tree-{check}.stdout"))).map_err(|e| e.to_string())?;
            cmd.stdout(output.try_clone().map_err(|e| e.to_string())?).stderr(output);
            let started = Instant::now();
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let usage = child.wait_with_rusage().map_err(|e| e.to_string())?;
            let row = aterm_json::json!({"check":check,"wall_s":started.elapsed().as_secs_f64(),"cpu_s":usage.cpu.as_secs_f64(),"success":usage.status.success()});
            crate::durable::write_fresh(&root.join(format!("tree-{check}.result.json")), row.to_string())
                .map_err(|e| e.to_string())?;
            if !usage.status.success() { return Err(format!("pilot tree {check}: {row}")); }
            Ok(row)
        })).collect();
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "pilot tree panicked".to_owned())?)
            .collect()
    })
}

pub(crate) fn run(repo: &Path, opts: &Options) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("qualify-pilot requires macOS release slices".into());
    }
    let git = crate::ledger::GitCli::new(repo);
    let release = crate::release::release_dir(&git)?;
    let pid = std::process::id();
    let start =
        crate::release_steps::started_of(pid).ok_or("pilot process identity unavailable")?;
    let _line = crate::release::RunClaim::take(
        &release.join(crate::release::RUNNING_FILE),
        (pid, &start),
        &release,
        &mut crate::release_steps::started_of,
    )
    .map_err(|e| format!("qualify-pilot cannot run beside a cut: {e:?}"))?;
    let commit = crate::snapshot::git_text(repo, &["rev-parse", "HEAD"])?;
    let (_release, take) = under_reservation(
        || {
            aterm_capacity::admit(reservation(), aterm_capacity::Admit::Wait)
                .map_err(|e| e.to_string())
        },
        || crate::buildplan::Take::prepare(repo, commit.trim()),
    )?;
    let cache = crate::snapshot::CacheRoot::for_repo(repo)?;
    let root = cache.path().join(".ubd/qualify-pilot").join(format!(
        "{}-{}-{pid}.noindex",
        commit.trim(),
        opts.mode
    ));
    std::fs::create_dir_all(root.parent().ok_or("pilot root has no parent")?)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir(&root)
        .map_err(|e| format!("create unique pilot (existing evidence retained): {e}"))?;
    let claim = aterm_capacity::admit(
        aterm_capacity::Need::new("qualify-pilot core pool")
            .rank(aterm_capacity::Rank::Release)
            .cores(aterm_capacity::cores())
            .disk(200 << 30, &root)
            .scratch(),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    let records = aterm_capacity::Ledger::user()
        .ok_or("no capacity ledger")?
        .list()
        .map_err(|e| e.to_string())?;
    let admission = aterm_json::json!({
        "requested":claim.record().need.cores,
        "rank":format!("{:?}",claim.record().need.rank),
        "granted":claim.cores(),
        "machine_cores":aterm_capacity::Reading::now(&root).cores,
        "records":records.iter().map(|r|aterm_json::json!({"pid":r.pid,"what":r.need.what,"requested":r.need.cores,"granted":r.held.map(|h|h.cores),"rank":format!("{:?}",r.need.rank)})).collect::<Vec<_>>()
    });
    crate::durable::write_fresh(&root.join("admission.json"), admission.to_string())
        .map_err(|e| e.to_string())?;
    eprintln!("qualify-pilot-admission: {admission}");
    require_series_grant(&opts.mode, aterm_capacity::cores(), claim.cores())?;
    let pool = aterm_capacity::Jobserver::new(&root.join("jobs"), claim.cores())
        .map_err(|e| e.to_string())?;
    let modes = order(&opts.mode);
    for (index, mode) in modes.iter().enumerate() {
        let at = root.join(format!("take{}-{mode}", index + 1));
        std::fs::create_dir(&at).map_err(|e| e.to_string())?;
        let verifier = if opts.mode == "series" && matches!(*mode, "shared" | "tree") {
            opts.before
                .as_ref()
                .ok_or("series requires --before-verify PATH for its controls")?
        } else {
            &opts.verify
        };
        let options = Options {
            verify: verifier.clone(),
            before: None,
            mode: (*mode).to_owned(),
        };
        one(repo, &take, &at, &options, &pool, commit.trim())?;
        take.verify()?;
    }
    Ok(())
}

fn reservation() -> aterm_capacity::Need {
    aterm_capacity::Need::new("qualify-pilot release reservation")
        .rank(aterm_capacity::Rank::Release)
}

/// Match the cutter's lifecycle: Release is visible before snapshotting, and its
/// guard remains held throughout the later pool admission and every measured take.
fn under_reservation<R, T>(
    reserve: impl FnOnce() -> Result<R, String>,
    prepare: impl FnOnce() -> Result<T, String>,
) -> Result<(R, T), String> {
    let reservation = reserve()?;
    Ok((reservation, prepare()?))
}

fn order(mode: &str) -> Vec<&str> {
    if mode == "qualify-series" {
        vec![
            "qualify-before",
            "qualify-after",
            "qualify-after",
            "qualify-before",
        ]
    } else if mode == "series" {
        vec!["alone", "shared", "chain", "chain", "shared", "tree"]
    } else {
        vec![mode]
    }
}

fn require_series_grant(mode: &str, requested: u32, granted: u32) -> Result<(), String> {
    // The control's shared FIFO has two Cargo clients for slices and one for the
    // tree. With fewer implicit tokens a memory waiter could hold the last CPU
    // token needed by the branch that can release its memory.
    if mode.starts_with("qualify-") && granted < 3 {
        return Err(format!(
            "qualify comparison needs at least 3 slots for its three Cargo clients, got {granted}; no take started"
        ));
    }
    if mode == "series" && granted != requested {
        Err(format!(
            "series needs the full {requested}-core grant, got {granted}; no take started"
        ))
    } else {
        Ok(())
    }
}

fn one(
    repo: &Path,
    take: &crate::buildplan::Take,
    root: &Path,
    opts: &Options,
    pool: &aterm_capacity::Jobserver,
    commit: &str,
) -> Result<(), String> {
    let pid = std::process::id();
    println!(
        "qualify-pilot: {} S={} mode={}",
        root.display(),
        pool.slots(),
        opts.mode
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut samples =
        crate::durable::create_fresh(&root.join("processes.jsonl")).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let (slices, tree) = std::thread::scope(|scope| -> Result<_, String> {
        let (done, receiver) = std::sync::mpsc::channel::<()>();
        let sampler = scope.spawn(move || -> Result<(), String> {
            let mut global = BTreeMap::new();
            loop {
                writeln!(
                    samples,
                    "{}",
                    sample(pid, started.elapsed().as_secs_f64(), &mut global)
                )
                .map_err(|e| e.to_string())?;
                if receiver.recv_timeout(Duration::from_secs(1))
                    != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
            Ok(())
        });
        let pair = if matches!(opts.mode.as_str(), "qualify-before" | "qualify-after") {
            qualify_pair(repo, take, root, opts, pool)
        } else {
            let tree = scope.spawn(|| tree(repo, root, opts, pool));
            let slices = crate::buildplan::contention_slices(take, root, pool, opts.mode == "tree");
            let tree = tree.join().map_err(|_| "pilot tree panicked".to_owned());
            slices.and_then(|slices| tree.and_then(|tree| tree.map(|tree| (slices, tree))))
        };
        drop(done);
        sampler.join().map_err(|_| "pilot sampler panicked")??;
        pair
    })?;
    let (verify_sha256, _) = crate::dmg::sha256_and_size(&opts.verify)?;
    let report = aterm_json::json!({"mode":opts.mode,"source":commit,"slots":pool.slots(),"wall_s":started.elapsed().as_secs_f64(),"slices":slices,"tree":tree,"verify_sha256":verify_sha256,"verify":opts.verify});
    crate::durable::write_fresh(&root.join("result.json"), report.to_string())
        .map_err(|e| e.to_string())?;
    println!("qualify-pilot-result: {report}");
    Ok(())
}

/// The same cold slices and complete release tree in both arms. The control uses
/// the former slice-admission edge; the treatment uses the production lint gate.
fn qualify_pair(
    repo: &Path,
    take: &crate::buildplan::Take,
    root: &Path,
    opts: &Options,
    pool: &aterm_capacity::Jobserver,
) -> Result<(Vec<aterm_json::Value>, Vec<aterm_json::Value>), String> {
    use crate::release_steps::TreeIndexHandoff;
    use std::sync::atomic::{AtomicBool, Ordering};
    let ended = AtomicBool::new(false);
    let handoff = TreeIndexHandoff::default();
    if opts.mode == "qualify-after" {
        handoff.expect_lint(root.join("tree-qualify.events"));
    }
    std::thread::scope(|scope| {
        if opts.mode == "qualify-before" {
            let slices = scope.spawn(|| {
                let _completed = SliceCompleted(&ended);
                crate::buildplan::contention_slices(take, root, pool, false)
            });
            while !ended.load(Ordering::SeqCst)
                && !crate::release_steps::slices_hold_memory(std::process::id())
                && crate::interrupt::caught().is_none()
            {
                std::thread::sleep(Duration::from_secs(1));
            }
            let tree = tree(repo, root, opts, pool);
            let slices = slices.join().map_err(|_| "pilot slices panicked")?;
            Ok((slices?, tree?))
        } else {
            let tree = scope.spawn(|| {
                let mut completed = TreeCompleted {
                    handoff: &handoff,
                    passed: false,
                };
                let result = tree(repo, root, opts, pool);
                completed.passed = result.is_ok();
                result
            });
            let admitted = handoff.await_lint(
                |path| {
                    crate::check_record::events(path).is_ok_and(|e| e.whole() && e.lint_admitted)
                },
                || crate::interrupt::caught().is_some(),
                || false,
                || std::thread::sleep(Duration::from_secs(1)),
            );
            let slices = admitted
                .and_then(|()| crate::buildplan::contention_slices(take, root, pool, false));
            let tree = tree.join().map_err(|_| "pilot tree panicked")?;
            Ok((slices?, tree?))
        }
    })
}

struct SliceCompleted<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for SliceCompleted<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct TreeCompleted<'a> {
    handoff: &'a crate::release_steps::TreeIndexHandoff,
    passed: bool,
}

impl Drop for TreeCompleted<'_> {
    fn drop(&mut self) {
        use crate::release_steps::TreeIndexCompletion;
        self.handoff.finish(if self.passed {
            TreeIndexCompletion::Compile
        } else {
            TreeIndexCompletion::Stopped
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn series_refuses_a_partial_grant_before_any_take_but_single_takes_can_measure_it() {
        assert_eq!(reservation().rank, aterm_capacity::Rank::Release);
        assert_eq!(reservation().cores, 0);
        assert_eq!(reservation().mem, 0);
        assert!(require_series_grant("series", 18, 18).is_ok());
        assert!(require_series_grant("series", 18, 4).is_err());
        assert!(require_series_grant("alone", 18, 4).is_ok());
        for slots in [1, 2] {
            assert!(require_series_grant("qualify-series", 18, slots).is_err());
        }
        assert!(require_series_grant("qualify-series", 18, 3).is_ok());
    }

    #[test]
    fn pilot_completion_is_published_on_unwind() {
        let ended = std::sync::atomic::AtomicBool::new(false);
        assert!(
            std::panic::catch_unwind(|| {
                let _completed = SliceCompleted(&ended);
                panic!("controlled slice panic");
            })
            .is_err()
        );
        assert!(ended.load(std::sync::atomic::Ordering::SeqCst));
        let dir = aterm_tempfile::tempdir().unwrap();
        let handoff = crate::release_steps::TreeIndexHandoff::default();
        handoff.expect_lint(dir.path().join("events"));
        assert!(
            std::panic::catch_unwind(|| {
                let _completed = TreeCompleted {
                    handoff: &handoff,
                    passed: false,
                };
                panic!("controlled tree panic");
            })
            .is_err()
        );
        assert!(
            handoff
                .await_lint(
                    |_| false,
                    || false,
                    || false,
                    || panic!("a panicked tree must release the pilot waiter")
                )
                .is_err()
        );
    }
    #[test]
    fn snapshotting_starts_under_a_live_reservation_and_failure_releases_it() {
        struct Guard<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let live = std::cell::Cell::new(false);
        let reserve = || {
            live.set(true);
            Ok(Guard(&live))
        };
        let (guard, ()) = under_reservation(reserve, || {
            assert!(live.get(), "snapshot preparation must already see Release");
            Ok(())
        })
        .unwrap();
        assert!(live.get());
        drop(guard);
        assert!(!live.get());
        // Negative control: a failed snapshot must not strand a Release reservation.
        let failed = under_reservation(reserve, || Err::<(), _>("snapshot failed".into()));
        assert!(failed.is_err());
        assert!(!live.get());
    }
    #[test]
    fn contention_workload_matches_release_checks_without_pulled_cells_or_wasm() {
        use aterm_verify::checks::Check;
        for (work, expected) in [
            (TreeWork::CompileTests, Check::Test),
            (TreeWork::Lint, Check::Lint),
        ] {
            let parsed = aterm_verify::cli::parse(work.arguments()).unwrap();
            assert_eq!(parsed.wanted(|_| true), vec![expected]);
            assert_eq!(parsed.compile_only, expected == Check::Test);
            assert!(!aterm_verify::checks::pulls_cells(
                &parsed.wanted(|_| true),
                parsed.release,
                true
            ));
        }
        // Negative controls: standalone invocations add work no release tree runs.
        let old_test = aterm_verify::cli::parse(["test", "--all", "--compile-only"]).unwrap();
        assert!(old_test.wanted(|_| true).contains(&Check::WasmSmoke));
        let old_lint = aterm_verify::cli::parse(["lint", "--all"]).unwrap();
        assert!(aterm_verify::checks::pulls_cells(
            &old_lint.wanted(|_| true),
            old_lint.release,
            true
        ));
    }
    #[test]
    fn a_tree_cargo_stays_tree_work_and_an_unrelated_cargo_does_not() {
        let tree = aterm_json::json!({"pid":2,"ppid":1,"name":"aterm-verify"});
        let cargo = aterm_json::json!({"pid":3,"ppid":2,"name":"targo"});
        let compiler = aterm_json::json!({"pid":4,"ppid":3,"name":"trustc"});
        let other = aterm_json::json!({"pid":5,"ppid":1,"name":"cargo"});
        let rows = BTreeMap::from([(2, &tree), (3, &cargo), (4, &compiler), (5, &other)]);
        assert_eq!(branch(&compiler, &rows, "aterm-verify"), "tree");
        assert_eq!(branch(&cargo, &rows, "aterm-verify"), "tree");
        assert_eq!(branch(&other, &rows, "aterm-verify"), "compat");
        let renamed = aterm_json::json!({"pid":2,"ppid":1,"name":"verify-before"});
        let renamed_rows = BTreeMap::from([(2, &renamed), (3, &cargo), (4, &compiler)]);
        assert_eq!(branch(&compiler, &renamed_rows, "verify-before"), "tree");
        assert_eq!(branch(&compiler, &renamed_rows, "aterm-verify"), "native");
        let missing: Result<u64, i32> = Err(3);
        assert!(observation(missing)["value"].is_null());
        assert_eq!(observation(Ok(0_u64))["value"].as_u64(), Some(0));
    }
    #[test]
    fn cargo_nested_unit_events_survive_summary_and_a_partial_chain_is_refused() {
        let dir = aterm_tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("result.json"), "{\"slots\":1}").unwrap();
        let sample = aterm_json::json!({"elapsed_s":1.0,"members":[
            {"pid":4,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64},
            {"pid":5,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64}
        ]});
        std::fs::write(dir.path().join("processes.jsonl"), format!("{sample}\n")).unwrap();
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let start = aterm_json::json!({"reason":"compiler-message","message":{"event":"release-slice-unit","phase":"started","pid":3,"compiler_pid":4,"queued_unix_s":10.0,"at_unix_s":11.0,"package":"aterm","unit":"aterm bin"}});
            let end = aterm_json::json!({"reason":"compiler-message","message":{"event":"release-slice-unit","phase":"ended","pid":3,"queued_unix_s":10.0,"at_unix_s":15.0,"cpu_s":2.0,"package":"aterm","unit":"aterm bin","success":true}});
            std::fs::write(
                dir.path().join(format!("{triple}.messages.jsonl")),
                format!("{start}\n{end}\n{{\"reason\":\"compiler-artifact\",\"fresh\":false,\"filenames\":[]}}\n{{\"reason\":\"compiler-artifact\",\"fresh\":true,\"filenames\":[]}}\n"),
            )
            .unwrap();
        }
        summarize(dir.path()).unwrap();
        let summary: aterm_json::Value = aterm_json::from_str(
            &std::fs::read_to_string(dir.path().join("summary.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(summary["completed_units"].as_u64(), Some(2));
        assert_eq!(summary["artifacts"][0]["compiled"].as_u64(), Some(1));
        assert_eq!(summary["artifacts"][0]["fresh"].as_u64(), Some(1));
        assert_eq!(summary["compiler_count_over_slots_s"].as_f64(), Some(1.0));
        let units = summary["critical_units"].as_array().unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0]["wall_s"].as_f64(), Some(4.0));
        assert_eq!(units[0]["cpu_s"].as_f64(), Some(2.0));
        assert_eq!(units[0]["pre_spawn_s"].as_f64(), Some(1.0));
        assert_eq!(units[0]["sampled_compiler_cpu_s"].as_f64(), Some(1.0));
        assert_eq!(units[0]["sampled_compiler_cpu_wait_s"].as_f64(), Some(0.2));
        // Negative control: no complete result may be inferred from a start without its end.
        let start = aterm_json::json!({"message":{"event":"release-slice-unit","phase":"started","pid":9,"queued_unix_s":20.0,"at_unix_s":21.0}});
        std::fs::write(
            dir.path().join("aarch64-apple-darwin.messages.jsonl"),
            format!("{start}\n"),
        )
        .unwrap();
        assert!(summarize(dir.path()).is_err());
    }
    #[test]
    fn a_missing_or_reused_compiler_pid_has_no_invented_lifetime_counter() {
        let mut counters = BTreeMap::new();
        assert!(compiler_counters(4, &counters).is_none());
        counters.insert((4, 1), Counters::default());
        assert!(compiler_counters(4, &counters).is_some());
        counters.insert((4, 2), Counters::default());
        assert!(compiler_counters(4, &counters).is_none());
    }
    #[test]
    fn descendant_sampling_follows_grandchildren_out_of_order_and_excludes_other_sessions() {
        use aterm_sysprobe::procs::ProcessReading;
        let row = |pid, ppid| ProcessReading {
            pid,
            ppid,
            ..ProcessReading::default()
        };
        let rows = [row(3, 2), row(90, 80), row(2, 1), row(4, 3), row(80, 0)];
        assert_eq!(descendants(&rows, 1), BTreeSet::from([1, 2, 3, 4]));
        assert_eq!(descendants(&rows, 80), BTreeSet::from([80, 90]));
    }
    #[test]
    fn experimental_modes_require_an_explicit_verifier_and_reject_unknown_priorities() {
        let parse_words = |s: &str| parse(s.split_whitespace().map(str::to_owned));
        for mode in [
            "alone",
            "shared",
            "chain",
            "tree",
            "series",
            "qualify-before",
            "qualify-after",
            "qualify-series",
        ] {
            let before = if mode == "series" {
                " --before-verify /old"
            } else {
                ""
            };
            assert_eq!(
                parse_words(&format!("--verify /v --mode {mode}{before}"))
                    .unwrap()
                    .mode,
                mode
            );
        }
        assert!(parse_words("--mode chain").is_err());
        assert!(parse_words("--verify /v --mode fastest").is_err());
        assert!(parse_words("--verify /v --mode chain --mode tree").is_err());
        assert!(parse_words("--verify /v --mode series").is_err());
        assert!(parse_words("--verify /v --mode chain --before-verify /old").is_err());
        assert_eq!(
            order("series"),
            ["alone", "shared", "chain", "chain", "shared", "tree"]
        );
        assert_eq!(order("shared"), ["shared"]);
        assert_eq!(
            order("qualify-series"),
            [
                "qualify-before",
                "qualify-after",
                "qualify-after",
                "qualify-before"
            ]
        );
        let parsed = aterm_verify::cli::parse(TreeWork::Qualify.arguments()).unwrap();
        let actual: BTreeSet<_> = parsed
            .wanted(|_| true)
            .into_iter()
            .map(|c| c.name())
            .collect();
        let expected: BTreeSet<_> = crate::check_record::Half::Tree
            .checks()
            .iter()
            .map(|c| c.name())
            .collect();
        assert_eq!(actual, expected);
        assert!(!parsed.compile_only);
    }
    #[test]
    fn only_lower_priority_invocations_gain_nice_and_keep_the_original_program() {
        let program = Path::new("/compiler path/tool");
        assert_eq!(invocation(program, false), (program.to_owned(), Vec::new()));
        if cfg!(unix) {
            assert_eq!(
                invocation(program, true),
                (
                    PathBuf::from("/usr/bin/nice"),
                    vec![
                        OsString::from("-n"),
                        "10".into(),
                        program.as_os_str().to_owned()
                    ]
                )
            );
        }
    }
}
