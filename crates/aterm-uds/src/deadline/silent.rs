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
/// On Apple hosts, interpreter scripts stay at their absolute source path so sibling-relative
/// helpers remain available when the caller changes its working directory.
#[must_use]
#[track_caller]
pub fn stand_in(from: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_vendor = "apple")]
    {
        use std::io::Read as _;

        let (mut file, _) = crate::regular_file::open(from)
            .unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
        let mut prefix = [0; 2];
        let script = match file.read_exact(&mut prefix) {
            Ok(()) => prefix == *b"#!",
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
            Err(error) => panic!("read {} header: {error}", from.display()),
        };
        if script {
            return std::path::absolute(from)
                .unwrap_or_else(|e| panic!("resolve {}: {e}", from.display()));
        }
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
/// `from`'s size, modification time and inode ([`version_key`]), while its executable name stays
/// intact. Programs such as Git choose their behavior from that executable name.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn kept_at(cache: &std::path::Path, from: &std::path::Path) -> std::path::PathBuf {
    let meta = std::fs::metadata(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
    let name = stand_in_name(from);
    cache
        .join("named")
        .join(format!("{name}-{}", version_key(&meta)))
        .join(name)
}

/// The file name a stand-in of `from` runs under.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn stand_in_name(from: &std::path::Path) -> &str {
    from.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stand-in")
}

/// The version of a source [`kept_at`] names its directory by: `<size>-<mtime>.<nsec>-<inode>`.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn version_key(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;
    format!(
        "{}-{}.{}-{}",
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ino()
    )
}

/// Whether `key` has [`version_key`]'s shape.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn is_version_key(key: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mut parts = key.split('-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(len), Some(mtime), Some(ino), None) => {
            digits(len)
                && digits(ino)
                && mtime
                    .split_once('.')
                    .is_some_and(|(secs, nsec)| digits(secs) && digits(nsec))
        }
        _ => false,
    }
}

/// How long a superseded stand-in no lookup asked for is kept at the least: a process that
/// looked one up execs it at once, and its first exec's assessment by macOS's program check ends
/// within its test's one-minute hang detector.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
const STAND_IN_IDLE: Duration = Duration::from_secs(60 * 60);

/// The file beside a stand-in's version directory that records the source it was copied from.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn source_record(version: &std::path::Path) -> std::path::PathBuf {
    let mut name = version.as_os_str().to_owned();
    name.push(".source");
    std::path::PathBuf::from(name)
}

/// The copy of `from` in `cache` that `sign` signed ([`kept_at`]), made the first time this
/// version of `from` is asked for.
///
/// The cache keeps one stand-in per source name: placing a new version removes the other
/// versions of the same name that [`superseded`] says no process may still exec. Each lookup
/// holds its version directory's shared lock ([`VersionLock`]) while it stamps the directory's
/// modification time, reads `from`'s version a second time and sees the stand-in still there,
/// and returns the stand-in only when all three hold; the removal ([`evict`]) holds the exclusive
/// lock while it reads the stamp a last time and removes the directory. So a lookup that returns
/// a stand-in stamped it before that last read, and the removal keeps it, whichever name of the
/// source (a hard link included) the lookup found it through.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn signed_stand_in(
    cache: &std::path::Path,
    from: &std::path::Path,
    sign: &mut dyn FnMut(&std::path::Path),
) -> std::path::PathBuf {
    loop {
        let signed = kept_at(cache, from);
        let placed = !signed.is_file();
        if placed {
            place_stand_in(cache, from, &signed, sign);
        }
        let version = signed.parent().expect("stand-in has a version directory");
        let stamped = std::fs::File::open(version).and_then(|dir| {
            let held = VersionLock::shared(dir)?;
            held.0.set_modified(std::time::SystemTime::now())?;
            Ok(held)
        });
        let held = match stamped {
            Ok(held) => held,
            // Removed since: `from` changed after its version was read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("stamp {}: {e}", version.display()),
        };
        // Removed while this lookup waited for the lock, or `from` changed since.
        if kept_at(cache, from) != signed || !signed.is_file() {
            continue;
        }
        drop(held);
        if placed {
            remove_superseded(version, stand_in_name(from), STAND_IN_IDLE, &in_use);
        }
        return signed;
    }
}

/// Copy `from` to `signed`, `sign`ed, and record `from` beside its version directory
/// ([`source_record`]); a stand-in another process placed there first is kept.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn place_stand_in(
    cache: &std::path::Path,
    from: &std::path::Path,
    signed: &std::path::Path,
    sign: &mut dyn FnMut(&std::path::Path),
) {
    use std::os::unix::ffi::OsStrExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static FRESH: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(cache).unwrap_or_else(|e| panic!("create {}: {e}", cache.display()));
    let name = stand_in_name(from);
    let fresh_at = |what: &str| {
        cache.join(format!(
            ".{name}.{what}.{}.{}",
            std::process::id(),
            FRESH.fetch_add(1, Ordering::Relaxed)
        ))
    };
    let fresh = fresh_at("copy");
    let record = fresh_at("source");
    struct Fresh<'a>(&'a std::path::Path);
    impl Drop for Fresh<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _fresh = (Fresh(&fresh), Fresh(&record));
    copy_regular_fresh(from, &fresh)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), fresh.display()));
    sign(&fresh);
    let version = signed.parent().expect("stand-in has a version directory");
    std::fs::create_dir_all(version)
        .unwrap_or_else(|e| panic!("create {}: {e}", version.display()));
    let source =
        std::path::absolute(from).unwrap_or_else(|e| panic!("resolve {}: {e}", from.display()));
    std::fs::write(&record, source.as_os_str().as_bytes())
        .unwrap_or_else(|e| panic!("write {}: {e}", record.display()));
    std::fs::rename(&record, source_record(version))
        .unwrap_or_else(|e| panic!("place {}: {e}", record.display()));
    match std::fs::hard_link(&fresh, signed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => panic!("place {} at {}: {e}", fresh.display(), signed.display()),
    }
}

/// Remove each other version directory of the stand-in `name` beside `kept` that [`superseded`]
/// says no process may still exec and no lookup stamped since ([`evict`]), its source record last.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn remove_superseded(
    kept: &std::path::Path,
    name: &str,
    idle: Duration,
    in_use: &dyn Fn(&std::path::Path) -> bool,
) {
    let (Some(named), Some(kept)) = (kept.parent(), kept.file_name()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(named) else {
        return;
    };
    for entry in entries.flatten() {
        let dir_name = entry.file_name();
        let Some(key) = dir_name
            .to_str()
            .and_then(|dir_name| dir_name.strip_prefix(name))
            .and_then(|rest| rest.strip_prefix('-'))
        else {
            continue;
        };
        if dir_name == kept || !is_version_key(key) {
            continue;
        }
        let version = entry.path();
        if superseded(&version, name, key, idle, in_use) && evict(&version, idle) {
            let _ = std::fs::remove_file(source_record(&version));
        }
    }
}

/// An advisory lock held on a stand-in's version directory, released by `LOCK_UN` before its
/// close: shared by a lookup from its stamp to its last check, exclusive by [`evict`].
#[cfg(all(unix, any(target_vendor = "apple", test)))]
struct VersionLock(std::fs::File);

#[cfg(all(unix, any(target_vendor = "apple", test)))]
impl VersionLock {
    /// Take `dir`'s shared lock, waiting while an eviction holds it.
    fn shared(dir: std::fs::File) -> std::io::Result<Self> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        dir.lock_shared()?;
        Ok(Self(dir))
    }

    /// Take `dir`'s exclusive lock, or `None` when a lookup holds it or the lock fails.
    fn exclusive(dir: std::fs::File) -> Option<Self> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        dir.try_lock().ok()?;
        Some(Self(dir))
    }
}

#[cfg(all(unix, any(target_vendor = "apple", test)))]
impl Drop for VersionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether `meta`'s modification time (a lookup's stamp) is at least `idle` old.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn stamped_before(meta: std::io::Result<std::fs::Metadata>, idle: Duration) -> bool {
    meta.and_then(|meta| meta.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|since| since >= idle))
}

/// Remove the version directory `version` [`superseded`] judged, unless a lookup stamped it
/// since: under the directory's exclusive lock ([`VersionLock`]), held on the directory `version`
/// still names, its stamp is read again. A lookup holding the shared lock is still deciding, so
/// the directory is kept; one after the removal finds no stand-in there and places it afresh.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn evict(version: &std::path::Path, idle: Duration) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(held) = std::fs::File::open(version)
        .ok()
        .and_then(VersionLock::exclusive)
    else {
        return false;
    };
    let (Ok(locked), Ok(named)) = (held.0.metadata(), std::fs::metadata(version)) else {
        return false;
    };
    if (locked.dev(), locked.ino()) != (named.dev(), named.ino()) {
        return false;
    }
    stamped_before(Ok(locked), idle) && std::fs::remove_dir_all(version).is_ok()
}

/// Whether the stand-in `name` in the directory `version`, for the source version `key`, may be
/// removed: never one whose execution, or a first exec's assessment by macOS's program check,
/// may still be under way. It must hold, read in this order:
/// 1. its source is recorded ([`source_record`]) and no longer has `key`'s version (gone or
///    rebuilt), so no lookup of that source finds it again;
/// 2. no lookup stamped it within `idle` ([`STAND_IN_IDLE`]), so whoever found it last has long
///    since exec'd it and seen its assessment end;
/// 3. no process of this user holds it open, maps it or runs it (`in_use`).
#[cfg(all(unix, any(target_vendor = "apple", test)))]
fn superseded(
    version: &std::path::Path,
    name: &str,
    key: &str,
    idle: Duration,
    in_use: &dyn Fn(&std::path::Path) -> bool,
) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(source) = std::fs::read(source_record(version)) else {
        return false;
    };
    let source = std::path::Path::new(std::ffi::OsStr::from_bytes(&source));
    match std::fs::metadata(source) {
        Ok(meta) if version_key(&meta) == key => return false,
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    if !stamped_before(std::fs::metadata(version), idle) {
        return false;
    }
    let stand_in = version.join(name);
    !(stand_in.exists() && in_use(&stand_in))
}

/// Whether a process of this user has `file` open, mapped or running as its program, as
/// libproc's `proc_listpidspath` reads every process's descriptors, mapped regions and working
/// directory; a listing that fails says it may be.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn in_use(file: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn proc_listpidspath(
            kind: u32,
            typeinfo: u32,
            path: *const core::ffi::c_char,
            pathflags: u32,
            buffer: *mut core::ffi::c_void,
            buffersize: i32,
        ) -> i32;
    }
    const PROC_ALL_PIDS: u32 = 1;
    let Ok(path) = std::ffi::CString::new(file.as_os_str().as_bytes()) else {
        return true;
    };
    let mut pids = [0_i32; 64];
    // SAFETY: `path` is a live NUL-terminated string; `pids` has `buffersize` writable bytes.
    let found = unsafe {
        proc_listpidspath(
            PROC_ALL_PIDS,
            0,
            path.as_ptr(),
            0,
            pids.as_mut_ptr().cast(),
            i32::try_from(std::mem::size_of_val(&pids)).unwrap_or(i32::MAX),
        )
    };
    found != 0
}

/// No reader of other processes off Apple, where stand-ins are only tests': every stand-in may be in use.
#[cfg(all(unix, test, not(target_vendor = "apple")))]
fn in_use(_file: &std::path::Path) -> bool {
    true
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
#[path = "silent_tests.rs"]
mod tests;
