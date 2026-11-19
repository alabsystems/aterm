// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel `ProcWatch` tests over an interactive `zsh` pseudo-terminal.

use std::io;
use std::time::{Duration, Instant};

use aterm_uds::process::child_pids;
use aterm_uds::procwatch::{ProcEvent, ProcWatch, short_info};

use crate::foreground_handback::{FgCutter, ProcSeen};
use crate::holder_image::EdgeKind;

const MUST_HAPPEN: Duration = aterm_spec::wait::MUST_HAPPEN;

struct Zsh {
    shell: aterm_pty::SpawnedShell,
    out: Vec<u8>,
}

impl Zsh {
    fn start() -> Self {
        // SAFETY: a test process; the trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<std::ffi::OsString> = vec!["/bin/zsh".into(), "-f".into(), "-i".into()];
        let shell = aterm_pty::spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
            None,
            None,
        )
        .expect("zsh must spawn");
        aterm_pty::set_nonblocking(shell.master, true).expect("nonblocking master");
        let mut zsh = Self {
            shell,
            out: Vec::new(),
        };
        zsh.run_until("print RE\"\"ADY", "READY");
        zsh
    }

    fn pid(&self) -> i32 {
        self.shell.pid
    }

    fn fg(&self) -> i32 {
        // SAFETY: `tcgetpgrp` on the live master this rig owns.
        unsafe { libc::tcgetpgrp(self.shell.master) }
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 4096];
        loop {
            let n = aterm_pty::read(self.shell.master, &mut buf);
            let Ok(n) = usize::try_from(n) else { return };
            if n == 0 {
                return;
            }
            self.out.extend_from_slice(&buf[..n]);
        }
    }

    fn output_tail(&self) -> &[u8] {
        &self.out[self.out.len().saturating_sub(16 * 1024)..]
    }

    fn type_line(&mut self, line: &str) {
        let bytes = format!("{line}\r");
        let master = self.shell.master;
        let start = Instant::now();
        let mut written = 0;
        let result = submit_command_with(
            bytes.as_bytes(),
            |pending| {
                // SAFETY: this rig owns the live master, and `pending` is valid for its length.
                // One attempt leaves every retry, including EINTR, under the helper's bound.
                let n = unsafe { libc::write(master, pending.as_ptr().cast(), pending.len()) };
                if n < 0 {
                    return Err(io::Error::last_os_error());
                }
                let n = usize::try_from(n).expect("a nonnegative write fits usize");
                written += n;
                Ok(n)
            },
            || {
                self.pump();
                std::thread::sleep(Duration::from_millis(5));
            },
            || start.elapsed() >= MUST_HAPPEN,
        );
        if let Err(error) = result {
            self.pump();
            panic!(
                "command {line:?} stopped after {written}/{} bytes: {error} (errno {:?}); \
                 PTY {} bytes, tail {:?}",
                bytes.len(),
                error.raw_os_error(),
                self.out.len(),
                String::from_utf8_lossy(self.output_tail())
            );
        }
    }

    fn run_until(&mut self, line: &str, needle: &str) {
        let from = self.out.len();
        self.type_line(line);
        let start = Instant::now();
        while !String::from_utf8_lossy(&self.out[from..]).contains(needle) {
            assert!(
                start.elapsed() < MUST_HAPPEN,
                "hang: `{line}` never printed {needle}: {:?}",
                String::from_utf8_lossy(&self.out[from..])
            );
            std::thread::sleep(Duration::from_millis(5));
            self.pump();
        }
    }
}

impl Drop for Zsh {
    fn drop(&mut self) {
        for child in child_pids(self.pid()).unwrap_or_default() {
            // SAFETY: signalling a job this test's shell started.
            unsafe { libc::kill(child, libc::SIGKILL) };
        }
        // SAFETY: as above, the shell itself; then its master, which this rig owns.
        unsafe {
            libc::kill(self.pid(), libc::SIGKILL);
            libc::close(self.shell.master);
        }
    }
}

struct Heard {
    events: Vec<ProcEvent>,
    fg: i32,
}

fn hear_until(
    zsh: &mut Zsh,
    watch: &mut ProcWatch,
    what: &str,
    done: impl Fn(&[Heard]) -> bool,
) -> Vec<Heard> {
    let mut heard = Vec::new();
    let start = Instant::now();
    loop {
        let mut events = Vec::new();
        watch.drain(&mut events);
        if !events.is_empty() {
            let fg = zsh.fg();
            heard.push(Heard { events, fg });
        }
        zsh.pump();
        if done(&heard) {
            return heard;
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: {what} never happened; heard {:?}",
            heard.iter().map(|h| &h.events).collect::<Vec<_>>()
        );
        let mut fds = [libc::pollfd {
            fd: watch.fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one pollfd, live for the call, with a bounded timeout.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, 20) };
    }
}

fn forks(heard: &[Heard]) -> Vec<i32> {
    heard
        .iter()
        .flat_map(|h| &h.events)
        .filter_map(|e| match e {
            ProcEvent::Fork { child } => Some(*child),
            _ => None,
        })
        .collect()
}

fn first_exec(heard: &[Heard], pid: i32) -> Option<(&Heard, i32)> {
    heard.iter().find_map(|h| {
        h.events.iter().find_map(|e| match e {
            ProcEvent::Exec { pid: p, pgid, .. } if *p == pid => Some((h, *pgid)),
            _ => None,
        })
    })
}

fn execs_of(heard: &[Heard], pid: i32) -> Vec<bool> {
    heard
        .iter()
        .flat_map(|h| &h.events)
        .filter_map(|e| match e {
            ProcEvent::Exec { pid: p, late, .. } if *p == pid => Some(*late),
            _ => None,
        })
        .collect()
}

fn mkfifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let c_path = std::ffi::CString::new(path.display().to_string()).expect("path");
    // SAFETY: a NUL-terminated path in this test's scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    path
}

fn submit_command_with(
    bytes: &[u8],
    mut write: impl FnMut(&[u8]) -> io::Result<usize>,
    mut wait: impl FnMut(),
    mut expired: impl FnMut() -> bool,
) -> io::Result<()> {
    let mut pending = bytes;
    while !pending.is_empty() {
        if expired() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command submission made no completion within the hang bound",
            ));
        }
        match write(pending) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "PTY accepted no command bytes",
                ));
            }
            Ok(n) => {
                pending = pending.get(n..).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "PTY wrote beyond the command")
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => wait(),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[derive(Debug)]
enum FifoOpenError {
    Refused(io::Error),
    TimedOut(io::Error),
}

fn fifo_writer_with<T>(
    mut open: impl FnMut() -> io::Result<T>,
    mut wait: impl FnMut(),
    mut expired: impl FnMut() -> bool,
) -> Result<T, FifoOpenError> {
    loop {
        match open() {
            Ok(writer) => return Ok(writer),
            Err(error) => {
                if error.raw_os_error() != Some(libc::ENXIO)
                    && error.kind() != io::ErrorKind::Interrupted
                {
                    return Err(FifoOpenError::Refused(error));
                }
                if expired() {
                    return Err(FifoOpenError::TimedOut(error));
                }
                wait();
            }
        }
    }
}

fn open_writer(zsh: &mut Zsh, fifo: &std::path::Path) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    let c_path = std::ffi::CString::new(fifo.display().to_string()).expect("path");
    let start = Instant::now();
    let result = fifo_writer_with(
        || {
            // SAFETY: a NUL-terminated path in this test's scratch folder.
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a descriptor this test just opened and now owns.
            Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
        },
        || {
            zsh.pump();
            std::thread::sleep(Duration::from_millis(5));
        },
        || start.elapsed() >= MUST_HAPPEN,
    );
    match result {
        Ok(writer) => writer,
        Err(failure) => {
            zsh.pump();
            let (what, error) = match failure {
                FifoOpenError::Refused(error) => ("FIFO open failed", error),
                FifoOpenError::TimedOut(error) => {
                    ("hang: no FIFO reader within the hang bound", error)
                }
            };
            panic!(
                "{what}: {}: {error} (errno {:?}); PTY {} bytes, tail {:?}",
                fifo.display(),
                error.raw_os_error(),
                zsh.out.len(),
                String::from_utf8_lossy(zsh.output_tail())
            );
        }
    }
}

#[test]
fn fixture_command_submission_keeps_the_whole_line_and_real_failures() {
    use std::cell::Cell;

    let command = b"reroute\r";
    let mut submitted = Vec::new();
    let mut calls = 0;
    let waits = Cell::new(0);
    submit_command_with(
        command,
        |pending| {
            calls += 1;
            match calls {
                1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                2 => {
                    submitted.extend_from_slice(&pending[..2]);
                    Ok(2)
                }
                3 => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                _ => {
                    submitted.extend_from_slice(pending);
                    Ok(pending.len())
                }
            }
        },
        || waits.set(waits.get() + 1),
        || false,
    )
    .expect("every command byte is submitted");
    assert_eq!(submitted, command);
    assert_eq!(waits.get(), 1);

    let failed = submit_command_with(
        command,
        |_| Err(io::Error::from_raw_os_error(libc::EIO)),
        || panic!("a real write error is not backpressure"),
        || false,
    )
    .expect_err("a failed write cannot count as a submitted command");
    assert_eq!(failed.raw_os_error(), Some(libc::EIO));
    assert_eq!(
        submit_command_with(command, |_| Ok(0), || {}, || false)
            .expect_err("a zero write is not completion")
            .kind(),
        io::ErrorKind::WriteZero
    );

    for kind in [io::ErrorKind::Interrupted, io::ErrorKind::WouldBlock] {
        let looks = Cell::new(0);
        let attempts = Cell::new(0);
        let failed = submit_command_with(
            command,
            |_| {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from(kind))
            },
            || {},
            || {
                looks.set(looks.get() + 1);
                looks.get() > 2
            },
        )
        .expect_err("every retry observes the same hang bound");
        assert_eq!(failed.kind(), io::ErrorKind::TimedOut);
        assert_eq!(attempts.get(), 2);
    }
}

#[test]
fn fixture_fifo_opening_waits_only_for_a_reader_and_retains_errors() {
    use std::cell::Cell;

    let mut attempts = 0;
    let waits = Cell::new(0);
    let writer = fifo_writer_with(
        || {
            attempts += 1;
            match attempts {
                1 => Err(io::Error::from_raw_os_error(libc::ENXIO)),
                2 => Err(io::Error::from_raw_os_error(libc::EINTR)),
                _ => Ok(17),
            }
        },
        || waits.set(waits.get() + 1),
        || false,
    )
    .expect("a reader eventually opens the FIFO");
    assert_eq!(writer, 17);
    assert_eq!(waits.get(), 2);

    let failed = fifo_writer_with::<()>(
        || Err(io::Error::from_raw_os_error(libc::EACCES)),
        || panic!("a real open error is not an absent reader"),
        || true,
    )
    .expect_err("permanent errors are returned immediately");
    assert!(
        matches!(failed, FifoOpenError::Refused(error) if error.raw_os_error() == Some(libc::EACCES))
    );
    let failed = fifo_writer_with::<()>(
        || Err(io::Error::from_raw_os_error(libc::ENXIO)),
        || panic!("an expired wait does not wait again"),
        || true,
    )
    .expect_err("the last actual open error accompanies the timeout");
    assert!(
        matches!(failed, FifoOpenError::TimedOut(error) if error.raw_os_error() == Some(libc::ENXIO))
    );
}

fn release(gate: std::os::fd::OwnedFd) {
    use std::os::fd::AsRawFd as _;
    // SAFETY: a bounded write to the gate's write end, which this test owns.
    let wrote = unsafe { libc::write(gate.as_raw_fd(), b"go\n".as_ptr().cast(), 3) };
    assert_eq!(wrote, 3, "release the gate");
}

fn quiet(watch: &mut ProcWatch) -> Vec<ProcEvent> {
    let mut events = Vec::new();
    watch.drain(&mut events);
    events
}

fn script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn a_reroute_chain_is_a_fork_then_one_exec_per_link() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let gates = [mkfifo(dir.path(), "gate1"), mkfifo(dir.path(), "gate2")];
    let twin = script(
        dir.path(),
        "twin",
        &format!("read x < {}\nexec /bin/sleep 30", gates[1].display()),
    );
    let reroute = script(
        dir.path(),
        "reroute",
        &format!("read x < {}\nexec {}", gates[0].display(), twin.display()),
    );
    let mut zsh = Zsh::start();
    let mut watch = ProcWatch::new(zsh.pid()).expect("watch the shell");
    zsh.type_line(&reroute.display().to_string());

    let gate = open_writer(&mut zsh, &gates[0]);
    let heard = hear_until(&mut zsh, &mut watch, "the stub's exec", |heard| {
        forks(heard).iter().any(|c| first_exec(heard, *c).is_some())
    });
    let child = *forks(&heard)
        .iter()
        .find(|c| first_exec(&heard, **c).is_some())
        .expect("child");
    let order: Vec<&ProcEvent> = heard.iter().flat_map(|h| &h.events).collect();
    let fork_at = order
        .iter()
        .position(|e| **e == ProcEvent::Fork { child })
        .expect("fork");
    let exec_at = order
        .iter()
        .position(|e| matches!(e, ProcEvent::Exec { pid, .. } if *pid == child))
        .expect("exec");
    assert!(fork_at < exec_at, "the fork is heard first: {order:?}");
    let (at_exec, pgid) = first_exec(&heard, child).expect("exec");
    assert_eq!(pgid, child, "a job leads its own group");
    assert_eq!(
        at_exec.fg, child,
        "at the first exec tcgetpgrp names the child"
    );
    let cutter = FgCutter::new(zsh.pid());
    assert_eq!(
        cutter.proc_kind(&ProcSeen {
            pid: child,
            pgid,
            fg: at_exec.fg
        }),
        Some(EdgeKind::Group),
        "the gather would cut a Group edge there"
    );
    assert_eq!(execs_of(&heard, child), [true], "the stub: one exec, late");
    assert_eq!(quiet(&mut watch), [], "nothing more while the stub waits");

    release(gate);
    let gate = open_writer(&mut zsh, &gates[1]);
    let heard = hear_until(&mut zsh, &mut watch, "the twin's exec", |heard| {
        !execs_of(heard, child).is_empty()
    });
    assert_eq!(execs_of(&heard, child), [true], "the twin: one exec, late");
    assert_eq!(quiet(&mut watch), [], "nothing more while the twin waits");

    release(gate);
    let heard = hear_until(&mut zsh, &mut watch, "sleep's exec", |heard| {
        !execs_of(heard, child).is_empty()
    });
    assert_eq!(execs_of(&heard, child), [false], "sleep: one exec, fresh");
    assert!(
        aterm_sysprobe::process_path(u32::try_from(child).expect("pid"))
            .is_some_and(|p| p.ends_with("sleep")),
        "the chain ends in sleep"
    );
    assert!(short_info(child).expect("lives").execed);
}

#[test]
fn a_subshell_that_never_execs_is_no_exec() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = mkfifo(dir.path(), "gate");
    let mut zsh = Zsh::start();
    let mut watch = ProcWatch::new(zsh.pid()).expect("watch the shell");
    zsh.type_line(&format!("( read x < {} )", fifo.display()));
    let gate = open_writer(&mut zsh, &fifo);
    let heard = hear_until(&mut zsh, &mut watch, "the subshell's fork", |heard| {
        !forks(heard).is_empty()
    });
    let mut events = Vec::new();
    watch.drain(&mut events);
    let all: Vec<&ProcEvent> = heard
        .iter()
        .flat_map(|h| &h.events)
        .chain(&events)
        .collect();
    let child = forks(&heard)[0];
    assert!(
        !all.iter()
            .any(|e| matches!(e, ProcEvent::Exec { pid, .. } if *pid == child)),
        "no exec of the subshell: {all:?}"
    );
    assert!(!short_info(child).expect("lives").execed);
    assert_eq!(zsh.fg(), child, "the subshell holds the terminal");
    release(gate);
}

#[test]
fn a_command_substitution_at_the_prompt_is_no_group_edge() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = mkfifo(dir.path(), "gate");
    let mut zsh = Zsh::start();
    let mut watch = ProcWatch::new(zsh.pid()).expect("watch the shell");
    zsh.type_line(&format!("x=$(/bin/cat {})", fifo.display()));
    let gate = open_writer(&mut zsh, &fifo);
    let heard = hear_until(&mut zsh, &mut watch, "the substitution's exec", |heard| {
        forks(heard).iter().any(|c| first_exec(heard, *c).is_some())
    });
    let child = *forks(&heard)
        .iter()
        .find(|c| first_exec(&heard, **c).is_some())
        .expect("child");
    let (at_exec, pgid) = first_exec(&heard, child).expect("exec");
    assert_eq!(
        pgid,
        zsh.pid(),
        "the substitution runs in the shell's group"
    );
    assert_eq!(at_exec.fg, zsh.pid(), "the shell keeps the terminal");
    let cutter = FgCutter::new(zsh.pid());
    assert_eq!(
        cutter.proc_kind(&ProcSeen {
            pid: child,
            pgid,
            fg: at_exec.fg
        }),
        None,
        "no edge"
    );
    drop(gate);
}
