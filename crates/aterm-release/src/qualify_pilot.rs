// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local qualify measurements of builds, checks and fixtures, without publication.

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
    planted_red: bool,
    warmup: bool,
    warm_slices: bool,
    cold_tree: bool,
    build_return: BuildReturn,
    queued_tree_share: QueuedTreeShare,
    cores: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum QueuedTreeShare {
    Retained,
    Resume,
    #[default]
    Lend,
    Matched,
    LendingMatched,
    Compare,
    LendingCompare,
}

impl QueuedTreeShare {
    fn as_str(self) -> &'static str {
        match self {
            Self::Retained => "retained",
            Self::Resume => "resume",
            Self::Lend => "lend",
            Self::Matched => "matched",
            Self::LendingMatched => "lend-matched",
            Self::Compare => "compare",
            Self::LendingCompare => "lend-compare",
        }
    }

    fn for_take(self, take: &str) -> Self {
        if self == Self::Compare {
            match take {
                "before" | "current" | "warmup" => Self::Retained,
                "resumed" => Self::Resume,
                _ => unreachable!("comparison requires a named policy take"),
            }
        } else if self == Self::LendingCompare {
            match take {
                "before" | "warmup" => Self::Retained,
                "current" => Self::Resume,
                "lent" => Self::Lend,
                _ => unreachable!("lending comparison requires a named policy take"),
            }
        } else if self == Self::LendingMatched {
            match take {
                "before" | "warmup" => Self::Resume,
                "after" => Self::Lend,
                _ => unreachable!("matched lending requires a named policy take"),
            }
        } else if self == Self::Matched {
            match take {
                "before" | "warmup" => Self::Retained,
                "after" => Self::Resume,
                _ => unreachable!("matched queued-tree shares require a named take"),
            }
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BuildReturn {
    #[default]
    Early,
    Retained,
    Matched,
}

impl BuildReturn {
    fn as_str(self) -> &'static str {
        match self {
            Self::Early => "early",
            Self::Retained => "retained",
            Self::Matched => "matched",
        }
    }

    fn drain(self, shares: &aterm_capacity::BranchShares) {
        match self {
            Self::Early => shares.build_compilers_drained(),
            Self::Retained => shares.serial_bundle_drained(),
            Self::Matched => unreachable!("select the take's build-return policy before draining"),
        }
    }

    fn for_take(self, take: &str) -> Self {
        if self == Self::Matched {
            match take {
                "before" | "warmup" => Self::Retained,
                "after" => Self::Early,
                _ => unreachable!("matched build returns require a named before or after take"),
            }
        } else {
            self
        }
    }
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
        let [_, repo, root, commit, demoted, cache, build] = args else {
            return Err(
                "qualify pilot slice child needs repo, root, commit, priority, cache and build policy"
                    .into(),
            );
        };
        let commit = commit
            .to_str()
            .ok_or("qualify pilot slice child commit is not text")?;
        let demoted = match demoted.to_str() {
            Some("normal") => false,
            Some("demoted") => true,
            _ => return Err("qualify pilot slice child priority is invalid".into()),
        };
        let warm = match cache.to_str() {
            Some("warm") => true,
            Some("cold") => false,
            _ => return Err("qualify pilot slice cache policy is invalid".into()),
        };
        let full = match build.to_str() {
            Some("full") => true,
            Some("slices") => false,
            _ => return Err("qualify pilot build policy is invalid".into()),
        };
        slice_child(
            Path::new(repo),
            Path::new(root),
            commit,
            demoted,
            warm,
            full,
        )
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

fn require_unsigned_build(ended: &aterm_json::Value, commit: &str) -> Result<(), String> {
    let built = &ended["postlink"];
    let mut archs = built["architectures"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    archs.sort_unstable();
    let digests = [
        "snapshot_digest",
        "macho_sha256",
        "bundle_digest",
        "content_digest",
        "inputs",
    ];
    if built["success"].as_bool() == Some(true)
        && built["kind"].as_str() == Some("real-unsigned-build-finishing")
        && built["commit"].as_str() == Some(commit)
        && archs == ["arm64", "x86_64"]
        && digests.iter().all(|key| {
            built[*key].as_str().is_some_and(|text| {
                text.len() == 64
                    && text
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
    {
        Ok(())
    } else {
        Err("the real build child did not complete unsigned post-link and bundle stages".into())
    }
}

fn slice_child(
    repo: &Path,
    root: &Path,
    commit: &str,
    demoted: bool,
    warm: bool,
    full: bool,
) -> Result<(), String> {
    let preparing = Instant::now();
    let preparation_started_ms = crate::cut_timing::epoch_ms();
    let compilers = full.then(crate::artifact::compilers).transpose()?;
    let take = crate::buildplan::Take::prepare(repo, commit)?;
    let plan = crate::buildplan::contention_plan(repo, &take, root, full)?;
    emit_slice_child(aterm_json::json!({
        "event":"prepared", "wall_s":preparing.elapsed().as_secs_f64(),
        "started_at_unix_ms":preparation_started_ms,
        "ended_at_unix_ms":crate::cut_timing::epoch_ms(),
    }))?;
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
        let slices = crate::buildplan::contention_slices(
            &plan,
            &take,
            root,
            &pool,
            demoted,
            warm,
            Some(ready),
        );
        reporter
            .join()
            .map_err(|_| "slice admission reporter panicked".to_owned())??;
        slices
    })?;
    let postlink = match &compilers {
        Some(compilers) => Some(crate::buildplan::finish_contention_build(
            &plan, &take, root, &slices, compilers,
        )?),
        None => None,
    };
    take.verify()?;
    emit_slice_child(
        aterm_json::json!({"event":"ended","slices":slices.reports(),"postlink":postlink}),
    )
}

pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let mut verify = None;
    let mut mode = None;
    let mut before = None;
    let mut tree_work = None;
    let mut export_seconds = None;
    let mut real_export = false;
    let mut planted_red = false;
    let mut warmup = false;
    let mut warm_slices = None;
    let mut cold_tree = None;
    let mut build_return = None;
    let mut queued_tree_share = None;
    let mut cores = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--queued-tree-share"
                if queued_tree_share.is_none()
                    && matches!(
                        value.as_str(),
                        "retained"
                            | "resume"
                            | "lend"
                            | "matched"
                            | "lend-matched"
                            | "compare"
                            | "lend-compare"
                    ) =>
            {
                queued_tree_share = Some(match value.as_str() {
                    "retained" => QueuedTreeShare::Retained,
                    "resume" => QueuedTreeShare::Resume,
                    "lend" => QueuedTreeShare::Lend,
                    "matched" => QueuedTreeShare::Matched,
                    "lend-matched" => QueuedTreeShare::LendingMatched,
                    "lend-compare" => QueuedTreeShare::LendingCompare,
                    _ => QueuedTreeShare::Compare,
                });
            }
            "--cores" if cores.is_none() => {
                let count = value
                    .parse::<u32>()
                    .ok()
                    .filter(|count| *count > 0)
                    .ok_or("--cores needs a positive core count")?;
                cores = Some(count);
            }
            "--build-return"
                if build_return.is_none()
                    && matches!(value.as_str(), "early" | "retained" | "matched") =>
            {
                build_return = Some(match value.as_str() {
                    "early" => BuildReturn::Early,
                    "retained" => BuildReturn::Retained,
                    _ => BuildReturn::Matched,
                });
            }
            "--export-work" if value == "real" && !real_export => real_export = true,
            "--plant-red" if value == "test" && !planted_red => planted_red = true,
            "--warmup" if value == "full" && !warmup => warmup = true,
            "--slice-cache"
                if warm_slices.is_none() && matches!(value.as_str(), "warm" | "cold") =>
            {
                warm_slices = Some(value == "warm");
            }
            "--tree-cache" if cold_tree.is_none() && matches!(value.as_str(), "warm" | "cold") => {
                cold_tree = Some(value == "cold");
            }
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
                        "alone"
                            | "shared"
                            | "chain"
                            | "tree"
                            | "series"
                            | "export"
                            | "matched"
                            | "fixtures"
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
        planted_red,
        warmup,
        warm_slices: warm_slices.unwrap_or(real_export),
        cold_tree: cold_tree.unwrap_or(false),
        build_return: build_return.unwrap_or_default(),
        queued_tree_share: queued_tree_share.unwrap_or_default(),
        cores,
    };
    if matches!(options.mode.as_str(), "series" | "matched") != options.before.is_some() {
        return Err("series and matched require --before-verify PATH".into());
    }
    if matches!(
        options.queued_tree_share,
        QueuedTreeShare::Matched | QueuedTreeShare::LendingMatched
    ) && (options.mode != "matched" || options.before.as_ref() != Some(&options.verify))
    {
        return Err(
            "matched queued-tree shares require --mode matched and the same verifier in both arms"
                .into(),
        );
    }
    if matches!(
        options.queued_tree_share,
        QueuedTreeShare::Compare | QueuedTreeShare::LendingCompare
    ) && (options.mode != "matched" || options.build_return != BuildReturn::Early)
    {
        return Err("queued-tree comparison requires matched mode and early build return".into());
    }
    if options.build_return == BuildReturn::Matched && options.mode != "matched" {
        return Err("matched build returns require --mode matched".into());
    }
    if options.build_return == BuildReturn::Matched
        && options.before.as_ref() != Some(&options.verify)
    {
        return Err(
            "matched build returns require the same --verify and --before-verify path".into(),
        );
    }
    if options.real_export && options.export_seconds.is_some() {
        return Err("real export cannot use a surrogate duration".into());
    }
    if options.real_export && options.mode != "export" && options.tree_work != TreeShape::Release {
        return Err("real export and fixtures require --tree-work release".into());
    }
    if options.real_export
        && !matches!(
            options.mode.as_str(),
            "shared" | "chain" | "matched" | "export"
        )
    {
        return Err("real work requires shared, chain, matched or export mode".into());
    }
    if options.planted_red && (!options.real_export || options.mode == "export") {
        return Err("a planted red requires the full real local workload".into());
    }
    if options.warmup && (options.mode != "matched" || !options.real_export) {
        return Err("a full warmup requires the matched real local workload".into());
    }
    if options.cold_tree && (!options.real_export || options.mode == "export") {
        return Err("a cold native tree requires the full real local workload".into());
    }
    Ok(options)
}

impl Options {
    /// Calibration uses the same held grant as the following interleaved takes. Its green
    /// control run learns that width's memory peak before either measured arm starts.
    fn order(&self) -> Vec<&str> {
        let mut takes = if self.queued_tree_share == QueuedTreeShare::LendingCompare {
            vec!["before", "current", "lent", "lent", "current", "before"]
        } else if self.queued_tree_share == QueuedTreeShare::Compare {
            vec![
                "before", "current", "resumed", "resumed", "current", "before",
            ]
        } else {
            order(&self.mode)
        };
        if self.warmup {
            takes.insert(0, "warmup");
        }
        takes
    }

    fn take(&self, mode: &str) -> Result<Self, String> {
        let control = (self.mode == "series" && matches!(mode, "shared" | "tree"))
            || (self.mode == "matched" && matches!(mode, "before" | "warmup"));
        let verify = if control {
            self.before
                .as_ref()
                .ok_or("the control take requires --before-verify PATH")?
        } else {
            &self.verify
        };
        Ok(Self {
            verify: verify.clone(),
            before: None,
            mode: if self.mode == "matched" {
                "chain"
            } else {
                mode
            }
            .to_owned(),
            tree_work: self.tree_work,
            export_seconds: self.export_seconds,
            real_export: self.real_export,
            planted_red: self.planted_red && mode != "warmup",
            warmup: false,
            warm_slices: self.warm_slices,
            cold_tree: self.cold_tree,
            build_return: self.build_return.for_take(mode),
            queued_tree_share: self.queued_tree_share.for_take(mode),
            cores: self.cores,
        })
    }

    fn requested_cores(&self, host: u32) -> Result<u32, String> {
        let requested = self.cores.unwrap_or(host);
        if requested > host {
            return Err(format!(
                "requested {requested} cores on a {host}-core host; no take started"
            ));
        }
        Ok(requested)
    }

    fn require_grant(&self, requested: u32, granted: u32) -> Result<(), String> {
        require_series_grant(&self.mode, requested, granted)?;
        if self.cores.is_some() && requested != granted {
            return Err(format!(
                "requested a fixed {requested}-core pool, got {granted}; no take started"
            ));
        }
        Ok(())
    }

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
                "directory":ledger.dir(),
                "records":records.into_iter().map(|record| aterm_json::json!({
                    "pid":record.pid,
                    "start_ms":record.start_ms,
                    "seq":record.seq,
                    "what":record.need.what,
                    "rank":format!("{:?}",record.need.rank),
                    "mem":record.need.mem,
                    "mem_now":record.need.mem_now(),
                    "margin":record.need.margin,
                    "requested_cores":record.need.cores,
                    "granted_cores":record.held.map(|held|held.cores),
                    "asked_alone":record.need.alone,
                    "held_alone":record.held.map(|held|held.alone),
                    "held":record.held.is_some(),
                    "admitted_ms":record.held.map(|held|held.admitted_ms),
                })).collect::<Vec<_>>()
            }),
            Err(error) => {
                aterm_json::json!({"error":error.to_string(),"directory":ledger.dir(),"records":null})
            }
        },
        None => aterm_json::json!({"error":"no capacity ledger","directory":null,"records":null}),
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
    environment: Option<&aterm_verify::EnvSnapshot>,
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
                .arg("--log").arg(root.join(format!("tree-{check}.log")))
                .arg("--events").arg(root.join(format!("tree-{check}.events")));
            if !opts.real_export {
                cmd.env("CARGO_TARGET_DIR", root.join(format!("tree-{check}.noindex")));
            } else {
                apply_real_tree_environment(
                    &mut cmd,
                    environment.ok_or("real tree has no captured release environment")?,
                );
            }
            if root.join("test-history.json").is_file() {
                cmd.arg("--test-history").arg(root.join("test-history.json"));
            }
            if opts.planted_red {
                cmd.env("ATERM_QUALIFY_RED_MARKER", root.join("planted-red.json"));
            } else {
                cmd.env_remove("ATERM_QUALIFY_RED_MARKER");
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
            let expected_red = opts.planted_red && usage.status.code() == Some(1)
                && planted_red_only(root, &root.join(format!("tree-{check}.events")));
            let row = aterm_json::json!({"check":check,"wall_s":started.elapsed().as_secs_f64(),"cpu_s":usage.cpu.as_secs_f64(),"success":usage.status.success(),"expected_red":expected_red});
            crate::durable::write_fresh(&root.join(format!("tree-{check}.result.json")), row.to_string())
                .map_err(|e| e.to_string())?;
            if (opts.planted_red && !expected_red) || (!opts.planted_red && !usage.status.success()) {
                return Err(format!("pilot tree {check}: {row}"));
            }
            Ok(row)
        })}).collect();
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "pilot tree panicked".to_owned())?)
            .collect()
    })
}

fn planted_red_only(root: &Path, events: &Path) -> bool {
    let marker = std::fs::read_to_string(root.join("planted-red.json"))
        .ok()
        .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok());
    if !marker.is_some_and(|marker| {
        marker["test"].as_str() == Some("qualify_pilot::tests::planted_red_measurement_control")
    }) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(events) else {
        return false;
    };
    let parsed: Result<Vec<aterm_json::Value>, _> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap_or(line)))
        .collect();
    let Ok(events) = parsed else {
        return false;
    };
    if events
        .iter()
        .filter(|row| {
            row["event"].as_str() == Some("run-finished") && row["code"].as_u64() == Some(1)
        })
        .count()
        != 1
    {
        return false;
    }
    let rows: Vec<_> = events
        .iter()
        .filter(|row| {
            row["event"].as_str() == Some("check-finished")
                && row["outcome"].as_str() == Some("FAIL")
        })
        .collect();
    rows.len() == 1
        && rows[0]["check"].as_str() == Some("test")
        && rows[0]["findings_total"].as_u64() == Some(1)
        && rows[0]["findings"].as_array().is_some_and(|findings| {
            findings.len() == 1
                && findings[0].as_str().is_some_and(|finding| {
                finding == "-p aterm-release --test it -- qualify_pilot::tests::planted_red_measurement_control"
                })
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
    if opts.real_export
        && opts.mode != "export"
        && crate::slice_units::live_shared_floor(2) == u64::MAX
    {
        return Err("real gate measurements need complete slice history; calibrate with --mode alone --slice-cache warm first".into());
    }
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
                let cutter = crate::release_tools::stage_cutter(
                    &release,
                    commit.trim(),
                    &image,
                    &release.join("qualify-freeze.log"),
                    None,
                )?;
                crate::release_tools::publish_cutter(&release, &cutter)?;
                let tools = crate::release_tools::stage(
                    &release,
                    commit.trim(),
                    &cutter.tool.path,
                    &opts.verify,
                )?;
                crate::release_tools::publish(&release, &tools)?;
                let proof = crate::release_tools::cutter_record_path(&release);
                let mut command = aterm_uds::spawnfd::command(&cutter.tool.path);
                command
                    .current_dir(repo)
                    .args(frozen_arguments(opts, &tools.verify.path))
                    .env(crate::release_tools::RECORD_ENV, proof);
                apply_real_tree_environment(
                    &mut command,
                    &crate::release_steps::verify_environment(),
                );
                let status = command.status().map_err(|error| error.to_string())?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("frozen qualify pilot exited {status}"))
                };
            }
        }
    }
    // Freeze the tools under their original build layout before moving the graph's
    // artifacts. The proved images then stay outside the target cleared between takes.
    if opts.cold_tree {
        let target = cold_tree_target(repo)?;
        let image = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut images = vec![image.clone(), opts.verify.clone()];
        images.extend(opts.before.iter().cloned());
        images_outside_target(repo, &target, &images)?;
        if std::env::var_os("CARGO_TARGET_DIR").as_deref() != Some(target.as_os_str())
            || [
                "CARGO_BUILD_TARGET_DIR",
                "CARGO_BUILD_BUILD_DIR",
                "CARGO_HOME",
            ]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            let mut command = aterm_uds::spawnfd::command(image);
            command
                .current_dir(repo)
                .args(frozen_arguments(opts, &opts.verify));
            let mut environment = crate::release_steps::verify_environment();
            environment.cargo_target_dir = Some(target);
            apply_real_tree_environment(&mut command, &environment);
            let status = command.status().map_err(|error| error.to_string())?;
            return if status.success() {
                Ok(())
            } else {
                Err(format!("cold-target qualify pilot exited {status}"))
            };
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
    let requested_cores = opts.requested_cores(aterm_capacity::cores())?;
    let claim = aterm_capacity::admit(
        aterm_capacity::Need::new("qualify-pilot core pool")
            .rank(aterm_capacity::Rank::Release)
            .cores(requested_cores)
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
    opts.require_grant(requested_cores, claim.cores())?;
    let modes = opts.order();
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
        let options = opts.take(mode)?;
        let take = aterm_json::json!({
            "source":commit.trim(), "arm":mode, "measured":*mode != "warmup",
            "slots":claim.cores(), "verify":options.verify,
            "planted_red":options.planted_red, "build_return":options.build_return.as_str(),
            "queued_tree_share":options.queued_tree_share.as_str(),
        });
        crate::durable::write_fresh(&at.join("take.json"), take.to_string())
            .map_err(|error| error.to_string())?;
        if *mode == "fixtures" {
            let result = crate::release_steps::measure_local_fixtures(
                repo,
                &at,
                commit.trim(),
                &std::sync::atomic::AtomicBool::new(true),
                None,
            )?;
            println!("qualify-fixtures-result: {result}");
        } else if *mode == "export" {
            let result = crate::release_steps::measure_local_export(
                repo,
                &at,
                commit.trim(),
                claim.cores(),
                None,
            )?;
            println!("qualify-export-result: {result}");
            if result["success"].as_bool() != Some(true) {
                return Err(format!("local full export did not pass: {result}"));
            }
        } else {
            let environment = opts
                .real_export
                .then(|| {
                    let mut environment = crate::release_steps::verify_environment();
                    isolate_side_lanes(&at, &mut environment)?;
                    if opts.cold_tree {
                        environment.cargo_target_dir = Some(cold_tree_target(repo)?);
                    }
                    Ok::<_, String>(environment)
                })
                .transpose()?;
            let target = environment
                .as_ref()
                .map(|environment| prepare_real_tree(repo, opts, environment))
                .transpose()?;
            let result = one(
                repo,
                &at,
                &options,
                claim.cores(),
                commit.trim(),
                environment.as_ref(),
                mode,
            );
            if opts.cold_tree {
                // The next take clears the native target. Preserve the complete compiler
                // index beside this take's verdicts before discarding its artifacts.
                let index = target
                    .as_ref()
                    .ok_or("cold tree has no captured target")?
                    .join("verify/test-executables.json");
                match crate::durable::read_text(&index) {
                    Ok(index) => {
                        crate::durable::write_fresh(&at.join("test-executables.json"), index)
                            .map_err(|error| error.to_string())?
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound && result.is_err() => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            result?;
        }
        crate::release_tools::clean_at(repo, commit.trim())?;
    }
    Ok(())
}

fn apply_real_tree_environment(
    command: &mut aterm_uds::spawnfd::Command,
    environment: &aterm_verify::EnvSnapshot,
) {
    environment.apply_cargo_selectors(command);
    command.env("HOME", &environment.home);
    command.env_remove(aterm_verify::SIDE_LANES_ENV);
    if let Some(dir) = &environment.side_lanes_dir {
        command.env(aterm_verify::SIDE_LANES_ENV, dir);
    }
}

/// Cold side targets need no authority to delete a previous build or its live mappings.
/// The native target stays fixed so its actual Cargo freshness can be compared.
fn isolate_side_lanes(
    at: &Path,
    environment: &mut aterm_verify::EnvSnapshot,
) -> Result<(), String> {
    let side = at.join("tree-lanes.noindex");
    std::fs::create_dir(&side).map_err(|error| {
        format!(
            "create fresh verification side targets {}: {error}",
            side.display()
        )
    })?;
    environment.side_lanes_dir = Some(side);
    Ok(())
}

fn cold_tree_target(repo: &Path) -> Result<PathBuf, String> {
    let target = crate::snapshot::CacheRoot::for_repo(repo)?
        .path()
        .join(".ubd/qualify-native.noindex");
    Ok(aterm_verify::physical(&target))
}

/// The private target may be cleared only while every measurement image lives elsewhere.
/// Resolve symlinks before the containment test; relative image paths belong to the repo.
fn images_outside_target(repo: &Path, target: &Path, images: &[PathBuf]) -> Result<(), String> {
    let target = match std::fs::canonicalize(target) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::path::absolute(target).map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    for image in images {
        let image = if image.is_absolute() {
            image.clone()
        } else {
            repo.join(image)
        };
        let image = std::fs::canonicalize(&image)
            .map_err(|error| format!("resolve measurement image {}: {error}", image.display()))?;
        if image.starts_with(&target) {
            return Err(format!(
                "measurement image {} is inside the cold native target {}; run the tools from outside it",
                image.display(),
                target.display()
            ));
        }
    }
    Ok(())
}

fn prepare_real_tree(
    repo: &Path,
    opts: &Options,
    environment: &aterm_verify::EnvSnapshot,
) -> Result<PathBuf, String> {
    let repo = aterm_verify::physical(repo);
    let plan = aterm_verify::NativeBuildPlan::capture(environment, &repo)
        .map_err(|error| error.to_string())?;
    let ctx = aterm_verify::Ctx::new(
        repo.clone(),
        aterm_verify::Scope::workspace(),
        environment.clone(),
        repo.join("target/qualify-prepare"),
    );
    if ctx.target_dir.as_path() != plan.target()
        || ctx.main_build_dir.as_deref() != plan.build_dir()
    {
        return Err("real tree layout changed while capturing its cleanup context".into());
    }
    if opts.cold_tree {
        let target = cold_tree_target(&repo)?;
        let main = aterm_verify::plan::lane_dir(&ctx, aterm_verify::plan::Lane::MainTarget)
            .ok_or("the full tree has no native target")?;
        if main != target {
            return Err("the cold native target was not handed to the measurement process".into());
        }
        if ctx.cached_lanes_dir.is_some()
            || aterm_verify::fallback_build_dir(&ctx.env, &repo, &target).is_none()
        {
            return Err("cold native measurements require intermediates inside the owned target; an active Cargo build-directory configuration moves them elsewhere".into());
        }
        let mut images = vec![
            std::env::current_exe().map_err(|error| error.to_string())?,
            opts.verify.clone(),
        ];
        images.extend(opts.before.iter().cloned());
        let oracle = aterm_verify::plan::lane_dir(&ctx, aterm_verify::plan::Lane::LibcOracleTarget)
            .ok_or("the full tree has no libc oracle target")?;
        images_outside_target(&repo, &target, &images)?;
        images_outside_target(&repo, &oracle, &images)?;
        if target.exists()
            && !atpkg::build_cache::remove_unbuilt(&target).map_err(|error| error.to_string())?
        {
            return Err("the owned native target is in use; no matched take started".into());
        }
        if oracle.exists()
            && !atpkg::build_cache::remove_unbuilt(&oracle).map_err(|error| error.to_string())?
        {
            return Err("the owned libc oracle target is in use; no matched take started".into());
        }
    }
    let lint = aterm_verify::plan::lane_dir(&ctx, aterm_verify::plan::Lane::TippyTarget)
        .ok_or("the full tree has no lint lane")?;
    if environment.side_lanes_dir.is_some() {
        if lint.exists() {
            return Err(
                "the selected cold lint target already exists; no matched take started".into(),
            );
        }
    } else if lint.exists() {
        return Err(
            "real measurements require fresh isolated side targets; no matched take started".into(),
        );
    }
    let index = ctx.target_dir.join("verify/test-executables.json");
    match std::fs::remove_file(index) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    plan.prepare().map_err(|error| error.to_string())?;
    Ok(ctx.target_dir)
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
        "--build-return".into(),
        opts.build_return.as_str().into(),
        "--queued-tree-share".into(),
        opts.queued_tree_share.as_str().into(),
    ];
    if let Some(before) = &opts.before {
        let before = if opts.build_return == BuildReturn::Matched
            || matches!(
                opts.queued_tree_share,
                QueuedTreeShare::Matched | QueuedTreeShare::LendingMatched
            ) {
            verify
        } else {
            before
        };
        args.extend(["--before-verify".into(), before.as_os_str().to_owned()]);
    }
    if opts.real_export {
        args.extend(["--export-work".into(), "real".into()]);
    }
    if opts.planted_red {
        args.extend(["--plant-red".into(), "test".into()]);
    }
    if opts.warmup {
        args.extend(["--warmup".into(), "full".into()]);
    }
    args.extend([
        "--slice-cache".into(),
        if opts.warm_slices { "warm" } else { "cold" }.into(),
        "--tree-cache".into(),
        if opts.cold_tree { "cold" } else { "warm" }.into(),
    ]);
    if let Some(seconds) = opts.export_seconds {
        args.extend(["--export-seconds".into(), seconds.to_string().into()]);
    }
    if let Some(cores) = opts.cores {
        args.extend(["--cores".into(), cores.to_string().into()]);
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
    opts: &Options,
    shares: &aterm_capacity::BranchShares,
) -> Result<aterm_uds::spawnfd::Command, String> {
    let mut command = aterm_uds::spawnfd::command(image);
    command
        .arg(SLICE_CHILD_FLAG)
        .arg(repo)
        .arg(root)
        .arg(commit)
        .arg(if opts.mode == "tree" {
            "demoted"
        } else {
            "normal"
        })
        .arg(if opts.warm_slices { "warm" } else { "cold" })
        .arg(if opts.real_export { "full" } else { "slices" })
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

trait PilotSliceProcess {
    fn owns_unreaped(&self) -> bool;
    fn kill_for_custody(&mut self) -> std::io::Result<()>;
    fn reap_for_custody(&mut self) -> std::io::Result<()>;
}

impl PilotSliceProcess for aterm_uds::spawnfd::Child {
    fn owns_unreaped(&self) -> bool {
        aterm_uds::spawnfd::Child::owns_unreaped(self)
    }

    fn kill_for_custody(&mut self) -> std::io::Result<()> {
        aterm_uds::spawnfd::Child::kill(self)
    }

    fn reap_for_custody(&mut self) -> std::io::Result<()> {
        aterm_uds::spawnfd::Child::wait(self).map(|_| ())
    }
}

/// Keeps the actual slice child owned across every early return from the pilot.
struct PilotSliceChild<Process: PilotSliceProcess>(Process);

impl<Process: PilotSliceProcess> PilotSliceChild<Process> {
    fn new(process: Process) -> Self {
        Self(process)
    }
}

impl<Process: PilotSliceProcess> std::ops::Deref for PilotSliceChild<Process> {
    type Target = Process;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<Process: PilotSliceProcess> std::ops::DerefMut for PilotSliceChild<Process> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<Process: PilotSliceProcess> Drop for PilotSliceChild<Process> {
    fn drop(&mut self) {
        if self.0.owns_unreaped() {
            let _ = self.0.kill_for_custody();
            let _ = self.0.reap_for_custody();
        }
    }
}

fn ready_shape(event: &aterm_json::Value, minimum: u64) -> bool {
    let label = match event["mode"].as_str() {
        Some("shared") => "release slice memory budget",
        Some("serial") => crate::slice_units::SERIAL_PARENT,
        Some("cold") => crate::slice_units::COLD_PARENT,
        _ => return false,
    };
    let Some(mem) = event["mem"].as_u64() else {
        return false;
    };
    event["event"].as_str() == Some("slice-memory-ready")
        && event["held"].as_bool() == Some(true)
        && event["label"].as_str() == Some(label)
        && if event["mode"].as_str() == Some("cold") {
            mem == minimum
        } else {
            mem >= minimum
        }
}

fn ready_record_matches(
    event: &aterm_json::Value,
    child_pid: u32,
    alone: bool,
    record: &aterm_capacity::Record,
) -> bool {
    let identity = &event["claim"];
    record.pid == child_pid
        && identity["pid"].as_u64() == Some(u64::from(child_pid))
        && identity["start_ms"].as_u64() == Some(record.start_ms)
        && identity["seq"].as_u64() == Some(record.seq)
        && event["label"].as_str() == Some(record.need.what.as_str())
        && event["mem"].as_u64() == Some(record.need.mem)
        && record.accounting == aterm_capacity::Accounting::Holder
        && record.need.rank == aterm_capacity::Rank::Release
        && record.need.margin == 1
        && record.need.alone == alone
        && record.held.is_some_and(|held| held.alone == alone)
}

fn ready_claim_is_live(event: &aterm_json::Value, child_pid: u32) -> Result<bool, String> {
    if event["event"].as_str() != Some("slice-memory-ready")
        || event["held"].as_bool() != Some(true)
    {
        return Err(format!("slice child reported no held memory gate: {event}"));
    }
    let shared = crate::slice_units::live_shared_floor(2);
    let (minimum, alone) = match event["mode"].as_str() {
        Some("shared") => (shared, false),
        Some("serial") => (
            crate::slice_units::serial_parent_bytes(
                shared.saturating_sub(2 * crate::slice_units::DRIVER),
            ),
            false,
        ),
        Some("cold") => (crate::slice_units::DRIVER, true),
        _ => return Err(format!("slice child memory gate has unknown mode: {event}")),
    };
    if !ready_shape(event, minimum) {
        return Err(format!(
            "slice child memory gate has the wrong label or reservation: {event}"
        ));
    }
    let identities = [event];
    let ledger = aterm_capacity::Ledger::user().ok_or("no user capacity ledger")?;
    let matches = |records: &[aterm_capacity::Record]| {
        identities.iter().all(|claim| {
            records
                .iter()
                .any(|record| ready_record_matches(claim, child_pid, alone, record))
        })
    };
    // Two complete scans reject a directory read that straddled a claim's removal.
    let first = ledger.list().map_err(|error| error.to_string())?;
    let second = ledger.list().map_err(|error| error.to_string())?;
    Ok(matches(&first) && matches(&second))
}

fn serial_before_tree<T>(
    serial: bool,
    release: impl FnOnce() -> Result<(), String>,
    completed: impl FnOnce() -> Result<T, String>,
    drained: impl FnOnce(),
) -> Result<Option<T>, String> {
    if !serial {
        return Ok(None);
    }
    release()?;
    let result = completed()?;
    drained();
    Ok(Some(result))
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

struct TreeMemoryReady {
    report: aterm_json::Value,
    shared: Option<crate::release_steps::SharedTreeMemory>,
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
) -> Result<TreeMemoryReady, String> {
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
                return Ok(TreeMemoryReady {
                    shared: crate::release_steps::SharedTreeMemory::new(&claim, state),
                    report: aterm_json::json!({
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
                    }),
                });
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

fn one(
    repo: &Path,
    root: &Path,
    opts: &Options,
    slots: u32,
    commit: &str,
    environment: Option<&aterm_verify::EnvSnapshot>,
    arm: &str,
) -> Result<(), String> {
    let pid = std::process::id();
    let cpu_before = aterm_sysprobe::jobs::cpu_with_reaped_ns(pid);
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
    let started_at_unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs_f64());
    let export_seconds = opts.export_seconds();
    let export_share = crate::release_steps::export_share(slots, export_seconds.is_some(), true);
    let shares = aterm_capacity::BranchShares::new(
        &root.join("qualify.jobs"),
        slots,
        export_share,
        [true, opts.mode != "alone"],
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
    let export_client = std::sync::OnceLock::new();
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
    let (
        slices,
        tree,
        export,
        fixtures,
        serial_export,
        graph_wall_s,
        memory_ready,
        tree_memory_ready,
        tree_cpu_transfers,
        build_usage,
        appkit,
    ) = std::thread::scope(|scope| -> Result<_, String> {
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

        // The common series reservation holds the matched grant throughout each take.
        // AppKit adopts no branch pool: its production --admit now command runs alone,
        // before either slice memory parent or the tree starts.
        let appkit = opts
            .real_export
            .then(|| crate::release_steps::measure_local_appkit(repo, &root.join("appkit"), commit))
            .transpose()?;
        if opts.real_export {
            let client = crate::release_steps::prepare_local_export_client(
                repo,
                &root.join("preparation"),
                commit,
                slots,
            )?;
            export_client
                .set(client)
                .map_err(|_| "the export client was already prepared".to_owned())?;
        }
        let client = export_client.get();

        let image = std::env::current_exe()
            .map_err(|error| format!("find qualify pilot executable: {error}"))?;
        let build_finished = FinishPilotShare {
            shares: &shares,
            branch: 0,
            admission_barrier: Some(&barrier),
        };
        let mut command = slice_child_command(&image, repo, root, commit, opts, &shares)?;
        let stderr = crate::durable::create_fresh(&root.join("slice-child.stderr"))
            .map_err(|error| error.to_string())?;
        let transcript = crate::durable::create_fresh(&root.join("slice-child.stdout"))
            .map_err(|error| error.to_string())?;
        command
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(stderr);
        let mut child = PilotSliceChild::new(
            command
                .spawn()
                .map_err(|error| format!("start qualify pilot slice child: {error}"))?,
        );
        let child_pid = child.id();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("qualify pilot slice child has no output pipe".into());
        };
        let (send_event, events) = std::sync::mpsc::channel();
        let relay = scope.spawn(move || relay_slice_child_events(stdout, transcript, send_event));
        let prepared = next_slice_child_event(&events, &mut child, "preparation")?;
        if prepared["event"].as_str() != Some("prepared") {
            return Err(format!(
                "slice child did not prepare its sealed source: {prepared}"
            ));
        }
        let tree_ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tree_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tree_started, tree_pid) = std::sync::mpsc::channel();
        let admission = (barrier_slots > 0 || opts.mode == "tree").then_some(TreeAdmissionSignal {
            started: tree_started,
            ended: std::sync::Arc::clone(&tree_ended),
            stop: std::sync::Arc::clone(&tree_stop),
        });
        let serial_ended = std::sync::Mutex::new(None);
        let (memory_ready, tree, old_order_tree_pid) = start_tree_around_memory(
            &opts.mode,
            || {
                let mut ready = next_slice_child_event(&events, &mut child, "memory admission")?;
                if !ready_claim_is_live(&ready, child_pid)? {
                    return Err("slice child memory gate was not durably live".into());
                }
                let serial_start = started.elapsed().as_secs_f64();
                let ended = serial_before_tree(
                    barrier_slots > 0 && matches!(ready["mode"].as_str(), Some("serial" | "cold")),
                    || {
                        if let Some(reservation) = barrier
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            reservation
                                .release_to_build()
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(())
                    },
                    || {
                        let ended =
                            next_slice_child_event(&events, &mut child, "serial slice results")?;
                        if ended["event"].as_str() != Some("ended") {
                            return Err(format!("serial slices did not drain: {ended}"));
                        }
                        Ok(ended)
                    },
                    || opts.build_return.drain(&shares),
                )?;
                if let Some(ended) = ended {
                    let details = ready
                        .as_object_mut()
                        .ok_or("slice admission is not an object")?;
                    details.insert("serial_build_start_s".into(), serial_start.into());
                    details.insert(
                        "serial_build_end_s".into(),
                        started.elapsed().as_secs_f64().into(),
                    );
                    *serial_ended
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ended);
                }
                Ok(ready)
            },
            || {
                scope.spawn(|| {
                    tree(
                        repo,
                        root,
                        opts,
                        &shares,
                        Some(&barrier),
                        admission,
                        environment,
                    )
                })
            },
            || {
                tree_pid
                    .recv()
                    .map_err(|_| "release tree ended before reporting its process".to_owned())
            },
        )?;
        let (tree_memory_ready, shared_tree) = if barrier_slots > 0 {
            let tree_pid = match old_order_tree_pid {
                Some(pid) => pid,
                None => tree_pid
                    .recv()
                    .map_err(|_| "release tree ended before reporting its process".to_owned())?,
            };
            let TreeMemoryReady {
                report: ready,
                shared,
            } = await_tree_memory(tree_pid, &tree_ended, barrier_slots)?;
            if ready["queued"].as_bool() == Some(true) || ready["invalid"].as_bool() == Some(true) {
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
            (ready, shared)
        } else {
            (
                aterm_json::json!({
                    "event":"tree-memory-ready",
                    "held":false,
                    "mode":if opts.mode == "tree" { "tree-first-probe" } else { "not-release-shaped" },
                    "barrier_slots":0,
                }),
                None,
            )
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
                        client,
                    )
                } else {
                    Ok(export_surrogate(shares, export_share, seconds))
                }
            })
        });
        let tree_cpu_transfers = if matches!(
            opts.queued_tree_share,
            QueuedTreeShare::Resume | QueuedTreeShare::Lend
        ) {
            shared_tree.map(|queued| {
                let ended = std::sync::Arc::clone(&tree_ended);
                let shares = &shares;
                let policy = opts.queued_tree_share;
                scope.spawn(move || {
                    let ledger = aterm_capacity::Ledger::user()?;
                    let finished = || {
                        ended.load(std::sync::atomic::Ordering::SeqCst)
                            || crate::interrupt::caught().is_some()
                    };
                    let held = || queued.held(&ledger);
                    let pause = || std::thread::sleep(Duration::from_secs(1));
                    if policy == QueuedTreeShare::Lend {
                        crate::release_steps::await_tree_cpu_lending(
                            shares, &queued, &finished, held, pause,
                        )
                    } else {
                        crate::release_steps::await_tree_cpu_resumption(
                            shares, &queued, &finished, held, pause,
                        )
                    }
                })
            })
        } else {
            None
        };
        let prefetched = serial_ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let ended = match prefetched {
            Some(ended) => ended,
            None => next_slice_child_event(&events, &mut child, "slice results")?,
        };
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
        if opts.real_export {
            require_unsigned_build(&ended, commit)?;
        }
        let fixtures = if opts.real_export {
            opts.build_return.drain(&shares);
            Some(crate::release_steps::measure_local_fixtures(
                repo,
                &root.join("fixtures"),
                commit,
                &tree_ended,
                (opts.build_return == BuildReturn::Early).then_some(&shares),
            )?)
        } else {
            None
        };
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
        let tree_cpu_transfers = tree_cpu_transfers
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "pilot tree CPU observer panicked".to_owned())
            })
            .transpose()?
            .flatten();
        let graph_wall_s = started.elapsed().as_secs_f64();
        let serial_export = if opts.real_export
            && !opts.planted_red
            && export
                .as_ref()
                .is_some_and(|value| value["success"].as_bool() == Some(true))
        {
            Some(crate::release_steps::measure_local_export(
                repo,
                &root.join("serial-export"),
                commit,
                slots,
                client,
            )?)
        } else {
            None
        };
        drop(done);
        sampler.join().map_err(|_| "pilot sampler panicked")??;
        Ok((
            slices,
            tree,
            export,
            fixtures,
            serial_export,
            graph_wall_s,
            memory_ready,
            tree_memory_ready,
            tree_cpu_transfers,
            aterm_json::json!({
                "pid":child_pid,
                "cpu_s":build_usage.cpu.as_secs_f64(),
                "peak_bytes":build_usage.maxrss,
                "preparation":prepared,
                "postlink":ended["postlink"],
            }),
            appkit,
        ))
    })?;
    let (verify_sha256, _) = crate::dmg::sha256_and_size(&opts.verify)?;
    let marker = std::fs::read_to_string(root.join("planted-red.json"))
        .ok()
        .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok());
    let planted_failure_marker_s = marker
        .as_ref()
        .and_then(|marker| marker["at_unix_ns"].as_str())
        .and_then(|stamp| stamp.parse::<u128>().ok())
        .zip(started_at_unix_s)
        .map(|(stamp, start)| stamp as f64 / 1e9 - start)
        .filter(|elapsed| *elapsed >= 0.0);
    // Every branch child has been reaped and the sampler has joined. A single kernel delta
    // counts the coordinator and reaped descendants once despite overlapping branch timers.
    let cpu_s = cpu_before
        .zip(aterm_sysprobe::jobs::cpu_with_reaped_ns(pid))
        .and_then(|(before, after)| after.checked_sub(before))
        .map(|ns| ns as f64 / 1e9);
    let report = aterm_json::json!({
        "mode":opts.mode, "tree_work":opts.tree_work.as_str(),
        "build_return":opts.build_return.as_str(),
        "queued_tree_share":opts.queued_tree_share.as_str(),
        "tree_cache":if opts.cold_tree {"cold"} else {"warm"},
        "slice_cache":if opts.warm_slices {"warm"} else {"cold"},
        "source":commit, "slots":slots, "started_at_unix_s":started_at_unix_s,
        "arm":arm, "measured":arm != "warmup",
        "wall_s":started.elapsed().as_secs_f64(), "cpu_s":cpu_s,
        "cpu_scope":"coordinator-and-reaped-descendants",
        "preparation":export_client.get().map(crate::release_steps::LocalExportClient::record),
        "appkit":appkit,
        "graph_wall_s":graph_wall_s, "slices":slices, "tree":tree, "export":export,
        "fixtures":fixtures, "serial_export":serial_export, "planted_red":marker,
        "planted_failure_marker_s":planted_failure_marker_s, "serial_staging_push_s":null,
        "export_share":export_share,
        "branch_fifos":{"build":shares.pool(false).path(),"tree":shares.pool(true).path()},
        "slice_memory_ready":memory_ready, "tree_memory_ready":tree_memory_ready,
        "tree_cpu_transfers":tree_cpu_transfers,
        "build_child":build_usage, "verify_sha256":verify_sha256, "verify":opts.verify,
    });
    crate::durable::write_fresh(&root.join("result.json"), report.to_string())
        .map_err(|e| e.to_string())?;
    let prefix = if arm == "warmup" {
        "qualify-pilot-warmup-result"
    } else {
        "qualify-pilot-result"
    };
    println!("{prefix}: {report}");
    if opts.planted_red && planted_failure_marker_s.is_none() {
        return Err("the expected red has no measured failure timestamp".into());
    }
    if opts.real_export && report["export"]["success"].as_bool() != Some(true) {
        return Err("the full export failed; the take is not a passing gate".into());
    }
    if opts.real_export
        && !opts.planted_red
        && report["serial_export"]["success"].as_bool() != Some(true)
    {
        return Err("the serial local revalidation failed; the take is not a passing gate".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "qualify_pilot_tests.rs"]
mod tests;
