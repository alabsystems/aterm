// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Idle-capacity main sampling. The controller renews a remote lease while a disposable
//! process group judges one immutable tree with that tree's tools. Only journal records leave it.

use crate::judged::{self, Decision, Record, Row};
use aterm_uds::spawnfd::Tether as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "watch_census.rs"]
mod census;
#[path = "watch_service.rs"]
mod service;

const HELP: &str = "verify watch [--workspace DIR] [--remote NAME] [--branch BRANCH] [--once] [--interval SECONDS]\n\
verify watch --census REF [--since UNIX_SECONDS] reads states and causes from a fetched journal.\n\
With --since, count attempts from history, including overwritten cancellations, and emit JSON records.\n\
verify watch --install [--workspace DIR] [--interval SECONDS] installs a persistent macOS main lane.\n\
Samples origin/main in a dedicated persistent clone at Background rank. Claims and verdicts\n\
are appended to refs/aterm/judged (14 days of records). A release ends the sample; Ordinary\n\
work retains priority for overlapping resources. --once attempts one sample. No hook, gate or publication.\n";

/// Re-run a Background verifier as its own cancellable process group when its launcher did not
/// give it one. A candidate can be newer than a long-lived watch controller: the invariant
/// belongs to the candidate that asks for Background claims, not to the route that launched it.
///
/// `Some` is the re-run's exit code, or [`crate::exit::COULD_NOT_RUN`] when it could not start or
/// did not report one. The caller exits with it. `None` means this invocation already owns its
/// group, or is not a Background run.
#[must_use]
pub fn background_entry(args: &[std::ffi::OsString]) -> Option<i32> {
    background_entry_with(args, background_requested(args), true)
}

fn background_requested(args: &[std::ffi::OsString]) -> bool {
    args.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg.split_once('=').map_or(arg.as_ref(), |(flag, _)| flag) == "--background"
    })
}

fn background_entry_with(
    args: &[std::ffi::OsString],
    requested: bool,
    tether: bool,
) -> Option<i32> {
    // SAFETY: getpgrp has no pointer arguments and only reads this process's group.
    if !requested || unsafe { libc::getpgrp() } == std::process::id() as i32 {
        return None;
    }
    let run = || -> std::io::Result<std::process::ExitStatus> {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe()?);
        command.args(args).setsid(true);
        if tether {
            command.tether();
        }
        command.spawn()?.wait()
    };
    Some(match run() {
        Ok(status) => {
            if !status.success() {
                eprintln!("verify background child: {status}");
            }
            background_status(status)
        }
        Err(why) => {
            eprintln!("verify: --background could not enter its dedicated process group: {why}");
            crate::exit::COULD_NOT_RUN
        }
    })
}

fn background_status(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal))
            if signal == libc::SIGHUP || signal == libc::SIGINT || signal == libc::SIGTERM =>
        {
            75
        }
        (None, _) => crate::exit::COULD_NOT_RUN,
    }
}

#[derive(Clone, Debug)]
struct Options {
    workspace: PathBuf,
    remote: String,
    branch: String,
    once: bool,
    interval: Duration,
}

fn options(args: &[String], cwd: &Path) -> Result<Options, String> {
    let home = std::env::var_os("HOME").ok_or("watch needs HOME")?;
    let mut opts = Options {
        workspace: PathBuf::from(home).join(".local/state/aterm-verify/.ubd/watch/checkout"),
        remote: "origin".into(),
        branch: "main".into(),
        once: false,
        interval: Duration::from_secs(60),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--once" => opts.once = true,
            "--workspace" => {
                opts.workspace = cwd.join(it.next().ok_or("--workspace needs a directory")?)
            }
            "--remote" => opts.remote = it.next().ok_or("--remote needs a name")?.clone(),
            "--branch" => opts.branch = it.next().ok_or("--branch needs a branch")?.clone(),
            "--interval" => {
                let n: u64 = it
                    .next()
                    .ok_or("--interval needs seconds")?
                    .parse()
                    .map_err(|_| "invalid interval")?;
                if n == 0 {
                    return Err("interval must be positive".into());
                }
                opts.interval = Duration::from_secs(n);
            }
            other => return Err(format!("unknown watch option {other}")),
        }
    }
    if opts.remote.starts_with('-') || opts.branch.starts_with('-') {
        return Err("remote and branch cannot be options".into());
    }
    Ok(opts)
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    judged::git(root, args).map_err(|why| format!("git {}: {why}", args.join(" ")))
}

/// The lane controller. An internal sample bootstrap is always launched in its own process group;
/// it replaces itself with the verifier built from the sampled head before orchestration begins.
pub fn run(args: &[String], cwd: &Path) -> i32 {
    let _ = crate::report::running_driver();
    if args.first().is_some_and(|s| s == "--census") {
        return match census::run_args(cwd, &args[1..]) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!("verify watch census: {why}");
                3
            }
        };
    }
    if args.first().is_some_and(|s| s == "--install") {
        return match options(&args[1..], cwd).and_then(|o| service::install(&o, cwd)) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!("verify watch install: {why}");
                3
            }
        };
    }
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return 0;
    }
    if args.first().is_some_and(|s| s == "--sample") {
        return match args.get(1) {
            Some(result) => match sample(cwd, Path::new(result)) {
                Ok(true) => 0,
                Ok(false) => 75,
                Err(why) => {
                    eprintln!("watch sample: {why}");
                    3
                }
            },
            None => 2,
        };
    }
    if args.first().is_some_and(|s| s == "--sample-ready") {
        return match args.get(1) {
            Some(result) => match sample_ready(cwd, Path::new(result)) {
                Ok(true) => 0,
                Ok(false) => 75,
                Err(why) => {
                    eprintln!("watch sampled-head runner: {why}");
                    3
                }
            },
            None => 2,
        };
    }
    let opts = match options(args, cwd) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n{HELP}");
            return 2;
        }
    };
    match controller(&opts, cwd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("verify watch: COULD NOT RUN — {e}");
            3
        }
    }
}

fn controller(opts: &Options, cwd: &Path) -> Result<(), String> {
    let parent = opts.workspace.parent().ok_or("workspace has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // A host-wide lock, independent of the chosen checkout: at most one controller per Mac.
    let lock_path = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?)
        .join(".local/state/aterm-verify/watch.lock");
    fs::create_dir_all(lock_path.parent().ok_or("lock has no parent")?)
        .map_err(|e| e.to_string())?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    let _lock = aterm_toolchain::record_lock::RecordLock::try_lock(lock_file, 0)
        .map_err(|e| format!("another sample owns this Mac: {e}"))?;
    let mut opts = opts.clone();
    opts.workspace = prepare_workspace(&opts, cwd)?;
    let stop = aterm_toolchain::interrupt::Watch::enter();
    loop {
        if stop.caught().is_some() {
            return Ok(());
        }
        let result = attempt(&opts);
        if let Err(why) = &result {
            eprintln!("verify watch: sample could not run: {why}");
        }
        if opts.once {
            return result;
        }
        stop.nap(opts.interval, &|| false);
    }
}

fn prepare_workspace(opts: &Options, cwd: &Path) -> Result<PathBuf, String> {
    let source = git(cwd, &["remote", "get-url", &opts.remote])?;
    let source = if cwd.join(&source).exists() {
        fs::canonicalize(cwd.join(source))
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned()
    } else {
        source
    };
    if !opts.workspace.exists() {
        git(
            cwd,
            &[
                "clone",
                "--no-hardlinks",
                cwd.to_str().ok_or("source checkout is not UTF-8")?,
                opts.workspace.to_str().ok_or("workspace is not UTF-8")?,
            ],
        )?;
        // Seed independent objects locally; every sample still fetches the actual remote.
        // A full network clone of this repository can exceed the bounded Git call.
        git(&opts.workspace, &["remote", "set-url", "origin", &source])?;
        fs::write(
            opts.workspace.join(".git/verify-watch-owner"),
            source.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    let root = &opts.workspace;
    if fs::read_to_string(root.join(".git/verify-watch-owner"))
        .ok()
        .as_deref()
        != Some(source.as_str())
    {
        return Err("workspace was not created by verify watch; preserving that checkout".into());
    }
    if git(root, &["remote", "get-url", "origin"])? != source {
        return Err("watch workspace belongs to another remote".into());
    }
    fs::canonicalize(root).map_err(|e| e.to_string())
}

fn attempt(opts: &Options) -> Result<(), String> {
    let signal_from = aterm_uds::signal_origin::records().len();
    let root = &opts.workspace;
    if foreground_waiting(root)? {
        return Ok(());
    }
    // verify watch follows the invoking checkout's arbitrary remote, which may be private. URL
    // shape alone is no authority to classify this read as credential-less.
    git(root, &["fetch", "--quiet", "origin", &opts.branch])?;
    let commit = git(root, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    let tree = git(root, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    let host = aterm_uds::process::kernel_host_name().ok_or("host has no name")?;
    let started = now();
    let mut record = Record {
        commit: commit.clone(),
        tree,
        host,
        // No verifier has run yet. Only a sampled-head result may name one.
        verifier: String::new(),
        controller: format!(
            "sha256={} pid={} {}",
            crate::report::running_driver().unwrap_or("unavailable"),
            std::process::id(),
            aterm_uds::signal_origin::identity(std::process::id() as i32)
        ),
        inputs: String::new(),
        token: aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?,
        state: "judging".into(),
        cause: String::new(),
        started,
        updated: started,
        lease_until: started + judged::LEASE,
        rows: Vec::new(),
    };
    eprintln!(
        "watch controller: {}; tree={}",
        record.controller, record.tree
    );
    // The claim precedes all expensive work. A losing append must inspect the winning claim.
    loop {
        let until = judged::deadline();
        let head = judged::head(root, "origin", until)?;
        if judged::record(root, head.as_deref(), &record.tree, until)?
            .is_some_and(|r| r.occupied(now()))
        {
            return Ok(());
        }
        if judged::append(root, "origin", head.as_deref(), &record, until)? {
            break;
        }
    }
    let result = judge(root, &mut record);
    if let Err(why) = result {
        record.state = "could-not-run".into();
        record.cause = why.clone();
        record.rows.push(Row {
            id: "lane".into(),
            outcome: "could-not-run".into(),
            log: why,
            red_items: vec![],
            suspects: vec![],
            fixed_by: None,
            decisions: vec![],
        });
    }
    let signals: Vec<_> = aterm_uds::signal_origin::records()
        .into_iter()
        .skip(signal_from)
        .collect();
    if !signals.is_empty() {
        record.cause.push_str(&format!("\n{}", signals.join("\n")));
    }
    record.updated = now();
    record.lease_until = 0;
    fs::write(
        root.join(".git/watch-record.toml"),
        aterm_toml::to_string(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    publish_owned(root, &record, judged::deadline()).map_err(|e| e.to_string())?;
    println!(
        "verify watch: {} {} elapsed_s={} (journal {}) cause={:?}",
        record.state,
        record.commit,
        record.updated.saturating_sub(record.started),
        judged::REF,
        record.cause
    );
    Ok(())
}

#[derive(Debug)]
enum PublishError {
    Lost,
    Unavailable(String),
}

impl From<String> for PublishError {
    fn from(why: String) -> Self {
        Self::Unavailable(why)
    }
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost => f.write_str("sample lost its lease; no verdict was written"),
            Self::Unavailable(why) => f.write_str(why),
        }
    }
}

const STOP_GRACE: Duration = Duration::from_secs(20);
const LEASE_MARGIN: Duration = Duration::from_secs(30);
const _: () = assert!(LEASE_MARGIN.as_secs() > STOP_GRACE.as_secs());

/// Only a confirmed append extends the lease. Leave enough of it to stop the group.
struct Renewal {
    expires: Instant,
    epoch_until: u64,
    next: Instant,
    last_failure: Option<String>,
}

impl Renewal {
    fn new(expires: Instant, epoch_until: u64, now: Instant) -> Self {
        Self {
            expires,
            epoch_until,
            next: now + Duration::from_secs(30),
            last_failure: None,
        }
    }

    fn cutoff(&self) -> Instant {
        self.expires - LEASE_MARGIN
    }

    fn valid(&self, now: Instant, epoch: u64) -> bool {
        // Unix time covers sleep/forward jumps; Instant prevents backward jumps extending ownership.
        now < self.cutoff() && epoch.saturating_add(LEASE_MARGIN.as_secs()) < self.epoch_until
    }

    fn deadline(&self, now: Instant, epoch: u64) -> Instant {
        self.cutoff().min(
            now + Duration::from_secs(
                self.epoch_until
                    .saturating_sub(epoch.saturating_add(LEASE_MARGIN.as_secs())),
            ),
        )
    }

    fn finish(
        &mut self,
        result: Result<(), PublishError>,
        proposed: Instant,
        proposed_until: u64,
        now: Instant,
        epoch: u64,
    ) -> Result<(), PublishError> {
        match result {
            Ok(()) => {
                self.expires = proposed + Duration::from_secs(judged::LEASE);
                self.epoch_until = proposed_until;
                self.next = now + Duration::from_secs(30);
                Ok(())
            }
            Err(PublishError::Unavailable(why)) if self.valid(now, epoch) => {
                self.last_failure = Some(why);
                self.next = now + Duration::from_secs(5);
                Ok(())
            }
            Err(why) => Err(why),
        }
    }
}

fn publish_owned(root: &Path, record: &Record, until: Instant) -> Result<(), PublishError> {
    loop {
        let head = judged::head(root, "origin", until)?;
        if !judged::record(root, head.as_deref(), &record.tree, until)?
            .is_some_and(|r| r.token == record.token)
        {
            return Err(PublishError::Lost);
        }
        if judged::append(root, "origin", head.as_deref(), record, until)? {
            return Ok(());
        }
    }
}

fn sample_command(program: &Path) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(program);
    // launchd's session is zero. Tests exercise native terminal identities, so
    // the sample leads both its own session and its cancellable process group.
    cmd.setsid(true).tether();
    cmd
}

fn candidate_path(root: &Path) -> PathBuf {
    root.join(".ubd/target.noindex/debug")
        .join(format!("aterm-verify{}", std::env::consts::EXE_SUFFIX))
}

fn handoff_candidate(root: &Path) -> PathBuf {
    #[cfg(test)]
    if let Some(program) = std::env::var_os(SAMPLE_EXEC_TEST_CANDIDATE) {
        return program.into();
    }
    candidate_path(root)
}

struct SampleExec {
    program: PathBuf,
    cwd: PathBuf,
    args: Vec<std::ffi::OsString>,
    removed: Vec<String>,
    added: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl SampleExec {
    fn ready(root: &Path, result: &Path, stage2: &Path, path: &std::ffi::OsStr) -> Self {
        Self {
            program: handoff_candidate(root),
            cwd: root.to_owned(),
            args: vec![
                "watch".into(),
                "--sample-ready".into(),
                result.as_os_str().to_owned(),
            ],
            removed: sample_removed_env(),
            added: vec![
                ("TRUST_STAGE2_BIN".into(), stage2.as_os_str().to_owned()),
                ("PATH".into(), path.to_owned()),
                (
                    "CARGO_TARGET_DIR".into(),
                    root.join(".ubd/target.noindex").into_os_string(),
                ),
            ],
        }
    }

    fn exec(self) -> std::io::Error {
        let here = match std::env::current_dir().and_then(fs::canonicalize) {
            Ok(here) => here,
            Err(error) => return error,
        };
        let cwd = match fs::canonicalize(&self.cwd) {
            Ok(cwd) => cwd,
            Err(error) => return error,
        };
        if here != cwd {
            return std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "sample exec is in {}, expected {}",
                    here.display(),
                    cwd.display()
                ),
            );
        }
        let inherited: Vec<_> = std::env::vars_os()
            .filter(|(name, _)| {
                let name = name.to_string_lossy();
                !self.removed.iter().any(|removed| {
                    removed
                        .strip_suffix('*')
                        .map_or(name == removed.as_str(), |prefix| name.starts_with(prefix))
                })
            })
            .collect();
        let mut command = aterm_uds::spawnfd::transparent(&self.program);
        command.args(&self.args).env_exactly(inherited);
        for (name, value) in self.added {
            command.env(name, value);
        }
        command.exec()
    }
}

#[cfg(test)]
const SAMPLE_EXEC_TEST_CANDIDATE: &str = "ATERM_VERIFY_SAMPLE_EXEC_TEST_CANDIDATE";
#[cfg(test)]
const SAMPLE_EXEC_TEST_MODE: &str = "ATERM_VERIFY_SAMPLE_EXEC_TEST_MODE";
#[cfg(test)]
const SAMPLE_EXEC_TEST_NAME: &str =
    "watch::tests::the_sample_bootstrap_execs_the_sampled_head_image";

#[cfg(test)]
fn sample_test_handoff() -> Option<(PathBuf, std::ffi::OsString)> {
    std::env::var_os(SAMPLE_EXEC_TEST_CANDIDATE)?;
    Some((
        PathBuf::from("/sample-test-stage2"),
        std::env::var_os("PATH").unwrap_or_default(),
    ))
}

#[cfg(not(test))]
fn sample_test_handoff() -> Option<(PathBuf, std::ffi::OsString)> {
    None
}

#[cfg(test)]
fn configure_sample_test(mut next: SampleExec) -> SampleExec {
    next.args = ["--exact", SAMPLE_EXEC_TEST_NAME, "--nocapture"]
        .map(std::ffi::OsString::from)
        .into();
    next.added
        .push((SAMPLE_EXEC_TEST_MODE.into(), "candidate".into()));
    next
}

fn judge(root: &Path, record: &mut Record) -> Result<(), String> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("watch workspace is dirty; preserving its work".into());
    }
    git(root, &["checkout", "--quiet", "--detach", &record.commit])?;
    // vendor/astream is private. Its URL may be HTTPS, but only caller authority—not URL shape—
    // can say a read is anonymous.
    git(root, &["submodule", "update", "--init", "vendor/astream"])?;
    git(root, &["config", "submodule.recurse", "true"])?;
    record.inputs = source_inputs(root)?;
    record.updated = now();
    record.lease_until = now() + judged::LEASE;
    publish_owned(root, record, judged::deadline()).map_err(|e| e.to_string())?;
    let result = root.join(".git/watch-result.toml");
    let _ = fs::remove_file(&result);
    let log = root.join(".git/watch-sample.log");
    // This controller may be long-lived. It launches only the tiny bootstrap in its own image;
    // that bootstrap builds and execs this sampled head's verifier before any row is run.
    let mut cmd = sample_command(&std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.current_dir(root)
        .args(["watch", "--sample"])
        .arg(&result);
    let output = File::create(&log).map_err(|e| e.to_string())?;
    cmd.stderr(output.try_clone().map_err(|e| e.to_string())?)
        .stdout(output);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let began = Instant::now();
    let mut renewal = Renewal::new(
        began + Duration::from_secs(record.lease_until.saturating_sub(now())),
        record.lease_until,
        began,
    );
    let stopped = aterm_toolchain::interrupt::Watch::enter();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if let Some(signal) = stopped.caught() {
            stop_sample(&mut child)?;
            record.state = "cancelled".into();
            record.cause = format!(
                "controller signal={signal}; {}",
                aterm_uds::signal_origin::records().join("\n")
            );
            return Ok(());
        }
        let proposed = Instant::now();
        if !renewal.valid(proposed, now()) {
            let cleanup = stop_sample(&mut child).err();
            return Err(format!(
                "cleanup={cleanup:?}; could not renew the judging lease before its cleanup margin; controller={}; last failed transaction={}",
                record.controller,
                renewal.last_failure.as_deref().unwrap_or("none observed")
            ));
        }
        if proposed >= renewal.next {
            record.updated = now();
            record.lease_until = now() + judged::LEASE;
            let published = judged::interruptible(|| {
                publish_owned(root, record, renewal.deadline(proposed, now()))
            });
            if let Err(why) = renewal.finish(
                published,
                proposed,
                record.lease_until,
                Instant::now(),
                now(),
            ) {
                let _ = stop_sample(&mut child);
                return Err(format!(
                    "judging lease renewal: {why}; controller={}; previous failed transaction={}",
                    record.controller,
                    renewal.last_failure.as_deref().unwrap_or("none observed")
                ));
            }
        }
        stopped.nap(Duration::from_millis(100), &|| false);
    };
    if !status.success() {
        if status.code().is_none() || status.code() == Some(75) {
            cancel_record(
                record,
                status,
                &fs::read_to_string(&log).unwrap_or_default(),
            );
            return Ok(());
        }
        return Err(format!(
            "sample exited {status}: {}",
            row(
                "sample",
                status.code(),
                &fs::read_to_string(&log).unwrap_or_default()
            )
            .log
        ));
    }
    let text = fs::read_to_string(result).map_err(|error| error.to_string())?;
    accept_sample_result(&text, record)
}

/// The controller's root-free acceptance boundary. It can copy a sampled result, but has no
/// repository capability with which to reinterpret or re-attribute it.
fn accept_sample_result(text: &str, record: &mut Record) -> Result<(), String> {
    let sample = aterm_toml::from_str::<Sample>(text).map_err(|error| error.to_string())?;
    // The sampled-head image owns verifier identity, verdict, input and attribution semantics.
    // The long-lived controller owns only the lease and append; recomputing here would silently
    // restore its stale rules.
    record.state = sample.state;
    record.verifier = sample.verifier;
    record.inputs = sample.inputs;
    record.rows = sample.rows;
    record.cause = record
        .rows
        .iter()
        .filter(|row| row.outcome == "could-not-run")
        .map(|row| format!("{}: {}", row.id, row.log))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(())
}

fn cancel_record(record: &mut Record, status: std::process::ExitStatus, log: &str) {
    use std::os::unix::process::ExitStatusExt as _;
    let log = row("sample", Some(75), log).log;
    record.state = "cancelled".into();
    record.cause = match status.signal() {
        Some(signal) => {
            format!("sample signal={signal}; sender unavailable from wait status\n{log}")
        }
        None => format!("sample exit=75; refusal or cooperative stop\n{log}"),
    };
}

fn stop_sample(child: &mut aterm_uds::spawnfd::Child) -> Result<(), String> {
    let _ = child.terminate();
    let deadline = Instant::now() + STOP_GRACE;
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            aterm_uds::process::kill_group_until_empty(child.id() as i32)
                .map_err(|e| e.to_string())?;
            child.wait().map_err(|e| e.to_string())?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Sample {
    state: String,
    verifier: String,
    inputs: String,
    rows: Vec<Row>,
}

fn verdict(rows: &[Row]) -> &'static str {
    if rows.iter().any(|r| r.outcome == "red") {
        "red"
    } else if rows.len() == 6
        && [
            "inputs",
            "stable-x86_64",
            "tree",
            "export",
            "cut",
            "inputs-end",
        ]
        .iter()
        .all(|id| rows.iter().any(|r| r.id == *id && r.outcome == "pass"))
    {
        "pass"
    } else {
        "could-not-run"
    }
}

fn inherited_build_env(name: &str) -> bool {
    aterm_toolchain::release_env::clears(name)
        || aterm_toolchain::release_env::launcher_env(name)
        || name.starts_with("TRUST_TARGO_")
}

fn scrub(cmd: &mut aterm_uds::spawnfd::Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(inherited_build_env) {
            cmd.env_remove(name);
        }
    }
}

/// Whether a new sample must stay out of the lane until foreground work has entered it. Once a
/// sample owns a judging claim, its Background stage admissions retain their place behind
/// Ordinary work instead of discarding the judgment.
fn foreground_waiting(root: &Path) -> Result<bool, String> {
    pressure_waiting(root, SamplePhase::Starting)
}

/// Whether a Release or line must stop an in-flight judgment before its next stage. Ordinary
/// work is deliberately absent: capacity gives its overlapping resource reservations priority.
pub(crate) fn running_should_stop(root: &Path) -> Result<bool, String> {
    pressure_waiting(root, SamplePhase::Running)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SamplePhase {
    Starting,
    Running,
}

fn pressure_waiting(root: &Path, phase: SamplePhase) -> Result<bool, String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("no capacity ledger")?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let rows = ledger.list().map_err(|e| e.to_string())?;
    Ok(pressure_requires_stop(&root, &rows, phase))
}

fn pressure_requires_stop(
    root: &Path,
    rows: &[aterm_capacity::Record],
    phase: SamplePhase,
) -> bool {
    rows.iter().any(|r| {
        r.need.rank == aterm_capacity::Rank::Release
            || (phase == SamplePhase::Starting
                && r.need.rank == aterm_capacity::Rank::Ordinary
                && r.held.is_none()
                && Path::new(&r.checkout) != root)
    })
}

/// One non-tree stage's atomic resource envelope. Memory and cores must be admitted together:
/// a Background memory wait may never retain cores a foreground build needs.
fn stage_need(id: &str, cores: u32) -> aterm_capacity::Need {
    aterm_capacity::Need::new(format!("verify watch {id}"))
        .mem_gib(26)
        .cores(cores)
        .covers()
}

fn candidate_build_command(
    root: &Path,
    targo: &Path,
    stage2: &Path,
    path: &std::ffi::OsStr,
    cores: u32,
    marker: &str,
) -> crate::exec::Cmd {
    crate::exec::Cmd::new(targo)
        .args([
            "--unverified",
            "build",
            "--locked",
            "--profile",
            "test",
            "-p",
            "aterm-verify",
            "--bin",
            "aterm-verify",
        ])
        .env("TRUST_STAGE2_BIN", stage2)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"))
        .env("CARGO_BUILD_JOBS", cores.to_string())
        .env("BUILD_SLOT_HELD", "1")
        .env(aterm_capacity::COVER_ENV, marker)
}

/// Inert evidence returned only after the candidate's combined memory/CPU claim is gone.
/// Keeping the claim local to [`run_candidate_build`] makes it impossible for result writing,
/// metadata reads or sampled-input collection to retain those resources.
struct CandidateBuild {
    ran: crate::exec::Run,
    began: Instant,
    admission_waiting_s: f64,
    granted_cores: u32,
}

fn run_candidate_build(
    root: &Path,
    env: &crate::EnvSnapshot,
    tools: &crate::Toolchain,
    scratch: &Path,
    removed_env: &[&str],
) -> Result<Option<CandidateBuild>, String> {
    let admitted_at = Instant::now();
    let stage_claim = match aterm_capacity::admit(
        stage_need("candidate", aterm_capacity::cores()),
        aterm_capacity::Admit::Preemptible,
    ) {
        Ok(claim) => claim,
        Err(why @ aterm_capacity::Error::Stopped { .. }) => {
            eprintln!("watch candidate admission stopped: {why}");
            return Ok(None);
        }
        Err(why) => return Err(why.to_string()),
    };
    let admission_waiting_s = admitted_at.elapsed().as_secs_f64();
    let granted_cores = stage_claim.cores();
    let marker = stage_claim.record().cover_marker();
    let direct_path = tools.path_with_stage2_first(&env.path);
    let cmd = candidate_build_command(
        root,
        &tools.targo,
        &tools.stage2_dir,
        &direct_path,
        granted_cores,
        &marker,
    );
    let began = Instant::now();
    let ran = crate::exec::run(
        &cmd,
        crate::exec::ExecEnv {
            events: None,
            cwd: root,
            path: &env.path,
            scratch,
            child_ceiling: None,
            remove_env: removed_env,
            add_env: &[],
            stage_groups: None,
            host: &crate::exec::LiveHost,
            hang_samples: None,
        },
    );
    drop(stage_claim);
    Ok(Some(CandidateBuild {
        ran,
        began,
        admission_waiting_s,
        granted_cores,
    }))
}

/// The already-built sampled-head verifier for the tree row. It starts directly, so it, not a
/// `targo run` wrapper, leads the cancellable process group and owns its Background claims.
fn tree_command(
    root: &Path,
    program: &Path,
    args: &[&str],
    stage2: &Path,
    path: &std::ffi::OsStr,
) -> crate::exec::Cmd {
    crate::exec::Cmd::new(program)
        .args(args.iter().copied())
        .env("TRUST_STAGE2_BIN", stage2)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"))
}

fn sample_removed_env() -> Vec<String> {
    let mut removed: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| inherited_build_env(name))
        .collect();
    for name in [
        "BUILD_SLOT_HELD",
        "CARGO_BUILD_JOBS",
        aterm_capacity::COVER_ENV,
        aterm_capacity::DISK_COVER_ENV,
        "ATERM_RELEASE_*",
    ] {
        if !removed.iter().any(|present| present == name) {
            removed.push(name.to_owned());
        }
    }
    removed
}

fn sample(root: &Path, result: &Path) -> Result<bool, String> {
    use aterm_capacity::Machine as _;
    crate::exec::group::kill_on_interrupt();
    let test_handoff = sample_test_handoff();
    let is_test_handoff = test_handoff.is_some();
    // This is the long-lived controller's only sampled work. Build the sampled head's verifier
    // while this process leads the controller-owned group, then replace this image in place so
    // every row, input, attribution, seal and verdict rule comes from the head being judged.
    let (stage2, path) = if let Some(handoff) = test_handoff {
        handoff
    } else {
        let _claim = match aterm_capacity::admit(
            aterm_capacity::Need::new("verify watch sample bootstrap"),
            aterm_capacity::Admit::Preemptible,
        ) {
            Ok(claim) => claim,
            Err(why @ aterm_capacity::Error::Stopped { .. }) => {
                eprintln!("watch bootstrap admission stopped: {why}");
                return Ok(false);
            }
            Err(why) => return Err(why.to_string()),
        };
        let env = crate::EnvSnapshot::capture();
        let tools = crate::preflight(&env, root)?;
        let prefix = aterm_toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
        let lease = aterm_toolchain::lease::hold_for(
            &prefix,
            &tools.stage2_dir,
            "verify watch sampled-head bootstrap",
            aterm_toolchain::lease::DEFAULT_WAIT,
        )
        .map_err(|e| format!("{e:?}"))?;
        if let aterm_toolchain::lease::Hold::NotTaken(_, why) = &lease {
            return Err(format!("compiler lease: {why}"));
        }
        let scratch = root.join(".git/watch-exec");
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let removed = sample_removed_env();
        let removed_refs: Vec<&str> = removed.iter().map(String::as_str).collect();
        // `run_candidate_build` cannot return its claim, so everything below this call runs
        // after the candidate's memory and cores have been released.
        let Some(built) = run_candidate_build(root, &env, &tools, &scratch, &removed_refs)? else {
            return Ok(false);
        };
        fs::write(root.join(".git/watch-candidate.log"), &built.ran.output)
            .map_err(|e| e.to_string())?;
        let run_stopping = crate::exec::group::stopping();
        if built.ran.code == Some(75) || run_stopping {
            eprintln!("{}", stopped_run("bootstrap", &built.ran, run_stopping));
            return Ok(false);
        }
        if !built.ran.ok {
            let mut row = candidate_failure_row(&built.ran);
            row.log = format!(
                "sampled-head verifier did not start; bootstrap evidence only: \
                 granted_cores={} admission_waiting_s={} load1={:?} elapsed_s={}\n{}",
                built.granted_cores,
                built.admission_waiting_s,
                aterm_capacity::Live.read(None).load1,
                built.began.elapsed().as_secs_f64(),
                row.log
            );
            write_bootstrap_result(root, result, row)?;
            return Ok(true);
        }
        // Returning only inert argv/env data makes this scope the cleanup proof: the bootstrap
        // claim and compiler lease are dropped before an exec can skip destructors.
        (
            tools.stage2_dir.clone(),
            tools.path_with_stage2_first(&env.path),
        )
    };
    // Production and the subprocess negative control share this one candidate selection. A
    // stale/current-image construction here therefore breaks the control.
    let next = SampleExec::ready(root, result, &stage2, &path);
    #[cfg(test)]
    let next = if is_test_handoff {
        configure_sample_test(next)
    } else {
        next
    };
    if !is_test_handoff && running_should_stop(root)? {
        eprintln!(
            "watch bootstrap handoff: Release capacity or live line claim requested preemption"
        );
        return Ok(false);
    }
    if !crate::exec::group::default_interrupts_for_exec() {
        eprintln!("watch bootstrap handoff: pending interrupt prevented exec");
        return Ok(false);
    }
    let error = next.exec();
    crate::exec::group::kill_on_interrupt();
    Err(format!(
        "could not replace the bootstrap with the sampled-head verifier: {error}"
    ))
}

fn source_inputs(root: &Path) -> Result<String, String> {
    let mut inputs = git(root, &["rev-parse", "HEAD:rust-toolchain.toml"])?;
    if let Ok(lock) = git(root, &["rev-parse", "--verify", "HEAD:trust.lock"]) {
        inputs.push_str(&format!("\ntrust.lock={lock}"));
    }
    Ok(inputs)
}

fn candidate_failure_row(ran: &crate::exec::Run) -> Row {
    let mut row = row("bootstrap", ran.code, &ran.output);
    if let Some(why) = ran.environment_failure() {
        row.outcome = "could-not-run".into();
        row.red_items.clear();
        row.log.push_str(&format!("\n{why}"));
    }
    row
}

fn stopped_run(scope: &str, ran: &crate::exec::Run, run_stopping: bool) -> String {
    let (kind, condition) = match ran.environment_failure() {
        Some(why) => ("environment", why),
        None if run_stopping => (
            "run-stop",
            format!("sample run marked stopping; {}", ran.unnamed_cause()),
        ),
        None => ("child-exit", ran.unnamed_cause()),
    };
    format!(
        "watch {scope} stopped: code={:?} kind={kind} condition={condition:?}\n{}",
        ran.code, ran.output
    )
}

fn write_bootstrap_result(root: &Path, result: &Path, mut row: Row) -> Result<(), String> {
    let state = if row.outcome == "red" {
        "red"
    } else {
        row.outcome = "could-not-run".into();
        row.red_items.clear();
        "could-not-run"
    };
    row.suspects.clear();
    row.fixed_by = None;
    row.decisions.clear();
    write_result(
        result,
        &Sample {
            state: state.into(),
            verifier: String::new(),
            inputs: source_inputs(root)?,
            rows: vec![row],
        },
    )
}

fn write_sampled_result(root: &Path, result: &Path, mut rows: Vec<Row>) -> Result<(), String> {
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    if let Err(why) = attribute(root, &commit, &tree, &mut rows)
        && let Some(row) = rows.last_mut()
    {
        row.log
            .push_str(&format!("\nsuspect attribution unavailable: {why}"));
    }
    let mut inputs = source_inputs(root)?;
    if let Some(row) = rows.iter().find(|row| row.id == "inputs") {
        inputs = format!("compiler-pin={inputs}\n{}", row.log);
    }
    let state = verdict(&rows).to_owned();
    write_result(
        result,
        &Sample {
            state,
            verifier: commit,
            inputs,
            rows,
        },
    )
}

fn write_result(result: &Path, sample: &Sample) -> Result<(), String> {
    let text = aterm_toml::to_string(sample).map_err(|e| e.to_string())?;
    fs::write(result, text).map_err(|e| e.to_string())
}

fn sample_ready(root: &Path, result: &Path) -> Result<bool, String> {
    use aterm_capacity::Machine as _;
    let current = fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let expected = fs::canonicalize(candidate_path(root)).map_err(|e| e.to_string())?;
    if current != expected {
        return Err(format!(
            "sampled-head runner is {}, expected {}",
            current.display(),
            expected.display()
        ));
    }
    crate::exec::group::kill_on_interrupt();
    // The zero-resource claim gives a release authority to end the whole sample group even
    // between stages. Resource ownership belongs to each stage, where memory and CPU are one
    // ordered admission.
    let _claim = match aterm_capacity::admit(
        aterm_capacity::Need::new("verify watch sample lifecycle"),
        aterm_capacity::Admit::Preemptible,
    ) {
        Ok(claim) => claim,
        Err(why @ aterm_capacity::Error::Stopped { .. }) => {
            eprintln!("watch sample lifecycle admission stopped: {why}");
            return Ok(false);
        }
        Err(why) => return Err(why.to_string()),
    };
    let env = crate::EnvSnapshot::capture();
    let tools = crate::preflight(&env, root)?;
    let prefix = aterm_toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let lease = aterm_toolchain::lease::hold_for(
        &prefix,
        &tools.stage2_dir,
        "verify watch sample",
        aterm_toolchain::lease::DEFAULT_WAIT,
    )
    .map_err(|e| format!("{e:?}"))?;
    if let aterm_toolchain::lease::Hold::NotTaken(_, why) = &lease {
        return Err(format!("compiler lease: {why}"));
    }
    let mut rows = Vec::new();
    let scratch = root.join(".git/watch-exec");
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let removed = sample_removed_env();
    let removed_refs: Vec<&str> = removed.iter().map(String::as_str).collect();
    // Compat first: this is the same snapshot, compiler and invocation as the stage step.
    let stages: [(&str, &[&str]); 6] = [
        ("inputs", &["ship", "inputs"]),
        ("stable-x86_64", &["ship", "check", "--compat"]),
        ("tree", &["verify", "release", "--tree", "--background"]),
        ("export", &["ship", "check", "--export"]),
        ("cut", &["ship", "check", "--cut", "--source"]),
        ("inputs-end", &["ship", "inputs", "--recheck"]),
    ];
    for (id, args) in stages {
        if running_should_stop(root)? {
            eprintln!(
                "watch before stage={id}: Release capacity or live line claim requested preemption"
            );
            return Ok(false);
        }
        let admitted_at = Instant::now();
        let stage_claim = if id == "tree" {
            None
        } else {
            Some(
                match aterm_capacity::admit(
                    stage_need(id, aterm_capacity::cores()),
                    aterm_capacity::Admit::Preemptible,
                ) {
                    Ok(claim) => claim,
                    Err(why @ aterm_capacity::Error::Stopped { .. }) => {
                        eprintln!("watch stage={id} admission stopped: {why}");
                        return Ok(false);
                    }
                    Err(why) => return Err(why.to_string()),
                },
            )
        };
        let admission_waiting_s = admitted_at.elapsed().as_secs_f64();
        let granted_cores = stage_claim.as_ref().map_or(0, aterm_capacity::Claim::cores);
        let log = root.join(format!(".git/watch-{id}.log"));
        let began = Instant::now();
        let ran = if id == "tree" {
            // The sampled-head verifier was built once by the bootstrap and is this process's
            // own image. Its nested tree verifier owns each combined memory/CPU claim, so the
            // sample retains no outer cores around those admissions.
            assert!(stage_claim.is_none());
            let direct_path = tools.path_with_stage2_first(&env.path);
            let current = std::env::current_exe().map_err(|e| e.to_string())?;
            let direct = tree_command(root, &current, &args[1..], &tools.stage2_dir, &direct_path);
            let mut ran = crate::exec::run(
                &direct,
                crate::exec::ExecEnv {
                    events: None,
                    cwd: root,
                    path: &env.path,
                    scratch: &scratch,
                    child_ceiling: None,
                    remove_env: &removed_refs,
                    add_env: &[],
                    stage_groups: None,
                    host: &crate::exec::LiveHost,
                    hang_samples: None,
                },
            );
            if let Ok(build_output) = fs::read_to_string(root.join(".git/watch-candidate.log")) {
                ran.output.insert_str(0, &build_output);
            }
            ran
        } else {
            let stage_marker = stage_claim
                .as_ref()
                .map(|held| held.record().cover_marker())
                .ok_or("watch stage has no resource claim")?;
            let cmd = crate::exec::Cmd::new(&tools.targo)
                .args([
                    "--unverified",
                    "run",
                    "--locked",
                    "--profile",
                    "test",
                    "-p",
                    "aterm-release",
                    "--",
                ])
                .args(args[1..].iter().copied())
                .env("TRUST_STAGE2_BIN", &tools.stage2_dir)
                .env("PATH", tools.path_with_stage2_first(&env.path))
                .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"))
                .env("CARGO_BUILD_JOBS", granted_cores.to_string())
                .env("BUILD_SLOT_HELD", "1")
                .env(aterm_capacity::COVER_ENV, stage_marker);
            crate::exec::run(
                &cmd,
                crate::exec::ExecEnv {
                    events: None,
                    cwd: root,
                    path: &env.path,
                    scratch: &scratch,
                    child_ceiling: None,
                    remove_env: &removed_refs,
                    add_env: &[],
                    stage_groups: None,
                    host: &crate::exec::LiveHost,
                    hang_samples: None,
                },
            )
        };
        fs::write(&log, &ran.output).map_err(|e| e.to_string())?;
        let run_stopping = crate::exec::group::stopping();
        if ran.code == Some(75) || run_stopping {
            eprintln!(
                "{}",
                stopped_run(&format!("stage={id}"), &ran, run_stopping)
            );
            return Ok(false);
        }
        let mut row = row(id, ran.code, &ran.output);
        if let Some(why) = ran.environment_failure() {
            row.outcome = "could-not-run".into();
            row.red_items.clear();
            row.log.push_str(&format!("\n{why}"));
        }
        if id == "inputs-end"
            && row.outcome == "pass"
            && !rows
                .first()
                .is_some_and(|r: &Row| seals_match(&r.log, &row.log))
        {
            row.outcome = "could-not-run".into();
            row.log
                .push_str("\ninputs changed during the sample; no green verdict");
        }
        row.log = format!(
            "granted_cores={} admission_waiting_s={admission_waiting_s} load1={:?} elapsed_s={}\n{}",
            granted_cores,
            aterm_capacity::Live.read(None).load1,
            began.elapsed().as_secs_f64(),
            row.log
        );
        let red = row.outcome == "red";
        rows.push(row);
        if red {
            break;
        }
    }
    write_sampled_result(root, result, rows)?;
    Ok(true)
}

fn seal(text: &str) -> Option<&str> {
    text.lines()
        .rev()
        .find(|line| line.len() == 64 && line.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn seals_match(first: &str, last: &str) -> bool {
    seal(first).is_some() && seal(first) == seal(last)
}

#[derive(serde::Deserialize)]
struct StageEvidence {
    event: String,
    findings: Vec<String>,
    decisions: Vec<StageDecision>,
}

#[derive(serde::Deserialize)]
struct StageDecision {
    detail: String,
    outcome: String,
}

fn row(id: &str, code: Option<i32>, text: &str) -> Row {
    let outcome = match code {
        Some(0) => "pass",
        Some(1) if !id.starts_with("inputs") => "red",
        Some(101) if text.contains("could not compile") => "red",
        _ => "could-not-run",
    };
    let mut red_items = Vec::new();
    let mut decisions = Vec::new();
    let mut stage_evidence = false;
    for line in text
        .lines()
        .filter_map(|l| l.strip_prefix("verify-event: "))
    {
        if let Ok(event) = aterm_json::from_str::<StageEvidence>(line)
            && event.event == "stage-finished"
        {
            // Stage findings contain proven failures. Aggregate check findings also
            // contain blocked rows, and human summaries add timing-dependent aliases.
            stage_evidence = true;
            if outcome == "red" {
                red_items.extend(event.findings);
            }
            decisions.extend(event.decisions.into_iter().map(|item| Decision {
                id: item.detail,
                outcome: item.outcome,
            }));
        }
    }
    if outcome == "red" && !stage_evidence {
        red_items.extend(
            text.lines()
                .filter_map(|l| l.trim().strip_prefix("FAIL").map(|s| s.trim().to_owned())),
        );
    }
    if outcome == "red" && red_items.is_empty() {
        red_items.push(id.into());
    }
    red_items.sort();
    red_items.dedup();
    // Keep the final tail and explicit preemption evidence: later NOT RUN reporting must
    // not erase the signal sender or the admission condition from the portable journal.
    let tail = text
        .lines()
        .rev()
        .take(50)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let mut evidence = Vec::new();
    let provenance = text.lines().filter(|line| {
        line.starts_with("signal=")
            || line.contains("child was killed by signal")
            || line.starts_with("child-exit ")
    });
    for line in text
        .lines()
        .filter(|line| line.starts_with("capacity background ") || line.starts_with("watch "))
        .take(16)
        .chain(provenance)
    {
        if !tail.lines().any(|kept| kept == line) && !evidence.contains(&line) {
            evidence.push(line);
        }
    }
    evidence.push(&tail);
    let log = evidence.join("\n");
    Row {
        id: id.into(),
        outcome: outcome.into(),
        log,
        red_items,
        suspects: vec![],
        fixed_by: None,
        decisions,
    }
}

fn attribute(root: &Path, commit: &str, tree: &str, rows: &mut [Row]) -> Result<(), String> {
    let until = judged::deadline();
    let head = judged::head(root, "origin", until)?;
    let previous = judged::records(root, head.as_deref(), until)?;
    // A late verdict for an older head must not move the attribution base backwards. Commit
    // topology, not journal publication time, defines the last opposing verdict on this head's
    // history. Reading the ancestry once also drops records from rewritten or unrelated refs.
    let ancestors: BTreeSet<String> = git(root, &["rev-list", commit])?
        .lines()
        .map(str::to_owned)
        .collect();
    for row in rows {
        let wanted = if row.outcome == "red" {
            "pass"
        } else if row.outcome == "pass" {
            "red"
        } else {
            continue;
        };
        let candidates: Vec<&Record> = previous
            .iter()
            .filter(|r| {
                r.tree != tree
                    && ancestors.contains(&r.commit)
                    && r.rows
                        .iter()
                        .any(|old| old.id == row.id && old.outcome == wanted)
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let mut independent = vec!["merge-base", "--independent"];
        independent.extend(candidates.iter().map(|record| record.commit.as_str()));
        let independent = git(root, &independent)?;
        let maxima: Vec<&str> = independent.lines().collect();
        let [nearest] = maxima.as_slice() else {
            // Two incomparable last verdicts (possible after rewritten histories rejoin) define
            // no honest single range. Leave attribution empty rather than inventing one.
            continue;
        };
        let Some(base) = candidates
            .iter()
            .copied()
            .find(|record| record.commit == *nearest)
        else {
            return Err("independent attribution base was not a journal candidate".into());
        };
        let evidence = if row.outcome == "red" {
            &*row
        } else {
            base.rows
                .iter()
                .find(|r| r.id == row.id)
                .ok_or("missing attribution row")?
        };
        let links = transition_gitlinks(root, &base.commit)?;
        let roots = item_crates(evidence, &links);
        let paths = item_paths(root, &roots, row.outcome == "red", &links)?;
        let range = format!("{}..{}", base.commit, commit);
        let mut args = vec!["rev-list", "--no-merges", range.as_str()];
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
            // Workspace configuration can change any item's compiled graph.
            args.extend([
                "Cargo.toml",
                "Cargo.lock",
                "trust.lock",
                "rust-toolchain.toml",
                ".cargo",
                ".gitmodules",
            ]);
        }
        let commits = git(root, &args)?;
        let suspects: Vec<String> = commits.lines().map(str::to_owned).collect();
        if row.outcome == "red" {
            row.suspects = suspects;
        } else if suspects.len() == 1 {
            row.fixed_by = suspects.first().cloned();
        }
    }
    Ok(())
}

fn transition_gitlinks(root: &Path, base: &str) -> Result<String, String> {
    let mut links = git(root, &["ls-tree", "-rz", "--full-tree", "HEAD"])?;
    links.push('\0');
    links.push_str(&git(root, &["ls-tree", "-rz", "--full-tree", base])?);
    Ok(links)
}

fn item_crates(row: &Row, links: &str) -> BTreeSet<String> {
    let text = format!("{}\n{}", row.log, row.red_items.join("\n"));
    let mut result = BTreeSet::new();
    for token in text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')))
    {
        let scoped =
            gitlink_suffix(token, links).or_else(|| token.find("crates/").map(|at| &token[at..]));
        let Some(scoped) = scoped else {
            continue;
        };
        let parts: Vec<_> = Path::new(scoped)
            .components()
            .filter_map(|part| match part {
                std::path::Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect();
        if let Some(at) = parts.iter().position(|part| *part == "crates")
            && parts.get(at + 1).is_some()
        {
            result.insert(parts[..=at + 1].join("/"));
        }
    }
    let words: Vec<_> = text.split_whitespace().collect();
    for pair in words.windows(2) {
        if pair[0] == "-p" {
            let name = pair[1].split('@').next().unwrap_or(pair[1]);
            if name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                result.insert(format!("crates/{name}"));
            }
        }
    }
    result
}

fn item_paths(
    root: &Path,
    roots: &BTreeSet<String>,
    dependencies: bool,
    links: &str,
) -> Result<BTreeSet<String>, String> {
    if roots.is_empty() {
        return Ok(BTreeSet::new());
    }
    let (mut paths, roots) = split_gitlink_paths(roots, links);
    if roots.is_empty() {
        return Ok(paths);
    }
    let env = crate::EnvSnapshot::capture();
    let tools = crate::preflight(&env, root)?;
    let mut cmd = aterm_uds::spawnfd::command(&tools.targo);
    cmd.current_dir(root)
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"]);
    scrub(&mut cmd);
    let text = judged::output(&mut cmd, None)?;
    let metadata: aterm_json::Value = aterm_json::from_str(&text).map_err(|e| e.to_string())?;
    let graph = metadata_graph(root, &metadata)?;
    let roots = resolve_roots(root, &roots, &metadata, &graph)?;
    let resolved = if dependencies {
        dependency_closure(&roots, &graph)
    } else {
        roots
    };
    paths.extend(gitlink_paths(&resolved, links));
    Ok(paths)
}

fn resolve_roots(
    root: &Path,
    roots: &BTreeSet<String>,
    metadata: &aterm_json::Value,
    graph: &DependencyGraph,
) -> Result<BTreeSet<String>, String> {
    let packages = metadata
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or("metadata has no packages")?;
    let mut resolved = BTreeSet::new();
    for scope in roots {
        if graph.contains_key(scope) {
            resolved.insert(scope.clone());
            continue;
        }
        let name = scope.strip_prefix("crates/").unwrap_or(scope);
        let mut found = false;
        for package in packages {
            if package.get("name").and_then(aterm_json::Value::as_str) == Some(name)
                && let Some(manifest) = package
                    .get("manifest_path")
                    .and_then(aterm_json::Value::as_str)
                && let Some(dir) = Path::new(manifest)
                    .parent()
                    .and_then(|p| p.strip_prefix(root).ok())
                    .and_then(Path::to_str)
            {
                resolved.insert(dir.to_owned());
                found = true;
            }
            if let Some(dependencies) = package
                .get("dependencies")
                .and_then(aterm_json::Value::as_array)
            {
                for dependency in dependencies {
                    if dependency.get("name").and_then(aterm_json::Value::as_str) == Some(name)
                        && let Some(path) =
                            dependency.get("path").and_then(aterm_json::Value::as_str)
                        && let Some(path) = Path::new(path)
                            .strip_prefix(root)
                            .ok()
                            .and_then(Path::to_str)
                    {
                        resolved.insert(path.to_owned());
                        found = true;
                    }
                }
            }
        }
        if !found {
            return Err(format!("cannot resolve item scope {scope}"));
        }
    }
    Ok(resolved)
}

fn gitlink_paths(paths: &BTreeSet<String>, tree: &str) -> BTreeSet<String> {
    paths
        .iter()
        .map(|path| enclosing_gitlink(path, tree).map_or_else(|| path.clone(), str::to_owned))
        .collect()
}

fn split_gitlink_paths(
    paths: &BTreeSet<String>,
    tree: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut linked = BTreeSet::new();
    let mut ordinary = BTreeSet::new();
    for path in paths {
        if let Some(link) = enclosing_gitlink(path, tree) {
            linked.insert(link.to_owned());
        } else {
            ordinary.insert(path.clone());
        }
    }
    (linked, ordinary)
}

fn enclosing_gitlink<'a>(path: &str, tree: &'a str) -> Option<&'a str> {
    tree.split('\0')
        .filter_map(|entry| {
            let (header, path) = entry.split_once('\t')?;
            header.starts_with("160000 ").then_some(path)
        })
        .find(|link| Path::new(path).starts_with(link))
}

fn gitlink_suffix<'a>(path: &'a str, tree: &str) -> Option<&'a str> {
    tree.split('\0')
        .filter_map(|entry| {
            let (header, link) = entry.split_once('\t')?;
            header.starts_with("160000 ").then_some(link)
        })
        .filter_map(|link| {
            if path == link || path.starts_with(&format!("{link}/")) {
                return Some((link.len(), path));
            }
            let needle = format!("/{link}");
            let at = path.find(&needle)? + 1;
            let end = at + link.len();
            (path.as_bytes().get(end).is_none() || path.as_bytes().get(end) == Some(&b'/'))
                .then_some((link.len(), &path[at..]))
        })
        .max_by_key(|(link_len, _)| *link_len)
        .map(|(_, suffix)| suffix)
}

type DependencyGraph = BTreeMap<String, (String, BTreeSet<String>)>;

fn metadata_graph(root: &Path, value: &aterm_json::Value) -> Result<DependencyGraph, String> {
    let mut graph = BTreeMap::new();
    let packages = value
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or("metadata has no packages")?;
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(aterm_json::Value::as_str)
            .ok_or("package has no manifest")?;
        let Some(dir) = Path::new(manifest)
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .and_then(Path::to_str)
        else {
            continue;
        };
        let mut deps = BTreeSet::new();
        if let Some(dependencies) = package
            .get("dependencies")
            .and_then(aterm_json::Value::as_array)
        {
            for dep in dependencies {
                if let Some(path) = dep.get("path").and_then(aterm_json::Value::as_str)
                    && let Ok(path) = Path::new(path).strip_prefix(root)
                    && let Some(path) = path.to_str()
                {
                    deps.insert(path.to_owned());
                }
            }
        }
        graph.insert(dir.to_owned(), (dir.to_owned(), deps));
    }
    Ok(graph)
}

fn dependency_closure(roots: &BTreeSet<String>, graph: &DependencyGraph) -> BTreeSet<String> {
    let mut paths = roots.clone();
    let mut pending: Vec<_> = graph
        .iter()
        .filter(|(_, (dir, _))| roots.contains(dir))
        .map(|(name, _)| name.clone())
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some((dir, deps)) = graph.get(&name) {
            paths.insert(dir.clone());
            pending.extend(deps.iter().cloned());
        } else {
            // --no-deps omits non-workspace path packages; their enclosing Git pin
            // still affects this item's compiled inputs.
            paths.insert(name);
        }
    }
    paths
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
