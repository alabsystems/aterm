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

/// Returns `(pid, ppid)` pairs from a `ps` snapshot.
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

/// Returns `ps -o command= -p <pid>`, or an empty string.
#[must_use]
pub fn command_line(pid: i32) -> String {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
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

/// Returns the controlling TTY of `pid` (`??` if none).
#[must_use]
pub fn tty_of(pid: i32) -> String {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Returns `ps -o lstart= -p <pid>`.
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
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(jailed_file(run_dir))
    {
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

/// Reads jailed root PIDs from [`jailed_file`] (`all` includes exited PIDs).
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
        std::fs::read_to_string(run.run.join("members.txt"))
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
/// # Errors
/// Returns an error if the witness process is missing or exited.
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
    child.wait_with_output().map_err(|e| e.to_string())
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
        let deep: Vec<(i32, i32)> = (11..=90).map(|pid| (pid, pid - 1)).collect();
        let got = descendants_in(&deep, 10);
        let want: Vec<i32> = (11..=90).rev().collect();
        assert_eq!(
            got, want,
            "deep process chains past 64 levels are not truncated"
        );
        let cyclic = [(11, 10), (12, 11), (10, 12), (11, 12)];
        assert_eq!(descendants_in(&cyclic, 10), vec![12, 11]);
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
    /// right after the SIGKILL can still find it running.
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

    /// A jailed child in a session of its own kept by [`keep_jailed`] (the lab's broker) is one
    /// the sweeps see when the sandbox gives no answer: [`jail_members`] of a run with no mark
    /// takes only the process tree, and no pid the run records is the child's parent.
    #[test]
    fn a_kept_jailed_child_is_swept_without_a_sandbox_answer() {
        let root = aterm_tempfile::tempdir().unwrap();
        let run = crate::run::Run {
            run: root.path().to_path_buf(),
            ..crate::run::Run::default()
        };
        #[expect(
            clippy::disallowed_types,
            reason = "the shape of the lab's jailed launch: a std Child in a session of its own"
        )]
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("600")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::sys::in_own_session(&mut cmd);
        let pid = keep_jailed(&run.run, cmd.spawn().unwrap());
        let seen = jail_members(&run);
        let roots = jailed_roots(&run.run, false);
        crate::sys::kill(pid, libc::SIGKILL).unwrap();
        assert!(ended_within(pid, Duration::from_secs(60)), "reaped");
        assert_eq!(roots, vec![pid], "recorded as a jailed root");
        assert!(seen.contains(&pid), "swept: {seen:?} lacks {pid}");
        assert!(
            !jail_members(&run).contains(&pid),
            "an ended root is no member"
        );
    }

    /// AN EXIT THE WATCH SAW IS WAITED ON UNTIL THE CHILD IS REAPED
    /// ([`settled`]): told its exit came while the child still runs (as the
    /// kernel's event can come before the child is a zombie its parent can
    /// reap), the wait reaps it once it ends, so it never reads alive after
    /// the wait said it ended.
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

    /// A child's paths ([`open_paths`]) are its working directory, its executable and each file
    /// it holds through a descriptor, in descriptor order, a PTY master as `/dev/ptmx`; its table
    /// counts those descriptors ([`crate::sys::fd_count`]). Past its stdin, stdout and stderr the
    /// child may hold files it opened itself (dyld opens `/dev/dtracehelper` while it starts).
    #[test]
    fn a_childs_paths_and_descriptors_are_read_in_process() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("lab-procs-open-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let held = dir.join("held");
        let master = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/ptmx")
            .expect("a PTY master");
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .current_dir(&dir)
            .stdin(master)
            .stdout(std::fs::File::create(&held).unwrap())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn a holder");
        let pid = i32::try_from(child.id()).unwrap();
        let (paths, count) = (open_paths(pid), crate::sys::fd_count(pid));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let want: Vec<PathBuf> = [
            dir.as_path(),
            Path::new("/bin/sleep"),
            Path::new("/dev/ptmx"),
            held.as_path(),
            Path::new("/dev/null"),
        ]
        .map(Path::to_path_buf)
        .into();
        let paths = paths.expect("its paths");
        assert!(paths.complete);
        assert_eq!(
            paths.paths.get(..want.len()),
            Some(&want[..]),
            "{:?}",
            paths.paths
        );
        assert!(
            count.is_some_and(|count| count >= 3),
            "stdin, stdout and stderr are counted; the child can open more"
        );
    }

    #[test]
    fn only_complete_process_paths_can_report_an_absence() {
        use crate::codex_proof::Verdict;
        use aterm_uds::holders::Exe;
        use std::io::ErrorKind;

        let home = ["/Users//owner".to_owned()];
        let report = |read| {
            let seen = crate::selftest::open_files_report(7, read);
            crate::selftest::strays_verdict(
                &crate::selftest::owner_strays(&seen.text, &home, &[]),
                &[7],
                seen.complete,
            )
        };
        let read = |file: &str, cwd: Option<&str>, exe| {
            open_paths_with(
                7,
                |_| Ok(vec![PathBuf::from(file)]),
                |_| cwd.map(PathBuf::from),
                |_| exe,
            )
        };
        let scratch = "/private/tmp/owned";
        let executable = || Exe::At(PathBuf::from("/bin/sleep"));
        let complete = read(scratch, Some(scratch), executable()).expect("all paths read");
        assert!(complete.complete);
        assert_eq!(
            complete.paths,
            [
                PathBuf::from(scratch),
                PathBuf::from("/bin/sleep"),
                PathBuf::from(scratch)
            ]
        );
        assert_eq!(report(Ok(complete)), Verdict::Ok);
        for (file, cwd, exe) in [
            ("/Users//owner/file", scratch, executable()),
            (scratch, "/Users//owner", executable()),
            (
                scratch,
                scratch,
                Exe::At(PathBuf::from("/Users//owner/program")),
            ),
        ] {
            assert_eq!(report(read(file, Some(cwd), exe)), Verdict::Fail);
        }
        for file in [scratch, "/Users//owner/file"] {
            for (cwd, exe) in [(None, executable()), (Some(scratch), Exe::Unnamed)] {
                let partial = read(file, cwd, exe).expect("the descriptors read");
                assert!(!partial.complete);
                assert!(partial.paths.contains(&PathBuf::from(file)));
                assert_eq!(
                    report(Ok(partial)),
                    if file == scratch {
                        Verdict::Unproven
                    } else {
                        Verdict::Fail
                    }
                );
            }
        }
        let exited = read(scratch, Some(scratch), Exe::Exited)
            .expect_err("the executable read found an exited process");
        assert_eq!(exited.kind(), ErrorKind::NotFound);
        assert_eq!(report(Err(exited)), Verdict::Unproven);
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::Other,
        ] {
            let error = open_paths_with(
                7,
                |_| Err(std::io::Error::new(kind, "descriptor read failed")),
                |_| panic!("no cwd read after the descriptor error"),
                |_| panic!("no executable read after the descriptor error"),
            )
            .expect_err("the descriptor error is preserved");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "descriptor read failed");
            assert_eq!(report(Err(error)), Verdict::Unproven);
        }
        assert_eq!(
            open_paths(-1)
                .expect_err("invalid pid before any read")
                .kind(),
            ErrorKind::InvalidInput
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
