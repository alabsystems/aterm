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
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_content(target: &Path) -> String {
    let mut s = String::from(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexec ",
    );
    s.push_str(&sh_shim_quote(target));
    s.push_str(" \"$@\"\n");
    s
}

/// Single-quote a path for the `sh` stub, escaping embedded quotes POSIX-style. A
/// managed-store path never contains one; this is fail-closed defence in depth, the
/// same posture the `.cmd` side takes.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_quote(target: &Path) -> String {
    let mut out = String::from("'");
    for c in target.to_string_lossy().chars() {
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
        let c = cmd_tombstone_content("atpkg: ay was yanked/revoked — run `aterm pkg update`");
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

    /// Defence in depth: a path containing a quote cannot break out of the stub.
    #[test]
    fn an_embedded_quote_cannot_escape_the_stub() {
        let nasty = Path::new("/prefix/store/o'ny/1/bin/x");
        let body = sh_shim_content(nasty);
        assert!(body.contains(r"'\''"), "quote is POSIX-escaped: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(nasty));
    }
}
