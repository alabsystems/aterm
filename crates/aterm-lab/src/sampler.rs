// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The egress witness (`aterm-lab sampler <run-dir>`, started by `up`,
//! unjailed): every second it lists the network sockets (`lsof -nP -i`) of
//! every process in the lab's jail — the stub, the private instance's whole
//! tree (its shells, the real Claude Code or Codex), what the lab started in
//! the jail outside that tree (`codex-run`, the agents' `--version`, the
//! selftests' controls) and whatever any of them left behind (an orphan
//! launchd adopted, a process in a session of its own). They are found by the
//! jail's mark ([`crate::sys::jail_pids`]); only when the kernel gives no
//! answer, by the ppid walk from the stub, the instance and the live roots of
//! `<run>/jailed.pids`. It appends each NEW endpoint of each process to
//! `<run>/sockets.log` as `cmd[pid] proto kind endpoint` (the pid says which
//! process: the tab's Codex or a codex-run's), and each member it saw to
//! `<run>/members.txt` (`egress` reads the kernel's refusals of those pids).
//! An endpoint whose host is not loopback also goes to `<run>/egress.log` as
//! `EXTERNAL …` (it should stay empty: the jail makes such a connect fail
//! anyway). Each pass also records what those processes hold in a per-user
//! runtime directory of the shared `/tmp` that none of the variables tried
//! moves (Muse Code's registry), which `down` takes back
//! ([`crate::runtime_dirs`]). It lives as
//! long as the lab, not as one instance: `up` starts it before anything is
//! jailed and `down` ends it once the jail is empty, and it reads `env.sh`
//! again at every pass (the instance `restore` starts anew). It
//! also ends when `env.sh` is gone (an `up` that failed). A lab a verb owns
//! (not `up`'s) hands it the verb's lifeline (`--lifeline-fd 0`): once that is
//! cut — the verb ended however it ended, a SIGKILL included — it goes on
//! until the jail is empty and then ends, never outliving the lab it watches.
//! A sampler by nature: the kernel offers no event for "a socket was opened
//! by that tree".

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use crate::run::Run;

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
    // The verb's lifeline, when it owns the lab: cut, the witness ends once the
    // jail is empty.
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
    // env.sh gone: the lab is down (or its `up` failed)
    while let Ok(text) = std::fs::read_to_string(dir.join("env.sh")) {
        let run = Run::parse(&text);
        let m = members(&run, dir);
        if m.is_empty() && cut.load(std::sync::atomic::Ordering::SeqCst) {
            // The verb that owned the lab is gone and its jail is empty.
            return 0;
        }
        if m.is_empty() {
            // nothing in the jail yet (or any more): no `lsof -p` of nothing
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
        assert!(e.contains(&"claude[2] UDP listen *:*".to_string()));
    }
}
