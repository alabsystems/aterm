// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: run the calling `#[test]` ALONE, in a child of the test binary —
//! or a NAMED helper test as a stand-in process ([`StandIn`]).
//!
//! Some tests cannot share the lib-test process: one that changes something
//! process-wide (the cwd, `RLIMIT_NOFILE`, the appearance preferences, the
//! config home), or one whose subject a sibling's fork disturbs — a child any
//! thread forks holds a copy of every descriptor until it execs, so a socket
//! this test closed stays open in it (`control_retire_tests`). Such a test
//! starts with `if !test_alone::alone() { return; }`: in the lib-test process
//! [`alone`] runs `<test binary> --exact <this test> --test-threads=1` and
//! answers `false`; in that child it answers `true`, and the body runs there.
//!
//! The child must have run EXACTLY that test and passed it. An exit status is
//! not that: `--exact` with a name that selects nothing runs zero tests and
//! exits 0, so the hand-written copies this replaces passed vacuously the
//! moment a test or its module was renamed. The name is the harness's own —
//! the thread libtest runs the test on is named after it — so it cannot go
//! stale, and the child's `1 passed` is required all the same.
//!
//! A STAND-IN is the same child for a test that needs a second process of its
//! own — a successor that dials, a racer, a parent that dies, a live process
//! whose table is read — with that process's role in the caller's own
//! environment and arguments ([`StandIn::command`]). It runs a helper test the
//! caller names ([`StandIn::helper`]: the module by `module_path!()`, so only
//! the function's name is written by hand) or the calling test itself
//! ([`StandIn::this_test`]), and the caller judges its end by the same rule:
//! [`Running::passed`] requires exactly `1 passed`, [`Running::selected`] —
//! for a helper whose own exit is the subject — that libtest ran exactly that
//! one test. A renamed helper fails there, by name, instead of running nothing
//! and passing.
//!
//! The child is spawned with no `pre_exec` (std takes its `posix_spawn` path),
//! writes into files rather than pipes, and is reaped by a bounded wait on its
//! exit: a pipe's end of file waits for every copy of its write end, and a
//! child a sibling test forked in the instant the pipe existed holds one until
//! it execs — seconds for a child demoted to the background band before its
//! exec.
//!
//! A stand-in that needs a DESCRIPTOR of its starter's is given it by [`hand`]
//! (whose hook makes that one spawn a fork) and takes it by [`handed`], which
//! checks what arrived: a descriptor number in an environment variable is
//! only a number, and which descriptors a `Command` child holds depends on
//! the std that spawned it.

use std::ffi::OsStr;
#[cfg(unix)]
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

/// Names the test the child was started for. Set only on that child, so a
/// child of the child (a stand-in it spawns, another test run alone) never
/// reads as it unless it runs the very same test.
const ALONE: &str = "ATERM_TEST_ALONE";

/// How long the child may run before it is named wedged and SIGKILLed: a hang
/// detector, never a latency budget. Five of [`aterm_spec::wait::MUST_HAPPEN`],
/// because each wait inside the child is a minute of its own, and a child that
/// fails should fail with its own message, not this one.
const CHILD_HANG: Duration = Duration::from_secs(5 * 60);

/// The calling test's name, as libtest gives it to `--exact`.
fn current_test() -> String {
    std::thread::current()
        .name()
        .expect("libtest runs each test on a thread named after it")
        .to_string()
}

/// Whether this process is the child [`alone`] started for the calling test.
pub(crate) fn in_child() -> bool {
    std::env::var_os(ALONE).is_some_and(|test| {
        std::thread::current()
            .name()
            .is_some_and(|name| test == OsStr::new(name))
    })
}

/// `true` in the child, which runs the test's body; `false` in the lib-test
/// process, once the child ran exactly the calling test and passed it.
///
/// # Panics
///
/// In the lib-test process, when the child failed, ran no test of the
/// caller's name, or was still running after [`CHILD_HANG`].
#[track_caller]
pub(crate) fn alone() -> bool {
    if in_child() {
        return true;
    }
    run_alone_with(&[]);
    false
}

/// [`alone`]'s lib-test half with `env` added to the child's environment —
/// for a caller that prepares something the child reads (a scratch config
/// home) and so tests [`in_child`] itself.
///
/// # Panics
///
/// As [`alone`].
#[track_caller]
pub(crate) fn run_alone_with(env: &[(&str, &OsStr)]) {
    let test = current_test();
    let mut child = StandIn::new(&test, "alone in its own process");
    child.command().env(ALONE, &test);
    for (key, value) in env {
        child.command().env(key, value);
    }
    child.spawn().passed();
}

/// A helper test of this binary, about to run as a process of its own: the
/// test binary with `--exact <helper> --test-threads=1`, its output in files,
/// and whatever the caller adds ([`Self::command`]). Started by
/// [`Self::spawn`], judged by the [`Running`] it answers.
pub(crate) struct StandIn {
    test: String,
    how: &'static str,
    command: Command,
    output: Output,
}

impl StandIn {
    /// The helper test `name` of `module` — the caller's `module_path!()`,
    /// so a renamed module cannot leave the name stale.
    #[cfg(unix)]
    pub(crate) fn helper(module: &str, name: &str) -> Self {
        let module = module.split_once("::").map_or("", |(_, rest)| rest);
        let test = if module.is_empty() {
            name.to_string()
        } else {
            format!("{module}::{name}")
        };
        Self::new(&test, "as a stand-in")
    }

    /// The calling test itself, by the harness's own name — for a test that
    /// re-runs itself with its stand-in's role in the environment.
    pub(crate) fn this_test() -> Self {
        Self::new(&current_test(), "as a stand-in")
    }

    fn new(test: &str, how: &'static str) -> Self {
        let output = Output::new();
        // std's Command: callers hand a stand-in descriptors (a PTY slave and its
        // `pre_exec`, fds named in its environment: [`hand`]) and shipped
        // `&mut Command` helpers.
        let mut command = Command::new(std::env::current_exe().expect("the test binary"));
        command
            .args([
                "--exact",
                test,
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdin(Stdio::null())
            .stdout(output.file("stdout"))
            .stderr(output.file("stderr"));
        Self {
            test: test.to_string(),
            how,
            command,
            output,
        }
    }

    /// The command, for the stand-in's own environment and arguments. Its
    /// program, its stdio and its `--exact` name are this module's. A
    /// descriptor for the stand-in goes through [`hand`], never through a
    /// number written into its environment by hand.
    pub(crate) fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Start it.
    ///
    /// # Panics
    ///
    /// When the test binary cannot be spawned.
    pub(crate) fn spawn(mut self) -> Running {
        let child = self
            .command
            .spawn()
            .unwrap_or_else(|e| panic!("spawn `{}` {}: {e}", self.test, self.how));
        Running {
            test: self.test,
            how: self.how,
            child,
            status: None,
            detached: false,
            output: self.output,
        }
    }
}

/// The variable beside `var` that says what the descriptor named in `var` IS.
#[cfg(unix)]
fn is_var(var: &str) -> String {
    format!("{var}_IS")
}

/// What this process holds at `fd`, as the kernel names it: `fstat`'s file
/// type, device, inode and (for a device) device number, with the
/// descriptor's access mode. Every process that holds that open object reads
/// the same answer. MEASURED on Darwin 25.6.0 (2026-10-13): a pipe end, a
/// unix-socket end, a PTY end and a file each keep their answer across `fork`
/// and `exec`, and the two ends of one pipe or socket pair give two. (Linux
/// gives a pipe's two ends one inode, by its source — not measured — and the
/// access mode then tells them apart.) `Err` says why there is no answer.
#[cfg(unix)]
fn descriptor(fd: RawFd) -> Result<String, String> {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes one `struct stat` through the pointer and reads
    // nothing else; a number that names nothing is `EBADF`.
    if unsafe { libc::fstat(fd, info.as_mut_ptr()) } != 0 {
        return Err(format!("not open ({})", std::io::Error::last_os_error()));
    }
    // SAFETY: `fstat` returned 0, so it initialized the structure.
    let info = unsafe { info.assume_init() };
    let kind = match info.st_mode & libc::S_IFMT {
        libc::S_IFIFO => "pipe",
        libc::S_IFSOCK => "socket",
        libc::S_IFCHR => "character device",
        libc::S_IFREG => "file",
        libc::S_IFDIR => "directory",
        _ => "other",
    };
    // SAFETY: `F_GETFL` reads the status flags of a descriptor found open above.
    let access = match unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE {
        libc::O_RDONLY => "read-only",
        libc::O_WRONLY => "write-only",
        _ => "read-write",
    };
    Ok(format!(
        "{kind} {}:{} (device number {}), {access}",
        info.st_dev, info.st_ino, info.st_rdev
    ))
}

/// HAND A CHILD A DESCRIPTOR, SO THAT IT CAN CHECK WHAT ARRIVED. `fd` reaches
/// the child `command` starts — a stand-in's ([`StandIn::command`]) or one
/// that replaces this process (`exec`) — at its own number, which the child
/// finds in `var` and takes with [`handed`]; `fd` itself may stay
/// close-on-exec here, as it should.
///
/// The hand-over is a `pre_exec` hook that clears close-on-exec on exactly
/// `fd`, in the child: the one spelling that works under every std. A std
/// with the SP-1 strip (Trust seals from `62f89f11` on) gives a `Command`
/// child its stdio and nothing else, whatever flags this process's
/// descriptors carry — a hook sends `Command` down its fork path, where that
/// std marks the child's table close-on-exec BEFORE the hooks run — and a std
/// without it marks nothing, so the clear is all a flagged descriptor needs
/// there. (`CommandExt::inherit_fd` exists only with SP-1 and is unstable
/// there, feature `trust_process_inherit_fds`.)
///
/// ONLY FROM A PROCESS THAT RUNS ONE TEST. The hook makes the spawn a `fork`,
/// and a fork of the whole lib-test process holds a copy of every sibling
/// test's descriptors until it execs (the module docs). So the caller is a
/// stand-in, or a test behind [`alone`], or it `exec`s, which forks nothing.
///
/// WHY THE CHILD CHECKS. Until 2026-10-13 the one fixture that handed a
/// stand-in descriptors (`seamless::tests::commit_parent_death_helper`)
/// cleared `FD_CLOEXEC` in the parent, spawned a bare `Command` and wrote the
/// numbers into its environment. Under an SP-1 std nothing arrived: the
/// stand-in wrapped two numbers that were not open, the production code it
/// armed on them stopped at once for a reason the test was not about, and
/// the test passed on that exit — it could not fail for the regression in its
/// name (measured: a mutant of that regression passed 5 runs of 5). So the
/// number travels with what the descriptor IS ([`descriptor`]), and the child
/// refuses anything else by name.
///
/// # Panics
///
/// When `fd` is not open in this process.
#[cfg(unix)]
#[track_caller]
pub(crate) fn hand(command: &mut Command, var: &str, fd: RawFd) {
    use std::os::unix::process::CommandExt as _;
    let named = descriptor(fd).unwrap_or_else(|why| panic!("hand over descriptor {fd}: {why}"));
    command.env(var, fd.to_string()).env(is_var(var), named);
    // SAFETY: the hook runs in the forked child before its exec (in this
    // process itself for an `exec`) and makes only async-signal-safe calls
    // (`fcntl`) on one integer copied into it; `last_os_error` reads `errno`
    // and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The descriptor this process was handed under `var` ([`hand`]), now its
/// own: found at its number and found to be the very descriptor its starter
/// named, or the process stops here and says which it holds instead.
///
/// Taken ONCE, and close-on-exec from then on, as the product's intake leaves
/// a descriptor it was handed (`seamless::take_commit_fd_from`): a child this
/// process starts later holds it only if it is handed on. The flag is also
/// how a second take is refused, which would make two owners of one number —
/// a descriptor crosses an `exec` only with the flag clear, so one that
/// carries it was taken here already.
///
/// # Panics
///
/// When `var` is not [`hand`]'s, or the number holds anything but that
/// descriptor: THE HAND-OVER FAILED, and whatever the caller meant to do on
/// the descriptor would have been done on something else. And on a second
/// take.
#[cfg(unix)]
#[track_caller]
pub(crate) fn handed(var: &str) -> OwnedFd {
    let fd = std::env::var(var)
        .ok()
        .and_then(|number| number.parse::<RawFd>().ok())
        .unwrap_or_else(|| panic!("`{var}` names no descriptor number"));
    let named = std::env::var(is_var(var))
        .unwrap_or_else(|_| panic!("`{var}` was not set by `test_alone::hand`"));
    let held = descriptor(fd);
    let (Ok(found) | Err(found)) = &held;
    assert!(
        held.as_ref() == Ok(&named),
        "THE HAND-OVER FAILED: this process was handed descriptor {fd} as `{var}` ({named}), \
         and it holds there: {found}. Its starter hands descriptors over with \
         `test_alone::hand`; a number in the environment alone hands over nothing"
    );
    // SAFETY: `F_GETFD` reads the flags of a descriptor found open above.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0 && flags & libc::FD_CLOEXEC == 0,
        "`{var}`: descriptor {fd} is close-on-exec in this process, so it was taken here \
         already. A handed descriptor is taken once: its one owner is the first take's answer"
    );
    // SAFETY: `F_SETFD` sets the flags of that descriptor, and nothing else.
    let armed = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    assert_eq!(armed, 0, "descriptor {fd} did not take close-on-exec");
    // SAFETY: its starter handed the descriptor to this process alone, it
    // was just found open at its number, and no earlier take owns it.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Where a stand-in's output goes: two files in a scratch directory of its
/// own, read once it exited.
struct Output {
    scratch: aterm_tempfile::TempDir,
}

impl Output {
    fn new() -> Self {
        Self {
            scratch: aterm_tempfile::TempDir::new().expect("a scratch dir for the child's output"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    fn file(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.path(name)).expect("a file for the child's output")
    }

    fn read(&self, name: &str) -> String {
        String::from_utf8_lossy(&std::fs::read(self.path(name)).unwrap_or_default()).into_owned()
    }
}

/// A started stand-in. Dropped unreaped, it is SIGKILLed and reaped, so a
/// test that fails midway leaves no process behind ([`Self::detach`] is the
/// one way to leave it running).
pub(crate) struct Running {
    test: String,
    how: &'static str,
    child: Child,
    status: Option<ExitStatus>,
    detached: bool,
    output: Output,
}

impl Running {
    /// Its process id.
    #[cfg(unix)]
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    /// Whether it has exited (reaped here, so its pid is not reused under a
    /// caller still holding it). Never blocks.
    pub(crate) fn exited(&mut self) -> bool {
        if self.status.is_none() {
            self.status = self.child.try_wait().expect("the child's exit");
        }
        self.status.is_some()
    }

    /// Wait for its exit — a hang detector of [`CHILD_HANG`], past which it
    /// is SIGKILLed and reads as wedged — and answer what it did.
    pub(crate) fn wait(mut self) -> Ran {
        self.finish()
    }

    /// [`Self::wait`], for a stand-in still held by reference.
    fn finish(&mut self) -> Ran {
        if self.status.is_none() {
            let child = &mut self.child;
            self.status = aterm_spec::wait::within(CHILD_HANG, Duration::from_millis(10), || {
                child.try_wait().expect("the child's exit")
            });
        }
        let status = self.status;
        if status.is_none() {
            // Not reaped yet (`try_wait` said so), so the pid is still the
            // child's. Reaped here; `Drop` then has nothing left to do.
            let _ = self.child.kill();
            self.status = self.child.wait().ok();
        }
        Ran {
            test: self.test.clone(),
            how: self.how,
            status,
            stdout: self.output.read("stdout"),
            stderr: self.output.read("stderr"),
        }
    }

    /// Wait for its exit, and require that it ran exactly its one test and
    /// passed it.
    ///
    /// # Panics
    ///
    /// When it failed, ran no test of that name, or was wedged.
    #[track_caller]
    pub(crate) fn passed(self) -> Ran {
        let ran = self.wait();
        if let Err(why) = ran.passed_one() {
            panic!("{why}");
        }
        ran
    }

    /// Wait for its exit, and require only that libtest selected exactly its
    /// one test — for a helper whose own way out (a status, a signal, an exit
    /// before libtest's summary) is the caller's subject.
    ///
    /// # Panics
    ///
    /// When it ran no test of that name, or was wedged.
    #[track_caller]
    #[cfg(unix)]
    pub(crate) fn selected(self) -> Ran {
        let ran = self.wait();
        if let Err(why) = ran.selected_one() {
            panic!("{why}");
        }
        ran
    }

    /// Leave it running past this handle: a stand-in that must outlive the
    /// process that spawned it (its parent's death is the subject).
    #[cfg(unix)]
    pub(crate) fn detach(mut self) {
        self.detached = true;
    }
}

/// Wait until `ready` — a hang detector of [`aterm_spec::wait::MUST_HAPPEN`]
/// — or until one of `stand_ins` ends first, which is then judged by its own
/// run: it failed, or ran no test of its name (a renamed helper), and its
/// output says which. One that passed before `ready` never readied it.
///
/// # Panics
///
/// When a stand-in ended first, or `ready` never held.
#[track_caller]
pub(crate) fn until_or_ended(
    what: &str,
    stand_ins: &mut [Running],
    mut ready: impl FnMut() -> bool,
) {
    let seen = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || {
            if ready() {
                return Some(None);
            }
            stand_ins.iter_mut().position(Running::exited).map(Some)
        },
    );
    match seen {
        Some(None) => {}
        Some(Some(ended)) => {
            let ran = stand_ins[ended].finish();
            match ran.passed_one() {
                Err(why) => panic!("{why}\n(it ended before: {what})"),
                Ok(()) => panic!(
                    "`{}`, {}, passed without: {what}\n{}",
                    ran.test,
                    ran.how,
                    ran.output()
                ),
            }
        }
        None => panic!("not within {:?}: {what}", aterm_spec::wait::MUST_HAPPEN),
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.detached || self.status.is_some() {
            return;
        }
        // `Child::kill` is SIGKILL (AGENTS.md); unreaped, the pid is
        // still the child's.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a stand-in did: its exit (`None`: killed as wedged) and its output.
pub(crate) struct Ran {
    test: String,
    how: &'static str,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Ran {
    fn output(&self) -> String {
        format!("stdout:\n{}\nstderr:\n{}", self.stdout, self.stderr)
    }

    /// `Ok` when it exited 0 having run exactly its test and passed it — the
    /// verdict [`Running::passed`] panics with, for a caller that judges
    /// several stand-ins before it fails.
    pub(crate) fn passed_one(&self) -> Result<(), String> {
        let (test, how) = (&self.test, self.how);
        let Some(status) = self.status else {
            return Err(self.wedged());
        };
        if !status.success() {
            return Err(format!(
                "`{test}`, {how}, failed ({status})\n{}",
                self.output()
            ));
        }
        let one_passed = self
            .stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;"));
        if !one_passed {
            return Err(self.ran_none());
        }
        Ok(())
    }

    /// `Ok` when libtest selected exactly its test, however it ended.
    #[cfg(unix)]
    fn selected_one(&self) -> Result<(), String> {
        if self.status.is_none() {
            return Err(self.wedged());
        }
        if !self.stdout.lines().any(|line| line == "running 1 test") {
            return Err(self.ran_none());
        }
        Ok(())
    }

    fn wedged(&self) -> String {
        format!(
            "`{}`, {}, still ran after {CHILD_HANG:?} (SIGKILLed)\n{}",
            self.test,
            self.how,
            self.output()
        )
    }

    fn ran_none(&self) -> String {
        format!(
            "`{}`, {}, did not run that one test — a name that selects nothing runs \
             nothing and exits 0\n{}",
            self.test,
            self.how,
            self.output()
        )
    }
}
