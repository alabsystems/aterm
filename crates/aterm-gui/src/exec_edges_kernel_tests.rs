// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel `ProcWatch` tests over an interactive `zsh` pseudo-terminal.

use std::time::{Duration, Instant};

use aterm_uds::process::child_pids;
use aterm_uds::procwatch::{ProcEvent, ProcWatch, short_info};

use crate::foreground_handback::{FgCutter, ProcSeen};
use crate::holder_image::EdgeKind;

/// A wait for something that must happen: a hang detector, never a latency budget.
const MUST_HAPPEN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// An interactive zsh with no startup files on its own PTY.
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

    /// Read what the shell wrote so far (the tty never fills).
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

    fn type_line(&self, line: &str) {
        aterm_pty::write_all(self.shell.master, format!("{line}\r").as_bytes());
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

/// One drain heard: its events and `tcgetpgrp` read right after it.
struct Heard {
    events: Vec<ProcEvent>,
    fg: i32,
}

/// Drain `watch` (and the shell's output) until `done` holds over every
/// drain so far, parking on the watch between drains.
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

/// The first drain that heard an `exec` of `pid`, and that event's group.
fn first_exec(heard: &[Heard], pid: i32) -> Option<(&Heard, i32)> {
    heard.iter().find_map(|h| {
        h.events.iter().find_map(|e| match e {
            ProcEvent::Exec { pid: p, pgid, .. } if *p == pid => Some((h, *pgid)),
            _ => None,
        })
    })
}

/// The late flag of each `exec` of `pid` heard, in order.
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

/// A fifo named `name` in `dir`.
fn mkfifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let c_path = std::ffi::CString::new(path.display().to_string()).expect("path");
    // SAFETY: a NUL-terminated path in this test's scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    path
}

/// The write end of `fifo`, once a reader has it open (a nonblocking
/// writer's open finds one): the reader is past every `exec` before its open.
fn open_writer(zsh: &mut Zsh, fifo: &std::path::Path) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    let c_path = std::ffi::CString::new(fifo.display().to_string()).expect("path");
    let start = Instant::now();
    loop {
        // SAFETY: as above.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd >= 0 {
            // SAFETY: a descriptor this test just opened and now owns.
            return unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        }
        assert!(
            start.elapsed() < MUST_HAPPEN,
            "hang: nothing ever read {}",
            fifo.display()
        );
        zsh.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Let the reader held at `gate` go on.
fn release(gate: std::os::fd::OwnedFd) {
    use std::os::fd::AsRawFd as _;
    // SAFETY: a bounded write to the gate's write end, which this test owns.
    let wrote = unsafe { libc::write(gate.as_raw_fd(), b"go\n".as_ptr().cast(), 3) };
    assert_eq!(wrote, 3, "release the gate");
}

/// What `watch` holds now, drained.
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

/// A reroute stub `exec`ing a twin `exec`ing `/bin/sleep`, typed at the
/// prompt, each link held at a gate until the one before was heard: the
/// shell's fork is heard first, and at the stub's `exec` the child already
/// holds the terminal, so it is a Group edge from the shell. Then each link is
/// exactly one `exec` event of the child. The stub was found exec'd by the
/// fork's own read (it was past its `exec` when the fork was drained), so it
/// is reported late, and so is the twin's, the child's first kernel `exec`,
/// which may restate it; `sleep`'s is not.
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

/// Control: a subshell that runs only builtins is a fork the watch hears and
/// never an `exec`, though it takes the terminal.
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

/// Control: a command substitution at the prompt forks a child that `exec`s
/// in the shell's own group while the shell keeps the terminal: heard, and no
/// edge of either kind. The child (`cat` on a gate) lives until the gate
/// closes, after the judging.
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
