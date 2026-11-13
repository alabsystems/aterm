// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Detached process spawning, child reaping, and jail process-tree inspection.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use aterm_uds::spawnfd::{Child, Command};

/// A child this process started: by the one spawn seam, or forked by a std
/// `Command` that hands it a descriptor above stdio ([`keep_forked`]).
enum Kept {
    Spawned(Child),
    Forked(std::process::Child),
}

impl Kept {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Kept::Spawned(c) => c.try_wait(),
            Kept::Forked(c) => c.try_wait(),
        }
    }
}

/// Children this process started and has not reaped: `alive` reaps them.
fn children() -> &'static Mutex<HashMap<i32, Kept>> {
    static C: OnceLock<Mutex<HashMap<i32, Kept>>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

/// Registers a child forked by `std::process::Command` so [`alive`] reaps it on exit.
pub fn keep_forked(child: std::process::Child) -> i32 {
    let pid = i32::try_from(child.id()).unwrap_or(0);
    children()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(pid, Kept::Forked(child));
    pid
}

/// Spawns `cmd` and registers its handle so [`alive`] reaps it on exit.
///
/// # Errors
/// Returns an error if spawning `cmd` fails.
pub fn spawn_kept(cmd: &mut Command) -> std::io::Result<i32> {
    let child = cmd.spawn()?;
    let pid = i32::try_from(child.id()).unwrap_or(0);
    children()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(pid, Kept::Spawned(child));
    Ok(pid)
}

/// Returns whether `pid` is alive, reaping any exited child started by this process.
#[must_use]
pub fn alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    let mut kids = children().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(child) = kids.get_mut(&pid)
        && !matches!(child.try_wait(), Ok(None))
    {
        kids.remove(&pid);
        return false;
    }
    drop(kids);
    crate::sys::alive(pid)
}

/// Waits up to `limit` for `pid` to exit and be reaped.
#[must_use]
pub fn wait_gone(pid: i32, limit: Duration) -> bool {
    if !alive(pid) {
        return true;
    }
    let Some(w) = u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::exitwatch::ExitWatch::watch)
    else {
        return ended_within(pid, limit);
    };
    if !alive(pid) {
        return true;
    }
    let mut fds = [crate::sys::pollfd(w.as_raw_fd())];
    let ms = i32::try_from(limit.as_millis()).unwrap_or(i32::MAX);
    let _ = crate::sys::poll(&mut fds, ms);
    settled(pid, w.exit_status().is_some(), limit)
}

/// [`wait_gone`]'s answer once its watch fired or ran out (`exited`: the
/// kernel's exit event came): whether `pid` ended.
fn settled(pid: i32, exited: bool, limit: Duration) -> bool {
    if exited {
        let _ = ended_within(pid, limit);
        return true;
    }
    !alive(pid)
}

/// The rest of [`wait_gone`] for a `pid` the kernel would not watch: it has
/// ended, or it is ENDING — the kernel refuses the watch of a process already
/// on its way out that still answers `kill(pid, 0)` and is not a zombie yet,
/// so under load one read of [`alive`] can take a SIGKILLed child of ours for
/// alive (`a_wait_that_runs_out_leaves_the_child_to_a_later_one`).
fn ended_within(pid: i32, limit: Duration) -> bool {
    let deadline = crate::stop::deadline_after(limit);
    loop {
        if !alive(pid) {
            return true;
        }
        let ours = children()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&pid);
        if !ours || std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `pid`'s kernel record ([`aterm_sysprobe::kproc::record`]); `None` for pid 0 or 1, a pid
/// that is gone, or one whose record does not read.
fn record(pid: i32) -> Option<aterm_sysprobe::kproc::Record> {
    u32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .and_then(aterm_sysprobe::kproc::record)
}

/// Every process's `(pid, ppid)`, from one read of the kernel's process table
/// ([`aterm_sysprobe::kproc::records`]) with no program run; empty when it cannot be read.
#[must_use]
pub fn table() -> Vec<(i32, i32)> {
    aterm_sysprobe::kproc::records()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| Some((i32::try_from(r.pid).ok()?, i32::try_from(r.ppid).ok()?)))
        .collect()
}

/// Returns all descendants of `root` in `table`, children before parents.
#[must_use]
pub fn descendants_in(table: &[(i32, i32)], root: i32) -> Vec<i32> {
    if root <= 1 {
        return Vec::new();
    }
    let mut kids: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for &(pid, ppid) in table {
        kids.entry(ppid).or_default().push(pid);
    }
    let mut visited = BTreeSet::from([root]);
    let mut stack = vec![(root, 0usize, false)];
    let mut out = Vec::new();
    while let Some((node, idx, _)) = stack.last_mut() {
        let p = *node;
        let children = kids.get(&p).map_or(&[][..], Vec::as_slice);
        let mut pushed = false;
        while *idx < children.len() {
            let c = children[*idx];
            *idx += 1;
            if visited.insert(c) {
                stack.push((c, 0, true));
                pushed = true;
                break;
            }
        }
        if !pushed && let Some((done, _, true)) = stack.pop() {
            out.push(done);
        }
    }
    out
}

/// Returns all current descendants of `root`.
#[must_use]
pub fn descendants(root: i32) -> Vec<i32> {
    descendants_in(&table(), root)
}

/// `r`'s command line as `ps -ww -o command=` prints it ([`aterm_sysprobe::kproc::ps_args`]),
/// read in this process. Empty while its one thread has not run, and when that cannot be told
/// (another user's process, one gone: [`aterm_uds::process::thread_ran`] is `None`), since an
/// argument read of a program macOS's program check holds can hang
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` M29). A zombie reads as `<defunct>` with
/// nothing read.
fn command_of(r: &aterm_sysprobe::kproc::Record) -> String {
    let ran = i32::try_from(r.pid)
        .ok()
        .and_then(aterm_uds::process::thread_ran);
    if !r.zombie && ran != Some(true) {
        return String::new();
    }
    aterm_sysprobe::kproc::ps_args(r, aterm_sysprobe::kproc::Locale::Utf8)
}

/// `pid`'s command line ([`command_of`]), or an empty string when it is gone or unread.
#[must_use]
pub fn command_line(pid: i32) -> String {
    record(pid).map(|r| command_of(&r)).unwrap_or_default()
}

/// Every process's `(pid, ppid, command line)` ([`command_of`]) from one read of the process
/// table; empty when it cannot be read.
#[must_use]
pub fn commands() -> Vec<(i32, i32, String)> {
    aterm_sysprobe::kproc::records()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| {
            Some((
                i32::try_from(r.pid).ok()?,
                i32::try_from(r.ppid).ok()?,
                command_of(&r),
            ))
        })
        .collect()
}

/// Returns `ps -o stat= -p <pid>`, or `None` if the process is gone.
#[must_use]
pub fn state_of(pid: i32) -> Option<String> {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether `pid` is an unreaped zombie.
#[must_use]
pub fn zombie(pid: i32) -> bool {
    record(pid).is_some_and(|r| r.zombie)
}

/// `pid`'s controlling terminal as [`crate::sys::tty_name`] names it (`ttys003` for a
/// pseudo-terminal, `dev<major>.<minor>` for any other terminal, or `??` with none),
/// read in this process; empty when it is gone.
#[must_use]
pub fn tty_of(pid: i32) -> String {
    record(pid).map_or_else(String::new, |r| crate::sys::tty_name(r.tty))
}

/// When `pid` started, as a token that names that one process: the kernel record's start, in
/// seconds and microseconds. Empty for pid 0 or 1 and when it is gone or unread, so a caller
/// that compares two reads signals nothing on an unread one.
#[must_use]
pub fn start_time(pid: i32) -> String {
    record(pid)
        .map(|r| format!("{}.{:06}", r.start_s, r.start_us))
        .unwrap_or_default()
}

/// Rows of `pid ppid comm` for `pids` (`ps -o pid=,ppid=,comm=`, `comm` as
/// [`aterm_sysprobe::kproc::ps_comm`] reads it, empty while the thread has not run), read in
/// this process; a pid that is gone is left out.
#[must_use]
pub fn rows_of(pids: &[i32]) -> Vec<(i32, i32, String)> {
    pids.iter()
        .filter_map(|&pid| {
            let r = record(pid)?;
            let ran = r.zombie || aterm_uds::process::thread_ran(pid) == Some(true);
            let comm = if ran {
                aterm_sysprobe::kproc::ps_comm(&r, aterm_sysprobe::kproc::Locale::Utf8)
            } else {
                String::new()
            };
            Some((pid, i32::try_from(r.ppid).ok()?, comm))
        })
        .collect()
}

/// A composite path read, retaining named paths even when another component was unreadable.
#[derive(Debug)]
pub struct OpenPaths {
    /// The known working directory, executable, then descriptor paths in descriptor order.
    pub paths: Vec<PathBuf>,
    /// The descriptor table read and both the working directory and executable were named.
    pub complete: bool,
}

/// The paths `pid` uses that `lsof -p <pid> -Fn` names, read in this process with no reference
/// taken on any socket or kqueue of it: its working directory, its executable, then each file
/// it holds open through a descriptor, in descriptor order ([`aterm_uds::holders::open_files`];
/// a PTY master reads as `/dev/ptmx`). An unnamed working directory or executable makes the
/// read incomplete. The rest lsof names (the libraries and other files it maps, its sockets'
/// addresses) are not read.
///
/// # Errors
/// [`aterm_uds::holders::open_files`]'s: `NotFound` once `pid` has exited, `PermissionDenied`
/// for a table this user may not read.
pub fn open_paths(pid: i32) -> std::io::Result<OpenPaths> {
    let pid = u32::try_from(pid).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    open_paths_with(
        pid,
        aterm_uds::holders::open_files,
        aterm_sysprobe::process_cwd,
        aterm_uds::holders::exe_of,
    )
}

fn open_paths_with(
    pid: u32,
    files: impl FnOnce(u32) -> std::io::Result<Vec<PathBuf>>,
    cwd: impl FnOnce(u32) -> Option<PathBuf>,
    exe: impl FnOnce(u32) -> aterm_uds::holders::Exe,
) -> std::io::Result<OpenPaths> {
    let files = files(pid)?;
    let exe = match exe(pid) {
        aterm_uds::holders::Exe::At(path) => Some(path),
        aterm_uds::holders::Exe::Unnamed => None,
        aterm_uds::holders::Exe::Exited => return Err(std::io::ErrorKind::NotFound.into()),
    };
    let cwd = cwd(pid);
    Ok(OpenPaths {
        complete: cwd.is_some() && exe.is_some(),
        paths: cwd.into_iter().chain(exe).chain(files).collect(),
    })
}

/// Returns the path to `<run_dir>/jailed.pids`.
#[must_use]
pub fn jailed_file(run_dir: &Path) -> PathBuf {
    run_dir.join("jailed.pids")
}

/// Appends a jailed root `pid` to [`jailed_file`].
pub fn record_jailed(run_dir: &Path, pid: i32) {
    use std::io::Write as _;
    let line = format!("{pid}\t{}\n", start_time(pid));
    if let Ok(mut f) = crate::util::append_regular_file(&jailed_file(run_dir)) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Keeps a long-lived child started in the lab's jail ([`keep_forked`]) and records it as a
/// jailed root ([`record_jailed`]), so the sweeps that fall back to the process tree when the
/// sandbox gives no answer ([`jail_members`], the sampler's members) still see it and its
/// children: one in a session of its own is no descendant of anything the lab names by pid.
pub fn keep_jailed(run_dir: &Path, child: std::process::Child) -> i32 {
    let pid = keep_forked(child);
    record_jailed(run_dir, pid);
    pid
}

/// Reads jailed root PIDs from [`jailed_file`]. Unless `all`, each requires a canonical start
/// token matching the current kernel record. `all` includes unproven and exited PIDs as
/// historical evidence; that list does not authorize signals.
#[must_use]
pub fn jailed_roots(run_dir: &Path, all: bool) -> Vec<i32> {
    let text = crate::util::read_regular_text(&jailed_file(run_dir)).unwrap_or_default();
    jailed_roots_with(&text, all, start_time)
}

fn jailed_roots_with(text: &str, all: bool, mut start_of: impl FnMut(i32) -> String) -> Vec<i32> {
    text.lines()
        .filter_map(|l| {
            let (pid, start) = l.split_once('\t').unwrap_or((l, ""));
            let pid: i32 = pid.trim().parse().ok()?;
            (pid > 1 && (all || matching_start(start, || start_of(pid)))).then_some(pid)
        })
        .collect()
}

/// A stored start authorizes a root only when it has this build's format and matches a fresh
/// kernel read. Legacy `ps -o lstart=` and malformed tokens remain historical evidence, never
/// proof that a live process with the same PID belongs to this run.
fn matching_start(start: &str, current: impl FnOnce() -> String) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match start.split_once('.') {
        Some((s, us)) if digits(s) && digits(us) && us.len() == 6 => current() == start,
        _ => false,
    }
}

/// Returns all live PIDs currently inside the lab's jail.
#[must_use]
pub fn jail_members(run: &crate::run::Run) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::new();
    if !run.mark.is_empty() {
        match crate::sys::jail_pids(&run.mark) {
            Ok(p) => out.extend(p),
            Err(e) => eprintln!("aterm-lab: {e}: only the process tree is taken"),
        }
    }
    let table = table();
    for r in std::iter::once(run.instance_pid).chain(jailed_roots(&run.run, false)) {
        out.extend(descendants_in(&table, r));
        if alive(r) {
            out.push(r);
        }
    }
    if alive(run.stub_pid) {
        out.push(run.stub_pid);
    }
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|p| *p > 1 && seen.insert(*p));
    out
}

/// Returns all live PIDs of the lab's processes except the stub.
#[must_use]
pub fn lab_tree(run: &crate::run::Run) -> Vec<i32> {
    jail_members(run)
        .into_iter()
        .filter(|&p| p != run.stub_pid)
        .collect()
}

/// Returns all PIDs that have belonged to this lab run.
#[must_use]
pub fn lab_pids_ever(run: &crate::run::Run) -> Vec<i32> {
    let mut v = vec![run.instance_pid, run.stub_pid];
    v.extend(jail_members(run));
    v.extend(jailed_roots(&run.run, true));
    v.extend(
        crate::util::read_regular_text(&run.run.join("members.txt"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse::<i32>().ok()),
    );
    let mut seen = std::collections::BTreeSet::new();
    v.retain(|p| *p > 1 && seen.insert(*p));
    v
}

/// Verifies that the lab's egress witness is still running.
///
/// A sampler macOS's program check still holds at its first exec has words nobody may read
/// yet ([`command_of`]), so this waits for its thread to run, at most
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`], before reading them.
///
/// # Errors
/// Returns an error if the witness process is missing or exited.
pub fn need_witness(run: &crate::run::Run) -> Result<(), String> {
    let line = format!(
        "{} sampler {}",
        run.run.join("bin/aterm-lab").display(),
        run.run.display()
    );
    let pause = || std::thread::sleep(Duration::from_millis(5));
    if runs_with(
        run.sampler_pid,
        ("aterm-lab", &line),
        aterm_uds::deadline::FIRST_EXEC_HANG,
        pause,
    ) {
        return Ok(());
    }
    Err(format!(
        "lab '{}': its egress witness (pid {}) is not running, so nothing more is started in its jail ({} down {0}, then up)",
        run.tag,
        if run.sampler_pid > 1 {
            run.sampler_pid.to_string()
        } else {
            "none".into()
        },
        crate::LAB
    ))
}

/// Whether `pid` lives with `line` in its command line ([`command_line`]), read once its thread
/// has run. While it has not, and the kernel names it `comm` (its record's name, read with no
/// argument read), this waits, at most `within`, calling `pause` between looks, since its words
/// read as empty until then. A pid named otherwise is another program by now (the witness's pid
/// freed and taken), so it is read at once, never waited on.
fn runs_with(
    pid: i32,
    (comm, line): (&str, &str),
    within: Duration,
    mut pause: impl FnMut(),
) -> bool {
    let end = std::time::Instant::now() + within;
    while alive(pid)
        && record(pid).is_some_and(|r| r.comm == comm)
        && aterm_uds::process::thread_ran(pid) == Some(false)
        && std::time::Instant::now() < end
    {
        pause();
    }
    alive(pid) && command_line(pid).contains(line)
}

/// Output handling mode for [`jailed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JailOut {
    /// Capture stdout and discard stderr.
    Capture,
    /// Capture both stdout and stderr.
    CaptureAll,
    /// Inherit both stdout and stderr from this process.
    Inherit,
}

/// Runs `argv` inside the lab's jail from `dir` with `envs` and `out` handling.
///
/// # Errors
/// The witness is not running, the program could not start, or waiting for it
/// failed.
pub fn jailed(
    run: &crate::run::Run,
    dir: &Path,
    vars: &[(&str, &str)],
    argv: &[&str],
    out: JailOut,
) -> Result<(i32, String), String> {
    let o = jailed_output(run, dir, vars, argv, out)?;
    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&o.stderr));
    Ok((o.status.code().unwrap_or(1), text))
}

/// The argument vector for a program in this lab's jail, with a clean environment and an
/// optional working directory bound by `env -C`. Without that binding the launcher chooses its
/// working directory. Each variable and program argument stays one argument, with no shell
/// interpretation. This only constructs the command; a launcher checks its witness.
#[must_use]
pub fn jailed_argv(
    run: &crate::run::Run,
    cwd: Option<&Path>,
    vars: &[(&str, &str)],
    argv: &[&str],
) -> Vec<String> {
    let mut words = vec![
        "/usr/bin/sandbox-exec".into(),
        "-f".into(),
        run.run.join("jail.sb").to_string_lossy().into_owned(),
        "/usr/bin/env".into(),
        "-i".into(),
    ];
    if let Some(dir) = cwd {
        words.extend(["-C".into(), dir.to_string_lossy().into_owned()]);
    }
    words.extend(["PATH=/usr/bin:/bin".into(), "xcrun_nocache=1".into()]);
    words.extend(vars.iter().map(|(k, v)| format!("{k}={v}")));
    words.extend(argv.iter().map(|arg| (*arg).to_owned()));
    words
}

/// [`jailed`]'s run, its status and its two streams apart (a mint's capability on stdout, its
/// refusal on stderr: [`crate::bus::lay`]).
///
/// # Errors
/// The witness is not running, the program could not start, or waiting for it
/// failed.
pub fn jailed_output(
    run: &crate::run::Run,
    dir: &Path,
    vars: &[(&str, &str)],
    argv: &[&str],
    out: JailOut,
) -> Result<std::process::Output, String> {
    need_witness(run)?;
    let words = jailed_argv(run, None, vars, argv);
    let (program, args) = words.split_first().ok_or("empty jail command")?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args)
        .current_dir(dir)
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    match out {
        JailOut::Capture => {
            cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null());
        }
        JailOut::CaptureAll => {
            cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::piped());
        }
        JailOut::Inherit => {}
    }
    let child = cmd.spawn().map_err(|e| {
        format!(
            "cannot start {} in the jail: {e}",
            argv.first().unwrap_or(&"")
        )
    })?;
    record_jailed(&run.run, i32::try_from(child.id()).unwrap_or(0));
    child.wait_with_output().map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod tests;
