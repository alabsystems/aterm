// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE INTERRUPT: SIGINT (Ctrl-C), SIGTERM (`kill <pid>`) and SIGHUP (a closed
//! terminal) are caught, never fatal — each one this process did not inherit IGNORED:
//! under `nohup` (or a background `&` of a shell without job control) the release keeps
//! SIG_IGN, as every program that respects it does (`if (signal(sig, SIG_IGN) !=
//! SIG_IGN) signal(sig, handler)`), and its children inherit it.
//!
//! The handler only records (atomics: async-signal-safe). While a step's child runs, a
//! forwarder thread ([`Running`]) hands the first signal to every process of the
//! child's group and a second one SIGKILL to them — every process but a Trust `trustd`
//! and what it runs ([`group_members`]). A trustd shares no group with its launcher
//! from trust `41f912aa5f4` (2026-09-22) on, but the store seal of 2026-10 (321aaeda7)
//! predates it, so a trustd a step's `targo trust check` starts (`pub stage`'s public
//! clone gate runs one) lives in that step's group — and a signalled trustd records
//! DIRTY, after which targo starts none until someone attests no solver survives
//! (AGENTS.md: never signal a trustd). So no signal goes to a group as a whole. The
//! release then sees the step end and says its INTERRUPTED line.
//!
//! `ship release`'s steps, `ship hook` and the trust lane all read it: one module, its
//! two arms below.

#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
    /// `SIG_IGN` as `signal` takes and answers it.
    const SIG_IGN: usize = 1;

    /// The first signal caught (0: none).
    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    /// How many were caught.
    static COUNT: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn kill(pid: i32, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32) {
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Catch SIGINT, SIGTERM and SIGHUP for the release's life — except a signal this
    /// process was started ignoring, which stays ignored.
    pub fn install() {
        let handler = on_signal as extern "C" fn(i32) as usize;
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `signal` takes an integer and a handler value: SIG_IGN, or
            // `on_signal`, an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work (two atomics).
            unsafe {
                if signal(sig, SIG_IGN) != SIG_IGN {
                    signal(sig, handler);
                }
            }
        }
    }

    /// The signal that interrupted the release, if one did.
    pub fn caught() -> Option<i32> {
        let sig = SIGNAL.load(Ordering::SeqCst);
        (sig != 0).then_some(sig)
    }

    /// `sig` to `pid` (a member of a step's group, never a group as a whole).
    pub fn send(pid: i32, sig: i32) {
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, sig);
        }
    }

    /// The running step's child, for as long as this value lives: a forwarder thread
    /// polls the signals caught (every 50 ms) and hands them on — so a signal that
    /// arrived between the spawn and this is handed on at its first look.
    pub struct Running {
        done: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Running {
        /// `leader` is the child's pid, which leads its own session and group.
        pub fn enter(leader: u32) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done))
            });
            Self { done, thread }
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn forward(group: i32, done: &AtomicBool) {
        let mut handed = 0u32;
        while !done.load(Ordering::SeqCst) {
            let caught = COUNT.load(Ordering::SeqCst);
            if caught > handed {
                if handed == 0 {
                    signal_group(group, SIGNAL.load(Ordering::SeqCst));
                    handed = 1;
                } else {
                    signal_group(group, SIGKILL);
                    handed = caught;
                }
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// `sig` to every member of `group` that is not a trustd's ([`super::group_members`]);
    /// SIGKILL is sent twice, the group listed again between, for what forked meanwhile.
    /// A listing that cannot be read signals the group's leader alone: never a trustd.
    fn signal_group(group: i32, sig: i32) {
        let passes = if sig == SIGKILL { 2 } else { 1 };
        for _ in 0..passes {
            let listing = aterm_uds::spawnfd::command("/bin/ps")
                .args(["-A", "-o", "pid=,ppid=,pgid=,comm="])
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let members = listing.map_or_else(|| vec![group], |l| super::group_members(&l, group));
            for pid in members {
                send(pid, sig);
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn install() {}
    pub fn caught() -> Option<i32> {
        None
    }
    pub struct Running;
    impl Running {
        pub fn enter(_leader: u32) -> Self {
            Self
        }
    }
}

pub use imp::*;

/// The members of process group `group` in a `ps -A -o pid=,ppid=,pgid=,comm=` listing
/// that a release's interrupt may signal: every one but a Trust `trustd` (its command's
/// last path component) and every descendant of one — the solvers it runs. A trustd is
/// never signalled (the module docs say why); it outlives the step by design.
#[must_use]
pub fn group_members(listing: &str, group: i32) -> Vec<i32> {
    // (pid, ppid, pgid, comm), the command being the rest of the line (paths hold spaces)
    let rows: Vec<(i32, i32, i32, &str)> = listing
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut num = || -> Option<i32> {
                let end = rest.find(char::is_whitespace)?;
                let n = rest[..end].parse().ok()?;
                rest = rest[end..].trim_start();
                Some(n)
            };
            let (pid, ppid, pgid) = (num()?, num()?, num()?);
            Some((pid, ppid, pgid, rest.trim_end()))
        })
        .collect();
    let mut spared: std::collections::BTreeSet<i32> = rows
        .iter()
        .filter(|(_, _, _, comm)| comm.rsplit('/').next() == Some("trustd"))
        .map(|(pid, ..)| *pid)
        .collect();
    loop {
        let before = spared.len();
        for (pid, ppid, ..) in &rows {
            if spared.contains(ppid) {
                spared.insert(*pid);
            }
        }
        if spared.len() == before {
            break;
        }
    }
    rows.iter()
        .filter(|(pid, _, pgid, _)| *pgid == group && !spared.contains(pid))
        .map(|(pid, ..)| *pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interrupt signals a step's group member by member, and never a Trust trustd
    /// or what it runs — even one living in the step's group (a seal before trust
    /// `41f912aa5f4` leaves it in its launcher's) — nor anything outside the group.
    #[test]
    fn the_interrupt_spares_a_trustd_and_its_solvers() {
        let listing = "\
            1     0     1 /sbin/launchd\n\
          500   400   500 /bin/sh\n\
          501   500   500 /Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/targo\n\
          502   501   500 /Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/trustd\n\
          503   502   500 z3\n\
          504   503   500 z3-worker\n\
          505   500   500 sleep\n\
          600   500   600 /usr/libexec/trustd\n\
          601     1   777 /bin/sh\n\
          garbage line\n";
        assert_eq!(group_members(listing, 500), [500, 501, 505]);
        // another group's members, and a group holding nothing, are no one's
        assert_eq!(group_members(listing, 777), [601]);
        assert!(group_members(listing, 999).is_empty());
    }
}
