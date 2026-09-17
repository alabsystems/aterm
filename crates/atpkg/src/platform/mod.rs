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
// build (nothing there calls them), so they raise no dead-code lint.
// ---------------------------------------------------------------------------

/// The body of a Windows bin shim: `@"<target>" %*`, CRLF-terminated. `target` is
/// the absolute path to the store/checkout executable the shim forwards to. The
/// no-environment form of [`cmd_shim_content_env`], which the backend now writes
/// through; kept for the tests that pin the plain shape.
#[cfg(test)]
pub(crate) fn cmd_shim_content(target: &Path) -> String {
    cmd_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// [`cmd_shim_content`] with the shim's exported environment ahead of the forward
/// line: one `@set "NAME=VALUE"` per entry (the quoted `set` form, so a value's
/// trailing space or `&` is literal), then `@"<target>" %*`. The `@` keeps every line
/// silent. An empty `env` is byte-identical to [`cmd_shim_content`].
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
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
    s.push_str("\" %*\r\n");
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
/// string building, so it is rendered on every platform and unit-tested everywhere; only
/// the Unix twin carries it (a `.cmd` twin is the plain shim).
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
        assert_eq!(
            c,
            "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"B=two words\"\r\n\
             @\"C:\\store\\claude\\2026082701\\bin\\claude.exe\" %*\r\n"
        );
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
                "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"PATH=C:\\x\"\r\n@\"C:\\a.exe\" %*\r\n"
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
