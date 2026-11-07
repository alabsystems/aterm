// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Idle-capacity main sampling. The controller renews a remote lease while a disposable
//! process group judges one immutable tree with that tree's tools. Only journal records leave it.

use crate::judged::{self, Decision, Record, Row, git};
use aterm_uds::spawnfd::{Stdio, Tether as _};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "watch_service.rs"]
mod service;

const HELP: &str = "verify watch [--workspace DIR] [--remote NAME] [--branch BRANCH] [--once] [--interval SECONDS]\n\
verify watch --install [--workspace DIR] [--interval SECONDS] installs a persistent macOS main lane.\n\
Samples origin/main in a dedicated persistent clone at Background rank. Claims and verdicts\n\
are appended to refs/aterm/judged (14 days of records). A release ends the sample; an Ordinary\n\
waiter yields it at the next stage. --once attempts one sample. No hook, gate or publication.\n";

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
        workspace: PathBuf::from(home).join(".local/state/aterm-verify/watch/checkout"),
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

/// The lane controller. An internal sample verb is always launched in its own process group.
pub fn run(args: &[String], cwd: &Path) -> i32 {
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
    let root = &opts.workspace;
    if foreground_waiting(root)? {
        return Ok(());
    }
    git(root, &["fetch", "--quiet", "origin", &opts.branch])?;
    let commit = git(root, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    let tree = git(root, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    let host = aterm_uds::process::kernel_host_name().ok_or("host has no name")?;
    let started = now();
    let mut record = Record {
        commit: commit.clone(),
        tree,
        host,
        verifier: git(root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        inputs: String::new(),
        token: aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?,
        state: "judging".into(),
        started,
        updated: started,
        lease_until: started + judged::LEASE,
        rows: Vec::new(),
    };
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
    record.updated = now();
    record.lease_until = 0;
    publish_owned(root, &record, judged::deadline()).map_err(|e| e.to_string())?;
    println!(
        "verify watch: {} {} elapsed_s={} (journal {})",
        record.state,
        record.commit,
        record.updated.saturating_sub(record.started),
        judged::REF
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
}

impl Renewal {
    fn new(expires: Instant, epoch_until: u64, now: Instant) -> Self {
        Self {
            expires,
            epoch_until,
            next: now + Duration::from_secs(30),
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
        self.cutoff()
            .min(
                now + Duration::from_secs(
                    self.epoch_until
                        .saturating_sub(epoch.saturating_add(LEASE_MARGIN.as_secs())),
                ),
            )
            .min(now + Duration::from_secs(5))
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
            Err(PublishError::Unavailable(_)) if self.valid(now, epoch) => {
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

fn judge(root: &Path, record: &mut Record) -> Result<(), String> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("watch workspace is dirty; preserving its work".into());
    }
    git(root, &["checkout", "--quiet", "--detach", &record.commit])?;
    git(root, &["submodule", "update", "--init", "vendor/astream"])?;
    git(root, &["config", "submodule.recurse", "true"])?;
    record.verifier = record.commit.clone();
    record.inputs = git(root, &["rev-parse", "HEAD:rust-toolchain.toml"])?;
    if let Ok(lock) = git(root, &["rev-parse", "--verify", "HEAD:trust.lock"]) {
        record.inputs.push_str(&format!("\ntrust.lock={lock}"));
    }
    record.updated = now();
    record.lease_until = now() + judged::LEASE;
    publish_owned(root, record, judged::deadline()).map_err(|e| e.to_string())?;
    let result = root.join(".git/watch-result.toml");
    let _ = fs::remove_file(&result);
    let log = root.join(".git/watch-sample.log");
    let mut cmd = sample_command(&std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.current_dir(root)
        .args(["watch", "--sample"])
        .arg(&result);
    cmd.stdout(File::create(log).map_err(|e| e.to_string())?)
        .stderr(Stdio::inherit());
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
        if stopped.caught().is_some() {
            stop_sample(&mut child)?;
            record.state = "cancelled".into();
            return Ok(());
        }
        let proposed = Instant::now();
        if !renewal.valid(proposed, now()) {
            stop_sample(&mut child)?;
            return Err("could not renew the judging lease before its cleanup margin".into());
        }
        if proposed >= renewal.next {
            record.updated = now();
            record.lease_until = now() + judged::LEASE;
            let published = publish_owned(root, record, renewal.deadline(proposed, now()));
            if let Err(why) = renewal.finish(
                published,
                proposed,
                record.lease_until,
                Instant::now(),
                now(),
            ) {
                let _ = stop_sample(&mut child);
                return Err(why.to_string());
            }
        }
        stopped.nap(Duration::from_millis(100), &|| false);
    };
    if !status.success() {
        if status.code().is_none() || status.code() == Some(75) {
            record.state = "cancelled".into();
            return Ok(());
        }
        return Err(format!("sample exited {status}; see .git/watch-sample.log"));
    }
    let text = fs::read_to_string(result).map_err(|e| e.to_string())?;
    record.rows = aterm_toml::from_str::<Sample>(&text)
        .map_err(|e| e.to_string())?
        .rows;
    if let Some(inputs) = record.rows.iter().find(|r| r.id == "inputs") {
        record.inputs = format!("compiler-pin={}\n{}", record.inputs, inputs.log);
    }
    record.state = verdict(&record.rows).into();
    if let Err(why) = attribute(root, record)
        && let Some(row) = record.rows.last_mut()
    {
        row.log
            .push_str(&format!("\nsuspect attribution unavailable: {why}"));
    }
    Ok(())
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

fn scrub(cmd: &mut aterm_uds::spawnfd::Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|n| {
            aterm_toolchain::release_env::clears(n) || aterm_toolchain::release_env::launcher_env(n)
        }) {
            cmd.env_remove(name);
        }
    }
}

/// Used at each tree-stage boundary. Claims in this sample's private checkout belong to
/// its own children; their internal memory waits cannot preempt their parent.
pub(crate) fn foreground_waiting(root: &Path) -> Result<bool, String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("no capacity ledger")?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let rows = ledger.list().map_err(|e| e.to_string())?;
    Ok(stage_should_yield(&root, &rows))
}

fn stage_should_yield(root: &Path, rows: &[aterm_capacity::Record]) -> bool {
    rows.iter().any(|r| {
        r.need.rank == aterm_capacity::Rank::Release
            || (r.need.rank == aterm_capacity::Rank::Ordinary
                && r.held.is_none()
                && Path::new(&r.checkout) != root)
    })
}

fn sample(root: &Path, result: &Path) -> Result<bool, String> {
    use aterm_capacity::Machine as _;
    crate::exec::group::kill_on_interrupt();
    let admitted_at = Instant::now();
    let claim = match aterm_capacity::admit(
        aterm_capacity::Need::new("verify watch sample").cores(aterm_capacity::cores()),
        aterm_capacity::Admit::Preemptible,
    ) {
        Ok(claim) => claim,
        Err(aterm_capacity::Error::Stopped { .. }) => return Ok(false),
        Err(why) => return Err(why.to_string()),
    };
    let admission_waiting_s = admitted_at.elapsed().as_secs_f64();
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
    let removed: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| {
            aterm_toolchain::release_env::clears(name)
                || aterm_toolchain::release_env::launcher_env(name)
        })
        .collect();
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
        if claim.should_yield().map_err(|e| e.to_string())? {
            return Ok(false);
        }
        // The tree verifier admits each of its own stages. Tool compilation and the compat
        // check need their own bound: the measured tree-compile peak is 26 GiB (§3).
        let memory = if id == "tree" {
            None
        } else {
            Some(
                match aterm_capacity::admit(
                    aterm_capacity::Need::new(format!("verify watch {id}"))
                        .mem_gib(26)
                        .covers(),
                    aterm_capacity::Admit::Preemptible,
                ) {
                    Ok(claim) => claim,
                    Err(aterm_capacity::Error::Stopped { .. }) => return Ok(false),
                    Err(why) => return Err(why.to_string()),
                },
            )
        };
        let mut cmd = crate::exec::Cmd::new(&tools.targo);
        let package = if args[0] == "ship" {
            "aterm-release"
        } else {
            "aterm-verify"
        };
        cmd = cmd
            .args([
                "--unverified",
                "run",
                "--locked",
                "--profile",
                "test",
                "-p",
                package,
                "--",
            ])
            .args(args[1..].iter().copied());
        cmd = cmd
            .env("TRUST_STAGE2_BIN", &tools.stage2_dir)
            .env("PATH", tools.path_with_stage2_first(&env.path))
            .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"))
            .env("CARGO_BUILD_JOBS", claim.cores().to_string())
            .env("BUILD_SLOT_HELD", "1");
        if let Some(memory) = &memory {
            cmd = cmd.env(aterm_capacity::COVER_ENV, memory.record().cover_marker());
        }
        let log = root.join(format!(".git/watch-{id}.log"));
        let began = Instant::now();
        let ran = crate::exec::run(
            &cmd,
            crate::exec::ExecEnv {
                cwd: root,
                path: &env.path,
                scratch: &scratch,
                child_ceiling: None,
                remove_env: &removed_refs,
                add_env: &[],
                stage_groups: None,
                host: &crate::exec::LiveHost,
            },
        );
        fs::write(&log, &ran.output).map_err(|e| e.to_string())?;
        if ran.code == Some(75) || crate::exec::group::stopping() {
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
            claim.cores(),
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
    let text = aterm_toml::to_string(&Sample { rows }).map_err(|e| e.to_string())?;
    fs::write(result, text).map_err(|e| e.to_string())?;
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

fn row(id: &str, code: Option<i32>, text: &str) -> Row {
    let outcome = match code {
        Some(0) => "pass",
        Some(1) if !id.starts_with("inputs") => "red",
        Some(101) if text.contains("could not compile") => "red",
        _ => "could-not-run",
    };
    let mut red_items: Vec<String> = if outcome == "red" {
        text.lines()
            .filter_map(|l| l.trim().strip_prefix("FAIL").map(|s| s.trim().to_owned()))
            .collect()
    } else {
        vec![]
    };
    let mut decisions = Vec::new();
    for line in text
        .lines()
        .filter_map(|l| l.strip_prefix("verify-event: "))
    {
        if let Ok(event) = aterm_json::from_str::<aterm_json::Value>(line)
            && let Some(items) = event.get("findings").and_then(aterm_json::Value::as_array)
        {
            if outcome == "red" {
                red_items.extend(
                    items
                        .iter()
                        .filter_map(aterm_json::Value::as_str)
                        .map(str::to_owned),
                );
            }
            if let Some(items) = event.get("decisions").and_then(aterm_json::Value::as_array) {
                for item in items {
                    if let (Some(id), Some(outcome)) = (
                        item.get("detail").and_then(aterm_json::Value::as_str),
                        item.get("outcome").and_then(aterm_json::Value::as_str),
                    ) {
                        decisions.push(Decision {
                            id: id.into(),
                            outcome: outcome.into(),
                        });
                    }
                }
            }
        }
    }
    if outcome == "red" && red_items.is_empty() {
        red_items.push(id.into());
    }
    red_items.sort();
    red_items.dedup();
    // Keep bounded evidence in the portable journal; the full log stays in the stable clone.
    let log = text
        .lines()
        .rev()
        .take(50)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
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

fn attribute(root: &Path, record: &mut Record) -> Result<(), String> {
    let until = judged::deadline();
    let head = judged::head(root, "origin", until)?;
    let mut previous = judged::records(root, head.as_deref(), until)?;
    previous.sort_by_key(|r| std::cmp::Reverse(r.updated));
    for row in &mut record.rows {
        let wanted = if row.outcome == "red" {
            "pass"
        } else if row.outcome == "pass" {
            "red"
        } else {
            continue;
        };
        let Some(base) = previous.iter().find(|r| {
            r.tree != record.tree
                && r.rows
                    .iter()
                    .any(|old| old.id == row.id && old.outcome == wanted)
        }) else {
            continue;
        };
        let ancestry = judged::command(
            root,
            &["merge-base", "--is-ancestor", &base.commit, &record.commit],
        )
        .status()
        .map_err(|e| e.to_string())?;
        if !ancestry.success() {
            continue;
        }
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
        let range = format!("{}..{}", base.commit, record.commit);
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
mod tests {
    use super::*;
    #[test]
    fn a_sample_has_a_real_session_as_well_as_its_own_group() {
        let topology = |mut command: aterm_uds::spawnfd::Command| {
            let mut child = command.arg("60").spawn().unwrap();
            let pid = i32::try_from(child.id()).unwrap();
            // SAFETY: these integer queries name this test's own unreaped child.
            let (group, session) = unsafe { (libc::getpgid(pid), libc::getsid(pid)) };
            child.kill().unwrap();
            child.wait().unwrap();
            (pid, group, session)
        };
        let (pid, group, session) = topology(sample_command(Path::new("/bin/sleep")));
        assert_eq!((group, session), (pid, pid));
        let mut group_only = aterm_uds::spawnfd::command("/bin/sleep");
        group_only.process_group(0);
        let (pid, group, session) = topology(group_only);
        assert_eq!(group, pid);
        assert_ne!(
            session, pid,
            "a group alone leaves launchd's session inherited"
        );
    }
    #[test]
    fn attribution_resolves_package_aliases_and_submodule_pins() {
        let metadata: aterm_json::Value = aterm_json::from_str(r#"{"packages":[{"name":"alias","manifest_path":"/repo/crates/actual/Cargo.toml","dependencies":[]}]}"#).unwrap();
        let graph = metadata_graph(Path::new("/repo"), &metadata).unwrap();
        let roots = BTreeSet::from(["crates/alias".into()]);
        assert_eq!(
            resolve_roots(Path::new("/repo"), &roots, &metadata, &graph).unwrap(),
            BTreeSet::from(["crates/actual".into()])
        );
        assert!(
            resolve_roots(
                Path::new("/repo"),
                &BTreeSet::from(["crates/unknown".into()]),
                &metadata,
                &graph
            )
            .is_err()
        );
        let paths = BTreeSet::from(["vendor/bus/crates/client".into(), "vendor/bus-other".into()]);
        assert_eq!(
            gitlink_paths(&paths, "160000 commit abc\tvendor/bus\0"),
            BTreeSet::from(["vendor/bus".into(), "vendor/bus-other".into()])
        );
    }
    #[test]
    fn attribution_queries_real_metadata_without_a_compilation_lane() {
        let root = std::fs::canonicalize(aterm_tempfile::manifest_dir().join("../..")).unwrap();
        let links = git(&root, &["ls-tree", "-rz", "--full-tree", "HEAD"]).unwrap();
        let paths = item_paths(
            &root,
            &BTreeSet::from(["crates/aterm-verify".into()]),
            false,
            &links,
        )
        .unwrap();
        assert_eq!(paths, BTreeSet::from(["crates/aterm-verify".into()]));
    }
    #[test]
    fn attribution_preserves_a_nested_crate_at_its_git_pin() {
        let root = std::fs::canonicalize(aterm_tempfile::manifest_dir().join("../..")).unwrap();
        let links = git(&root, &["ls-tree", "-rz", "--full-tree", "HEAD"]).unwrap();
        let evidence = row(
            "tree",
            Some(1),
            "FAIL -p astream-effects at /fleet/other-checkout/vendor/astream/crates/astream-effects/src/lib.rs:12",
        );
        let roots = item_crates(&evidence, &links);
        assert_eq!(
            roots,
            BTreeSet::from([
                "crates/astream-effects".into(),
                "vendor/astream/crates/astream-effects".into()
            ])
        );
        assert_eq!(
            item_paths(&root, &roots, false, &links).unwrap(),
            BTreeSet::from(["vendor/astream".into()])
        );
    }
    #[test]
    fn attribution_keeps_a_git_pin_removed_by_the_fix() {
        let dir = aterm_tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]).unwrap();
        git(dir.path(), &["config", "user.name", "Andrew Yates"]).unwrap();
        git(
            dir.path(),
            &["config", "user.email", "test@example.invalid"],
        )
        .unwrap();
        fs::write(dir.path().join("item"), "seed").unwrap();
        git(dir.path(), &["add", "item"]).unwrap();
        git(dir.path(), &["commit", "-m", "seed"]).unwrap();
        let target = git(dir.path(), &["rev-parse", "HEAD"]).unwrap();
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                &target,
                "vendor/retired",
            ],
        )
        .unwrap();
        git(dir.path(), &["commit", "-m", "pin"]).unwrap();
        let pinned = git(dir.path(), &["rev-parse", "HEAD"]).unwrap();
        git(
            dir.path(),
            &["update-index", "--force-remove", "vendor/retired"],
        )
        .unwrap();
        git(dir.path(), &["commit", "-m", "remove pin"]).unwrap();
        let links = transition_gitlinks(dir.path(), &pinned).unwrap();
        assert_eq!(
            item_crates(
                &row(
                    "tree",
                    Some(1),
                    "error at /fleet/old/vendor/retired/crates/client/src/lib.rs"
                ),
                &links
            ),
            BTreeSet::from(["vendor/retired/crates/client".into()])
        );
    }
    #[test]
    fn stopping_a_sample_first_allows_its_signal_handler_to_clean_up() {
        use std::os::unix::process::ExitStatusExt as _;
        let mut child = aterm_uds::spawnfd::command(aterm_uds::os_path!("sleep"))
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        stop_sample(&mut child).unwrap();
        assert_eq!(
            child.wait().unwrap().signal(),
            Some(libc::SIGTERM),
            "direct SIGKILL would prevent the worker from retiring its nested groups"
        );
    }
    #[test]
    fn failed_renewals_preserve_only_confirmed_time_and_ownership_loss_stops() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let mut lease = Renewal::new(at(120), 1120, start);
        assert!(
            lease
                .finish(
                    Err(PublishError::Unavailable("network".into())),
                    at(30),
                    1150,
                    at(31),
                    1031
                )
                .is_ok()
        );
        assert_eq!(lease.expires, at(120));
        assert_eq!(lease.epoch_until, 1120);
        assert_eq!(lease.next, at(36));
        assert_eq!(lease.deadline(at(87), 1087), at(90));
        assert!(
            !lease.valid(at(31), 1121),
            "sleep or a forward wall-clock jump cannot prolong ownership"
        );
        assert!(
            !lease.valid(at(121), 900),
            "a backward wall-clock jump cannot prolong ownership"
        );
        assert!(lease.finish(Ok(()), at(60), 1180, at(61), 1061).is_ok());
        assert_eq!(lease.expires, at(180));
        assert_eq!(lease.cutoff(), at(150));
        assert!(
            lease
                .finish(Err(PublishError::Lost), at(80), 1200, at(81), 1081)
                .is_err()
        );
        assert!(
            lease
                .finish(
                    Err(PublishError::Unavailable("network".into())),
                    at(145),
                    1265,
                    at(150),
                    1150
                )
                .is_err()
        );
        assert_eq!(lease.expires, at(180));
    }

    #[test]
    fn workspace_seed_survives_source_removal_without_contacting_remote() {
        let dir = aterm_tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main", "source"]).unwrap();
        let source = dir.path().join("source");
        git(&source, &["config", "user.name", "Andrew Yates"]).unwrap();
        git(&source, &["config", "user.email", "test@example.invalid"]).unwrap();
        fs::write(source.join("item"), "independent objects").unwrap();
        git(&source, &["add", "item"]).unwrap();
        git(&source, &["commit", "-m", "seed"]).unwrap();
        // A network-first clone fails against this deliberately unavailable remote.
        git(
            &source,
            &["remote", "add", "origin", "https://invalid.invalid/absent"],
        )
        .unwrap();
        let opts = Options {
            workspace: dir.path().join("lane"),
            remote: "origin".into(),
            branch: "main".into(),
            once: false,
            interval: Duration::from_secs(60),
        };
        let root = prepare_workspace(&opts, &source).unwrap();
        fs::remove_dir_all(&source).unwrap();
        assert_eq!(
            git(&root, &["show", "HEAD:item"]).unwrap(),
            "independent objects"
        );
        assert_eq!(
            git(&root, &["remote", "get-url", "origin"]).unwrap(),
            "https://invalid.invalid/absent"
        );
        assert!(!root.join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn attribution_names_non_merge_suspects_and_only_an_unambiguous_fix() {
        let dir = aterm_tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--bare", "remote"]).unwrap();
        git(dir.path(), &["init", "-b", "main", "source"]).unwrap();
        let root = dir.path().join("source");
        git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
        git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                dir.path().join("remote").to_str().unwrap(),
            ],
        )
        .unwrap();
        let commit = |name: &str, text: &str| {
            fs::write(root.join("item"), text).unwrap();
            git(&root, &["add", "item"]).unwrap();
            git(&root, &["commit", "-m", name]).unwrap();
            git(&root, &["rev-parse", "HEAD"]).unwrap()
        };
        let record = |commit: &str, state: &str, updated| Record {
            commit: commit.into(),
            tree: git(&root, &["rev-parse", &format!("{commit}^{{tree}}")]).unwrap(),
            host: "test".into(),
            verifier: commit.into(),
            inputs: "test".into(),
            token: commit.into(),
            state: state.into(),
            started: updated,
            updated,
            lease_until: 0,
            rows: vec![row(
                "tree",
                Some(if state == "red" { 1 } else { 0 }),
                "whole-workspace item",
            )],
        };
        let base = commit("base", "good");
        assert!(
            judged::append(
                &root,
                "origin",
                None,
                &record(&base, "pass", 1),
                judged::deadline()
            )
            .unwrap()
        );
        let red = commit("red", "bad");
        let mut red_record = record(&red, "red", 2);
        attribute(&root, &mut red_record).unwrap();
        assert_eq!(
            red_record.rows[0].suspects.as_slice(),
            std::slice::from_ref(&red)
        );
        let head = judged::head(&root, "origin", judged::deadline())
            .unwrap()
            .unwrap();
        assert!(
            judged::append(
                &root,
                "origin",
                Some(&head),
                &red_record,
                judged::deadline()
            )
            .unwrap()
        );
        let fix = commit("fix", "good again");
        let fixed_tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let merge = git(
            &root,
            &[
                "commit-tree",
                &fixed_tree,
                "-p",
                &fix,
                "-p",
                &base,
                "-m",
                "merge",
            ],
        )
        .unwrap();
        let mut passing = record(&merge, "pass", 3);
        attribute(&root, &mut passing).unwrap();
        assert_eq!(
            passing.rows[0].fixed_by.as_deref(),
            Some(fix.as_str()),
            "the landing merge is not a second suspect"
        );
        let extra = commit("another change", "good with another edit");
        let mut ambiguous = record(&extra, "pass", 4);
        attribute(&root, &mut ambiguous).unwrap();
        assert!(
            ambiguous.rows[0].fixed_by.is_none(),
            "two non-merge changes cannot be attributed to one fix"
        );
    }
    #[test]
    fn only_foreground_waiters_outside_the_sample_and_every_release_preempt_a_stage() {
        let mut row = aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::Holder,
            pid: 1,
            start_ms: 1,
            seq: 0,
            checkout: "/sample".into(),
            need: aterm_capacity::Need::new("fixture"),
            timing: aterm_capacity::TimingEvidence::default(),
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        assert!(!stage_should_yield(
            Path::new("/sample"),
            std::slice::from_ref(&row)
        ));
        row.checkout = "/other".into();
        assert!(stage_should_yield(
            Path::new("/sample"),
            std::slice::from_ref(&row)
        ));
        row.held = Some(aterm_capacity::Held {
            admitted_ms: 1,
            cores: 0,
            alone: false,
        });
        assert!(!stage_should_yield(
            Path::new("/sample"),
            std::slice::from_ref(&row)
        ));
        row.need.rank = aterm_capacity::Rank::Release;
        assert!(stage_should_yield(Path::new("/sample"), &[row]));
    }
    #[test]
    fn partial_or_unavailable_samples_never_become_green() {
        let pass = row("tree", Some(0), "ok tests");
        assert_eq!(verdict(std::slice::from_ref(&pass)), "could-not-run");
        assert_eq!(verdict(&vec![pass; 4]), "could-not-run");
        let rows = [
            "inputs",
            "stable-x86_64",
            "tree",
            "export",
            "cut",
            "inputs-end",
        ]
        .map(|id| row(id, Some(0), "ok"));
        assert_eq!(verdict(&rows), "pass");
        assert_eq!(verdict(&[row("compat", Some(1), "error[E0502]")]), "red");
        assert_eq!(
            verdict(&[row("tree", Some(3), "NOT RUN compiler")]),
            "could-not-run"
        );
        assert!(
            row("tree", Some(3), "FAIL infrastructure")
                .red_items
                .is_empty()
        );
        assert_eq!(
            row("tree", Some(1), "  FAIL -p agh --lib jq_all").red_items,
            ["-p agh --lib jq_all"]
        );
    }
    #[test]
    fn changed_or_missing_input_seals_cannot_qualify_a_sample() {
        let first = "a".repeat(64);
        assert!(seals_match(&format!("inputs: measured\n{first}\n"), &first));
        assert!(!seals_match(&first, &"b".repeat(64)));
        assert!(!seals_match("no seal", "no seal"));
    }

    #[test]
    fn fresh_workspace_is_clean_and_an_existing_checkout_is_preserved() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main", "source"]).unwrap();
        let source = root.join("source");
        git(&source, &["config", "user.name", "Andrew Yates"]).unwrap();
        git(&source, &["config", "user.email", "test@example.invalid"]).unwrap();
        fs::write(source.join("tracked"), "original").unwrap();
        git(&source, &["add", "tracked"]).unwrap();
        git(&source, &["commit", "-m", "base"]).unwrap();
        git(
            &source,
            &["remote", "add", "origin", source.to_str().unwrap()],
        )
        .unwrap();
        let mut opts = Options {
            workspace: root.join("lane"),
            remote: "origin".into(),
            branch: "main".into(),
            once: true,
            interval: Duration::from_secs(1),
        };
        let physical = prepare_workspace(&opts, &source).unwrap();
        assert!(
            git(&opts.workspace, &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        let alias = root.join("lane-alias");
        std::os::unix::fs::symlink(&opts.workspace, &alias).unwrap();
        opts.workspace = alias;
        assert_eq!(
            prepare_workspace(&opts, &source).unwrap(),
            physical,
            "all children and dependency attribution receive the same physical workspace"
        );
        opts.workspace = source.clone();
        fs::write(source.join("tracked"), "person's work").unwrap();
        assert!(prepare_workspace(&opts, &source).is_err());
        assert_eq!(
            fs::read_to_string(source.join("tracked")).unwrap(),
            "person's work"
        );
    }
    #[test]
    fn suspects_include_transitive_workspace_dependencies_but_not_unrelated_crates() {
        let graph = BTreeMap::from([
            (
                "a".into(),
                ("crates/a".into(), BTreeSet::from(["b".into()])),
            ),
            (
                "b".into(),
                ("crates/b".into(), BTreeSet::from(["c".into()])),
            ),
            ("c".into(), ("crates/c".into(), BTreeSet::new())),
            (
                "unrelated".into(),
                ("crates/unrelated".into(), BTreeSet::new()),
            ),
        ]);
        let roots = item_crates(&row("tree", Some(1), "FAIL -p a --lib item"), "");
        assert_eq!(
            dependency_closure(&roots, &graph),
            BTreeSet::from(["crates/a".into(), "crates/b".into(), "crates/c".into()])
        );
        let roots = item_crates(
            &row("compat", Some(1), "error at crates/b/src/lib.rs:12"),
            "",
        );
        assert_eq!(roots, BTreeSet::from(["crates/b".into()]));
        let metadata = aterm_json::json!({"packages": [
            {"name": "alias", "manifest_path": "/repo/crates/a/Cargo.toml", "dependencies": [{"name": "renamed", "path": "/repo/crates/b"}, {"name": "external", "path": null}]},
            {"name": "actual", "manifest_path": "/repo/crates/b/Cargo.toml", "dependencies": [{"name": "c", "path": "/repo/crates/c"}]},
            {"name": "c", "manifest_path": "/repo/crates/c/Cargo.toml", "dependencies": [{"name":"bus", "path":"/repo/vendor/bus/crates/client"}]},
            {"name": "unrelated", "manifest_path": "/repo/crates/unrelated/Cargo.toml", "dependencies": []}
        ]});
        let graph = metadata_graph(Path::new("/repo"), &metadata).unwrap();
        assert_eq!(
            dependency_closure(&BTreeSet::from(["crates/a".into()]), &graph),
            BTreeSet::from([
                "crates/a".into(),
                "crates/b".into(),
                "crates/c".into(),
                "vendor/bus/crates/client".into()
            ])
        );
    }
}
