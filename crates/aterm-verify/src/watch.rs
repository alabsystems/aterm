// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The follower: judges origin/main's newest tip after the landings that made it, one pass at a
//! time, and files its claim and its verdict in the judged journal (`refs/aterm/judged`,
//! [`crate::judged`]): a `judging` record with a lease at the tip's tree's slot, renewed while
//! the pass runs, then a pass or a red with its rows. A follower on another Mac is declined
//! while that lease is live and takes the tree once it lapsed. No commit status is read or
//! posted. A pass runs every row to its end (a red stops no later row). A one-shot pass returns
//! repair context to its caller. The persistent follower starts an independent repair worker
//! unless one still works or could not finish the same reds. Which tips the follower judged,
//! and in what order, is read from the journal and from the tip's ancestry over every parent,
//! never from its first-parent chain: a landing merges main into its branch, so the tip before
//! it is the merge's second parent.

use crate::judged::{self, Record, Unfiled};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "watch_service.rs"]
mod service;

const HELP: &str = "verify watch [--workspace DIR] [--remote NAME] [--branch BRANCH] [--once] [--interval SECONDS] [--repair off|worker] [--repair-cwd DIR]\n\
Follows origin/main in a dedicated clone: each pass judges the newest tip no follower has\n\
judged, with the verifier built from that tip, and files its claim and its verdict in the\n\
judged journal (refs/aterm/judged, verdicts/<tree>.toml): judging, with a 20-minute lease it\n\
renews every 10, then pass or red with the rows. A follower on another Mac is declined while\n\
that lease is live and takes the tip once it lapsed; a tip this Mac's attempt ended on with\n\
no verdict is not claimed again by it for 20 minutes. No commit status is read or posted.\n\
A pass runs the release's rows (ship inputs, the\n\
x86_64 compat compile, verify release --tree, verify cells, verify appkit on a Mac with a\n\
desk session, ship's export and cut checks, the inputs recheck) to their ends: a red stops\n\
no later row. Reds are named by package, target and test.\n\
A release goes first: no pass starts while the capacity ledger holds a Release claim or cannot\n\
be read, and a pass holds one background claim for all its rows, which a release that starts\n\
meanwhile ends at once; the attempt is filed cancelled and the tip is judged again later.\n\
Repairs are off unless asked: a persistent follower's red is filed in the journal with its\n\
rows, and it starts, asks after and ends no agent session or tab. With --repair worker a red\n\
starts one Claude Code repair worker in DIR (default: this repository's main\n\
checkout) unless the last one still works or could not finish the same reds, or has been\n\
stalled at its start for less than 15 minutes; it bisects\n\
between the last tip the journal says the follower judged green and the red one, over every\n\
parent (a landing's merge has the tip before it as its second parent), and fixes forward\n\
independently while the follower continues.\n\
--once judges one tip and returns repair context to its caller: use native subagents or work\n\
directly, with your own shell tools for commands. It starts and retires no agent sessions.\n\
No hook, gate or publication.\n\
verify watch --install [--workspace DIR] [--interval SECONDS] [--repair off|worker] [--repair-cwd DIR]\n\
installs it as this user's LaunchAgent, replacing an earlier install. The LaunchAgent's\n\
arguments carry the repair switch, off unless --repair worker was given.\n\
verify watch --uninstall removes that LaunchAgent: its property list first, so nothing starts\n\
it again, then the loaded follower, which stops its pass and files it cancelled. Its checkout,\n\
its logs and the verdicts it filed stay.\n";

/// Leave room for the persistent worker's instructions within the control task's 64 KiB limit.
const MAX_REPAIR_CONTEXT_BYTES: usize = 60 * 1024;
/// The remote of the follower's own clone ([`prepare_workspace`] points it at the followed
/// repository): main and the judged journal are read there, and the journal written.
const ORIGIN: &str = "origin";

#[derive(Clone, Debug)]
struct Options {
    workspace: PathBuf,
    remote: String,
    branch: String,
    once: bool,
    interval: Duration,
    /// Whether a persistent follower's red starts a repair worker: `--repair worker`. Off unless
    /// asked, and the installed LaunchAgent's arguments say which (`--repair off|worker`).
    repair: bool,
    /// Where a repair worker starts: `--repair-cwd`, else [`main_checkout`].
    repair_cwd: Option<PathBuf>,
}

impl Options {
    fn repair_mode<'a>(&self, worker: &'a mut Option<Repair>) -> RepairMode<'a> {
        if self.once {
            RepairMode::Caller
        } else if self.repair {
            RepairMode::Persistent(worker)
        } else {
            RepairMode::Off
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
        repair: false,
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
            "--repair" => {
                opts.repair = match it.next().map(String::as_str) {
                    Some("worker") => true,
                    Some("off") => false,
                    _ => return Err("--repair needs off or worker".into()),
                }
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

/// The output of a command that must end within a minute: a local read, or `aterm ctl`. A
/// step that moves objects waits by progress instead ([`git_working`]).
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

/// How long a git step that moves objects may go without progress before it is a hang.
const GIT_STALL: Duration = Duration::from_secs(60);

/// A git step that takes as long as the machine and the network make it: the seed clone (38.8
/// to 67.8 s measured for this repository, so a 60 s wall bound failed it at random), a
/// fetch, a checkout, a submodule's first clone. It is ended only once it printed nothing and
/// its process tree used no CPU and did no I/O for [`GIT_STALL`], which is a hang and reads as
/// one.
fn git_working(root: &Path, args: &[&str]) -> Result<String, String> {
    git_working_within(root, args, GIT_STALL)
}

/// [`git_working`], a hang being `stall` without progress.
fn git_working_within(root: &Path, args: &[&str], stall: Duration) -> Result<String, String> {
    let step = format!("git {}", args.join(" "));
    let mut cmd = git_command(root);
    cmd.args(args);
    let out = aterm_uds::deadline::output_while_process_progressing(&mut cmd, stall, step.clone())
        .map_err(|why| format!("{step}: {why}"))?;
    if !out.status.success() {
        return Err(format!(
            "{step}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|why| format!("{step}: {why}"))
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
    if args.first().is_some_and(|s| s == "--uninstall") {
        if args.len() != 1 {
            eprintln!("verify watch --uninstall takes no other word\n{HELP}");
            return 2;
        }
        return match service::uninstall() {
            Ok(said) => {
                println!("{said}");
                0
            }
            Err(why) => {
                eprintln!("verify watch uninstall: {why}");
                3
            }
        };
    }
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return 0;
    }
    if args.first().is_some_and(|s| s == "--sample-ready") {
        // Only a controller installed before the follower hands the tip's verifier this
        // option, once a minute. This verifier runs no row for it and writes it no result.
        match service::retire_pre_follower() {
            Ok(said) => eprintln!("{said}"),
            Err(why) => {
                eprintln!("verify watch: the controller that ran this was not retired: {why}")
            }
        }
        return 75;
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
    let mut world = Live { repair_cwd };
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
/// that the journal refuses at every turn, says why once, not at every turn; a pass in between makes
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

/// Where a workspace is seeded before it is moved into place: beside it, by a name only this
/// tool uses.
fn seeding(workspace: &Path) -> Result<PathBuf, String> {
    let name = workspace.file_name().ok_or("workspace has no name")?;
    let mut seed = name.to_owned();
    seed.push(".seeding");
    Ok(workspace.with_file_name(seed))
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
        // The clone is made beside the workspace and moved in whole, with its owner's mark, so
        // a seed that was stopped half way leaves no workspace this follower would then refuse
        // as one it did not make. What an earlier seed left under that name is this tool's
        // own, and is cleared.
        let seed = seeding(&opts.workspace)?;
        match fs::remove_dir_all(&seed) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", seed.display())),
        }
        // Seed independent objects locally; every pass still fetches the actual remote.
        git_working(
            cwd,
            &[
                "clone",
                "--no-hardlinks",
                cwd.to_str().ok_or("source checkout is not UTF-8")?,
                seed.to_str().ok_or("workspace is not UTF-8")?,
            ],
        )?;
        git(&seed, &["remote", "set-url", "origin", &source])?;
        fs::write(seed.join(".git/verify-watch-owner"), source.as_bytes())
            .map_err(|e| e.to_string())?;
        fs::rename(&seed, &opts.workspace)
            .map_err(|e| format!("{}: {e}", opts.workspace.display()))?;
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

/// Immutable worker custody, as a verdict's record carries it ([`Record::repair`] and the two
/// fields after it). A record with a session and no well-formed assignment keeps the session
/// with an unknown assignment: nothing but the recorded hash is evidence that the worker tried
/// a red.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Repair {
    sid: String,
    assignment: Option<String>,
    /// When the worker was started, Unix seconds; 0 where no record says.
    started: u64,
}

impl Repair {
    /// The custody `record` carries: a tab ID, or the `s-` ID of an older worker.
    fn of(record: &Record) -> Option<Self> {
        let sid = record.repair.as_str();
        let named = aterm_types::tab_id::is_tab_id(sid) || aterm_types::tab_id::is_legacy(sid);
        named.then(|| Self {
            sid: sid.to_owned(),
            assignment: Some(record.repair_assignment.clone()).filter(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }),
            started: record.repair_started,
        })
    }

    /// Write this custody into `record`.
    fn put(&self, record: &mut Record) {
        record.repair.clone_from(&self.sid);
        record.repair_assignment = self.assignment.clone().unwrap_or_default();
        record.repair_started = self.started;
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

/// How long a worker stalled at its start holds the repair place, in seconds from when it was
/// started ([`Repair::started`]). A session that starts does so in seconds; one still at its
/// start after this is not going to, and held the one place for as long as it sat there.
const STARTUP_STALL: u64 = 15 * 60;

/// Where a repair worker's task stands, as `aterm ctl @<sid> task` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Worker {
    /// It works, or it is stalled at a hold it leaves by itself; or its record could not be
    /// read. It holds the one repair place.
    Working,
    /// It is stalled at its start: it holds the place for [`STARTUP_STALL`] from its start.
    Starting,
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
        (Some("working"), _) | (Some("stalled"), Some("held")) => Worker::Working,
        (Some("stalled"), Some("startup")) => Worker::Starting,
        (Some("failed" | "stalled"), _) => Worker::Failed,
        (Some("done" | "exited"), _) => Worker::Ended,
        (None, _) if record.split_whitespace().eq(["OK", "task", "-"]) => Worker::Ended,
        _ => Worker::Working,
    }
}

/// What a pass does beyond its checkout and the journal: the repair worker. The controller's is
/// [`Live`]; a test scripts one.
trait World {
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

/// A one-shot caller owns its repair; only a persistent follower asked to (`--repair worker`)
/// manages an agent session.
enum RepairMode<'a> {
    Caller,
    /// A persistent follower with repairs off: a red is filed, and no session is started,
    /// asked after or ended.
    Off,
    Persistent(&'a mut Option<Repair>),
}

fn write_caller_repair(output: &mut dyn std::io::Write, context: &str) -> std::io::Result<()> {
    writeln!(output, "{context}")?;
    output.flush()
}

/// The live world: the workers through `aterm ctl`.
struct Live {
    repair_cwd: PathBuf,
}

impl Live {
    fn ctl(args: &[&str], input: &[u8]) -> Result<String, String> {
        let mut cmd = aterm_uds::spawnfd::command("aterm");
        cmd.arg("ctl").args(args);
        output(&mut cmd, input).map_err(|why| format!("aterm ctl {}: {why}", args[0]))
    }
}

impl World for Live {
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

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// What a claim says of the controller that made it: that it is the follower's, its image and
/// process, then the stable selectors of where it runs. A missing or malformed identity stays
/// absent; reading one never mints a node.
fn controller_identity(image: &str, pid: u32, node: Option<&str>, session: Option<&str>) -> String {
    let mut identity = format!("follower sha256={image} pid={pid}");
    if let Some(node) = node.filter(|node| aterm_net::fleet::is_node_id(node)) {
        identity.push_str(&format!(" node={node}"));
    }
    if let Some(session) = session.filter(|session| {
        aterm_types::tab_id::is_tab_id(session) || aterm_types::tab_id::is_legacy(session)
    }) {
        identity.push_str(&format!(" session={session}"));
    }
    identity
}

/// Why `held` keeps this Mac's follower out of `tip`. Times are Unix seconds as the record
/// holds them, so one reason reads the same at every turn of the loop ([`log_line`]).
fn declined(tip: &str, held: &Record) -> String {
    if held.verdict() || held.state == "judging" {
        format!("{tip}: {}", held.summary())
    } else {
        format!(
            "{tip}: this Mac's attempt ended {} at {}; it is not claimed again before {}",
            held.state,
            held.updated,
            held.updated.saturating_add(judged::RETRY_AFTER)
        )
    }
}

/// One turn: fetch main, and judge its newest tip unless the journal says a follower has, or
/// does now.
fn attempt(
    opts: &Options,
    world: &mut dyn World,
    worker: &mut Option<Repair>,
) -> Result<Start, String> {
    attempt_with(opts, world, worker, &now, &live_release, &mut judge)
}

/// Why no pass starts now: the capacity ledger (`ledger`: its claims, `None` where no home
/// directory holds one) has a Release claim, waiting or admitted, or could not be read, which
/// supplies no authority to start repeatable work beside a release. A release cut's
/// qualification claims at that rank, and goes first. The Trust toolchain's train does not:
/// Trust's `build-slot` admits it through the build-CPU lease pool, and it holds no claim of
/// this ledger at any rank. So no start is declined for the train and no pass is ended by it;
/// the rows' builds and the train's share cores and memory as any two builds do, each admitted
/// by its own claims. The follower yields to the train once the train claims at Release rank,
/// which is the train's to do.
fn release_holds(ledger: Option<std::io::Result<Vec<aterm_capacity::Record>>>) -> Option<String> {
    match ledger {
        None => Some("no home directory holds the capacity ledger".into()),
        Some(Err(why)) => Some(format!("the capacity ledger could not be read: {why}")),
        Some(Ok(claims)) => claims
            .into_iter()
            .find(|claim| claim.need.rank == aterm_capacity::Rank::Release)
            .map(|claim| {
                format!(
                    "Release claim pid={} what={:?} checkout={:?}",
                    claim.pid, claim.need.what, claim.checkout
                )
            }),
    }
}

/// [`release_holds`] of this user's ledger as it is now.
fn live_release() -> Option<String> {
    release_holds(aterm_capacity::Ledger::user().map(|ledger| ledger.list()))
}

/// What judges a claimed tip: [`judge`], or a test's script of a pass.
type Judge<'a> = &'a mut dyn FnMut(&Path, &mut Record) -> Result<(Judged, Option<String>), String>;

/// [`attempt`] by the clock `now` (Unix seconds), what `release` says holds this Mac
/// ([`release_holds`]) and the pass `judge`.
fn attempt_with(
    opts: &Options,
    world: &mut dyn World,
    worker: &mut Option<Repair>,
    now: &dyn Fn() -> u64,
    release: &dyn Fn() -> Option<String>,
    judge: Judge<'_>,
) -> Result<Start, String> {
    let root = &opts.workspace;
    // What the last attempt left reaches the journal before anything new is claimed.
    settle(root, now())?;
    // A release goes first: under one nothing is fetched, claimed or built, so the bootstrap's
    // build is covered too, which runs before the pass can hold its own claim.
    if let Some(why) = release() {
        return Ok(Start::Declined(format!("a release goes first: {why}")));
    }
    // verify watch follows the invoking checkout's arbitrary remote, which may be private. URL
    // shape alone is no authority to classify this read as credential-less.
    git_working(root, &["fetch", "--quiet", ORIGIN, &opts.branch])?;
    let tip = git(root, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    let tree = git(root, &["rev-parse", &format!("{tip}^{{tree}}")])?;
    let node = aterm_types::dirs::state_dir()
        .and_then(|dir| aterm_net::fleet_member::read_node_id(&dir).ok().flatten());
    let session = std::env::var("ATERM_SESSION_ID").ok();
    let started = now();
    let mut record = Record {
        commit: tip.clone(),
        tree,
        host: host(),
        // The pass builds the tip's own verifier from this commit; no other one judges it.
        verifier: String::new(),
        controller: controller_identity(
            crate::report::running_driver().unwrap_or("unavailable"),
            std::process::id(),
            node.as_deref(),
            session.as_deref(),
        ),
        inputs: String::new(),
        token: aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?,
        state: "judging".into(),
        cause: String::new(),
        started,
        updated: started,
        lease_until: started + judged::LEASE,
        judge: judged::FOLLOWER.into(),
        repair: String::new(),
        repair_assignment: String::new(),
        repair_started: 0,
        rows: Vec::new(),
    };
    // The claim precedes all expensive work, and a declined one writes nothing.
    if let Some(held) = judged::claim(root, ORIGIN, &record, started, judged::deadline())? {
        return Ok(Start::Declined(declined(&tip, &held)));
    }
    // Kept from the claim until the journal holds this attempt's last record: a controller that
    // ends while judging leaves its claim here, for the next loop to close. A claim whose append
    // ended with its outcome unknown returned above: it is not kept, and lapses with its lease.
    save(root, &record)?;
    let (ended, unrenewed) = judge(root, &mut record).unwrap_or_else(|why| {
        let sample = Sample {
            state: "could-not-run".into(),
            rows: vec![could_not_run("follower", why)],
        };
        (Judged::Sample(sample), None)
    });
    let written = match ended {
        Judged::Stopped(signal) => {
            record.state = "cancelled".into();
            record.cause = format!("the follower was stopped by signal {signal}");
            Ok(())
        }
        Judged::Sample(sample) => {
            let earlier = if matches!(sample.state.as_str(), "pass" | "red") {
                before(root, &tip)
            } else {
                Before::default()
            };
            let mode = opts.repair_mode(worker);
            conclude(world, &mut record, &earlier, &sample, mode, root, now())
        }
    };
    if let Some(why) = unrenewed {
        if !record.cause.is_empty() {
            record.cause.push('\n');
        }
        record.cause.push_str(&why);
    }
    match (finish(root, &mut record, now()), written) {
        (Err(filed), Err(written)) => Err(format!("{filed}; {written}")),
        (Err(why), Ok(())) | (Ok(()), Err(why)) => Err(why),
        (Ok(()), Ok(())) => Ok(Start::Judged),
    }
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

/// End an attempt at `epoch`: its last record is kept in the checkout first, so a filing that
/// fails, or a controller that ends here, leaves it for the next loop ([`settle`]).
fn finish(root: &Path, record: &mut Record, epoch: u64) -> Result<(), String> {
    record.updated = epoch;
    record.lease_until = 0;
    save(root, record)?;
    settle(root, epoch)
}

/// File the record the checkout keeps ([`judged::file`]), say it, and forget it once the
/// journal holds it. A kept record still `judging` is a claim whose controller ended before
/// its attempt did, since no judgment of this controller is in flight when its loop turns: it
/// is closed as could-not-run. A kept record that cannot be read is set aside, and stops no
/// later pass.
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
        let why = "the follower ended before this attempt had a last record";
        record.state = "could-not-run".into();
        record.cause = why.into();
        record
            .rows
            .push(journal_row(&could_not_run("follower", why.into())));
        record.updated = epoch;
        record.lease_until = 0;
        save(root, &record)?;
    }
    let place = judged::file(root, ORIGIN, &record, epoch, judged::deadline())?;
    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "verify watch: {} {} (journal {}, {})",
        record.commit,
        record.summary(),
        judged::REF,
        match place {
            Some(judged::Place::Slot) => "at its tree's slot",
            Some(judged::Place::Beside) => "beside another attempt's record",
            None => "which held it already",
        }
    );
    for row in &record.rows {
        for red in &row.red_items {
            println!("verify watch: red {}: {red}", row.id);
        }
    }
    Ok(())
}

/// A pass's row as the journal files it. The follower names no suspect and decides no stage.
fn journal_row(row: &Row) -> judged::Row {
    judged::Row {
        id: row.id.clone(),
        outcome: row.outcome.clone(),
        log: row.log.clone(),
        red_items: row.red_items.clone(),
        suspects: Vec::new(),
        fixed_by: None,
        decisions: Vec::new(),
    }
}

/// What the journal says of the tips before `tip` ([`sequence`], [`walk`]). A journal or a
/// history that cannot be read leaves the custody unknown (`complete` false): no worker is
/// started beside one that may work.
fn before(root: &Path, tip: &str) -> Before {
    let read = || -> Result<(Vec<Record>, bool), String> {
        let until = judged::deadline();
        let head = judged::head(root, ORIGIN, until)?;
        let records = judged::records(root, head.as_deref(), until)?;
        // Every ancestor of the tip, over every parent, no parent before a child of it.
        let listed = git(root, &["rev-list", "--topo-order", tip])?;
        let shallow = match git(root, &["rev-parse", "--is-shallow-repository"])?.as_str() {
            "false" => false,
            "true" => true,
            _ => return Err("cannot decide whether follower history is shallow".into()),
        };
        Ok((sequence(records, tip, &listed), !shallow))
    };
    match read() {
        Ok((before, complete)) => walk(&before, complete),
        Err(why) => {
            eprintln!("verify watch: the tips judged before {tip} could not be read: {why}");
            Before::default()
        }
    }
}

/// The tip sequence: the follower's verdicts in `records` (the journal as it is now) on the
/// commits before `tip` that `listed` names, in its order. `listed` is `git rev-list
/// --topo-order <tip>`, so that is every judged ancestor of the tip, newest first. The tips a
/// follower judged were each main's tip once, and main only moves forward, so ancestry orders
/// them. The first-parent chain does not hold them: a landing merges main into its branch and
/// pushes the merge, whose first parent is the branch and whose second is the tip before it.
/// A record on a commit that is no ancestor of the tip (another branch's `--once`) is none of
/// main's.
fn sequence(records: Vec<Record>, tip: &str, listed: &str) -> Vec<Record> {
    let mut judged = std::collections::BTreeMap::new();
    for record in records {
        if record.judge == judged::FOLLOWER && record.verdict() && record.commit != tip {
            // A tree's slot is listed before the records beside it, and is the one kept.
            judged.entry(record.commit.clone()).or_insert(record);
        }
    }
    listed
        .lines()
        .filter_map(|commit| judged.remove(commit))
        .collect()
}

/// What the tips before a red one say: the newest green, the oldest red since it, and the
/// newest verdict that carries a repair worker. A red that carries none (a one-shot caller's,
/// or one filed with repairs off) does not replace that custody.
#[derive(Debug, Default, PartialEq, Eq)]
struct Before {
    green: Option<String>,
    oldest_red: Option<String>,
    /// The newest red, or the green frontier, that carries a worker.
    repair: Option<Repair>,
    /// The journal and the history were read whole, or back to a green. Where not, a worker no
    /// record read names may still work.
    complete: bool,
}

/// Read `before`, the tip sequence ([`sequence`]: the follower's verdicts on the tips before
/// this one, newest first), back to the newest green. `complete`: the history they were
/// ordered by is whole.
fn walk(before: &[Record], complete: bool) -> Before {
    let mut found = Before {
        complete,
        ..Before::default()
    };
    for record in before {
        if found.repair.is_none() {
            found.repair = Repair::of(record);
        }
        if record.state == "pass" {
            found.green = Some(record.commit.clone());
            // Every verdict back to the green was read, however much history lies before it.
            found.complete = true;
            break;
        }
        found.oldest_red = Some(record.commit.clone());
    }
    found
}

/// Give `record`, this attempt's claim, the verdict of `sample`, its rows and its repair
/// custody; the caller files it. A caller gets repair context without managing any worker. A
/// persistent red with repairs off is filed as it is. With repairs on it names one repair
/// worker: the one `worker` or the newest verdict before it names while it works, while it has
/// been stalled at its start for less than [`STARTUP_STALL`], or while it could not finish
/// these same reds (a fresh one would meet what stopped it); else a new one, the ended one
/// retired. Where the journal could not be read
/// back to a worker or a green, none is started: one may work.
/// # Errors
/// A one-shot caller's repair context could not be written; the record is whole all the same.
fn conclude(
    world: &mut dyn World,
    record: &mut Record,
    earlier: &Before,
    sample: &Sample,
    mode: RepairMode<'_>,
    root: &Path,
    epoch: u64,
) -> Result<(), String> {
    record.rows = sample.rows.iter().map(journal_row).collect();
    match sample.state.as_str() {
        "pass" => {
            record.state = "pass".into();
            let repair = match mode {
                RepairMode::Persistent(worker) => worker.clone(),
                RepairMode::Caller | RepairMode::Off => None,
            }
            .or_else(|| earlier.repair.clone());
            if let Some(repair) = repair {
                repair.put(record);
            }
            Ok(())
        }
        "red" => {
            record.state = "red".into();
            let tip = record.commit.clone();
            let worker = match mode {
                RepairMode::Caller => {
                    let context = repair_context(&tip, earlier, &sample.rows, root);
                    // This pass owns no worker. The next persistent follower finds existing
                    // custody in the worker-bearing verdict, beyond this caller-only red.
                    return world
                        .caller_repair(&format!(
                            "Use your harness's native subagents for same-machine delegation, or work directly; \
                             use your own shell/background tools for commands.\n\n{context}"
                        ))
                        .map_err(|write| format!("caller repair output: {write}"));
                }
                // The red and its rows are the whole of it: the journal keeps them, and a worker
                // an older verdict names keeps its custody there, beyond this red.
                RepairMode::Off => return Ok(()),
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
                Some((repair, Worker::Starting))
                    if epoch.saturating_sub(repair.started) < STARTUP_STALL =>
                {
                    Some(repair)
                }
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
                    let task = repair_task(&tip, earlier, &sample.rows, root);
                    match world.spawn(&task) {
                        Ok(sid) => Some(Repair {
                            sid,
                            assignment: Some(current),
                            started: epoch,
                        }),
                        Err(why) => {
                            eprintln!("verify watch: no repair worker started: {why}");
                            None
                        }
                    }
                }
            };
            if let Some(repair) = worker.as_ref() {
                repair.put(record);
            }
            Ok(())
        }
        state => {
            let stopped = state == "cancelled";
            record.state = if stopped {
                "cancelled"
            } else {
                "could-not-run"
            }
            .into();
            record.cause = sample
                .rows
                .iter()
                .find(|row| row.outcome != "pass")
                .map_or_else(
                    || "could not run: no row".to_owned(),
                    |row| {
                        format!(
                            "could not run {}: {}",
                            row.id,
                            row.log
                                .lines()
                                .rev()
                                .find(|l| !l.trim().is_empty())
                                .unwrap_or("")
                        )
                    },
                );
            Ok(())
        }
    }
}

/// Bounded context shared by the caller and the persistent follower's repair worker. The bisect runs
/// between the tip, which these reds were seen on, and the green, the one tip known good; the
/// oldest red since the green may have been red for other tests, so it is only named. It runs
/// over every parent: the tips between the two are on merges' second parents.
fn repair_context(tip: &str, earlier: &Before, rows: &[Row], root: &Path) -> String {
    let (green_line, range) = match &earlier.green {
        Some(green) => (
            format!("The last tip the follower judged green is {green}."),
            format!("git bisect start {tip} {green}"),
        ),
        None => (
            "The journal names no green tip before it: mark a good one yourself (`git bisect good <rev>`) once the red tests pass there.".to_owned(),
            format!("git bisect start {tip}"),
        ),
    };
    let oldest = earlier.oldest_red.as_ref().map_or_else(String::new, |red| {
        format!(" The oldest red tip since then is {red}; its reds may be other ones.")
    });
    let mut context = format!(
        "The follower (`verify watch`, verdicts in the journal {}) judged origin/main red at {tip}. \
         {green_line}{oldest}\n\nRed, by row, then package, target and test:\n",
        judged::REF
    );
    let instructions = format!(
        "\nFind the landing that turned each red and fix it forward:\n\
         1. Work in a new worktree of your own under ~/.ubd, never in the follower's checkout {}.\n\
         2. Run each red test on origin/main first. Where every one passes there, a landing since {tip} \
         fixed them: name it and end.\n\
         3. Bisect over every parent, never with --first-parent (a landing merges main into its \
         branch, so the tips between the two are second parents), running only the red tests, one at a time: \
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

/// Seconds after a renewal the journal did not take before the next one is tried.
const RENEW_RETRY: u64 = 60;
const _: () = assert!(RENEW_RETRY < judged::RENEW_EVERY);

/// When a claim is renewed: every [`judged::RENEW_EVERY`] seconds, and [`RENEW_RETRY`] seconds
/// after a renewal the journal did not take. A renewal that fails is counted and ends nothing:
/// a judgment of an hour or two is worth more than its lease, and where two hosts each reach a
/// pass or a red for one tree the journal keeps both ([`judged::file`]).
struct Renewal {
    next: Instant,
    failures: u32,
    last_failure: Option<String>,
}

impl Renewal {
    /// A claim made at `now`: its first renewal is due one [`judged::RENEW_EVERY`] later.
    fn new(now: Instant) -> Self {
        Self {
            next: now + Duration::from_secs(judged::RENEW_EVERY),
            failures: 0,
            last_failure: None,
        }
    }

    fn renewed(&mut self, now: Instant) {
        self.next = now + Duration::from_secs(judged::RENEW_EVERY);
    }

    fn failed(&mut self, now: Instant, why: &Unfiled) {
        // Another attempt's hold ends with its verdict or its lease, neither of them soon.
        let again = match why {
            Unfiled::Displaced(_) => judged::RENEW_EVERY,
            Unfiled::Unavailable(_) => RENEW_RETRY,
        };
        self.next = now + Duration::from_secs(again);
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

/// What a judgment's wait reads and does. The controller's is [`LiveSample`]; a test scripts
/// one, and runs a judgment's hours in no time.
trait Attended {
    /// The pass's exit status, once it has ended.
    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String>;
    /// End the pass and what it started.
    fn stop(&mut self) -> Result<(), String>;
    /// The signal this controller caught, if any.
    fn signal(&self) -> Option<i32>;
    /// Now: the monotonic clock, and Unix seconds.
    fn now(&self) -> (Instant, u64);
    /// Wait before the next look.
    fn pause(&mut self);
}

/// How a judgment's wait ended.
#[derive(Debug)]
enum Ended {
    Exited(std::process::ExitStatus),
    /// This controller caught the signal and stopped the pass.
    Interrupted(i32),
}

/// The controller's pass: its process group and the real clocks.
struct LiveSample {
    child: aterm_uds::spawnfd::Child,
    stopped: aterm_toolchain::interrupt::Watch,
}

impl Attended for LiveSample {
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

    fn pause(&mut self) {
        self.stopped.nap(Duration::from_millis(100), &|| false);
    }
}

/// Wait for `sample` to end, renewing `record`'s claim meanwhile through `renew`
/// ([`Renewal`]). Only the pass's own end or this controller's signal ends the wait. A renewal
/// the journal did not take is logged and counted, and what the count holds is returned beside
/// the end.
fn attend(
    record: &mut Record,
    sample: &mut dyn Attended,
    renew: &mut dyn FnMut(&Record, u64) -> Result<(), Unfiled>,
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
        if at >= renewal.next {
            record.updated = epoch;
            record.lease_until = epoch + judged::LEASE;
            match renew(record, epoch) {
                Ok(()) => renewal.renewed(at),
                Err(why) => {
                    eprintln!(
                        "verify watch: the claim was not renewed, and the pass goes on: {why}"
                    );
                    renewal.failed(at, &why);
                }
            }
        }
        sample.pause();
    }
}

/// What a pass left its controller.
enum Judged {
    /// The pass's own result, or what stopped it.
    Sample(Sample),
    /// This controller caught the signal and stopped the pass.
    Stopped(i32),
}

/// Judge `record`'s commit in a pass of its own, renewing its claim meanwhile: how the pass
/// ended, and what the renewals that failed leave for its last record.
fn judge(root: &Path, record: &mut Record) -> Result<(Judged, Option<String>), String> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("watch workspace is dirty; preserving its work".into());
    }
    git_working(root, &["checkout", "--quiet", "--detach", &record.commit])?;
    // vendor/astream is private. Its URL may be HTTPS, but only caller authority—not URL shape—
    // can say a read is anonymous.
    git_working(root, &["submodule", "update", "--init", "vendor/astream"])?;
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
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut live = LiveSample {
        child,
        stopped: aterm_toolchain::interrupt::Watch::enter(),
    };
    let (ended, evidence) = attend(record, &mut live, &mut |record, epoch| {
        judged::interruptible(|| judged::renew(root, ORIGIN, record, epoch, judged::deadline()))
    })?;
    let status = match ended {
        Ended::Exited(status) => status,
        Ended::Interrupted(signal) => return Ok((Judged::Stopped(signal), evidence)),
    };
    let text = fs::read_to_string(&log).unwrap_or_default();
    if !status.success() {
        return Ok((Judged::Sample(stopped_sample(status, &text)), evidence));
    }
    let result = fs::read_to_string(result).map_err(|error| error.to_string())?;
    let sample = aterm_toml::from_str(&result).map_err(|error| error.to_string())?;
    Ok((Judged::Sample(sample), evidence))
}

/// The result of a pass that ended with no result of its own: what stopped it, and its log.
fn stopped_sample(status: std::process::ExitStatus, log: &str) -> Sample {
    use std::os::unix::process::ExitStatusExt as _;
    let mut row = row("sample", status.code(), log);
    row.outcome = "could-not-run".into();
    row.red_items.clear();
    // A pass that was stopped (a release took this Mac, a signal, a refusal) is cancelled; one
    // that broke, or whose environment failed it (a full disk also exits 75, and says so:
    // `stopped_run`), could not run. Neither is a verdict, and the tip is judged again.
    let (state, why) = match (status.signal(), status.code()) {
        (Some(signal), _) => (
            "cancelled",
            format!("pass signal={signal}; sender unavailable from wait status"),
        ),
        (None, Some(75)) if log.contains(" kind=environment ") => (
            "could-not-run",
            "pass exit=75: its environment failed".to_owned(),
        ),
        (None, Some(75)) => (
            "cancelled",
            "pass exit=75: refusal or cooperative stop".to_owned(),
        ),
        (None, code) => ("could-not-run", format!("pass exited {code:?}")),
    };
    row.log = format!("{why}\n{}", row.log);
    Sample {
        state: state.into(),
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
        aterm_uds::backup::Backups::SYSTEM
            .build_dir(&root.join(".ubd/target.noindex"))
            .map_err(|e| e.to_string())?;
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

/// What a pass claims of this Mac, once, for all its rows: background work, which waits its
/// turn behind ordinary work for what they share and which a release ends at once
/// ([`aterm_capacity::Admit::Preemptible`] ends this process's group; the pass then exits 75
/// and its attempt is filed cancelled). Its memory covers the builds its rows start, as the
/// lane's stage claim did (26 GiB); the checks inside claim their own measured peaks. It asks
/// one core, not the machine's: a core grant held for a pass's hour or two is taken from every
/// ordinary claim that comes later, and the rows' builds ask theirs as they run.
fn pass_need() -> aterm_capacity::Need {
    aterm_capacity::Need::new("verify watch pass")
        .rank(aterm_capacity::Rank::Background)
        .mem_gib(26)
        .cores(1)
        .covers()
}

/// The pass's claim from its admission: `None` where the wait was stopped (a release arrived,
/// the ledger could not be read, or an interrupt), which is a stop and no failure of the pass.
fn pass_claim(
    admitted: Result<aterm_capacity::Claim, aterm_capacity::Error>,
) -> Result<Option<aterm_capacity::Claim>, String> {
    match admitted {
        Ok(claim) => Ok(Some(claim)),
        Err(aterm_capacity::Error::Stopped { .. }) => Ok(None),
        Err(why) => Err(format!("capacity: {why}")),
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
    // One claim for the whole pass, taken here because the bootstrap's own would end with its
    // image: this process leads the pass's group, which is what a release ends.
    let admitted = aterm_capacity::admit(pass_need(), aterm_capacity::Admit::Preemptible);
    let Some(_claim) = pass_claim(admitted)? else {
        eprintln!(
            "watch pass stopped before it was admitted: a release holds this Mac, or the pass was interrupted"
        );
        return Ok(false);
    };
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
    aterm_uds::backup::Backups::SYSTEM
        .build_dir(&root.join(".ubd/target.noindex"))
        .map_err(|e| e.to_string())?;
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
