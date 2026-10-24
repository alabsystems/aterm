// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal handling and process-group forwarding for release steps.

#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
    const SIG_IGN: usize = 1;

    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    static COUNT: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn kill(pid: i32, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32) {
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Catches `SIGINT`, `SIGTERM`, and `SIGHUP` unless already ignored.
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

    /// Sends `sig` to `pid`.
    pub fn send(pid: i32, sig: i32) {
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, sig);
        }
    }

    /// Guard that forwards caught signals to a running step's process group.
    pub struct Running {
        done: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        _credentials: crate::credential_wait::Watch,
    }

    impl Running {
        /// Starts forwarding caught signals for the process group led by `leader`.
        pub fn enter(leader: u32) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done))
            });
            Self {
                done,
                thread,
                _credentials: crate::credential_wait::Watch::enter(leader),
            }
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

/// Returns signalable members of process group `group`, sparing `trustd` and its descendants.
#[must_use]
pub fn group_members(listing: &str, group: i32) -> Vec<i32> {
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
        assert_eq!(group_members(listing, 777), [601]);
        assert!(group_members(listing, 999).is_empty());
    }
}
