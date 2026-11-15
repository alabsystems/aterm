// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Idle-capacity main sampling. The controller renews a remote lease while a disposable
//! process group judges one immutable tree with that tree's tools. Only journal records leave it.
//! A renewal that fails ends no judgment, and an attempt's last record is kept in the checkout
//! until the journal holds it.

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
verify watch --census REF [--since UNIX_SECONDS] [--flaky] reads states and causes from a fetched journal.\n\
With --since, count attempts from history, including overwritten cancellations, and emit JSON records.\n\
With --flaky, list instead the test cases that were red on one tree and green on another with\n\
an unchanged closure: for each test case a red row names, a record whose same row passed and\n\
whose commit holds the case's closure under the same git object ids. The closure is the case's\n\
crate, every path package Cargo.lock resolves below it (one the root manifest patches in\n\
included) and the toolchain files (trust.lock, rust-toolchain.toml, .cargo); it is computed at\n\
both commits and must name the same paths and the same registry packages. A line gives the\n\
case, both commits, both hosts, each row's load1 and, as config_changed, which of Cargo.toml,\n\
Cargo.lock and .gitmodules differ outside that closure. It reads journal records and git\n\
objects and starts no build, test or judgment.\n\
The list cannot show a case that fails only in a saturated landing (the lane samples at\n\
Background rank), a case whose row never passed on an unchanged closure, or a change outside\n\
the closure (a root manifest row that is no dependency, a file the case reads elsewhere in the\n\
tree or on its host).\n\
verify watch --install [--workspace DIR] [--interval SECONDS] installs a persistent macOS main lane.\n\
Samples origin/main in a dedicated persistent clone at Background rank. Claims and verdicts\n\
are appended to refs/aterm/judged (14 days of records). A release ends the sample; Ordinary\n\
work retains priority for overlapping resources. --once attempts one sample. No hook, gate or publication.\n\
A claim that cannot be renewed ends no sample; a record the journal did not take is kept in the\n\
clone and published by the next loop.\n";

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
    let mut declined = None;
    loop {
        if stop.caught().is_some() {
            return Ok(());
        }
        let result = attempt(&opts);
        if let Some(line) = log_line(&mut declined, &result) {
            eprintln!("{line}");
        }
        if opts.once {
            return result.map(|_| ());
        }
        stop.nap(opts.interval, &|| false);
    }
}

/// How a turn of the controller's loop ended.
#[derive(Debug, PartialEq, Eq)]
enum Start {
    /// A tree was claimed, and its attempt's last record is in the journal.
    Judged,
    /// No sample started, and why.
    Declined(String),
}

/// What a turn of the loop leaves in the lane's own log: a failure each time, and a declined
/// start once per reason, `declined` being the reason last written. A lane that starts nothing
/// for hours says why once, not at every turn.
fn log_line(declined: &mut Option<String>, result: &Result<Start, String>) -> Option<String> {
    let reason = match result {
        Ok(Start::Declined(why)) => Some(why),
        _ => None,
    };
    let line = match result {
        Err(why) => Some(format!("verify watch: sample could not run: {why}")),
        Ok(Start::Declined(why)) if declined.as_ref() != Some(why) => {
            Some(format!("verify watch: no sample started: {why}"))
        }
        Ok(_) => None,
    };
    *declined = reason.cloned();
    line
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

fn attempt(opts: &Options) -> Result<Start, String> {
    let signal_from = aterm_uds::signal_origin::records().len();
    let root = &opts.workspace;
    // What the last attempt left reaches the journal before anything new is claimed.
    settle(root, now())?;
    if foreground_waiting(root)? {
        return Ok(Start::Declined(
            "a release holds this Mac, or foreground work waits for its capacity".into(),
        ));
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
        if let Some(held) = holder(root, head.as_deref(), &record.tree, now(), until)? {
            return Ok(Start::Declined(format!(
                "{} is {} by {}",
                record.commit, held.state, held.host
            )));
        }
        if judged::append(root, "origin", head.as_deref(), &record, until)? {
            break;
        }
    }
    // Kept from the claim until the journal holds this attempt's last record: a controller that
    // ends while judging leaves its claim here, for the next loop to close. A claim whose append
    // ended with its outcome unknown (the push child killed at its deadline after the remote
    // took the commit) returned above: it is not kept, and lapses with no later record.
    save(root, &record)?;
    let mut unrenewed = None;
    let result = judge(root, &mut record, &mut unrenewed);
    if let Err(why) = result {
        record.state = "could-not-run".into();
        record.cause = why.clone();
        record.rows.push(lane_row(why));
    }
    let signals: Vec<_> = aterm_uds::signal_origin::records()
        .into_iter()
        .skip(signal_from)
        .collect();
    if !signals.is_empty() {
        record.cause.push_str(&format!("\n{}", signals.join("\n")));
    }
    if let Some(why) = unrenewed {
        record.cause.push_str(&format!("\n{why}"));
    }
    conclude(root, &mut record, now())?;
    Ok(Start::Judged)
}

/// The record that keeps a new claim out of `tree` at `epoch`: a live claim or a verdict in its
/// slot, or a verdict filed beside it. A tree with a verdict is not judged again, wherever the
/// verdict is filed, so no red is run again into a pass.
fn holder(
    root: &Path,
    head: Option<&str>,
    tree: &str,
    epoch: u64,
    until: Instant,
) -> Result<Option<Record>, String> {
    if let Some(held) = judged::record(root, head, tree, until)?
        && held.occupied(epoch)
    {
        return Ok(Some(held));
    }
    Ok(judged::beside(root, head, tree, until)?
        .into_iter()
        .find(|late| matches!(late.state.as_str(), "pass" | "red")))
}

#[derive(Debug)]
enum PublishError {
    /// Another attempt's live claim or verdict holds the tree's slot; what it is.
    Displaced(String),
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
            Self::Displaced(held) => write!(f, "another attempt holds this tree: {held}"),
            Self::Unavailable(why) => f.write_str(why),
        }
    }
}

const STOP_GRACE: Duration = Duration::from_secs(20);
/// How long after a renewal that failed the next one is tried, in seconds.
const RENEW_RETRY: u64 = 60;
const _: () = assert!(RENEW_RETRY < judged::RENEW_EVERY);

/// When a claim is renewed: at each stage end and at least every [`judged::RENEW_EVERY`]
/// seconds, and [`RENEW_RETRY`] seconds after a renewal the journal did not take. A renewal
/// that fails is counted and ends nothing: a judgment of an hour or two is worth more than its
/// lease, and where two hosts each reach a pass or a red for one tree the journal keeps both
/// ([`publish`]).
struct Renewal {
    next: Instant,
    stages: usize,
    failures: u32,
    last_failure: Option<String>,
}

impl Renewal {
    /// A claim whose first renewal is due at `now`: it carries the sampled inputs.
    fn new(now: Instant) -> Self {
        Self {
            next: now,
            stages: 0,
            failures: 0,
            last_failure: None,
        }
    }

    fn due(&self, now: Instant, stages: usize) -> bool {
        now >= self.next || stages != self.stages
    }

    fn renewed(&mut self, now: Instant, stages: usize) {
        self.next = now + Duration::from_secs(judged::RENEW_EVERY);
        self.stages = stages;
    }

    fn failed(&mut self, now: Instant, stages: usize, why: &PublishError) {
        // Another attempt's hold ends with its verdict or its lease, neither of them soon.
        let again = match why {
            PublishError::Displaced(_) => judged::RENEW_EVERY,
            PublishError::Unavailable(_) => RENEW_RETRY,
        };
        self.next = now + Duration::from_secs(again);
        self.stages = stages;
        self.failures += 1;
        self.last_failure = Some(why.to_string());
    }

    /// What the renewals that failed leave for the attempt's last record.
    fn evidence(&self) -> Option<String> {
        self.last_failure.as_ref().map(|last| {
            format!(
                "claim renewals that failed: {}; the last: {last}",
                self.failures
            )
        })
    }
}

/// Whether `held`, the record in a tree's slot, keeps `record` out of it: another attempt's
/// live claim or verdict. Anything else there is what a new claim may replace
/// ([`Record::occupied`]), and so may this attempt's own next record.
fn displaced_by<'a>(held: Option<&'a Record>, record: &Record, epoch: u64) -> Option<&'a Record> {
    held.filter(|held| held.token != record.token && held.occupied(epoch))
}

/// Renew `record`'s claim of its tree's slot, where no other attempt holds it.
fn renew(root: &Path, record: &Record, epoch: u64, until: Instant) -> Result<(), PublishError> {
    loop {
        let head = judged::head(root, "origin", until)?;
        let held = judged::record(root, head.as_deref(), &record.tree, until)?;
        if let Some(held) = displaced_by(held.as_ref(), record, epoch) {
            return Err(PublishError::Displaced(format!(
                "{} by {}",
                held.state, held.host
            )));
        }
        if judged::append(root, "origin", head.as_deref(), record, until)? {
            return Ok(());
        }
    }
}

/// File `record`, an attempt's last, in the journal, and say where. It goes beside its tree's
/// slot only where another attempt's live claim, or another attempt's pass or red, holds the
/// slot ([`displaced_by`]); the slot keeps that record. Anywhere else it goes at the slot.
/// Where the slot holds another attempt's lapsed claim, or the cancelled or could-not-run
/// record another attempt ended with, this record replaces it at the head, and the replaced
/// record stays only in the journal's history (`--census REF --since` reads it). Another
/// attempt's pass or red is never replaced. A record the journal has already held is not
/// appended again. Every write is the journal's compare-and-swap append.
fn publish(
    root: &Path,
    record: &Record,
    epoch: u64,
    until: Instant,
) -> Result<Option<judged::Place>, String> {
    loop {
        let head = judged::head(root, "origin", until)?;
        if judged::published(root, head.as_deref(), record, until)? {
            return Ok(None);
        }
        let held = judged::record(root, head.as_deref(), &record.tree, until)?;
        let place = match displaced_by(held.as_ref(), record, epoch) {
            Some(_) => judged::Place::Beside,
            None => judged::Place::Slot,
        };
        if judged::append_at(root, "origin", head.as_deref(), record, place, until)? {
            return Ok(Some(place));
        }
    }
}

/// What a judgment's wait reads and does. The controller's is [`Live`]; a test scripts one, and
/// runs a judgment's hours in no time.
trait Attended {
    /// The sample's exit status, once it has ended.
    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String>;
    /// End the sample and what it started.
    fn stop(&mut self) -> Result<(), String>;
    /// The signal this controller caught, if any.
    fn signal(&self) -> Option<i32>;
    /// Now: the monotonic clock, and Unix seconds.
    fn now(&self) -> (Instant, u64);
    /// How many of the sample's stages have ended.
    fn stages(&mut self) -> usize;
    /// Wait before the next look.
    fn pause(&mut self);
}

/// How a judgment's wait ended.
#[derive(Debug)]
enum Ended {
    Exited(std::process::ExitStatus),
    /// This controller caught the signal and stopped the sample.
    Interrupted(i32),
}

/// The controller's sample: its process group, the checkout its stages write their logs in, and
/// the real clocks.
struct Live<'a> {
    child: aterm_uds::spawnfd::Child,
    stopped: aterm_toolchain::interrupt::Watch,
    root: &'a Path,
    began: std::time::SystemTime,
    looked: Option<Instant>,
    stages: usize,
}

impl Attended for Live<'_> {
    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        self.child.try_wait().map_err(|e| e.to_string())
    }

    fn stop(&mut self) -> Result<(), String> {
        stop_sample(&mut self.child)
    }

    fn signal(&self) -> Option<i32> {
        self.stopped.caught()
    }

    fn now(&self) -> (Instant, u64) {
        (Instant::now(), now())
    }

    fn stages(&mut self) -> usize {
        // One directory read every few seconds, not at every look.
        if self
            .looked
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            self.stages = stage_ends(self.root, self.began);
            self.looked = Some(Instant::now());
        }
        self.stages
    }

    fn pause(&mut self) {
        self.stopped.nap(Duration::from_millis(100), &|| false);
    }
}

/// How many stages of the sample that began at `since` have ended: each writes
/// `.git/watch-<stage>.log` once, as it ends. The sample's own output and the service's log
/// are no stage's.
fn stage_ends(root: &Path, since: std::time::SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(root.join(".git")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("watch-")
                && name.ends_with(".log")
                && name != "watch-sample.log"
                && name != "watch-service.log"
                && entry
                    .metadata()
                    .and_then(|file| file.modified())
                    .is_ok_and(|written| written >= since)
        })
        .count()
}

/// Where the checkout keeps its controller's attempt, from the claim until the journal holds
/// the attempt's last record.
fn saved(root: &Path) -> PathBuf {
    root.join(".git/watch-record.toml")
}

/// Keep `record` in the checkout, whole or not at all.
fn save(root: &Path, record: &Record) -> Result<(), String> {
    let path = saved(root);
    let fresh = path.with_extension("toml.new");
    fs::write(
        &fresh,
        aterm_toml::to_string(record).map_err(|e| e.to_string())?,
    )
    .and_then(|()| fs::rename(&fresh, &path))
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// End an attempt at `epoch`: its last record is kept in the checkout first, so a publish that
/// fails, or a controller that ends here, leaves it for the next loop ([`settle`]).
fn conclude(root: &Path, record: &mut Record, epoch: u64) -> Result<(), String> {
    record.updated = epoch;
    record.lease_until = 0;
    save(root, record)?;
    settle(root, epoch)
}

/// Publish the record the checkout keeps ([`publish`]), and forget it once the journal holds
/// it. A kept record still `judging` is a claim whose controller ended before its attempt did,
/// since no judgment of this controller is in flight when its loop turns: it is closed as
/// could-not-run. A kept record that cannot be read is set aside, and stops no later sample.
fn settle(root: &Path, epoch: u64) -> Result<(), String> {
    let path = saved(root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut record = match aterm_toml::from_str::<Record>(&text) {
        Ok(record) => record,
        Err(error) => {
            let aside = path.with_extension("unreadable.toml");
            eprintln!(
                "verify watch: {} is no record ({error}); kept as {}",
                path.display(),
                aside.display()
            );
            return fs::rename(&path, &aside).map_err(|e| format!("{}: {e}", path.display()));
        }
    };
    if record.state == "judging" {
        let why = "the controller ended before this attempt had a last record";
        record.state = "could-not-run".into();
        record.cause = why.into();
        record.rows.push(lane_row(why.into()));
        record.updated = epoch;
        record.lease_until = 0;
        save(root, &record)?;
    }
    let place = publish(root, &record, epoch, judged::deadline())?;
    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "verify watch: {} {} elapsed_s={} (journal {}, {}) cause={:?}",
        record.state,
        record.commit,
        record.updated.saturating_sub(record.started),
        judged::REF,
        match place {
            Some(judged::Place::Slot) => "at its tree's slot",
            Some(judged::Place::Beside) => "beside another attempt's record",
            None => "which held it already",
        },
        record.cause
    );
    Ok(())
}

/// The row of a record the lane itself could not run.
fn lane_row(why: String) -> Row {
    Row {
        id: "lane".into(),
        outcome: "could-not-run".into(),
        log: why,
        red_items: vec![],
        suspects: vec![],
        fixed_by: None,
        decisions: vec![],
    }
}

/// Wait for `sample` to end, renewing `record`'s claim meanwhile ([`Renewal`]). Only the
/// sample's own end or this controller's signal ends the wait. A renewal the journal did not
/// take is logged and counted, and what the count holds is returned beside the end.
fn attend(
    root: &Path,
    record: &mut Record,
    sample: &mut dyn Attended,
) -> Result<(Ended, Option<String>), String> {
    let mut renewal = Renewal::new(sample.now().0);
    loop {
        if let Some(status) = sample.exited()? {
            return Ok((Ended::Exited(status), renewal.evidence()));
        }
        if let Some(signal) = sample.signal() {
            sample.stop()?;
            return Ok((Ended::Interrupted(signal), renewal.evidence()));
        }
        let (at, epoch) = sample.now();
        let stages = sample.stages();
        if renewal.due(at, stages) {
            record.updated = epoch;
            record.lease_until = epoch + judged::LEASE;
            match judged::interruptible(|| renew(root, record, epoch, judged::deadline())) {
                Ok(()) => renewal.renewed(at, stages),
                Err(why) => {
                    eprintln!(
                        "verify watch: the claim was not renewed, and the sample goes on: {why}"
                    );
                    renewal.failed(at, stages, &why);
                }
            }
        }
        sample.pause();
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

/// Judge `record`'s commit in a sample of its own, and leave the attempt's state, cause and
/// rows in `record`. `unrenewed` is what the claim's failed renewals left, if any failed.
fn judge(root: &Path, record: &mut Record, unrenewed: &mut Option<String>) -> Result<(), String> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("watch workspace is dirty; preserving its work".into());
    }
    git(root, &["checkout", "--quiet", "--detach", &record.commit])?;
    // vendor/astream is private. Its URL may be HTTPS, but only caller authority—not URL shape—
    // can say a read is anonymous.
    git(root, &["submodule", "update", "--init", "vendor/astream"])?;
    git(root, &["config", "submodule.recurse", "true"])?;
    // The wait's first renewal carries these to the journal.
    record.inputs = source_inputs(root)?;
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
    let began = std::time::SystemTime::now();
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut live = Live {
        child,
        stopped: aterm_toolchain::interrupt::Watch::enter(),
        root,
        began,
        looked: None,
        stages: 0,
    };
    let (ended, evidence) = attend(root, record, &mut live)?;
    *unrenewed = evidence;
    let status = match ended {
        Ended::Exited(status) => status,
        Ended::Interrupted(signal) => {
            record.state = "cancelled".into();
            record.cause = format!(
                "controller signal={signal}; {}",
                aterm_uds::signal_origin::records().join("\n")
            );
            return Ok(());
        }
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
            args.extend(WORKSPACE_INPUTS);
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

/// Workspace configuration can change any item's compiled graph.
const WORKSPACE_INPUTS: [&str; 6] = [
    "Cargo.toml",
    "Cargo.lock",
    "trust.lock",
    "rust-toolchain.toml",
    ".cargo",
    ".gitmodules",
];

fn transition_gitlinks(root: &Path, base: &str) -> Result<String, String> {
    let mut links = git(root, &["ls-tree", "-rz", "--full-tree", "HEAD"])?;
    links.push('\0');
    links.push_str(&git(root, &["ls-tree", "-rz", "--full-tree", base])?);
    Ok(links)
}

fn item_crates(row: &Row, links: &str) -> BTreeSet<String> {
    named_crates(&format!("{}\n{}", row.log, row.red_items.join("\n")), links)
}

/// The crates `text` names: a path through `crates/` or under a git pin of `links`, and each
/// `-p` package.
fn named_crates(text: &str, links: &str) -> BTreeSet<String> {
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
    let read = |name: &str| {
        crate::read_regular_text(root.join(name))
            .map_err(|e| e.to_string())
            .and_then(|text| aterm_toml::from_str(&text).map_err(|e| e.to_string()))
            .map_err(|why| format!("{name}: {why}"))
    };
    item_closure(root, roots, dependencies, links, &|| {
        let env = crate::EnvSnapshot::capture();
        let tools = crate::preflight(&env, root)?;
        let mut cmd = aterm_uds::spawnfd::command(&tools.targo);
        cmd.current_dir(root)
            .args(["metadata", "--locked", "--no-deps", "--format-version", "1"]);
        scrub(&mut cmd);
        let text = judged::output(&mut cmd, None)?;
        Ok(Described {
            metadata: aterm_json::from_str(&text).map_err(|e| e.to_string())?,
            manifest: read("Cargo.toml")?,
            lock: read("Cargo.lock")?,
        })
    })
    .map(|closure| closure.paths)
}

/// What an item's closure is read from, of one tree.
struct Described {
    /// The workspace as `targo metadata --no-deps` words it: its `packages`, each a `name`, a
    /// `manifest_path` under the root and `dependencies` by `name` and `path`.
    metadata: aterm_json::Value,
    /// The root manifest, whose `[patch]` tables put a package named by version at a path.
    manifest: aterm_toml::Value,
    /// `Cargo.lock`, which names every package each package links.
    lock: aterm_toml::Value,
}

/// What an item reads of a workspace.
#[derive(Debug, Default, PartialEq, Eq)]
struct Closure {
    /// The directories of its crates and of every path package those link, each a git pin where
    /// it lies in one.
    paths: BTreeSet<String>,
    /// The registry packages those link, each as the lock identifies it: its name, version,
    /// source and checksum.
    registry: BTreeSet<String>,
}

/// The closure of the crates at `roots` (with `dependencies`, of every package those link too)
/// in the workspace `described` answers with, asked only where a root lies outside every git pin
/// of `links`.
fn item_closure<D: std::borrow::Borrow<Described>>(
    root: &Path,
    roots: &BTreeSet<String>,
    dependencies: bool,
    links: &str,
    described: &dyn Fn() -> Result<D, String>,
) -> Result<Closure, String> {
    if roots.is_empty() {
        return Ok(Closure::default());
    }
    let (pinned, roots) = split_gitlink_paths(roots, links);
    let mut closure = Closure::default();
    if !roots.is_empty() {
        let described = described()?;
        let described = described.borrow();
        let graph = metadata_graph(root, &described.metadata)?;
        let roots = resolve_roots(root, &roots, &described.metadata, &graph)?;
        if dependencies {
            closure = locked_closure(root, &roots, described)?;
            closure.paths.extend(dependency_closure(&roots, &graph));
        } else {
            closure.paths = roots;
        }
        closure.paths = gitlink_paths(&closure.paths, links);
    }
    closure.paths.extend(pinned);
    Ok(closure)
}

/// The path packages a root manifest's `[patch]` tables name: each one's package name and its
/// directory under the root.
fn patched(manifest: &aterm_toml::Value) -> Vec<(String, String)> {
    use aterm_toml::Value;
    manifest
        .get("patch")
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(|sources| sources.values())
        .filter_map(Value::as_table)
        .flat_map(|rows| rows.iter())
        .filter_map(|(key, row)| {
            let path = row.get("path")?.as_str()?;
            let name = row.get("package").and_then(Value::as_str).unwrap_or(key);
            Some((
                name.to_owned(),
                crate::export_scan::join_lexically("", path),
            ))
        })
        .collect()
}

/// Where the description puts each package, by name: where a package of it, a path dependency
/// or a patch row of the root manifest has one of that name.
fn placed(
    root: &Path,
    described: &Described,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    use aterm_json::Value;
    fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str)
    }
    let mut placed = BTreeMap::<String, BTreeSet<String>>::new();
    let mut place = |name: Option<&str>, dir: Option<String>| {
        if let (Some(name), Some(dir)) = (name, dir) {
            placed.entry(name.to_owned()).or_default().insert(dir);
        }
    };
    let under = |path: &Path| path.strip_prefix(root).ok()?.to_str().map(str::to_owned);
    let packages = described
        .metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("metadata has no packages")?;
    for package in packages {
        place(
            text(package, "name"),
            text(package, "manifest_path")
                .and_then(|manifest| under(Path::new(manifest).parent()?)),
        );
        for dependency in package
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            place(
                text(dependency, "name"),
                text(dependency, "path").and_then(|path| under(Path::new(path))),
            );
        }
    }
    for (name, path) in patched(&described.manifest) {
        place(Some(&name), Some(path));
    }
    Ok(placed)
}

/// One `[[package]]` of a `Cargo.lock`.
struct Locked<'a> {
    name: &'a str,
    version: &'a str,
    /// Where it comes from. A path package has none.
    source: Option<&'a str>,
    checksum: &'a str,
    /// Each as `name`, `name version` or `name version (source)`: as much as tells the package
    /// from the lock's others.
    dependencies: Vec<&'a str>,
}

impl Locked<'_> {
    /// Whether a dependency's words name this package.
    fn is(&self, version: Option<&str>, source: Option<&str>) -> bool {
        version.is_none_or(|version| version == self.version)
            && source.is_none_or(|source| {
                self.source.is_some_and(|from| {
                    source.strip_prefix('(').and_then(|s| s.strip_suffix(')')) == Some(from)
                })
            })
    }
}

/// The packages of a `Cargo.lock`, by name.
fn locked(lock: &aterm_toml::Value) -> Result<BTreeMap<&str, Vec<Locked<'_>>>, String> {
    use aterm_toml::Value;
    let mut locked = BTreeMap::<&str, Vec<Locked<'_>>>::new();
    for package in lock
        .get("package")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = |key: &str| package.get(key).and_then(Value::as_str);
        let (Some(name), Some(version)) = (text("name"), text("version")) else {
            return Err("Cargo.lock holds a package without a name or a version".into());
        };
        locked.entry(name).or_default().push(Locked {
            name,
            version,
            source: text("source"),
            checksum: text("checksum").unwrap_or_default(),
            dependencies: package
                .get("dependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect(),
        });
    }
    Ok(locked)
}

/// What the lock resolves below the packages at `roots`: the directories of the path packages
/// and the identities of the rest. The lock names every package a package links, so it holds
/// the edges no manifest names by path: a dependency by version that the root manifest's
/// `[patch]` tables put at a path, whether a workspace crate names it or a registry package
/// between them does.
///
/// A path package's directories are those [`placed`] has for its name, so two of one name (two
/// versions of one patched package) are both in the closure. One nothing places adds no path:
/// the manifest that names it is one the description lacks, a git pin's or (under `--no-deps`)
/// a path package's outside the workspace.
///
/// # Errors
/// A root whose package the lock does not hold: nothing says what it links.
fn locked_closure(
    root: &Path,
    roots: &BTreeSet<String>,
    described: &Described,
) -> Result<Closure, String> {
    let placed = placed(root, described)?;
    let locked = locked(&described.lock)?;
    let mut pending = Vec::new();
    for dir in roots {
        let before = pending.len();
        pending.extend(
            placed
                .iter()
                .filter(|(_, dirs)| dirs.contains(dir))
                .flat_map(|(name, _)| locked.get(name.as_str()).into_iter().flatten())
                .filter(|package| package.source.is_none()),
        );
        if pending.len() == before {
            return Err(format!("Cargo.lock holds no package at {dir}"));
        }
    }
    let mut closure = Closure::default();
    let mut seen = BTreeSet::new();
    while let Some(package) = pending.pop() {
        if !seen.insert((package.name, package.version, package.source)) {
            continue;
        }
        match package.source {
            Some(source) => {
                closure.registry.insert(format!(
                    "{} {} {source} {}",
                    package.name, package.version, package.checksum
                ));
            }
            None => closure
                .paths
                .extend(placed.get(package.name).into_iter().flatten().cloned()),
        }
        for dependency in &package.dependencies {
            let mut words = dependency.splitn(3, ' ');
            let name = words.next().unwrap_or_default();
            let (version, source) = (words.next(), words.next());
            pending.extend(
                locked
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|package| package.is(version, source)),
            );
        }
    }
    Ok(closure)
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
