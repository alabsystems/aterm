// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: run the calling `#[test]` alone in a child of the test binary, or
//! a named helper test as a stand-in process ([`StandIn`]).
//!
//! A test that changes something process-wide (the cwd, `RLIMIT_NOFILE`, the
//! appearance preferences, the config home), or whose subject a sibling's fork
//! disturbs (a forked child holds a copy of every descriptor until it execs,
//! `control_retire_tests`), starts with `if !test_alone::alone() { return; }`.
//! In the lib-test process [`alone`] runs `<test binary> --exact <this test>
//! --test-threads=1` and answers `false`; in that child it answers `true`.
//!
//! The child must have run exactly that test and passed it: `--exact` with a
//! name that selects nothing exits 0, so an exit status alone passes a renamed
//! test. The name is the harness's own (libtest names the test's thread after
//! it), and the child's `1 passed` is required.
//!
//! A stand-in is the same child for a test that needs a second process (a
//! successor that dials, a racer, a parent that dies), with its role in the
//! caller's environment and arguments ([`StandIn::command`]). It runs a named
//! helper test ([`StandIn::helper`]) or the calling test itself
//! ([`StandIn::this_test`]). [`Running::passed`] requires exactly `1 passed`;
//! [`Running::selected`], for a helper whose own exit is the subject, that
//! libtest ran exactly that one test.
//!
//! The child is spawned with no `pre_exec` (std takes its `posix_spawn` path),
//! writes into files rather than pipes, and is reaped by a bounded wait on its
//! exit: a pipe's end of file waits for every copy of its write end, and a
//! child a sibling test forked in the instant the pipe existed holds one until
//! it execs — seconds for a child demoted to the background band before its
//! exec.
//!
//! A stand-in that needs one of its starter's descriptors is given it by
//! [`hand`] and takes it by [`handed`], which checks what arrived.

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
/// so a child whose own one-minute wait fails fails with its own message.
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

/// [`alone`]'s lib-test half with `env` added to the child's environment, for
/// a caller that prepares something the child reads (a scratch config home)
/// and so tests [`in_child`] itself.
///
/// # Panics
///
/// As [`alone`].
#[track_caller]
pub(crate) fn run_alone_with(env: &[(&str, &OsStr)]) {
    spawn_alone_with(env).passed();
}

/// [`run_alone_with`]'s child, started and not yet waited for: for a caller
/// that must hold a lock while the child inherits this process's environment
/// (`seamless::tests::ENV_LOCK`) but not while the child runs.
///
/// # Panics
///
/// When the child cannot be started.
#[track_caller]
pub(crate) fn spawn_alone_with(env: &[(&str, &OsStr)]) -> Running {
    let test = current_test();
    let mut child = StandIn::new(&test, "alone in its own process");
    child.command().env(ALONE, &test);
    for (key, value) in env {
        child.command().env(key, value);
    }
    child.spawn()
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
        // std's Command: callers hand a stand-in descriptors (a PTY slave and
        // its `pre_exec`, [`hand`]) and use shipped `&mut Command` helpers.
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

/// The variable beside `var` that says what the descriptor named in `var` is.
#[cfg(unix)]
fn is_var(var: &str) -> String {
    format!("{var}_IS")
}

/// What this process holds at `fd`, as the kernel names it: `fstat`'s file
/// type, device, inode and (for a device) device number, plus the access mode.
/// Every process holding that open object reads the same answer: on Darwin a
/// pipe end, a unix-socket end, a PTY end and a file each keep theirs across
/// `fork` and `exec`, and the two ends of one pipe or socket pair differ (on
/// Linux a pipe's ends share an inode; the access mode tells them apart).
/// `Err` says why there is no answer.
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

/// Hand `fd` to the child `command` starts (a stand-in's, [`StandIn::command`],
/// or an `exec` replacing this process) at its own number, which the child
/// finds in `var` and takes with [`handed`]. `fd` may stay close-on-exec here.
///
/// The hand-over is a `pre_exec` hook that clears close-on-exec on exactly
/// `fd` in the child, the one spelling that works under every std: a std with
/// the SP-1 strip gives a `Command` child only its stdio (a hook sends it down
/// the fork path, which marks the child's table close-on-exec before the hooks
/// run), and a std without it marks nothing. (`CommandExt::inherit_fd` exists
/// only with SP-1, unstable, feature `trust_process_inherit_fds`.)
///
/// Only from a process that runs one test: the hook makes the spawn a fork,
/// and a fork of the lib-test process holds every sibling test's descriptors
/// until it execs. So the caller is a stand-in, a test behind [`alone`], or
/// an `exec`.
///
/// `var`'s companion carries what the descriptor is ([`descriptor`]), so the
/// child refuses anything else by name: under an SP-1 std a number alone names
/// nothing open, and a test armed on it passes for a reason it is not about.
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

/// The descriptor this process was handed under `var` ([`hand`]), now its own:
/// found at its number and found to be the descriptor its starter named, or
/// the process stops here saying what it holds instead.
///
/// Taken once, and close-on-exec from then on (as `seamless::take_commit_fd_from`
/// leaves a handed descriptor), so a later child holds it only if handed on.
/// The flag also refuses a second take: a descriptor crosses an `exec` only
/// with the flag clear, so one that carries it was taken here already.
///
/// # Panics
///
/// When `var` is not [`hand`]'s, when the number holds anything but that
/// descriptor (the hand-over failed), or on a second take.
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

    /// Wait for its exit (a hang detector of [`CHILD_HANG`], past which it is
    /// SIGKILLed and reads as wedged) and answer what it did.
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
    /// passed it. A child that ended COULD NOT RUN ends the caller so too,
    /// with the child's reason: its own libtest report, nested in a failure
    /// message, would read as a failure (`aterm-verify`'s `libtest`
    /// classifier splits blocks on the child's `failures:` lines).
    ///
    /// # Panics
    ///
    /// When it failed, ran no test of that name, or was wedged.
    #[track_caller]
    pub(crate) fn passed(self) -> Ran {
        let ran = self.wait();
        if let Err(why) = ran.passed_one() {
            if let Some(reason) = ran.could_not_run() {
                aterm_spec::host::could_not_run(format!("`{}`, {}: {reason}", ran.test, ran.how));
            }
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

/// Wait until `ready` (a hang detector of [`aterm_spec::wait::MUST_HAPPEN`])
/// or until one of `stand_ins` ends first, which is then judged by its own
/// run: it failed or ran no test of its name, and its output says which. One
/// that passed before `ready` never readied it.
///
/// An end is judged only after `ready` is asked once more: what a stand-in did
/// before its exit is there once its exit is seen, so one that readied and
/// ended between the two looks of a round (a quick child: its marker, its
/// calls, its exit) readied it. Judged on the first look alone, such a child
/// read as one that passed without readying: measured once in nine runs of the
/// whole lib beside the live spare suite at load ~100
/// (`native_document_host_tests::writerless_fifo_…`, `passed without: the
/// child reaches the FIFO calls`), and in 51 of 2400 runs of five tests that
/// wait so, beside the live suite at load 73-239; 0 of 2400 since.
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
            let ended = stand_ins.iter_mut().position(Running::exited)?;
            Some((!ready()).then_some(ended))
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
        // `Child::kill` is SIGKILL; unreaped, the pid is still the child's.
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

    /// `Ok` when it exited 0 having run exactly its test and passed it: the
    /// verdict [`Running::passed`] panics with, for a caller that judges several
    /// stand-ins before it fails.
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

    /// What the child's one test said when it ended COULD NOT RUN: the rest of
    /// its line that opens with [`aterm_spec::host::COULD_NOT_RUN`]. `None`
    /// for a pass, a wedge or any other failure.
    fn could_not_run(&self) -> Option<&str> {
        if self.status.is_none_or(|status| status.success()) {
            return None;
        }
        self.stdout
            .lines()
            .chain(self.stderr.lines())
            .find_map(|line| line.trim().strip_prefix(aterm_spec::host::COULD_NOT_RUN))
            .map(|rest| rest.trim_start_matches([' ', '—', ':']).trim())
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

#[cfg(all(test, unix))]
mod tests {
    use super::{Running, StandIn, until_or_ended};

    /// The stand-in role of [`a_stand_in_that_readied_then_ended_between_two_looks_readied`].
    const ROLE: &str = "ATERM_TEST_ALONE_READIED_THEN_ENDED";

    /// A stand-in that readied the caller and then ended, both between the
    /// wait's first look at `ready` and its look at the stand-ins, readied it:
    /// the wait asks `ready` again before it judges the end. The stand-in here
    /// has already ended and passed, and `ready` answers `false` to its first
    /// question only, which is that interleaving every time.
    /// NEGATIVE CONTROL: on the wait that judged the end on its first look,
    /// this panicked with `passed without: the second look`.
    #[test]
    fn a_stand_in_that_readied_then_ended_between_two_looks_readied() {
        if std::env::var_os(ROLE).is_some() {
            return;
        }
        let mut stand_in = StandIn::this_test();
        stand_in.command().env(ROLE, "1");
        let mut stand_ins = [stand_in.spawn()];
        let ended = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || stand_ins[0].exited().then_some(()),
        );
        assert!(ended.is_some(), "the stand-in never ended");
        let mut looks = 0;
        until_or_ended("the second look", &mut stand_ins, || {
            looks += 1;
            looks > 1
        });
        assert_eq!(looks, 2, "readied on the look after the end was seen");
        let [stand_in] = stand_ins;
        Running::passed(stand_in);
    }
}
