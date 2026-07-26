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
//!   symlink. [`atomic_symlink`] creates it (used for `current` and the Kani
//!   sysroot/toolchain dir links).
//! * **Bin shims** — a `bin/<tool>.cmd` batch wrapper (`@"<target>.exe" %*`), not a
//!   symlink into the store. [`install_shim`] writes it, [`install_tombstone_shim`]
//!   writes the failing (`exit /b 70`) variant, and [`resolve_shim`] reads a shim's
//!   target back (parsing the `.cmd`) — the inverse of the Unix `read_link`.
//! * **Private state** — [`ensure_private_dir`]/[`harden_file`]/[`write`-side mode]
//!   rely on the per-user `%LOCALAPPDATA%` profile ACL (POSIX mode/owner bits have no
//!   analogue): [`harden_file`]/[`set_mode`] are no-ops, [`dir_meta_is_private`] is a
//!   best-effort `true`, [`file_mode`](permission_mode)/[`our_uid`] report the
//!   not-applicable sentinel.
//! * **Disk** — [`volume_free_bytes`] calls `GetDiskFreeSpaceExW` (dependency-free
//!   manual FFI) instead of `statvfs`; both fail **OPEN** (`None` on any error).
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
#[cfg(any(windows, test))]
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

/// Install a `bin/<tool>` shim pointing at `tool` inside a build's `bin/` directory
/// (`build_bin_dir`). The concrete executable name gets [`EXE_SUFFIX`] appended
/// (`tool` on Unix, `tool.exe` on Windows); `shim` is the concrete shim path
/// (`bin/<tool>` on Unix, `bin/<tool>.cmd` on Windows — see [`crate::store::Layout::shim`]).
///
/// * **Unix**: a symlink `shim -> build_bin_dir/<tool>` (atomic temp-symlink + rename).
/// * **Windows**: a `bin/<tool>.cmd` batch wrapper invoking `build_bin_dir\<tool>.exe`.
pub fn install_shim(build_bin_dir: &Path, tool: &str, shim: &Path) -> io::Result<()> {
    let mut exe = String::from(tool);
    exe.push_str(EXE_SUFFIX);
    install_shim_to(shim, &build_bin_dir.join(exe))
}

// ---------------------------------------------------------------------------
// Pure `.cmd` shim formatting/parsing.
//
// These carry the Windows shim CONTENT logic but are pure string functions, so
// they are compiled (and unit-tested) on every platform via `cfg(any(windows,
// test))` — the Windows backend calls them for I/O, and the Unix test build
// exercises them directly, keeping the correct-by-construction Windows format
// covered by the (Unix-run) test suite. They are compiled OUT of a non-test Unix
// build (nothing there calls them), so they raise no dead-code lint.
// ---------------------------------------------------------------------------

/// The body of a Windows bin shim: `@"<target>" %*`, CRLF-terminated. `target` is
/// the absolute path to the store/checkout executable the shim forwards to.
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content(target: &Path) -> String {
    let mut s = String::from("@\"");
    s.push_str(&target.to_string_lossy());
    s.push_str("\" %*\r\n");
    s
}

/// Whether `target` is safe to embed inside a `@"<target>" %*` shim WITHOUT breaking
/// out of the quoting or triggering batch expansion. A managed-store path (validated
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

/// The body of a Windows **tombstone** shim: prints `message` to stderr and exits
/// 70 (`EX_SOFTWARE`), matching the Unix `sh` tombstone's contract. `message` is
/// `cmd`-escaped so a crafted tool name cannot break out of the `echo`.
#[cfg(any(windows, test))]
pub(crate) fn cmd_tombstone_content(message: &str) -> String {
    let mut s = String::from("@echo ");
    s.push_str(&cmd_echo_escape(message));
    s.push_str(" 1>&2\r\n@exit /b 70\r\n");
    s
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
/// [`cmd_shim_content`] (`@"<target>" %*`). Returns `None` for a tombstone shim
/// (no quoted target) or any unrecognized content — the Windows inverse of the
/// Unix `read_link` returning `Err` for a non-symlink.
#[cfg(any(windows, test))]
pub(crate) fn parse_cmd_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@\"")
            && let Some(end) = rest.find('"')
        {
            return Some(PathBuf::from(&rest[..end]));
        }
    }
    None
}

#[cfg(any(windows, test))]
const MAX_CMD_SHIM_BYTES: usize = 64 * 1024;

/// Read the Windows `.cmd` shim through the package-metadata admission seam
/// before parsing it. Compiled in Unix tests as a cross-platform regression for
/// the Windows backend's otherwise-unexercised file behavior.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_target(path: &Path) -> Option<PathBuf> {
    let content = crate::metadata_io::read_bounded_regular_utf8(path, MAX_CMD_SHIM_BYTES).ok()?;
    parse_cmd_shim_target(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shim_content_wraps_target_and_forwards_args() {
        let c = cmd_shim_content(Path::new("C:\\store\\ay\\18\\bin\\ay.exe"));
        assert_eq!(c, "@\"C:\\store\\ay\\18\\bin\\ay.exe\" %*\r\n");
    }

    #[test]
    fn cmd_shim_round_trips_through_parse() {
        let target = PathBuf::from("C:\\store\\ay\\18\\bin\\ay.exe");
        let content = cmd_shim_content(&target);
        assert_eq!(parse_cmd_shim_target(&content), Some(target));
    }

    #[test]
    fn tombstone_content_has_no_forward_target_and_exits_70() {
        let c = cmd_tombstone_content("atpkg: ay was yanked/revoked — run `atpkg update`");
        assert!(c.contains("1>&2"), "notice goes to stderr: {c}");
        assert!(c.contains("exit /b 70"), "exits 70: {c}");
        // A tombstone must NOT parse as an installed shim (mirrors read_link Err on Unix).
        assert_eq!(parse_cmd_shim_target(&c), None);
    }

    #[test]
    fn cmd_echo_escape_neutralizes_metacharacters() {
        // A crafted tool name with cmd metacharacters must be inert inside echo.
        let esc = cmd_echo_escape("a&b|c>d<e^f%g\"h");
        assert_eq!(esc, "a^&b^|c^>d^<e^^f%%g^\"h");
    }

    #[test]
    fn parse_cmd_shim_target_rejects_garbage() {
        assert_eq!(parse_cmd_shim_target("not a shim\r\n"), None);
        assert_eq!(parse_cmd_shim_target(""), None);
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
