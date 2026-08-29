// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending-program stubs (R6): from the moment the lean app first launches (and
//! immediately after adoption), EVERY default-set program name resolves on `PATH`,
//! and running one is always helpful — never "command not found". The EXTRAS
//! (owner decision 2026-08-26: `codex`, `claude`) get the same courtesy with one
//! difference the stub itself records ([`StubKind`]): typing an extra's name asks a
//! vendor-named consent question before a byte moves, instead of bumping an install
//! that is already coming.
//!
//! A stub is a tiny `/bin/sh` file at `<prefix>/bin/<tool>` (the same seam
//! [`crate::flow`]'s tombstone shims use — no parallel machinery) that execs
//! `atpkg __pending <tool>`: a short honest message with the LIVE install state, plus
//! a bump of that program to the front of the install queue. It grants nothing —
//! the authoritative roster stays the signed index — and it is replaced by the real
//! shim the moment the program installs, atomically: `platform::install_shim`
//! already lands via temp+`rename(2)` (see `activate.rs`), so the name resolves to
//! SOMETHING at every instant — stub before, real shim after, no window of absence,
//! no `EEXIST`. `prune_stale_shims` skips it (a stub resolves to no store target)
//! and `active_builds` never counts it.
//!
//! # The fallback chain (a stub must never dangle)
//!
//! An embedded absolute atpkg path breaks on app relocation or self-update, and a
//! stub that prints sh's own "not found" would violate the guarantee. So the script
//! falls back, in order:
//!
//! 1. the embedded co-located `atpkg` path, if it exists and is executable;
//! 2. `command -v atpkg` (the `~/.local/bin` alias / shell hook);
//! 3. a static honest message — and exit 127.
//!
//! The per-launch seed-pass reconcile REWRITES stubs, refreshing embedded paths as
//! a matter of course.
//!
//! # Trust posture
//!
//! Stub names pass [`crate::store::shim_allowed`] by construction (they are only
//! ever written through [`crate::store::ToolName`]) — the sensitive-name refusal
//! applies to stubs identically. A stub is recognized by its marker line and
//! removed only when it still carries it: a real shim, a tombstone, a dev link or
//! any hand-made file is never touched.
//!
//! # An alias is never a stub (owner decision 2026-08-27)
//!
//! A stub is laid for the PLAIN program name only. The `alab-<tool>` alias
//! ([`crate::activate::Aliases`], `crate::store::ALIAS_PREFIX`) exists to name
//! ALab's copy unambiguously ONCE IT IS INSTALLED — beside a `trust` that Homebrew's
//! p11-kit may shadow, `alab-trust` always runs the managed build. Before the
//! install there is nothing to disambiguate: the plain-name stub already answers
//! (bumps, or asks consent), and a second promising name on `PATH` would double
//! every "not found" into two courtesy scripts for one program. So the reconcile
//! never lays `alab-*`, and [`write_pending_stub_with`] is a no-op for an alias
//! name — the alias appears with the real shims, and only then.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use crate::store::{Layout, ToolName};

/// The compile-time default-set roster the stubs cover BEFORE the first signed index
/// resolves: `(name, one authored what-it-is line)`. Names only — no versions, no
/// URLs — so staleness is harmless: the index-resolve reconcile adds stubs for newly
/// published names and removes ones the signed index no longer lists.
pub const DEFAULT_SET_STUB_NAMES: &[(&str, &str)] = &[
    ("ay", "ALab's SMT solver"),
    ("clean", "ALab's theorem prover"),
    ("nn", "ALab's neural-network tool"),
    ("ny", "ALab's neural-network verifier"),
    (
        "trust",
        "the Trust compiler bundle — a Rust compiler that verifies what it compiles",
    ),
    (
        "trust-cg",
        "the Trust compiler's codegen member (coherence group)",
    ),
    (
        "trust-ir",
        "the Trust compiler's IR member (coherence group)",
    ),
    ("trust-mc", "the Trust model checker"),
    (
        "trust-vc",
        "the Trust compiler's verification-condition member (coherence group)",
    ),
    ("ty", "ALab's specification checker"),
];

/// The compile-time EXTRAS roster: programs the signed index lists with `extra = true`
/// (owner decision 2026-08-26) — listed and pinned, available on request, NEVER
/// installed by default. Their stubs are laid at adoption beside the default set's,
/// but typing the name asks a vendor-named consent question first ([`StubKind::Extra`]);
/// nothing downloads before the answer. The authored line IS the consent copy, so it
/// names the vendor, the license, the rough size and the host the bytes come from —
/// descriptively ("OpenAI Codex CLI", "Anthropic Claude Code"), never as ALab marks.
/// Names only, no versions/URLs; the index-resolve reconcile is the authority once a
/// signed index is readable, and its `https` row is what actually pins the bytes.
pub const EXTRAS_STUB_NAMES: &[(&str, &str)] = &[
    (
        "codex",
        "OpenAI Codex CLI — Apache-2.0, ~90 MB, downloaded from github.com/openai/codex",
    ),
    (
        "claude",
        "Anthropic Claude Code — proprietary, ~230 MB, downloaded from downloads.claude.ai",
    ),
];

/// The authored one-line description for `name`, if either compiled roster carries
/// one. A program published after this binary gets the honest generic line instead.
#[must_use]
pub fn describe(name: &str) -> Option<&'static str> {
    DEFAULT_SET_STUB_NAMES
        .iter()
        .chain(EXTRAS_STUB_NAMES)
        .find(|(n, _)| *n == name)
        .map(|(_, d)| *d)
}

/// Whether the compiled EXTRAS roster names `name` — the pre-index answer to "is this
/// an extra". A stub on disk, kept current by the reconcile from the SIGNED index,
/// outranks it ([`pending_stub_kind`]); this is the fallback for a name typed with
/// no stub laid.
#[must_use]
pub fn compiled_extra(name: &str) -> bool {
    EXTRAS_STUB_NAMES.iter().any(|(n, _)| *n == name)
}

/// What a pending stub stands for — the ONE fact the stub file carries beyond its
/// name, so `atpkg __pending` can answer offline and instantly whether the typed
/// name is a default-set member (bump it; it is coming) or an EXTRA (ask first;
/// nothing is coming until the user says so). Written by the reconcile from the
/// signed index's `extra` flag, so it is as current as the last resolve. It is a
/// claim from the user's own trust domain: its worst misreading is one unnecessary
/// consent question, never an install — the opt-in marker and the signed index are
/// what add work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubKind {
    /// A default-set member: the pass installs it unasked; the stub bumps it.
    DefaultSet,
    /// An extra: listed and pinned, installed only after a typed-name consent.
    Extra,
}

/// The marker line that identifies a pending stub — the recognition gate for every
/// rewrite/removal below. Version-suffixed so a future stub format can coexist.
const STUB_MARKER: &str = "# atpkg pending-program stub v1";

/// The second marker line an EXTRA's stub carries (right after [`STUB_MARKER`]); its
/// absence means default-set. Recognized in the same two spellings (bare, and behind
/// a batch `rem`), so [`stub_kind`] reads it on every platform.
const EXTRA_MARKER: &str = "# atpkg extra: asks consent before installing";

/// The third marker line a stub MAY carry: the names the program REQUIRES
/// ([`crate::manifest::Program::requires`]), space-separated after the colon, written
/// from the signed index by the reconcile so an extra's consent question can list them
/// OFFLINE (`Install codex? It also needs: clt [y/N]`). Read back through the
/// [`ToolName`] gate ([`stub_requires`]); like the kind, its worst misreading is one
/// wrong word in a question — never an install.
const REQUIRES_MARKER: &str = "# atpkg requires:";

/// The static last-resort message (fallback 3) — the one line the stub can always
/// say without an atpkg to ask.
pub const STUB_UNREACHABLE_MSG: &str =
    "aterm's package manager is not reachable — open aterm to finish installing";

/// Single-quote `s` for safe embedding in a `/bin/sh` script (the POSIX `'\''`
/// escape). [`crate::store::shim_allowed`] does not forbid shell metacharacters, so
/// the stub body must never let a crafted name break out of its quotes. (A local
/// twin of the platform backend's private helper — same rule, one screen away from
/// its use.)
fn sh_single_quote(s: &str) -> String {
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

/// The stub script body for `tool`, embedding `atpkg` as the co-located fallback-1
/// path. Pure, so the shape is pinned by tests.
///
/// NOTE the `exec "$ATPKG"` spelling: `platform::parse_sh_shim_target` recognizes
/// real shims by a trimmed line starting `exec '`, so the stub deliberately execs
/// through a variable — a stub must resolve as NO store target (`resolve_shim` ⇒
/// `None`), or `active_builds`/`which`/the front door would mistake it for an
/// installed tool.
#[cfg(test)]
#[must_use]
fn stub_content(tool: &ToolName, atpkg: &Path, kind: StubKind) -> String {
    stub_content_with(tool, atpkg, kind, &[])
}

/// [`stub_content`] carrying the program's `requires` on the requires marker line.
#[must_use]
fn stub_content_with(tool: &ToolName, atpkg: &Path, kind: StubKind, requires: &[String]) -> String {
    if cfg!(windows) {
        stub_content_cmd_with(tool, atpkg, kind, requires)
    } else {
        stub_content_sh_with(tool, atpkg, kind, requires)
    }
}

/// The requires marker line's payload: the admissible names, space-separated. Empty
/// (no line at all) when the program requires nothing — every stub laid before this
/// marker existed reads exactly as it did.
fn requires_marker_line(requires: &[String]) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for r in requires {
        if ToolName::new(r).is_some() && !names.contains(&r.as_str()) {
            names.push(r.as_str());
        }
    }
    if names.is_empty() {
        return None;
    }
    let mut line = String::from(REQUIRES_MARKER);
    for n in names {
        line.push(' ');
        line.push_str(n);
    }
    Some(line)
}

/// The POSIX `/bin/sh` stub body (macOS/Linux — the shim there is a plain
/// executable file).
#[cfg(test)]
#[must_use]
fn stub_content_sh(tool: &ToolName, atpkg: &Path, kind: StubKind) -> String {
    stub_content_sh_with(tool, atpkg, kind, &[])
}

/// [`stub_content_sh`] with the requires marker line.
#[must_use]
fn stub_content_sh_with(
    tool: &ToolName,
    atpkg: &Path,
    kind: StubKind,
    requires: &[String],
) -> String {
    let name = sh_single_quote(tool.as_str());
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push('\n');
    if kind == StubKind::Extra {
        s.push_str(EXTRA_MARKER);
        s.push('\n');
    }
    if let Some(line) = requires_marker_line(requires) {
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str("# Replaced by the real shim when the program installs.\nATPKG=");
    s.push_str(&sh_single_quote(&atpkg.to_string_lossy()));
    s.push_str("\nif [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending ");
    s.push_str(&name);
    s.push_str("\nfi\nif command -v atpkg >/dev/null 2>&1; then\n  exec atpkg __pending ");
    s.push_str(&name);
    s.push_str("\nfi\nprintf '%s\\n' ");
    s.push_str(&sh_single_quote(STUB_UNREACHABLE_MSG));
    s.push_str(" 1>&2\nexit 127\n");
    s
}

/// The batch twin: on Windows the shim slot is `bin/<tool>.cmd`
/// ([`crate::store::ToolName::shim_file`]), and this module used to lay a
/// `#!/bin/sh` body into it — cmd.exe then read POSIX shell as batch and the
/// "pending" promise rendered as `'#!' is not recognized…` garbage. Same
/// three fallbacks as the sh body, batch-spelled; the marker rides a `rem`
/// line ([`is_pending_stub`] accepts both spellings) so recognition, rewrite
/// and removal keep working on the file cmd.exe can actually run. `exit /b
/// 127` throughout — the stub contract is "the tool did not run" regardless
/// of which fallback answered. Tool names are `ToolName`-vetted and the atpkg
/// path rides plain double quotes, the exact conventions of the real `.cmd`
/// shim writer (`platform::cmd_shim_content`).
#[cfg(test)]
#[must_use]
fn stub_content_cmd(tool: &ToolName, atpkg: &Path, kind: StubKind) -> String {
    stub_content_cmd_with(tool, atpkg, kind, &[])
}

/// [`stub_content_cmd`] with the requires marker line (behind `rem`, like the others).
#[must_use]
fn stub_content_cmd_with(
    tool: &ToolName,
    atpkg: &Path,
    kind: StubKind,
    requires: &[String],
) -> String {
    let name = tool.as_str();
    let atpkg = atpkg.to_string_lossy();
    let mut s = String::from("@echo off\r\nrem ");
    s.push_str(STUB_MARKER);
    s.push_str("\r\n");
    if kind == StubKind::Extra {
        s.push_str("rem ");
        s.push_str(EXTRA_MARKER);
        s.push_str("\r\n");
    }
    if let Some(line) = requires_marker_line(requires) {
        s.push_str("rem ");
        s.push_str(&line);
        s.push_str("\r\n");
    }
    s.push_str("rem Replaced by the real shim when the program installs.\r\n");
    s.push_str(&format!(
        "if exist \"{atpkg}\" (\r\n  \"{atpkg}\" __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
    ));
    s.push_str(&format!(
        "where atpkg >nul 2>nul\r\nif not errorlevel 1 (\r\n  atpkg __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
    ));
    s.push_str(&format!(
        "echo {STUB_UNREACHABLE_MSG} 1>&2\r\nexit /b 127\r\n"
    ));
    s
}

/// Whether `name` can ride a batch script without becoming syntax:
/// [`crate::store::shim_allowed`] admits quotes, `%`, carets and ampersands —
/// the sh body neutralizes those with single-quoting, but cmd.exe has no
/// robust equivalent (`%VAR%` expands even inside double quotes). A name that
/// cannot be embedded inertly gets NO stub on Windows (fail closed: a missing
/// courtesy stub costs one "command not found"; an injectable script costs
/// arbitrary execution under the user's account). Real roster names are
/// `[a-z0-9-]` and all pass.
#[must_use]
fn cmd_stub_name_safe(name: &str) -> bool {
    !name.chars().any(|c| {
        matches!(
            c,
            '"' | '%' | '^' | '&' | '<' | '>' | '|' | '!' | '\r' | '\n'
        )
    })
}

/// The co-located `atpkg` alias beside the running executable — fallback 1's
/// embedded path. Canonicalized so an argv0 alias (`atpkg` → `aterm`) or a
/// `~/.local/bin` symlink resolves to the real bundle before the sibling join.
fn embedded_atpkg_path() -> std::path::PathBuf {
    // `EXE_SUFFIX` (".exe" on Windows, "" elsewhere): a bare `atpkg` join
    // embedded a path that exists on no Windows install — the same probe bug
    // the GUI's co-located resolver fixed — so fallback 1 always missed there
    // and every stub run leaned on PATH luck.
    let atpkg = format!("atpkg{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&atpkg)))
        .unwrap_or_else(|| std::path::PathBuf::from(atpkg))
}

/// Whether `line` is `marker` in either spelling — bare (the sh body) or behind a
/// batch `rem` (the `.cmd` body). Both are recognized on every platform, so a store
/// migrated across platforms (or a stub laid by the old sh-everywhere writer on
/// Windows) still reconciles instead of squatting.
fn is_marker_line(line: &str, marker: &str) -> bool {
    let l = line.trim();
    l == marker
        || l.strip_prefix("rem ")
            .is_some_and(|rest| rest.trim() == marker)
}

/// The kind of the pending stub at `path`, or `None` when it is not a stub THIS
/// module wrote — the recognition gate for rewrite and removal. Bounded,
/// symlink-refusing read; anything else (absent, a real shim symlink, a tombstone,
/// a hand-made file) answers `None`.
#[must_use]
pub fn stub_kind(path: &Path) -> Option<StubKind> {
    // A real Unix-era symlink shim is not even a regular file; the bounded reader
    // refuses it before content is considered.
    let text = crate::metadata_io::read_bounded_regular_utf8(path, 64 * 1024).ok()?;
    let mut stub = false;
    let mut extra = false;
    for l in text.lines() {
        if is_marker_line(l, STUB_MARKER) {
            stub = true;
        } else if is_marker_line(l, EXTRA_MARKER) {
            extra = true;
        }
    }
    // The extra line without the stub marker is nobody's file: not a stub.
    stub.then_some(if extra {
        StubKind::Extra
    } else {
        StubKind::DefaultSet
    })
}

/// Whether the file at `path` is a pending stub THIS module wrote (of either kind).
#[must_use]
pub fn is_pending_stub(path: &Path) -> bool {
    stub_kind(path).is_some()
}

/// The `requires` names the pending stub at `path` carries on its requires marker line
/// (in the stub's order), each re-admitted through the [`ToolName`] gate; empty for a
/// stub with no such line, and for anything that is not a stub this module wrote.
#[must_use]
pub fn stub_requires(path: &Path) -> Vec<String> {
    if stub_kind(path).is_none() {
        return Vec::new();
    }
    let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(path, 64 * 1024) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        let bare = l.strip_prefix("rem ").map_or(l, str::trim);
        let Some(rest) = bare.strip_prefix(REQUIRES_MARKER) else {
            continue;
        };
        for name in rest.split_whitespace() {
            if let Some(t) = ToolName::new(name)
                && !out.contains(&t.as_str().to_string())
            {
                out.push(t.as_str().to_string());
            }
        }
    }
    out
}

/// [`stub_requires`] for the stub `tool` currently resolves to in this layout.
#[must_use]
pub fn pending_stub_requires(layout: &Layout, tool: &str) -> Vec<String> {
    ToolName::new(tool).map_or_else(Vec::new, |t| stub_requires(&layout.shim(&t)))
}

/// The kind of the pending stub `tool` currently resolves to in this layout, or
/// `None` when it resolves to no stub (absent, a real shim, a tombstone…).
#[must_use]
pub fn pending_stub_kind(layout: &Layout, tool: &str) -> Option<StubKind> {
    ToolName::new(tool).and_then(|t| stub_kind(&layout.shim(&t)))
}

/// Whether `tool` currently resolves to a pending stub in this layout — the front
/// door's second arm (`store_resolves || pending_stub_exists`).
#[must_use]
pub fn pending_stub_exists(layout: &Layout, tool: &str) -> bool {
    pending_stub_kind(layout, tool).is_some()
}

/// Lay (or refresh) the DEFAULT-SET pending stub for `tool` — see
/// [`write_pending_stub_kind`].
pub fn write_pending_stub(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    write_pending_stub_kind(layout, tool, StubKind::DefaultSet)
}

/// Lay (or refresh) the pending stub for `tool` as `kind`, atomically (temp `0755` +
/// `rename(2)`, the tombstone writer's shape). NEVER over anything that is not
/// already a pending stub: a resolvable shim, a tombstone, or any unrecognized file
/// wins and the write is a clean no-op — the stub is the lowest-precedence occupant
/// of the name. A stub of the OTHER kind is rewritten (the reconcile is how an
/// index flag change reaches the file).
pub fn write_pending_stub_kind(layout: &Layout, tool: &ToolName, kind: StubKind) -> io::Result<()> {
    write_pending_stub_with(layout, tool, kind, &[])
}

/// [`write_pending_stub_kind`] carrying the program's `requires` (from the signed index)
/// on the stub's requires marker line, so the consent question can name them offline.
pub fn write_pending_stub_with(
    layout: &Layout,
    tool: &ToolName,
    kind: StubKind,
    requires: &[String],
) -> io::Result<()> {
    if cfg!(windows) && !cmd_stub_name_safe(tool.as_str()) {
        // See `cmd_stub_name_safe`: no inert embedding exists, so no stub.
        return Ok(());
    }
    if tool.is_alias() {
        // An alias is never a stub (module doc): it names the managed copy once
        // installed, and the plain name's stub already answers until then.
        return Ok(());
    }
    let shim = layout.shim(tool);
    match std::fs::symlink_metadata(&shim) {
        Err(_) => {}                          // absent: ours to claim
        Ok(_) if is_pending_stub(&shim) => {} // ours: rewrite refreshes path + kind
        Ok(_) => return Ok(()), // someone else's file (shim/tombstone/hand-made): never touch
    }
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    let body = stub_content_with(tool, &embedded_atpkg_path(), kind, requires);
    write_executable_atomic(&shim, &body)
}

/// Temp+rename an executable script onto `dest` (the tombstone writer's discipline,
/// restated here because that helper hard-codes its own body).
fn write_executable_atomic(dest: &Path, body: &str) -> io::Result<()> {
    let file_name = dest
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stub has no file name"))?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(file_name);
    tmp_name.push_str(".stub-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = dest.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    crate::call2(std::fs::write, tmp.as_path(), body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Remove `program`'s stub iff the name still resolves to a pending stub — the
/// per-program removal discipline (`atpkg uninstall <p>`, an index de-listing).
pub fn remove_stub(layout: &Layout, program: &str) {
    if let Some(tool) = ToolName::new(program) {
        let shim = layout.shim(&tool);
        if is_pending_stub(&shim) {
            let _ = std::fs::remove_file(&shim);
        }
    }
}

/// Remove EVERY pending stub in `bin/` — `uninstall --all` / a recorded decline.
/// Recognition-gated per file, so nothing that is not a stub can be swept.
pub fn remove_all_stubs(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_pending_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Lay the compile-time rosters' stubs at ADOPTION time — before a single network
/// byte moves, so `PATH` coverage exists from the first instant the machine wants
/// the toolset: the default set as [`StubKind::DefaultSet`], the extras as
/// [`StubKind::Extra`] (typing one asks; it never auto-installs). Skips programs
/// already installed and ones the user removed on purpose; failures are per-name
/// and best-effort (a stub is a courtesy, never a gate on the pass). The compiled
/// rosters carry no `system` key, so system satisfaction is the index reconcile's
/// call — the first resolve retires any stub a system install makes moot.
pub fn lay_adoption_stubs(layout: &Layout) {
    let installed = crate::ops::active_builds(layout);
    let removed = layout.removed_programs();
    let rosters = DEFAULT_SET_STUB_NAMES
        .iter()
        .map(|(n, _)| (*n, StubKind::DefaultSet))
        .chain(EXTRAS_STUB_NAMES.iter().map(|(n, _)| (*n, StubKind::Extra)));
    for (name, kind) in rosters {
        if installed.contains_key(name) || removed.contains(name) {
            continue;
        }
        if let Some(tool) = ToolName::new(name) {
            let _ = write_pending_stub_kind(layout, &tool, kind);
        }
    }
}

/// The index-resolve reconcile: `wanted` is the SIGNED set the pass will keep
/// complete (installable ∧ not-removed — opted-in extras included), `extras` every
/// index-listed EXTRA that may carry a stub (listed, not removed on purpose, not
/// system-satisfied, servable here — the caller's filters), `installed` the active
/// builds. Adds/refreshes a stub for every wanted-or-extra absent name (embedded
/// paths refreshed as a matter of course; an extra's stub is laid as
/// [`StubKind::Extra`] whether or not it is opted in, since the KIND says what the
/// program is and the opt-in marker says what the user answered), then sweeps every
/// pending stub whose name is no longer in either set and missing — de-listed,
/// removed on purpose, or now installed under a name its real shims do not expose.
pub fn reconcile(
    layout: &Layout,
    wanted: &BTreeSet<String>,
    extras: &BTreeSet<String>,
    installed: &BTreeMap<String, u64>,
) {
    reconcile_with_requires(layout, wanted, extras, installed, &BTreeMap::new());
}

/// [`reconcile`] with the signed index's `requires` per program (`requires_of`, absent ⇒
/// none), written onto each stub's requires marker line so an extra's consent question
/// lists what it also needs ([`pending_stub_requires`]).
pub fn reconcile_with_requires(
    layout: &Layout,
    wanted: &BTreeSet<String>,
    extras: &BTreeSet<String>,
    installed: &BTreeMap<String, u64>,
    requires_of: &BTreeMap<String, Vec<String>>,
) {
    let mut keep: BTreeSet<&str> = BTreeSet::new();
    for name in wanted.union(extras) {
        if installed.contains_key(name.as_str()) {
            continue;
        }
        // The ToolName gate IS the sensitive-name refusal: an index listing `sudo`
        // gets no stub, exactly as it gets no shim.
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        let kind = if extras.contains(name.as_str()) {
            StubKind::Extra
        } else {
            StubKind::DefaultSet
        };
        let requires: &[String] = requires_of.get(name.as_str()).map_or(&[], Vec::as_slice);
        if write_pending_stub_with(layout, &tool, kind, requires).is_ok() {
            keep.insert(name.as_str());
        }
    }
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_pending_stub(&path) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let logical = ToolName::from_shim_file(name);
        let stays = logical.as_ref().is_some_and(|t| keep.contains(t.as_str()));
        if !stays {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-stub-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    /// Every compiled roster name passes the shim gate — a roster entry that could
    /// not be shimmed could never be stubbed either, and should fail HERE, not at a
    /// user's first launch.
    #[test]
    fn roster_names_all_pass_shim_allowed() {
        for (name, desc) in DEFAULT_SET_STUB_NAMES {
            assert!(
                crate::store::shim_allowed(name),
                "{name} would be refused as a shim"
            );
            assert!(!desc.is_empty(), "{name} needs its authored line");
        }
    }

    /// An alias is never a stub (module doc): the reconcile lays the PLAIN names only,
    /// and the writer is a no-op for an `alab-` name even when asked directly.
    #[test]
    fn an_alias_is_never_a_stub() {
        let l = layout("alias-stub");
        let wanted: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
        reconcile(&l, &wanted, &BTreeSet::new(), &BTreeMap::new());
        assert!(
            pending_stub_exists(&l, "trust"),
            "the plain name is stubbed"
        );
        assert!(
            std::fs::symlink_metadata(l.shim(&tool("alab-trust"))).is_err(),
            "no stub is laid under the alias name"
        );
        assert_eq!(pending_stub_kind(&l, "alab-trust"), None);
        write_pending_stub(&l, &tool("alab-trust")).unwrap();
        assert!(
            std::fs::symlink_metadata(l.shim(&tool("alab-trust"))).is_err(),
            "the writer is a no-op for an alias"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The stub body: marker present, the three-step fallback chain in order, the
    /// tool name and embedded path quote-safely embedded — and CRUCIALLY it parses
    /// as NO shim target, so `which`/`active_builds`/`prune_stale_shims` never
    /// mistake a stub for an installed tool.
    #[test]
    fn stub_content_shape_and_shim_invisibility() {
        // The sh body directly — `stub_content` dispatches by compile target,
        // and this shape must stay pinned from every build host.
        let body = stub_content_sh(
            &tool("trust"),
            Path::new("/Apps/aterm.app/Contents/MacOS/atpkg"),
            StubKind::DefaultSet,
        );
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains(STUB_MARKER));
        assert!(
            !body.contains(EXTRA_MARKER),
            "a default-set stub carries no extra line"
        );
        let atpkg_pos = body.find("[ -x \"$ATPKG\" ]").unwrap();
        let command_v = body.find("command -v atpkg").unwrap();
        // The message embeds sh-quoted (its apostrophe becomes '\''), so probe a
        // quote-free distinctive slice of it.
        let static_msg = body.find("package manager is not reachable").unwrap();
        assert!(
            atpkg_pos < command_v && command_v < static_msg,
            "fallbacks in order"
        );
        assert!(body.contains("__pending 'trust'"));
        assert!(body.trim_end().ends_with("exit 127"));
        assert_eq!(
            crate::platform::parse_sh_shim_target(&body),
            None,
            "a stub must never parse as a store shim"
        );
        // Quote-safety: a name with an embedded quote cannot break out.
        let nasty = ToolName::new("a'b").expect("shim_allowed admits quotes");
        let body = stub_content_sh(&nasty, Path::new("/x'y/atpkg"), StubKind::DefaultSet);
        assert!(body.contains("__pending 'a'\\''b'"));
        assert!(body.contains("ATPKG='/x'\\''y/atpkg'"));
    }

    /// The batch twin's shape: what cmd.exe actually runs on Windows, where
    /// the shim slot is `<tool>.cmd` — the old sh-everywhere writer put
    /// `#!/bin/sh` there and the pending promise rendered as `'#!' is not
    /// recognized…` garbage. Pinned from every build host (pure string).
    #[test]
    fn cmd_stub_shape_marker_and_safety() {
        let body = stub_content_cmd(
            &tool("trust"),
            Path::new(r"C:\Program Files\aterm\atpkg.exe"),
            StubKind::DefaultSet,
        );
        assert!(body.starts_with("@echo off\r\n"), "batch, not sh: {body}");
        assert!(!body.contains("#!/bin/sh"), "no POSIX in a .cmd file");
        let exist = body.find("if exist").unwrap();
        let where_probe = body.find("where atpkg").unwrap();
        let static_msg = body.find("package manager is not reachable").unwrap();
        assert!(
            exist < where_probe && where_probe < static_msg,
            "fallbacks in order"
        );
        assert!(body.contains("__pending \"trust\""));
        assert!(
            body.matches("exit /b 127").count() >= 3,
            "every arm exits 127"
        );
        // Recognition round-trips through the `rem` spelling.
        assert!(
            body.lines().any(|l| l
                .trim()
                .strip_prefix("rem ")
                .is_some_and(|r| r.trim() == STUB_MARKER)),
            "the marker rides a rem line"
        );

        // The batch-hostility gate: sh can neutralize these, batch cannot —
        // and `shim_allowed` admits them, so the WRITE must refuse.
        for hostile in ["a%b", "a\"b", "a&b", "a^b", "a|b", "a<b", "a!b"] {
            assert!(
                !cmd_stub_name_safe(hostile),
                "{hostile:?} has no inert batch embedding"
            );
        }
        for fine in ["trust", "ay", "clean-2", "a'b", "a b"] {
            assert!(cmd_stub_name_safe(fine), "{fine:?} is batch-inert quoted");
        }
    }

    /// A stub written with the batch marker is still a stub to every consumer:
    /// recognition (and therefore rewrite/removal/reconcile) must accept both
    /// spellings on both platforms, or a store migrated across platforms
    /// squats its own bin dir.
    #[test]
    fn rem_marker_recognition_round_trips() {
        let dir = std::env::temp_dir().join(format!("atpkg-stub-rem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trust.cmd");
        std::fs::write(
            &path,
            stub_content_cmd(
                &tool("trust"),
                Path::new(r"C:\x\atpkg.exe"),
                StubKind::DefaultSet,
            ),
        )
        .unwrap();
        assert!(is_pending_stub(&path), "batch stub recognized");
        let _ = std::fs::remove_file(&path);
    }

    /// Laid stub → recognized; real-shim install lands OVER it via temp+rename with
    /// the name resolving to SOMETHING at every instant (no window of absence, no
    /// EEXIST), and afterwards the stub is gone and the shim is exactly today's
    /// fast path.
    #[cfg(unix)]
    #[test]
    fn install_over_a_stub_never_leaves_a_window() {
        let l = layout("exec-through");
        let t = tool("trust");
        write_pending_stub(&l, &t).unwrap();
        let shim = l.shim(&t);
        assert!(is_pending_stub(&shim));
        assert!(pending_stub_exists(&l, "trust"));
        assert_eq!(crate::platform::resolve_shim(&shim), None);
        // The real build lands.
        let build_bin = l.prefix.join("store/trust/210/bin");
        std::fs::create_dir_all(&build_bin).unwrap();
        std::fs::write(build_bin.join("trust"), "#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::install_shim(&build_bin, &t, &shim)
            .expect("install over a stub must not EEXIST");
        // After: the SAME name resolves to the store target; the stub is gone.
        let target = crate::platform::resolve_shim(&shim).expect("real shim resolves");
        assert!(target.starts_with(&build_bin));
        assert!(!is_pending_stub(&shim));
        // At every instant: the write path is temp+rename, so the only two
        // observable states are the two proven above — pin the mechanism by
        // asserting no non-stub temp survives beside the shim.
        let strays: Vec<_> = std::fs::read_dir(l.bin_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "trust")
            .collect();
        assert!(
            strays.is_empty(),
            "temp+rename leaves nothing beside the shim: {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The stub is the LOWEST-precedence occupant: it never overwrites a real shim,
    /// a tombstone, or a hand-made file — and removal is recognition-gated the same
    /// way.
    #[cfg(unix)]
    #[test]
    fn stub_never_clobbers_and_removal_is_gated() {
        let l = layout("no-clobber");
        let t = tool("ty");
        let shim = l.shim(&t);
        l.ensure_dir(&l.bin_dir()).unwrap();
        std::fs::write(&shim, "#!/bin/sh\n# someone's own file\nexit 3\n").unwrap();
        write_pending_stub(&l, &t).unwrap();
        assert!(
            std::fs::read_to_string(&shim)
                .unwrap()
                .contains("someone's own file"),
            "a foreign file wins over the stub"
        );
        remove_stub(&l, "ty");
        assert!(
            shim.exists(),
            "removal only removes what the marker proves ours"
        );
        // A tombstone survives too.
        crate::activate::install_tombstone_shim(&l, &t).unwrap();
        write_pending_stub(&l, &t).unwrap();
        assert!(!is_pending_stub(&shim), "a tombstone outranks a stub");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Adoption lays the whole roster minus removed/installed; `remove_all_stubs`
    /// clears exactly the stubs.
    #[cfg(unix)]
    #[test]
    fn adoption_lays_the_roster_and_remove_all_clears_it() {
        let l = layout("adoption");
        std::fs::write(l.removed(), "ny\n").unwrap();
        lay_adoption_stubs(&l);
        for (name, _) in DEFAULT_SET_STUB_NAMES {
            let expect = *name != "ny";
            assert_eq!(
                pending_stub_exists(&l, name),
                expect,
                "{name}: removed-on-purpose stays removed at adoption"
            );
        }
        // The extras ride along at adoption, as extras.
        for (name, _) in EXTRAS_STUB_NAMES {
            assert_eq!(
                pending_stub_kind(&l, name),
                Some(StubKind::Extra),
                "{name}: laid at adoption, marked as an extra"
            );
        }
        // A foreign file beside them survives the sweep.
        std::fs::write(l.bin_dir().join("mine"), "not a stub").unwrap();
        remove_all_stubs(&l);
        for (name, _) in DEFAULT_SET_STUB_NAMES.iter().chain(EXTRAS_STUB_NAMES) {
            assert!(!pending_stub_exists(&l, name), "{name} swept with the rest");
        }
        assert!(l.bin_dir().join("mine").exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The index reconcile: newly listed names gain stubs, de-listed ones lose
    /// them, sensitive names are refused, and installed programs need no stub.
    #[cfg(unix)]
    #[test]
    fn reconcile_tracks_the_signed_set_and_refuses_sensitive_names() {
        let l = layout("reconcile");
        lay_adoption_stubs(&l);
        assert!(pending_stub_exists(&l, "ay"));
        let wanted: BTreeSet<String> = [
            "brandnew".to_string(),
            "trust".to_string(),
            "sudo".to_string(),
        ]
        .into_iter()
        .collect();
        let installed: BTreeMap<String, u64> = BTreeMap::new();
        reconcile(&l, &wanted, &BTreeSet::new(), &installed);
        assert!(
            pending_stub_exists(&l, "brandnew"),
            "newly listed name gains a stub"
        );
        assert!(pending_stub_exists(&l, "trust"));
        assert!(
            !pending_stub_exists(&l, "sudo"),
            "the sensitive-name refusal binds stubs"
        );
        assert!(
            !pending_stub_exists(&l, "ay"),
            "a de-listed name loses its stub"
        );
        // Installed ⇒ stub retired even if the real shims expose other names.
        let installed: BTreeMap<String, u64> = [("trust".to_string(), 210)].into_iter().collect();
        reconcile(&l, &wanted, &BTreeSet::new(), &installed);
        assert!(!pending_stub_exists(&l, "trust"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The fallback chain END-TO-END, run through a real `/bin/sh`: a live embedded
    /// atpkg wins; a dangling embedded path falls back to `command -v atpkg`; with
    /// neither reachable the static honest message lands on stderr with exit 127.
    #[cfg(unix)]
    #[test]
    fn fallback_chain_runs_in_a_real_shell() {
        let l = layout("fallback");
        let t = tool("trust");
        // Fake atpkg records how it was invoked.
        let fake = l.prefix.join("fake-atpkg");
        std::fs::write(&fake, "#!/bin/sh\necho \"co-located: $*\"\nexit 127\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = |body: &str, path_env: &str| {
            let script = l.prefix.join("stub-under-test");
            std::fs::write(&script, body).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::process::Command::new(&script)
                .env("PATH", path_env)
                .output()
                .unwrap()
        };
        // 1: embedded path exists and is executable.
        let out = run(
            &stub_content(&t, &fake, StubKind::DefaultSet),
            "/nonexistent",
        );
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "co-located: __pending trust\n"
        );
        // 2: embedded dangles; PATH carries an `atpkg`.
        let path_dir = l.prefix.join("pathbin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::write(
            path_dir.join("atpkg"),
            "#!/bin/sh\necho \"path: $*\"\nexit 127\n",
        )
        .unwrap();
        std::fs::set_permissions(
            path_dir.join("atpkg"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let out = run(
            &stub_content(
                &t,
                Path::new("/gone/after/relocation/atpkg"),
                StubKind::DefaultSet,
            ),
            path_dir.to_str().unwrap(),
        );
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "path: __pending trust\n"
        );
        // 3: nothing reachable — the static honest message, exit 127.
        let out = run(
            &stub_content(&t, Path::new("/gone/atpkg"), StubKind::DefaultSet),
            "/nonexistent",
        );
        assert_eq!(out.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&out.stderr).contains(STUB_UNREACHABLE_MSG));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The EXTRAS roster: every name passes the shim gate (a roster entry that could
    /// not be shimmed could never be stubbed either), carries the vendor-named
    /// consent copy, and NEVER collides with a default-set name — one name, one
    /// kind, or the two rosters would disagree about whether typing it asks.
    #[test]
    fn extras_roster_names_pass_shim_allowed_and_never_collide_with_the_default_set() {
        for (name, desc) in EXTRAS_STUB_NAMES {
            assert!(
                crate::store::shim_allowed(name),
                "{name} would be refused as a shim"
            );
            assert!(
                desc.contains("downloaded from"),
                "{name}: the consent copy must name where the bytes come from: {desc}"
            );
            assert!(
                DEFAULT_SET_STUB_NAMES.iter().all(|(n, _)| n != name),
                "{name} is in both rosters"
            );
            assert!(compiled_extra(name));
            assert_eq!(describe(name), Some(*desc), "describe covers the extras");
        }
        assert!(!compiled_extra("trust"));
        assert!(!compiled_extra("nonesuch"));
        // The consent copy is descriptive of the vendor, never an ALab mark.
        assert!(describe("codex").unwrap().starts_with("OpenAI "));
        assert!(describe("claude").unwrap().starts_with("Anthropic "));
    }

    /// An extra's stub carries its kind in BOTH spellings (sh and batch), the kind
    /// round-trips through the on-disk reader, and — like every stub — it parses as
    /// NO shim target. The extra line alone (no stub marker) is nobody's file.
    /// The requires marker (§17.10): written from the index's `requires`, read back
    /// through the `ToolName` gate in both spellings; a stub without the line requires
    /// nothing; a name the gate refuses (`sudo`) is dropped, never echoed.
    #[test]
    fn a_stub_carries_its_requires_and_reads_them_back_through_the_name_gate() {
        let reqs = vec![
            "clt".to_string(),
            "sudo".to_string(),
            "brew".to_string(),
            "clt".to_string(),
        ];
        let sh = stub_content_sh_with(
            &tool("codex"),
            Path::new("/x/atpkg"),
            StubKind::Extra,
            &reqs,
        );
        assert!(
            sh.contains("\n# atpkg requires: clt brew\n"),
            "the admissible names, deduplicated, in order: {sh}"
        );
        assert!(
            !sh.contains("sudo"),
            "a refused name is never written: {sh}"
        );
        let cmd = stub_content_cmd_with(
            &tool("codex"),
            Path::new("C:\\x\\atpkg.exe"),
            StubKind::Extra,
            &reqs,
        );
        assert!(
            cmd.contains("\r\nrem # atpkg requires: clt brew\r\n"),
            "{cmd}"
        );
        let l = layout("stub-requires");
        let bin = l.bin_dir();
        std::fs::create_dir_all(&bin).unwrap();
        for (label, body) in [("sh", &sh), ("cmd", &cmd)] {
            let p = bin.join(format!("codex-{label}"));
            std::fs::write(&p, body).unwrap();
            assert_eq!(stub_kind(&p), Some(StubKind::Extra));
            assert_eq!(
                stub_requires(&p),
                vec!["clt".to_string(), "brew".to_string()],
                "{label}"
            );
        }
        // No line ⇒ nothing required; not a stub ⇒ nothing, whatever the file says.
        let plain = bin.join("plain");
        std::fs::write(
            &plain,
            stub_content_sh(&tool("codex"), Path::new("/x/atpkg"), StubKind::Extra),
        )
        .unwrap();
        assert!(stub_requires(&plain).is_empty());
        let foreign = bin.join("foreign");
        std::fs::write(&foreign, "#!/bin/sh\n# atpkg requires: clt\nexit 0\n").unwrap();
        assert!(
            stub_requires(&foreign).is_empty(),
            "not our stub: no marker"
        );
        // The reconcile writes the line from the index's relation and the layout
        // reader gives it back; an extra with no relation gets no line.
        let none = BTreeMap::new();
        let mut requires_of = BTreeMap::new();
        requires_of.insert("codex".to_string(), vec!["clt".to_string()]);
        let extras: BTreeSet<String> = ["codex".to_string(), "claude".to_string()]
            .into_iter()
            .collect();
        reconcile_with_requires(&l, &BTreeSet::new(), &extras, &none, &requires_of);
        assert_eq!(pending_stub_requires(&l, "codex"), vec!["clt".to_string()]);
        assert!(pending_stub_requires(&l, "claude").is_empty());
        assert_eq!(pending_stub_kind(&l, "codex"), Some(StubKind::Extra));
        // A later reconcile with the relation gone rewrites the stub without the line.
        reconcile_with_requires(&l, &BTreeSet::new(), &extras, &none, &BTreeMap::new());
        assert!(pending_stub_requires(&l, "codex").is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn extra_stub_carries_its_kind_in_both_spellings() {
        let sh = stub_content_sh(&tool("codex"), Path::new("/x/atpkg"), StubKind::Extra);
        assert!(sh.contains(STUB_MARKER) && sh.contains(EXTRA_MARKER));
        assert!(sh.contains("__pending 'codex'"));
        assert_eq!(crate::platform::parse_sh_shim_target(&sh), None);
        let cmd = stub_content_cmd(
            &tool("codex"),
            Path::new(r"C:\x\atpkg.exe"),
            StubKind::Extra,
        );
        assert!(
            cmd.lines().any(|l| is_marker_line(l, EXTRA_MARKER)),
            "the batch body carries the extra line behind a rem: {cmd}"
        );
        let dir = std::env::temp_dir().join(format!("atpkg-stub-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, body, expect) in [
            ("codex", sh.as_str(), Some(StubKind::Extra)),
            ("codex.cmd", cmd.as_str(), Some(StubKind::Extra)),
            (
                "trust",
                &stub_content_sh(&tool("trust"), Path::new("/x/atpkg"), StubKind::DefaultSet),
                Some(StubKind::DefaultSet),
            ),
            (
                "orphan",
                "#!/bin/sh\n# atpkg extra: asks consent before installing\nexit 0\n",
                None,
            ),
            ("mine", "#!/bin/sh\nexit 3\n", None),
        ] {
            let path = dir.join(file);
            std::fs::write(&path, body).unwrap();
            assert_eq!(stub_kind(&path), expect, "{file}");
            assert_eq!(is_pending_stub(&path), expect.is_some(), "{file}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reconcile keeps an extra's stub beside the wanted set WITHOUT it being
    /// wanted (an extra is never in the default set), marks it as an extra even
    /// when it IS wanted (opted in — the kind says what the program is, the marker
    /// says what the user answered), flips a stub's kind when the index flag
    /// changes, and retires an extra's stub once it installs or is no longer listed.
    #[cfg(unix)]
    #[test]
    fn reconcile_keeps_extras_beside_the_wanted_set_and_flips_kind() {
        let l = layout("reconcile-extras");
        let wanted: BTreeSet<String> = ["ay".to_string(), "claude".to_string()]
            .into_iter()
            .collect();
        let extras: BTreeSet<String> = ["codex".to_string(), "claude".to_string()]
            .into_iter()
            .collect();
        let none: BTreeMap<String, u64> = BTreeMap::new();
        reconcile(&l, &wanted, &extras, &none);
        assert_eq!(pending_stub_kind(&l, "ay"), Some(StubKind::DefaultSet));
        assert_eq!(
            pending_stub_kind(&l, "codex"),
            Some(StubKind::Extra),
            "an extra keeps its stub without being wanted"
        );
        assert_eq!(
            pending_stub_kind(&l, "claude"),
            Some(StubKind::Extra),
            "an opted-in extra (wanted) is still marked as an extra"
        );
        // The index flag flips: codex becomes a default-set member, claude is
        // de-listed as an extra and not wanted either.
        let wanted: BTreeSet<String> = ["ay".to_string(), "codex".to_string()]
            .into_iter()
            .collect();
        reconcile(&l, &wanted, &BTreeSet::new(), &none);
        assert_eq!(
            pending_stub_kind(&l, "codex"),
            Some(StubKind::DefaultSet),
            "the rewrite carries the new kind"
        );
        assert!(
            !pending_stub_exists(&l, "claude"),
            "neither wanted nor an extra: swept"
        );
        // Installed ⇒ the extra's stub retires, exactly like a member's.
        let installed: BTreeMap<String, u64> =
            [("codex".to_string(), 2026082601)].into_iter().collect();
        reconcile(&l, &wanted, &extras, &installed);
        assert!(!pending_stub_exists(&l, "codex"));
        assert_eq!(pending_stub_kind(&l, "claude"), Some(StubKind::Extra));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
