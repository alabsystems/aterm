// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The follower: judges origin/main's newest tip after the landings that made it, one pass at a
//! time, and says what it found as a GitHub commit status on that commit. A pass runs every row
//! to its end (a red stops no later row), and nothing preempts it. A one-shot pass returns repair
//! context to its caller. The persistent follower starts an independent repair worker unless one
//! still works or could not finish the same reds. The verdict is the status; nothing else is kept.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "watch_service.rs"]
mod service;

/// The context the follower's commit statuses carry.
pub const CONTEXT: &str = crate::FOLLOWER_CONTEXT;

const HELP: &str = "verify watch [--workspace DIR] [--remote NAME] [--branch BRANCH] [--once] [--interval SECONDS] [--repair-cwd DIR]\n\
Follows origin/main in a dedicated clone: each pass judges the newest tip no follower has\n\
judged, with the verifier built from that tip, and posts the verdict as a commit status\n\
(context aterm/follower) through agh. A pass runs the release's rows (ship inputs, the\n\
x86_64 compat compile, verify release --tree, verify cells, verify appkit on a Mac with a\n\
desk session, ship's export and cut checks, the inputs recheck) to their ends: a red stops\n\
no later row, and no other work preempts it. Reds are named by package, target and test.\n\
A persistent follower's red starts one Claude Code repair worker in DIR (default: this repository's main\n\
checkout) unless the last one still works or could not finish the same reds; it bisects over\n\
first-parent landings and fixes forward independently while the follower continues.\n\
--once judges one tip and returns repair context to its caller: use native subagents or work\n\
directly, with your own shell tools for commands. It starts and retires no agent sessions.\n\
No hook, gate or publication.\n\
verify watch --install [--workspace DIR] [--interval SECONDS] [--repair-cwd DIR] installs it\n\
as this user's LaunchAgent, replacing an earlier install.\n";

/// The longest a GitHub status description is, in characters.
const DESCRIPTION_CHARS: usize = 140;
/// How many first-parent commits before a red tip a pass reads for the last green one.
const WALK: usize = 100;
/// Leave room for the persistent worker's instructions within the control task's 64 KiB limit.
const MAX_REPAIR_CONTEXT_BYTES: usize = 60 * 1024;

#[derive(Clone, Debug)]
struct Options {
    workspace: PathBuf,
    remote: String,
    branch: String,
    once: bool,
    interval: Duration,
    /// Where a repair worker starts: `--repair-cwd`, else [`main_checkout`].
    repair_cwd: Option<PathBuf>,
}

impl Options {
    fn repair_mode<'a>(&self, worker: &'a mut Option<Repair>) -> RepairMode<'a> {
        if self.once {
            RepairMode::Caller
        } else {
            RepairMode::Persistent(worker)
        }
    }

    /// The folder a repair worker starts in. A worktree is removed once its branch lands, so
    /// the default is the main checkout of `cwd`'s repository, never `cwd` itself.
    fn repair_cwd(&self, cwd: &Path) -> Result<PathBuf, String> {
        self.repair_cwd
            .clone()
            .map_or_else(|| main_checkout(cwd), Ok)
    }
}

/// The main checkout of `cwd`'s repository: the folder that holds its common `.git`.
fn main_checkout(cwd: &Path) -> Result<PathBuf, String> {
    let common = PathBuf::from(git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    match (common.file_name(), common.parent()) {
        (Some(name), Some(parent)) if name == ".git" => Ok(parent.to_owned()),
        _ => Err(format!(
            "{} is no main checkout's .git; name the repair folder with --repair-cwd",
            common.display()
        )),
    }
}

fn options(args: &[String], cwd: &Path) -> Result<Options, String> {
    let home = std::env::var_os("HOME").ok_or("watch needs HOME")?;
    let mut opts = Options {
        workspace: PathBuf::from(home).join(".local/state/aterm-verify/.ubd/watch/checkout"),
        remote: "origin".into(),
        branch: "main".into(),
        once: false,
        interval: Duration::from_secs(60),
        repair_cwd: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--once" => opts.once = true,
            "--workspace" => {
                opts.workspace = cwd.join(it.next().ok_or("--workspace needs a directory")?)
            }
            "--repair-cwd" => {
                opts.repair_cwd = Some(cwd.join(it.next().ok_or("--repair-cwd needs a directory")?))
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

/// A git command in `root`, with no inherited token.
fn git_command(root: &Path) -> aterm_uds::spawnfd::Command {
    let mut cmd = crate::git_provider::command();
    cmd.current_dir(root).env_remove("GITHUB_TOKEN");
    cmd
}

/// The output of a command that must end within a minute.
fn output(cmd: &mut aterm_uds::spawnfd::Command, input: &[u8]) -> Result<String, String> {
    let out = aterm_uds::deadline::output_with_input_until(
        cmd,
        aterm_uds::secret::Secret::new(input),
        Instant::now() + Duration::from_secs(60),
        64 * 1024 * 1024,
    )
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    output(git_command(root).args(args), &[])
        .map(|s| s.trim().to_owned())
        .map_err(|why| format!("git {}: {why}", args.join(" ")))
}

/// The follower's controller and its pass's two internal halves. The pass bootstrap (`--sample`)
/// builds the tip's verifier and replaces itself with it (`--pass`), so every row and the
/// verdict come from the tip being judged.
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
    if args.first().is_some_and(|s| s == "--pass") {
        return match args.get(1) {
            Some(result) => match pass(cwd, Path::new(result)) {
                Ok(true) => 0,
                Ok(false) => 75,
                Err(why) => {
                    eprintln!("watch pass: {why}");
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
    // A host-wide lock, independent of the chosen checkout: at most one follower per Mac.
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
        .map_err(|e| format!("another follower runs on this Mac: {e}"))?;
    let mut opts = opts.clone();
    let repair_cwd = opts.repair_cwd(cwd)?;
    opts.workspace = prepare_workspace(&opts, cwd)?;
    let mut world = Live::open(&opts.workspace, &repair_cwd)?;
    let stop = aterm_toolchain::interrupt::Watch::enter();
    let mut declined = None;
    let mut worker = None;
    loop {
        if stop.caught().is_some() {
            return Ok(());
        }
        let result = attempt(&opts, &mut world, &mut worker);
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
    /// A tip was judged, or its pass was interrupted.
    Judged,
    /// No pass started, and why.
    Declined(String),
}

/// What a turn of the loop leaves in the follower's own log: a failure or a declined start once
/// per reason, `last` being the line last written. A follower that starts nothing for hours, or
/// that GitHub refuses at every turn, says why once, not at every turn; a pass in between makes
/// the reason news again.
fn log_line(last: &mut Option<String>, result: &Result<Start, String>) -> Option<String> {
    let line = match result {
        Err(why) => format!("verify watch: pass could not run: {why}"),
        Ok(Start::Declined(why)) => format!("verify watch: no pass started: {why}"),
        Ok(Start::Judged) => {
            *last = None;
            return None;
        }
    };
    if last.as_ref() == Some(&line) {
        return None;
    }
    *last = Some(line.clone());
    Some(line)
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
        // Seed independent objects locally; every pass still fetches the actual remote.
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

/// One follower status on a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    /// `pending`, `success`, `failure` or `error`.
    state: String,
    description: String,
}

impl Status {
    fn new(state: &str, description: String) -> Self {
        Self {
            state: state.into(),
            description: clip(&description, DESCRIPTION_CHARS),
        }
    }

    /// Whether this status is a verdict: a commit that has one is not judged again.
    fn verdict(&self) -> bool {
        matches!(self.state.as_str(), "success" | "failure" | "error")
    }

    /// The repair worker a verdict carries: a tab ID, or the `s-` ID of an older worker.
    fn worker(&self) -> Option<&str> {
        if !matches!(self.state.as_str(), "success" | "failure") {
            return None;
        }
        let rest = self.description.strip_prefix("repair ").or_else(|| {
            if self.state != "failure" {
                return None;
            }
            let (count, rest) = self.description.split_once(" red, repair ")?;
            (!count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())).then_some(rest)
        })?;
        let sid = rest.split([':', ' ', ';']).next()?;
        let named = aterm_types::tab_id::is_tab_id(sid) || aterm_types::tab_id::is_legacy(sid);
        named.then_some(sid)
    }

    fn repair(&self) -> Option<Repair> {
        let sid = self.worker()?;
        let prefix = format!("repair {sid} assigned=");
        let assignment = self
            .description
            .strip_prefix(&prefix)
            .and_then(|tail| tail.split_once(';').map(|(hash, _)| hash))
            .filter(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .map(str::to_owned);
        Some(Repair {
            sid: sid.to_owned(),
            assignment,
        })
    }

    /// Only this producer header establishes that a failure assigned no worker.
    fn worker_free_failure(&self) -> bool {
        self.state == "failure"
            && self
                .description
                .split_once(" red:")
                .is_some_and(|(count, _)| {
                    !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())
                })
    }
}

/// Immutable worker custody. The status header is `repair <sid> assigned=<64 hex>; …`.
/// Its full hash precedes clipped human detail. Old or malformed assignments retain the SID
/// with unknown assignment: prose equality is never evidence that the worker tried a red.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Repair {
    sid: String,
    assignment: Option<String>,
}

impl Repair {
    fn header(&self) -> String {
        format!(
            "repair {} assigned={}; ",
            self.sid,
            self.assignment.as_deref().unwrap_or("?")
        )
    }
}

/// The original repair inputs: its known green frontier, if any, and canonical full red keys.
/// The same labels after a later green are a new failure for a worker that already failed.
fn assignment(green: Option<&str>, rows: &[Row]) -> String {
    let mut keys: Vec<_> = rows
        .iter()
        .filter(|row| row.outcome == "red")
        .flat_map(|row| {
            row.red_items
                .iter()
                .map(move |red| (row.id.as_str(), red.as_str()))
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-follower-repair-assignment\0");
    match green {
        Some(green) => {
            hash.update([1]);
            hash.update((green.len() as u64).to_le_bytes());
            hash.update(green.as_bytes());
        }
        None => hash.update([0]),
    }
    for (row, red) in keys {
        for field in [row, red] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

/// Where a repair worker's task stands, as `aterm ctl @<sid> task` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Worker {
    /// It works, or it is stalled at a start or a hold it leaves by itself; or its record could
    /// not be read. It holds the one repair place.
    Working,
    /// It ended unfinished (`failed`), or is stalled where only a mark moves it: it keeps the
    /// reds it was given.
    Failed,
    /// It finished, exited, runs no task, or its session is gone.
    Ended,
}

/// A worker's [`Worker`] from its task record (`OK task rev=… state=<s> … why=<w> …`).
fn task_state(record: &str) -> Worker {
    let word = |key: &str| {
        record
            .split_whitespace()
            .find_map(|word| word.strip_prefix(key))
    };
    match (word("state="), word("why=")) {
        (Some("working"), _) | (Some("stalled"), Some("startup" | "held")) => Worker::Working,
        (Some("failed" | "stalled"), _) => Worker::Failed,
        (Some("done" | "exited"), _) => Worker::Ended,
        (None, _) if record.split_whitespace().eq(["OK", "task", "-"]) => Worker::Ended,
        _ => Worker::Working,
    }
}

/// What a pass reads and writes beyond its checkout: main's commit statuses and the repair
/// worker. The controller's is [`Live`]; a test scripts one.
trait World {
    /// The follower's statuses on `commit`, newest first.
    fn statuses(&mut self, commit: &str) -> Result<Vec<Status>, String>;
    /// Post `status` on `commit`.
    fn post(&mut self, commit: &str, status: &Status) -> Result<(), String>;
    /// Return a one-shot pass's repair context to its caller.
    fn caller_repair(&mut self, context: &str) -> Result<(), String>;
    /// Where the worker `sid`'s task stands.
    fn worker(&mut self, sid: &str) -> Worker;
    /// End the agent of `sid`, a worker that no longer holds the repair place, so ended workers
    /// do not pile up idle.
    fn retire(&mut self, sid: &str);
    /// Start a repair worker on `task`: its sid.
    fn spawn(&mut self, task: &str) -> Result<String, String>;
}

/// A one-shot caller owns its repair; only the persistent follower manages an agent session.
enum RepairMode<'a> {
    Caller,
    Persistent(&'a mut Option<Repair>),
}

fn write_caller_repair(output: &mut dyn std::io::Write, context: &str) -> std::io::Result<()> {
    writeln!(output, "{context}")?;
    output.flush()
}

/// The live world: GitHub through the verifier's linked agh, routed by the clone's own remote,
/// and the workers through `aterm ctl`.
struct Live {
    root: PathBuf,
    slug: String,
    repair_cwd: PathBuf,
}

impl Live {
    fn open(root: &Path, repair_cwd: &Path) -> Result<Self, String> {
        let until = Instant::now() + Duration::from_secs(60);
        let run = |tool, args: &[std::ffi::OsString]| {
            crate::remote::run(root, &git_command, tool, args, until)
        };
        let remote = crate::remote::Remote::open(root, "origin", &run)?;
        let slug = remote
            .github()
            .ok_or("the follower posts commit statuses, so its origin must be a GitHub repository")?
            .to_owned();
        Ok(Self {
            root: root.to_owned(),
            slug,
            repair_cwd: repair_cwd.to_owned(),
        })
    }

    fn agh(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        let out = crate::remote::run(
            &self.root,
            &git_command,
            crate::remote::Tool::Agh,
            &args,
            Instant::now() + Duration::from_secs(120),
        )?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "agh {}: {}",
                args.first()
                    .map(|a| a.to_string_lossy())
                    .unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn ctl(args: &[&str], input: &[u8]) -> Result<String, String> {
        let mut cmd = aterm_uds::spawnfd::command("aterm");
        cmd.arg("ctl").args(args);
        output(&mut cmd, input).map_err(|why| format!("aterm ctl {}: {why}", args[0]))
    }
}

/// The follower's statuses, `(state, description)` newest first, in GitHub's answer to
/// `GET /repos/{owner}/{repo}/commits/{sha}/statuses`; `None` where the list or its relevant
/// fields are malformed, so unread evidence never becomes an absent follower status.
pub fn follower_statuses(answer: &aterm_json::Value) -> Option<Vec<(String, String)>> {
    let mut statuses = Vec::new();
    for row in answer.as_array()? {
        let context = row["context"].as_str()?;
        if context.is_empty() {
            return None;
        }
        if context != CONTEXT {
            continue;
        }
        let state = row["state"].as_str()?;
        let description = row["description"].as_str()?;
        if !matches!(state, "pending" | "success" | "failure" | "error") {
            return None;
        }
        statuses.push((state.to_owned(), description.to_owned()));
    }
    Some(statuses)
}

impl World for Live {
    fn statuses(&mut self, commit: &str) -> Result<Vec<Status>, String> {
        let path = format!("repos/{}/commits/{commit}/statuses", self.slug);
        let body = self.agh(&["api", &path, "--all"])?;
        let value: aterm_json::Value =
            aterm_json::from_slice(&body).map_err(|e| format!("{path}: {e}"))?;
        let rows =
            follower_statuses(&value).ok_or_else(|| format!("{path}: not a valid status list"))?;
        Ok(rows
            .into_iter()
            .map(|(state, description)| Status { state, description })
            .collect())
    }

    fn post(&mut self, commit: &str, status: &Status) -> Result<(), String> {
        let path = format!("repos/{}/statuses/{commit}", self.slug);
        self.agh(&[
            "api",
            &path,
            "-X",
            "POST",
            "-f",
            &format!("state={}", status.state),
            "-f",
            &format!("context={CONTEXT}"),
            "-f",
            &format!("description={}", status.description),
        ])
        .map(drop)
    }

    fn worker(&mut self, sid: &str) -> Worker {
        match Self::ctl(&[&format!("@{sid}"), "task"], &[]) {
            Ok(record) => task_state(&record),
            Err(why) if why.contains("no such session") => Worker::Ended,
            // Unread is not ended: a second worker beside a live one is the failure to avoid.
            Err(_) => Worker::Working,
        }
    }

    fn caller_repair(&mut self, context: &str) -> Result<(), String> {
        write_caller_repair(&mut std::io::stdout().lock(), context)
            .map_err(|error| error.to_string())
    }

    fn retire(&mut self, sid: &str) {
        // `quiet=60`: a tab a person typed in during the last minute is theirs, and stays.
        if let Err(why) = Self::ctl(&[&format!("@{sid}"), "signal", "term", "quiet=60"], &[]) {
            eprintln!("verify watch: worker {sid} left as it is: {why}");
        }
    }

    fn spawn(&mut self, task: &str) -> Result<String, String> {
        eprintln!(
            "verify watch: the persistent follower needs an independent repair session while it continues judging main"
        );
        let cwd = format!("cwd={}", self.repair_cwd.display());
        let reply = Self::ctl(
            &[
                "spawn",
                "agent=claude",
                // The follower has no tab of its own: a worker it starts opens behind the
                // person's work, never in front of it.
                "raise=false",
                &cwd,
                "why=the follower found main red: bisect and fix forward",
                "label=follower-repair",
                "-",
            ],
            task.as_bytes(),
        )?;
        reply
            .lines()
            .find_map(|line| line.strip_prefix("OK "))
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
            .ok_or_else(|| format!("aterm ctl spawn answered {reply:?}"))
    }
}

fn host() -> String {
    aterm_uds::process::kernel_host_name().unwrap_or_else(|| "this Mac".into())
}

/// Put stable controller selectors before the human host name so clipping a long host never
/// loses them. Missing identities remain absent; the read above a claim never mints a node.
fn pending_status(host: &str, node: Option<&str>, session: Option<&str>) -> Status {
    let mut description = "judging".to_owned();
    if let Some(node) = node.filter(|node| aterm_net::fleet::is_node_id(node)) {
        description.push_str(&format!(" node={node}"));
    }
    if let Some(session) = session.filter(|session| {
        aterm_types::tab_id::is_tab_id(session) || aterm_types::tab_id::is_legacy(session)
    }) {
        description.push_str(&format!(" session={session}"));
    }
    description.push_str(&format!(" on {host}"));
    Status::new("pending", description)
}

/// One turn: fetch main, and judge its newest tip unless a follower already has.
fn attempt(
    opts: &Options,
    world: &mut dyn World,
    worker: &mut Option<Repair>,
) -> Result<Start, String> {
    let root = &opts.workspace;
    // verify watch follows the invoking checkout's arbitrary remote, which may be private. URL
    // shape alone is no authority to classify this read as credential-less.
    git(root, &["fetch", "--quiet", "origin", &opts.branch])?;
    let tip = git(root, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    if let Some(done) = world.statuses(&tip)?.into_iter().find(Status::verdict) {
        return Ok(Start::Declined(format!(
            "{tip} has the follower's {}: {}",
            done.state, done.description
        )));
    }
    let node = aterm_types::dirs::state_dir()
        .and_then(|dir| aterm_net::fleet_member::read_node_id(&dir).ok().flatten());
    let session = std::env::var("ATERM_SESSION_ID").ok();
    world.post(
        &tip,
        &pending_status(&host(), node.as_deref(), session.as_deref()),
    )?;
    let Some(sample) = judge(root, &tip).unwrap_or_else(|why| {
        Some(Sample {
            state: "could-not-run".into(),
            rows: vec![could_not_run("follower", why)],
        })
    }) else {
        // The controller was stopped: the tip keeps its pending status and is judged again.
        return Ok(Start::Judged);
    };
    let history = git(
        root,
        &[
            "rev-list",
            "--first-parent",
            &format!("--max-count={}", WALK + 2),
            &tip,
        ],
    )
    .and_then(|text| {
        let shallow = match git(root, &["rev-parse", "--is-shallow-repository"])?.as_str() {
            "false" => false,
            "true" => true,
            _ => return Err("cannot decide whether follower history is shallow".into()),
        };
        Ok((text, shallow))
    });
    let (before, history_complete) = first_parent_history(history);
    let status = conclude(
        world,
        &tip,
        &before,
        history_complete,
        &sample,
        opts.repair_mode(worker),
        &opts.workspace,
    )?;
    println!(
        "verify watch: {tip} {}: {}",
        status.state, status.description
    );
    for row in &sample.rows {
        for red in &row.red_items {
            println!("verify watch: red {}: {red}", row.id);
        }
    }
    Ok(Start::Judged)
}

/// The query includes the tip so a true root is distinguishable from an unread `tip^`.
/// An extra ancestor exposes truncation; shallow history never establishes the absence of workers.
fn first_parent_history(result: Result<(String, bool), String>) -> (Vec<String>, bool) {
    match result {
        Ok((text, shallow)) => {
            if text.lines().next().is_none() {
                return (Vec::new(), false);
            }
            let mut before: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let complete = !shallow && before.len() <= WALK;
            before.truncate(WALK);
            (before, complete)
        }
        Err(why) => {
            eprintln!("verify watch: first-parent history could not be read: {why}");
            (Vec::new(), false)
        }
    }
}

/// What the commits before a red tip say: the newest green, the oldest red since it, and the
/// newest verdict that carries a repair worker. Caller-only failures do not replace that custody.
#[derive(Debug, Default, PartialEq, Eq)]
struct Before {
    green: Option<String>,
    oldest_red: Option<String>,
    /// The newest failure or the green frontier that carries a worker.
    repair: Option<Repair>,
    /// Every status was read back to the green, or to the end of the commits given. Where not,
    /// a worker no status read names may still work.
    complete: bool,
}

/// Read the follower's statuses of `before` (first-parent commits, newest first) back to the
/// newest green. A read that fails ends the walk with what it found.
fn walk(world: &mut dyn World, before: &[String], history_complete: bool) -> Before {
    let mut found = Before {
        complete: history_complete,
        ..Before::default()
    };
    for commit in before {
        let statuses = match world.statuses(commit) {
            Ok(statuses) => statuses,
            Err(why) => {
                eprintln!(
                    "verify watch: the walk to the last green tip stopped at {commit}: {why}"
                );
                found.complete = false;
                break;
            }
        };
        let Some(status) = statuses.into_iter().find(Status::verdict) else {
            continue;
        };
        match status.state.as_str() {
            "success" => {
                found.green = Some(commit.clone());
                if found.repair.is_none() {
                    found.repair = status.repair();
                }
                found.complete = true;
                break;
            }
            "failure" => {
                found.oldest_red = Some(commit.clone());
                let repair = status.repair();
                if repair.is_none() && !status.worker_free_failure() {
                    found.complete = false;
                    break;
                }
                if found.repair.is_none() {
                    found.repair = repair;
                }
            }
            _ => {}
        }
    }
    found
}

/// Post `sample`'s verdict on `tip`. A caller gets repair context without managing any worker.
/// A persistent red names one repair worker in its failure: the one
/// `worker` or the newest failure names while it works, or while it could not finish these same
/// reds (a fresh one would meet what stopped it); else a new one, the ended one retired. Where
/// the statuses could not be read back to a worker or a green, none is started: one may work.
fn conclude(
    world: &mut dyn World,
    tip: &str,
    before: &[String],
    history_complete: bool,
    sample: &Sample,
    mode: RepairMode<'_>,
    root: &Path,
) -> Result<Status, String> {
    let status = match sample.state.as_str() {
        "pass" => {
            let repair = match mode {
                RepairMode::Persistent(worker) => worker.clone(),
                RepairMode::Caller => None,
            }
            .or_else(|| walk(world, before, history_complete).repair);
            let header = repair.as_ref().map_or_else(String::new, Repair::header);
            Status::new(
                "success",
                format!(
                    "{header}pass on {}: {}",
                    host(),
                    sample
                        .rows
                        .iter()
                        .map(|row| row.id.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            )
        }
        "red" => {
            let reds = reds(&sample.rows);
            let failure = |repair: Option<&Repair>| {
                let header = repair.map_or_else(String::new, Repair::header);
                Status::new(
                    "failure",
                    format!("{header}{} red: {}", reds.len(), reds.join("; ")),
                )
            };
            let earlier = walk(world, before, history_complete);
            let worker = match mode {
                RepairMode::Caller => {
                    let context = repair_context(tip, &earlier, &sample.rows, root);
                    let written = world.caller_repair(&format!(
                        "Use your harness's native subagents for same-machine delegation, or work directly; \
                         use your own shell/background tools for commands.\n\n{context}"
                    ));
                    // This pass owns no worker. The next persistent follower finds existing
                    // custody in the worker-bearing failure, beyond this caller-only status.
                    let status = failure(None);
                    let posted = world.post(tip, &status);
                    return match (posted, written) {
                        (Err(post), Err(write)) => {
                            Err(format!("{post}; caller repair output: {write}"))
                        }
                        (Err(post), Ok(())) => Err(post),
                        (Ok(()), Err(write)) => Err(format!("caller repair output: {write}")),
                        (Ok(()), Ok(())) => Ok(status),
                    };
                }
                RepairMode::Persistent(worker) => worker,
            };
            let named = worker.clone().or_else(|| earlier.repair.clone());
            let held = named.map(|repair| {
                let state = world.worker(&repair.sid);
                (repair, state)
            });
            let current = assignment(earlier.green.as_deref(), &sample.rows);
            *worker = match held {
                Some((repair, Worker::Working)) => Some(repair),
                Some((repair, Worker::Failed))
                    if repair
                        .assignment
                        .as_ref()
                        .is_none_or(|assigned| *assigned == current) =>
                {
                    Some(repair)
                }
                unknown if !earlier.complete => unknown.map(|(repair, _)| repair),
                ended => {
                    if let Some((repair, _)) = ended {
                        world.retire(&repair.sid);
                    }
                    let task = repair_task(tip, &earlier, &sample.rows, root);
                    match world.spawn(&task) {
                        Ok(sid) => Some(Repair {
                            sid,
                            assignment: Some(current),
                        }),
                        Err(why) => {
                            eprintln!("verify watch: no repair worker started: {why}");
                            None
                        }
                    }
                }
            };
            failure(worker.as_ref())
        }
        _ => {
            let row = sample
                .rows
                .iter()
                .find(|row| row.outcome != "pass")
                .map_or_else(
                    || "no row".to_owned(),
                    |row| {
                        format!(
                            "{}: {}",
                            row.id,
                            row.log
                                .lines()
                                .rev()
                                .find(|l| !l.trim().is_empty())
                                .unwrap_or("")
                        )
                    },
                );
            Status::new("error", format!("could not run {row}"))
        }
    };
    world.post(tip, &status)?;
    Ok(status)
}

/// Every red of a pass, in row order, each once.
fn reds(rows: &[Row]) -> Vec<String> {
    let mut reds: Vec<String> = Vec::new();
    for red in rows.iter().flat_map(|row| &row.red_items) {
        if !reds.contains(red) {
            reds.push(red.clone());
        }
    }
    reds
}

/// `text` in at most `chars` characters, an ellipsis ending one that was cut.
fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(chars.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Bounded context shared by the caller and the persistent follower's repair worker. The bisect runs
/// between the tip, which these reds were seen on, and the green, the one tip known good; the
/// oldest red since the green may have been red for other tests, so it is only named.
fn repair_context(tip: &str, earlier: &Before, rows: &[Row], root: &Path) -> String {
    let (green_line, range) = match &earlier.green {
        Some(green) => (
            format!("The last green first-parent tip is {green}."),
            format!("git bisect start --first-parent {tip} {green}"),
        ),
        None => (
            "No first-parent tip read before it is green: mark a good one yourself (`git bisect good <rev>`) once the red tests pass there.".to_owned(),
            format!("git bisect start --first-parent {tip}"),
        ),
    };
    let oldest = earlier.oldest_red.as_ref().map_or_else(String::new, |red| {
        format!(" The oldest red tip since then is {red}; its reds may be other ones.")
    });
    let mut context = format!(
        "The follower (`verify watch`, commit status context {CONTEXT}) judged origin/main red at {tip}. \
         {green_line}{oldest}\n\nRed, by row, then package, target and test:\n"
    );
    let instructions = format!(
        "\nFind the landing that turned each red and fix it forward:\n\
         1. Work in a new worktree of your own under ~/.ubd, never in the follower's checkout {}.\n\
         2. Run each red test on origin/main first. Where every one passes there, a landing since {tip} \
         fixed them: name it and end.\n\
         3. Bisect over first-parent landings only, running only the red tests, one at a time: \
         `{range}`, then `git bisect run targo --unverified test -p <package> <target> -- <test> --exact`.\n\
         4. Fix forward on a branch and land it with `targo --unverified verify land`. Revert nothing.\n\n\
         A red that does not reproduce is read for a product cause first. Delete a test only where its \
         own timing is the shown cause, never only because its red did not reproduce.\n",
        root.display()
    );
    let notice = "\nMore red details remain in this pass's row logs.\n";
    let mut room =
        MAX_REPAIR_CONTEXT_BYTES.saturating_sub(context.len() + instructions.len() + notice.len());
    for (row, red) in rows
        .iter()
        .filter(|row| row.outcome == "red")
        .flat_map(|row| row.red_items.iter().map(move |red| (row, red)))
    {
        let line = format!("- {}: {red}\n", row.id);
        if line.len() > room {
            let mut end = room;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            context.push_str(&line[..end]);
            context.push_str(notice);
            break;
        }
        context.push_str(&line);
        room -= line.len();
    }
    context.push_str(&instructions);
    context
}

/// The independently supervised worker also needs the protocol that holds its failed reds.
fn repair_task(tip: &str, earlier: &Before, rows: &[Row], root: &Path) -> String {
    let mut task = repair_context(tip, earlier, rows, root);
    task.push_str(
        "Where no cause is found, end with `Could not finish: <why>`: this follower starts no \
         other worker on the same reds.\n",
    );
    task
}

const STOP_GRACE: Duration = Duration::from_secs(20);

fn sample_command(program: &Path) -> aterm_uds::spawnfd::Command {
    use aterm_uds::spawnfd::Tether as _;
    let mut cmd = aterm_uds::spawnfd::command(program);
    // launchd's session is zero. Tests exercise native terminal identities, so
    // the pass leads both its own session and its cancellable process group.
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
                "--pass".into(),
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
                    "pass exec is in {}, expected {}",
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

/// Judge `tip` in a pass of its own: its result, or `None` when this controller was stopped.
fn judge(root: &Path, tip: &str) -> Result<Option<Sample>, String> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("watch workspace is dirty; preserving its work".into());
    }
    git(root, &["checkout", "--quiet", "--detach", tip])?;
    // vendor/astream is private. Its URL may be HTTPS, but only caller authority—not URL shape—
    // can say a read is anonymous.
    git(root, &["submodule", "update", "--init", "vendor/astream"])?;
    git(root, &["config", "submodule.recurse", "true"])?;
    let result = root.join(".git/watch-result.toml");
    let _ = fs::remove_file(&result);
    let log = root.join(".git/watch-sample.log");
    // This controller may be long-lived. It launches only the tiny bootstrap in its own image;
    // that bootstrap builds and execs the tip's verifier before any row is run.
    let mut cmd = sample_command(&std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.current_dir(root)
        .args(["watch", "--sample"])
        .arg(&result);
    let file = File::create(&log).map_err(|e| e.to_string())?;
    cmd.stderr(file.try_clone().map_err(|e| e.to_string())?)
        .stdout(file);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stop = aterm_toolchain::interrupt::Watch::enter();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if stop.caught().is_some() {
            stop_sample(&mut child)?;
            return Ok(None);
        }
        stop.nap(Duration::from_millis(100), &|| false);
    };
    let text = fs::read_to_string(&log).unwrap_or_default();
    if !status.success() {
        return Ok(Some(stopped_sample(status, &text)));
    }
    let result = fs::read_to_string(result).map_err(|error| error.to_string())?;
    aterm_toml::from_str(&result).map_err(|error| error.to_string())
}

/// The result of a pass that ended with no result of its own: what stopped it, and its log.
fn stopped_sample(status: std::process::ExitStatus, log: &str) -> Sample {
    use std::os::unix::process::ExitStatusExt as _;
    let mut row = row("sample", status.code(), log);
    row.outcome = "could-not-run".into();
    row.red_items.clear();
    let why = match (status.signal(), status.code()) {
        (Some(signal), _) => format!("pass signal={signal}; sender unavailable from wait status"),
        (None, Some(75)) => "pass exit=75: refusal or cooperative stop".to_owned(),
        (None, code) => format!("pass exited {code:?}"),
    };
    row.log = format!("{why}\n{}", row.log);
    Sample {
        state: "could-not-run".into(),
        rows: vec![row],
    }
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

/// One row of a pass: what ran, how it ended, and its reds.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Row {
    id: String,
    /// `pass`, `red` or `could-not-run`.
    outcome: String,
    log: String,
    /// Each red by package, target and test, or by check and finding.
    red_items: Vec<String>,
}

/// What a pass hands its controller.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Sample {
    /// `pass`, `red` or `could-not-run`.
    state: String,
    rows: Vec<Row>,
}

/// The rows of a pass, in order: the release's tree rows, then cells, and appkit where the Mac
/// has a desk session.
const ROWS: [(&str, &[&str]); 8] = [
    ("inputs", &["ship", "inputs"]),
    ("stock-x86_64", &["ship", "check", "--compat"]),
    ("tree", &["verify", "release", "--tree"]),
    ("cells", &["verify", "cells"]),
    ("appkit", &["verify", "appkit"]),
    ("export", &["ship", "check", "--export"]),
    ("cut", &["ship", "check", "--cut", "--source"]),
    ("inputs-end", &["ship", "inputs", "--recheck"]),
];

/// The rows a pass runs on a Mac with (`desk`) or without a desk session.
fn planned(desk: bool) -> Vec<(&'static str, &'static [&'static str])> {
    ROWS.into_iter()
        .filter(|(id, _)| desk || *id != "appkit")
        .collect()
}

/// Whether this Mac has a desk session of this user that AppKit checks can run in: its console
/// is this user's and unlocked, and no SSH session carries the run.
fn desk() -> bool {
    use std::os::unix::fs::MetadataExt as _;
    cfg!(target_os = "macos")
        && std::env::var_os("SSH_CONNECTION").is_none_or(|ssh| ssh.is_empty())
        && fs::metadata("/dev/console")
            .is_ok_and(|console| console.uid() == aterm_uds::peer::our_uid())
        && !crate::smoke::screen_locked()
}

/// A `verify` row that exited 0 with a check that skipped ran less than it was planned to (an
/// appkit stage on a console locked since the pass planned it, a cell with no targo): it could
/// not run, never a pass. `verify` itself refuses a skip only under `release`.
fn skip_is_no_pass(row: &mut Row, output: &str) {
    if row.outcome != "pass" {
        return;
    }
    let skipped: Vec<String> = output
        .lines()
        .filter_map(crate::event_log::CheckRow::of_event)
        .filter(|check| check.word == "skip")
        .map(|check| check.check)
        .collect();
    if !skipped.is_empty() {
        row.outcome = "could-not-run".into();
        row.log.push_str(&format!(
            "\n{} skipped; a skip is no pass",
            skipped.join(", ")
        ));
    }
}

/// A pass's verdict: red where a row is red, a pass only where every planned row passed.
fn verdict(rows: &[Row], planned: &[&str]) -> &'static str {
    if rows.iter().any(|r| r.outcome == "red") {
        "red"
    } else if planned
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

fn exec_env<'a>(
    root: &'a Path,
    env: &'a crate::EnvSnapshot,
    scratch: &'a Path,
    removed: &'a [&'a str],
) -> crate::exec::ExecEnv<'a> {
    crate::exec::ExecEnv {
        events: None,
        cwd: root,
        path: &env.path,
        scratch,
        child_ceiling: None,
        remove_env: removed,
        add_env: &[],
        stage_groups: None,
        host: &crate::exec::LiveHost,
        hang_samples: None,
    }
}

/// The pass bootstrap: build the tip's verifier, then replace this image with it.
fn sample(root: &Path, result: &Path) -> Result<bool, String> {
    crate::exec::group::kill_on_interrupt();
    let test_handoff = sample_test_handoff();
    let is_test_handoff = test_handoff.is_some();
    // Build the tip's verifier while this process leads the controller-owned group, then
    // replace this image in place so every row, attribution and verdict rule comes from the tip
    // being judged.
    let (stage2, path) = if let Some(handoff) = test_handoff {
        handoff
    } else {
        let env = crate::EnvSnapshot::capture();
        let tools = crate::preflight(&env, root)?;
        let prefix = aterm_toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
        let lease = aterm_toolchain::lease::hold_for(
            &prefix,
            &tools.stage2_dir,
            "verify watch pass bootstrap",
            aterm_toolchain::lease::DEFAULT_WAIT,
        )
        .map_err(|e| format!("{e:?}"))?;
        if let aterm_toolchain::lease::Hold::NotTaken(_, why) = &lease {
            return Err(format!("compiler lease: {why}"));
        }
        let scratch = root.join(".git/watch-exec");
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let removed = sample_removed_env();
        let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
        let direct_path = tools.path_with_stage2_first(&env.path);
        let cmd = crate::exec::Cmd::new(&tools.targo)
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
            .env("TRUST_STAGE2_BIN", &tools.stage2_dir)
            .env("PATH", &direct_path)
            .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"));
        let ran = crate::exec::run(&cmd, exec_env(root, &env, &scratch, &removed));
        fs::write(root.join(".git/watch-candidate.log"), &ran.output).map_err(|e| e.to_string())?;
        let run_stopping = crate::exec::group::stopping();
        if ran.code == Some(75) || run_stopping {
            eprintln!("{}", stopped_run("bootstrap", &ran, run_stopping));
            return Ok(false);
        }
        if !ran.ok {
            write_bootstrap_result(result, candidate_failure_row(&ran))?;
            return Ok(true);
        }
        // Returning only inert argv/env data makes this scope the cleanup proof: the compiler
        // lease is dropped before an exec can skip destructors.
        (tools.stage2_dir.clone(), direct_path)
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
    #[cfg(not(test))]
    let _ = is_test_handoff;
    if !crate::exec::group::default_interrupts_for_exec() {
        eprintln!("watch bootstrap handoff: pending interrupt prevented exec");
        return Ok(false);
    }
    let error = next.exec();
    crate::exec::group::kill_on_interrupt();
    Err(format!(
        "could not replace the bootstrap with the tip's verifier: {error}"
    ))
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
            format!("pass run marked stopping; {}", ran.unnamed_cause()),
        ),
        None => ("child-exit", ran.unnamed_cause()),
    };
    format!(
        "watch {scope} stopped: code={:?} kind={kind} condition={condition:?}\n{}",
        ran.code, ran.output
    )
}

fn write_bootstrap_result(result: &Path, mut row: Row) -> Result<(), String> {
    let state = if row.outcome == "red" {
        "red"
    } else {
        row.outcome = "could-not-run".into();
        row.red_items.clear();
        "could-not-run"
    };
    write_result(
        result,
        &Sample {
            state: state.into(),
            rows: vec![row],
        },
    )
}

fn write_result(result: &Path, sample: &Sample) -> Result<(), String> {
    let text = aterm_toml::to_string(sample).map_err(|e| e.to_string())?;
    fs::write(result, text).map_err(|e| e.to_string())
}

/// A row the follower itself could not run, and why.
fn could_not_run(id: &str, why: String) -> Row {
    Row {
        id: id.into(),
        outcome: "could-not-run".into(),
        log: why,
        red_items: vec![],
    }
}

/// The pass itself, in the tip's own verifier: every planned row to its end.
fn pass(root: &Path, result: &Path) -> Result<bool, String> {
    let current = fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let expected = fs::canonicalize(candidate_path(root)).map_err(|e| e.to_string())?;
    if current != expected {
        return Err(format!(
            "the pass runs {}, expected {}",
            current.display(),
            expected.display()
        ));
    }
    crate::exec::group::kill_on_interrupt();
    let env = crate::EnvSnapshot::capture();
    let tools = crate::preflight(&env, root)?;
    let prefix = aterm_toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let lease = aterm_toolchain::lease::hold_for(
        &prefix,
        &tools.stage2_dir,
        "verify watch pass",
        aterm_toolchain::lease::DEFAULT_WAIT,
    )
    .map_err(|e| format!("{e:?}"))?;
    if let aterm_toolchain::lease::Hold::NotTaken(_, why) = &lease {
        return Err(format!("compiler lease: {why}"));
    }
    let scratch = root.join(".git/watch-exec");
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let removed = sample_removed_env();
    let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
    let direct_path = tools.path_with_stage2_first(&env.path);
    let rows_planned = planned(desk());
    let mut rows = Vec::new();
    for (id, args) in &rows_planned {
        let began = Instant::now();
        let cmd = if args[0] == "verify" {
            // The tip's verifier is this process's own image, built once by the bootstrap.
            crate::exec::Cmd::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(args[1..].iter().copied())
        } else {
            crate::exec::Cmd::new(&tools.targo)
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
        }
        .env("TRUST_STAGE2_BIN", &tools.stage2_dir)
        .env("PATH", &direct_path)
        .env("CARGO_TARGET_DIR", root.join(".ubd/target.noindex"));
        let mut ran = crate::exec::run(&cmd, exec_env(root, &env, &scratch, &removed));
        if *id == "tree"
            && let Ok(build_output) = fs::read_to_string(root.join(".git/watch-candidate.log"))
        {
            ran.output.insert_str(0, &build_output);
        }
        fs::write(root.join(format!(".git/watch-{id}.log")), &ran.output)
            .map_err(|e| e.to_string())?;
        let run_stopping = crate::exec::group::stopping();
        if ran.code == Some(75) || run_stopping {
            eprintln!("{}", stopped_run(&format!("row={id}"), &ran, run_stopping));
            return Ok(false);
        }
        let mut row = row(id, ran.code, &ran.output);
        if args[0] == "verify" {
            skip_is_no_pass(&mut row, &ran.output);
        }
        if let Some(why) = ran.environment_failure() {
            row.outcome = "could-not-run".into();
            row.red_items.clear();
            row.log.push_str(&format!("\n{why}"));
        }
        if *id == "inputs-end"
            && row.outcome == "pass"
            && !rows
                .iter()
                .find(|r: &&Row| r.id == "inputs")
                .is_some_and(|r| seals_match(&r.log, &row.log))
        {
            row.outcome = "could-not-run".into();
            row.log
                .push_str("\ninputs changed during the pass; no green verdict");
        }
        row.log = format!("elapsed_s={}\n{}", began.elapsed().as_secs(), row.log);
        // A red row stops no later one: the pass names every red it can see.
        rows.push(row);
    }
    let ids: Vec<&str> = rows_planned.iter().map(|(id, _)| *id).collect();
    let state = verdict(&rows, &ids).to_owned();
    write_result(result, &Sample { state, rows })?;
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

/// A red as the follower names it: a test by package, target and test, with any shard filter
/// left out (`-p aterm-gui --lib -- settings:: -- settings::x` is `aterm-gui --lib
/// settings::x`), a test binary with no case by its binary and why, and a check's finding by
/// its check.
fn red_name(red: &crate::event_log::Red) -> String {
    let binary = |spec: &str| -> String {
        spec.strip_prefix("-p ")
            .unwrap_or(spec)
            .split(" -- ")
            .next()
            .unwrap_or(spec)
            .to_owned()
    };
    match &red.id {
        Some(id) if id.starts_with("-p ") => match id.rsplit_once(" -- ") {
            Some((_, case)) => format!("{} {case}", binary(id)),
            None => binary(id),
        },
        Some(_) => red.what.clone(),
        None => match red.what.strip_prefix("-p ") {
            Some(rest) => {
                let (spec, why) = rest.split_once(": ").unwrap_or((rest, ""));
                let spec = binary(spec);
                if why.is_empty() {
                    spec
                } else {
                    format!("{spec}: {why}")
                }
            }
            None => red.what.clone(),
        },
    }
}

fn row(id: &str, code: Option<i32>, text: &str) -> Row {
    let outcome = match code {
        Some(0) => "pass",
        Some(1) if !id.starts_with("inputs") => "red",
        Some(101) if text.contains("could not compile") => "red",
        _ => "could-not-run",
    };
    let mut red_items: Vec<String> = Vec::new();
    if outcome == "red" {
        for red in text.lines().flat_map(crate::event_log::Red::of_event) {
            let name = red_name(&red);
            if !red_items.contains(&name) {
                red_items.push(name);
            }
        }
        if red_items.is_empty() {
            red_items.extend(
                text.lines()
                    .filter_map(|l| l.trim().strip_prefix("FAIL").map(|s| s.trim().to_owned()))
                    .filter(|s| !s.is_empty()),
            );
            red_items.dedup();
        }
        if red_items.is_empty() {
            red_items.push(id.into());
        }
    }
    // Keep the final tail and the signal provenance before it: later NOT RUN reporting must not
    // erase the signal sender.
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
        .filter(|line| line.starts_with("watch "))
        .take(16)
        .chain(provenance)
    {
        if !tail.lines().any(|kept| kept == line) && !evidence.contains(&line) {
            evidence.push(line);
        }
    }
    evidence.push(&tail);
    Row {
        id: id.into(),
        outcome: outcome.into(),
        log: evidence.join("\n"),
        red_items,
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
