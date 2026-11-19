// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Linux proof's guest-native supervisor for a cargo test or bench row.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use aterm_uds::deadline::{ProcessProgressProbe, ProgressWindow};
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_none() {
        eprintln!("usage: row_watch PROGRAM [ARGS...]");
        return std::process::ExitCode::from(2);
    }
    static STOP: AtomicI32 = AtomicI32::new(0);
    extern "C" fn stop(signal: i32) {
        STOP.store(signal, Ordering::Relaxed);
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler has the signal ABI, lives until exit and only stores an atomic.
        if unsafe { libc::signal(signal, stop as *const () as libc::sighandler_t) } == libc::SIG_ERR
        {
            eprintln!(
                "row-watch: installing signal handler: {}",
                std::io::Error::last_os_error()
            );
            return std::process::ExitCode::FAILURE;
        }
    }
    let mut window = ProgressWindow::new(Duration::from_secs(60), Instant::now());
    let mut tree = ProcessProgressProbe::default();
    let result = supervise(args, &mut std::io::stdout().lock(), |child, bytes| {
        if STOP.load(Ordering::Relaxed) != 0 {
            return true;
        }
        let pid = match child {
            Child::Pending => return false,
            Child::Running(root) => Some(root),
            Child::Exited => None,
        };
        let left = window.observe(Instant::now(), bytes, || {
            pid.map_or(Some(0), |root| tree.read(root))
        });
        if left.is_zero() {
            eprintln!(
                "\nrow-watch: no output or process-tree CPU or I/O progress for 60s; {}",
                evidence(pid)
            );
        }
        left.is_zero()
    });
    let code = match result {
        Ok(Some(status)) => status
            .code()
            .or_else(|| status.signal().map(|s| 128 + s))
            .unwrap_or(1),
        Ok(None) => match STOP.load(Ordering::Relaxed) {
            0 => 124,
            signal => 128 + signal,
        },
        Err(error) => {
            eprintln!("row-watch: {error}");
            1
        }
    };
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("row_watch requires Unix process groups");
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Child {
    Pending,
    Running(u32),
    Exited,
}

#[cfg(unix)]
fn supervise(
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    out: &mut impl std::io::Write,
    mut stopped: impl FnMut(Child, u64) -> bool,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    use std::cell::Cell;

    // Cargo's stderr announces a binary before its libtest stdout. Merge in the child so
    // the host tally sees that order even if both pipes would be readable on the same poll.
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args(["-c", "exec \"$@\" 2>&1", "row-watch"])
        .args(args);
    let child = Cell::new(Child::Pending);
    let written = Cell::new(0_u64);
    aterm_uds::deadline::stream_until_stopped_observing_exit(
        &mut command,
        None,
        |born| child.set(Child::Running(born)),
        |_, chunk| {
            out.write_all(chunk)?;
            out.flush()?;
            written.set(written.get().saturating_add(chunk.len() as u64));
            Ok(())
        },
        || stopped(child.get(), written.get()),
        |_| child.set(Child::Exited),
    )
}

#[cfg(unix)]
fn evidence(pid: Option<u32>) -> String {
    let Some(root) = pid else {
        return "the root exited and its output is still open".into();
    };
    let Some(sweep) = aterm_sysprobe::processes() else {
        return format!("root={root} process-table=unavailable");
    };
    evidence_from(root, &sweep)
}

#[cfg(unix)]
fn evidence_from(root: u32, sweep: &aterm_sysprobe::Sweep) -> String {
    use std::collections::BTreeSet;
    use std::fmt::Write as _;

    let mut detail = format!("root={root}");
    let descendants = aterm_sysprobe::procs::tree_in(sweep, root, sweep.procs.len());
    if descendants.is_none() {
        detail.push_str(" descendants=unavailable");
    }
    // A helper may lead its own group. An orphan may remain in the original group.
    let mut seen = BTreeSet::new();
    let mut members = descendants
        .iter()
        .flatten()
        .chain(sweep.procs.iter().filter(|p| p.pgid == root))
        .filter(|p| seen.insert(p.pid));
    for member in members.by_ref().take(16) {
        let _ = write!(
            detail,
            " [pid={} parent={} group={} {:?} {:?}",
            member.pid, member.ppid, member.pgid, member.name, member.state
        );
        #[cfg(target_os = "linux")]
        task_waits(&mut detail, member.pid);
        detail.push(']');
    }
    if members.next().is_some() {
        detail.push_str(" additional processes omitted");
    }
    detail
}

#[cfg(target_os = "linux")]
fn task_waits(detail: &mut String, pid: u32) {
    use aterm_uds::atomic_file::read_regular_text;
    use std::fmt::Write as _;

    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return;
    };
    for entry in tasks.flatten().take(16) {
        let task = entry.path();
        if let Ok(wait) = read_regular_text(&task.join("wchan"), 4096) {
            let _ = write!(
                detail,
                " {}={:?}",
                entry.file_name().to_string_lossy(),
                wait.trim()
            );
            if let Ok(line) = read_regular_text(&task.join("syscall"), 4096)
                && let Some(number) = syscall_number(&line)
            {
                let _ = write!(detail, "(syscall={number})");
            }
        }
    }
}

/// The kernel's call number alone. The rest of the line holds arguments and addresses.
#[cfg(all(unix, any(target_os = "linux", test)))]
fn syscall_number(line: &str) -> Option<u32> {
    line.split_whitespace().next()?.parse().ok()
}

#[cfg(all(test, unix))]
#[path = "tests.rs"]
mod tests;
