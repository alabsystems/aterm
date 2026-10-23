// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The egress witness (`aterm-lab sampler <run-dir>`, started by `up`, unjailed): every second it lists the network sockets (`lsof -nP -i`) of every process in the lab's jail — the stub, the private instance's whole tree (its shells, the real Claude Code or Codex), what the lab started in the jail outside that tree (`codex-run`, the agents' `--version`, the selftests' controls) and whatever any of them left behind (an orphan launchd adopted, a process in a session of its own).

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
            let path = e.path();
            let Ok(m) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if m.file_type().is_dir() {
                if path != scratch && depth < 8 {
                    todo.push((path, depth + 1));
                }
            } else if file_stale(&m) && touch_now(&path, Touch::Accessed).unwrap_or(false) {
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
        let (tag, rest) = l.split_at(l.len().min(1));
        match tag {
            "p" => rest.clone_into(&mut pid),
            "c" => rest.clone_into(&mut cmd),
            "P" => rest.clone_into(&mut proto),
            "n" => {
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
    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
    };
    let (Ok(mut sockets), Ok(mut egress)) = (open("sockets.log"), open("egress.log")) else {
        return 1;
    };
    let _ = open("members.txt");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut ever: BTreeSet<i32> = BTreeSet::new();
    let mut runtime: BTreeSet<crate::runtime_dirs::Held> = BTreeSet::new();
    let mut kept: Option<std::time::Instant> = None;
    while let Ok(text) = std::fs::read_to_string(dir.join("env.sh")) {
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
        let pids: Vec<String> = m.iter().map(ToString::to_string).collect();
        let now = aterm_uds::spawnfd::command("/usr/sbin/lsof")
            .args(["-nP", "-i", "-a", "-p", &pids.join(","), "-F", "pcPtn"])
            .output()
            .map(|o| endpoints(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
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
mod tests {
    use super::*;

    #[test]
    fn loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid() {
        let f = "p1\ncaterm-lab\nPTCP\ntIPv4\nn127.0.0.1:50130\nn127.0.0.1:50130->127.0.0.1:50141\n\
                 p2\ncclaude\nPTCP\nn127.0.0.1:50141->127.0.0.1:50130\nn10.0.0.2:5000->160.79.104.10:443 (SYN_SENT)\n\
                 PUDP\nn[::1]:53\nn*:*\n";
        let e: Vec<String> = endpoints(f).into_iter().collect();
        assert!(e.contains(&"aterm-lab[1] TCP listen 127.0.0.1:50130".to_string()));
        assert!(e.contains(&"aterm-lab[1] TCP conn 127.0.0.1:50141".to_string()));
        assert!(e.contains(&"claude[2] TCP conn 127.0.0.1:50130".to_string()));
        assert!(e.contains(&"claude[2] TCP conn 160.79.104.10:443  EXTERNAL".to_string()));
        assert!(e.contains(&"claude[2] UDP listen [::1]:53".to_string()));
        assert!(e.contains(&"claude[2] UDP unbound *:*".to_string()));
        let f = "p3\ncaterm\nPUDP\nn*:*\nn*:5353\nPTCP\nn*:8080\n";
        let e: Vec<String> = endpoints(f).into_iter().collect();
        assert_eq!(
            e,
            [
                "aterm[3] TCP listen *:8080",
                "aterm[3] UDP listen *:5353",
                "aterm[3] UDP unbound *:*",
            ]
        );
    }

    /// The empty directories under `top` that `tmp_cleaner`'s own test
    /// (`find -dx . -fstype local ! -name . -type d -empty -mtime +3`) would
    /// delete now.
    fn cleaner_would_take(top: &Path) -> BTreeSet<std::path::PathBuf> {
        let out = aterm_uds::spawnfd::command("/usr/bin/find")
            .arg("-x")
            .arg(top)
            .args(["-type", "d", "-empty", "-mtime", "+3"])
            .output()
            .expect("find ran");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(std::path::PathBuf::from)
            .collect()
    }

    /// A LIVE LAB IS KEPT FROM macOS'S `tmp_cleaner`, which clears the
    /// `/private/tmp` the default state dir sits in: with every entry's times
    /// five days old (judged four days ahead, since a test cannot age a change
    /// time), the run dir's own files outside `scratch/` (`env.sh`, the
    /// profile, the lab's program) and the socket dir's token get their access
    /// time made now, their modification time kept; the directories the
    /// profile pins (`scratch/`, its `SCRATCH_DIRS`, the socket dir's `proj`)
    /// get their modification time made now, so the cleaner's own `find`
    /// would take none of them.
    #[test]
    fn a_live_lab_is_kept_from_the_tmp_cleaner() {
        use std::os::unix::fs::MetadataExt as _;
        struct Top(std::path::PathBuf);
        impl Drop for Top {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let top = Top(std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-keep-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&top.0);
        let run = top.0.join("state/runs/t");
        let sock = top.0.join("at.sock");
        let scratch = run.join("scratch");
        let mut dirs = vec![run.join("bin"), scratch.join("unpinned"), sock.join("proj")];
        dirs.extend(
            crate::protected::SCRATCH_DIRS
                .iter()
                .map(|n| scratch.join(n)),
        );
        for d in &dirs {
            std::fs::create_dir_all(d).unwrap();
        }
        let own = [
            run.join("env.sh"),
            run.join("jail.sb"),
            run.join("bin/aterm-lab"),
            sock.join("a.sock.token"),
        ];
        let jailed = scratch.join("home/agent.json");
        let outside = top.0.join("outside");
        for f in own.iter().chain([&jailed, &outside]) {
            std::fs::write(f, "x\n").unwrap();
        }
        std::os::unix::fs::symlink(&outside, run.join("link")).unwrap();
        let sockdir = sock.display().to_string();
        assert_eq!(keep_from_the_cleaner(&run, &sockdir, SystemTime::now()), 0);
        let old = SystemTime::now() - Duration::from_secs(5 * 24 * 3600);
        let old_s = i64::try_from(
            old.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let times = std::fs::FileTimes::new()
            .set_accessed(old)
            .set_modified(old);
        for f in own.iter().chain([&jailed, &outside]) {
            std::fs::OpenOptions::new()
                .write(true)
                .open(f)
                .unwrap()
                .set_times(times)
                .unwrap();
        }
        for d in dirs.iter().chain([&scratch, &run]) {
            std::fs::File::open(d).unwrap().set_times(times).unwrap();
        }
        let pinned: BTreeSet<std::path::PathBuf> = dirs
            .iter()
            .filter(|d| !d.ends_with("unpinned") && !d.ends_with("bin"))
            .filter(|d| std::fs::read_dir(d).unwrap().next().is_none())
            .cloned()
            .collect();
        assert!(
            pinned.contains(&sock.join("proj")) && pinned.len() > 5,
            "{pinned:?}"
        );
        let before = cleaner_would_take(&top.0);
        assert!(
            pinned.is_subset(&before) && before.contains(&scratch.join("unpinned")),
            "{before:?}"
        );
        let start = SystemTime::now() - Duration::from_secs(2);
        let start_s = i64::try_from(
            start
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let ahead = SystemTime::now() + Duration::from_secs(4 * 24 * 3600);
        let made = keep_from_the_cleaner(&run, &sockdir, ahead);
        let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
        for f in &own {
            let m = meta(f);
            assert!(m.atime() >= start_s, "{} accessed now", f.display());
            assert_eq!(
                m.mtime(),
                old_s,
                "{} keeps its modification time",
                f.display()
            );
        }
        for d in &pinned {
            assert!(meta(d).mtime() >= start_s, "{} made new", d.display());
        }
        assert!(meta(&scratch).mtime() >= start_s);
        let after = cleaner_would_take(&top.0);
        assert!(pinned.is_disjoint(&after), "{after:?}");
        assert!(made >= own.len() + pinned.len(), "{made}");
        assert_eq!(
            meta(&jailed).atime(),
            old_s,
            "the scratch's files are the jail's"
        );
        assert!(after.contains(&scratch.join("unpinned")), "{after:?}");
        assert_eq!(meta(&outside).atime(), old_s, "no link is followed");
    }
}
