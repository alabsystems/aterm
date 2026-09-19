// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OS-primitive abstraction (§10) — the ONE place atpkg's platform-specific
//! filesystem, activation, disk-query, and process-exec edges live, behind a
//! single portable API with a per-OS backend ([`unix`]/[`windows`]).
//!
//! The **Unix backend is the crate's original behavior, moved verbatim** — every
//! call the rest of the crate makes through `platform::` lowers to exactly the
//! symlink / `chmod 0600` / `statvfs` / `getuid` / `execve` it did before, so a
//! Unix build is byte-for-byte behavior-identical.
//!
//! The **Windows backend** is the honest analogue of each primitive:
//!
//! * **Activation** — the `channels/<ch>/current` indirection is a directory
//!   **junction** (`mklink /J`, no admin required, unlike a symlink), not a POSIX
//!   symlink. [`atomic_symlink`] creates it (used for `current` and the sysroot
//!   sysroot/toolchain dir links).
//! * **Bin shims** — a `bin/<tool>.cmd` batch wrapper (`@"<target>.exe" %* & @exit /b`,
//!   [`CMD_FORWARD_TAIL`], behind the resume-proof frame [`CMD_FRAME_HEAD`] every `.cmd`
//!   this crate writes starts with since 2026-09-18), not a symlink into the store.
//!   [`install_shim`] writes it, [`install_tombstone_shim`] writes the failing
//!   (`exit /b 70`) variant, and [`resolve_shim`] reads a shim's target back (parsing the
//!   `.cmd`) — the inverse of the Unix `read_link`.
//! * **Private state** — [`ensure_private_dir`]/[`harden_file`]/[`write`-side mode]
//!   rely on the per-user `%LOCALAPPDATA%` profile ACL (POSIX mode/owner bits have no
//!   analogue): [`harden_file`]/[`set_mode`] are no-ops, [`dir_meta_is_private`] is a
//!   best-effort `true`, [`file_mode`](permission_mode)/[`our_uid`] report the
//!   not-applicable sentinel.
//! * **Disk** — [`volume_free_bytes`] calls `GetDiskFreeSpaceExW` (dependency-free
//!   manual FFI) instead of `statvfs`; both fail **OPEN** (`None` on any error).
//! * **Spotlight** — the index query ([`spotlight_query`],
//!   [`spotlight_index_state`]) is macOS only and `None` everywhere else, Windows
//!   Search having no per-directory opt-out for [`crate::noindex`] to honour or to
//!   measure. `None` means the question could not be ASKED, never "not indexed".
//! * **Exec** — [`exec_or_run`] `spawn().wait()` + `process::exit` (Windows has no
//!   `execve`) instead of replacing the process image.
//!
//! `ensure_private_dir` and the advisory [`FileLock`] are **delegated to
//! `aterm_update_core` on both platforms** — that shared crate already carries a
//! correct, reviewed cross-platform implementation of each (its Unix path is the
//! updater's own hardening, its Windows path a per-user-ACL `create_dir_all` +
//! `share_mode(0)` lock), so delegating avoids any drift with the macOS updater
//! while still giving atpkg a working Windows implementation.

use std::io;
use std::path::Path;
// Unconditional now: the Unix shim parser returns a `PathBuf` too, so this is no
// longer a Windows/test-only need.
use std::path::PathBuf;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// The cross-platform private-directory creator: delegated to
/// [`aterm_update_core::ensure_private_dir`] on **both** platforms (Unix hardens to
/// `0700`/owned-by-uid, Windows `create_dir_all` under the per-user profile ACL and
/// confirms the final component is a real directory). Owned here so every atpkg call
/// site reads `platform::ensure_private_dir` and cannot drift from the shared crate.
pub use aterm_update_core::ensure_private_dir;

/// The advisory exclusive file lock: [`aterm_update_core::FileLock`] on both
/// platforms (Unix `flock(LOCK_EX)`, Windows `share_mode(0)` with bounded retry).
pub use aterm_update_core::FileLock;

/// Acquire the advisory exclusive [`FileLock`] on `path`, creating it if absent.
/// Thin, portable wrapper so call sites name `platform::file_lock`.
pub fn file_lock(path: &Path) -> io::Result<FileLock> {
    FileLock::acquire(path)
}

/// Install a `bin/` shim for `tool` pointing at that tool's executable inside a build's
/// `bin/` directory (`build_bin_dir`). `shim` is the concrete shim path — build it with
/// [`crate::store::Layout::shim`], never by joining the tool name yourself.
///
/// Both renderings of the name are needed here and they are DIFFERENT files on Windows: the
/// shim is `bin/<tool>.cmd` ([`crate::store::ToolName::shim_file`], supplied by the caller as
/// `shim`) and its target is `<build>/bin/<tool>.exe`
/// ([`crate::store::ToolName::exe_file`], derived here). Taking a `ToolName` rather than a
/// `&str` is what keeps that pair from collapsing into one string again.
///
/// * **Unix**: a symlink `shim -> build_bin_dir/<tool>` (atomic temp-symlink + rename).
/// * **Windows**: a `bin/<tool>.cmd` batch wrapper invoking `build_bin_dir\<tool>.exe`.
pub fn install_shim(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
) -> io::Result<()> {
    install_shim_to(shim, &build_bin_dir.join(tool.exe_file()))
}

/// [`install_shim`] whose wrapper also EXPORTS `env` before it execs the target (design
/// S7, [`crate::shim_env`]): `export NAME='VALUE'` lines ahead of the `exec` on Unix,
/// `@set "NAME=VALUE"` lines ahead of the `@"<target>" %*` on Windows. With an empty
/// `env` the shim is byte-identical to [`install_shim`]'s. Temp + rename like every shim.
pub fn install_shim_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    install_shim_to_env(shim, &build_bin_dir.join(tool.exe_file()), env)
}

/// What a volume's Spotlight index does with new files ([`spotlight_index_state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// Indexing on: a planted file shows up.
    Enabled,
    /// Switched off (`mdutil -i off`, or "Indexing and searching disabled"): a setting.
    Disabled,
    /// "Index is read-only." — mds holds the index while the volume is low on space
    /// (measured 2026-09-16): it answers searches and takes no new entries until the
    /// hold lifts. Not a setting, and not a reason to leave build output unmigrated.
    ReadOnly,
}

/// [`install_shim_to`] with the exported `env` — the `(shim path, target)` form of
/// [`install_shim_env`], for the callers that already hold the target path.
pub fn install_shim_to(shim: &Path, target: &Path) -> io::Result<()> {
    install_shim_to_env(shim, target, &crate::shim_env::ShimEnv::NONE)
}

/// The shim [`install_shim_env`] would lay — the same target derivation, the same body
/// — RENDERED but not written, for the callers that lay a whole pass of shims in ONE
/// go through [`crate::lay::lay_executables`] (one untracked launchd job when this
/// process is provenance-tracked, instead of one per file).
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
// These carry the Windows shim CONTENT logic but are pure string functions, so
// they are compiled (and unit-tested) on every platform via `cfg(any(windows,
// test))` — the Windows backend calls them for I/O, and the Unix test build
// exercises them directly, keeping the correct-by-construction Windows format
// covered by the (Unix-run) test suite. They are compiled OUT of a non-test Unix
// build (nothing there calls them), so they raise no dead-code lint. The one
// exception is the frame ([`CMD_FRAME_HEAD`] and its pieces, [`cmd_framed`]): the
// pending stub's `.cmd` renderer in `crate::stub` picks its platform at RUNTIME
// (`cfg!(windows)`), so it is compiled on every platform and the frame it lays its
// stub behind must be too (review finding, 2026-09-18: a stub laid unframed was the
// one `.cmd` this crate wrote without it).
// ---------------------------------------------------------------------------

/// THE TAIL OF EVERY `.cmd` FORWARD LINE after the target's closing quote (2026-09-17):
/// `@"<target>" %* & @exit /b` — the program runs with the arguments verbatim, and the
/// SAME parsed line ends the batch. `cmd.exe` parses a line whole before it runs any of
/// it, and executes a batch file by re-reading it at a remembered BYTE OFFSET after every
/// line — so a shim re-laid (temp + remove + rename, `windows.rs`'s `atomic_write`) while
/// its program is running is never read again once the program returns: whatever stood
/// after the line in the OLD file is not looked for in the NEW one. A bare `exit /b`
/// returns with the ERRORLEVEL the program left, which is what `cmd /c` exits with;
/// `exit /b %errorlevel%` on the same line would be expanded when the line is PARSED,
/// before the program ran. The one string the renderers ([`cmd_shim_content_env`],
/// [`cmd_landing_prelude`]) write and the reader ([`parse_cmd_shim_target`]) keys on.
/// A file from BEFORE this tail IS read again after its program returns — at the old
/// file's end offset — which is what the frame ([`CMD_FRAME_HEAD`], 2026-09-18) makes
/// harmless. Written to the documented `cmd` rules; no Windows host has run it.
pub(crate) const CMD_FORWARD_TAIL: &str = " %* & @exit /b";

/// The tail a `.cmd` shim from BEFORE 2026-09-17 carries: `@"<target>" %*` alone, on its
/// last line. Still READ ([`parse_cmd_shim_target`]) until every such shim is re-laid — a
/// `bin/` shim or alias by its program's next install, update or `repair`, an `agents/`
/// twin by the next pass's `reconcile_agents` — so `which`, `doctor`, the sweeps and gc
/// keep resolving them meanwhile. Never written again. Its ONE re-lay to the framed shape
/// ([`CMD_FRAME_HEAD`]) is what the frame's padding is sized for.
pub(crate) const CMD_LEGACY_FORWARD_TAIL: &str = " %*";

/// THE FIRST LINE OF EVERY `.cmd` FILE THIS CRATE WRITES (2026-09-18) — a bin shim, an
/// alias, a tombstone, the `agents/` twin, and the pending stub (`crate::stub`, laid into
/// the same `bin\` and `agents\` slots ahead of an install): `@goto :main`, jumping over the padding
/// ([`CMD_PADDING_LINE`] × [`CMD_PADDING_LINES`]) and the [`CMD_PADDING_EXIT`] to the
/// [`CMD_MAIN_LABEL`] the real body starts at.
///
/// **Why a frame** (closing the one window 2026-09-17 documented and could not close):
/// `cmd.exe` executes a batch file by re-opening it and seeking to a remembered BYTE
/// OFFSET after every line it runs. A `.cmd` from before 2026-09-17 ends on `@"<target>"
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
/// under [`MAX_CMD_SHIM_BYTES`] by an order of magnitude with the longest body on top.
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

/// The LONGEST `.cmd` the pre-2026-09-17 writer could have laid, derived from that
/// writer's own caps: [`crate::shim_env::MAX_SHIM_ENV`] `@set "NAME=VALUE"` lines of at
/// most [`crate::shim_env::MAX_ENTRY_BYTES`] each, then `@"<target>" %*` with a target of
/// at most [`CMD_LEGACY_TARGET_BOUND_BYTES`] — 8 × 265 + 788 = 2908 bytes. (A twin from
/// before 2026-09-17 was the plain shim under the twin's name, so it is covered by the
/// same bound; a `bin/` shim never carried a prelude.) [`CMD_PADDING_END_BYTES`] ≥ this,
/// with 1185 bytes to spare.
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
/// `cmd`, and was never a line this writer laid; review finding, 2026-09-18). The
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
/// stub's (`crate::stub::stub_content_cmd_with`), the one `.cmd` writer outside this
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
/// [`cmd_shim_content_env`], which the backend now writes through; kept for the tests
/// that pin the plain shape.
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
/// REFUSES an unsafe target rather than emit an injectable `.cmd`. Compiled on every
/// platform: [`cmd_landing_prelude`] (rendered everywhere through [`landing_prelude`])
/// guards its four paths with it.
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
#[cfg(any(windows, test))]
fn cmd_echo_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%%"),
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
/// and nothing else (or, for a shim laid before 2026-09-17 and not yet re-laid,
/// [`CMD_LEGACY_FORWARD_TAIL`]) — not on any line that starts with `@"`: the `agents/`
/// twin's landing prelude ([`cmd_landing_prelude`]) runs the embedded `atpkg` as
/// `@"<atpkg>" __landing "<program>" "<prefix>" -- %* & @exit /b`, whose closing quote is
/// followed by ` __landing`, so the one reader of the target walks past it to the twin's
/// real forward. (The first cut kept the hand-over off `@"` with a second `if exist` on
/// the same path instead — a stat that, when the file vanished between the two, skipped
/// the hand-over and exited 0 with the tool never run; review finding, 2026-09-17.) The
/// frame's lines ([`CMD_FRAME_HEAD`], the colon labels, `@exit /b`, `:main`; 2026-09-18)
/// start with neither `@"` nor `@set "`, so this reader and [`parse_cmd_shim_env`] walk
/// past them the same way and answer the same over a framed file and a legacy one.
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
/// → `\\srv\sh` — so it can sit inside a `.cmd` line (2026-09-17). `std::fs::canonicalize`
/// answers a verbatim path on Windows (it goes through `GetFinalPathNameByHandleW`), and
/// that is what [`crate::stub::embedded_atpkg_path`] embeds: `cmd.exe`'s built-ins and
/// its command launch do not reliably accept the `\\?\` spelling (`if exist` answering
/// false would `goto store` on every run and the wait would silently never happen; a
/// launch refused would strand the tool behind `exit /b`). Pure string work — a path
/// without the prefix comes back unchanged — so it is rendered and pinned on every
/// platform; on Unix no path ever carries the prefix.
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
/// twin's [`cmd_landing_prelude`]) ahead of the `@set` lines and the forward line, the
/// whole behind the same frame ([`CMD_FRAME_HEAD`]) — the `.cmd` twin of
/// [`sh_shim_content_twin`]. The prelude ends on its `:store` label, so its `goto store`
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

/// The note ahead of the `.cmd` landing prelude — the `rem` twin of [`SH_LANDING_NOTE`].
/// No backtick, no `%`, no `^`: a `rem` line is still parsed for `%` expansion.
const CMD_LANDING_NOTE: &str = "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
     waits for it and then runs the new one (aterm help pkg).\r\n";

/// The label the prelude jumps to when there is nothing to wait for: the line right
/// ahead of the twin's own `@set` exports and `@"<target>" %* & @exit /b` forward.
const CMD_LANDING_LABEL: &str = "store";

/// THE `.cmd` LANDING PRELUDE of an `agents/` twin (2026-09-17, closing residual R4 of
/// [`crate::landing`]): the Windows twin of [`sh_landing_prelude`], designed for
/// `cmd.exe` line by line —
///
/// ```text
/// @rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing waits for it and then runs the new one (aterm help pkg).
/// @if not exist "<marker>" goto store
/// @if not exist "<atpkg>" goto store
/// @"<atpkg>" __landing "<program>" "<prefix>" -- %* & @exit /b
/// :store
/// ```
///
/// * ONE `if exist` on the marker — the `stat` the Unix `[ -f ]` is — and `goto` past
///   the whole prelude when it is absent, which is every run outside an update pass.
/// * A `goto`/label shape, NO PARENTHESISED BLOCK: inside `if … ( … )` a `)` in a quoted
///   path — `C:\Program Files (x86)\…` — closes the block early; on a single-line `if`
///   the quoted path is one token whatever it contains.
/// * The hand-over runs the embedded co-located `atpkg` DIRECTLY (a `.exe`, so no `call`
///   — `call` re-parses the line and a `%` in the user's arguments would be expanded a
///   second time), forwarding `%*` verbatim, after ONE `if exist` on it. Its closing
///   quote is followed by ` __landing`, never by the forward tail, which is the shape
///   [`parse_cmd_shim_target`] keys on — so `resolve_shim`, `sweep_agents_dir`'s
///   keep-predicate and every reader of the target still answer the store target off the
///   twin's real forward line. (The first cut prefixed this line with a SECOND `if exist`
///   on the same path so no line started with `@"`; an `atpkg.exe` replaced between the
///   two stats skipped the hand-over and exited 0 with the tool never run — review
///   finding, 2026-09-17. Now a launch that fails in that window is `cmd`'s own `9009`
///   with its message, the same class of ending the Unix twin's failed `exec` has.) It
///   falls through to the store forward when the embedded `atpkg` is gone — never
///   `where atpkg`: an older `atpkg` on PATH answers `__landing` with exit 2 `unknown
///   verb`, and the tool would never run (the Unix rule, review 2026-09-16).
/// * **The batch ends on the line that runs the program** — `… %* & @exit /b` on the
///   hand-over and on the twin's own forward ([`CMD_FORWARD_TAIL`]), never an exit line
///   of its own (review, 2026-09-17). `cmd.exe` executes a batch file by re-reading it at
///   a remembered BYTE OFFSET after every line, and the twin is RE-LAID while it may be
///   executing: `reconcile_agents` re-lays a twin whose bytes changed, and activation
///   re-lays `bin/` and the twin while a landing marker stands — the very moment a
///   `claude` is inside the hand-over. The first cut's `@exit /b %errorlevel%` on a line
///   of its own was reached by offset: a re-lay from an `atpkg` whose path is a different
///   length (a relocated app, a per-user install beside a system one) moved that offset
///   into the middle of some other line of the new file — a fragment run as a command,
///   then `:store`, then the tool a SECOND time. A line is parsed whole before any of it
///   runs and nothing is read after `exit /b`, so the steady state — a twin of THIS shape
///   re-laid while it runs, in the hand-over or in the forward — is closed. A bare
///   `exit /b` returns with the ERRORLEVEL the program left, which is what `cmd /c` exits
///   with; `exit /b %errorlevel%` on the same line would be expanded when the line is
///   PARSED, before the program ran — the rule that had put the exit on its own line.
///   **What the same-line exit could not close, and the frame does** (2026-09-18): a
///   twin from BEFORE 2026-09-17 — `@"<target>" %*` alone on its last line, the plain
///   shim under the twin's name, which every Windows twin was until then — that is
///   executing at the moment of its ONE re-lay. When its agent exits, `cmd` resumes at
///   the OLD file's end-of-file offset inside the NEW file; laid bare behind this
///   prelude, that offset landed in the prelude, and with the marker gone, `goto store`
///   ran the agent a SECOND time with the same arguments. Every `.cmd` this crate writes
///   now starts with the frame ([`CMD_FRAME_HEAD`]): `@goto :main`, 4080 bytes of
///   colon-only label lines, `@exit /b`, `:main`, and only then this prelude and the
///   body — so that resume, at any offset a legacy file can end at
///   ([`CMD_LEGACY_FILE_BOUND_BYTES`], from the old writer's own caps), reads a label,
///   an empty line or the bare `@exit /b`, prints nothing and returns with the agent's
///   own ERRORLEVEL. Closed by construction; the rendered text and a simulation of the
///   resume at every offset of a legacy file are pinned by the tests; `cmd.exe`'s reading
///   of it is unverified on a Windows host like all of this. (The gaps between two
///   consecutive line reads of the prelude carry the same offset hazard, microseconds
///   wide, as every batch file rewritten in place does; the program's whole run was the
///   window that lasted minutes.)
/// * `@set` lines: none — [`parse_cmd_shim_env`] reads only those, so the exports it
///   reads off the twin are the twin's own, laid after the label.
/// * Every path is embedded with its VERBATIM prefix taken off ([`strip_verbatim_prefix`])
///   — the embedded `atpkg` comes from `canonicalize`, which spells `\\?\C:\…` on
///   Windows, a spelling `cmd`'s `if exist` and its command launch do not reliably accept
///   — and its TRAILING SEPARATORS trimmed ([`cmd_embedded_path`]): a prefix configured
///   as `C:\…\pkg\` rendered `"C:\…\pkg\" -- %*`, and `\"` is an escaped quote to the
///   launched exe's argv parser (the `CommandLineToArgvW` rule), so the prefix operand
///   fused with the user's arguments and the tool never ran (review, 2026-09-17). A path
///   that is nothing but separators, or a bare drive once trimmed (`C:\` → `C:`, which
///   `cmd` reads relative to that drive's current directory), renders the EMPTY prelude.
///
/// Fail-closed like every `.cmd` body this crate writes: when the marker, the `atpkg`
/// path, the prefix or the program could break out of `"…"` or trigger `%` expansion
/// ([`cmd_target_is_injection_safe`]), or a path cannot be embedded at all, the prelude
/// is EMPTY — the twin is the plain shim and runs the store build with no wait, never an
/// injectable batch line and never a stranded tool. Pure string building, rendered and
/// unit-tested on every platform.
///
/// **No Windows host has run this.** The rendered text is pinned by tests on this
/// crate's Unix suite; the runtime behaviour (`cmd.exe`'s `goto`, `%*`, the same-line
/// `exit /b` and the ERRORLEVEL it returns with, the offset rule above) is written to
/// the documented `cmd` rules and is UNVERIFIED on Windows, like the rest of [`windows`].
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

/// A path as the `.cmd` landing prelude embeds it, or `None` when it cannot be
/// (2026-09-17): the verbatim prefix off ([`strip_verbatim_prefix`]), trailing `\` and
/// `/` trimmed — a quoted operand ending in `\"` is an escaped quote to the launched
/// exe's argv parser, and `if exist` wants none either — then the injection guard
/// ([`cmd_target_is_injection_safe`]). `None` for a path that is only separators, for a
/// bare drive once trimmed (`C:` is drive-relative to `cmd`), and for one the guard
/// refuses; the caller renders no prelude for any of them.
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

/// THE LANDING PRELUDE of the `agents/` twin this platform lays — what
/// [`crate::activate::reconcile_agents`] renders and hands to [`install_twin_to_env`] /
/// [`twin_executable_to_env`]: the `sh` prelude ([`sh_landing_prelude`]) on Unix, the
/// `.cmd` prelude ([`cmd_landing_prelude`]) on Windows. Both renderers are pure and
/// compiled everywhere; only the dispatch is per platform.
#[must_use]
pub(crate) fn landing_prelude(program: &str, prefix: &Path, marker: &Path, atpkg: &Path) -> String {
    if cfg!(windows) {
        cmd_landing_prelude(program, prefix, marker, atpkg)
    } else {
        sh_landing_prelude(program, prefix, marker, atpkg)
    }
}

/// The Unix `bin/` shim body: a `/bin/sh` stub that EXECs the store binary.
///
/// # Why this is not a symlink any more
///
/// It was, and that silently broke the product's headline tool on every install.
/// Trust's `targo` authenticates its own frontend before doing anything: it requires
/// `current_exe` to be a plain regular file (`protected Targo frontends cannot be
/// symlinks or reparse points`) and, in `validate_unprivileged_authority_path`, that
/// the path already equal its own `canonicalize()`. A symlinked shim fails both.
/// Reproduced directly: `targo --version` through a symlink gives
/// "could not authenticate Cargo/Targo frontend identity", while the same binary at
/// its real path prints its version.
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
/// The no-environment form of [`sh_shim_content_env`], which the backend now writes
/// through; kept for the tests that pin the plain shape.
#[cfg(test)]
pub(crate) fn sh_shim_content(target: &Path) -> String {
    sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// The marker comment ahead of the `export` lines of a shim that carries an
/// environment — a human reading `bin/claude` sees where the variables come from.
#[cfg(any(unix, test))]
const SH_SHIM_ENV_NOTE: &str =
    "# shim_env from the signed manifest: only this managed copy runs with it.\n";

/// [`sh_shim_content`] with the shim's exported environment ahead of the `exec`
/// (design S7, [`crate::shim_env`]): the note, then one `export NAME='VALUE'` per entry
/// — the value single-quoted by [`sh_shim_quote`]'s rule, so nothing in it is ever a
/// word to `sh` — then the unchanged `exec '<target>' "$@"`. An empty `env` is
/// byte-identical to [`sh_shim_content`]: every shim laid before the key existed.
///
/// `exec` keeps everything [`sh_shim_content`] promises (the tool authenticates at its
/// real path; argv[0] is the target) — the exports are inherited by the exec'd image,
/// which is the whole mechanism. `parse_sh_shim_target` reads the target off the exec
/// line as before, so every sweep keyed on where a shim resolves is unchanged.
///
/// The unrouted form of [`sh_shim_content_routed`], which the backend writes through;
/// kept for the tests that pin the shape every shim not routed through an exec root has.
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
/// byte for byte — every shim of every build that does not need an exec root, which is
/// every build Trust publishes from 2026-09-14 on. With `route = Some(R)`, the target
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
/// [`crate::compat`] lays a copy-on-write CLONE of the build ([`crate::clone`]) where
/// `rustc` holds `trustc`'s bytes, and the shim runs the tool from there. Run from that
/// tree, `tippy` and `targo tippy` lint (measured 2026-09-16 on bundle 8595).
///
/// THE GUARD checks, at every exec, that the root stands and is whole: `R` is a regular
/// executable file and not a symlink (every Trust frontend refuses a symlinked sibling
/// anyway), the store file `S` it stands for still exists (a build reclaimed from the
/// store never runs from a leftover root), and the root's marker `M` — written last,
/// before the root was committed by `rename(2)` — is a regular file and not a symlink (a
/// root half-way through a lay or a rebuild, or one laid as hard links before clones, has
/// none). Any false falls through to the unchanged store `exec` — today's behaviour
/// exactly. Every test is a `test`/`[` builtin: a handful of `stat`s, no fork. It does NOT
/// prove bytes (no builtin can): [`crate::compat::ensure_root`] proves them when it lays
/// the root, and `repair` and doctor re-read them at `Depth::Deep`. The guard used to be
/// `[ 'R' -ef 'S' ]`, same device and inode, which only a hard link satisfies. A failed
/// `exec R` after a true guard exits the shell (126) rather than falling through — the
/// same race today's store `exec` has with `gc`.
///
/// NOTHING THAT READS A SHIM SEES A DIFFERENCE. The env note and `export` lines stay
/// ahead of the guard, so both `exec`s inherit them. The guard line starts with `[`, so
/// `parse_sh_shim_target` — the first trimmed line that starts with `exec '` — still
/// answers `S`, `parse_sh_shim_env` still reads the same exports, and
/// `tools/bootstrap-publisher.sh`'s `sed '^exec …'` still prints `S`: `which`, gc's
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

/// The note ahead of the landing prelude of an `agents/` twin — what a human reading
/// `agents/claude` learns about the lines before the exports, and where to ask.
const SH_LANDING_NOTE: &str = "# atpkg agents twin: while a newer build of this program is landing, `atpkg __landing` \
     waits for it and then runs the new one (aterm help pkg).\n";

/// THE LANDING PRELUDE of an `agents/` twin ([`crate::landing`], 2026-09-16): one `[ -f
/// <marker> ]` — a single `stat` — and, only while the marker stands, a hand-over to
/// `atpkg __landing <program> '<prefix>' -- "$@"` through a VARIABLE naming the embedded
/// co-located `atpkg` (the one this process runs as); when that binary is not executable
/// the prelude falls through to the twin's own exports and store `exec`. The PREFIX
/// rides along as an operand so the verb finds the store with no `HOME` (an `env -i`
/// wrapper, a launchd job) and never exits without running the tool — the twin's own
/// `exec` line would have run it, so the hand-over must too (review, 2026-09-16). Pure
/// string building, so it is rendered on every platform and unit-tested everywhere; the
/// Unix twin carries it, and the `.cmd` twin carries [`cmd_landing_prelude`], its
/// `cmd.exe` twin (2026-09-17).
///
/// NO `command -v atpkg` FALLBACK (review, 2026-09-16). The pending stub's chain tries
/// whatever `atpkg` PATH finds when the embedded one is gone, and for `__pending` that
/// is right: with no tool installed there is nothing else to run. Here there is — the
/// store build the twin's own `exec` line runs — and an OLDER `atpkg` on PATH (a
/// `~/.local/bin` alias from before this verb, a stale bundle) answers `__landing` with
/// exit 2 `unknown verb`, so the user's `claude` would never run. The wait is a courtesy;
/// the `exec` is the guarantee. A twin whose embedded path dangles (the app relocated)
/// runs the old build silently until the next pass re-lays it with the live path
/// ([`crate::activate::reconcile_agents`] compares the rendered bytes) — the pre-prelude
/// behaviour, never a stranded tool.
///
/// NO LINE HERE IS A LITERAL `exec '`. `parse_sh_shim_target` takes the first trimmed
/// line that starts with `exec '` as the target; both `exec`s below are `exec "$…"` on a
/// line that starts with `if`, so every reader keyed on the target — `resolve_shim`,
/// `sweep_agents_dir`'s keep-predicate, `active_builds`, gc's witnesses — still answers
/// the store target off the twin's real `exec` line. `parse_sh_shim_env` reads only
/// `export ` lines, of which the prelude has none. `is_pending_stub` reads line 2, which
/// stays the shim comment.
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

/// [`sh_shim_content_routed`] with `prelude` (empty for every `bin/` shim; an `agents/`
/// twin's [`sh_landing_prelude`]) inserted right after the header comment — ahead of the
/// exports, so a hand-over to `atpkg __landing` inherits nothing the store `exec` would
/// have set (the verb execs `bin/<program>`, which exports them itself).
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
/// target AND its exported values (and, on every platform, the landing prelude's).
fn sh_quote_str(s: &str) -> String {
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

/// The inverse of [`sh_shim_content`] — recover the target a shim execs.
///
/// `resolve_shim` used to be `read_link`, and the whole store's bookkeeping is built
/// on it: `active_builds`, `prune_stale_shims` and gc all ask "which build does this
/// shim point at". Parsing the stub keeps every one of those answers identical.
/// A TOMBSTONE (a failing notice script with no `exec`) must parse as `None`, exactly
/// as `read_link` returned `Err` for it.
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

#[cfg(any(windows, test))]
const MAX_CMD_SHIM_BYTES: usize = 64 * 1024;

/// Shared bound for reading a shim of either dialect before parsing it.
pub(crate) const MAX_SHIM_BYTES: usize = 64 * 1024;

/// Read the Windows `.cmd` shim through the package-metadata admission seam
/// before parsing it. Compiled in Unix tests as a cross-platform regression for
/// the Windows backend's otherwise-unexercised file behavior.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_target(path: &Path) -> Option<PathBuf> {
    let content = crate::metadata_io::read_bounded_regular_utf8(path, MAX_CMD_SHIM_BYTES).ok()?;
    parse_cmd_shim_target(&content)
}

/// The environment the Windows `.cmd` shim at `path` exports ([`parse_cmd_shim_env`]),
/// read through the same bounded, symlink-refusing seam; NONE for anything unreadable.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_env(path: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(path, MAX_CMD_SHIM_BYTES) {
        Ok(content) => parse_cmd_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shim_content_wraps_target_and_forwards_args() {
        let c = cmd_shim_content(Path::new("C:\\store\\ay\\18\\bin\\ay.exe"));
        // The frame first (2026-09-18), then the forward line, which ends the batch on
        // the SAME line: nothing is read after the program returns, however the file was
        // re-laid meanwhile (2026-09-17).
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
        // A tombstone must NOT parse as an installed shim (mirrors read_link Err on Unix).
        assert_eq!(parse_cmd_shim_target(&c), None);
        // Framed like every other `.cmd` (2026-09-18): a tombstone is laid over a shim
        // that may be executing, and the notice must not be what its program's exit
        // resumes into.
        let mut want = cmd_frame();
        want.push_str("@echo atpkg: ay was yanked/revoked — run `aterm pkg update` 1>&2\r\n");
        want.push_str("@exit /b 70\r\n");
        assert_eq!(c, want);
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
        let root = std::env::temp_dir().join(format!("atpkg-cmd-shim-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("claude.cmd");
        std::fs::write(&path, &c).unwrap();
        assert_eq!(read_cmd_shim_env(&path), env);
        assert_eq!(read_cmd_shim_target(&path), Some(target));
        assert_eq!(
            read_cmd_shim_env(&root.join("absent.cmd")),
            crate::shim_env::ShimEnv::NONE
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// On a real Windows host the backend writes, resolves and reads the env-carrying
    /// `.cmd` through its I/O primitives — the same file the pure test above pins.
    #[cfg(windows)]
    #[test]
    fn windows_backend_lays_and_reads_an_env_shim() {
        let root = std::env::temp_dir().join(format!("atpkg-win-env-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The target reader keys on the forward line's exact tail — the one every shim is
    /// written with since 2026-09-17, and the ` %*`-only one of a shim laid before it
    /// (read until its next re-lay, so `which`, the sweeps and gc keep resolving it) —
    /// and nothing else: a tail with a code, a `%1`, a bare quoted program, garbage.
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
            "a shim from before 2026-09-17 still resolves until it is re-laid"
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
        assert_eq!(CMD_FORWARD_TAIL, " %* & @exit /b");
        assert_eq!(CMD_LEGACY_FORWARD_TAIL, " %*");
    }

    /// THE `.cmd` AGENTS TWIN (2026-09-17, residual R4 closed): its rendered text, exact,
    /// over a prefix with a space and a `)` in it — the `Program Files (x86)` shape that
    /// breaks a parenthesised `if` block, which is why the prelude is `goto`-shaped. The
    /// prelude sits ahead of the exports; its hand-over line is `@"<atpkg>" __landing …`,
    /// not a `@"<target>" %* & @exit /b` forward, so the Windows target parser still
    /// resolves the twin to the STORE target and the env parser still reads the twin's
    /// own exports; every line that runs a program ends the batch on that SAME line (a
    /// re-laid twin is never re-read at an offset once the program returns; review
    /// finding, 2026-09-17); a prefix with a trailing separator is embedded without it
    /// (`\"` would be an escaped quote to the exe's argv parser); a `bin/` shim carries
    /// none of it; an empty prelude is the plain shim byte for byte; a `\\?\`-verbatim
    /// path (what `canonicalize` answers on Windows) is embedded without the prefix.
    /// RENDERED TEXT ONLY: no Windows box ran this — `cmd.exe`'s reading of it is
    /// unverified here.
    #[test]
    fn cmd_agents_twin_carries_the_landing_prelude_and_still_resolves_to_the_store() {
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
        // ONE stat on the embedded atpkg: the hand-over is not guarded a second time (the
        // window between two stats exited 0 with nothing run; review, 2026-09-17).
        assert_eq!(prelude.matches("atpkg.exe").count(), 2);
        let twin = cmd_shim_content_twin(target, &env, &prelude);
        // The frame (2026-09-18) ahead of the prelude: a twin from before it, executing
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
        // A prefix configured with a trailing separator — `C:\…\pkg\`, or several, or a
        // `/` — is embedded without it: `"C:\…\pkg\" -- %*` would hand the exe
        // `C:\…\pkg" -- …` as ONE operand (review, 2026-09-17). The rendered text is the
        // same twin, byte for byte.
        for trailing in [
            "C:\\Program Files (x86)\\aterm\\pkg\\",
            "C:\\Program Files (x86)\\aterm\\pkg\\\\",
            "C:\\Program Files (x86)\\aterm\\pkg/",
        ] {
            assert_eq!(
                cmd_landing_prelude("claude", Path::new(trailing), marker, atpkg),
                prelude,
                "{trailing}"
            );
        }
        assert_eq!(
            cmd_landing_prelude(
                "claude",
                prefix,
                Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude\\"),
                Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\\"),
            ),
            prelude
        );
        // A path that is nothing but separators, or a bare drive once trimmed (`C:` is
        // drive-relative to cmd), cannot be embedded: no prelude, the plain shim.
        for bad in ["C:\\", "\\", "/", "\\\\", "C:", "\\\\?\\C:\\"] {
            assert_eq!(
                cmd_landing_prelude("claude", Path::new(bad), marker, atpkg),
                "",
                "{bad}"
            );
            assert_eq!(
                cmd_landing_prelude("claude", prefix, marker, Path::new(bad)),
                "",
                "{bad}"
            );
        }
        assert_eq!(
            cmd_embedded_path(Path::new("C:\\x\\pkg\\")),
            Some("C:\\x\\pkg".into())
        );
        assert_eq!(
            cmd_embedded_path(Path::new("\\\\srv\\share\\pkg\\")),
            Some("\\\\srv\\share\\pkg".into())
        );
        assert_eq!(cmd_embedded_path(Path::new("C:\\")), None);
        // The verbatim spelling `canonicalize` answers on Windows is embedded without its
        // prefix (`if exist "\\?\C:\…"` is not a spelling cmd reliably accepts).
        let verbatim = cmd_landing_prelude(
            "claude",
            Path::new("\\\\?\\C:\\Program Files (x86)\\aterm\\pkg"),
            Path::new("\\\\?\\C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude"),
            Path::new("\\\\?\\C:\\Program Files (x86)\\aterm\\app\\atpkg.exe"),
        );
        assert_eq!(verbatim, prelude);
        assert!(!verbatim.contains("\\\\?\\"));
        assert_eq!(
            strip_verbatim_prefix(Path::new("\\\\?\\UNC\\srv\\share\\aterm\\atpkg.exe")),
            PathBuf::from("\\\\srv\\share\\aterm\\atpkg.exe")
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
        let root = std::env::temp_dir().join(format!("atpkg-cmd-twin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("claude.cmd");
        std::fs::write(&path, &twin).unwrap();
        assert_eq!(read_cmd_shim_target(&path), Some(target.to_path_buf()));
        assert_eq!(read_cmd_shim_env(&path), env);
        std::fs::remove_dir_all(root).unwrap();
        // Fail-closed: a path that could break out of `"…"` or expand renders NO
        // prelude — the plain shim runs the store build; nothing injectable is written.
        for bad in [
            "C:\\x\\\"&calc.exe\"\\landing\\claude",
            "C:\\x\\%APPDATA%\\landing\\claude",
            "C:\\x\\landing\\claude\r\n@calc",
        ] {
            assert_eq!(
                cmd_landing_prelude("claude", prefix, Path::new(bad), atpkg),
                ""
            );
            assert_eq!(
                cmd_landing_prelude("claude", Path::new(bad), marker, atpkg),
                ""
            );
            assert_eq!(
                cmd_landing_prelude("claude", prefix, marker, Path::new(bad)),
                ""
            );
        }
        assert_eq!(cmd_landing_prelude("cla%ude", prefix, marker, atpkg), "");
        // The per-platform dispatch hands each backend its own dialect.
        let dispatched = landing_prelude("claude", prefix, marker, atpkg);
        if cfg!(windows) {
            assert_eq!(dispatched, prelude);
        } else {
            assert_eq!(
                dispatched,
                sh_landing_prelude("claude", prefix, marker, atpkg)
            );
        }
    }

    /// THE FRAME (2026-09-18), exact: `@goto :main`, 51 lines of 78 colons, `@exit /b`,
    /// `:main` — 4110 bytes ahead of every `.cmd` body this crate writes — and the bound
    /// it is sized to, derived from the legacy writer's own caps (8 `@set` entries of 256
    /// bytes, a `MAX_PATH` target at three UTF-8 bytes a unit): 2908 bytes, inside the
    /// padding with 1185 to spare. Every `.cmd` renderer — shim, twin, tombstone — goes
    /// through it, and both parsers read the same answers over a framed file as over a
    /// legacy one. RENDERED TEXT ONLY: `cmd.exe`'s reading of it is unverified here.
    #[test]
    fn cmd_frame_is_the_head_the_padding_and_the_exit_sized_to_the_legacy_bound() {
        let frame = cmd_frame();
        let mut want = String::from("@goto :main\r\n");
        for _ in 0..51 {
            want.push_str(&":".repeat(78));
            want.push_str("\r\n");
        }
        want.push_str("@exit /b\r\n:main\r\n");
        assert_eq!(frame, want);
        assert_eq!(frame.len(), CMD_FRAME_BYTES);
        assert_eq!(CMD_FRAME_BYTES, 4110, "the per-file cost, said in bytes");
        assert_eq!(CMD_PADDING_END_BYTES, 4093);
        assert_eq!(
            &frame[CMD_PADDING_END_BYTES..CMD_PADDING_END_BYTES + CMD_PADDING_EXIT.len()],
            CMD_PADDING_EXIT
        );
        // The bound, from the caps the legacy writer wrote under.
        assert_eq!(crate::shim_env::MAX_SHIM_ENV, 8);
        assert_eq!(crate::shim_env::MAX_ENTRY_BYTES, 256);
        assert_eq!(CMD_LEGACY_TARGET_BOUND_BYTES, 780);
        assert_eq!(
            CMD_LEGACY_FILE_BOUND_BYTES,
            8 * (6 + 256 + 3) + 2 + 780 + 1 + 3 + 2
        );
        assert_eq!(CMD_LEGACY_FILE_BOUND_BYTES, 2908);
        // The padding end as rendered (the compile-time assertion above the renderer pins
        // the constants; this pins the text they describe).
        let padding_end = frame.find("@exit /b").unwrap();
        assert_eq!(padding_end, CMD_PADDING_END_BYTES);
        assert!(padding_end >= CMD_LEGACY_FILE_BOUND_BYTES);
        assert_eq!(padding_end - CMD_LEGACY_FILE_BOUND_BYTES, 1185);
        assert_eq!(CMD_LEGACY_MIN_LINE_BYTES, 12, "`@set \"A=b\"` + CRLF");
        assert!(CMD_FRAME_HEAD.len() - 2 <= CMD_LEGACY_MIN_LINE_BYTES);
        // The line it is derived from is one the legacy writer COULD lay, and the one
        // byte shorter (an empty value) is one it could not (review finding, 2026-09-18).
        assert!(crate::shim_env::ShimEnv::admit(&["A=b".to_string()]).is_ok());
        assert!(crate::shim_env::ShimEnv::admit(&["A=".to_string()]).is_err());
        // A framed file with the longest body on top is far inside the reader's cap.
        assert!(frame.len() + CMD_LEGACY_FILE_BOUND_BYTES + 2048 < MAX_CMD_SHIM_BYTES);
        // Nothing in the frame expands, quotes or opens a block; CRLF throughout; every
        // line silent (`@`) or a label.
        assert!(!frame.contains(['%', '"', '(', ')', '^', '&']));
        assert_eq!(frame.matches("\r\n").count(), frame.matches('\n').count());
        for line in frame.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.starts_with('@') || line.starts_with(':'), "{line}");
        }
        // ONE writer: every `.cmd` renderer starts with the frame, and the head jumps to
        // the label the body starts at.
        let target = Path::new("C:\\store\\ay\\18\\bin\\ay.exe");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let prelude = cmd_landing_prelude(
            "ay",
            Path::new("C:\\pkg"),
            Path::new("C:\\pkg\\landing\\ay"),
            Path::new("C:\\app\\atpkg.exe"),
        );
        for (what, content) in [
            ("plain shim", cmd_shim_content(target)),
            ("env shim", cmd_shim_content_env(target, &env)),
            ("twin", cmd_shim_content_twin(target, &env, &prelude)),
            (
                "tombstone",
                cmd_tombstone_content("atpkg: ay was yanked/revoked"),
            ),
        ] {
            assert!(content.starts_with(&frame), "{what}: {content}");
            assert_eq!(content.matches("\r\n:main\r\n").count(), 1, "{what}");
            assert_eq!(content.matches("@goto :main").count(), 1, "{what}");
            assert!(
                !content[frame.len() - 2..].contains("\r\n@exit /b\r\n"),
                "{what}: the body carries no exit line of its own past the frame's"
            );
        }
        // Both parsers, over the framed file and over the legacy one: the same answers.
        let mut legacy_env = String::new();
        for (n, v) in env.entries() {
            legacy_env.push_str("@set \"");
            legacy_env.push_str(n);
            legacy_env.push('=');
            legacy_env.push_str(v);
            legacy_env.push_str("\"\r\n");
        }
        legacy_env.push_str("@\"C:\\store\\ay\\18\\bin\\ay.exe\"");
        legacy_env.push_str(CMD_LEGACY_FORWARD_TAIL);
        legacy_env.push_str("\r\n");
        let legacy_plain = "@\"C:\\store\\ay\\18\\bin\\ay.exe\" %*\r\n";
        for (what, content) in [
            ("legacy plain", legacy_plain.to_string()),
            ("framed plain", cmd_shim_content(target)),
            (
                "framed twin, no env",
                cmd_shim_content_twin(target, &crate::shim_env::ShimEnv::NONE, &prelude),
            ),
        ] {
            assert_eq!(
                parse_cmd_shim_target(&content),
                Some(target.to_path_buf()),
                "{what}"
            );
            assert_eq!(
                parse_cmd_shim_env(&content),
                crate::shim_env::ShimEnv::NONE,
                "{what}"
            );
        }
        for (what, content) in [
            ("legacy env", legacy_env),
            ("framed env", cmd_shim_content_env(target, &env)),
            ("framed twin", cmd_shim_content_twin(target, &env, &prelude)),
        ] {
            assert_eq!(
                parse_cmd_shim_target(&content),
                Some(target.to_path_buf()),
                "{what}"
            );
            assert_eq!(parse_cmd_shim_env(&content), env, "{what}");
        }
        assert_eq!(
            parse_cmd_shim_target(&frame),
            None,
            "the frame alone forwards nowhere"
        );
        assert_eq!(parse_cmd_shim_env(&frame), crate::shim_env::ShimEnv::NONE);
    }

    /// THE RESUME SIMULATION (2026-09-18, closing the one window 2026-09-17 documented):
    /// three legacy files — the plain twin every Windows twin was until 2026-09-17, an
    /// env-carrying `bin/` shim, and the LONGEST shim the legacy writer's caps allow —
    /// re-laid to their framed successors while executing. At EVERY offset `0..=old_len`
    /// of the new file: offset 0 is the fresh run (`@goto :main`); no line of the old
    /// file can end inside the head's non-inert bytes (`cmd` resumes only after a line
    /// end of the OLD file, and its shortest line is 12 bytes); every other offset reads
    /// a colon label, an empty line or `@exit /b`. The end-of-file resume — the one a
    /// running program's exit actually produces — lands on a colon label. As the control,
    /// the same old twin over the successor laid BARE (the 2026-09-17 shape) resumes
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
        // THE CONTROL: the same old twin over its successor laid BARE — the 2026-09-17
        // shape, prelude first — resumes inside the prelude: the window the frame closes.
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
        let root =
            std::env::temp_dir().join(format!("atpkg-cmd-shim-sparse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("ay.cmd");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_CMD_SHIM_BYTES + 1) as u64).unwrap();
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cmd_shim_reader_rejects_fifo_and_symlink_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root =
            std::env::temp_dir().join(format!("atpkg-cmd-shim-special-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod sh_shim_tests {
    use super::*;

    /// THE REGRESSION THAT BROKE THE PRODUCT. A symlinked shim made Trust's `targo`
    /// refuse to run on every successful install — it authenticates its own
    /// `current_exe` as a plain, already-canonical regular file. The stub must
    /// therefore BE a regular file whose `exec` hands off to the real path.
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

    /// A tombstone has no `exec` line, so it must resolve to nothing — exactly as
    /// `read_link` returned `Err` for it. `active_builds` and gc depend on this.
    #[test]
    fn a_tombstone_is_not_an_installed_shim() {
        let notice = "#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n";
        assert_eq!(parse_sh_shim_target(notice), None);
    }

    /// THE WRAPPER WITH AN ENVIRONMENT (design S7), exact: the note, one
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
        assert_eq!(
            body,
            "#!/bin/sh\n\
             # atpkg shim — exec so the tool authenticates at its real path.\n\
             # shim_env from the signed manifest: only this managed copy runs with it.\n\
             export DISABLE_AUTOUPDATER='1'\n\
             export B='it'\\''s two words'\n\
             exec '/prefix/store/claude/2026082701/bin/claude' \"$@\"\n"
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
        let root = std::env::temp_dir().join(format!("atpkg-sh-env-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        let out = std::process::Command::new(&shim)
            .arg("arg")
            .env_remove("DISABLE_AUTOUPDATER")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1|arg\n");
        // The plain shim: no export, the program sees nothing, and nothing is read back.
        install_shim_to(&shim, &target).unwrap();
        assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
        let out = std::process::Command::new(&shim)
            .arg("arg")
            .env_remove("DISABLE_AUTOUPDATER")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unset|arg\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Defence in depth: a path containing a quote cannot break out of the stub.
    #[test]
    fn an_embedded_quote_cannot_escape_the_stub() {
        let nasty = Path::new("/prefix/store/o'ny/1/bin/x");
        let body = sh_shim_content(nasty);
        assert!(body.contains(r"'\''"), "quote is POSIX-escaped: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(nasty));
    }

    /// THE ROUTED SHIM, exact: the header, the env note and exports (so both execs inherit
    /// them), the exec-root note, the guard, and the unchanged store `exec` line. `None` is
    /// the plain shim byte for byte; the target and the env parse back off the routed form
    /// exactly as off the plain one, because the guard line starts with `[`.
    #[test]
    fn the_routed_shim_is_exact_and_parses_as_the_plain_one() {
        let target = Path::new("/p/store/trust/8595/bin/tippy");
        let route = Path::new("/p/compat/trust/8595/bin/tippy");
        let none = crate::shim_env::ShimEnv::NONE;
        let body = sh_shim_content_routed(target, &none, Some(route));
        assert_eq!(
            body,
            "#!/bin/sh\n\
             # atpkg shim — exec so the tool authenticates at its real path.\n\
             # atpkg exec root: this build ships bin/rustc as a separate copy of trustc, which \
             its tippy refuses; the same tools run from a clone of the build where rustc holds \
             trustc's bytes (aterm pkg doctor).\n\
             [ ! -h '/p/compat/trust/8595/bin/tippy' ] && [ -f '/p/compat/trust/8595/bin/tippy' \
             ] && [ -x '/p/compat/trust/8595/bin/tippy' ] && [ -f '/p/store/trust/8595/bin/tippy' \
             ] && [ ! -h '/p/compat/trust/8595/.atpkg-root' ] && [ -f \
             '/p/compat/trust/8595/.atpkg-root' ] && exec '/p/compat/trust/8595/bin/tippy' \
             \"$@\"\n\
             exec '/p/store/trust/8595/bin/tippy' \"$@\"\n"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), none);
        assert_eq!(
            sh_shim_content_routed(target, &none, None),
            sh_shim_content(target),
            "no route: today's shim, byte for byte"
        );

        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let body = sh_shim_content_routed(target, &env, Some(route));
        assert_eq!(
            body,
            "#!/bin/sh\n\
             # atpkg shim — exec so the tool authenticates at its real path.\n\
             # shim_env from the signed manifest: only this managed copy runs with it.\n\
             export DISABLE_AUTOUPDATER='1'\n\
             # atpkg exec root: this build ships bin/rustc as a separate copy of trustc, which \
             its tippy refuses; the same tools run from a clone of the build where rustc holds \
             trustc's bytes (aterm pkg doctor).\n\
             [ ! -h '/p/compat/trust/8595/bin/tippy' ] && [ -f '/p/compat/trust/8595/bin/tippy' \
             ] && [ -x '/p/compat/trust/8595/bin/tippy' ] && [ -f '/p/store/trust/8595/bin/tippy' \
             ] && [ ! -h '/p/compat/trust/8595/.atpkg-root' ] && [ -f \
             '/p/compat/trust/8595/.atpkg-root' ] && exec '/p/compat/trust/8595/bin/tippy' \
             \"$@\"\n\
             exec '/p/store/trust/8595/bin/tippy' \"$@\"\n"
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
        assert_eq!(
            body.matches(r"'/it'\''s/compat/trust/1/.atpkg-root'")
                .count(),
            2
        );
        assert!(!body.contains("/it's/"), "no raw quote survives: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    }

    /// THE GUARD UNDER A REAL SHELL. The rendered shim is run by `/bin/sh` (bash 3.2 in
    /// POSIX mode on macOS) and by `/bin/dash` where present, with an empty environment and
    /// arguments carrying a space and both quote kinds, under a prefix whose path holds a
    /// space and a quote. Store and route are the same script, which prints the path it
    /// was exec'd as (`$0`), so the output says which one ran: a CLONE or a byte COPY at the
    /// route runs the route when the root's marker stands; with no marker, a marker that is
    /// a symlink, a route that is a SYMLINK, one that is not executable, or a MISSING route,
    /// the store path runs — today's shim. A HARD LINK is a regular file too, and the shell
    /// cannot tell it from a clone: that is `route_for_shim`'s job, and repair's, which
    /// rebuild such a root before a shim is ever rendered through it. The export reaches
    /// whichever ran.
    #[cfg(unix)]
    #[test]
    fn the_guard_runs_the_root_only_when_the_marked_root_stands() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = std::env::temp_dir()
            .join(format!("atpkg-sh-route-{}", std::process::id()))
            .join("pre fix's");
        let _ = std::fs::remove_dir_all(base.parent().unwrap());
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
}
