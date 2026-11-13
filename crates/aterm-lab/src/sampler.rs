// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The egress witness (`aterm-lab sampler <run-dir>`, started by `up`, unjailed): every second it lists the network sockets (`lsof -nP -i`) of every process in the lab's jail — the stub, the private instance's whole tree (its shells, the real Claude Code or Codex), what the lab started in the jail outside that tree (`codex-run`, the agents' `--version`, the selftests' controls) and whatever any of them left behind (an orphan launchd adopted, a process in a session of its own). A fork under 2 s old that has not exec'd, and a process exiting, wait for a later second (`aterm_sysprobe::jobs::sockets_readable`).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::run::Run;

/// How old every time of a live lab's file (a directory's modification time)
/// may grow before [`keep_from_the_cleaner`] makes it new: a day, well inside
/// the three days after which `tmp_cleaner` (`/usr/libexec/tmp_cleaner`, run
/// daily by `com.apple.tmp_cleaner`) deletes a file whose access, change and
/// modification times are all older, and an empty directory whose
/// modification time is.
const STALE: Duration = Duration::from_secs(24 * 3600);

/// How often the witness looks for stale entries: hourly, and at its start.
const KEEP_EVERY: Duration = Duration::from_secs(3600);

/// Keep the live lab at `run_dir` (and its socket dir `sockdir`, when named) from `tmp_cleaner`, judging staleness as of `now` ([`STALE`]); returns how many entries it made new.
pub fn keep_from_the_cleaner(run_dir: &Path, sockdir: &str, now: SystemTime) -> usize {
    use crate::sys::{Touch, touch_now};
    let before = now
        .checked_sub(STALE)
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let file_stale = |m: &std::fs::Metadata| {
        m.file_type().is_file() && m.atime() < before && m.mtime() < before && m.ctime() < before
    };
    let dir_stale = |m: &std::fs::Metadata| m.file_type().is_dir() && m.mtime() < before;
    let mut made = 0;
    let scratch = run_dir.join("scratch");
    let mut todo = vec![(run_dir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else {
                continue;
            };
            if m.file_type().is_dir() {
                let path = e.path();
                if path != scratch && depth < 8 {
                    todo.push((path, depth + 1));
                }
            } else if file_stale(&m) && touch_now(&e.path(), Touch::Accessed).unwrap_or(false) {
                made += 1;
            }
        }
    }
    let mut pinned: Vec<(std::path::PathBuf, Touch)> = vec![(scratch.clone(), Touch::Modified)];
    pinned.extend(
        crate::protected::SCRATCH_DIRS
            .iter()
            .map(|n| (scratch.join(n), Touch::Modified)),
    );
    if !sockdir.is_empty() {
        let sock = Path::new(sockdir);
        pinned.push((sock.join("proj"), Touch::Modified));
        pinned.push((sock.join("a.sock.token"), Touch::Accessed));
    }
    for (path, which) in pinned {
        let Ok(m) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let stale = match which {
            Touch::Modified => dir_stale(&m),
            Touch::Accessed => file_stale(&m),
        };
        if stale && touch_now(&path, which).unwrap_or(false) {
            made += 1;
        }
    }
    made
}

/// The `cmd[pid] proto kind endpoint[  EXTERNAL]` lines of `lsof -F pcPtn` output.
#[must_use]
pub fn endpoints(lsof_f: &str) -> BTreeSet<String> {
    let (mut pid, mut cmd, mut proto) = (String::new(), String::new(), String::new());
    let mut out = BTreeSet::new();
    for l in lsof_f.lines() {
        let mut chars = l.chars();
        let Some(tag) = chars.next() else {
            continue;
        };
        let rest = chars.as_str();
        match tag {
            'p' => rest.clone_into(&mut pid),
            'c' => rest.clone_into(&mut cmd),
            'P' => rest.clone_into(&mut proto),
            'n' => {
                let (kind, remote) = match rest.split_once("->") {
                    Some((_, r)) => ("conn", r),
                    None if rest.split(" (").next() == Some("*:*") => ("unbound", rest),
                    None => ("listen", rest),
                };
                let remote = remote.split(" (").next().unwrap_or(remote);
                let host = match remote.rfind(':') {
                    Some(i)
                        if remote[i + 1..] == *"*"
                            || remote[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
                    {
                        &remote[..i]
                    }
                    _ => remote,
                };
                let host = host.replace(['[', ']'], "");
                let lo = ["127.0.0.1", "::1", "localhost", "*", ""].contains(&host.as_str());
                out.insert(format!(
                    "{cmd}[{pid}] {proto} {kind} {remote}{}",
                    if lo { "" } else { "  EXTERNAL" }
                ));
            }
            _ => {}
        }
    }
    out
}

/// Every live process in the jail: by its mark; the ppid walk only when the
/// kernel gives no answer (the witness outlives the instance, and the pid of a
/// dead one may be another process now).
fn members(run: &Run, dir: &Path) -> Vec<i32> {
    if !run.mark.is_empty()
        && let Ok(m) = crate::sys::jail_pids(&run.mark)
    {
        return m;
    }
    let table = crate::procs::table();
    let mut v = vec![run.stub_pid];
    for r in std::iter::once(run.instance_pid).chain(crate::procs::jailed_roots(dir, false)) {
        v.push(r);
        v.extend(crate::procs::descendants_in(&table, r));
    }
    let mut seen = BTreeSet::new();
    v.retain(|p| *p > 1 && crate::sys::alive(*p) && seen.insert(*p));
    v
}

/// `aterm-lab sampler <run-dir> [--lifeline-fd <n>]`.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let Some(dir) = args.first() else {
        eprintln!("usage: aterm-lab sampler <run-dir> [--lifeline-fd <n>]");
        return 2;
    };
    let dir = Path::new(dir);
    let cut = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match args.get(1..) {
        Some([flag, n]) if flag == aterm_uds::lifeline::FLAG => {
            let Some(fd) = n.parse::<i32>().ok() else {
                eprintln!("sampler: {flag} expects a descriptor number, got {n}");
                return 2;
            };
            match aterm_uds::lifeline::adopt(fd) {
                Ok(owned) => {
                    let cut = std::sync::Arc::clone(&cut);
                    std::thread::spawn(move || {
                        let _ = aterm_uds::lifeline::wait_for_cut(owned);
                        cut.store(true, std::sync::atomic::Ordering::SeqCst);
                    });
                }
                Err(e) => {
                    eprintln!("sampler: {flag} {fd}: {e}");
                    return 2;
                }
            }
        }
        Some([]) | None => {}
        Some(rest) => {
            eprintln!("sampler: unexpected arguments {rest:?}");
            return 2;
        }
    }
    let open = |name: &str| crate::util::append_regular_file(&dir.join(name));
    let (Ok(mut sockets), Ok(mut egress)) = (open("sockets.log"), open("egress.log")) else {
        return 1;
    };
    let _ = open("members.txt");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut ever: BTreeSet<i32> = BTreeSet::new();
    let mut runtime: BTreeSet<crate::runtime_dirs::Held> = BTreeSet::new();
    let mut kept: Option<std::time::Instant> = None;
    while let Ok(text) = crate::util::read_regular_text(&dir.join("env.sh")) {
        let run = Run::parse(&text);
        if kept.is_none_or(|t| t.elapsed() >= KEEP_EVERY) {
            keep_from_the_cleaner(dir, &run.sockdir, SystemTime::now());
            kept = Some(std::time::Instant::now());
        }
        let m = members(&run, dir);
        if m.is_empty() && cut.load(std::sync::atomic::Ordering::SeqCst) {
            // The verb that owned the lab is gone and its jail is empty.
            return 0;
        }
        if m.is_empty() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if m.iter().any(|p| !ever.contains(p)) {
            ever.extend(m.iter().copied());
            let body: String = ever.iter().map(|p| format!("{p}\n")).collect();
            let _ = crate::util::atomic_write(&dir.join("members.txt"), body.as_bytes());
        }
        // `lsof -i` reads every descriptor of each member it lists (`PROC_PIDFDSOCKETINFO` for a
        // socket), and a read of a socket its holder closes meanwhile drains it. A fork that may be
        // mid-spawn (its exec closes each socket it inherited) and a member exiting are left to a
        // later second (`aterm_sysprobe::jobs::sockets_readable`); a spawn's or a fresh program's
        // closes this timer meets by chance alone. The kernel's TCP and UDP tables read no
        // descriptor, but show a reader that is no Apple platform binary its own sockets alone.
        let pids: Vec<String> = m
            .iter()
            .filter(|p| u32::try_from(**p).is_ok_and(aterm_sysprobe::jobs::sockets_readable))
            .map(ToString::to_string)
            .collect();
        let now = if pids.is_empty() {
            BTreeSet::new()
        } else {
            aterm_uds::spawnfd::command("/usr/sbin/lsof")
                .args(["-nP", "-i", "-a", "-p", &pids.join(","), "-F", "pcPtn"])
                .output()
                .map(|o| endpoints(&String::from_utf8_lossy(&o.stdout)))
                .unwrap_or_default()
        };
        for line in now.difference(&seen) {
            let ts = crate::util::hms(crate::util::now_secs());
            let _ = sockets.write_all(format!("{ts} {line}\n").as_bytes());
            if line.ends_with("EXTERNAL") {
                let _ = egress.write_all(format!("{ts} EXTERNAL {line}\n").as_bytes());
            }
        }
        seen.extend(now);
        crate::runtime_dirs::witness(dir, &m, &mut runtime);
        std::thread::sleep(Duration::from_secs(1));
    }
    0
}

#[cfg(test)]
#[path = "sampler_tests.rs"]
mod tests;
