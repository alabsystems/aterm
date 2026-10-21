// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OS-primitive abstraction (§10): atpkg's platform-specific filesystem, activation,
//! disk-query and process-exec edges, behind one API with a per-OS backend
//! ([`unix`]/[`windows`]).
//!
//! Windows analogues of each Unix primitive:
//!
//! * **Activation**: `store/<program>/current` and the sysroot/toolchain dir links are
//!   directory **junctions** (`mklink /J`, no admin needed), made by [`atomic_symlink`].
//! * **Bin shims**: a `bin/<tool>.cmd` batch wrapper (`@"<target>.exe" %* & @exit /b`,
//!   [`CMD_FORWARD_TAIL`], behind the resume-proof frame [`CMD_FRAME_HEAD`]) instead of a
//!   `sh` exec stub. [`install_tombstone_shim`] writes the failing (`exit /b 70`) variant;
//!   [`resolve_shim`] parses a shim's target back.
//! * **Private state**: rests on the per-user `%LOCALAPPDATA%` profile ACL.
//!   [`harden_file`]/[`set_mode`] are no-ops, [`dir_meta_is_private`] is a best-effort
//!   `true`, [`permission_mode`]/[`our_uid`] report a not-applicable sentinel.
//! * **Disk**: [`volume_free_bytes`] calls `GetDiskFreeSpaceExW` instead of `statvfs`;
//!   both fail **OPEN** (`None` on any error).
//! * **Exec**: [`exec_or_run`] is `spawn().wait()` + `process::exit` (no `execve`).
//!
//! `ensure_private_dir` and the advisory [`FileLock`] are delegated to
//! `aterm_update_core` on both platforms, so atpkg and the updater cannot drift.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What the kernel says of when a pid's process started ([`process_birth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Birth {
    /// Kernel start-time stamp used alongside the pid to detect reuse.
    At(u64),
    /// Nothing lives under the pid: no such process, or a zombie.
    Gone,
    /// The kernel would not say — a sandbox denying process-info, another account's process,
    /// a platform with no reader — which says nothing about the process either way.
    Unreadable,
}

impl Birth {
    /// The start time, when the kernel gave one.
    #[must_use]
    pub const fn at(self) -> Option<u64> {
        match self {
            Self::At(born) => Some(born),
            Self::Gone | Self::Unreadable => None,
        }
    }
}

/// The private-directory creator, [`aterm_update_core::ensure_private_dir`] on both
/// platforms (Unix: `0700`, owned by our uid; Windows: `create_dir_all` under the
/// per-user profile ACL, final component a real directory).
pub use aterm_update_core::ensure_private_dir;

/// The advisory exclusive file lock: [`aterm_update_core::FileLock`] on both
/// platforms (Unix `flock(LOCK_EX)`, Windows `share_mode(0)` with bounded retry).
pub use aterm_update_core::FileLock;

/// Create `path` exclusively (`create_new`: an existing file is never opened and
/// truncated) and, on Unix, born `0600`. The temp of every atomic rewrite of a private
/// or user-owned file (rc files, `shell.d` hooks, the link marker, cargo's config): it
/// holds the WHOLE content before the real mode is applied, so a umask-default (usually
/// `0644`) temp would leave a `0600` file's contents readable by other users until that
/// chmod. Windows has no mode to ask for at creation; the per-user profile ACL covers it.
pub(crate) fn create_private_new(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Create the ONE directory `dir`, exclusively (an existing entry of that name fails
/// [`io::ErrorKind::AlreadyExists`], so the directory is provably this call's), born no
/// wider than `mode` and then given exactly `mode`, whatever the umask.
///
/// `mkdir(2)` with `mode` is masked by the umask, which can only narrow it: the
/// directory is never wider than `mode` at any instant, so nobody else can reach into it
/// between its birth and the `chmod`. The `chmod` then gives back what a strict umask
/// took (a `077` umask would leave a store directory `0700`, which every other user of a
/// system prefix must traverse). A plain `create_dir_all` is born `0777 & ~umask`:
/// world-writable under umask `000`, group-writable under `002` (AT3, 2026-10-10).
/// Windows has no mode to ask for; the per-user profile ACL covers it.
pub(crate) fn make_dir_born(dir: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), mode)
            .create(dir)?;
        set_mode(dir, mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir(dir)
    }
}

/// [`make_dir_born`] for `dir` and every missing ancestor, top down: a component that is
/// already a directory (followed through a link, as `create_dir_all` follows it) is left
/// exactly as it is, mode and `st_ctime` included, and every component this call makes
/// is born no wider than `mode` and ends at exactly `mode`. The store's own directories
/// on the way to a build (`store/`, `store/<program>/`) and to a download
/// (`staging/`, `staging/<program>/`) are made by this, never by the umask.
pub(crate) fn make_dirs_born(dir: &Path, mode: u32) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        make_dirs_born(parent, mode)?;
    }
    match make_dir_born(dir, mode) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        made => made,
    }
}

/// Make one open file or directory DURABLE: the barrier an atomic publish takes on its
/// temp before the rename, and [`crate::store::sync_dir`] on the directory after it:
/// [`aterm_durable::durable`], which on Apple targets is `fcntl(F_FULLFSYNC)`, a
/// drive-cache flush. A write that needs only its order kept takes
/// [`aterm_durable::ordered`] ([`crate::store::sync_dir_ordered`]); a staged TREE takes
/// the lighter [`sync_file_contents`], for the reasons given there.
#[cfg(not(test))]
pub(crate) fn sync_durable(f: &std::fs::File) -> io::Result<()> {
    aterm_durable::durable(f)
}

/// [`sync_durable`] under this crate's own unit tests: a plain `fsync(2)`
/// ([`sync_file_contents`]). It is still a real flush, made at the same point and in the
/// same order, so a test sees the same files, renames and errors. What it drops is the
/// drive-cache barrier. No test can observe that barrier, because it only orders the disk
/// against a power loss. It also holds up most flushes, growing writes, creates and
/// renames on the volume while it runs (`docs/measured/flush-wait-2026-10-21.md`): one
/// `atpkg --lib` run made 7,381 `F_FULLFSYNC`s (shim lays, vendor records, `status.toml`,
/// the floors, swap directory syncs), and every test binary a run started beside it waited
/// behind them for its own temp-dir metadata. With this arm the run took 28.5-29.8 s instead of
/// 44.5-63.6 s alone, and 18.6-20.1 s instead of 74.6-96.5 s beside `aterm-gui --lib`
/// (load 13-37). Off Apple targets `sync_all` already is
/// `fsync(2)` (`FlushFileBuffers` on Windows, which is also what `sync_file_contents`
/// calls there), so this arm changes nothing else. Integration tests and every shipped
/// build compile the arm above.
#[cfg(test)]
pub(crate) fn sync_durable(f: &std::fs::File) -> io::Result<()> {
    // Recorded as durable by the counting seam, as `aterm_durable::durable` would be.
    #[cfg(unix)]
    {
        aterm_durable::seam::durable_fsync(f)
    }
    #[cfg(not(unix))]
    {
        aterm_durable::durable(f)
    }
}

/// macOS's QoS launcher, the one the window wraps its package passes in. Absent
/// elsewhere, where [`utility_clamped`] leaves a program bare.
#[cfg(unix)]
const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// The argv head that starts `program` at UTILITY QoS for a detached background pass:
/// `taskpolicy -c utility <program>`, the clamp the window's lane gives the same passes
/// (aterm-gui `qos::command(Role::Background, …)`). Bare, a session's pass — the
/// six-hourly `aterm pkg update`, the daily `machine apply`, a head-watch pass, a warm —
/// and everything it starts ran at the default band (`ps` pri 31), an equal of the
/// program being typed into; behind the clamp it reads pri 20.
///
/// `taskpolicy` execs the program in place (`POSIX_SPAWN_SETEXEC`), so the pid a
/// detaching shell reports and the process group it made stay the program's own.
/// Wrapped only when both ends are real: `taskpolicy` exists and `program` is an absolute
/// path to an executable file, so a missing program still fails as itself, not as
/// `taskpolicy`'s exit 66. Never for a child that launches an aterm window: the clamp is
/// inherited, and an aterm under UTILITY starves its own paint.
#[cfg(unix)]
pub(crate) fn utility_clamped(program: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    utility_clamped_with(program, Path::new(TASKPOLICY))
}

/// [`utility_clamped`] over an explicit launcher, so the decision is testable on every
/// Unix against a stand-in.
#[cfg(unix)]
fn utility_clamped_with(program: &std::ffi::OsStr, taskpolicy: &Path) -> Vec<std::ffi::OsString> {
    let path = Path::new(program);
    if path.is_absolute()
        && crate::toolchain::is_executable_file(path)
        && crate::toolchain::is_executable_file(taskpolicy)
    {
        vec![
            taskpolicy.into(),
            "-c".into(),
            "utility".into(),
            program.into(),
        ]
    } else {
        vec![program.into()]
    }
}

/// Start `program args` DETACHED: stdio on `/dev/null`, its own process group, and NOT
/// the caller's child. Used by `crates/aterm/src/main.rs` for the session's pass and by
/// [`crate::warm`]. The session lane then runs `aterm_cli::session_main` in the same
/// process, whose unix driver reaps the shell with `waitpid(-1)`: a finished pass left
/// as ITS zombie could be reaped in the shell's place, and `aterm` would exit with the
/// pass's status instead of the shell's (reliably on Linux, ~6% of exits on macOS). So
/// on unix a `/bin/sh` middle process — the group leader — backgrounds the program and
/// exits at once, and reaping it here re-parents the program to launchd/init. The
/// program runs at utility QoS ([`utility_clamped`]), as the window's passes do. `env`
/// is one variable set on it.
///
/// # Errors
/// The middle shell could not be started, or did not exit 0.
pub fn spawn_detached(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
) -> io::Result<()> {
    spawn_detached_at(program, args, env, true)
}

/// [`spawn_detached`] for the one pass a person waits on: a first run's (the store's first
/// install of the ALab toolset still ahead, [`crate::adoption::first_install_ahead`]), whose
/// stubs block until it lands. Started the same way, at the default band rather than behind
/// [`utility_clamped`], as the window starts its own first run (aterm-gui
/// `qos::atpkg_pass_command`): on a busy fresh Mac the clamp held it far behind.
///
/// # Errors
/// As [`spawn_detached`].
pub fn spawn_detached_waited(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
) -> io::Result<()> {
    spawn_detached_at(program, args, env, false)
}

/// [`spawn_detached`]'s body: `clamped` puts the program behind [`utility_clamped`].
fn spawn_detached_at(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
    clamped: bool,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        let head = if clamped {
            utility_clamped(program)
        } else {
            vec![program.into()]
        };
        let status = command
            .args(["-c", "\"$@\" &", "sh"])
            .args(head)
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .process_group(0)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("the detaching shell {status}")))
        }
    }
    #[cfg(not(unix))]
    {
        // `spawnfd::command` decides its Windows console (`Console::Quiet`): from a
        // console (the terminal session) the pass shares it, as a bare child did;
        // from a process with none it gets a windowless console, where a bare child
        // would open a visible one for its whole run. No QoS clamp exists here.
        let _ = clamped;
        let mut command = aterm_uds::spawnfd::command(program);
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
    }
}

/// Install a `bin/` shim for `tool` pointing at that tool's executable inside a build's
/// `bin/` directory (`build_bin_dir`). `shim` is the concrete shim path — build it with
/// [`crate::store::Layout::shim`], never by joining the tool name yourself.
///
/// The two renderings of the name are DIFFERENT files on Windows: the shim is
/// `bin/<tool>.cmd` ([`crate::store::ToolName::shim_file`], the caller's `shim`) and its
/// target `<build>/bin/<tool>.exe` ([`crate::store::ToolName::exe_file`], derived here).
/// Taking a `ToolName` keeps that pair from collapsing into one string.
#[cfg(test)]
pub(crate) fn install_shim(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
) -> io::Result<()> {
    install_shim_to(shim, &build_bin_dir.join(tool.exe_file()))
}

/// `install_shim` whose wrapper also EXPORTS `env` before it execs the target (design
/// S7, [`crate::shim_env`]): `export NAME='VALUE'` lines ahead of the `exec` on Unix,
/// `@set "NAME=VALUE"` lines ahead of the `@"<target>" %*` on Windows. With an empty
/// `env` the shim is byte-identical to `install_shim`'s. Temp + rename like every shim.
pub fn install_shim_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    install_shim_to_env(shim, &build_bin_dir.join(tool.exe_file()), env)
}

/// [`install_shim_to`] with the exported `env` — the `(shim path, target)` form of
/// [`install_shim_env`], for the callers that already hold the target path.
pub fn install_shim_to(shim: &Path, target: &Path) -> io::Result<()> {
    install_shim_to_env(shim, target, &crate::shim_env::ShimEnv::NONE)
}

/// The shim [`install_shim_env`] would lay — the same target derivation, the same body
/// — RENDERED but not written, for the callers that render a whole pass of shims before
/// they lay it ([`crate::lay::write_in_process`]) or compare it with what is on disk.
pub fn shim_executable_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
    shim_executable_to_env(shim, &build_bin_dir.join(tool.exe_file()), env)
}

// ---------------------------------------------------------------------------
// Pure `.cmd` shim formatting/parsing.
//
// Pure string functions, compiled under `cfg(any(windows, test))` so the Unix test
// suite covers the Windows format; a non-test Unix build leaves them out. The frame
// ([`CMD_FRAME_HEAD`] and its pieces, [`cmd_framed`]) is the exception: the pending
// stub's `.cmd` renderer in `crate::stub` picks its platform at RUNTIME
// (`cfg!(windows)`), so the frame must compile everywhere.
// ---------------------------------------------------------------------------

/// THE TAIL OF EVERY `.cmd` FORWARD LINE after the target's closing quote:
/// `@"<target>" %* & @exit /b` — the program runs with the arguments verbatim, and the
/// SAME parsed line ends the batch. `cmd.exe` parses a line whole before it runs any of
/// it, and executes a batch file by re-reading it at a remembered BYTE OFFSET after every
/// line — so a shim re-laid (temp + remove + rename, `windows.rs`'s `atomic_write`) while
/// its program is running is never read again once the program returns: whatever stood
/// after the line in the OLD file is not looked for in the NEW one. A bare `exit /b`
/// returns with the ERRORLEVEL the program left, which is what `cmd /c` exits with;
/// `exit /b %errorlevel%` on the same line would be expanded when the line is PARSED,
/// before the program ran. The one string the renderer ([`cmd_shim_content_env`])
/// writes and the reader ([`parse_cmd_shim_target`]) keys on.
/// A legacy file without this tail IS read again after its program returns — at the old
/// file's end offset — which the frame ([`CMD_FRAME_HEAD`]) makes harmless. Written to
/// the documented `cmd` rules; no Windows host has run it.
#[cfg(any(windows, test))]
pub(crate) const CMD_FORWARD_TAIL: &str = " %* & @exit /b";

/// The tail a legacy `.cmd` shim carries: `@"<target>" %*` alone, on its last line. Still
/// READ ([`parse_cmd_shim_target`]) until every such shim is re-laid — a `bin/` shim or alias by its program's next install, update or `repair`, an `agents/`
/// twin by the next pass's `reconcile_agents` — so `which`, `doctor`, the sweeps and gc
/// keep resolving them meanwhile. Never written again. Its ONE re-lay to the framed shape
/// ([`CMD_FRAME_HEAD`]) is what the frame's padding is sized for.
pub(crate) const CMD_LEGACY_FORWARD_TAIL: &str = " %*";

/// THE FIRST LINE OF EVERY `.cmd` FILE THIS CRATE WRITES — a bin shim, an alias, a
/// tombstone, the `agents/` twin, and the pending stub (`crate::stub`, laid into the same
/// `bin\` and `agents\` slots ahead of an install): `@goto :main`, jumping over the
/// padding ([`CMD_PADDING_LINE`] × [`CMD_PADDING_LINES`]) and the [`CMD_PADDING_EXIT`] to
/// the [`CMD_MAIN_LABEL`] the real body starts at.
///
/// **Why a frame**: `cmd.exe` executes a batch file by re-opening it and seeking to a
/// remembered BYTE OFFSET after every line it runs. A legacy `.cmd` ends on `@"<target>"
/// %*` ALONE ([`CMD_LEGACY_FORWARD_TAIL`]) — no exit on that line — so when it is
/// executing its program at the moment the next pass re-lays it (temp + remove + rename,
/// `windows.rs`'s `atomic_write`) and the program exits, `cmd` resumes reading the NEW
/// file at the OLD file's end-of-file offset: inside whatever line of the new body that
/// offset falls in, running a fragment (`'xxx' is not recognized`, ERRORLEVEL 9009 in
/// place of the program's) or a line that runs the program a SECOND time. The frame puts
/// that offset somewhere harmless BY CONSTRUCTION: every byte of the new file from the
/// head's own CRLF up to and including the padding exit reads, as a line, either a LABEL
/// (a line of nothing but colons — every suffix of it still starts with `:`, and `cmd`
/// skips a label line, printing nothing), an EMPTY line (a suffix of a CRLF), or exactly
/// `@exit /b`, which returns with the ERRORLEVEL the program left untouched. The padding
/// is sized so the longest legacy file the old writer could have laid
/// ([`CMD_LEGACY_FILE_BOUND_BYTES`]) ends inside it ([`CMD_PADDING_END_BYTES`] ≥ that
/// bound, pinned at compile time below and by the resume simulation in the tests). A
/// fresh run starts at offset 0, reads `@goto :main` and runs the body once; a file of
/// THIS shape re-laid while it runs is never read again at all, because every line that
/// runs a program ends the batch on that same line ([`CMD_FORWARD_TAIL`]).
///
/// The head line's OWN bytes 1..10 are not inert (`goto :main` alone would run the body,
/// `oto :main` is a 9009) — and no resume can land there: `cmd` remembers a position
/// after a CRLF of the OLD file, and the shortest line the old writer could lay
/// ([`CMD_LEGACY_MIN_LINE_BYTES`], 12 bytes) is longer than the whole head line (13); the
/// head's own CRLF, at offsets 11 and 12, reads empty. Only an old file's FIRST line end
/// can fall inside the head at all (every later end is further along), and the tightest
/// first line any writer lays is the pending stub's `@echo off` + CRLF, 11 bytes, ending
/// exactly on the head's CRLF (`crate::stub::CMD_STUB_FIRST_LINE`, pinned there). No command line has only inert suffixes (its shortest
/// ones are its last word), so the head sits at the one place an old line end cannot be.
///
/// The frame costs [`CMD_FRAME_BYTES`] per `.cmd` (4110 bytes: 13 + 51 × 80 + 10 + 7),
/// under [`MAX_SHIM_BYTES`] by an order of magnitude with the longest body on top.
/// Rendered by [`cmd_framed`], pinned by the tests on every platform; `cmd.exe`'s reading
/// of it (the `goto`, label skipping, the offset resume) is written to the documented
/// `cmd` rules and is UNVERIFIED on a Windows host, like the rest of [`windows`].
pub(crate) const CMD_FRAME_HEAD: &str = "@goto :main\r\n";

/// The label [`CMD_FRAME_HEAD`] jumps to: the line right ahead of the real body.
const CMD_MAIN_LABEL: &str = "main";

/// One line of the frame's padding: 78 colons and a CRLF, 80 bytes. A label to `cmd`
/// from any byte of it (every suffix starts with `:` or is a CRLF suffix).
const CMD_PADDING_LINE: &str =
    "::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::\r\n";

/// How many [`CMD_PADDING_LINE`]s the frame carries: 51 × 80 = 4080 bytes.
const CMD_PADDING_LINES: usize = 51;

/// The line that closes the padding: a resume landing exactly on it returns at once
/// with the program's ERRORLEVEL; a fresh run never reaches it (the head jumped past).
const CMD_PADDING_EXIT: &str = "@exit /b\r\n";

/// The byte offset at which [`CMD_PADDING_EXIT`] starts: head + padding = 13 + 4080 =
/// 4093. Every offset in `CMD_FRAME_HEAD.len() - 2 ..= CMD_PADDING_END_BYTES` of a
/// framed file reads as an inert line, so a legacy file no longer than this is safe to
/// re-lay over while it runs.
pub(crate) const CMD_PADDING_END_BYTES: usize =
    CMD_FRAME_HEAD.len() + CMD_PADDING_LINES * CMD_PADDING_LINE.len();

/// The frame's whole cost per file: head, padding, exit, `:main` line — 4110 bytes.
pub(crate) const CMD_FRAME_BYTES: usize =
    CMD_PADDING_END_BYTES + CMD_PADDING_EXIT.len() + 1 + CMD_MAIN_LABEL.len() + 2;

/// The LONGEST `.cmd` the legacy writer could have laid, derived from that
/// writer's own caps: [`crate::shim_env::MAX_SHIM_ENV`] `@set "NAME=VALUE"` lines of at
/// most [`crate::shim_env::MAX_ENTRY_BYTES`] each, then `@"<target>" %*` with a target of
/// at most [`CMD_LEGACY_TARGET_BOUND_BYTES`] — 8 × 265 + 788 = 2908 bytes. (A legacy
/// twin was the plain shim under the twin's name, so the same bound covers it; a `bin/`
/// shim never carried a prelude.) [`CMD_PADDING_END_BYTES`] ≥ this, with 1185 bytes to
/// spare.
pub(crate) const CMD_LEGACY_FILE_BOUND_BYTES: usize = crate::shim_env::MAX_SHIM_ENV
    * ("@set \"".len() + crate::shim_env::MAX_ENTRY_BYTES + "\"\r\n".len())
    + "@\"".len()
    + CMD_LEGACY_TARGET_BOUND_BYTES
    + "\"".len()
    + CMD_LEGACY_FORWARD_TAIL.len()
    + "\r\n".len();

/// The longest target line a legacy shim's forward could carry, in bytes: Windows's
/// `MAX_PATH` (260 UTF-16 units) at the UTF-8 worst case of three bytes each — the old
/// writer capped nothing itself, and `cmd.exe`'s command launch is not long-path aware
/// (unverified, like everything about `cmd` here), so a shim whose target was longer
/// than `MAX_PATH` never ran its program in the first place.
pub(crate) const CMD_LEGACY_TARGET_BOUND_BYTES: usize = 3 * 260;

/// The SHORTEST line the legacy writer could lay — `@set "A=b"` and its CRLF, 12 bytes:
/// a one-byte name (`shim_env::split_entry` wants `[A-Z0-9_]+`, not digit-led) and a
/// one-byte value (`shim_env::admit` REFUSES an empty value — `@set "A="` is `unset` to
/// `cmd`, and was never a line this writer laid). The
/// forward line is longer: `@"` + an absolute `…\<tool>.exe` + `" %*` + CRLF is at
/// least 16. An old file's line ends, the only offsets `cmd` can resume at, are therefore
/// ≥ 12 — past every non-inert byte of [`CMD_FRAME_HEAD`] (its bytes 1..10), and past
/// its CRLF at bytes 11 and 12, which reads empty anyway.
pub(crate) const CMD_LEGACY_MIN_LINE_BYTES: usize = "@set \"A=b\"\r\n".len();

// The frame is sized by construction, not by a test alone.
const _: () = assert!(CMD_PADDING_END_BYTES >= CMD_LEGACY_FILE_BOUND_BYTES);
const _: () = assert!(CMD_FRAME_HEAD.len() - 2 <= CMD_LEGACY_MIN_LINE_BYTES);

/// `body` behind the resume-proof frame ([`CMD_FRAME_HEAD`]): `@goto :main`, the
/// padding, `@exit /b`, `:main`, then `body` verbatim. The ONE place the frame is
/// rendered; every `.cmd` renderer below goes through it, and so does the pending
/// stub's (`crate::stub::stub_content_cmd`), the one `.cmd` writer outside this
/// module.
pub(crate) fn cmd_framed(body: &str) -> String {
    let mut s = String::with_capacity(CMD_FRAME_BYTES + body.len());
    s.push_str(CMD_FRAME_HEAD);
    for _ in 0..CMD_PADDING_LINES {
        s.push_str(CMD_PADDING_LINE);
    }
    s.push_str(CMD_PADDING_EXIT);
    s.push(':');
    s.push_str(CMD_MAIN_LABEL);
    s.push_str("\r\n");
    s.push_str(body);
    s
}

/// What `cmd.exe` reads when it resumes a batch file at byte `offset`: the bytes from
/// there to the next line feed (a CR ahead of it dropped, as `cmd` drops it), or `None`
/// at or past the end, where `cmd` reads end-of-file and the batch ends. The resume
/// simulation's reader, shared with `crate::stub`'s tests (the pending stub is framed
/// too); a model of the documented `cmd` rules, never a run on Windows.
#[cfg(test)]
pub(crate) fn cmd_resumed_line(file: &[u8], offset: usize) -> Option<String> {
    if offset >= file.len() {
        return None;
    }
    let rest = &file[offset..];
    let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    let line = rest[..end].strip_suffix(b"\r").unwrap_or(&rest[..end]);
    Some(String::from_utf8_lossy(line).into_owned())
}

/// Whether a resumed line does nothing and leaves the program's ERRORLEVEL alone: a
/// label of nothing but colons (a suffix of a padding line), an empty line (a suffix of
/// a CRLF), exactly `@exit /b`, or end-of-file.
#[cfg(test)]
pub(crate) fn cmd_resumed_line_is_inert(line: Option<&str>) -> bool {
    match line {
        None => true,
        Some(l) => l.is_empty() || l.bytes().all(|b| b == b':') || l == "@exit /b",
    }
}

/// The frame alone ([`cmd_framed`] of nothing): what every `.cmd` this crate writes
/// starts with, byte for byte, for the tests that pin a whole file.
#[cfg(test)]
pub(crate) fn cmd_frame() -> String {
    cmd_framed("")
}

/// The body of a Windows bin shim: the frame ([`CMD_FRAME_HEAD`]), then `@"<target>" %*
/// & @exit /b` ([`CMD_FORWARD_TAIL`]), CRLF-terminated. `target` is the absolute path to
/// the store/checkout executable the shim forwards to. The no-environment form of
/// [`cmd_shim_content_env`], for the tests that pin the plain shape.
#[cfg(test)]
pub(crate) fn cmd_shim_content(target: &Path) -> String {
    cmd_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// [`cmd_shim_content`] with the shim's exported environment ahead of the forward
/// line: the frame, then one `@set "NAME=VALUE"` per entry (the quoted `set` form, so a
/// value's trailing space or `&` is literal), then `@"<target>" %* & @exit /b`. The `@`
/// keeps every line silent. An empty `env` is byte-identical to [`cmd_shim_content`].
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    cmd_framed(&cmd_shim_body_env(target, env))
}

/// The UNFRAMED shim body — the `@set` lines and the forward line — that
/// [`cmd_shim_content_env`] frames and [`cmd_shim_content_twin`] lays behind a prelude.
#[cfg(any(windows, test))]
fn cmd_shim_body_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    let mut s = String::new();
    for (name, value) in env.entries() {
        s.push_str("@set \"");
        s.push_str(name);
        s.push('=');
        s.push_str(value);
        s.push_str("\"\r\n");
    }
    s.push_str("@\"");
    s.push_str(&target.to_string_lossy());
    s.push('"');
    s.push_str(CMD_FORWARD_TAIL);
    s.push_str("\r\n");
    s
}

/// Whether every entry of `env` can sit inside `@set "NAME=VALUE"` without breaking
/// out: no `"` (closes the quote), no `%` (batch expansion), no CR/LF/NUL (an extra
/// line). [`crate::shim_env::ShimEnv::admit`] already refuses all of these, so this is
/// the same defence in depth as [`cmd_target_is_injection_safe`]: the I/O site refuses
/// rather than emit an injectable `.cmd`.
#[cfg(any(windows, test))]
pub(crate) fn cmd_env_is_injection_safe(env: &crate::shim_env::ShimEnv) -> bool {
    env.entries().iter().all(|(n, v)| {
        !n.chars()
            .chain(v.chars())
            .any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0'))
    })
}

/// The environment a Windows shim written by [`cmd_shim_content_env`] exports: its
/// `@set "NAME=VALUE"` lines, re-admitted through the rule (fail-closed: a hand-edited
/// line that breaks it reads as NONE). A tombstone or a plain shim reads as NONE.
#[cfg(any(windows, test))]
pub(crate) fn parse_cmd_shim_env(content: &str) -> crate::shim_env::ShimEnv {
    let mut raw: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@set \"")
            && let Some(entry) = rest.strip_suffix('"')
        {
            raw.push(entry.to_string());
        }
    }
    crate::shim_env::ShimEnv::admit(&raw).unwrap_or_default()
}

/// Whether `target` is safe to embed inside a `@"<target>" %* & @exit /b` shim WITHOUT
/// breaking out of the quoting or triggering batch expansion. A managed-store path (validated
/// program/build/tool components under the prefix) never contains any of these, so
/// this is defense-in-depth, fail-closed: a `"` closes the quote (command injection),
/// a `%` triggers `%VAR%` expansion (path substitution), and CR/LF/NUL inject extra
/// batch lines. The bin shim can't safely ESCAPE a `"` inside `@"…"`, so the I/O site
/// REFUSES an unsafe target rather than emit an injectable `.cmd`.
#[cfg(any(windows, test))]
pub(crate) fn cmd_target_is_injection_safe(target: &Path) -> bool {
    !target
        .to_string_lossy()
        .chars()
        .any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0'))
}

/// The body of a Windows **tombstone** shim: the frame ([`CMD_FRAME_HEAD`] — a tombstone
/// replaces a shim that may be executing, like any re-lay), then `message` printed to
/// stderr and exit 70 (`EX_SOFTWARE`), matching the Unix `sh` tombstone's contract.
/// `message` is `cmd`-escaped so a crafted tool name cannot break out of the `echo`.
#[cfg(any(windows, test))]
pub(crate) fn cmd_tombstone_content(message: &str) -> String {
    let mut s = String::from("@echo ");
    s.push_str(&cmd_echo_escape(message));
    s.push_str(" 1>&2\r\n@exit /b 70\r\n");
    cmd_framed(&s)
}

/// Escape a string for safe embedding in a `cmd.exe` `echo` argument: the shell
/// metacharacters `^ & < > | ( ) "` are `^`-escaped and `%` is doubled (batch
/// variable-expansion). `^` is handled first so its own escape is not re-escaped.
/// An em dash becomes `-`: cmd.exe reads the batch file in the console's OEM code
/// page, where the UTF-8 em dash prints as mojibake.
#[cfg(any(windows, test))]
fn cmd_echo_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%%"),
            '\u{2014}' => out.push('-'),
            '^' | '&' | '<' | '>' | '|' | '(' | ')' | '"' => {
                out.push('^');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Parse the forward target out of a Windows bin shim written by
/// [`cmd_shim_content`] (`@"<target>" %* & @exit /b`). Returns `None` for a tombstone
/// shim (no quoted target) or any unrecognized content — the Windows inverse of the
/// Unix `read_link` returning `Err` for a non-symlink.
///
/// Keyed on the forward line's EXACT shape — `@"`, the target, then [`CMD_FORWARD_TAIL`]
/// and nothing else (or, for a legacy shim not yet re-laid, [`CMD_LEGACY_FORWARD_TAIL`])
/// — not on any line that starts with `@"`: a legacy twin's landing prelude runs
/// `@"<atpkg>" __landing … -- %* & @exit /b`, whose closing quote is followed by
/// ` __landing`, so this reader walks past it to the twin's real forward. The frame's
/// lines ([`CMD_FRAME_HEAD`], the colon labels, `@exit /b`, `:main`) start with neither
/// `@"` nor `@set "`, so this reader and [`parse_cmd_shim_env`] answer the same over a
/// framed file and a legacy one.
#[cfg(any(windows, test))]
pub(crate) fn parse_cmd_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@\"")
            && let Some(end) = rest.find('"')
            && matches!(&rest[end + 1..], CMD_FORWARD_TAIL | CMD_LEGACY_FORWARD_TAIL)
        {
            return Some(PathBuf::from(&rest[..end]));
        }
    }
    None
}

/// `path` with a Windows VERBATIM prefix taken off — `\\?\C:\…` → `C:\…`, `\\?\UNC\srv\sh`
/// → `\\srv\sh` — so it can sit inside a `.cmd` line. `std::fs::canonicalize` answers a
/// verbatim path on Windows, and so does [`crate::stub::co_located_atpkg_path`];
/// `cmd.exe`'s built-ins and command launch do not reliably accept the `\\?\` spelling.
/// Pure string work (a path without the prefix comes back unchanged), pinned on every
/// platform.
#[must_use]
pub(crate) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        let mut out = String::from("\\\\");
        out.push_str(rest);
        return PathBuf::from(out);
    }
    if let Some(rest) = s.strip_prefix("\\\\?\\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// [`cmd_shim_content_env`] with `prelude` (empty for every `bin/` shim; an `agents/`
/// twin's [`twin_prelude`], empty on Windows today) ahead of the `@set` lines and the
/// forward line, the whole behind the same frame ([`CMD_FRAME_HEAD`]) — the `.cmd` twin
/// of [`sh_shim_content_twin`]. A prelude ends on a `:store` label, so its `goto store`
/// lands exactly on the exports the store forward needs.
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content_twin(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> String {
    let mut s = String::from(prelude);
    s.push_str(&cmd_shim_body_env(target, env));
    cmd_framed(&s)
}

/// The note line of the LEGACY `.cmd` landing prelude ([`cmd_landing_prelude`]).
#[cfg(test)]
const CMD_LANDING_NOTE: &str = "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
     waits for it and then runs the new one (aterm help pkg).\r\n";

/// The label the LEGACY `.cmd` landing prelude jumps to past its hand-over.
#[cfg(test)]
const CMD_LANDING_LABEL: &str = "store";

/// THE LEGACY `.cmd` LANDING PRELUDE — the bytes an old Windows `agents\` twin carries
/// ahead of its exports, rendered ONLY so the tests can lay such a twin; the next pass
/// re-lays a twin that carries it ([`crate::landing`]).
///
/// ```text
/// @rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing waits for it and then runs the new one (aterm help pkg).
/// @if not exist "<marker>" goto store
/// @if not exist "<atpkg>" goto store
/// @"<atpkg>" __landing "<program>" "<prefix>" -- %* & @exit /b
/// :store
/// ```
///
/// Every reader still answers over those bytes: the hand-over's closing quote is followed
/// by ` __landing`, never by [`CMD_FORWARD_TAIL`], so [`parse_cmd_shim_target`] walks
/// past it to the twin's real forward, and it carries no `@set` line for
/// [`parse_cmd_shim_env`]. Each line that runs a program ends the batch on that same line,
/// so the re-lay that retires it is safe while it runs. Empty when a path cannot be
/// embedded ([`cmd_embedded_path`]) or the program could break out of `"…"`.
#[cfg(test)]
#[must_use]
pub(crate) fn cmd_landing_prelude(
    program: &str,
    prefix: &Path,
    marker: &Path,
    atpkg: &Path,
) -> String {
    let (Some(marker), Some(atpkg), Some(prefix)) = (
        cmd_embedded_path(marker),
        cmd_embedded_path(atpkg),
        cmd_embedded_path(prefix),
    ) else {
        return String::new();
    };
    if !cmd_target_is_injection_safe(Path::new(program)) {
        return String::new();
    }
    let mut s = String::from(CMD_LANDING_NOTE);
    s.push_str("@if not exist \"");
    s.push_str(&marker);
    s.push_str("\" goto ");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n@if not exist \"");
    s.push_str(&atpkg);
    s.push_str("\" goto ");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n@\"");
    s.push_str(&atpkg);
    s.push_str("\" ");
    s.push_str(crate::landing::HIDDEN_VERB);
    s.push_str(" \"");
    s.push_str(program);
    s.push_str("\" \"");
    s.push_str(&prefix);
    s.push_str("\" --");
    s.push_str(CMD_FORWARD_TAIL);
    s.push_str("\r\n:");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n");
    s
}

/// A path as the LEGACY `.cmd` landing prelude embedded it, or `None` when it could not:
/// the verbatim prefix off ([`strip_verbatim_prefix`]), trailing `\` and `/` trimmed (a
/// quoted operand ending in `\"` is an escaped quote to the launched exe's argv parser),
/// then the injection guard ([`cmd_target_is_injection_safe`]).
#[cfg(test)]
fn cmd_embedded_path(path: &Path) -> Option<String> {
    let stripped = strip_verbatim_prefix(path);
    let trimmed = stripped
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    if trimmed.is_empty()
        || trimmed.ends_with(':')
        || !cmd_target_is_injection_safe(Path::new(&trimmed))
    {
        return None;
    }
    Some(trimmed)
}

/// THE PRELUDE of an agent program's `agents/` twin and its `bin/` shim — what
/// [`crate::activate::reconcile_agents`] renders into both: the self-update block
/// ([`sh_selfupdate_prelude`]) then the `--yolo` block ([`sh_yolo_prelude`]) on
/// Unix, and NOTHING on Windows — a `.cmd` block would test `"%~1"=="update"`,
/// which expands the user's first argument onto a batch line, where an argument
/// carrying a `"` can end the batch with the tool never run (the strand the Unix
/// rule forbids), and a `--yolo` rewrite would have to take `%*` apart the same
/// unsafe way; it waits for a Windows host that has measured `%~1` with `"`, `%`
/// and `^` (`docs/DESIGN-vendor-self-update-2026-09-19.md`). A
/// twin that still carries a legacy landing prelude compares unequal on the next pass
/// and is re-laid once (temp + rename; a running `sh` keeps the old inode, a running
/// `.cmd` ends its batch on the line that runs the program). `verbs` is the program's row
/// ([`crate::selfupdate::verbs_of`]) and `yolo` its policy
/// ([`crate::yolo::policy_of`]): each empty renders its block as nothing, and both
/// empty is the plain shim under the twin's name. `atpkg` is the one
/// [`crate::stub::Embedder`] settled on, or `None` to name none.
#[must_use]
pub(crate) fn twin_prelude(
    program: &str,
    prefix: &Path,
    atpkg: Option<&Path>,
    verbs: &[&str],
    yolo: &crate::yolo::Policy,
) -> String {
    if cfg!(windows) {
        return String::new();
    }
    let mut s = sh_selfupdate_prelude(program, prefix, crate::stub::named(atpkg), verbs);
    s.push_str(&sh_yolo_prelude(program, yolo, verbs));
    s
}

/// The Unix `bin/` shim body: a `/bin/sh` stub that EXECs the store binary.
///
/// # Why not a symlink
///
/// Trust's `targo` authenticates its own frontend before doing anything: it requires
/// `current_exe` to be a plain regular file (`protected Targo frontends cannot be
/// symlinks or reparse points`) and, in `validate_unprivileged_authority_path`, that
/// the path already equal its own `canonicalize()`. A symlinked shim fails both
/// ("could not authenticate Cargo/Targo frontend identity").
///
/// `exec` is what makes the stub work where a hardlink cannot: the process IMAGE is
/// replaced, so by the time targo authenticates, `current_exe` is the real binary at
/// its real path — and, just as importantly, `frontend.parent()` is the true toolchain
/// `bin/`, which is how targo finds its sysroot siblings. A hardlink would be a plain
/// file but would sit in `<prefix>/bin`, where those siblings are not.
///
/// Keeping a shim at all (rather than putting the store's `bin/` on PATH) is what
/// keeps the `exposes` allowlist meaningful — `shim_allowed` refuses a tool honestly
/// or maliciously named `sudo`/`ssh`/`git`, and a raw directory on PATH would expose
/// every binary in a build regardless.
///
/// `exec "<target>" "$@"` sets argv[0] to the target, which targo's brand detection
/// reads: the stub is invisible to the tool it launches.
///
/// The no-environment form of [`sh_shim_content_env`], for the tests that pin the plain
/// shape.
#[cfg(test)]
pub(crate) fn sh_shim_content(target: &Path) -> String {
    sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// The marker comment ahead of the `export` lines of a shim that carries an
/// environment — a human reading `bin/claude` sees where the variables come from: the
/// program's policy (compiled for a vendor-direct program, signed for an index one).
#[cfg(any(unix, test))]
const SH_SHIM_ENV_NOTE: &str =
    "# shim_env is aterm's policy for this program: only this managed copy runs with it.\n";

/// [`sh_shim_content`] with the shim's exported environment ahead of the `exec`
/// (design S7, [`crate::shim_env`]): the note, then one `export NAME='VALUE'` per entry
/// — the value single-quoted by [`sh_shim_quote`]'s rule, so nothing in it is ever a
/// word to `sh` — then the unchanged `exec '<target>' "$@"`. An empty `env` is
/// byte-identical to [`sh_shim_content`]: every shim laid before the key existed.
///
/// The exec'd image inherits the exports; `parse_sh_shim_target` still reads the target
/// off the exec line. The unrouted form of [`sh_shim_content_routed`], for the tests.
#[cfg(test)]
pub(crate) fn sh_shim_content_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    sh_shim_content_routed(target, env, None)
}

/// The note ahead of the guard line of a shim routed through a [`crate::compat`] exec
/// root — what a human reading `bin/tippy` learns about the extra line, and where to ask.
#[cfg(any(unix, test))]
const SH_SHIM_ROUTE_NOTE: &str = "# atpkg exec root: this build ships bin/rustc as a separate copy of trustc, which its \
     tippy refuses; the same tools run from a clone of the build where rustc holds trustc's bytes (aterm pkg doctor).\n";

/// [`sh_shim_content_env`] with an optional ROUTE: with `route = None` it is that shim,
/// byte for byte (every build that needs no exec root). With `route = Some(R)`, the target
/// `S`'s `exec` line is preceded by the note and one guard line:
///
/// ```text
/// [ ! -h 'R' ] && [ -f 'R' ] && [ -x 'R' ] && [ -f 'S' ] && [ ! -h 'M' ] && [ -f 'M' ] && exec 'R' "$@"
/// exec 'S' "$@"
/// ```
///
/// where `M` is the root's [`crate::compat::ROOT_MARKER`] (`R` is `<root>/bin/<file>`, so
/// `M` is `<root>/.atpkg-root`).
///
/// WHY. Trust bundles 8571, 8589, 8590 and 8595 ship `bin/rustc` as a separately signed
/// copy of `bin/trustc` (2,428 bytes differ on 8595, every one inside the ad-hoc code
/// signature), and their tippy refuses to run unless the sibling `rustc` is the same file
/// as, or byte-identical to, the selected `trustc` — "rustc-compatible sibling … is not
/// the selected Trust compiler". The store is content-addressed and never modified, so
/// [`crate::compat`] lays a copy-on-write clone of the build ([`crate::clone`]) where
/// `rustc` holds `trustc`'s bytes, and the shim runs the tool from there.
///
/// The guard checks, at every exec, that the root stands and is whole: `R` is a regular
/// executable file and not a symlink, the store file `S` it stands for still exists (so a
/// reclaimed build never runs from a leftover root), and the marker `M` — written last, before
/// the `rename(2)` that commits the root — is a regular file, which a half-laid root has not.
/// Any false falls through to the unchanged store `exec`. It proves no bytes:
/// [`crate::compat::ensure_root`] does that when it lays the root.
///
/// NOTHING THAT READS A SHIM SEES A DIFFERENCE. The env note and `export` lines stay
/// ahead of the guard, so both `exec`s inherit them. The guard line starts with `[`, so
/// `parse_sh_shim_target` — the first trimmed line that starts with `exec '` — still
/// answers `S`, `parse_sh_shim_env` still reads the same exports, and a `sed '^exec …'`
/// still prints `S`: `which`, gc's
/// witnesses, `prune_stale_shims`, the alias reconcile and doctor's broken-shim scan all
/// answer as they did for the plain shim.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_content_routed(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
) -> String {
    sh_shim_body(target, env, route, "")
}

/// The note line of the LEGACY `sh` landing prelude ([`sh_landing_prelude`]).
#[cfg(all(test, unix))]
const SH_LANDING_NOTE: &str = "# atpkg agents twin: while a newer build of this program is landing, `atpkg __landing` \
     waits for it and then runs the new one (aterm help pkg).\n";

/// THE LEGACY `sh` LANDING PRELUDE — the bytes an old Unix `agents/` twin carries after
/// its self-update block, rendered ONLY so the tests can lay such a twin; the next pass
/// re-lays a twin that carries it ([`crate::landing`]). One `[ -f <marker> ]`, and while
/// the marker stands a hand-over to `atpkg __landing <program> '<prefix>' -- "$@"`, which
/// `exec`s `bin/<program>` at once. No line of it is a literal `exec '` and none an
/// `export `, so every reader of the twin's target and env answers over those bytes as
/// over a twin without them.
#[cfg(all(test, unix))]
#[must_use]
pub(crate) fn sh_landing_prelude(
    program: &str,
    prefix: &Path,
    marker: &Path,
    atpkg: &Path,
) -> String {
    let mut operands = sh_quote_str(program);
    operands.push(' ');
    operands.push_str(&sh_quote_str(&prefix.to_string_lossy()));
    let mut s = String::from(SH_LANDING_NOTE);
    s.push_str("if [ -f ");
    s.push_str(&sh_quote_str(&marker.to_string_lossy()));
    s.push_str(" ]; then\n  __atpkg=");
    s.push_str(&sh_quote_str(&atpkg.to_string_lossy()));
    s.push_str("\n  if [ -x \"$__atpkg\" ]; then exec \"$__atpkg\" ");
    s.push_str(crate::landing::HIDDEN_VERB);
    s.push(' ');
    s.push_str(&operands);
    s.push_str(" -- \"$@\"; fi\nfi\n");
    s
}

/// The invariant tail of the self-update block's note line — what follows the program,
/// its verbs and the verb name, which are rendered per row: a human reading
/// `agents/claude` learns what the `case` ahead of the exports does, and where to ask.
const SH_SELFUPDATE_NOTE: &str =
    "` — aterm updates this copy from its vendor and verifies every build here (aterm help pkg).\n";

/// THE SELF-UPDATE BLOCK of an agent program's `agents/` twin AND its `bin/` shim
/// ([`crate::selfupdate`]; outside an aterm session `bin/<program>` is the managed name),
/// the whole of [`twin_prelude`] on Unix:
///
/// ```text
/// # atpkg: `claude update|upgrade|install` is answered by `atpkg __selfupdate` — aterm updates this copy from its vendor and verifies every build here (aterm help pkg).
/// case "$1" in
///   update|upgrade|install)
///     __atpkg='/Applications/aterm.app/Contents/MacOS/atpkg'
///     if [ -x "$__atpkg" ] && [ "${__ATPKG_SELFUPDATE_PID-}" != "$$" ]; then __ATPKG_SELFUPDATE_PID=$$ exec "$__atpkg" __selfupdate 'claude' '/Users//u/Library/Application Support/aterm/pkg' -- "$@"; fi
///     ;;
/// esac
/// ```
///
/// * THE RE-ENTRY GUARD ([`crate::selfupdate::REENTRY_VAR`]): the verb's forward `exec`s
///   `bin/<program>`, which carries this block, under the same pid; the block hands over
///   only when the variable does not name its own `$$`, so the forward reaches the store
///   build once instead of handing over again forever — whichever atpkg the block names.
///   An assignment prefix on `exec`, measured exported to the exec'd image, with `$$`
///   unchanged across the `exec`, in sh, bash, zsh, dash and ksh.
/// * `case "$1"` — the FIRST TOKEN, exactly, and never glob-expanded (the word is
///   quoted); the patterns are literal words from the row, validated by
///   [`crate::selfupdate::is_shell_safe_verb`] at render; an unset `$1` is the empty
///   string and matches nothing; `"$@"` is verbatim. No user byte is ever parsed as
///   shell, which is why the classification of everything past `$1` lives in Rust.
/// * THE EXEC IS THE GUARANTEE: when `$__atpkg` is not executable — the app relocated,
///   the bundle gone — the arm falls through `;;` to the twin's own exports and the store
///   `exec`, so the vendor's verb runs; never `command -v atpkg`, which would exec an
///   OLDER `atpkg` into `unknown verb` exit 2 with the tool never run. An EMPTY `atpkg` (no atpkg to name,
///   [`crate::stub::named`]) renders `__atpkg=''` and falls through the same way.
/// * NO LINE HERE IS A LITERAL `exec '` (the one `exec` is `exec "$__atpkg"` on a line
///   starting with `if`), NO `export ` line, no pending-stub marker; every embedded
///   string goes through [`sh_quote_str`] — so `parse_sh_shim_target`, `resolve_shim`,
///   `sweep_agents_dir`'s keep-predicate and `parse_sh_shim_env` are unaffected.
/// * FAIL-CLOSED RENDER: the EMPTY string — the twin is the plain shim — when
///   `verbs` is empty (a member with no self-updater, or a program with no row), when any
///   verb fails the word rule (it would break the `case` pattern — including `esac`, the
///   one regex-shaped word `sh` reserves where a pattern list starts: measured,
///   `esac)` in that slot is a parse error in sh, bash, dash, zsh and ksh, and the twin
///   would never reach its `exec`; [`crate::selfupdate::is_shell_safe_verb`] refuses it),
///   or when the program is not a [`crate::store::ToolName`] or fails the same word rule
///   (it lands in the note line and in a quoted operand). Never an injectable line, never
///   a stranded tool.
///
/// Pure string building, pinned on every platform.
#[must_use]
pub(crate) fn sh_selfupdate_prelude(
    program: &str,
    prefix: &Path,
    atpkg: &Path,
    verbs: &[&str],
) -> String {
    if verbs.is_empty()
        || !verbs
            .iter()
            .all(|v| crate::selfupdate::is_shell_safe_verb(v))
        || crate::store::ToolName::new(program).is_none()
        || !crate::selfupdate::is_shell_safe_verb(program)
    {
        return String::new();
    }
    let patterns = verbs.join("|");
    let guard = crate::selfupdate::REENTRY_VAR;
    let mut s = String::from("# atpkg: `");
    s.push_str(program);
    s.push(' ');
    s.push_str(&patterns);
    s.push_str("` is answered by `atpkg ");
    s.push_str(crate::selfupdate::HIDDEN_VERB);
    s.push_str(SH_SELFUPDATE_NOTE);
    s.push_str("case \"$1\" in\n  ");
    s.push_str(&patterns);
    s.push_str(")\n    __atpkg=");
    s.push_str(&sh_quote_str(&atpkg.to_string_lossy()));
    s.push_str("\n    if [ -x \"$__atpkg\" ] && [ \"${");
    s.push_str(guard);
    s.push_str("-}\" != \"$$\" ]; then ");
    s.push_str(guard);
    s.push_str("=$$ exec \"$__atpkg\" ");
    s.push_str(crate::selfupdate::HIDDEN_VERB);
    s.push(' ');
    s.push_str(&sh_quote_str(program));
    s.push(' ');
    s.push_str(&sh_quote_str(&prefix.to_string_lossy()));
    s.push_str(" -- \"$@\"; fi\n    ;;\nesac\n");
    s
}

/// THE `--yolo` BLOCK of an agent program's twin and `bin/` shim ([`crate::yolo`]),
/// rendered after the self-update block in [`twin_prelude`]. For claude (no word
/// lists) it reads:
///
/// ```text
/// # atpkg: yolo by default — this name runs claude with every approval and sandbox off ('--dangerously-skip-permissions'); `--yolo` ahead of a literal `--` spells the same, a launch that types its own posture runs as typed, and the command as run is printed on stderr (aterm help pkg).
/// __atpkg_yolo=0
/// __atpkg_posture=0
/// __atpkg_p0=0
/// for __atpkg_a in "$@"; do
///   case "$__atpkg_a" in
///     --) break ;;
///     --yolo) __atpkg_yolo=1 ;;
///     '--dangerously-skip-permissions') __atpkg_p0=1; __atpkg_posture=1 ;;
///     '--allow-dangerously-skip-permissions'|'--permission-mode'|'--permission-mode='*) __atpkg_posture=1 ;;
///   esac
/// done
/// __atpkg_say=
/// if [ "$__atpkg_yolo" = 1 ]; then
///   __atpkg_say="aterm: --yolo: running 'claude'"
///   __atpkg_stop=0
///   __atpkg_n=$#
///   while [ "$__atpkg_n" -gt 0 ]; do
///     __atpkg_n=$((__atpkg_n - 1))
///     if [ "$__atpkg_stop" = 0 ] && [ "$1" = "--yolo" ]; then
///       shift
///       if [ "$__atpkg_p0" = 0 ]; then __atpkg_p0=1; set -- "$@" '--dangerously-skip-permissions'; fi
///     else
///       if [ "$1" = "--" ]; then __atpkg_stop=1; fi
///       set -- "$@" "$1"
///       shift
///     fi
///   done
/// elif [ "$__atpkg_posture" = 0 ]; then
///   __atpkg_say="aterm: yolo (default): running 'claude'"
///   set -- '--dangerously-skip-permissions' "$@"
/// fi
/// if [ -n "$__atpkg_say" ]; then
///   printf '%s' "$__atpkg_say" 1>&2
///   for __atpkg_a in "$@"; do
///     printf " '" 1>&2
///     __atpkg_rest=$__atpkg_a
///     while case "$__atpkg_rest" in *"'"*) true ;; *) false ;; esac; do
///       printf '%s' "${__atpkg_rest%%"'"*}" 1>&2
///       printf '%s' "'\\''" 1>&2
///       __atpkg_rest=${__atpkg_rest#*"'"}
///     done
///     printf '%s' "$__atpkg_rest" 1>&2
///     printf "'" 1>&2
///   done
///   printf '\n' 1>&2
/// fi
/// ```
///
/// A row with word lists (muse) also tracks the first non-flag word: the
/// default goes behind an `after` word and never on a `skip` word, and a
/// `--yolo` typed ahead of an `after` word expands behind it (the pending
/// rotation; [`crate::yolo::decide`] is the rule both lanes keep).
///
/// * PURE `/bin/sh`, no hand-over: every user word is only ever compared
///   (`case`, `[ … = … ]`) or moved quoted (`set -- "$@" "$1"`), never parsed,
///   so the default holds with no atpkg reachable and every run still reaches
///   the twin's `exec`. A rotation consumes exactly the original `$#` words —
///   each appended once at the tail, a `--yolo` as the flags not already
///   present (a flag is never added twice: codex refuses its bypass repeated)
///   — so order and empty words survive; a literal `--` freezes the rest.
/// * THE ECHO is [`crate::yolo::echo_line`]/[`crate::yolo::default_echo_line`]'s
///   twin: each word single-quoted, an embedded `'` split out as `'\''`
///   (printed with `printf '%s'` — as a printf FORMAT `\'` collapses to `'`
///   under bash, measured 2026-10-19), on stderr only, on a rewritten launch
///   only.
/// * No line is a literal `exec '` or `export `, so every reader of the twin's
///   target and env answers as over a plain shim; every embedded word is
///   rendered quoted and validated first.
/// * FAIL-CLOSED RENDER: the EMPTY string when the flags or the markers are
///   empty (a default that cannot tell a decided launch would upgrade the
///   harness's own resume lines), when any flag, marker or word fails its word
///   rule, or when the program is not a [`crate::store::ToolName`] or fails the
///   verb word rule — the self-update block's rule, widened.
///
/// Measured against this shape in sh, bash, dash and zsh (2026-10-19),
/// including arguments carrying quotes, `$`, backticks and spaces. Pure string
/// building, pinned on every platform; [`twin_prelude`] renders it on Unix only.
#[must_use]
pub(crate) fn sh_yolo_prelude(
    program: &str,
    policy: &crate::yolo::Policy,
    skip_extra: &[&str],
) -> String {
    let crate::yolo::Policy {
        flags,
        markers,
        after,
        skip,
    } = *policy;
    if flags.is_empty()
        || markers.is_empty()
        || !flags.iter().all(|f| crate::yolo::is_shell_safe_flag(f))
        || !markers.iter().all(|m| crate::yolo::is_shell_safe_marker(m))
        || !after
            .iter()
            .chain(skip.iter())
            .chain(skip_extra.iter())
            .all(|w| crate::selfupdate::is_shell_safe_verb(w))
        || crate::store::ToolName::new(program).is_none()
        || !crate::selfupdate::is_shell_safe_verb(program)
    {
        return String::new();
    }
    // The twin's self-update verbs ride along as skip words (`twin_prelude`
    // passes them): an update shape is decided ahead of this block, and its
    // help forward through `bin/` must not be defaulted on the way.
    let skip_all: Vec<&str> = skip
        .iter()
        .copied()
        .chain(skip_extra.iter().copied().filter(|w| !skip.contains(w)))
        .collect();
    let spelled = flags
        .iter()
        .map(|f| sh_quote_str(f))
        .collect::<Vec<_>>()
        .join(" ");
    let words = !after.is_empty() || !skip_all.is_empty();
    // One `if` per flag not already present/emitted — the dedupe both the
    // by-the-flag rotation and (via the posture scan) the default rely on.
    let fresh = |indent: &str| {
        let mut f = String::new();
        for (i, flag) in flags.iter().enumerate() {
            f.push_str(indent);
            f.push_str("if [ \"$__atpkg_p");
            f.push_str(&i.to_string());
            f.push_str("\" = 0 ]; then __atpkg_p");
            f.push_str(&i.to_string());
            f.push_str("=1; set -- \"$@\" ");
            f.push_str(&sh_quote_str(flag));
            f.push_str("; fi\n");
        }
        f
    };
    let say = |prefix: &str| {
        let mut v = String::from("\"");
        v.push_str(prefix);
        v.push_str(&sh_quote_str(program));
        v.push('"');
        v
    };
    let mut s = String::from("# atpkg: yolo by default — this name runs ");
    s.push_str(program);
    s.push_str(" with every approval and sandbox off (");
    s.push_str(&spelled);
    s.push_str(
        "); `--yolo` ahead of a literal `--` spells the same, a launch that types \
         its own posture runs as typed, and the command as run is printed on stderr \
         (aterm help pkg).\n",
    );
    s.push_str("__atpkg_yolo=0\n__atpkg_posture=0\n");
    s.push_str("__atpkg_have_word=0\n__atpkg_word=\n__atpkg_word_at=0\n__atpkg_word_ok=1\n");
    for i in 0..flags.len() {
        s.push_str("__atpkg_p");
        s.push_str(&i.to_string());
        s.push_str("=0\n");
    }
    s.push_str(
        "for __atpkg_a in \"$@\"; do\n  \
         case \"$__atpkg_a\" in\n    \
         --) break ;;\n    \
         --yolo) __atpkg_yolo=1 ;;\n",
    );
    for (i, flag) in flags.iter().enumerate() {
        s.push_str("    ");
        s.push_str(&sh_quote_str(flag));
        s.push_str(") __atpkg_p");
        s.push_str(&i.to_string());
        s.push_str("=1; __atpkg_posture=1 ;;\n");
    }
    // The patterns `yolo::marker_matches` keeps, in `case` spelling: a `--word=`
    // entry and a short flag are prefixes (their attached forms are the same
    // flag to the vendor's parser), a long flag is itself or its `=value` form.
    let marker_pattern = |m: &str| {
        if m.ends_with('=') || (m.len() == 2 && m.starts_with('-') && !m.starts_with("--")) {
            let mut p = sh_quote_str(m);
            p.push('*');
            p
        } else {
            let mut p = sh_quote_str(m);
            p.push('|');
            p.push_str(&sh_quote_str(&format!("{m}=")));
            p.push('*');
            p
        }
    };
    let rest: Vec<String> = markers
        .iter()
        .filter(|m| !flags.contains(m))
        .map(|m| marker_pattern(m))
        .collect();
    if !rest.is_empty() {
        s.push_str("    ");
        s.push_str(&rest.join("|"));
        s.push_str(") __atpkg_posture=1 ;;\n");
    }
    s.push_str("  esac\ndone\n");
    // Discover the original argument position separately from the posture
    // scan: a known value can itself spell a subcommand or posture word.
    let (values, switches) = crate::yolo::word_options(program);
    let standalone: Vec<&str> = std::iter::once(crate::yolo::WORD)
        .chain(flags.iter().copied())
        .chain(switches.iter().copied())
        .collect();
    s.push_str(
        "__atpkg_at=0\n__atpkg_value=0\n\
         for __atpkg_a in \"$@\"; do\n  \
         __atpkg_at=$((__atpkg_at + 1))\n  \
         if [ \"$__atpkg_a\" = \"--\" ]; then break; fi\n  \
         if [ \"$__atpkg_value\" = 1 ]; then __atpkg_value=0; continue; fi\n  \
         case \"$__atpkg_a\" in\n    ",
    );
    s.push_str(
        &standalone
            .iter()
            .map(|f| sh_quote_str(f))
            .collect::<Vec<_>>()
            .join("|"),
    );
    s.push_str(") ;;\n");
    if !values.is_empty() {
        s.push_str("    ");
        s.push_str(
            &values
                .iter()
                .map(|f| sh_quote_str(f))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") __atpkg_value=1 ;;\n    ");
        s.push_str(
            &values
                .iter()
                .map(|f| marker_pattern(f))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") ;;\n");
    }
    s.push_str(
        "    -*) __atpkg_word_ok=0; break ;;\n    \
         *) __atpkg_have_word=1; __atpkg_word=$__atpkg_a; \
         __atpkg_word_at=$__atpkg_at; break ;;\n  \
         esac\ndone\n\
         if [ \"$__atpkg_value\" = 1 ]; then __atpkg_word_ok=0; fi\n",
    );
    if !after.is_empty() {
        s.push_str(
            "__atpkg_after=0\n\
             if [ \"$__atpkg_have_word\" = 1 ]; then\n  \
             case \"$__atpkg_word\" in\n    ",
        );
        s.push_str(
            &after
                .iter()
                .map(|w| sh_quote_str(w))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") __atpkg_after=1 ;;\n  esac\nfi\n");
    }
    s.push_str("__atpkg_say=\nif [ \"$__atpkg_yolo\" = 1 ]");
    if !after.is_empty() {
        s.push_str(" && [ \"$__atpkg_word_ok\" = 1 ]");
    }
    s.push_str("; then\n  __atpkg_say=");
    s.push_str(&say(crate::yolo::ECHO_PREFIX));
    s.push_str(
        "\n  \
         __atpkg_stop=0\n  __atpkg_at=0\n",
    );
    if !after.is_empty() {
        s.push_str("  __atpkg_pending=0\n");
    }
    s.push_str(
        "  __atpkg_n=$#\n  \
         while [ \"$__atpkg_n\" -gt 0 ]; do\n    \
         __atpkg_n=$((__atpkg_n - 1))\n    \
         __atpkg_at=$((__atpkg_at + 1))\n    \
         if [ \"$__atpkg_stop\" = 0 ] && [ \"$1\" = \"--yolo\" ]; then\n      \
         shift\n",
    );
    if after.is_empty() {
        s.push_str(&fresh("      "));
    } else {
        s.push_str(
            "      if [ \"$__atpkg_after\" = 1 ] && [ \"$__atpkg_at\" -lt \"$__atpkg_word_at\" ]; then\n        \
             __atpkg_pending=1\n      \
             else\n",
        );
        s.push_str(&fresh("        "));
        s.push_str("      fi\n");
    }
    s.push_str(
        "    else\n      \
         if [ \"$1\" = \"--\" ]; then __atpkg_stop=1; fi\n",
    );
    if after.is_empty() {
        s.push_str(
            "      set -- \"$@\" \"$1\"\n      \
             shift\n",
        );
    } else {
        s.push_str(
            "      set -- \"$@\" \"$1\"\n      \
             shift\n      \
             if [ \"$__atpkg_pending\" = 1 ] && [ \"$__atpkg_at\" = \"$__atpkg_word_at\" ]; then\n        \
             __atpkg_pending=0\n",
        );
        s.push_str(&fresh("          "));
        s.push_str("      fi\n");
    }
    s.push_str(
        "    fi\n  done\nelif [ \"$__atpkg_posture\" = 0 ] && [ \"$__atpkg_word_ok\" = 1 ]; then\n",
    );
    if !words {
        s.push_str("  __atpkg_say=");
        s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
        s.push_str("\n  set -- ");
        s.push_str(&spelled);
        s.push_str(" \"$@\"\n");
    } else {
        if skip_all.is_empty() {
            s.push_str("  __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push('\n');
        } else {
            s.push_str(
                "  if [ \"$__atpkg_have_word\" = 1 ]; then\n    case \"$__atpkg_word\" in\n      ",
            );
            s.push_str(
                &skip_all
                    .iter()
                    .map(|w| sh_quote_str(w))
                    .collect::<Vec<_>>()
                    .join("|"),
            );
            s.push_str(") ;;\n      *) __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push_str(" ;;\n    esac\n  else\n    __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push_str("\n  fi\n");
        }
        if after.is_empty() {
            s.push_str("  if [ -n \"$__atpkg_say\" ]; then\n    set -- ");
            s.push_str(&spelled);
            s.push_str(" \"$@\"\n  fi\n");
        } else {
            s.push_str(
                "  if [ -n \"$__atpkg_say\" ] && [ \"$__atpkg_after\" = 0 ]; then\n    \
                 set -- ",
            );
            s.push_str(&spelled);
            s.push_str(
                " \"$@\"\n  \
                 elif [ -n \"$__atpkg_say\" ]; then\n    \
                 __atpkg_n=$#\n    \
                 __atpkg_at=0\n    \
                 while [ \"$__atpkg_n\" -gt 0 ]; do\n      \
                 __atpkg_n=$((__atpkg_n - 1))\n      \
                 __atpkg_at=$((__atpkg_at + 1))\n      \
                 set -- \"$@\" \"$1\"\n      \
                 if [ \"$__atpkg_at\" = \"$__atpkg_word_at\" ]; then\n        \
                 set -- \"$@\" ",
            );
            s.push_str(&spelled);
            s.push_str(
                "\n      \
                 fi\n      \
                 shift\n    \
                 done\n  \
                 fi\n",
            );
        }
    }
    s.push_str(
        "fi\n\
         if [ -n \"$__atpkg_say\" ]; then\n  \
         printf '%s' \"$__atpkg_say\" 1>&2\n  \
         for __atpkg_a in \"$@\"; do\n    \
         printf \" '\" 1>&2\n    \
         __atpkg_rest=$__atpkg_a\n    \
         while case \"$__atpkg_rest\" in *\"'\"*) true ;; *) false ;; esac; do\n      \
         printf '%s' \"${__atpkg_rest%%\"'\"*}\" 1>&2\n      \
         printf '%s' \"'\\\\''\" 1>&2\n      \
         __atpkg_rest=${__atpkg_rest#*\"'\"}\n    \
         done\n    \
         printf '%s' \"$__atpkg_rest\" 1>&2\n    \
         printf \"'\" 1>&2\n  \
         done\n  \
         printf '\\n' 1>&2\n\
         fi\n",
    );
    s
}

/// [`sh_shim_content_routed`] with `prelude` (empty for every shim but an agent program's;
/// its `agents/` twin's and its `bin/` shim's [`twin_prelude`]) inserted right after the
/// header comment — ahead of the exports, so a hand-over to `atpkg __selfupdate` inherits
/// nothing the store `exec` would have set.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_content_twin(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
    prelude: &str,
) -> String {
    sh_shim_body(target, env, route, prelude)
}

/// The one body every Unix shim renders through — see [`sh_shim_content_routed`] for
/// the shape and [`sh_shim_content_twin`] for the prelude.
#[cfg(any(unix, test))]
fn sh_shim_body(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
    prelude: &str,
) -> String {
    let mut s = String::from(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n",
    );
    s.push_str(prelude);
    if !env.is_empty() {
        s.push_str(SH_SHIM_ENV_NOTE);
    }
    for (name, value) in env.entries() {
        s.push_str("export ");
        s.push_str(name);
        s.push('=');
        s.push_str(&sh_quote_str(value));
        s.push('\n');
    }
    // Decided here, in the one body every writer and every "already right" check renders.
    if cfg!(target_os = "macos")
        && let Some(block) = target
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(crate::build_cache::shim_block)
    {
        s.push_str(&block);
    }
    let target_q = sh_shim_quote(target);
    if let Some(route) = route {
        let marker = route
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| route.to_path_buf(), crate::compat::root_marker);
        let (r, m) = (sh_shim_quote(route), sh_shim_quote(&marker));
        s.push_str(SH_SHIM_ROUTE_NOTE);
        for (test, path) in [
            ("[ ! -h ", &r),
            ("[ -f ", &r),
            ("[ -x ", &r),
            ("[ -f ", &target_q),
            ("[ ! -h ", &m),
            ("[ -f ", &m),
        ] {
            s.push_str(test);
            s.push_str(path);
            s.push_str(" ] && ");
        }
        s.push_str("exec ");
        s.push_str(&r);
        s.push_str(" \"$@\"\n");
    }
    let target = target_q;
    s.push_str("exec ");
    s.push_str(&target);
    s.push_str(" \"$@\"\n");
    s
}

/// The environment a Unix shim written by [`sh_shim_content_env`] exports: its
/// `export NAME='VALUE'` lines, unquoted by the inverse of [`sh_quote_str`] and
/// re-admitted through the rule (fail-closed: a hand-edited line that breaks it reads
/// as NONE). A tombstone, a pending stub or a plain shim reads as NONE.
#[cfg(any(unix, test))]
pub(crate) fn parse_sh_shim_env(content: &str) -> crate::shim_env::ShimEnv {
    let mut raw: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("export ") else {
            continue;
        };
        let Some((name, quoted)) = rest.split_once("='") else {
            continue;
        };
        let Some(value) = quoted.strip_suffix('\'') else {
            continue;
        };
        let mut entry = String::from(name);
        entry.push('=');
        entry.push_str(&value.replace("'\\''", "'"));
        raw.push(entry);
    }
    crate::shim_env::ShimEnv::admit(&raw).unwrap_or_default()
}

/// Single-quote a path for the `sh` stub, escaping embedded quotes POSIX-style. A
/// managed-store path never contains one; this is fail-closed defence in depth, the
/// same posture the `.cmd` side takes.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_quote(target: &Path) -> String {
    sh_quote_str(&target.to_string_lossy())
}

/// [`sh_shim_quote`] over a string: the one quoting rule the shim body uses for its
/// target AND its exported values (and, on every platform, the self-update block's, and
/// the `ln -sfn` a replaced rustup link prints as its way back — `crate::seam`).
pub(crate) fn sh_quote_str(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The inverse of [`sh_shim_content`] — recover the target a shim execs, which
/// `active_builds`, `prune_stale_shims` and gc all key on. A TOMBSTONE (a failing notice
/// script with no `exec`) parses as `None`.
#[cfg(any(unix, test))]
pub(crate) fn parse_sh_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("exec '")
            && let Some(end) = rest.rfind("' \"$@\"")
        {
            return Some(PathBuf::from(rest[..end].replace("'\\''", "'")));
        }
    }
    None
}

/// Bound for reading a shim of either dialect before parsing it.
pub(crate) const MAX_SHIM_BYTES: usize = 64 * 1024;

/// Read the Windows `.cmd` shim through the bounded, symlink-refusing metadata seam
/// before parsing it. Compiled in Unix tests too.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_target(path: &Path) -> Option<PathBuf> {
    let content = crate::metadata_io::read_bounded_regular_utf8(path, MAX_SHIM_BYTES).ok()?;
    parse_cmd_shim_target(&content)
}

/// The environment the Windows `.cmd` shim at `path` exports ([`parse_cmd_shim_env`]),
/// read through the same bounded, symlink-refusing seam; NONE for anything unreadable.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_env(path: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(path, MAX_SHIM_BYTES) {
        Ok(content) => parse_cmd_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp's decision, against a stand-in launcher so it runs the same on every
    /// Unix: an absolute executable program runs behind `taskpolicy -c utility`; a
    /// relative, missing or non-executable one, or no launcher, leaves the program alone.
    #[cfg(unix)]
    #[test]
    fn only_an_absolute_executable_runs_behind_the_utility_clamp() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testdir::dir("atpkg-clamp-");
        let launcher = dir.join("taskpolicy");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "").unwrap();
        let argv = |program: &Path, launcher: &Path| -> Vec<String> {
            utility_clamped_with(program.as_os_str(), launcher)
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let l = launcher.to_string_lossy().into_owned();
        assert_eq!(
            argv(Path::new("/bin/sh"), &launcher),
            [l.as_str(), "-c", "utility", "/bin/sh"]
        );
        let missing = dir.join("no-such-tool");
        for (program, why) in [
            (Path::new("sh"), "a relative program"),
            (missing.as_path(), "a missing program"),
            (plain.as_path(), "a file with no execute bit"),
            (&*dir, "a directory"),
        ] {
            assert_eq!(
                argv(program, &launcher),
                [program.to_string_lossy().into_owned()],
                "{why} is started bare, so it fails as itself"
            );
        }
        assert_eq!(
            argv(Path::new("/bin/sh"), &dir.join("no-launcher")),
            ["/bin/sh"],
            "no launcher, no clamp"
        );
    }

    /// The live contract: a program [`spawn_detached`] starts runs at utility (`ps` pri 20
    /// or below), not the default band (31) the session lane's passes got bare; one
    /// [`spawn_detached_waited`] starts (a first run's) runs at the default band.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_detached_program_starts_at_utility_and_a_waited_one_at_the_default_band() {
        type Spawn =
            fn(&std::ffi::OsStr, &[std::ffi::OsString], Option<(&str, &str)>) -> io::Result<()>;
        let pri_of = |spawn: Spawn| -> i32 {
            let dir = crate::testdir::dir("atpkg-detached-band-");
            let said = dir.join("pri");
            let script = format!(
                "/bin/ps -o pri= -p $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'",
                said.display()
            );
            spawn("/bin/sh".as_ref(), &["-c".into(), script.into()], None).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let pri = loop {
                if let Ok(text) = std::fs::read_to_string(&said) {
                    break text;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the detached program never ran"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            pri.trim()
                .parse()
                .unwrap_or_else(|_| panic!("no priority in {pri:?}"))
        };
        let pri = pri_of(spawn_detached);
        assert!(
            pri <= 20,
            "a detached pass ran at pri {pri}, not utility (20)"
        );
        let pri = pri_of(spawn_detached_waited);
        assert!(
            pri > 20,
            "a first run's detached pass ran at pri {pri}, below the default band (31)"
        );
    }

    /// Observed at creation, before any write or chmod: the temp is private from the
    /// instant it exists, and an existing one is refused.
    #[cfg(unix)]
    #[test]
    fn a_private_temp_is_never_created_wider_than_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testdir::dir("atpkg-private-new-");
        let path = dir.join(".zshrc.atpkg-1.tmp");
        let f = create_private_new(&path).unwrap();
        assert_eq!(
            f.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the temp must be private from the instant it exists"
        );
        drop(f);
        assert!(
            create_private_new(&path).is_err(),
            "exclusive: an existing temp is never opened and truncated"
        );
    }

    #[test]
    fn cmd_shim_content_wraps_target_and_forwards_args() {
        let c = cmd_shim_content(Path::new("C:\\store\\ay\\18\\bin\\ay.exe"));
        // The frame first, then the forward line, which ends the batch on the SAME line:
        // nothing is read after the program returns, however the file was re-laid
        // meanwhile.
        let mut want = cmd_frame();
        want.push_str("@\"C:\\store\\ay\\18\\bin\\ay.exe\" %* & @exit /b\r\n");
        assert_eq!(c, want);
    }

    #[test]
    fn cmd_shim_round_trips_through_parse() {
        let target = PathBuf::from("C:\\store\\ay\\18\\bin\\ay.exe");
        let content = cmd_shim_content(&target);
        assert_eq!(parse_cmd_shim_target(&content), Some(target));
    }

    #[test]
    fn tombstone_content_has_no_forward_target_and_exits_70() {
        let c = cmd_tombstone_content("atpkg: ay was yanked/revoked — run `aterm pkg update`");
        assert!(c.contains("1>&2"), "notice goes to stderr: {c}");
        assert!(c.contains("exit /b 70"), "exits 70: {c}");
        // cmd.exe reads the file in the console's OEM code page: the em dash goes.
        assert!(c.is_ascii(), "a .cmd tombstone is ASCII: {c}");
        // A tombstone must NOT parse as an installed shim (mirrors read_link Err on Unix).
        assert_eq!(parse_cmd_shim_target(&c), None);
    }

    #[test]
    fn cmd_echo_escape_neutralizes_metacharacters() {
        // A crafted tool name with cmd metacharacters must be inert inside echo.
        let esc = cmd_echo_escape("a&b|c>d<e^f%g\"h");
        assert_eq!(esc, "a^&b^|c^>d^<e^^f%%g^\"h");
    }

    /// THE WINDOWS WRAPPER WITH AN ENVIRONMENT (design S7), exact: one `@set "NAME=VALUE"`
    /// per entry, then the unchanged forward line; the target still parses off the
    /// forward line, the env parses back off the `@set` lines, an empty env is the plain
    /// shim byte for byte, and the injection guard refuses what the manifest rule refuses.
    #[test]
    fn cmd_shim_with_env_sets_then_forwards_and_round_trips() {
        let target = PathBuf::from("C:\\store\\claude\\2026082701\\bin\\claude.exe");
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=two words".to_string(),
        ])
        .unwrap();
        let c = cmd_shim_content_env(&target, &env);
        let mut want = cmd_frame();
        want.push_str(
            "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"B=two words\"\r\n\
             @\"C:\\store\\claude\\2026082701\\bin\\claude.exe\" %* & @exit /b\r\n",
        );
        assert_eq!(c, want);
        assert_eq!(parse_cmd_shim_target(&c), Some(target.clone()));
        assert_eq!(parse_cmd_shim_env(&c), env);
        assert_eq!(
            cmd_shim_content_env(&target, &crate::shim_env::ShimEnv::NONE),
            cmd_shim_content(&target),
            "no env: the shim every manifest without the key gets"
        );
        assert_eq!(
            parse_cmd_shim_env(&cmd_shim_content(&target)),
            crate::shim_env::ShimEnv::NONE
        );
        // A tombstone carries no env either.
        assert_eq!(
            parse_cmd_shim_env(&cmd_tombstone_content("atpkg: x was yanked")),
            crate::shim_env::ShimEnv::NONE
        );
        // A hand-edited `@set` the rule refuses reads as NONE, never as half an env.
        assert_eq!(
            parse_cmd_shim_env(
                "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"PATH=C:\\x\"\r\n@\"C:\\a.exe\" %* & @exit /b\r\n"
            ),
            crate::shim_env::ShimEnv::NONE
        );
        assert!(cmd_env_is_injection_safe(&env));
        assert!(cmd_env_is_injection_safe(&crate::shim_env::ShimEnv::NONE));
        // The reader goes through the bounded seam: a real file round-trips.
        let root = crate::testdir::dir("atpkg-cmd-shim-env-");
        let path = root.join("claude.cmd");
        std::fs::write(&path, &c).unwrap();
        assert_eq!(read_cmd_shim_env(&path), env);
        assert_eq!(read_cmd_shim_target(&path), Some(target));
        assert_eq!(
            read_cmd_shim_env(&root.join("absent.cmd")),
            crate::shim_env::ShimEnv::NONE
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// On a real Windows host the backend writes, resolves and reads the env-carrying
    /// `.cmd` through its I/O primitives — the same file the pure test above pins.
    #[cfg(windows)]
    #[test]
    fn windows_backend_lays_and_reads_an_env_shim() {
        let root = crate::testdir::dir("atpkg-win-env-shim-");
        let target = root.join("claude.exe");
        std::fs::write(&target, b"x").unwrap();
        let shim = root.join("claude.cmd");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        install_shim_to_env(&shim, &target, &env).unwrap();
        assert_eq!(
            std::fs::read_to_string(&shim).unwrap(),
            cmd_shim_content_env(&target, &env)
        );
        assert_eq!(resolve_shim(&shim), Some(target.clone()));
        assert_eq!(shim_env_of(&shim), env);
        // Re-laid without an env: the plain shim, and nothing left of the exports.
        install_shim_to(&shim, &target).unwrap();
        assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
        assert_eq!(resolve_shim(&shim), Some(target));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The target reader keys on the forward line's exact tail — the current one and the
    /// legacy ` %*`-only one (read until its next re-lay, so `which`, the sweeps and gc
    /// keep resolving it) — and nothing else: a tail with a code, a `%1`, a bare quoted
    /// program, garbage.
    #[test]
    fn parse_cmd_shim_target_keys_on_the_forward_tail_and_reads_the_legacy_one() {
        assert_eq!(parse_cmd_shim_target("not a shim\r\n"), None);
        assert_eq!(parse_cmd_shim_target(""), None);
        let target = PathBuf::from("C:\\a.exe");
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\" %* & @exit /b\r\n"),
            Some(target.clone())
        );
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\" %*\r\n"),
            Some(target),
            "a legacy shim still resolves until it is re-laid"
        );
        for not_a_forward in [
            "@\"C:\\a.exe\"\r\n",
            "@\"C:\\a.exe\" %1\r\n",
            "@\"C:\\a.exe\" %* & @exit /b 3\r\n",
            "@\"C:\\a.exe\" %* & exit /b\r\n",
            "@\"C:\\a.exe\" %*& @exit /b\r\n",
            "@\"C:\\a.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n",
        ] {
            assert_eq!(
                parse_cmd_shim_target(not_a_forward),
                None,
                "{not_a_forward:?}"
            );
        }
    }

    /// A LEGACY `.cmd` AGENTS TWIN — laid with the landing prelude — still resolves to the
    /// store: its prelude's text, exact (the bytes on disk
    /// the readers must keep walking past until the next pass re-lays the twin), over a
    /// prefix with a space and a `)` in it; the hand-over line is `@"<atpkg>" __landing …`,
    /// not a `@"<target>" %* & @exit /b` forward, so the target parser still resolves the
    /// twin to the STORE target and the env parser reads the twin's own exports; every line
    /// that runs a program ends the batch on that SAME line, so the re-lay that retires it
    /// is safe while it runs. RENDERED TEXT ONLY: no Windows box ran this.
    #[test]
    fn a_legacy_cmd_twin_with_the_landing_prelude_still_resolves_to_the_store() {
        // Literal `\` paths: what `Layout` joins on Windows (`Path::join` on this Mac
        // would put a `/` in, which is not what a Windows twin carries).
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
        assert_eq!(
            prelude,
            "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
             waits for it and then runs the new one (aterm help pkg).\r\n\
             @if not exist \"C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude\" goto store\r\n\
             @if not exist \"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" goto store\r\n\
             @\"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" __landing \"claude\" \
             \"C:\\Program Files (x86)\\aterm\\pkg\" -- %* & @exit /b\r\n\
             :store\r\n"
        );
        // ONE stat on the embedded atpkg: a second guard would open a window between two
        // stats where the file vanishes and the batch exits 0 with nothing run.
        assert_eq!(prelude.matches("atpkg.exe").count(), 2);
        let twin = cmd_shim_content_twin(target, &env, &prelude);
        // The frame ahead of the prelude: a legacy twin, executing
        // as it is re-laid, resumes inside the padding and returns, never in the prelude.
        let mut want = cmd_frame();
        want.push_str(&prelude);
        want.push_str("@set \"DISABLE_AUTOUPDATER=1\"\r\n");
        want.push_str(
            "@\"C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe\" \
             %* & @exit /b\r\n",
        );
        assert_eq!(twin, want);
        // cmd.exe rules the prelude is built to: no `( … )` block anywhere, every line
        // silent (`@` or a label), CRLF throughout, no `call`, no `where atpkg`; and
        // every line that runs a program ends the batch ON THAT LINE — `cmd` resumes a
        // batch file at a byte offset after each line, and the twin is re-laid while it
        // may be running, so no exit line of its own, and never `%errorlevel%` (expanded
        // when the line is parsed, before the program ran).
        for line in twin.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(
                line.starts_with('@') || line.starts_with(':'),
                "silent: {line}"
            );
            assert!(
                !line.ends_with('(') && !line.starts_with(')'),
                "no block: {line}"
            );
            if line.starts_with("@\"") {
                assert!(
                    line.ends_with(CMD_FORWARD_TAIL),
                    "ends the batch on the line that runs the program: {line}"
                );
            } else {
                // The frame's own `@exit /b` is the ONE exit line of its own: it closes
                // the padding a legacy resume lands in, and a fresh run jumps past it.
                assert!(
                    !line.contains("exit /b") || line == CMD_PADDING_EXIT.trim_end(),
                    "no exit line of its own past the frame's: {line}"
                );
            }
            assert!(!line.starts_with("@call"), "no call: {line}");
        }
        assert!(!twin.contains("errorlevel"), "{twin}");
        assert_eq!(
            twin.matches(" & @exit /b").count(),
            2,
            "the hand-over and the forward, each ending the batch: {twin}"
        );
        assert_eq!(
            twin.matches("\r\n@exit /b\r\n").count(),
            1,
            "the frame's padding exit, the one exit line of its own: {twin}"
        );
        assert!(!twin.contains('\n') || twin.matches("\r\n").count() == twin.matches('\n').count());
        assert!(!twin.contains("where atpkg"));
        // No `\"` anywhere: to the launched exe's argv parser it is an escaped quote,
        // and the operand would fuse with what follows it.
        assert!(!twin.contains("\\\""), "{twin}");
        // Every reader of the twin: the target parser walks past the hand-over line (its
        // closing quote is followed by ` __landing`, not the forward tail) to the real
        // forward, the env parser reads the twin's own exports.
        assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
        assert_eq!(parse_cmd_shim_env(&twin), env);
        assert_eq!(
            parse_cmd_shim_target(
                "@\"C:\\app\\atpkg.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n"
            ),
            None,
            "the hand-over line alone is no forward line"
        );
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\"\r\n"),
            None,
            "a quoted program with no forward tail is not the shim's forward"
        );
        // The verbatim spelling `canonicalize` answers on Windows loses its prefix before
        // any path is embedded in a `.cmd` (the pending stub's too).
        assert_eq!(
            strip_verbatim_prefix(Path::new("\\\\?\\UNC\\srv\\share\\aterm\\atpkg.exe")),
            PathBuf::from("\\\\srv\\share\\aterm\\atpkg.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("\\\\?\\C:\\aterm\\atpkg.exe")),
            PathBuf::from("C:\\aterm\\atpkg.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("/usr/local/aterm/atpkg")),
            PathBuf::from("/usr/local/aterm/atpkg")
        );
        // The prelude is ahead of the exports and the forward is the last line.
        assert!(twin.find("__landing").unwrap() < twin.find("@set ").unwrap());
        assert!(twin.trim_end().ends_with("\" %* & @exit /b"));
        // The bin/ shim carries none of it; an empty prelude is the plain shim.
        assert!(!cmd_shim_content_env(target, &env).contains("__landing"));
        assert_eq!(
            cmd_shim_content_twin(target, &env, ""),
            cmd_shim_content_env(target, &env)
        );
        // Through the bounded file reader, the same answers.
        let root = crate::testdir::dir("atpkg-cmd-twin-");
        let path = root.join("claude.cmd");
        std::fs::write(&path, &twin).unwrap();
        assert_eq!(read_cmd_shim_target(&path), Some(target.to_path_buf()));
        assert_eq!(read_cmd_shim_env(&path), env);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// THE `.cmd` TWIN RENDERS NO PRELUDE: [`twin_prelude`]'s Windows arm is empty, so the
    /// `.cmd` twin is the framed `.cmd` shim byte for byte: no `%~1`, no `__selfupdate`, no
    /// `__landing`, one forward ending its batch on its own line. The renderer is called
    /// directly, cfg-free, so this pins the Windows bytes from the Unix suite.
    #[test]
    fn the_cmd_twin_renders_no_prelude_and_is_the_plain_framed_shim() {
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        if cfg!(windows) {
            assert_eq!(
                twin_prelude(
                    "claude",
                    prefix,
                    Some(atpkg),
                    &["update", "upgrade", "install"],
                    &crate::yolo::policy_of("claude")
                ),
                "",
                "on Windows the twin carries no prelude"
            );
        }
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let twin = cmd_shim_content_twin(target, &env, "");
        assert_eq!(
            twin,
            cmd_shim_content_env(target, &env),
            "the plain framed shim"
        );
        assert_eq!(twin.matches(" & @exit /b").count(), 1, "{twin}");
        assert_eq!(twin.matches("\r\n@exit /b\r\n").count(), 1, "{twin}");
        assert!(!twin.contains("%~1"), "{twin}");
        assert!(!twin.contains(crate::selfupdate::HIDDEN_VERB), "{twin}");
        assert!(!twin.contains(crate::landing::HIDDEN_VERB), "{twin}");
        assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
        assert_eq!(parse_cmd_shim_env(&twin), env);
    }

    /// THE RESUME SIMULATION: three legacy files — the plain twin, an env-carrying `bin/`
    /// shim, and the LONGEST shim the legacy writer's caps allow —
    /// re-laid to their framed successors while executing. At EVERY offset `0..=old_len`
    /// of the new file: offset 0 is the fresh run (`@goto :main`); no line of the old
    /// file can end inside the head's non-inert bytes (`cmd` resumes only after a line
    /// end of the OLD file, and its shortest line is 12 bytes); every other offset reads
    /// a colon label, an empty line or `@exit /b`. The end-of-file resume — the one a
    /// running program's exit actually produces — lands on a colon label. As the control,
    /// the same old twin over the successor laid BARE (no frame) resumes
    /// into the prelude. A simulation of `cmd`'s documented rules, not a run on Windows.
    #[test]
    fn a_legacy_cmd_re_laid_while_executing_resumes_into_the_frame_at_every_offset() {
        fn legacy(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
            let mut s = String::new();
            for (n, v) in env.entries() {
                s.push_str("@set \"");
                s.push_str(n);
                s.push('=');
                s.push_str(v);
                s.push_str("\"\r\n");
            }
            s.push_str("@\"");
            s.push_str(&target.to_string_lossy());
            s.push('"');
            s.push_str(CMD_LEGACY_FORWARD_TAIL);
            s.push_str("\r\n");
            s
        }
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        let none = crate::shim_env::ShimEnv::NONE;
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=two words".to_string(),
        ])
        .unwrap();
        let longest_raw: Vec<String> = (0..crate::shim_env::MAX_SHIM_ENV)
            .map(|i| format!("V{i}={}", "x".repeat(crate::shim_env::MAX_ENTRY_BYTES - 3)))
            .collect();
        let longest_env = crate::shim_env::ShimEnv::admit(&longest_raw).unwrap();
        let longest_target = PathBuf::from(format!(
            "C:\\{}\\claude.exe",
            "p".repeat(CMD_LEGACY_TARGET_BOUND_BYTES - "C:\\\\claude.exe".len())
        ));
        assert_eq!(
            longest_target.to_string_lossy().len(),
            CMD_LEGACY_TARGET_BOUND_BYTES
        );
        let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
        assert!(!prelude.is_empty());
        let cases = [
            (
                "legacy plain twin -> framed twin behind the prelude",
                legacy(target, &none),
                cmd_shim_content_twin(target, &none, &prelude),
            ),
            (
                "legacy env shim -> framed env shim",
                legacy(target, &env),
                cmd_shim_content_env(target, &env),
            ),
            (
                "longest legacy shim -> its framed successor",
                legacy(&longest_target, &longest_env),
                cmd_shim_content_env(&longest_target, &longest_env),
            ),
        ];
        assert_eq!(
            cases[2].1.len(),
            CMD_LEGACY_FILE_BOUND_BYTES,
            "the longest case IS the derived bound"
        );
        for (what, old, new) in &cases {
            assert_eq!(
                parse_cmd_shim_target(old),
                parse_cmd_shim_target(new),
                "{what}"
            );
            assert_eq!(parse_cmd_shim_env(old), parse_cmd_shim_env(new), "{what}");
            let old = old.as_bytes();
            let new = new.as_bytes();
            assert!(
                old.len() <= CMD_PADDING_END_BYTES,
                "{what}: {} bytes",
                old.len()
            );
            // Where `cmd` can be in the old file: after each of its line ends.
            let ends: Vec<usize> = old
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1)
                .collect();
            assert_eq!(ends.last().copied(), Some(old.len()), "{what}");
            // No old line is shorter than the legacy writer's minimum, so no old line
            // ends inside the head.
            assert!(
                ends.iter().all(|&e| e >= CMD_LEGACY_MIN_LINE_BYTES),
                "{what}: {ends:?}"
            );
            for offset in 0..=old.len() {
                let line = cmd_resumed_line(new, offset);
                if offset == 0 {
                    assert_eq!(
                        line.as_deref(),
                        Some("@goto :main"),
                        "{what}: the fresh run"
                    );
                    continue;
                }
                if offset < CMD_FRAME_HEAD.len() - 2 {
                    // The head's own bytes: not inert (`goto :main` would run the body
                    // again), and not a place `cmd` can resume — no legacy line is that
                    // short, so no old line ends here.
                    assert!(
                        !ends.contains(&offset),
                        "{what}: an old line ends at {offset}"
                    );
                    continue;
                }
                assert!(
                    cmd_resumed_line_is_inert(line.as_deref()),
                    "{what}: a resume at offset {offset} of {} reads {line:?}",
                    old.len()
                );
            }
            // The resume a running program's exit actually produces: the old file's
            // end-of-file offset, a colon label in the new one — never the prelude, never
            // a forward line.
            let at_eof = cmd_resumed_line(new, old.len()).unwrap();
            assert!(
                !at_eof.is_empty() && at_eof.bytes().all(|b| b == b':'),
                "{what}: {at_eof:?}"
            );
            // Every old line end, the general resume set, reads inert.
            for end in &ends {
                assert!(cmd_resumed_line_is_inert(
                    cmd_resumed_line(new, *end).as_deref()
                ));
            }
        }
        // The padding by itself, whatever the old file: every offset from the head's own
        // CRLF to the padding exit is inert, and the exit line begins exactly where the
        // padding ends — the bound keeps a legacy end-of-file at or before it.
        let framed = cmd_shim_content(target);
        for offset in CMD_FRAME_HEAD.len() - 2..=CMD_PADDING_END_BYTES {
            let line = cmd_resumed_line(framed.as_bytes(), offset);
            assert!(
                cmd_resumed_line_is_inert(line.as_deref()),
                "{offset}: {line:?}"
            );
        }
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), CMD_PADDING_END_BYTES).as_deref(),
            Some("@exit /b")
        );
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), CMD_FRAME_HEAD.len() - 2).as_deref(),
            Some(""),
            "the head's CRLF reads empty"
        );
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), 1).as_deref(),
            Some("goto :main"),
            "why the head must sit where no old line can end"
        );
        // A legacy file exactly at the bound ends on the padding exit's first byte and
        // reads `@exit /b`; nothing the old writer could lay reaches past it.
        let padding_end = framed.find("@exit /b").unwrap();
        assert!(cases[2].1.len() <= padding_end, "{}", cases[2].1.len());
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), cases[2].1.len()).as_deref(),
            Some(&":".repeat(78)[(cases[2].1.len() - CMD_FRAME_HEAD.len()) % 80..]),
            "the bound's own end-of-file offset, inside the padding"
        );
        // THE CONTROL: the same old twin over its successor laid BARE — prelude first, no
        // frame — resumes inside the prelude: the window the frame closes.
        let mut bare = prelude.clone();
        bare.push_str(&cmd_shim_body_env(target, &none));
        let old_len = cases[0].1.len();
        let line = cmd_resumed_line(bare.as_bytes(), old_len).unwrap();
        assert!(
            !cmd_resumed_line_is_inert(Some(&line)),
            "laid bare, the resume lands in the prelude: {line:?}"
        );
        // The steady state needs no simulation: a framed file's program line ends the
        // batch on that line, so a re-lay over it is never read at all.
        assert!(framed.trim_end().ends_with(CMD_FORWARD_TAIL));
    }

    #[test]
    fn cmd_target_injection_guard_rejects_quote_percent_newline() {
        // A managed store path is always safe.
        assert!(cmd_target_is_injection_safe(Path::new(
            "C:\\Users\\me\\AppData\\Local\\aterm\\pkg\\store\\ay\\18\\bin\\ay.exe"
        )));
        assert!(cmd_target_is_injection_safe(Path::new(
            "/managed/store/ay/18/bin/ay"
        )));
        // Anything that would break out of `@"<target>" %*` is refused.
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\\"&calc.exe\"\\ay.exe"
        )));
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\%APPDATA%\\ay.exe"
        )));
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\ay.exe\r\n@calc"
        )));
    }

    #[test]
    fn cmd_shim_reader_rejects_sparse_oversize() {
        let root = crate::testdir::dir("atpkg-cmd-shim-sparse-");
        let path = root.join("ay.cmd");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_SHIM_BYTES + 1) as u64).unwrap();
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cmd_shim_reader_rejects_fifo_and_symlink_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = crate::testdir::dir("atpkg-cmd-shim-special-");
        let path = root.join("ay.cmd");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_file(&path).unwrap();
        let target = root.join("target.cmd");
        std::fs::write(&target, cmd_shim_content(Path::new("C:\\ay.exe"))).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod sh_shim_tests {
    use super::*;

    /// Trust's `targo` authenticates its own `current_exe` as a plain, already-canonical
    /// regular file, so the stub must BE a regular file whose `exec` hands off to the real
    /// path.
    #[test]
    fn the_sh_shim_execs_the_real_path_and_round_trips() {
        let target = Path::new("/prefix/store/trust/5520/bin/targo");
        let body = sh_shim_content(target);
        assert!(body.starts_with("#!/bin/sh\n"), "{body}");
        assert!(
            body.contains("exec '/prefix/store/trust/5520/bin/targo' \"$@\""),
            "the stub must EXEC (replacing the process image) so the tool authenticates \
             at its real path, and forward args verbatim: {body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    }

    /// A tombstone has no `exec` line, so it resolves to nothing; `active_builds` and gc
    /// depend on this.
    #[test]
    fn a_tombstone_is_not_an_installed_shim() {
        let notice = "#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n";
        assert_eq!(parse_sh_shim_target(notice), None);
    }

    /// THE WRAPPER WITH AN ENVIRONMENT (design S7): one
    /// `export NAME='VALUE'` per entry, then the unchanged `exec` line. The target still
    /// parses off the exec line (every sweep keyed on where a shim resolves is
    /// unchanged), the env parses back off the exports, an empty env is the plain shim
    /// byte for byte, and a quote in a value is POSIX-escaped both ways.
    #[test]
    fn the_sh_shim_with_env_exports_then_execs_and_round_trips() {
        let target = Path::new("/prefix/store/claude/2026082701/bin/claude");
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=it's two words".to_string(),
        ])
        .unwrap();
        let body = sh_shim_content_env(target, &env);
        assert!(
            body.contains(
                "\nexport DISABLE_AUTOUPDATER='1'\n\
                 export B='it'\\''s two words'\n\
                 exec '/prefix/store/claude/2026082701/bin/claude' \"$@\"\n"
            ),
            "{body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), env);
        assert_eq!(
            sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE),
            sh_shim_content(target),
            "no env: the shim every manifest without the key gets, byte for byte"
        );
        assert_eq!(
            parse_sh_shim_env(&sh_shim_content(target)),
            crate::shim_env::ShimEnv::NONE
        );
        // A tombstone and a hand-edited export the rule refuses both read as NONE.
        assert_eq!(
            parse_sh_shim_env("#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n"),
            crate::shim_env::ShimEnv::NONE
        );
        assert_eq!(
            parse_sh_shim_env(
                "#!/bin/sh\nexport DISABLE_AUTOUPDATER='1'\nexport PATH='/x'\nexec '/a' \"$@\"\n"
            ),
            crate::shim_env::ShimEnv::NONE
        );
    }

    /// The exports REACH the exec'd program: the backend lays the wrapper, the target is a
    /// script that prints the variable, and running the shim prints the value — while a
    /// plain re-lay of the same shim prints nothing. `resolve_shim` and `shim_env_of` read
    /// both back.
    #[cfg(unix)]
    #[test]
    fn the_laid_wrapper_exports_into_the_exec_d_program() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-env-shim-");
        let target = root.join("claude");
        std::fs::write(
            &target,
            b"#!/bin/sh\nprintf '%s|%s\\n' \"${DISABLE_AUTOUPDATER:-unset}\" \"$1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shim = root.join("shim");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        install_shim_to_env(&shim, &target, &env).unwrap();
        assert_eq!(resolve_shim(&shim).as_deref(), Some(target.as_path()));
        assert_eq!(shim_env_of(&shim), env);
        // The test's own process may run under a managed agent shim that already
        // exports DISABLE_AUTOUPDATER=1 (an aterm-spawned Claude Code session does);
        // the child must see only what the shim under test lays, so scrub the
        // ambient copy from both invocations.
        // Each shim was just laid: past the `ETXTBSY` a sibling test's fork can hold it in.
        let run_shim = || {
            aterm_spec::wait::past_text_busy(|| {
                std::process::Command::new(&shim)
                    .arg("arg")
                    .env_remove("DISABLE_AUTOUPDATER")
                    .output()
            })
            .unwrap()
        };
        let out = run_shim();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1|arg\n");
        // The plain shim: no export, the program sees nothing, and nothing is read back.
        install_shim_to(&shim, &target).unwrap();
        assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
        let out = run_shim();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unset|arg\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Defence in depth: a path containing a quote cannot break out of the stub.
    #[test]
    fn an_embedded_quote_cannot_escape_the_stub() {
        let nasty = Path::new("/prefix/store/o'ny/1/bin/x");
        let body = sh_shim_content(nasty);
        assert!(body.contains(r"'\''"), "quote is POSIX-escaped: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(nasty));
    }

    /// THE ROUTED SHIM: the exports ahead of the guard (so both execs inherit them), and
    /// the unchanged store `exec` line. `None` is
    /// the plain shim byte for byte; the target and the env parse back off the routed form
    /// exactly as off the plain one, because the guard line starts with `[`.
    #[test]
    fn the_routed_shim_is_exact_and_parses_as_the_plain_one() {
        let target = Path::new("/p/store/trust/8595/bin/tippy");
        let route = Path::new("/p/compat/trust/8595/bin/tippy");
        let none = crate::shim_env::ShimEnv::NONE;
        let body = sh_shim_content_routed(target, &none, Some(route));
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), none);
        assert_eq!(
            sh_shim_content_routed(target, &none, None),
            sh_shim_content(target),
            "no route: today's shim, byte for byte"
        );

        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let body = sh_shim_content_routed(target, &env, Some(route));
        assert!(
            body.find("export DISABLE_AUTOUPDATER='1'").unwrap()
                < body.find("exec '/p/compat/").unwrap(),
            "both execs inherit the exports: {body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), env);
        assert_eq!(
            sh_shim_content_routed(target, &env, None),
            sh_shim_content_env(target, &env)
        );
        // A tombstone-shaped or stub-shaped reader keyed on the FIRST `exec '` line is the
        // contract; the guard's own `exec 'R'` sits mid-line and is never that line.
        assert_eq!(
            body.lines()
                .filter(|l| l.trim_start().starts_with("exec '"))
                .count(),
            1
        );
    }

    /// A quote in the prefix is POSIX-escaped in every quoted slot — the route three times
    /// in the tests and once in the exec, the target once in its `-f` test and once in its
    /// own `exec`, the marker twice — and the target still parses back whole.
    #[test]
    fn a_quote_in_the_prefix_is_escaped_in_every_slot_of_the_guard() {
        let target = Path::new("/it's/store/trust/1/bin/targo");
        let route = Path::new("/it's/compat/trust/1/bin/targo");
        let body = sh_shim_content_routed(target, &crate::shim_env::ShimEnv::NONE, Some(route));
        assert_eq!(
            body.matches(r"'/it'\''s/compat/trust/1/bin/targo'").count(),
            4
        );
        assert_eq!(
            body.matches(r"'/it'\''s/store/trust/1/bin/targo'").count(),
            2
        );
        // The marker is `compat::root_marker`'s host join of the root (`\` on Windows).
        let marker = if cfg!(windows) {
            r"'/it'\''s/compat/trust/1\.atpkg-root'"
        } else {
            r"'/it'\''s/compat/trust/1/.atpkg-root'"
        };
        assert_eq!(body.matches(marker).count(), 2);
        assert!(!body.contains("/it's/"), "no raw quote survives: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    }

    /// THE GUARD UNDER A REAL SHELL. The rendered shim is run by `/bin/sh` (bash 3.2 in
    /// POSIX mode on macOS) and by `/bin/dash` where present, with an empty environment and
    /// arguments carrying a space and both quote kinds, under a prefix whose path holds a
    /// space and a quote. Store and route are the same script, which prints the path it
    /// was exec'd as (`$0`), so the output says which one ran: the route when the root's
    /// marker stands; with no marker, a symlinked marker or route, a non-executable route, or
    /// a missing one, the store path. A hard link is a regular file too and the shell cannot
    /// tell it from a clone — that is `route_for_shim` and repair's job.
    #[cfg(unix)]
    #[test]
    fn the_guard_runs_the_root_only_when_the_marked_root_stands() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = crate::testdir::dir("atpkg-sh-route-");
        let base = scratch.join("pre fix's");
        let store_bin = base.join("store").join("trust").join("8595").join("bin");
        let root_bin = base.join("compat").join("trust").join("8595").join("bin");
        std::fs::create_dir_all(&store_bin).unwrap();
        std::fs::create_dir_all(&root_bin).unwrap();
        std::fs::create_dir_all(base.join("bin")).unwrap();
        let store = store_bin.join("tippy");
        let route = root_bin.join("tippy");
        std::fs::write(
            &store,
            b"#!/bin/sh\nprintf '%s|%s|%s|%s|%s\\n' \"$0\" \"$#\" \"$1\" \"$2\" \
              \"${DISABLE_AUTOUPDATER:-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let shim = base.join("bin").join("tippy");
        std::fs::write(&shim, sh_shim_content_routed(&store, &env, Some(&route))).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let shells: Vec<&str> = ["/bin/sh", "/bin/dash"]
            .into_iter()
            .filter(|s| Path::new(s).is_file())
            .collect();
        assert!(shells.contains(&"/bin/sh"), "every Unix has /bin/sh");
        let run = |shell: &str| -> String {
            let out = std::process::Command::new(shell)
                .arg(&shim)
                .arg("a b")
                .arg("it's \"q\"")
                .env_clear()
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let ran = |path: &Path| format!("{}|2|a b|it's \"q\"|1\n", path.display());

        let marker = crate::compat::root_marker(&base.join("compat").join("trust").join("8595"));
        for (what, marked, runs_route) in [
            ("clone", true, true),
            ("byte copy", true, true),
            ("clone", false, false),
            ("clone behind a symlinked marker", false, false),
            ("symlink", true, false),
            ("not executable", true, false),
            ("missing", true, false),
        ] {
            let _ = std::fs::remove_file(&route);
            let _ = std::fs::remove_file(&marker);
            match what {
                "clone" | "clone behind a symlinked marker" => {
                    crate::clone::clone_file(&store, &route).unwrap();
                }
                "byte copy" => std::fs::copy(&store, &route).map(|_| ()).unwrap(),
                "symlink" => std::os::unix::fs::symlink(&store, &route).unwrap(),
                "not executable" => {
                    std::fs::copy(&store, &route).unwrap();
                    std::fs::set_permissions(&route, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                _ => {}
            }
            if marked {
                std::fs::write(&marker, b"atpkg exec root v1\n").unwrap();
            } else if what == "clone behind a symlinked marker" {
                std::os::unix::fs::symlink(&store, &marker).unwrap();
            }
            let expected = if runs_route { &route } else { &store };
            for shell in &shells {
                assert_eq!(
                    run(shell),
                    ran(expected),
                    "{shell}, route is a {what}, marker {marked}"
                );
            }
        }
        // A missing ROOT (not only a missing file in it) falls back the same way.
        std::fs::remove_dir_all(base.join("compat")).unwrap();
        for shell in &shells {
            assert_eq!(run(shell), ran(&store), "{shell}, no root");
        }
        std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
    }

    /// THE SELF-UPDATE BLOCK ([`crate::selfupdate`]), for claude (three
    /// verbs) and codex (one): the `case "$1"` on the row's verbs, the repeated
    /// `__atpkg=` assignment, the variable `exec` with the program and the prefix quoted
    /// and `"$@"` verbatim, `;;`, `esac`. Empty for an empty verb list, for a verb that
    /// would break the `case` pattern, for a program with a quote in it and for a name the
    /// tool-name gate refuses; a quote in the prefix or the atpkg path is POSIX-escaped in
    /// every slot. On Unix [`twin_prelude`] IS this block, and the whole twin still reads
    /// as a plain shim:
    /// exactly one literal `exec '` line (the store target), no `export `, the target and
    /// the env parse back, no pending-stub marker, the block ahead of the exports, and the
    /// plain `bin/` render carries none of it (the agents reconcile lays an agent program's
    /// `bin/` with the block, `crate::activate::reconcile_agents`).
    #[test]
    fn the_self_update_block_is_exact_is_the_whole_prelude_and_breaks_no_reader() {
        let prefix = Path::new("/Users//u/Library/Application Support/aterm/pkg");
        let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
        let target = Path::new(
            "/Users//u/Library/Application Support/aterm/pkg/store/claude/2026091902/bin/claude",
        );
        let verbs = crate::selfupdate::verbs_of("claude");
        let block = sh_selfupdate_prelude("claude", prefix, atpkg, verbs);
        for code in [
            "\ncase \"$1\" in\n  update|upgrade|install)\n",
            "\n    __atpkg='/Applications/aterm.app/Contents/MacOS/atpkg'\n",
            "__ATPKG_SELFUPDATE_PID=$$ exec \"$__atpkg\" __selfupdate 'claude' \
             '/Users//u/Library/Application Support/aterm/pkg' -- \"$@\"; fi\n    ;;\nesac\n",
        ] {
            assert!(block.contains(code), "{code:?} in {block}");
        }
        let codex =
            sh_selfupdate_prelude("codex", prefix, atpkg, crate::selfupdate::verbs_of("codex"));
        assert!(codex.contains("\n  update)\n"), "{codex}");
        assert!(codex.contains("__selfupdate 'codex' "), "{codex}");
        // Fail-closed: no verbs, an unsafe verb, an unsafe program, a refused tool name.
        assert_eq!(sh_selfupdate_prelude("claude", prefix, atpkg, &[]), "");
        assert_eq!(
            sh_selfupdate_prelude("ay", prefix, atpkg, crate::selfupdate::verbs_of("ay")),
            "",
            "a program with no row has no verbs and no block"
        );
        assert_eq!(
            sh_selfupdate_prelude("muse", prefix, atpkg, crate::selfupdate::verbs_of("muse")),
            "",
            "a rostered program with no updater has no block"
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "up|date)"]),
            ""
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["Update"]),
            ""
        );
        // `esac` passes the regex and breaks the pattern (measured in sh, bash, dash, zsh,
        // ksh: a parse error before any exec); refused per verb, so it is
        // refused in every slot, first or not.
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["esac"]),
            ""
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "esac"]),
            ""
        );
        assert_eq!(sh_selfupdate_prelude("cl'aude", prefix, atpkg, verbs), "");
        assert_eq!(sh_selfupdate_prelude("cl aude", prefix, atpkg, verbs), "");
        assert_eq!(
            sh_selfupdate_prelude("sudo", prefix, atpkg, verbs),
            "",
            "not a tool name"
        );
        // A quote in the prefix and in the atpkg path is escaped in every slot.
        let quoted = sh_selfupdate_prelude(
            "claude",
            Path::new("/it's/pkg"),
            Path::new("/it's/app/atpkg"),
            verbs,
        );
        assert!(quoted.contains("__atpkg='/it'\\''s/app/atpkg'"), "{quoted}");
        assert!(
            quoted.contains("__selfupdate 'claude' '/it'\\''s/pkg' -- \"$@\"; fi"),
            "{quoted}"
        );
        assert!(!quoted.contains("/it's/"), "{quoted}");
        // The whole prelude is the block: no landing check, no marker, no `__landing`.
        let want = block.clone();
        let twin_pre = twin_prelude(
            "claude",
            prefix,
            Some(atpkg),
            verbs,
            &crate::yolo::Policy::NONE,
        );
        if cfg!(windows) {
            assert_eq!(twin_pre, "", "the .cmd twin renders no prelude");
        } else {
            assert_eq!(twin_pre, want);
        }
        assert!(!want.contains(crate::landing::HIDDEN_VERB), "{want}");
        assert!(!want.contains("landing"), "{want}");
        assert_eq!(
            twin_prelude(
                "claude",
                prefix,
                Some(atpkg),
                &[],
                &crate::yolo::Policy::NONE
            ),
            "",
            "no verbs, no yolo policy: no prelude — the twin is the plain shim"
        );
        // The full twin, read by every parser as before.
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let twin = sh_shim_content_twin(target, &env, None, &want);
        assert_eq!(
            twin.lines()
                .filter(|l| l.trim().starts_with("exec '"))
                .count(),
            1,
            "{twin}"
        );
        assert!(
            !want.lines().any(|l| l.trim().starts_with("export ")),
            "{want}"
        );
        assert_eq!(parse_sh_shim_target(&twin).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&twin), env);
        assert!(!twin.contains("pending-program stub"), "{twin}");
        assert!(
            twin.find("case \"$1\"").unwrap() < twin.find("export DISABLE_AUTOUPDATER").unwrap(),
            "{twin}"
        );
        assert!(twin.trim_end().ends_with("\"$@\""));
        assert!(!sh_shim_content_env(target, &env).contains("__selfupdate"));
        assert_eq!(
            sh_shim_content_twin(target, &env, None, ""),
            sh_shim_content_env(target, &env)
        );
    }

    /// THE YOLO BLOCK EXECUTED: laid twins over a store build that prints its
    /// argv. By default the flags are injected (behind muse's session words),
    /// by the flag they replace `--yolo` without ever doubling, and a launch
    /// that decided its own posture passes untouched and unprinted — the same
    /// argv [`crate::yolo::decide`] returns and the same lines its echo
    /// builders spell; stdout carries only the build's own words.
    #[cfg(unix)]
    #[test]
    fn the_yolo_block_expands_prints_and_reaches_the_exec() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-yolo-twin-");
        let target = root.join("store-build");
        std::fs::write(&target, b"#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prefix = root.join("pkg");
        let lay = |program: &str| {
            let prelude = twin_prelude(
                program,
                &prefix,
                None,
                &[],
                &crate::yolo::policy_of(program),
            );
            assert!(
                !prelude.lines().any(|l| l.trim().starts_with("exec '")),
                "{prelude}"
            );
            assert!(
                !prelude.lines().any(|l| l.trim().starts_with("export ")),
                "{prelude}"
            );
            assert!(!prelude.contains("__selfupdate"), "{prelude}");
            let twin = root.join(program);
            std::fs::write(
                &twin,
                sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
            )
            .unwrap();
            std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));
            twin
        };
        let run = |twin: &Path, args: &[&str]| {
            let out = aterm_spec::wait::past_text_busy(|| {
                std::process::Command::new(twin).args(args).output()
            })
            .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        let claude = lay("claude");
        // By default: injected at the front, the default echo, stdout clean.
        assert_eq!(
            run(&claude, &["-p", "hello"]),
            (
                "<--dangerously-skip-permissions><-p><hello>\n".into(),
                "aterm: yolo (default): running 'claude' \
                 '--dangerously-skip-permissions' '-p' 'hello'\n"
                    .into()
            )
        );
        // A decided posture, and the flag itself typed: as typed, silent.
        for args in [
            &["--permission-mode", "plan"][..],
            &["--permission-mode=auto"][..],
            &["--dangerously-skip-permissions", "-p", "x"][..],
        ] {
            let (stdout, stderr) = run(&claude, args);
            assert_eq!(stderr, "", "{args:?}");
            assert!(!stdout.contains("aterm:"), "{args:?}: {stdout}");
        }
        assert_eq!(
            run(&claude, &["--permission-mode", "plan"]).0,
            "<--permission-mode><plan>\n"
        );
        // Attached and config spellings are the same decided posture in the
        // generated shell as in `decide`: no injection and no echo.
        let codex = lay("codex");
        for args in [
            &["-sread-only", "exec", "x"][..],
            &["-a=never", "exec", "x"][..],
            &["-c", "sandbox_mode=\"read-only\"", "exec", "x"][..],
            &["resume", "thread-id"][..],
        ] {
            let (stdout, stderr) = run(&codex, args);
            assert_eq!(stderr, "", "{args:?}");
            assert!(
                !stdout.contains("--dangerously-bypass-approvals-and-sandbox"),
                "{args:?}: {stdout}"
            );
        }
        // By the flag: in place, the flag's echo. A prompt with quote/$/backtick.
        let (stdout, stderr) = run(
            &claude,
            &["--model", "opus", "--yolo", "-p", "it's $x `y`", ""],
        );
        assert_eq!(
            stdout,
            "<--model><opus><--dangerously-skip-permissions><-p><it's $x `y`><>\n"
        );
        let typed: Vec<String> = ["--model", "opus", "--yolo", "-p", "it's $x `y`", ""]
            .iter()
            .map(|w| (*w).to_string())
            .collect();
        let crate::yolo::Launch::Flag(expanded) = crate::yolo::decide("claude", &typed) else {
            panic!("flag launch");
        };
        assert_eq!(
            stderr,
            format!("{}\n", crate::yolo::echo_line("claude", &expanded))
        );
        // The separator freezes the rest; the default still lands in front.
        let (stdout, stderr) = run(&claude, &["--", "--yolo"]);
        assert_eq!(stdout, "<--dangerously-skip-permissions><--><--yolo>\n");
        assert_eq!(
            stderr,
            "aterm: yolo (default): running 'claude' '--dangerously-skip-permissions' \
             '--' '--yolo'\n"
        );
        // muse: behind its session words, never on its tool words.
        let muse = lay("muse");
        let behind = "<exec><--disable-approval><--disable-sandbox><--trust-workspace>\
                      <--disable-request-user-input><x>\n";
        assert_eq!(run(&muse, &["exec", "x"]).0, behind);
        assert_eq!(run(&muse, &["--yolo", "exec", "x"]).0, behind);
        assert_eq!(run(&muse, &["exec", "--yolo", "x"]).0, behind);
        assert_eq!(
            run(&muse, &["skills", "list"]),
            ("<skills><list>\n".into(), String::new())
        );
        assert_eq!(
            run(&muse, &["--approval-mode", "never"]),
            ("<--approval-mode><never>\n".into(), String::new())
        );
        let (stdout, stderr) = run(&muse, &[]);
        assert_eq!(
            stdout,
            "<--disable-approval><--disable-sandbox><--trust-workspace>\
             <--disable-request-user-input>\n"
        );
        assert_eq!(
            stderr,
            format!(
                "{}\n",
                crate::yolo::default_echo_line(
                    "muse",
                    &crate::yolo::flags_of("muse")
                        .iter()
                        .map(|f| (*f).to_string())
                        .collect::<Vec<_>>()
                )
            )
        );
        // Leading option values cannot hide a resume/fork from the default,
        // and an unknown arity cannot authorize guessing one.
        for args in [
            &["--model", "opus", "resume", "thread-id"][..],
            &["-m", "opus", "fork", "thread-id"][..],
            &["--model=opus", "resume", "thread-id"][..],
            &["-mopus", "fork", "thread-id"][..],
            &["--cd", "/owned/path", "resume", "thread-id"][..],
            &["--new-option", "value", "resume", "thread-id"][..],
            &["--image", "one.png", "two.png", "resume", "thread-id"][..],
        ] {
            assert_eq!(
                run(&codex, args),
                (
                    format!(
                        "{}\n",
                        args.iter().map(|a| format!("<{a}>")).collect::<String>()
                    ),
                    String::new()
                ),
                "{args:?}"
            );
        }
        assert_eq!(
            run(&codex, &["--model", "resume", "fix this"]).0,
            "<--dangerously-bypass-approvals-and-sandbox><--model><resume><fix this>\n"
        );
        // The model's value spells the command too: insertion uses the
        // original argument position, never the first equal string.
        for args in [
            &["--model", "exec", "exec", "x"][..],
            &["--yolo", "--model", "exec", "exec", "x"][..],
            &["--model", "exec", "--yolo", "exec", "x"][..],
        ] {
            assert_eq!(
                run(&muse, args).0,
                "<--model><exec><exec><--disable-approval><--disable-sandbox>\
                 <--trust-workspace><--disable-request-user-input><x>\n",
                "{args:?}"
            );
        }
        assert_eq!(
            run(&muse, &["--new-option", "value", "--yolo", "exec", "x"]),
            (
                "<--new-option><value><--yolo><exec><x>\n".into(),
                String::new()
            )
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The yolo renderer's fail-closed rows, and its exact first line — the note a
    /// person reading the twin sees.
    #[test]
    fn the_yolo_block_renders_fail_closed() {
        use crate::yolo::Policy;
        let ok = Policy {
            flags: &["--ok"],
            markers: &["--ok"],
            ..Policy::NONE
        };
        assert_eq!(sh_yolo_prelude("claude", &Policy::NONE, &[]), "");
        assert_eq!(
            sh_yolo_prelude("claude", &Policy { markers: &[], ..ok }, &[]),
            "",
            "no markers: the default cannot tell a decided launch"
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    flags: &["--ok", "not-a-flag"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    markers: &["--yo lo"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    after: &["ex'ec"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(sh_yolo_prelude("Claude", &ok, &[]), "");
        assert_eq!(sh_yolo_prelude("../claude", &ok, &[]), "");
        let block = sh_yolo_prelude("muse", &crate::yolo::policy_of("muse"), &[]);
        let note = block.lines().next().unwrap();
        assert_eq!(
            note,
            "# atpkg: yolo by default — this name runs muse with every approval \
             and sandbox off ('--disable-approval' '--disable-sandbox' \
             '--trust-workspace' '--disable-request-user-input'); `--yolo` ahead \
             of a literal `--` spells the same, a launch that types its own \
             posture runs as typed, and the command as run is printed on stderr \
             (aterm help pkg)."
        );
        assert!(block.contains(crate::yolo::ECHO_PREFIX), "{block}");
        assert!(block.contains(crate::yolo::DEFAULT_ECHO_PREFIX), "{block}");
    }
}
