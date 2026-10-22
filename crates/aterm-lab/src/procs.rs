// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's processes: spawning a detached one (its own session, no
//! controlling terminal), knowing whether one still lives (reaping our own
//! children, so an ended one is never a zombie that reads alive), and the
//! process tree under an instance.

use std::collections::{BTreeMap, HashMap};
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

/// Keep `child`, forked by a std `Command` (one that hands a descriptor
/// above stdio, [`crate::lab::launch_ready`]), so [`alive`] reaps it once it
/// ends. Its pid.
pub fn keep_forked(child: std::process::Child) -> i32 {
    let pid = i32::try_from(child.id()).unwrap_or(0);
    children()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(pid, Kept::Forked(child));
    pid
}

/// Start `cmd` and keep its handle, so [`alive`] reaps it once it ends. Its pid.
///
/// # Errors
/// The spawn failed.
pub fn spawn_kept(cmd: &mut Command) -> std::io::Result<i32> {
    let child = cmd.spawn()?;
    let pid = i32::try_from(child.id()).unwrap_or(0);
    children()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(pid, Kept::Spawned(child));
    Ok(pid)
}

/// Whether `pid` lives (never 0 or 1). A child of ours that has exited is
/// reaped here and reads dead.
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

/// Wait up to `limit` for `pid` to end, on the kernel's exit event (a
/// kqueue `EVFILT_PROC` watch, so no thread and no polling); a child of ours
/// is reaped once it has. Whether it ended.
#[must_use]
pub fn wait_gone(pid: i32, limit: Duration) -> bool {
    if !alive(pid) {
        return true;
    }
    // registered while it lives; `None` means it ended in between, or is
    // ending ([`ended_within`])
    let Some(w) = u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::exitwatch::ExitWatch::watch)
    else {
        return ended_within(pid, limit);
    };
    // a child of ours that ended before the watch was registered is reaped here
    if !alive(pid) {
        return true;
    }
    let mut fds = [crate::sys::pollfd(w.as_raw_fd())];
    let ms = i32::try_from(limit.as_millis()).unwrap_or(i32::MAX);
    let _ = crate::sys::poll(&mut fds, ms);
    settled(pid, w.exit_status().is_some(), limit)
}

/// [`wait_gone`]'s answer once its watch fired or ran out (`exited`: the
/// kernel's exit event came): whether `pid` ended. The exit event can come
/// before a child of ours is a zombie its parent can reap, so where it came
/// a child of ours is looked at again until it is reaped, within `limit`
/// ([`ended_within`]): with one read of [`alive`] there, the wait said the
/// child had ended while it still read alive, and it was left unreaped
/// (`a_wait_that_runs_out_leaves_the_child_to_a_later_one` failed so once
/// in a full run, 2026-10-22, the review of `b9/resid4-lab`). Without the
/// event, `alive` reaps an ended child of ours, so none is left a zombie.
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
/// so one read of [`alive`] took a SIGKILLed child of ours for alive (the
/// merge contract's run of 2026-10-17, under load:
/// `a_wait_that_runs_out_leaves_the_child_to_a_later_one`). A child of ours is
/// looked at again until it is reaped, within `limit`; any other `pid` is read
/// once.
fn ended_within(pid: i32, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
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

/// `pid ppid` of every process (one `ps` snapshot).
#[must_use]
pub fn table() -> Vec<(i32, i32)> {
    let Ok(out) = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-axo", "pid=,ppid="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// Every descendant of `root` in `table`, children before their parents
/// (nothing for 0, 1 or less: that would be every process).
#[must_use]
pub fn descendants_in(table: &[(i32, i32)], root: i32) -> Vec<i32> {
    if root <= 1 {
        return Vec::new();
    }
    let mut kids: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for &(pid, ppid) in table {
        kids.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    fn walk(p: i32, kids: &BTreeMap<i32, Vec<i32>>, out: &mut Vec<i32>, depth: u32) {
        if depth > 64 {
            return;
        }
        for &c in kids.get(&p).map(Vec::as_slice).unwrap_or_default() {
            walk(c, kids, out, depth + 1);
            out.push(c);
        }
    }
    walk(root, &kids, &mut out, 0);
    out
}

/// Every descendant of `root` now.
#[must_use]
pub fn descendants(root: i32) -> Vec<i32> {
    descendants_in(&table(), root)
}

/// `ps -o command= -p <pid>`: the command line, or empty.
#[must_use]
pub fn command_line(pid: i32) -> String {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// `ps -o stat= -p <pid>`: the process's state (`T…` stopped, `S…`
/// sleeping); `None` when it is gone or `ps` gave nothing.
#[must_use]
pub fn state_of(pid: i32) -> Option<String> {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The tty a process holds (`??` for none).
#[must_use]
pub fn tty_of(pid: i32) -> String {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `ps -o lstart= -p <pid>`: when the process started, as `ps` prints it
/// (with its pid, a process's identity: a pid the system has given to
/// another process since has another start time). Empty when it is gone.
#[must_use]
pub fn start_time(pid: i32) -> String {
    if pid <= 1 {
        return String::new();
    }
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `<run>/jailed.pids`: each process the lab started in the jail outside the
/// instance's tree (`codex-run`, the agents' `--version`, the selftests'
/// controls), as `pid TAB start time`. The run directory is written from
/// outside the jail only.
#[must_use]
pub fn jailed_file(run_dir: &Path) -> PathBuf {
    run_dir.join("jailed.pids")
}

/// Append a jailed root to [`jailed_file`] as it starts.
pub fn record_jailed(run_dir: &Path, pid: i32) {
    use std::io::Write as _;
    let line = format!("{pid}\t{}\n", start_time(pid));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(jailed_file(run_dir))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The pids of [`jailed_file`] that are still the process the lab started
/// (`all`: every pid it names, live or not).
#[must_use]
pub fn jailed_roots(run_dir: &Path, all: bool) -> Vec<i32> {
    let text = std::fs::read_to_string(jailed_file(run_dir)).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (pid, start) = l.split_once('\t').unwrap_or((l, ""));
            let pid: i32 = pid.trim().parse().ok()?;
            (pid > 1 && (all || (!start.is_empty() && start_time(pid) == start))).then_some(pid)
        })
        .collect()
}

/// Every live process in the lab's jail, the stub's among them, without
/// repeats. Found by the jail itself ([`crate::sys::jail_pids`] of its mark):
/// an orphan launchd adopted when its parent exited, a process in a session of
/// its own, one that cleared its environment. The ppid walk from the instance
/// and the live roots of [`jailed_file`] stays beside it (an older lab's
/// `env.sh` names no mark).
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

/// The live pids of the lab's processes but the stub.
#[must_use]
pub fn lab_tree(run: &crate::run::Run) -> Vec<i32> {
    jail_members(run)
        .into_iter()
        .filter(|&p| p != run.stub_pid)
        .collect()
}

/// The pids the kernel's refusals of this lab may name: the stub, the
/// instance, every member of the jail now, every pid [`jailed_file`] ever
/// named and every member the witness saw (`members.txt`); each names the pid
/// of its day.
#[must_use]
pub fn lab_pids_ever(run: &crate::run::Run) -> Vec<i32> {
    let mut v = vec![run.instance_pid, run.stub_pid];
    v.extend(jail_members(run));
    v.extend(jailed_roots(&run.run, true));
    v.extend(
        std::fs::read_to_string(run.run.join("members.txt"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse::<i32>().ok()),
    );
    let mut seen = std::collections::BTreeSet::new();
    v.retain(|p| *p > 1 && seen.insert(*p));
    v
}

/// The lab's egress witness is running (by its pid and its command line):
/// nothing more is started in the jail without it, since a dead witness sees
/// nothing and says so to no one.
///
/// # Errors
/// The witness is not running.
pub fn need_witness(run: &crate::run::Run) -> Result<(), String> {
    let line = format!(
        "{} sampler {}",
        run.run.join("bin/aterm-lab").display(),
        run.run.display()
    );
    if alive(run.sampler_pid) && command_line(run.sampler_pid).contains(&line) {
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

/// What [`jailed`] does with the program's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JailOut {
    /// Standard output captured, standard error dropped.
    Capture,
    /// Standard output and standard error captured, in that order.
    CaptureAll,
    /// Both on this process's own.
    Inherit,
}

/// `argv` in the lab's jail, from `dir`, in a session of its own, stdin
/// `/dev/null`, with an environment of `PATH=/usr/bin:/bin`,
/// `xcrun_nocache=1` and `vars` (a later one wins), where the egress witness
/// watches it and anything it leaves behind (the jail's mark); its pid and
/// start time go into [`jailed_file`] as it starts. Its exit status, and its
/// standard output when captured.
///
/// # Errors
/// The witness is not running, or the program could not start.
pub fn jailed(
    run: &crate::run::Run,
    dir: &Path,
    vars: &[(&str, &str)],
    argv: &[&str],
    out: JailOut,
) -> Result<(i32, String), String> {
    need_witness(run)?;
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
    cmd.arg("-f").arg(run.run.join("jail.sb")).args([
        "/usr/bin/env",
        "-i",
        "PATH=/usr/bin:/bin",
        "xcrun_nocache=1",
    ]);
    for (k, v) in vars {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.args(argv)
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
    let o = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&o.stderr));
    Ok((o.status.code().unwrap_or(1), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_come_children_first_and_never_for_init() {
        let t = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1)];
        assert_eq!(descendants_in(&t, 10), vec![12, 11, 13]);
        assert!(descendants_in(&t, 1).is_empty());
        assert!(descendants_in(&t, 0).is_empty());
    }

    #[test]
    fn an_ended_child_is_reaped_and_reads_dead() {
        let pid = spawn_kept(
            aterm_uds::spawnfd::command("/usr/bin/true").stdout(aterm_uds::spawnfd::Stdio::null()),
        )
        .unwrap();
        assert!(wait_gone(pid, Duration::from_secs(60)), "true exits");
        assert!(!alive(pid));
    }

    /// A child the kernel would not watch, killed and on its way out, is
    /// waited for until it is reaped ([`ended_within`]): one read of `alive`
    /// right after the SIGKILL can still find it running. NEGATIVE CONTROLS: a
    /// child that lives is ours still when the wait runs out, and a process
    /// that is no child of ours is read once.
    #[test]
    fn a_killed_child_the_kernel_will_not_watch_is_waited_for_until_reaped() {
        let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
        assert!(
            !ended_within(pid, Duration::from_millis(50)),
            "sleep 600 lives"
        );
        assert!(alive(pid), "still ours to reap");
        crate::sys::kill(pid, libc::SIGKILL).unwrap();
        assert!(ended_within(pid, Duration::from_secs(60)), "reaped");
        assert!(!crate::sys::alive(pid), "no zombie keeps the pid");
        let me = i32::try_from(std::process::id()).unwrap();
        assert!(
            !ended_within(me, Duration::from_secs(60)),
            "not ours: read once"
        );
    }

    /// AN EXIT THE WATCH SAW IS WAITED ON UNTIL THE CHILD IS REAPED
    /// ([`settled`]): told its exit came while the child still runs (as the
    /// kernel's event can come before the child is a zombie its parent can
    /// reap), the wait reaps it once it ends, so it never reads alive after
    /// the wait said it ended. NEGATIVE CONTROLS: with no exit event, a live
    /// child is no ended one; a process that is no child of ours is read
    /// once, and the event's word stands.
    #[test]
    fn an_exit_the_watch_saw_is_waited_on_until_the_child_is_reaped() {
        let long = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
        assert!(!settled(long, false, Duration::from_secs(60)), "it runs");
        crate::sys::kill(long, libc::SIGKILL).unwrap();
        let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("0.3")).unwrap();
        assert!(settled(pid, true, Duration::from_secs(60)), "its exit seen");
        assert!(!crate::sys::alive(pid), "reaped: no zombie keeps the pid");
        assert!(!alive(pid));
        assert!(settled(long, true, Duration::from_secs(60)), "killed");
        assert!(!crate::sys::alive(long), "reaped");
        let me = i32::try_from(std::process::id()).unwrap();
        assert!(
            settled(me, true, Duration::from_secs(60)),
            "not ours: read once"
        );
    }

    #[test]
    fn a_wait_that_runs_out_leaves_the_child_to_a_later_one() {
        let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
        assert!(
            !wait_gone(pid, Duration::from_millis(50)),
            "sleep 600 lives"
        );
        assert!(alive(pid), "still ours to reap");
        crate::sys::kill(pid, libc::SIGKILL).unwrap();
        assert!(wait_gone(pid, Duration::from_secs(60)), "a SIGKILL ends it");
        assert!(!alive(pid));
        assert!(!crate::sys::alive(pid), "reaped: no zombie keeps the pid");
    }
}
