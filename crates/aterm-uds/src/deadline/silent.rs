// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds, never release ones): whether a macOS service a test needs answers, and the end of a test that needs one that does not.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(unix)]
use super::held_words;
use super::{Refused, TRUST};
use crate::host::could_not_run;

/// The one-minute hang detector for the native trust-service probe.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(60);

/// Whether macOS's `trustd` does not answer (asked once per process).
#[must_use]
pub fn trust_service_silent() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    *SILENT.get_or_init(|| {
        cfg!(target_vendor = "apple")
            && probe_silent_at(Instant::now(), Instant::now(), |within| {
                TRUST.run(within, within, probe)
            })
    })
}

fn probe_silent_at(
    started: Instant,
    now: Instant,
    run: impl FnOnce(Duration) -> Result<(), Refused>,
) -> bool {
    let Some(within) = (started + PROBE_DEADLINE)
        .checked_duration_since(now)
        .filter(|within| !within.is_zero())
    else {
        return true;
    };
    run(within) == Err(Refused::Unanswered)
}

/// End the calling test COULD NOT RUN when macOS's `trustd` does not answer ([`trust_service_silent`]): every code-signing and TLS check waits on it, so the silence is the machine's, not the code's. The words count this user's programs macOS's program check holds now ([`crate::process::held_programs`]).
#[track_caller]
pub fn require_trust_service() {
    if trust_service_silent() {
        could_not_run(trust_silent_words(crate::process::held_programs()));
    }
}

/// [`require_trust_service`]'s words, with a second clause counting the programs `held` names when there are any: a fact beside the silence, never its cause, since nothing measured ties the two.
fn trust_silent_words(held: Option<crate::process::HeldNow>) -> String {
    let mut words = String::from(
        "macOS's trust service (trustd) did not answer, so no code-signing or TLS check ran \
         here; rebooting the Mac clears a wedged trustd",
    );
    if let Some(n) = held.filter(|n| n.count > 0) {
        let (programs, wait) = if n.count == 1 {
            ("program", "waits")
        } else {
            ("programs", "wait")
        };
        words.push_str(&format!(
            "; {} {programs} of this user {wait} for macOS's program check now",
            n.count
        ));
    }
    words
}

/// End the calling test COULD NOT RUN when `out` is an `hdiutil` run that failed with `Operation timed out` (a wedged disk-image service, after about 121 s).
#[track_caller]
pub fn require_disk_image_service(out: &std::process::Output) {
    if timed_out(out) {
        could_not_run(
            "macOS's disk-image service (hdiutil) did not answer, so no disk-image check ran \
             here; rebooting the Mac clears a wedged disk-image service",
        );
    }
}

/// Whether `out` is a failed `hdiutil` run that timed out.
fn timed_out(out: &std::process::Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("Operation timed out")
}

/// How long a test's own `codesign` may run: a hang detector, never a budget.
pub const CODESIGN_HANG: Duration = Duration::from_secs(60);

/// Sign `path` ad hoc (`codesign --force --sign -`), as a test does to a copy of an Apple binary so the kernel lets it run off the system volume.
#[track_caller]
pub fn sign_ad_hoc(path: &std::path::Path) {
    let out = super::output_until(
        crate::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(path),
        std::time::Instant::now() + CODESIGN_HANG,
    )
    .unwrap_or_else(|e| panic!("codesign --force --sign - {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "codesign --force --sign - {}: {}: {}",
        path.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// The kept stand-in of the Apple binary `from`, signed ad hoc ([`sign_ad_hoc`]), for a test to exec IN PLACE: it runs off the system volume under its own identity (the kernel kills a copy that keeps Apple's signature, and hides a platform binary's environment from other processes).
#[must_use]
#[track_caller]
pub fn stand_in(from: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_vendor = "apple")]
    {
        signed_stand_in(&stand_in_cache(), from, &mut |fresh| {
            require_trust_service();
            sign_ad_hoc(fresh);
        })
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        from.to_path_buf()
    }
}

/// Copy the regular file `from` (no link) to `to`, made anew, with `from`'s permissions.
#[cfg(any(target_vendor = "apple", not(unix)))]
pub(crate) fn copy_regular_fresh(
    from: &std::path::Path,
    to: &std::path::Path,
) -> std::io::Result<u64> {
    let (mut reader, meta) = crate::regular_file::open(from)?;
    let _ = std::fs::remove_file(to);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(crate::atomic_file::LOCK_OPEN_FLAGS);
    }
    let mut writer = opts.open(to)?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(copied)
}

/// The line [`copy_regular_fresh`]'s `sh` runs off Apple: `$1` opened under `set -C`, readable
/// by this user alone (`umask 077`) until its permissions are set, holding its stdin. `set -C`
/// (POSIX's noclobber; dash, bash and BusyBox's `sh` alike) makes a new file, with `O_EXCL`,
/// where `$1` names nothing, and refuses a regular file, a link to one and a link to nothing; a
/// file of another type, or a link to one (a device, a FIFO), it opens as it is, with no
/// `O_EXCL` and no truncation.
#[cfg(all(unix, not(target_vendor = "apple")))]
const WRITES_ITS_INPUT: &str = "set -C; umask 077; exec cat >\"$1\"";

/// Copy the regular file `from` (no link) to `to`, made anew, with `from`'s permissions, the
/// bytes written by a child process: Linux refuses to exec a file anything holds open for
/// writing (`ETXTBSY`), and a child another thread forks while this process holds `to` open
/// keeps that descriptor until its own exec (a paused fork, as long as it stays paused). The
/// child reads `from` through this process's descriptor of it (its stdin), opens `to` after its
/// fork and has exited before this returns, so no process holds a writer of `to` once it is
/// made.
///
/// Whatever was at `to` is removed first, so the child's `set -C` opens a file there that it did
/// not make only when one is put there after that, by whoever may write `to`'s directory. The
/// permissions are then set through a descriptor this process opens to read on what is at `to`,
/// not following a link ([`crate::regular_file::open`]) and only when it is a regular file of
/// one name: a link, another type of file or a hard link put in its place is refused
/// (`InvalidData`) and its target's mode left as it was.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub(crate) fn copy_regular_fresh(
    from: &std::path::Path,
    to: &std::path::Path,
) -> std::io::Result<u64> {
    copy_regular_fresh_by(from, to, WRITES_ITS_INPUT)
}

/// [`copy_regular_fresh`], its child's `sh` running `line` ([`WRITES_ITS_INPUT`]).
#[cfg(all(unix, not(target_vendor = "apple")))]
fn copy_regular_fresh_by(
    from: &std::path::Path,
    to: &std::path::Path,
    line: &str,
) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let (reader, meta) = crate::regular_file::open(from)?;
    let _ = std::fs::remove_file(to);
    let status = crate::spawnfd::command("/bin/sh")
        .args(["-c", line, "sh"])
        .arg(to)
        .stdin(crate::spawnfd::Stdio::from(reader))
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "{}: the `sh` copying {} into it ended with {status}",
            to.display(),
            from.display()
        )));
    }
    let (made, at) = crate::regular_file::open(to)?;
    if at.nlink() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: not the file the copy made", to.display()),
        ));
    }
    made.set_permissions(meta.permissions())?;
    Ok(at.len())
}

/// Copy the Apple binary `from` to `to`, signed ad hoc: a plain copy of its [`stand_in`], for a
/// test that needs the executable at a path of its own (in a bundle, renamed over). On another
/// Unix, a copy of the file `from` resolves to, since the copy takes only a regular file and a
/// program there is often a link (Debian's and Ubuntu's `/bin/sh`, to dash), its bytes written
/// by a child process (`copy_regular_fresh`).
///
/// The copy runs under `to`'s name, so `from` must be a program that acts as itself under any
/// name. A multicall binary picks its tool by the name it runs under (`argv[0]`): a copy of
/// BusyBox, or of uutils' `coreutils`, is the tool `from` names only when `to`'s file name is
/// that tool's. Alpine's `/bin/cat` is a link to BusyBox, whose copy named `exe` says
/// `exe: applet not found` and copies nothing.
#[track_caller]
pub fn ad_hoc_copy(from: &std::path::Path, to: &std::path::Path) {
    #[cfg(not(all(unix, not(target_vendor = "apple"))))]
    let from = stand_in(from);
    #[cfg(all(unix, not(target_vendor = "apple")))]
    let from =
        std::fs::canonicalize(from).unwrap_or_else(|e| panic!("resolve {}: {e}", from.display()));
    copy_regular_fresh(&from, to)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), to.display()));
}

/// A directory holding programs a test copied and runs ([`ad_hoc_copy`], a copy of the test binary, a bundle around one), removed when dropped only once [`KeptUntilRan::ran`] said every child started from a file under it has shown it ran ([`prints`], [`cat_echoes`], a file the copy wrote).
#[derive(Debug)]
pub struct KeptUntilRan {
    dir: std::path::PathBuf,
    ran: bool,
}

impl KeptUntilRan {
    /// Take over removing `dir`: a directory the test made, or an `aterm_tempfile::TempDir`'s `keep()`.
    #[must_use]
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ran: false,
        }
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.dir
    }

    /// Every child started from a file under the directory has shown it ran: it goes when dropped.
    pub const fn ran(&mut self) {
        self.ran = true;
    }
}

impl Drop for KeptUntilRan {
    fn drop(&mut self) {
        if self.ran {
            let _ = std::fs::remove_dir_all(&self.dir);
            return;
        }
        // The copy may lie under this test binary's own root, which then outlives its exit.
        crate::test_binary::hold_own_root();
        let kept = format!(
            "KEPT {}: a program copied there was started and never showed it ran, so macOS may \
             still be assessing its first exec; remove it once no process runs from it",
            self.dir.display()
        );
        if std::thread::panicking() {
            eprintln!("{kept}");
        } else {
            panic!("{kept} (the test ended without calling `KeptUntilRan::ran`)");
        }
    }
}

/// How long [`cat_echoes`] waits for the echo: the one-minute hang detector, never a budget.
pub const ECHO_HANG: Duration = Duration::from_secs(60);

/// The proof [`ad_hoc_copy`] asks for before a copied executable goes: `child`, spawned from the copy with piped stdout, writes the line `line` (lines before it are skipped) within `hang` ([`ECHO_HANG`] for a copy of a small Apple binary, [`FIRST_EXEC_HANG`](super::FIRST_EXEC_HANG) for a copy of the test binary), so its exec's assessment is done and it runs from its file. A child macOS's program check still holds at `hang` ends the test COULD NOT RUN ([`held_words`]).
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
pub fn prints(child: &mut crate::spawnfd::Child, line: &str, hang: Duration) {
    prints_judged(child, line, hang, &|pid| {
        crate::process::at_program_check(i32::try_from(pid).unwrap_or(0))
    });
}

/// The longest `test <name> ... ` [`is_proof`] reads before a proof line, bytes.
#[cfg(unix)]
const LIBTEST_PREFIX_MAX: u64 = 1024;

/// Whether `got`, one line of a child's output without its newline, is the proof line `wanted`:
/// the line itself, or the line after libtest's `test <name> ... `. A test binary run again at one
/// test thread (`--test-threads=1`, or a `RUST_TEST_THREADS=1` it inherits) writes that prefix,
/// unended, before the test it runs prints anything, so the test's first line follows it on the
/// same line.
#[cfg(unix)]
fn is_proof(got: &[u8], wanted: &[u8]) -> bool {
    got == wanted
        || got.strip_suffix(wanted).is_some_and(|before| {
            before.starts_with(b"test ")
                && before.ends_with(b" ... ")
                && before.len() > b"test  ... ".len()
        })
}

/// [`prints`], with `held` asked whether the child is held at macOS's program check when `hang` runs out.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
fn prints_judged(
    child: &mut crate::spawnfd::Child,
    line: &str,
    hang: Duration,
    held: &dyn Fn(u32) -> Option<bool>,
) {
    use std::io::BufRead as _;
    use std::sync::mpsc::RecvTimeoutError;
    let Some(from) = child.stdout.take() else {
        refused(child, line, "spawn it with piped stdout");
    };
    let (tx, seen) = std::sync::mpsc::channel();
    let wanted = line.as_bytes().to_vec();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut from = std::io::BufReader::new(from);
        let limit = u64::try_from(wanted.len())
            .unwrap_or(u64::MAX)
            .saturating_add(2 + LIBTEST_PREFIX_MAX);
        let mut got = Vec::with_capacity(wanted.len().saturating_add(2));
        loop {
            got.clear();
            match (&mut from).take(limit).read_until(b'\n', &mut got) {
                Ok(0) | Err(_) => break,
                Ok(_) if !got.ends_with(b"\n") => loop {
                    match from.fill_buf() {
                        Ok([]) | Err(_) => return,
                        Ok(buf) => {
                            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                from.consume(pos + 1);
                                break;
                            }
                            let len = buf.len();
                            from.consume(len);
                        }
                    }
                },
                Ok(_) => {
                    if got
                        .strip_suffix(b"\n")
                        .is_some_and(|got| is_proof(got, &wanted))
                    {
                        let _ = tx.send(());
                        drop(tx);
                        drop(got);
                        let _ = std::io::copy(&mut from, &mut std::io::sink());
                        break;
                    }
                }
            }
        }
    });
    match seen.recv_timeout(hang) {
        Ok(()) => {}
        // How it ended says why: its own exit status, or a signal no test of its own sent.
        Err(RecvTimeoutError::Disconnected) => match super::exit_within(child, ECHO_HANG) {
            Some(status) => refused(child, line, &format!("its output ended first ({status})")),
            None => refused(child, line, "its output ended first, and it ran on"),
        },
        Err(RecvTimeoutError::Timeout) => match held_words(child.id(), hang, held(child.id())) {
            Some(words) => {
                end(child);
                could_not_run(words)
            }
            None => refused(child, line, &format!("not within {hang:?}")),
        },
    }
}

/// A test's run of `cmd` to its end, its output drained and its stdin `/dev/null`, within `hang`, the test's hang detector. That is [`ECHO_HANG`] for one thing that must happen, such as the test binary run again in place (`current_exe`, whose exec macOS assessed when the test binary started), and the run's own detectors added up past that when it waits in turn on several. A copy of the test binary at a new path is a first exec instead, which [`prints`] proves within [`FIRST_EXEC_HANG`](super::FIRST_EXEC_HANG) before its file goes. The run leads a process group of its own; at `hang` it is asked whether macOS's program check holds it, the group is killed, and the run is reaped within [`ECHO_HANG`] (one the kill does not end is handed to [`crate::reaper`]). A run the program check held ends the test COULD NOT RUN ([`held_words`]).
///
/// # Panics
/// When the run does not start, cannot be waited on, or, not held, outlives `hang`: the test's own failure, naming the run and what it wrote by its end.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
pub fn ends_within(cmd: &mut crate::spawnfd::Command, hang: Duration) -> std::process::Output {
    ends_within_by(
        cmd,
        hang,
        &Overrun {
            held: &program_check,
            kill: super::children::kill_group,
            reap: ECHO_HANG,
        },
    )
}

/// [`ends_within`] for a test's own `std` launch, one a `pre_exec` step or the signal state it measures keeps off [`crate::spawnfd`]. Past `hang` only the child is killed: it is in the process group `std` left it in, often the test binary's own.
///
/// # Panics
/// As [`ends_within`]'s.
#[cfg(all(unix, test))]
#[track_caller]
pub(crate) fn std_ends_within(
    cmd: &mut std::process::Command,
    hang: Duration,
) -> std::process::Output {
    std_ends_within_by(
        cmd,
        hang,
        &Overrun {
            held: &program_check,
            kill: kill_child,
            reap: ECHO_HANG,
        },
    )
}

/// [`std_ends_within`], with `how` to end a run past `hang`.
#[cfg(all(unix, test))]
#[track_caller]
fn std_ends_within_by(
    cmd: &mut std::process::Command,
    hang: Duration,
    how: &Overrun<'_, std::process::Child>,
) -> std::process::Output {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{program} {}", super::RunError::Spawn(e)));
    let pipes = (child.stdout.take(), child.stderr.take());
    judged(&program, child, pipes, None, hang, how)
}

/// `SIGKILL` `child` alone, until it is reaped (`std` signals nothing after).
#[cfg(all(unix, test))]
fn kill_child(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// [`crate::process::at_program_check`] for a child's pid.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
fn program_check(pid: u32) -> Option<bool> {
    crate::process::at_program_check(i32::try_from(pid).unwrap_or(0))
}

/// How a test's wait ends a run past its hang detector: `held` is asked whether macOS's program
/// check holds the run's pid while it still runs, `kill` signals it, and its reap is waited on
/// within `reap`.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
struct Overrun<'a, C> {
    held: &'a dyn Fn(u32) -> Option<bool>,
    kill: fn(&mut C),
    reap: Duration,
}

/// [`ends_within`], with `how` to end a run past `hang`.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
fn ends_within_by(
    cmd: &mut crate::spawnfd::Command,
    hang: Duration,
    how: &Overrun<'_, crate::spawnfd::Child>,
) -> std::process::Output {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let (child, pipes, group) = super::started(cmd, crate::spawnfd::Stdio::null())
        .unwrap_or_else(|e| panic!("{program} {e}"));
    judged(&program, child, pipes, group, hang, how)
}

/// `child`, `program`'s run, started with `pipes` and leading `group` when it leads one, waited on
/// to its end within `hang` and judged as [`ends_within`] says, `how` ending it past `hang`.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
fn judged<C: super::Waited>(
    program: &str,
    child: C,
    pipes: super::Pipes,
    group: Option<u32>,
    hang: Duration,
    how: &Overrun<'_, C>,
) -> std::process::Output {
    let by = super::saturating_after(std::time::Instant::now(), hang);
    let mut overran = None;
    let ended = super::drained_owned(
        child,
        pipes,
        group,
        &mut |_, _| by.saturating_duration_since(std::time::Instant::now()),
        &mut |child| {
            let pid = child.pid();
            overran = Some((pid, (how.held)(pid)));
            (how.kill)(child);
            super::reap_within(
                child,
                None,
                super::saturating_after(std::time::Instant::now(), how.reap),
            );
        },
        false,
        None,
    );
    match ended {
        Ok(Ok(out)) => out,
        Ok(Err(late)) => {
            let (pid, held) = overran.unwrap_or_default();
            if let Some(words) = held_words(pid, hang, held) {
                could_not_run(words);
            }
            let (stdout, stderr) = late.wrote();
            panic!(
                "{program} (pid {pid}) did not finish in time: not within its {hang:?} hang \
                 detector\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            )
        }
        Err(e) => panic!("{program} {e}"),
    }
}

/// `child` killed and reaped within the one-minute hang detector ([`super::kill_within`]), its file left.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
fn end(child: &mut crate::spawnfd::Child) {
    super::kill_within(child, ECHO_HANG);
}

/// [`prints`]' and [`cat_echoes`]' end without their proof: `child` [`end`]ed, the panic naming
/// the line and why.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
fn refused(child: &mut crate::spawnfd::Child, line: &str, why: &str) -> ! {
    end(child);
    panic!(
        "the copied program (pid {}) never wrote {line:?}: {why}",
        child.id()
    );
}

/// [`prints`] for a copy of `cat`: `child`, spawned with piped stdin and stdout, echoes a line written to it within [`ECHO_HANG`].
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
pub fn cat_echoes(child: &mut crate::spawnfd::Child) -> std::process::ChildStdin {
    use std::io::Write as _;
    let (Some(mut to), true) = (child.stdin.take(), child.stdout.is_some()) else {
        refused(child, "ran", "spawn it with piped stdin and stdout");
    };
    if let Err(e) = to.write_all(b"ran\n") {
        refused(child, "ran", &format!("write to it: {e}"));
    }
    prints(child, "ran", ECHO_HANG);
    to
}

/// Where [`stand_in`] keeps its signed stand-ins: the per-user temporary directory, which
/// no other user can write.
#[cfg(target_vendor = "apple")]
fn stand_in_cache() -> std::path::PathBuf {
    std::env::temp_dir().join("aterm-ad-hoc-stand-ins")
}

/// Where the stand-in of this version of `from` is kept in `cache`: a directory holds
/// `from`'s size, modification time and inode, while its executable name stays intact.
/// Programs such as Git choose their behavior from that executable name.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn kept_at(cache: &std::path::Path, from: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
    let name = from
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stand-in");
    cache
        .join("named")
        .join(format!(
            "{name}-{}-{}.{}-{}",
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ino()
        ))
        .join(name)
}

/// The copy of `from` in `cache` that `sign` signed ([`kept_at`]), made the first time this
/// version of `from` is asked for.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn signed_stand_in(
    cache: &std::path::Path,
    from: &std::path::Path,
    sign: &mut dyn FnMut(&std::path::Path),
) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static FRESH: AtomicU64 = AtomicU64::new(0);
    let signed = kept_at(cache, from);
    if signed.is_file() {
        return signed;
    }
    std::fs::create_dir_all(cache).unwrap_or_else(|e| panic!("create {}: {e}", cache.display()));
    let name = from
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stand-in");
    let fresh = cache.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        FRESH.fetch_add(1, Ordering::Relaxed)
    ));
    struct Fresh<'a>(&'a std::path::Path);
    impl Drop for Fresh<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _fresh = Fresh(&fresh);
    copy_regular_fresh(from, &fresh)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), fresh.display()));
    sign(&fresh);
    let parent = signed.parent().expect("stand-in has a version directory");
    std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
    match std::fs::hard_link(&fresh, &signed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => panic!("place {} at {}: {e}", fresh.display(), signed.display()),
    }
    signed
}

/// One certificate-chained code check; its verdict is discarded.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn probe() {
    use core::ffi::c_void;
    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: CFTypeRef,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> CFTypeRef;
    }
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecStaticCodeCreateWithPath(path: CFTypeRef, flags: u32, out: *mut CFTypeRef) -> i32;
        fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> i32;
    }

    const PATH: &[u8] = b"/usr/bin/true";
    // SAFETY: `PATH` is `len` live bytes, copied by the call.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            PATH.as_ptr(),
            PATH.len() as isize,
            0,
        )
    };
    if url.is_null() {
        return;
    }
    let mut code: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; an out-parameter for one new reference.
    let created = unsafe { SecStaticCodeCreateWithPath(url, 0, &mut code) };
    if created == 0 && !code.is_null() {
        // SAFETY: `code` is a live static code object; no requirement.
        let _ = unsafe { SecStaticCodeCheckValidity(code, 0, std::ptr::null()) };
        // SAFETY: we own exactly one reference.
        unsafe { CFRelease(code) };
    }
    // SAFETY: we own exactly one reference.
    unsafe { CFRelease(url) };
}

#[cfg(not(target_vendor = "apple"))]
fn probe() {}

#[cfg(all(test, unix))]
mod tests {
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_completed_native_trust_probe_is_not_silent() {
        super::require_trust_service();
        static GATE: super::super::Gate = super::super::Gate::new("native-probe-test");
        let now = std::time::Instant::now();
        assert!(!super::probe_silent_at(
            now - std::time::Duration::from_secs(31),
            now,
            |within| GATE.run(within, within, super::probe),
        ));
    }

    #[test]
    fn an_expired_probe_does_not_start_and_a_failed_probe_is_not_silence() {
        let now = std::time::Instant::now();
        assert!(super::probe_silent_at(
            now - std::time::Duration::from_secs(61),
            now,
            |_| panic!("an expired probe must not start"),
        ));
        assert!(!super::probe_silent_at(now, now, |_| {
            Err(super::Refused::Failed("native failure".to_owned()))
        }));
        assert!(super::probe_silent_at(now, now, |_| {
            Err(super::Refused::Unanswered)
        }));
    }
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{ExitStatus, Output};

    fn run(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Only a failed run that timed out is the service's silence.
    #[test]
    fn only_a_timed_out_failure_is_a_silent_disk_image_service() {
        use super::timed_out as silent;
        assert!(silent(&run(
            1,
            "hdiutil: create failed - Operation timed out\n"
        )));
        assert!(!silent(&run(
            1,
            "hdiutil: create failed - No space left on device\n"
        )));
        assert!(!silent(&run(1, "")));
        assert!(!silent(&run(0, "Operation timed out\n")));
    }

    /// A stand-in is signed once per version of its binary: a second ask for the same
    /// binary copies the signed one, and a changed binary is signed again.
    #[test]
    fn a_stand_in_is_signed_once_per_version_of_its_binary() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"version one").expect("the binary");
        let mut signed = Vec::new();
        let mut sign = |fresh: &std::path::Path| {
            signed.push(fresh.to_path_buf());
            let mut bytes = std::fs::read(fresh).expect("the fresh copy");
            bytes.extend_from_slice(b" signed");
            std::fs::write(fresh, bytes).expect("sign the fresh copy");
        };
        let first = super::signed_stand_in(&cache, &binary, &mut sign);
        let again = super::signed_stand_in(&cache, &binary, &mut sign);
        assert_eq!(
            first.file_name(),
            binary.file_name(),
            "argv[0] keeps the program's name"
        );
        assert_eq!(first, again, "one stand-in per version");
        assert_eq!(std::fs::read(&first).unwrap(), b"version one signed");
        std::fs::write(&binary, b"version two, longer").expect("the binary changes");
        let changed = super::signed_stand_in(&cache, &binary, &mut sign);
        assert_ne!(changed, first, "a new version is a new stand-in");
        assert_eq!(
            std::fs::read(&changed).unwrap(),
            b"version two, longer signed"
        );
        assert_eq!(signed.len(), 2, "signed once per version: {signed:?}");
        let left: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with('.'))
            .collect();
        assert!(left.is_empty(), "no unplaced copy left: {left:?}");
    }

    /// A stand-in once placed is never replaced: a caller that finds one placed while it
    /// signed its own (another test process's, which may be running it while macOS assesses
    /// it by path) keeps the placed one, file and all, and leaves no copy of its own behind.
    #[test]
    fn a_placed_stand_in_is_never_replaced() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"bytes").expect("the binary");
        let kept = super::kept_at(&cache, &binary);
        let mut placed = None;
        let got = super::signed_stand_in(&cache, &binary, &mut |fresh| {
            std::fs::write(fresh, b"ours").expect("sign ours");
            std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
            std::fs::write(&kept, b"theirs").expect("another process places its own");
            placed = Some(std::fs::metadata(&kept).expect("theirs").ino());
        });
        assert_eq!(got, kept);
        assert_eq!(
            std::fs::read(&kept).unwrap(),
            b"theirs",
            "the placed one stays"
        );
        assert_eq!(
            Some(std::fs::metadata(&kept).unwrap().ino()),
            placed,
            "the same file, not a replacement"
        );
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            1,
            "no copy of our own left"
        );
        let other = dir.path().join("other");
        std::fs::write(&other, b"other bytes").expect("another binary");
        let own = super::signed_stand_in(&cache, &other, &mut |fresh| {
            std::fs::write(fresh, b"ours").expect("sign ours");
        });
        assert_eq!(
            std::fs::read(&own).unwrap(),
            b"ours",
            "nothing placed: ours is"
        );
    }

    /// OFF APPLE A COPY'S MODE IS SET ONLY ON THE FILE ITS CHILD MADE. A link to a FIFO put at
    /// `to` after the copy cleared the name (here by the child's own line, ahead of its open) is
    /// opened by `set -C`, and the child writes into the FIFO; the copy then refuses `to`
    /// (`InvalidData`) and leaves the link, and the FIFO's mode, as they were. NEGATIVE CONTROL:
    /// the bytes reached the FIFO, so the child did write through the link; and a plain copy
    /// holds `from`'s bytes, with `from`'s mode.
    #[cfg(not(target_vendor = "apple"))]
    #[test]
    fn a_copys_mode_is_never_set_through_a_link() {
        use std::io::Read as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        const SOURCE: &[u8] = b"#!/bin/sh\nexit 0\n";
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mode = |path: &std::path::Path| {
            std::fs::symlink_metadata(path)
                .map(|m| m.mode() & 0o7777)
                .ok()
        };
        let from = dir.path().join("from");
        std::fs::write(&from, SOURCE).expect("the source");
        std::fs::set_permissions(&from, std::fs::Permissions::from_mode(0o750)).expect("chmod");
        let fifo = dir.path().join("fifo");
        let made = super::ends_within(
            crate::spawnfd::command("mkfifo")
                .args(["-m", "600"])
                .arg(&fifo),
            super::ECHO_HANG,
        );
        assert!(made.status.success(), "mkfifo: {made:?}");
        assert_eq!(mode(&fifo), Some(0o600));
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("the FIFO's reader");
        let to = dir.path().join("to");
        let steered = format!("ln -s fifo \"$1\"; {}", super::WRITES_ITS_INPUT);
        let refused = super::copy_regular_fresh_by(&from, &to, &steered);
        let mut got = Vec::new();
        reader.read_to_end(&mut got).expect("what the child wrote");
        assert_eq!(got, SOURCE, "the child wrote through the link");
        assert_eq!(mode(&fifo), Some(0o600), "the FIFO's mode was set");
        assert!(
            std::fs::symlink_metadata(&to).is_ok_and(|m| m.file_type().is_symlink()),
            "the link is left"
        );
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::InvalidData)
        );
        let plain = dir.path().join("plain");
        let copied = super::copy_regular_fresh(&from, &plain).ok();
        assert_eq!(copied, Some(SOURCE.len() as u64));
        assert_eq!(std::fs::read(&plain).ok().as_deref(), Some(SOURCE));
        assert_eq!(mode(&plain), Some(0o750));
    }

    /// Only an echo of its line is a `cat`'s proof that it runs: `cat` itself passes and runs
    /// on until its stdin closes.
    #[test]
    fn only_an_echo_proves_a_cat_runs() {
        use crate::spawnfd::{Stdio, command};
        let piped = |program: &str, args: &[&str]| {
            command(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn")
        };
        let mut cat = piped("/bin/cat", &[]);
        let stdin = super::cat_echoes(&mut cat);
        assert_eq!(cat.try_wait().expect("poll"), None, "it runs on");
        drop(stdin);
        assert!(cat.wait().expect("reap").success(), "and ends at its EOF");
        for (program, args) in [
            ("/bin/sh", &["-c", "read line; echo other"][..]),
            ("/usr/bin/true", &[][..]),
        ] {
            let mut child = piped(program, args);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::cat_echoes(&mut child)
            }));
            assert!(refused.is_err(), "{program} {args:?} is no proof");
            assert!(
                child.try_wait().expect("poll").is_some(),
                "{program}: killed and reaped"
            );
        }
        let mut unpiped = command("/bin/cat")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::cat_echoes(&mut unpiped)
        }));
        assert!(refused.is_err(), "no pipes, no proof");
        assert!(
            unpiped.try_wait().expect("poll").is_some(),
            "killed and reaped"
        );
    }

    /// A copied program whose output ends before its line is named with how it ended, its own exit
    /// status or the signal that ended it, so a failure says whether something else killed it.
    #[test]
    fn a_copied_program_that_ends_first_is_named_with_how_it_ended() {
        use crate::spawnfd::{Stdio, command};
        for (script, said) in [
            ("exit 3", "(exit status: 3)"),
            ("kill -9 $$", "(signal: 9 (SIGKILL))"),
        ] {
            let mut child = command("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn");
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::prints(&mut child, "parked", super::ECHO_HANG);
            }))
            .expect_err("no proof");
            let words = refused
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or_default();
            assert!(
                words.ends_with(&format!("its output ended first {said}")),
                "{script}: {words}"
            );
        }
    }

    /// Only the line itself is a copied program's proof that it runs: a child that writes other
    /// lines first and then the line passes, and its output after it is read on, so a child
    /// that writes a megabyte more still ends.
    #[test]
    fn only_its_line_proves_a_copied_program_runs() {
        use crate::spawnfd::{Stdio, command};
        let piped = |script: &str| {
            command("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn")
        };
        let mut child = piped("echo running 1 test; echo parked; head -c 1048576 /dev/zero");
        super::prints(&mut child, "parked", super::ECHO_HANG);
        let hang = std::time::Instant::now() + super::ECHO_HANG;
        while child.try_wait().expect("poll").is_none() {
            assert!(
                std::time::Instant::now() < hang,
                "its output after the line was never read"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for (script, hang) in [
            ("echo parked-not; echo other", super::ECHO_HANG),
            ("exec sleep 60", std::time::Duration::from_millis(200)),
        ] {
            let mut child = piped(script);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::prints(&mut child, "parked", hang);
            }));
            assert!(refused.is_err(), "{script} is no proof");
            assert!(
                child.try_wait().expect("poll").is_some(),
                "{script}: killed and reaped"
            );
        }
        let mut unpiped = command("/bin/sh")
            .args(["-c", "echo parked"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::prints(&mut unpiped, "parked", super::ECHO_HANG);
        }));
        assert!(refused.is_err(), "no pipe, no proof");
        assert!(
            unpiped.try_wait().expect("poll").is_some(),
            "killed and reaped"
        );
    }

    /// A directory of copies goes when dropped only once they ran: dropped by a failing test's
    /// unwind it stays, and dropped by a test that never said so it stays and fails that test.
    #[test]
    fn a_directory_of_copies_goes_only_once_they_ran() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let laid = |name: &str| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(dir.join("bin")).expect("the directory");
            std::fs::write(dir.join("bin").join("copy"), b"copied").expect("the copy");
            dir
        };
        let proven = laid("proven");
        let mut kept = super::KeptUntilRan::new(&proven);
        assert_eq!(kept.path(), proven);
        kept.ran();
        drop(kept);
        assert!(!proven.exists(), "its copies ran: it goes");

        let failing = laid("failing");
        let unwound = std::panic::catch_unwind(|| {
            let _kept = super::KeptUntilRan::new(&failing);
            panic!("a hang detector gave up");
        });
        assert!(unwound.is_err());
        assert!(
            failing.join("bin").join("copy").is_file(),
            "a failing test's unwind keeps it"
        );

        let forgot = laid("forgot");
        let refused = std::panic::catch_unwind(|| drop(super::KeptUntilRan::new(&forgot)))
            .expect_err("a test that never said its copies ran fails");
        let why = refused
            .downcast_ref::<String>()
            .expect("the guard panics with its words");
        assert!(why.contains(&forgot.display().to_string()), "{why}");
        assert!(
            forgot.join("bin").join("copy").is_file(),
            "and keeps the directory"
        );
    }

    /// A signing that panics leaves no stand-in, so the next ask signs again.
    #[test]
    fn a_refused_signing_leaves_no_stand_in() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"bytes").expect("the binary");
        let refused = std::panic::catch_unwind(|| {
            super::signed_stand_in(&cache, &binary, &mut |_| panic!("codesign refused"));
        });
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            0,
            "nothing left"
        );
        let mut asked = 0;
        super::signed_stand_in(&cache, &binary, &mut |_| asked += 1);
        assert_eq!(asked, 1, "the next ask signs");
    }

    /// Whether `pid` is this test's own child and has not exited: a `WNOWAIT` `waitid`, which
    /// reads alike on every Unix and reaps nothing.
    fn runs_as_ours(pid: u32) -> bool {
        // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is valid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a non-blocking `WNOWAIT` `waitid` of one pid into the live local `info`.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::from(pid),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // SAFETY: `info` was zeroed, then filled by `waitid` or, with no exit to report under
        // `WNOHANG`, left with `si_pid` 0.
        rc == 0 && unsafe { info.si_pid() } == 0
    }

    /// Whether `pid` is no child of this test's any more: reaped, so `waitid` answers `ECHILD`.
    fn reaped(pid: u32) -> bool {
        // SAFETY: as in `runs_as_ours`.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: as in `runs_as_ours`.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::from(pid),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
    }

    /// THE PROOF LINE AT ONE TEST THREAD: a test binary run again at `--test-threads=1` writes
    /// `test <name> ... ` unended before its test's first line, so the proof follows it on that
    /// line; the line alone is proof too. NEGATIVE CONTROLS: a line that only ends with the proof,
    /// one where the proof is followed by more, and the bare prefix with no name are not.
    #[test]
    fn a_proof_line_counts_alone_and_after_libtests_one_thread_prefix() {
        let proof = b"appended";
        assert!(super::is_proof(b"appended", proof));
        assert!(super::is_proof(
            b"test tests::tables::child_append ... appended",
            proof
        ));
        for not in [
            &b"not appended"[..],
            b"xappended",
            b"test tests::tables::child_append ... appended ok",
            b"test  ... appended",
            b"test a ...appended",
            b"",
        ] {
            assert!(
                !super::is_proof(not, proof),
                "{}",
                String::from_utf8_lossy(not)
            );
        }
    }

    /// The pid a run wrote to `mark` (`echo $$ > mark`), once it has written all of it, within
    /// the one-minute hang detector.
    fn wrote_pid(mark: &std::path::Path) -> Option<u32> {
        let by = std::time::Instant::now() + super::ECHO_HANG;
        loop {
            let read = std::fs::read_to_string(mark).unwrap_or_default();
            if let Some(pid) = read.strip_suffix('\n') {
                return pid.parse().ok();
            }
            if std::time::Instant::now() >= by {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A child the program check holds past the hang detector ends the test COULD NOT RUN,
    /// naming it, and is killed and reaped; one that is not held, or unread, is still the
    /// test's own failure. (`sleep` writes nothing, so the 100 ms runs out by construction.)
    /// The reader is asked about the child itself while it still runs: the fake answers only
    /// for the child's pid while it is this test's unexited child (`runs_as_ours`).
    #[test]
    fn a_child_held_at_the_program_check_ends_could_not_run_and_others_still_hang() {
        use crate::spawnfd::{Stdio, command};
        let ends = |held: Option<bool>| {
            let mut child = command("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn");
            let pid = child.id();
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::prints_judged(
                    &mut child,
                    "ran",
                    std::time::Duration::from_millis(100),
                    &|asked| {
                        (asked == pid && runs_as_ours(asked))
                            .then_some(held)
                            .flatten()
                    },
                );
            }))
            .expect_err("a silent child ends the test");
            let why = refused
                .downcast_ref::<String>()
                .expect("the wait panics with its words")
                .clone();
            assert!(
                child.try_wait().expect("poll").is_some(),
                "{held:?}: killed and reaped"
            );
            (pid, why)
        };
        let (pid, why) = ends(Some(true));
        assert!(
            why.starts_with(crate::host::COULD_NOT_RUN),
            "expected COULD NOT RUN, got: {why}"
        );
        assert!(why.contains(&format!("(pid {pid})")), "{why}");
        for held in [Some(false), None] {
            let (pid, why) = ends(held);
            assert!(
                why.starts_with(&format!("the copied program (pid {pid}) never wrote")),
                "{held:?}: {why}"
            );
        }
    }

    /// A run that outlives its hang detector ends the test: COULD NOT RUN, naming it, while the
    /// program check held it then, and otherwise the test's own failure, naming it with what it
    /// wrote; either way it is killed and reaped. A run that ends hands back its status and
    /// output. Both the `spawnfd` run and a test's own `std` launch are judged so. The run
    /// writes a line, then its pid, then sleeps past the 100 ms by construction. The fake
    /// reads the pid the run wrote (within the one-minute hang detector) and answers only while
    /// the pid it was given is this test's unexited child, so it is asked about the run itself,
    /// while it still runs.
    #[test]
    fn a_run_past_its_hang_detector_ends_the_test_and_one_held_could_not_run() {
        const RUN: &str = "echo ran; echo $$ > \"$0\"; exec /bin/sleep 30";
        let hang = std::time::Duration::from_millis(100);
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let ends = |held: Option<bool>, by_std: bool| {
            let mark = dir.path().join(format!("pid-{held:?}-{by_std}"));
            let asked = std::cell::Cell::new(None);
            let fake = |pid| {
                asked.set(Some((pid, wrote_pid(&mark))));
                runs_as_ours(pid).then_some(held).flatten()
            };
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if by_std {
                    let how = super::Overrun {
                        held: &fake,
                        kill: super::kill_child,
                        reap: super::ECHO_HANG,
                    };
                    super::std_ends_within_by(
                        std::process::Command::new("/bin/sh")
                            .args(["-c", RUN])
                            .arg(&mark),
                        hang,
                        &how,
                    );
                } else {
                    let how = super::Overrun {
                        held: &fake,
                        kill: crate::deadline::children::kill_group,
                        reap: super::ECHO_HANG,
                    };
                    super::ends_within_by(
                        crate::spawnfd::command("/bin/sh")
                            .args(["-c", RUN])
                            .arg(&mark),
                        hang,
                        &how,
                    );
                }
            }))
            .expect_err("a run past its hang detector ends the test");
            let why = refused
                .downcast_ref::<String>()
                .expect("the wait panics with its words")
                .clone();
            let (pid, its) = asked
                .get()
                .expect("the run was asked about at its hang detector");
            assert_eq!(
                Some(pid),
                its,
                "{held:?}, std {by_std}: the run itself was asked"
            );
            assert!(reaped(pid), "{held:?}, std {by_std}: killed and reaped");
            (pid, why)
        };
        for by_std in [false, true] {
            let (pid, why) = ends(Some(true), by_std);
            assert!(
                why.starts_with(crate::host::COULD_NOT_RUN)
                    && why.contains(&format!("(pid {pid})")),
                "std {by_std}: expected COULD NOT RUN naming it, got: {why}"
            );
            for held in [Some(false), None] {
                let (pid, why) = ends(held, by_std);
                assert!(
                    why.starts_with(&format!("/bin/sh (pid {pid}) did not finish in time"))
                        && why.contains("\nstdout: ran\n"),
                    "{held:?}, std {by_std}: {why}"
                );
            }
        }
        let shell = "echo out; echo err >&2; exit 3";
        for out in [
            super::ends_within(
                crate::spawnfd::command("/bin/sh").args(["-c", shell]),
                super::ECHO_HANG,
            ),
            super::std_ends_within(
                std::process::Command::new("/bin/sh").args(["-c", shell]),
                super::ECHO_HANG,
            ),
        ] {
            assert_eq!(
                (out.status.code(), &out.stdout[..], &out.stderr[..]),
                (Some(3), &b"out\n"[..], &b"err\n"[..])
            );
        }
    }

    /// A run its kill does not end (whether a program macOS's program check holds takes its
    /// `SIGKILL` is unmeasured) still ends the test, once its reap has been waited on for its
    /// bound: the kill here sends nothing, and the outer wait is the one-minute hang detector.
    /// The run, which nothing signalled, is then killed by its pid: this test's own unexited
    /// child, which the fake was asked about. NEGATIVE CONTROL: with the reap after the kill
    /// the unbounded wait it was, the run outlives the hang detector and the test fails.
    #[test]
    fn a_run_its_kill_does_not_end_still_ends_the_test_at_its_hang_detector() {
        let (tell, asked) = std::sync::mpsc::channel();
        let (tx, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let held = |pid| {
                let _ = tell.send(pid);
                runs_as_ours(pid).then_some(true)
            };
            let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::ends_within_by(
                    crate::spawnfd::command("/bin/sleep").arg("120"),
                    std::time::Duration::from_millis(100),
                    &super::Overrun {
                        held: &held,
                        kill: |_| {},
                        reap: std::time::Duration::from_millis(100),
                    },
                );
            }));
            let _ = tx.send(waited.map_err(|why| why.downcast_ref::<String>().cloned()));
        });
        let pid = asked
            .recv_timeout(super::ECHO_HANG)
            .expect("the run was asked about at its hang detector");
        let waited = ended.recv_timeout(super::ECHO_HANG);
        if runs_as_ours(pid) {
            let _ = crate::process::kill_group_until_empty(i32::try_from(pid).expect("a pid"));
        }
        let why = waited
            .expect(
                "the wait on a run its kill does not end ended within the one-minute hang detector",
            )
            .expect_err("a run past its hang detector ends the test")
            .expect("the wait panics with its words");
        assert!(
            why.starts_with(crate::host::COULD_NOT_RUN) && why.contains(&format!("(pid {pid})")),
            "{why}"
        );
    }

    /// A run that cannot start is the test's own failure, naming the program and why, never a
    /// COULD NOT RUN, for the `spawnfd` run and a test's own `std` launch alike.
    #[test]
    fn a_run_that_cannot_start_fails_its_test_naming_it() {
        const MISSING: &str = "/nonexistent/aterm-uds-no-such-program";
        let refused = |run: &dyn Fn()| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .expect_err("a run that cannot start ends the test")
                .downcast_ref::<String>()
                .expect("the wait panics with its words")
                .clone()
        };
        let mut refusals = vec![refused(&|| {
            super::ends_within(&mut crate::spawnfd::command(MISSING), super::ECHO_HANG);
        })];
        // Under a translator std's glibc posix_spawn loses the exec error (the store to
        // its CLONE_VM-shared args is dropped, measured on the x86_64 cell under
        // Rosetta): the spawn reports Ok and the child exits 127, so a std run cannot
        // fail to start there. The transparent arm above still must, and does.
        if crate::process::translator().is_none() {
            refusals.push(refused(&|| {
                super::std_ends_within(&mut std::process::Command::new(MISSING), super::ECHO_HANG);
            }));
        }
        for why in refusals {
            assert!(
                why.starts_with(&format!("{MISSING} could not start: ")),
                "{why}"
            );
        }
    }

    /// The trustd skip counts this user's held programs beside the silence when there are
    /// any, and says nothing of them when there are none or they could not be counted.
    #[test]
    fn the_trust_skip_counts_the_wait_without_blaming_it() {
        use crate::process::HeldNow;
        let plain = super::trust_silent_words(None);
        assert!(!plain.contains("program check"), "{plain}");
        assert_eq!(super::trust_silent_words(Some(HeldNow::default())), plain);
        let two = HeldNow {
            count: 2,
            oldest_ms: 35_940_000,
        };
        assert_eq!(
            super::trust_silent_words(Some(two)),
            format!("{plain}; 2 programs of this user wait for macOS's program check now")
        );
        assert_eq!(
            super::trust_silent_words(Some(HeldNow { count: 1, ..two })),
            format!("{plain}; 1 program of this user waits for macOS's program check now")
        );
    }

    /// The silence ends the test COULD NOT RUN; an answer returns.
    #[test]
    fn a_silent_disk_image_service_ends_the_test_could_not_run() {
        super::require_disk_image_service(&run(1, "hdiutil: No space left on device\n"));
        let refused = std::panic::catch_unwind(|| {
            super::require_disk_image_service(&run(1, "hdiutil: Operation timed out\n"));
        })
        .expect_err("a timed-out run ends the test");
        let why = refused
            .downcast_ref::<String>()
            .expect("could_not_run panics with its words");
        assert!(why.starts_with(crate::host::COULD_NOT_RUN), "{why}");
    }
}
