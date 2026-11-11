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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Options {
    verify: PathBuf,
    before: Option<PathBuf>,
    mode: String,
    tree_work: TreeShape,
    export_seconds: Option<u64>,
    real_export: bool,
}

/// The measured 0.187.0 export wall. The release-shaped pilot holds the production export
/// reservation for this long without contacting the publication engine.
const RELEASE_EXPORT_SECONDS: u64 = 763;
const SLICE_CHILD_FLAG: &str = "--qualify-pilot-slice-child";
const SLICE_CHILD_EVENT: &str = "qualify-pilot-slice-event: ";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TreeShape {
    #[default]
    CompileLint,
    Release,
}

/// Hidden sibling process used by the pilot. Production's build and tree are sibling process
/// trees; keeping the slice parent here prevents an ancestor claim from covering the tree.
pub(crate) fn slice_child_entry(args: &[OsString]) -> Option<std::process::ExitCode> {
    if args.first().and_then(|arg| arg.to_str()) != Some(SLICE_CHILD_FLAG) {
        return None;
    }
    let result = (|| {
        let [_, repo, root, commit, demoted] = args else {
            return Err("qualify pilot slice child needs repo, root, commit and priority".into());
        };
        let commit = commit
            .to_str()
            .ok_or("qualify pilot slice child commit is not text")?;
        let demoted = match demoted.to_str() {
            Some("normal") => false,
            Some("demoted") => true,
            _ => return Err("qualify pilot slice child priority is invalid".into()),
        };
        slice_child(Path::new(repo), Path::new(root), commit, demoted)
    })();
    Some(match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = emit_slice_child(aterm_json::json!({"event":"failed","error":error}));
            eprintln!("aterm-release: qualify pilot slice child: {error}");
            std::process::ExitCode::from(2)
        }
    })
}

fn emit_slice_child(event: aterm_json::Value) -> Result<(), String> {
    println!("{SLICE_CHILD_EVENT}{event}");
    std::io::stdout().flush().map_err(|error| error.to_string())
}

fn slice_child(repo: &Path, root: &Path, commit: &str, demoted: bool) -> Result<(), String> {
    let take = crate::buildplan::Take::prepare(repo, commit)?;
    emit_slice_child(aterm_json::json!({"event":"prepared"}))?;
    let pool = aterm_capacity::Jobserver::inherited()
        .ok_or("qualify pilot slice child was not handed its jobserver")?
        .map_err(|error| format!("adopt qualify pilot jobserver: {error}"))?;
    let (ready, admissions) = std::sync::mpsc::channel();
    let slices = std::thread::scope(|scope| -> Result<_, String> {
        let reporter = scope.spawn(move || -> Result<(), String> {
            for event in admissions {
                emit_slice_child(event)?;
            }
            Ok(())
        });
        let slices = crate::buildplan::contention_slices(&take, root, &pool, demoted, Some(ready));
        reporter
            .join()
            .map_err(|_| "slice admission reporter panicked".to_owned())??;
        slices
    })?;
    take.verify()?;
    emit_slice_child(aterm_json::json!({"event":"ended","slices":slices}))
}

pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let mut verify = None;
    let mut mode = None;
    let mut before = None;
    let mut tree_work = None;
    let mut export_seconds = None;
    let mut real_export = false;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--export-work" if value == "real" && !real_export => real_export = true,
            "--verify" if verify.is_none() => verify = Some(PathBuf::from(value)),
            "--before-verify" if before.is_none() => before = Some(PathBuf::from(value)),
            "--tree-work"
                if tree_work.is_none() && matches!(value.as_str(), "compile-lint" | "release") =>
            {
                tree_work = Some(if value == "release" {
                    TreeShape::Release
                } else {
                    TreeShape::CompileLint
                });
            }
            "--export-seconds" if export_seconds.is_none() => {
                export_seconds = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("--export-seconds needs seconds, not {value:?}"))?,
                );
            }
            "--mode"
                if mode.is_none()
                    && matches!(
                        value.as_str(),
                        "alone" | "shared" | "chain" | "tree" | "series" | "export" | "matched"
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
        mode: mode.ok_or("qualify-pilot requires --mode alone|shared|chain|tree|series")?,
        tree_work: tree_work.unwrap_or_default(),
        export_seconds,
        real_export,
    };
    if matches!(options.mode.as_str(), "series" | "matched") != options.before.is_some() {
        return Err("series and matched require --before-verify PATH".into());
    }
    if options.real_export && options.export_seconds.is_some() {
        return Err("real export cannot use a surrogate duration".into());
    }
    Ok(options)
}

impl Options {
    fn export_seconds(&self) -> Option<u64> {
        if self.real_export {
            return Some(0);
        }
        self.export_seconds
            .or(
                (self.tree_work == TreeShape::Release && self.mode != "alone")
                    .then_some(RELEASE_EXPORT_SECONDS),
            )
            .filter(|seconds| *seconds > 0)
    }
}

/// Unix nice is a relative CPU priority, not a background task band or a core cap: lower
/// priority work can use all idle cores. It changes neither private FIFO allocation nor memory
/// claims.
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

fn ledger_snapshot(ledger: Option<aterm_capacity::Ledger>) -> aterm_json::Value {
    match ledger {
        Some(ledger) => match ledger.list() {
            Ok(records) => aterm_json::json!({
                "error":null,
                "records":records.into_iter().map(|record| aterm_json::json!({
                    "pid":record.pid,
                    "start_ms":record.start_ms,
                    "seq":record.seq,
                    "what":record.need.what,
                    "rank":format!("{:?}",record.need.rank),
                    "mem":record.need.mem,
                    "mem_now":record.need.mem_now(),
                    "held":record.held.is_some(),
                    "admitted_ms":record.held.map(|held|held.admitted_ms),
                })).collect::<Vec<_>>()
            }),
            Err(error) => aterm_json::json!({"error":error.to_string(),"records":null}),
        },
        None => aterm_json::json!({"error":"no capacity ledger","records":null}),
    }
}

fn fifo_snapshot(path: &Path) -> aterm_json::Value {
    #[cfg(unix)]
    fn read(path: &Path) -> std::io::Result<u32> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.file_type().is_fifo() {
            return Err(std::io::Error::other("not a jobserver FIFO"));
        }
        let mut count: libc::c_int = 0;
        // SAFETY: file owns a live FIFO descriptor; FIONREAD writes exactly one c_int
        // into count and consumes no byte, so observing cannot change either share.
        if unsafe { libc::ioctl(file.as_raw_fd(), libc::FIONREAD, &raw mut count) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        u32::try_from(count).map_err(|_| std::io::Error::other("negative FIFO byte count"))
    }
    #[cfg(not(unix))]
    fn read(_path: &Path) -> std::io::Result<u32> {
        Err(std::io::Error::other("FIFO observation requires Unix"))
    }
    match read(path) {
        Ok(count) => aterm_json::json!({"available_tokens":count,"error":null}),
        Err(error) => aterm_json::json!({"available_tokens":null,"error":error.to_string()}),
    }
}

fn sample(
    root: u32,
    path: &Path,
    elapsed: f64,
    global: &mut BTreeMap<(u32, u64), u64>,
) -> aterm_json::Value {
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
    let waits = sweep.as_ref().map(|sweep| {
        let ids = descendants(&sweep.procs, root);
        aterm_toolchain::waits::user_waiting()
            .into_iter()
            .filter(|wait| ids.contains(&wait.pid))
            .map(|wait| {
                aterm_json::json!({
                    "pid":wait.pid,"since_unix_s":wait.since,"reason":wait.text,
                })
            })
            .collect::<Vec<_>>()
    });
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let machine = aterm_capacity::Reading::now(path);
    let capacity = aterm_json::json!({
        "machine":{
            "cores":machine.cores,
            "load1":machine.load1,
            "available":machine.available,
            "physical":machine.physical,
            "wired":machine.wired,
            "swap_used":machine.swap_used,
            "pressure":machine.pressure.map(aterm_sysprobe::MemoryLevel::as_str),
        },
        "global":ledger_snapshot(aterm_capacity::Ledger::user()),
        "slice_budget":ledger_snapshot(Some(aterm_capacity::Ledger::at(path.join("slice-memory-budget")))),
        "cpu_tokens":{
            "build":fifo_snapshot(&path.join("qualify.jobs")),
            "tree":fifo_snapshot(&path.join("qualify.tree.jobs")),
        },
    });
    aterm_json::json!({"at_unix_s":epoch,"elapsed_s":elapsed+reading_started.elapsed().as_secs_f64(), "global_cpu_delta_ns":(!initial && sweep.is_some()).then_some(global_delta), "load":aterm_sysprobe::load_average(), "truncated":sweep.as_ref().map(|s| s.truncated), "members":rows,"capacity":capacity,"wait_marks":waits})
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
    let mut global_capacity_samples = 0_u64;
    let mut global_capacity_unavailable = 0_u64;
    let mut private_capacity_samples = 0_u64;
    let mut private_capacity_unavailable = 0_u64;
    let mut last_capacity = None::<(aterm_json::Value, aterm_json::Value)>;
    let mut capacity_timeline = Vec::new();
    let mut peak_host_used_bytes = None::<u64>;
    let mut peak_descendant_footprint_bytes = 0_u64;
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
        if value["capacity"]["global"]["records"].as_array().is_some() {
            global_capacity_samples += 1;
        } else {
            global_capacity_unavailable += 1;
        }
        if value["capacity"]["slice_budget"]["records"]
            .as_array()
            .is_some()
        {
            private_capacity_samples += 1;
        } else {
            private_capacity_unavailable += 1;
        }
        let capacity_pair = (
            value["capacity"]["global"].clone(),
            value["capacity"]["slice_budget"].clone(),
        );
        if last_capacity.as_ref() != Some(&capacity_pair) {
            capacity_timeline.push(aterm_json::json!({
                "elapsed_s":at,
                "at_unix_s":value["at_unix_s"].clone(),
                "global":capacity_pair.0.clone(),
                "slice_budget":capacity_pair.1.clone(),
            }));
            last_capacity = Some(capacity_pair);
        }
        if let (Some(physical), Some(available)) = (
            value["capacity"]["machine"]["physical"].as_u64(),
            value["capacity"]["machine"]["available"].as_u64(),
        ) {
            let used = physical.saturating_sub(available);
            peak_host_used_bytes = Some(peak_host_used_bytes.map_or(used, |peak| peak.max(used)));
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
        peak_descendant_footprint_bytes = peak_descendant_footprint_bytes.max(
            members
                .iter()
                .filter_map(|row| row["footprint"].as_u64())
                .fold(0_u64, u64::saturating_add),
        );
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
                    units.push(aterm_json::json!({"triple":triple,"package":message["package"],"unit":message["unit"],"started_unix_s":begin,"ended_unix_s":end,"pre_spawn_s":begin-queued,"memory_waiting_s":start["memory_waiting_s"],"memory_waited_for":start["memory_waited_for"],"wall_s":end-begin,"cpu_s":message["cpu_s"],"compiler_pid":start["compiler_pid"],"sampled_compiler_cpu_s":counters.map(|c|c.cpu as f64/1e9),"sampled_compiler_cpu_wait_s":counters.filter(|c|!c.runnable_missing).map(|c|c.runnable.saturating_sub(c.cpu) as f64/1e9),"success":message["success"]}));
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
    let waits: Vec<_> = units
        .iter()
        .filter_map(|unit| unit["memory_waiting_s"].as_f64())
        .collect();
    let waited_units: Vec<_> = units
        .iter()
        .filter(|unit| {
            unit["memory_waiting_s"]
                .as_f64()
                .is_some_and(|wait| wait > 0.0)
        })
        .cloned()
        .collect();
    let unit_wait = aterm_json::json!({
        "reported_units":waits.len(),
        "sum_s":waits.iter().sum::<f64>(),
        "max_s":waits.iter().copied().reduce(f64::max),
        "waited_units":waited_units,
    });
    let tree_wall_s = result["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["wall_s"].as_f64())
        .fold(0.0_f64, f64::max);
    let slice_walls: Vec<_> = result["slices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| aterm_json::json!({"triple":row["triple"],"wall_s":row["wall_s"]}))
        .collect();
    let summary = aterm_json::json!({"result":result.clone(),"qualify_wall_s":result["wall_s"],"tree_wall_s":tree_wall_s,"export_wall_s":result["export"]["wall_s"],"slice_walls":slice_walls,"slice_unit_admission_wait":unit_wait,"slice_memory_ready":result["slice_memory_ready"],"tree_memory_ready":result["tree_memory_ready"],"peak_host_used_bytes":peak_host_used_bytes,"peak_descendant_footprint_bytes":peak_descendant_footprint_bytes,"global_capacity_samples":global_capacity_samples,"global_capacity_unavailable_samples":global_capacity_unavailable,"private_capacity_samples":private_capacity_samples,"private_capacity_unavailable_samples":private_capacity_unavailable,"capacity_timeline":capacity_timeline,"sample_started_unix_s":started_unix_s,"artifacts":artifacts,"sampled_roles":roles,"bins":bins,"critical_units":critical,"sampled_max_cores":max_cores,"max_live_compilers":max_live_compilers,"max_progressing_compilers":max_progressing_compilers,"compiler_count_over_slots_s":over_width_s,"max_sample_interval_s":max_interval,"max_load1":max_load,"unavailable_samples":unavailable,"truncated_samples":truncated_samples,"unread_process_rows":unread_process_rows,"completed_units":units.len()});
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeWork {
    CompileTests,
    Lint,
    Release,
}

impl TreeWork {
    fn label(self) -> &'static str {
        match self {
            Self::CompileTests => "test",
            Self::Lint => "lint",
            Self::Release => "release-tree",
        }
    }
    fn arguments(self) -> Vec<&'static str> {
        match self {
            Self::CompileTests => vec!["test", "--all", "--as-release", "--compile-only"],
            Self::Lint => vec!["release", "--tree", "--checks", "lint"],
            Self::Release => vec!["release", "--tree"],
        }
    }
}

impl TreeShape {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CompileLint => "compile-lint",
            Self::Release => "release",
        }
    }

    fn work(self) -> &'static [TreeWork] {
        const COMPILE_LINT: &[TreeWork] = &[TreeWork::CompileTests, TreeWork::Lint];
        const RELEASE: &[TreeWork] = &[TreeWork::Release];
        match self {
            Self::CompileLint => COMPILE_LINT,
            Self::Release => RELEASE,
        }
    }
}

#[derive(Clone)]
struct TreeAdmissionSignal {
    started: std::sync::mpsc::Sender<u32>,
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct TreeAdmissionEnded(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for TreeAdmissionEnded {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn tree<'a>(
    repo: &Path,
    root: &Path,
    opts: &Options,
    shares: &'a aterm_capacity::BranchShares,
    admission_barrier: Option<&std::sync::Mutex<Option<aterm_capacity::InitialReservation<'a>>>>,
    admission: Option<TreeAdmissionSignal>,
) -> Result<Vec<aterm_json::Value>, String> {
    let _finished = FinishPilotShare {
        shares,
        branch: 1,
        admission_barrier,
    };
    if opts.mode == "alone" {
        return Ok(Vec::new());
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = opts.tree_work.work().iter().copied().map(|work| {
            let admission = admission.clone();
            scope.spawn(move || {
            let _ended = admission
                .as_ref()
                .map(|signal| TreeAdmissionEnded(std::sync::Arc::clone(&signal.ended)));
            let check = work.label();
            let (program, prefix) = invocation(&opts.verify, opts.mode == "chain");
            let mut cmd = aterm_uds::spawnfd::command(program);
            cmd.args(prefix).args(work.arguments());
            cmd.current_dir(repo)
                .env(aterm_capacity::RELEASE_RUN_ENV, "1")
                .env("CARGO_TARGET_DIR", root.join(format!("tree-{check}.noindex")))
                .arg("--log").arg(root.join(format!("tree-{check}.log")))
                .arg("--events").arg(root.join(format!("tree-{check}.events")));
            if root.join("test-history.json").is_file() {
                cmd.arg("--test-history").arg(root.join("test-history.json"));
            }
            shares
                .hand_to(&mut cmd, true)
                .map_err(|error| format!("hand the tree its qualify share: {error}"))?;
            let output = crate::durable::create_fresh(&root.join(format!("tree-{check}.stdout"))).map_err(|e| e.to_string())?;
            cmd.stdout(output.try_clone().map_err(|e| e.to_string())?).stderr(output);
            let started = Instant::now();
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            if let Some(signal) = &admission {
                signal
                    .started
                    .send(child.id())
                    .map_err(|_| "pilot stopped awaiting the tree's memory admission".to_owned())?;
            }
            let usage = loop {
                if admission.as_ref().is_some_and(|signal| {
                    signal.stop.load(std::sync::atomic::Ordering::SeqCst)
                }) {
                    child.kill().map_err(|e| format!("stop malformed pilot tree: {e}"))?;
                    break child.wait_with_rusage().map_err(|e| e.to_string())?;
                }
                if let Some(usage) = child.try_wait_with_rusage().map_err(|e| e.to_string())? {
                    break usage;
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            let row = aterm_json::json!({"check":check,"wall_s":started.elapsed().as_secs_f64(),"cpu_s":usage.cpu.as_secs_f64(),"success":usage.status.success()});
            crate::durable::write_fresh(&root.join(format!("tree-{check}.result.json")), row.to_string())
                .map_err(|e| e.to_string())?;
            if !usage.status.success() { return Err(format!("pilot tree {check}: {row}")); }
            Ok(row)
        })}).collect();
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
    let commit = crate::snapshot::git_text(repo, &["rev-parse", "HEAD"])?;
    crate::release_tools::clean_at(repo, commit.trim())?;
    if opts.real_export || opts.mode == "export" {
        match crate::cutter::running_stamp() {
            Ok(crate::cutter::Stamp::Proved(source)) if source == commit.trim() => {}
            stamp => {
                let stamp = stamp.map_err(|why| why.why.clone())?;
                if stamp.commit() != commit.trim() {
                    return Err(
                        "real export requires the measured candidate's own clean cutter".into(),
                    );
                }
                let image = std::env::current_exe().map_err(|error| error.to_string())?;
                let tools =
                    crate::release_tools::stage(&release, commit.trim(), &image, &opts.verify)?;
                let proof = crate::release_tools::publish(&release, &tools)?;
                let mut command = aterm_uds::spawnfd::command(&tools.cutter.path);
                command
                    .current_dir(repo)
                    .args(frozen_arguments(opts, &tools.verify.path))
                    .env(crate::release_tools::RECORD_ENV, proof);
                let status = command.status().map_err(|error| error.to_string())?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("frozen qualify pilot exited {status}"))
                };
            }
        }
    }
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
    let (_release, ()) = under_reservation(
        || {
            aterm_capacity::admit(reservation(), aterm_capacity::Admit::Wait)
                .map_err(|e| e.to_string())
        },
        || Ok(()),
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
        "waiting_s":claim.waited().as_secs_f64(),
        "asked_at_unix_ms":claim.record().start_ms,
        "admitted_at_unix_ms":claim.record().held.map(|held|held.admitted_ms),
        "machine_cores":aterm_capacity::Reading::now(&root).cores,
        "records":records.iter().map(|r|aterm_json::json!({"pid":r.pid,"what":r.need.what,"requested":r.need.cores,"granted":r.held.map(|h|h.cores),"rank":format!("{:?}",r.need.rank)})).collect::<Vec<_>>()
    });
    crate::durable::write_fresh(&root.join("admission.json"), admission.to_string())
        .map_err(|e| e.to_string())?;
    eprintln!("qualify-pilot-admission: {admission}");
    require_series_grant(&opts.mode, aterm_capacity::cores(), claim.cores())?;
    let modes = order(&opts.mode);
    let history = aterm_types::dirs::account_state_dir()
        .map(|root| {
            aterm_verify::test_history::load(&root.join(aterm_capacity::book::TEST_HISTORY))
        })
        .unwrap_or_default()
        .encode();
    crate::durable::write_fresh(&root.join("test-history-seed.json"), &history)
        .map_err(|error| error.to_string())?;
    for (index, mode) in modes.iter().enumerate() {
        crate::release_tools::clean_at(repo, commit.trim())?;
        let at = root.join(format!("take{}-{mode}", index + 1));
        std::fs::create_dir(&at).map_err(|e| e.to_string())?;
        crate::durable::write_fresh(&at.join("test-history.json"), &history)
            .map_err(|error| error.to_string())?;
        let verifier = if (opts.mode == "series" && matches!(*mode, "shared" | "tree"))
            || (opts.mode == "matched" && *mode == "before")
        {
            opts.before
                .as_ref()
                .ok_or("series requires --before-verify PATH for its controls")?
        } else {
            &opts.verify
        };
        let options = Options {
            verify: verifier.clone(),
            before: None,
            mode: if opts.mode == "matched" {
                "chain"
            } else {
                mode
            }
            .to_string(),
            tree_work: opts.tree_work,
            export_seconds: opts.export_seconds,
            real_export: opts.real_export,
        };
        if *mode == "export" {
            let result = crate::release_steps::measure_local_export(
                repo,
                &at,
                commit.trim(),
                claim.cores(),
            )?;
            println!("qualify-export-result: {result}");
            if result["success"].as_bool() != Some(true) {
                return Err(format!("local full export did not pass: {result}"));
            }
        } else {
            one(repo, &at, &options, claim.cores(), commit.trim())?;
        }
        crate::release_tools::clean_at(repo, commit.trim())?;
    }
    Ok(())
}

fn frozen_arguments(opts: &Options, verify: &Path) -> Vec<OsString> {
    let mut args = vec![
        "qualify-pilot".into(),
        "--verify".into(),
        verify.into(),
        "--mode".into(),
        opts.mode.clone().into(),
        "--tree-work".into(),
        opts.tree_work.as_str().into(),
    ];
    if let Some(before) = &opts.before {
        args.extend(["--before-verify".into(), before.clone().into()]);
    }
    if opts.real_export {
        args.extend(["--export-work".into(), "real".into()]);
    }
    if let Some(seconds) = opts.export_seconds {
        args.extend(["--export-seconds".into(), seconds.to_string().into()]);
    }
    args
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
    if mode == "series" {
        vec!["alone", "shared", "chain", "chain", "shared", "tree"]
    } else if mode == "matched" {
        vec!["before", "after", "after", "before"]
    } else {
        vec![mode]
    }
}

fn require_series_grant(mode: &str, requested: u32, granted: u32) -> Result<(), String> {
    if mode == "series" && granted != requested {
        Err(format!(
            "series needs the full {requested}-core grant, got {granted}; no take started"
        ))
    } else {
        Ok(())
    }
}

fn slice_child_command(
    image: &Path,
    repo: &Path,
    root: &Path,
    commit: &str,
    demoted: bool,
    shares: &aterm_capacity::BranchShares,
) -> Result<aterm_uds::spawnfd::Command, String> {
    let mut command = aterm_uds::spawnfd::command(image);
    command
        .arg(SLICE_CHILD_FLAG)
        .arg(repo)
        .arg(root)
        .arg(commit)
        .arg(if demoted { "demoted" } else { "normal" })
        .current_dir(repo);
    shares
        .hand_to(&mut command, false)
        .map_err(|error| format!("hand the build its qualify share: {error}"))?;
    Ok(command)
}

fn relay_slice_child_events(
    input: impl std::io::Read,
    mut transcript: impl std::io::Write,
    sender: std::sync::mpsc::Sender<Result<aterm_json::Value, String>>,
) -> Result<(), String> {
    for line in std::io::BufReader::new(input).lines() {
        let line = line.map_err(|error| format!("read slice child output: {error}"))?;
        writeln!(transcript, "{line}")
            .map_err(|error| format!("write slice child transcript: {error}"))?;
        let Some(event) = line.strip_prefix(SLICE_CHILD_EVENT) else {
            continue;
        };
        let event = aterm_json::from_str(event)
            .map_err(|error| format!("decode slice child event: {error}"));
        sender
            .send(event)
            .map_err(|_| "pilot stopped reading slice child events".to_owned())?;
    }
    Ok(())
}

fn next_slice_child_event(
    receiver: &std::sync::mpsc::Receiver<Result<aterm_json::Value, String>>,
    child: &mut aterm_uds::spawnfd::Child,
    expected: &str,
) -> Result<aterm_json::Value, String> {
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => return event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    return Err(format!(
                        "slice child exited ({status}) before reporting {expected}"
                    ));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "slice child closed its events before reporting {expected}"
                ));
            }
        }
    }
}

fn shared_ready_shape(event: &aterm_json::Value, minimum: u64) -> bool {
    event["event"].as_str() == Some("slice-memory-ready")
        && event["held"].as_bool() == Some(true)
        && event["mode"].as_str() == Some("shared")
        && event["label"].as_str() == Some("release slice memory budget")
        && event["mem"].as_u64().is_some_and(|mem| mem >= minimum)
}

fn ready_claim_is_live(event: &aterm_json::Value, child_pid: u32) -> Result<bool, String> {
    if event["event"].as_str() != Some("slice-memory-ready")
        || event["held"].as_bool() != Some(true)
    {
        return Err(format!("slice child reported no held memory gate: {event}"));
    }
    if event["mode"].as_str() == Some("build-ended") {
        return Ok(true);
    }
    if event["mode"].as_str() != Some("shared") {
        return Err(format!("slice child memory gate has unknown mode: {event}"));
    }
    let minimum = crate::slice_units::live_shared_floor(2);
    if !shared_ready_shape(event, minimum) {
        return Err(format!(
            "slice child shared memory gate has the wrong label or reservation: {event}"
        ));
    }
    let identities = [event];
    let ledger = aterm_capacity::Ledger::user().ok_or("no user capacity ledger")?;
    let matches = |records: &[aterm_capacity::Record]| {
        identities.iter().all(|claim| {
            let identity = &claim["claim"];
            records.iter().any(|record| {
                record.pid == child_pid
                    && identity["pid"].as_u64() == Some(u64::from(child_pid))
                    && identity["start_ms"].as_u64() == Some(record.start_ms)
                    && identity["seq"].as_u64() == Some(record.seq)
                    && claim["label"].as_str() == Some(record.need.what.as_str())
                    && claim["mem"].as_u64() == Some(record.need.mem)
                    && record.accounting == aterm_capacity::Accounting::Holder
                    && record.need.rank == aterm_capacity::Rank::Release
                    && record.need.margin == 1
                    && record.held.is_some_and(|held| !held.alone)
            })
        })
    };
    // Two complete scans reject a directory read that straddled a claim's removal.
    let first = ledger.list().map_err(|error| error.to_string())?;
    let second = ledger.list().map_err(|error| error.to_string())?;
    Ok(matches(&first) && matches(&second))
}

/// The release-shaped shared and chain takes use the fixed admission order. `tree` deliberately
/// starts the current tree child first as an ordering probe, but its new shared build parent means
/// it is not the retained pre-change memory-order control.
fn admission_barrier_share(opts: &Options, slots: u32, export_share: u32) -> u32 {
    if opts.tree_work == TreeShape::Release && matches!(opts.mode.as_str(), "shared" | "chain") {
        slots.saturating_sub(export_share)
    } else {
        0
    }
}

fn tippy_claim_shapes() -> Result<[crate::release_steps::TreeLintClaimShape; 1], String> {
    crate::release_steps::tree_lint_claim_shapes()
        .ok_or_else(|| "the release tree has no Tippy stage".to_owned())
}

fn matching_tree_claim(
    records: &[aterm_capacity::Record],
    processes: &[aterm_sysprobe::procs::ProcessReading],
    tree_pid: u32,
    shapes: &[crate::release_steps::TreeLintClaimShape],
) -> Option<(aterm_capacity::Record, crate::release_steps::TreeLintClaim)> {
    if !processes.iter().any(|process| process.pid == tree_pid) {
        return None;
    }
    let line = descendants(processes, tree_pid);
    let mut outcome = None;
    for record in records.iter().filter(|record| line.contains(&record.pid)) {
        let Some(state) = crate::release_steps::classify_tree_lint_claim(record, shapes) else {
            continue;
        };
        let stronger = crate::release_steps::stronger_tree_lint_claim(
            outcome.as_ref().map(|(_, state)| *state),
            state,
        );
        if outcome
            .as_ref()
            .is_none_or(|(_, current)| *current != stronger)
        {
            outcome = Some((record.clone(), state));
        }
    }
    outcome
}

/// Wait until the verifier has durably requested lint memory, while the build's slice parent is
/// already held and every compile-capable jobserver token is still behind the pilot's barrier.
/// A held request opens the concurrent schedule. An exact queued request opens production's
/// bounded escape: slice work starts against its private parent, and lint stays queued until the
/// host has room. A malformed production label stops the tree before any compiler token returns.
/// Waiting for held-only here would deadlock whenever the parent and early claim do not fit in
/// the first resource-host reading.
fn await_tree_memory(
    tree_pid: u32,
    ended: &std::sync::atomic::AtomicBool,
    barrier_slots: u32,
) -> Result<aterm_json::Value, String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("no user capacity ledger")?;
    let shapes = tippy_claim_shapes()?;
    let waiting = Instant::now();
    loop {
        if ended.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the release tree ended before requesting its Tippy memory claim".into());
        }
        let processes = aterm_sysprobe::processes()
            .ok_or("cannot inspect the release tree while awaiting its memory claim")?;
        let first = ledger.list().map_err(|error| error.to_string())?;
        if let Some((claim, state)) =
            matching_tree_claim(&first, &processes.procs, tree_pid, &shapes)
        {
            // A second whole-ledger read rejects a scan that straddled the claim's removal.
            let second = ledger.list().map_err(|error| error.to_string())?;
            let still = matching_tree_claim(&second, &processes.procs, tree_pid, &shapes);
            if still.is_some_and(|(still, still_state)| {
                (still.pid, still.start_ms, still.seq) == (claim.pid, claim.start_ms, claim.seq)
                    && still_state == state
            }) {
                let held = state == crate::release_steps::TreeLintClaim::Held;
                let queued = state == crate::release_steps::TreeLintClaim::Waiting;
                let invalid = state == crate::release_steps::TreeLintClaim::Invalid;
                return Ok(aterm_json::json!({
                    "event":"tree-memory-ready",
                    "held":held,
                    "queued":queued,
                    "invalid":invalid,
                    "mode":if held { "concurrent" } else if queued { "bounded-escape" } else { "refused" },
                    "label":claim.need.what,
                    "mem":claim.need.mem,
                    "claim":{"pid":claim.pid,"start_ms":claim.start_ms,"seq":claim.seq},
                    "tree_pid":tree_pid,
                    "barrier_slots":barrier_slots,
                    "waiting_s":waiting.elapsed().as_secs_f64(),
                }));
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Start the tree-first probe before observing slice admission; every other take proves the slice
/// parent first. The probe still uses the current shared build parent, so only the retained
/// pre-change binary is a faithful old-memory-order control.
fn start_tree_around_memory<M, T, P>(
    mode: &str,
    await_memory: impl FnOnce() -> Result<M, String>,
    start_tree: impl FnOnce() -> T,
    await_tree_start: impl FnOnce() -> Result<P, String>,
) -> Result<(M, T, Option<P>), String> {
    if mode == "tree" {
        let tree = start_tree();
        let started = await_tree_start()?;
        let memory = await_memory()?;
        Ok((memory, tree, Some(started)))
    } else {
        let memory = await_memory()?;
        let tree = start_tree();
        Ok((memory, tree, None))
    }
}

struct FinishPilotShare<'shares, 'barrier> {
    shares: &'shares aterm_capacity::BranchShares,
    branch: usize,
    admission_barrier:
        Option<&'barrier std::sync::Mutex<Option<aterm_capacity::InitialReservation<'shares>>>>,
}

impl Drop for FinishPilotShare<'_, '_> {
    fn drop(&mut self) {
        if let Some(barrier) = self.admission_barrier {
            barrier
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        self.shares.finish(self.branch);
    }
}

fn export_surrogate(
    shares: &aterm_capacity::BranchShares,
    share: u32,
    seconds: u64,
) -> aterm_json::Value {
    let _finished = FinishPilotShare {
        shares,
        branch: 2,
        admission_barrier: None,
    };
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(seconds));
    aterm_json::json!({
        "kind":"jobserver-share-surrogate",
        "share":share,
        "configured_s":seconds,
        "wall_s":started.elapsed().as_secs_f64(),
    })
}

fn one(repo: &Path, root: &Path, opts: &Options, slots: u32, commit: &str) -> Result<(), String> {
    let pid = std::process::id();
    println!(
        "qualify-pilot: {} S={} mode={}",
        root.display(),
        slots,
        opts.mode
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut samples =
        crate::durable::create_fresh(&root.join("processes.jsonl")).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let export_seconds = opts.export_seconds();
    let export_share = crate::release_steps::export_share(slots, export_seconds.is_some(), true);
    let shares = aterm_capacity::BranchShares::new(
        &root.join("qualify.jobs"),
        slots,
        export_share,
        [true, true],
    )
    .map_err(|error| format!("create the qualify branch shares: {error}"))?;
    let barrier_slots = admission_barrier_share(opts, slots, export_share);
    if opts.tree_work == TreeShape::Release
        && matches!(opts.mode.as_str(), "shared" | "chain")
        && barrier_slots == 0
    {
        return Err("the release-shaped admission barrier has no non-export jobserver slot".into());
    }
    // This storage outlives the scoped branch threads which borrow it. No compiler can consume
    // its reserved memory before both branch parents are live.
    let barrier = std::sync::Mutex::new(
        (barrier_slots > 0)
            .then(|| shares.reserve_initial())
            .transpose()
            .map_err(|error| {
                format!("reserve the admission barrier's {barrier_slots} branch tokens: {error}")
            })?,
    );
    if barrier
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|tokens| tokens.len() != barrier_slots as usize)
    {
        return Err(format!(
            "admission barrier reserved {} of its {barrier_slots} jobserver slots",
            barrier
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map_or(0, aterm_capacity::InitialReservation::len)
        ));
    }
    let (slices, tree, export, memory_ready, tree_memory_ready, build_usage) = std::thread::scope(
        |scope| -> Result<_, String> {
            let (done, receiver) = std::sync::mpsc::channel::<()>();
            let sampler = scope.spawn(move || -> Result<(), String> {
                let mut global = BTreeMap::new();
                loop {
                    writeln!(
                        samples,
                        "{}",
                        sample(pid, root, started.elapsed().as_secs_f64(), &mut global)
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

            let image = std::env::current_exe()
                .map_err(|error| format!("find qualify pilot executable: {error}"))?;
            let build_finished = FinishPilotShare {
                shares: &shares,
                branch: 0,
                admission_barrier: Some(&barrier),
            };
            let mut command =
                slice_child_command(&image, repo, root, commit, opts.mode == "tree", &shares)?;
            let stderr = crate::durable::create_fresh(&root.join("slice-child.stderr"))
                .map_err(|error| error.to_string())?;
            let transcript = crate::durable::create_fresh(&root.join("slice-child.stdout"))
                .map_err(|error| error.to_string())?;
            command
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(stderr);
            let mut child = command
                .spawn()
                .map_err(|error| format!("start qualify pilot slice child: {error}"))?;
            let child_pid = child.id();
            let Some(stdout) = child.stdout.take() else {
                let _ = child.kill();
                let _ = child.wait();
                return Err("qualify pilot slice child has no output pipe".into());
            };
            let (send_event, events) = std::sync::mpsc::channel();
            let relay =
                scope.spawn(move || relay_slice_child_events(stdout, transcript, send_event));
            let prepared = next_slice_child_event(&events, &mut child, "preparation")?;
            if prepared["event"].as_str() != Some("prepared") {
                return Err(format!(
                    "slice child did not prepare its sealed source: {prepared}"
                ));
            }
            let tree_ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let tree_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (tree_started, tree_pid) = std::sync::mpsc::channel();
            let admission =
                (barrier_slots > 0 || opts.mode == "tree").then_some(TreeAdmissionSignal {
                    started: tree_started,
                    ended: std::sync::Arc::clone(&tree_ended),
                    stop: std::sync::Arc::clone(&tree_stop),
                });
            let (memory_ready, tree, old_order_tree_pid) = start_tree_around_memory(
                &opts.mode,
                || {
                    let ready = next_slice_child_event(&events, &mut child, "memory admission")?;
                    if !ready_claim_is_live(&ready, child_pid)? {
                        return Err("slice child memory gate was not durably live".into());
                    }
                    Ok(ready)
                },
                || scope.spawn(|| tree(repo, root, opts, &shares, Some(&barrier), admission)),
                || {
                    tree_pid
                        .recv()
                        .map_err(|_| "release tree ended before reporting its process".to_owned())
                },
            )?;
            let tree_memory_ready = if barrier_slots > 0 {
                let tree_pid = match old_order_tree_pid {
                    Some(pid) => pid,
                    None => tree_pid.recv().map_err(|_| {
                        "release tree ended before reporting its process".to_owned()
                    })?,
                };
                let ready = await_tree_memory(tree_pid, &tree_ended, barrier_slots)?;
                if ready["queued"].as_bool() == Some(true)
                    || ready["invalid"].as_bool() == Some(true)
                {
                    if let Some(reservation) = barrier
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        reservation
                            .release_to_build()
                            .map_err(|error| format!("serialize qualify onto build: {error}"))?;
                    }
                    if ready["invalid"].as_bool() == Some(true) {
                        tree_stop.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                } else {
                    barrier
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                }
                ready
            } else {
                aterm_json::json!({
                    "event":"tree-memory-ready",
                    "held":false,
                    "mode":if opts.mode == "tree" { "tree-first-probe" } else { "not-release-shaped" },
                    "barrier_slots":0,
                })
            };
            let export = export_seconds.map(|seconds| {
                let shares = &shares;
                scope.spawn(move || {
                    if opts.real_export {
                        let _finished = FinishPilotShare {
                            shares,
                            branch: 2,
                            admission_barrier: None,
                        };
                        crate::release_steps::measure_local_export(
                            repo,
                            &root.join("export"),
                            commit,
                            export_share,
                        )
                    } else {
                        Ok(export_surrogate(shares, export_share, seconds))
                    }
                })
            });
            let ended = next_slice_child_event(&events, &mut child, "slice results")?;
            if ended["event"].as_str() != Some("ended") {
                return Err(format!("slice child did not report its results: {ended}"));
            }
            let slices = ended["slices"]
                .as_array()
                .cloned()
                .ok_or("slice child result is not an array")?;
            let build_usage = child
                .wait_with_rusage()
                .map_err(|error| format!("wait for qualify pilot slice child: {error}"))?;
            drop(build_finished);
            let relayed = relay
                .join()
                .map_err(|_| "slice child event relay panicked".to_owned())?;
            relayed?;
            if !build_usage.status.success() {
                return Err(format!(
                    "qualify pilot slice child failed ({}) — see {}",
                    build_usage.status,
                    root.join("slice-child.stderr").display()
                ));
            }
            let tree = tree.join().map_err(|_| "pilot tree panicked")??;
            let export = export
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "pilot export panicked".to_owned())?
                })
                .transpose()?;
            drop(done);
            sampler.join().map_err(|_| "pilot sampler panicked")??;
            Ok((
                slices,
                tree,
                export,
                memory_ready,
                tree_memory_ready,
                aterm_json::json!({
                    "pid":child_pid,
                    "cpu_s":build_usage.cpu.as_secs_f64(),
                    "peak_bytes":build_usage.maxrss,
                }),
            ))
        },
    )?;
    let (verify_sha256, _) = crate::dmg::sha256_and_size(&opts.verify)?;
    let report = aterm_json::json!({"mode":opts.mode,"tree_work":opts.tree_work.as_str(),"source":commit,"slots":slots,"wall_s":started.elapsed().as_secs_f64(),"slices":slices,"tree":tree,"export":export,"export_share":export_share,"branch_fifos":{"build":shares.pool(false).path(),"tree":shares.pool(true).path()},"slice_memory_ready":memory_ready,"tree_memory_ready":tree_memory_ready,"build_child":build_usage,"verify_sha256":verify_sha256,"verify":opts.verify});
    crate::durable::write_fresh(&root.join("result.json"), report.to_string())
        .map_err(|e| e.to_string())?;
    println!("qualify-pilot-result: {report}");
    if opts.real_export && report["export"]["success"].as_bool() != Some(true) {
        return Err("the full export failed; the take is not a passing gate".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn observing_a_private_fifo_does_not_spend_its_tokens() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jobs");
        let pool = aterm_capacity::Jobserver::new(&path, 3).unwrap();
        assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(3));
        let token = pool.try_acquire().unwrap().unwrap();
        assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(2));
        assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(2));
        drop(token);
        assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(3));
        assert!(fifo_snapshot(&dir.path().join("missing"))["available_tokens"].is_null());
    }
    #[test]
    fn matched_takes_use_one_priority_and_real_export_cannot_take_a_surrogate_wall() {
        let options = parse(
            [
                "--verify",
                "after",
                "--before-verify",
                "before",
                "--mode",
                "matched",
                "--tree-work",
                "release",
                "--export-work",
                "real",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(order(&options.mode), ["before", "after", "after", "before"]);
        assert!(options.real_export);
        assert_eq!(options.export_seconds(), Some(0));
        let mut frozen = options.clone();
        frozen.verify = PathBuf::from("/frozen/aterm-verify");
        let arguments = frozen_arguments(&options, &frozen.verify);
        let decoded = parse(
            arguments
                .into_iter()
                .skip(1)
                .map(|argument| argument.into_string().unwrap()),
        )
        .unwrap();
        assert_eq!(decoded, frozen);
        assert!(
            parse(
                ["--verify", "after", "--mode", "matched",]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
        assert!(
            parse(
                [
                    "--verify",
                    "after",
                    "--mode",
                    "shared",
                    "--export-work",
                    "real",
                    "--export-seconds",
                    "763",
                ]
                .into_iter()
                .map(str::to_owned)
            )
            .is_err()
        );
    }
    #[test]
    fn series_refuses_a_partial_grant_before_any_take_but_single_takes_can_measure_it() {
        assert_eq!(reservation().rank, aterm_capacity::Rank::Release);
        assert_eq!(reservation().cores, 0);
        assert_eq!(reservation().mem, 0);
        assert!(require_series_grant("series", 18, 18).is_ok());
        assert!(require_series_grant("series", 18, 4).is_err());
        assert!(require_series_grant("alone", 18, 4).is_ok());
    }
    #[test]
    fn tree_first_probe_starts_tree_before_slice_memory_admission() {
        let events = std::cell::RefCell::new(Vec::new());
        let (memory, tree, started) = start_tree_around_memory(
            "tree",
            || {
                events.borrow_mut().push("memory");
                Ok(1)
            },
            || {
                events.borrow_mut().push("tree");
                2
            },
            || {
                events.borrow_mut().push("tree-started");
                Ok(3)
            },
        )
        .unwrap();
        assert_eq!((memory, tree), (1, 2));
        assert_eq!(started, Some(3));
        assert_eq!(&*events.borrow(), &["tree", "tree-started", "memory"]);

        events.borrow_mut().clear();
        let (_, _, started) = start_tree_around_memory(
            "shared",
            || {
                events.borrow_mut().push("memory");
                Ok(())
            },
            || events.borrow_mut().push("tree"),
            || -> Result<(), String> {
                panic!("the fixed order consumes its start handshake after memory admission")
            },
        )
        .unwrap();
        assert_eq!(started, None);
        assert_eq!(
            &*events.borrow(),
            &["memory", "tree"],
            "moving the control to the fixed order is the negative mutation"
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
        let sample = aterm_json::json!({"elapsed_s":1.0,"at_unix_s":100.0,"capacity":{
            "global":{"error":null,"records":[{"what":"release slice memory budget","mem":49478023296_u64}]},
            "slice_budget":{"error":null,"records":[{"what":"release slice x86_64 unit aterm link","mem":24696061952_u64}]}
        },"members":[
            {"pid":4,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64},
            {"pid":5,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64}
        ]});
        let private_unread = aterm_json::json!({"elapsed_s":2.0,"at_unix_s":101.0,"capacity":{
            "global":{"error":null,"records":[]},
            "slice_budget":{"error":"unread","records":null}
        },"members":[]});
        std::fs::write(
            dir.path().join("processes.jsonl"),
            format!("{sample}\n{private_unread}\n"),
        )
        .unwrap();
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let start = aterm_json::json!({"reason":"compiler-message","message":{"event":"release-slice-unit","phase":"started","pid":3,"compiler_pid":4,"queued_unix_s":10.0,"at_unix_s":11.0,"package":"aterm","unit":"aterm bin","memory_waiting_s":0.5,"memory_waited_for":{"reason":"memory short","holders":[{"pid":8,"what":"release slice x86_64 unit aterm link","claimed_bytes":24696061952_u64}]}}});
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
        assert_eq!(summary["global_capacity_samples"].as_u64(), Some(2));
        assert_eq!(summary["private_capacity_samples"].as_u64(), Some(1));
        assert_eq!(
            summary["private_capacity_unavailable_samples"].as_u64(),
            Some(1),
            "NEGATIVE CONTROL: global readability cannot stand in for the private ledger"
        );
        assert_eq!(summary["capacity_timeline"].as_array().unwrap().len(), 2);
        assert_eq!(
            summary["capacity_timeline"][0]["at_unix_s"].as_f64(),
            Some(100.0),
            "unit events and holder snapshots retain the same epoch clock"
        );
        assert_eq!(
            summary["slice_unit_admission_wait"]["waited_units"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            summary["slice_unit_admission_wait"]["waited_units"][0]["memory_waited_for"]["holders"]
                [0]["what"]
                .as_str(),
            Some("release slice x86_64 unit aterm link")
        );
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
        for mode in ["alone", "shared", "chain", "tree", "series"] {
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
        let release = parse_words("--verify /v --mode shared --tree-work release").unwrap();
        assert_eq!(release.tree_work, TreeShape::Release);
        assert_eq!(release.export_seconds(), Some(RELEASE_EXPORT_SECONDS));
        let override_export =
            parse_words("--verify /v --mode shared --tree-work release --export-seconds 5")
                .unwrap();
        assert_eq!(override_export.export_seconds(), Some(5));
        let no_export =
            parse_words("--verify /v --mode shared --tree-work release --export-seconds 0")
                .unwrap();
        assert_eq!(no_export.export_seconds(), None);
        assert!(parse_words("--verify /v --mode shared --tree-work almost").is_err());
        assert!(parse_words("--verify /v --mode shared --export-seconds soon").is_err());
    }

    #[test]
    fn release_shared_and_chain_hold_every_non_export_token_but_tree_is_only_a_probe() {
        let parse_words = |words: &str| parse(words.split_whitespace().map(str::to_owned)).unwrap();
        let shared = parse_words("--verify /v --mode shared --tree-work release");
        let chain = parse_words("--verify /v --mode chain --tree-work release");
        let old = parse_words("--verify /v --mode tree --tree-work release");
        let compile_lint = parse_words("--verify /v --mode shared");
        assert_eq!(admission_barrier_share(&shared, 18, 4), 14);
        assert_eq!(admission_barrier_share(&chain, 18, 4), 14);
        assert_eq!(admission_barrier_share(&old, 18, 4), 0);
        assert_eq!(admission_barrier_share(&compile_lint, 18, 4), 0);
        // Negative control: omitting the barrier leaves every non-export compiler token live.
        assert_eq!(18 - 4 - admission_barrier_share(&old, 18, 4), 14);
        assert_eq!(18 - 4 - admission_barrier_share(&shared, 18, 4), 0);
    }

    #[test]
    fn only_a_live_exact_tippy_request_in_the_tree_opens_the_compiler_barrier() {
        use aterm_sysprobe::procs::ProcessReading;
        let shapes = tippy_claim_shapes().unwrap();
        let process = |pid, ppid| ProcessReading {
            pid,
            ppid,
            ..ProcessReading::default()
        };
        let processes = [process(100, 1), process(110, 100), process(200, 1)];
        let held = aterm_capacity::Held {
            admitted_ms: 2,
            cores: 0,
            alone: false,
        };
        let record = |label: &str, bytes: u64| aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::Holder,
            pid: 110,
            start_ms: 1,
            seq: 2,
            checkout: "/release".into(),
            need: aterm_capacity::Need::new(label)
                .rank(aterm_capacity::Rank::Release)
                .mem(bytes)
                .margin(1)
                .covers(),
            timing: aterm_capacity::TimingEvidence::default(),
            held: Some(held),
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        for shape in &shapes {
            let valid = record(&shape.label, shape.bytes);
            assert_eq!(
                matching_tree_claim(std::slice::from_ref(&valid), &processes, 100, &shapes)
                    .map(|(_, state)| state),
                Some(crate::release_steps::TreeLintClaim::Held)
            );
            let mut waiting = valid;
            waiting.held = None;
            assert_eq!(
                matching_tree_claim(&[waiting], &processes, 100, &shapes).map(|(_, state)| state),
                Some(crate::release_steps::TreeLintClaim::Waiting),
                "an exact queued request selects the bounded escape"
            );
        }

        let whole = &shapes[0];
        let valid = record(&whole.label, whole.bytes);
        let mut invalid = valid.clone();
        invalid.need.mem -= 1;
        assert_eq!(
            matching_tree_claim(std::slice::from_ref(&invalid), &processes, 100, &shapes)
                .map(|(_, state)| state),
            Some(crate::release_steps::TreeLintClaim::Invalid),
            "an undersized whole-stage claim refuses the tree instead of opening its barrier"
        );
        let mut lookalike = valid.clone();
        lookalike.need.what.push_str(" later");
        let mut ordinary = valid.clone();
        ordinary.need.rank = aterm_capacity::Rank::Ordinary;
        let mut uncovered = valid.clone();
        uncovered.need.covers = false;
        let mut foreign = valid.clone();
        foreign.pid = 200;
        assert!(matching_tree_claim(&[lookalike], &processes, 100, &shapes).is_none());
        for mutant in [ordinary, uncovered] {
            assert_eq!(
                matching_tree_claim(&[mutant], &processes, 100, &shapes).map(|(_, state)| state),
                Some(crate::release_steps::TreeLintClaim::Invalid)
            );
        }
        assert!(matching_tree_claim(&[foreign], &processes, 100, &shapes).is_none());

        let mut waiting = valid.clone();
        waiting.held = None;
        for records in [
            vec![invalid.clone(), waiting.clone(), valid.clone()],
            vec![valid, waiting, invalid],
        ] {
            assert_eq!(
                matching_tree_claim(&records, &processes, 100, &shapes).map(|(_, state)| state),
                Some(crate::release_steps::TreeLintClaim::Held),
                "duplicate file order cannot override the held valid request"
            );
        }
        assert!(
            matching_tree_claim(&[], &processes[1..], 100, &shapes).is_none(),
            "a missing tree root cannot authenticate a stale claim"
        );
    }

    #[test]
    fn shared_ready_events_name_the_full_release_budget_and_relay_without_inference() {
        let event = aterm_json::json!({
            "event":"slice-memory-ready",
            "mode":"shared",
            "held":true,
            "label":"release slice memory budget",
            "mem":crate::slice_units::MIN_SHARED,
            "claim":{"pid":7,"start_ms":8,"seq":9},
        });
        assert!(shared_ready_shape(&event, crate::slice_units::MIN_SHARED));
        assert!(
            !shared_ready_shape(&event, crate::slice_units::MIN_SHARED + (1 << 30)),
            "NEGATIVE CONTROL: a calibrated parent cannot be authenticated at the code floor"
        );
        for mutant in [
            aterm_json::json!({
                "event":"slice-memory-ready", "mode":"shared", "held":true,
                "label":"release slice aarch64-apple-darwin",
                "mem":crate::slice_units::MIN_SHARED,
            }),
            aterm_json::json!({
                "event":"slice-memory-ready", "mode":"shared", "held":true,
                "label":"release slice memory budget",
                "mem":crate::slice_units::MIN_SHARED - 1,
            }),
            aterm_json::json!({
                "event":"slice-memory-ready", "mode":"shared", "held":false,
                "label":"release slice memory budget",
                "mem":crate::slice_units::MIN_SHARED,
            }),
        ] {
            assert!(!shared_ready_shape(&mutant, crate::slice_units::MIN_SHARED));
        }

        let input = format!("unrelated output\n{SLICE_CHILD_EVENT}{event}\n");
        let mut transcript = Vec::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        relay_slice_child_events(input.as_bytes(), &mut transcript, sender).unwrap();
        assert_eq!(receiver.recv().unwrap().unwrap(), event);
        assert_eq!(String::from_utf8(transcript).unwrap(), input);

        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(
            relay_slice_child_events(
                format!("{SLICE_CHILD_EVENT}not-json\n").as_bytes(),
                Vec::new(),
                sender,
            )
            .is_ok()
        );
        assert!(receiver.recv().unwrap().is_err());
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
