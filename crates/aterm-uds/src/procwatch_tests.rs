// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`ProcWatch`] against the real kernel (Darwin), and its bookkeeping against a scripted kernel
//! (every unix).

use super::*;
#[cfg(target_vendor = "apple")]
use crate::process::child_pids;

#[cfg(target_vendor = "apple")]
const MUST_HAPPEN: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_vendor = "apple")]
fn drain_until(
    watch: &mut ProcWatch,
    seen: &mut Vec<ProcEvent>,
    what: &str,
    done: impl Fn(&[ProcEvent]) -> bool,
) {
    let start = std::time::Instant::now();
    loop {
        watch.drain(seen);
        if done(seen) {
            return;
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: {what} never happened; saw {seen:?}"
        );
        park(watch);
    }
}

#[cfg(target_vendor = "apple")]
fn park(watch: &ProcWatch) {
    let mut fds = [libc::pollfd {
        fd: watch.fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one pollfd, live for the call, with a bounded timeout.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, 100) };
}

#[cfg(target_vendor = "apple")]
struct Shell {
    child: crate::spawnfd::Child,
}

#[cfg(target_vendor = "apple")]
impl Shell {
    fn start() -> Self {
        let child = crate::spawnfd::command("/bin/sh")
            .arg("-s")
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn /bin/sh");
        Self { child }
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.id()).expect("pid")
    }

    fn run(&mut self, line: &str) {
        use std::io::Write as _;
        let stdin = self.child.stdin.as_mut().expect("stdin");
        stdin.write_all(line.as_bytes()).expect("type");
        stdin.write_all(b"\n").expect("type");
        stdin.flush().expect("flush");
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for Shell {
    fn drop(&mut self) {
        for pid in child_pids(self.pid()).unwrap_or_default() {
            for grandchild in child_pids(pid).unwrap_or_default() {
                // SAFETY: signalling a process this test started.
                unsafe { libc::kill(grandchild, libc::SIGKILL) };
            }
            // SAFETY: as above.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_vendor = "apple")]
fn mkfifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let c_path = std::ffi::CString::new(path.display().to_string()).expect("path");
    // SAFETY: a NUL-terminated path in this test's scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    path
}

#[cfg(target_vendor = "apple")]
fn open_writer(fifo: &std::path::Path) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    let c_path = std::ffi::CString::new(fifo.display().to_string()).expect("path");
    let start = std::time::Instant::now();
    loop {
        // SAFETY: as above.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd >= 0 {
            // SAFETY: a fresh descriptor this test owns.
            return unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: nothing ever read {}",
            fifo.display()
        );
        std::thread::yield_now();
    }
}

#[cfg(target_vendor = "apple")]
fn release(gate: &std::os::fd::OwnedFd, line: &[u8]) {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `line`'s bytes, live for the call, to this test's own descriptor.
    let wrote = unsafe { libc::write(gate.as_raw_fd(), line.as_ptr().cast(), line.len()) };
    assert_eq!(
        usize::try_from(wrote).ok(),
        Some(line.len()),
        "release the gate"
    );
}

#[cfg(target_vendor = "apple")]
fn forked(seen: &[ProcEvent]) -> Vec<i32> {
    seen.iter()
        .filter_map(|e| match e {
            ProcEvent::Fork { child } => Some(*child),
            _ => None,
        })
        .collect()
}

#[cfg(target_vendor = "apple")]
fn execs_of(seen: &[ProcEvent], pid: i32) -> usize {
    seen.iter()
        .filter(|e| matches!(e, ProcEvent::Exec { pid: p, .. } if *p == pid))
        .count()
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_forked_child_is_watched_and_its_exec_reported() {
    let mut shell = Shell::start();
    let mut watch = ProcWatch::new(shell.pid()).expect("watch the shell");
    let mut seen = Vec::new();
    shell.run("/bin/sleep 30 &");
    drain_until(&mut watch, &mut seen, "the fork's exec", |seen| {
        forked(seen).iter().any(|c| execs_of(seen, *c) > 0)
    });
    let child = *forked(&seen)
        .iter()
        .find(|c| execs_of(&seen, **c) > 0)
        .expect("child");
    let fork_at = seen
        .iter()
        .position(|e| *e == ProcEvent::Fork { child })
        .expect("fork");
    let exec_at = seen
        .iter()
        .position(|e| matches!(e, ProcEvent::Exec { pid, .. } if *pid == child))
        .expect("exec");
    assert!(fork_at < exec_at, "the fork is heard first: {seen:?}");
    let info = short_info(child).expect("the child lives");
    assert!(info.execed, "PROC_FLAG_EXEC marks an exec'd child");
    if let ProcEvent::Exec { pgid, .. } = seen[exec_at] {
        assert_eq!(pgid, info.pgid, "the event carries the child's group");
    }
    // SAFETY: signalling the child this test's shell started.
    unsafe { libc::kill(child, libc::SIGKILL) };
    drain_until(&mut watch, &mut seen, "the child's exit", |seen| {
        seen.contains(&ProcEvent::Exit { pid: child })
    });
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_child_that_never_execs_is_no_exec() {
    let mut shell = Shell::start();
    let mut watch = ProcWatch::new(shell.pid()).expect("watch the shell");
    let mut seen = Vec::new();
    shell.run("( while :; do /bin/sleep 1; done ) &");
    drain_until(&mut watch, &mut seen, "the subshell's fork", |seen| {
        !forked(seen).is_empty()
    });
    let child = forked(&seen)[0];
    let start = std::time::Instant::now();
    while child_pids(child).unwrap_or_default().is_empty() {
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: the subshell never ran"
        );
        std::thread::yield_now();
    }
    watch.drain(&mut seen);
    assert_eq!(
        execs_of(&seen, child),
        0,
        "no exec of the subshell: {seen:?}"
    );
    assert!(
        !short_info(child).expect("the subshell lives").execed,
        "PROC_FLAG_EXEC stays clear on a fork that never execs"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn posix_spawn_raises_note_fork() {
    const NAME: &str = "procwatch::tests::posix_spawn_raises_note_fork";
    const ALONE_ENV: &str = "ATERM_UDS_PROCWATCH_ALONE";
    if std::env::var_os(ALONE_ENV).is_none() {
        let out = crate::deadline::output_until(
            crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(ALONE_ENV, "1"),
            std::time::Instant::now() + MUST_HAPPEN * 2,
        )
        .expect("the test, alone, within the hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("ALONE fork seen"),
            "{:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let me = i32::try_from(std::process::id()).expect("pid");
    let mut watch = ProcWatch::new(me).expect("watch this process");
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("posix_spawn");
    let pid = i32::try_from(child.id()).expect("pid");
    let mut seen = Vec::new();
    drain_until(&mut watch, &mut seen, "the spawn's fork", |seen| {
        forked(seen).contains(&pid)
    });
    assert!(
        seen.contains(&ProcEvent::Exec {
            pid,
            pgid: short_info(pid).expect("lives").pgid,
            late: true
        }),
        "a spawned child is found exec'd at registration: {seen:?}"
    );
    let _ = child.kill();
    let _ = child.wait();
    println!("ALONE fork seen");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_followed_holder_reports_its_exec_until_unfollowed() {
    let shell = Shell::start();
    let mut holder = Shell::start();
    let mut watch = ProcWatch::new(shell.pid()).expect("watch the shell");
    watch.follow(holder.pid());
    holder.run("exec /bin/sh -s");
    let mut seen = Vec::new();
    drain_until(&mut watch, &mut seen, "the holder's exec", |seen| {
        execs_of(seen, holder.pid()) > 0
    });
    watch.unfollow();
    holder.run("exec /bin/sh -s");
    holder.run("/bin/sleep 30 &");
    let start = std::time::Instant::now();
    while child_pids(holder.pid()).unwrap_or_default().is_empty() {
        assert!(start.elapsed() < MUST_HAPPEN, "hang: the holder never ran");
        std::thread::yield_now();
    }
    let before = execs_of(&seen, holder.pid());
    watch.drain(&mut seen);
    assert_eq!(
        execs_of(&seen, holder.pid()),
        before,
        "an unfollowed holder is no event: {seen:?}"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_new_watch_adopts_running_children_silently() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = mkfifo(dir.path(), "gate");
    let mut shell = Shell::start();
    shell.run(&format!("/bin/cat {} > /dev/null &", fifo.display()));
    let _gate = open_writer(&fifo);
    let children = child_pids(shell.pid()).expect("the table answers");
    let [child] = children[..] else {
        panic!("one job: {children:?}");
    };
    assert!(short_info(child).expect("lives").execed, "the job is cat");
    let mut watch = ProcWatch::new(shell.pid()).expect("watch the shell");
    let mut seen = Vec::new();
    watch.drain(&mut seen);
    assert!(seen.is_empty(), "no past events: {seen:?}");
    // The job ends by `SIGKILL`, not by closing the gate: closed just as `cat`'s open returns,
    // the FIFO's only writer can leave `cat` asleep in its read with no writer left (macOS 27
    // under load, 2026-10-29), and its exit never comes. The gate stays open to the end.
    // SAFETY: signalling the job this test's shell started.
    unsafe { libc::kill(child, libc::SIGKILL) };
    drain_until(&mut watch, &mut seen, "the adopted job's exit", |seen| {
        seen.contains(&ProcEvent::Exit { pid: child })
    });
    assert_eq!(
        execs_of(&seen, child),
        0,
        "no exec of the adopted job: {seen:?}"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_child_past_the_cap_is_counted_and_heard_only_once_followed() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = mkfifo(dir.path(), "gate");
    let mut shell = Shell::start();
    for _ in 0..MAX_CHILDREN {
        shell.run("/bin/sleep 30 &");
    }
    let start = std::time::Instant::now();
    let held = loop {
        let now = child_pids(shell.pid()).unwrap_or_default();
        if now.len() == MAX_CHILDREN {
            break now;
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: the shell never held {MAX_CHILDREN} jobs: {now:?}"
        );
        std::thread::yield_now();
    };
    let mut watch = ProcWatch::new(shell.pid()).expect("watch the shell");
    assert_eq!(watch.overflow(), 0, "the cap's worth adopted");
    shell.run(&format!(
        "( read x < {}; exec /bin/sleep 30 ) &",
        fifo.display()
    ));
    let gate = open_writer(&fifo);
    let extra = child_pids(shell.pid())
        .unwrap_or_default()
        .into_iter()
        .find(|pid| !held.contains(pid))
        .expect("the 17th job");
    let mut seen = Vec::new();
    let start = std::time::Instant::now();
    loop {
        watch.drain(&mut seen);
        if watch.overflow() > 0 {
            break;
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: the 17th fork was never heard; saw {seen:?}"
        );
        park(&watch);
    }
    assert_eq!(watch.overflow(), 1);
    assert!(
        !forked(&seen).contains(&extra),
        "no Fork past the cap: {seen:?}"
    );
    watch.follow(extra);
    release(&gate, b"go\n");
    drain_until(&mut watch, &mut seen, "the followed child's exec", |seen| {
        execs_of(seen, extra) > 0
    });
    assert!(
        seen.iter().any(|e| matches!(
            e,
            ProcEvent::Exec { pid, late: false, .. } if *pid == extra
        )),
        "a fresh exec of the followed child: {seen:?}"
    );
    assert_eq!(watch.overflow(), 1, "counted once");
}

#[cfg(not(target_vendor = "apple"))]
#[test]
fn no_watch_off_darwin() {
    assert!(ProcWatch::new(i32::try_from(std::process::id()).expect("pid")).is_none());
}

#[test]
fn a_gone_shell_is_no_watch() {
    assert!(ProcWatch::new(1).is_none());
    assert!(ProcWatch::new(0).is_none());
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Register(Vec<i32>),
    Delete(i32),
    Info(i32),
}

#[derive(Default)]
struct Scripted {
    calls: Vec<Call>,
    execed: Vec<i32>,
}

impl Kernel for Scripted {
    fn register(&mut self, pids: &[i32]) -> Vec<bool> {
        self.calls.push(Call::Register(pids.to_vec()));
        vec![true; pids.len()]
    }

    fn delete(&mut self, pid: i32) {
        self.calls.push(Call::Delete(pid));
    }

    fn info(&mut self, pid: i32) -> Option<ProcShort> {
        self.calls.push(Call::Info(pid));
        Some(ProcShort {
            pgid: pid,
            execed: self.execed.contains(&pid),
        })
    }
}

impl Scripted {
    fn read_after_registered(&self) -> Result<(), String> {
        for (at, call) in self.calls.iter().enumerate() {
            if let Call::Info(pid) = call
                && !self.calls[..at]
                    .iter()
                    .any(|c| matches!(c, Call::Register(p) if p.contains(pid)))
            {
                return Err(format!(
                    "{pid} read before it was registered: {:?}",
                    self.calls
                ));
            }
        }
        Ok(())
    }
}

#[test]
fn every_process_is_registered_before_its_image_is_read() {
    let mut kernel = Scripted {
        execed: vec![101, 200, 300],
        ..Scripted::default()
    };
    let mut tracked = Tracked::default();
    let mut out = Vec::new();
    tracked.forked(&[100, 101], &mut kernel, &mut out);
    assert_eq!(
        out,
        [
            ProcEvent::Fork { child: 100 },
            ProcEvent::Fork { child: 101 },
            ProcEvent::Exec {
                pid: 101,
                pgid: 101,
                late: true
            },
        ]
    );
    assert_eq!(
        kernel.calls[0],
        Call::Register(vec![100, 101]),
        "one registration, before any read"
    );
    tracked.adopt(200, &mut kernel);
    tracked.adopt(201, &mut kernel);
    tracked.follow(300, 1, &mut kernel);
    assert_eq!(kernel.read_after_registered(), Ok(()));
    for pid in [101, 200, 300] {
        assert!(tracked.take_late(pid), "{pid} late once");
        assert!(!tracked.take_late(pid), "{pid} fresh after");
    }
    assert!(!tracked.take_late(100) && !tracked.take_late(201));
}

#[test]
fn a_child_past_the_cap_counts_once_and_an_exit_makes_room() {
    let mut kernel = Scripted::default();
    let mut tracked = Tracked::default();
    let mut out = Vec::new();
    let first: Vec<i32> = (1000..1020).collect();
    tracked.forked(&first, &mut kernel, &mut out);
    assert_eq!(tracked.children.len(), MAX_CHILDREN);
    assert_eq!(tracked.overflow, 4);
    assert_eq!(
        out.len(),
        MAX_CHILDREN,
        "a Fork for each watched child only"
    );
    let mut again = first.clone();
    again.push(1020);
    tracked.forked(&again, &mut kernel, &mut out);
    assert_eq!(
        tracked.overflow, 5,
        "the old four count once; the new one once"
    );
    tracked.exited(1000);
    let now: Vec<i32> = again
        .iter()
        .copied()
        .filter(|p| ![1000, 1016].contains(p))
        .chain([1021])
        .collect();
    let before = out.len();
    tracked.forked(&now, &mut kernel, &mut out);
    assert_eq!(out[before..], [ProcEvent::Fork { child: 1021 }]);
    assert_eq!(tracked.overflow, 5);
    assert!(!tracked.unwatched.contains(&1016));
    let calls = kernel.calls.len();
    tracked.follow(1001, 1, &mut kernel);
    tracked.follow(1, 1, &mut kernel);
    tracked.follow(1017, 1, &mut kernel);
    tracked.follow(1017, 1, &mut kernel);
    assert_eq!(
        kernel.calls[calls..],
        [Call::Register(vec![1017]), Call::Info(1017)],
        "{:?}",
        kernel.calls
    );
    tracked.unfollow(&mut kernel);
    assert_eq!(kernel.calls.last(), Some(&Call::Delete(1017)));
}
