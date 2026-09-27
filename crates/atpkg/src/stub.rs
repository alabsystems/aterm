// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending-program stubs (R6): from the moment the lean app first launches (and
//! immediately after adoption), EVERY default-set program name resolves on `PATH`,
//! and running one is always helpful — never "command not found". (The EXTRAS tier and
//! its consent stubs were retired 2026-09-24, design 2026-09-22 §5.3(c): every program
//! the signed index names is default-set, so a stub always bumps an install that is
//! already coming. A stub an older client laid with its extra or requires marker line
//! is still a stub — recognition is the first marker alone — and is re-laid plain.)
//!
//! A stub is a tiny `/bin/sh` file at `<prefix>/bin/<tool>` (the same seam
//! [`crate::flow`]'s tombstone shims use — no parallel machinery) that execs
//! `atpkg __pending <tool>`: a short honest message with the LIVE install state, plus
//! a bump of that program to the front of the install queue — and, for a person at a
//! terminal while a pass installs it, a wait under the progress meter that runs the
//! program the moment it lands. It grants nothing —
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
//! 1. the embedded atpkg path, if it exists and is executable;
//! 2. `command -v atpkg` (the `~/.local/bin` alias / shell hook);
//! 3. a static honest message — and exit 127.
//!
//! The per-launch seed-pass reconcile REWRITES stubs whose bytes changed, refreshing
//! embedded paths as a matter of course; a byte-identical stub is left alone, so a
//! steady-state pass lays nothing.
//!
//! # Which atpkg a laid file names ([`Embedder`])
//!
//! The same rule for every file this crate lays with an atpkg path in it — these stubs, the
//! reroute stubs ([`crate::reroute::lay`]) and the `agents/` twins' self-update block
//! ([`crate::activate::reconcile_agents`]). The process laying it offers the `atpkg` beside
//! its own executable ([`co_located_atpkg_path`]); the file may already name one. Then:
//!
//! * **a path that is not there is never written.** A process whose co-located `atpkg` does
//!   not exist offers nothing. With nothing live on either side the file names none (an
//!   empty value, which fallback 1's `[ -x ]` refuses), and the next fallback answers.
//! * **a live path is displaced only by one that lasts at least as long** ([`Durability`]):
//!   an app bundle outranks a plain binary, which outranks a transient copy — one in a
//!   temporary directory, a cargo target directory, Gatekeeper's translocated copy or a
//!   mounted disk image. A path that is gone is displaced by anything live.
//!
//! Measured 2026-09-25, the end-to-end run's defect D1: a copy of aterm started from a
//! scratch directory (no `atpkg` beside it) re-laid the machine's eight reroute stubs naming
//! `<scratch>/bin/atpkg` — never there, and deleted with the scratch directory — over stubs
//! naming the installed app's. The same lay runs at every launch, every session spawn and
//! every pass, so every copy of aterm, wherever it ran, re-pointed the machine's files at
//! the `atpkg` beside it — there or not.
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
//! (it bumps), and a second promising name on `PATH` would double
//! every "not found" into two courtesy scripts for one program. So the reconcile
//! never lays `alab-*`, and `write_pending_stub` is a no-op for an alias
//! name — the alias appears with the real shims, and only then.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

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

/// THE AGENT PROGRAMS (owner decision 2026-09-10): the coding agents aterm is the
/// version manager for. Default-set members like every program the index names (an index
/// that still marks them `extra = true` — build 21 does — reads the same since the key
/// was retired). What is theirs alone, each enforced where it lives:
///
/// * their shims are ALSO laid under `<prefix>/agents/`, the one managed directory
///   that goes FIRST on `PATH` ([`crate::activate::lay_agent_shims`]), so the managed
///   copy is what `claude`/`codex` run — the rule-1 exception recorded in
///   docs/design/DESIGN-which-copy-runs;
/// * a pass that leaves them at the index pin says so on stdout
///   (`crate::cli::MANAGED_CURRENT_MARKER`).
///
/// Names only — the signed index pins the bytes, as for every member.
pub const AGENT_PROGRAMS: &[&str] = &["claude", "codex"];

/// Whether `name` is one of [`AGENT_PROGRAMS`].
#[must_use]
pub fn is_agent_program(name: &str) -> bool {
    AGENT_PROGRAMS.contains(&name)
}

/// The authored what-it-is line for each agent program (the pre-index stub copy). It
/// still names the vendor, the license, the rough size and the host the bytes come
/// from — descriptively ("OpenAI Codex CLI", "Anthropic Claude Code"), never as ALab
/// marks — because the pending stub prints it while the install is on its way.
pub const AGENT_STUB_NAMES: &[(&str, &str)] = &[
    (
        "claude",
        "Anthropic Claude Code — proprietary, ~200 MB, downloaded from downloads.claude.ai",
    ),
    (
        "codex",
        "OpenAI Codex CLI — Apache-2.0, ~110 MB (~290 MB on disk), downloaded from github.com/openai/codex",
    ),
];

/// The authored one-line description for `name`, if either compiled roster carries
/// one. A program published after this binary gets the honest generic line instead.
#[must_use]
pub fn describe(name: &str) -> Option<&'static str> {
    DEFAULT_SET_STUB_NAMES
        .iter()
        .chain(AGENT_STUB_NAMES)
        .find(|(n, _)| *n == name)
        .map(|(_, d)| *d)
}

/// The marker line that identifies a pending stub — the recognition gate for every
/// rewrite/removal below. Version-suffixed so a future stub format can coexist.
const STUB_MARKER: &str = "# atpkg pending-program stub v1";

/// The static last-resort message (fallback 3) — the one line the stub can always
/// say without an atpkg to ask.
pub const STUB_UNREACHABLE_MSG: &str =
    "aterm's package manager is not reachable — open aterm to finish installing";

/// Single-quote `s` for safe embedding in a `/bin/sh` script (the POSIX `'\''`
/// escape). [`crate::store::shim_allowed`] does not forbid shell metacharacters, so
/// the stub body must never let a crafted name break out of its quotes. (A local
/// twin of the platform backend's private helper — same rule, one screen away from
/// its use.)
pub(crate) fn sh_single_quote(s: &str) -> String {
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

/// The inverse of [`sh_single_quote`] (and of the platform backend's twin, the same rule):
/// the text a word made of `'…'` runs and `\'` escapes stands for, or `None` for any other
/// spelling — a value this crate did not quote is never guessed at.
fn sh_single_unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while !rest.is_empty() {
        if let Some(quoted) = rest.strip_prefix('\'') {
            let end = quoted.find('\'')?;
            out.push_str(&quoted[..end]);
            rest = &quoted[end + 1..];
        } else {
            rest = rest.strip_prefix("\\'")?;
            out.push('\'');
        }
    }
    Some(out)
}

/// The atpkg a laid `/bin/sh` body names on its `<var>=` line — `ATPKG` for a pending or a
/// reroute stub, `__atpkg` for an `agents/` twin's self-update block — or `None` when it
/// names none (no such line, a value this crate did not quote, or the empty value).
#[must_use]
pub(crate) fn sh_named_atpkg(body: &str, var: &str) -> Option<PathBuf> {
    body.lines()
        .find_map(|line| {
            let value = line.trim_start().strip_prefix(var)?.strip_prefix('=')?;
            sh_single_unquote(value.trim_end())
        })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The atpkg a laid batch stub ([`stub_content_cmd`]) names on its `if exist "…" (` line, or
/// `None` when it names none.
#[must_use]
fn cmd_named_atpkg(body: &str) -> Option<PathBuf> {
    body.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("if exist \"")?
                .strip_suffix("\" (")
                .map(str::to_owned)
        })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The atpkg the pending stub `body` names, in either spelling.
fn stub_named_atpkg(body: &str) -> Option<PathBuf> {
    sh_named_atpkg(body, "ATPKG").or_else(|| cmd_named_atpkg(body))
}

/// The stub script body for `tool`, naming `atpkg` as the fallback-1 path, or none
/// ([`Embedder::embed`] decides which). Pure, so the shape is pinned by tests.
///
/// NOTE the `exec "$ATPKG"` spelling: `platform::parse_sh_shim_target` recognizes
/// real shims by a trimmed line starting `exec '`, so the stub deliberately execs
/// through a variable — a stub must resolve as NO store target (`resolve_shim` ⇒
/// `None`), or `active_builds`/`which`/the front door would mistake it for an
/// installed tool.
#[must_use]
fn stub_content(tool: &ToolName, atpkg: Option<&Path>) -> String {
    let atpkg = named(atpkg);
    if cfg!(windows) {
        stub_content_cmd(tool, atpkg)
    } else {
        stub_content_sh(tool, atpkg)
    }
}

/// How a renderer is told to name no atpkg: the EMPTY path. The `/bin/sh` bodies render it
/// `ATPKG=''` / `__atpkg=''`, a value every body's `[ -x ]` refuses, so the body goes
/// straight to its next fallback; the batch body leaves its `if exist` block out.
#[must_use]
pub(crate) fn named(atpkg: Option<&Path>) -> &Path {
    atpkg.unwrap_or_else(|| Path::new(""))
}

/// The POSIX `/bin/sh` stub body (macOS/Linux — the shim there is a plain
/// executable file).
#[must_use]
fn stub_content_sh(tool: &ToolName, atpkg: &Path) -> String {
    let name = sh_single_quote(tool.as_str());
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push('\n');
    s.push_str("# Replaced by the real shim when the program installs.\nATPKG=");
    s.push_str(&sh_single_quote(&atpkg.to_string_lossy()));
    // The caller's arguments ride along (`"$@"`, 2026-09-15): an agent program's stub
    // execs the user's own copy meanwhile ([`crate::cli::pending_passthrough`]), and
    // `claude -p 'x'` typed during the install must reach it whole.
    s.push_str("\nif [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending ");
    s.push_str(&name);
    s.push_str(" \"$@\"\nfi\nif command -v atpkg >/dev/null 2>&1; then\n  exec atpkg __pending ");
    s.push_str(&name);
    s.push_str(" \"$@\"\nfi\nprintf '%s\\n' ");
    s.push_str(&sh_single_quote(STUB_UNREACHABLE_MSG));
    s.push_str(" 1>&2\nexit 127\n");
    s
}

/// The first line of the batch stub's body, `@echo off` and its CRLF: 11 bytes. The
/// tightest case of the frame's head argument: `cmd` resumes a rewritten batch file only
/// after a line end of the OLD file, and the only old line end that can fall inside the
/// head ([`crate::platform::CMD_FRAME_HEAD`], 13 bytes) is the FIRST line's (every later
/// end is further along) — a pre-frame stub's ends exactly on the head's own CRLF, byte
/// 11, which reads as an empty line; a byte shorter would end on `n` of `:main` and run
/// the body again. Pinned at compile time below and by the stub's resume simulation.
const CMD_STUB_FIRST_LINE: &str = "@echo off\r\n";

const _: () = assert!(CMD_STUB_FIRST_LINE.len() >= crate::platform::CMD_FRAME_HEAD.len() - 2);

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
/// shim writer (`platform::cmd_shim_content`). Laid behind the same resume-proof
/// frame as every other `.cmd` this crate writes ([`crate::platform::cmd_framed`],
/// 2026-09-18): a stub of the pre-frame shape that is executing its `where atpkg`
/// line (the one line of it `cmd` reads back after running something, the
/// `__pending` calls sitting inside blocks `cmd` parses whole and exits from) at
/// the moment it is re-laid resumes in the padding and returns silently, instead of
/// inside the new body. [`is_pending_stub`] scans every line for the `rem` marker, so a
/// framed stub is recognized, rewritten and removed exactly as before;
/// [`CMD_STUB_FIRST_LINE`] is the first line AFTER `:main`. An EMPTY `atpkg` ([`named`])
/// leaves the `if exist` block out: the stub names no atpkg and starts at `where atpkg`.
/// Unverified on a Windows host.
#[must_use]
fn stub_content_cmd(tool: &ToolName, atpkg: &Path) -> String {
    let name = tool.as_str();
    let atpkg = atpkg.to_string_lossy();
    let mut s = String::from(CMD_STUB_FIRST_LINE);
    s.push_str("rem ");
    s.push_str(STUB_MARKER);
    s.push_str("\r\n");
    s.push_str("rem Replaced by the real shim when the program installs.\r\n");
    if !atpkg.is_empty() {
        s.push_str(&format!(
            "if exist \"{atpkg}\" (\r\n  \"{atpkg}\" __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
        ));
    }
    s.push_str(&format!(
        "where atpkg >nul 2>nul\r\nif not errorlevel 1 (\r\n  atpkg __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
    ));
    s.push_str(&format!(
        "echo {STUB_UNREACHABLE_MSG} 1>&2\r\nexit /b 127\r\n"
    ));
    crate::platform::cmd_framed(&s)
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

/// The co-located `atpkg` alias beside the running executable — what this process OFFERS
/// as fallback 1's path ([`Embedder`] decides whether a laid file names it). Canonicalized
/// so an argv0 alias (`atpkg` → `aterm`) or a `~/.local/bin` symlink resolves to the real
/// bundle before the sibling join. NOT checked for existence here: a copy of aterm with no
/// `atpkg` beside it answers a path that is not there ([`Embedder::this_process`] drops it).
///
/// On Windows `canonicalize` answers the VERBATIM spelling (`\\?\C:\…`), and this path
/// is embedded in `.cmd` files — the pending stub's — where `cmd.exe`'s `if exist` and
/// its command launch do not reliably accept it; the prefix is taken off there ([`crate::platform::strip_verbatim_prefix`],
/// review finding 2026-09-17; no Windows box has rendered a real one).
pub(crate) fn co_located_atpkg_path() -> PathBuf {
    // `EXE_SUFFIX` (".exe" on Windows, "" elsewhere): a bare `atpkg` join
    // embedded a path that exists on no Windows install — the same probe bug
    // the GUI's co-located resolver fixed — so fallback 1 always missed there
    // and every stub run leaned on PATH luck.
    let atpkg = format!("atpkg{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&atpkg)))
        .unwrap_or_else(|| PathBuf::from(atpkg));
    if cfg!(windows) {
        crate::platform::strip_verbatim_prefix(&path)
    } else {
        path
    }
}

/// Whether `path` is an atpkg a laid file can hand over to: an executable regular file,
/// a link to one counting (a bundle's `atpkg` is a symlink to `aterm`) — the `[ -x ]` every
/// laid body asks before its hand-over.
#[must_use]
pub(crate) fn is_live_atpkg(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// How long an atpkg path can be expected to keep standing — what [`Embedder::embed`]
/// compares when the atpkg a process offers differs from the live one a file already
/// names. A later variant outranks an earlier one.
///
/// The classes are the ones the code already tells apart: the app-bundle shape
/// `…/<X>.app/Contents/MacOS/<exe>` and the translocated and mounted-image launches
/// (`aterm_update::bundle::posture_from`, which this crate does not depend on — the same
/// three tests, restated), the `target/` build every "not a bundle" comment names, and the
/// scratch directory of D1 (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Durability {
    /// Goes away on its own: under a temporary directory, inside a cargo target directory
    /// (any directory above it carries a `CACHEDIR.TAG`, which cargo writes at every
    /// target root), Gatekeeper's translocated copy (`/AppTranslocation/`), or a mounted
    /// disk image (`/Volumes/…`).
    Transient,
    /// A plain binary anywhere else: `tools/install.sh`'s `~/.local/lib/aterm/bin` store
    /// (the Linux install and the source-build lane), a copy someone put somewhere on
    /// purpose.
    Plain,
    /// Inside an app bundle (`…/<X>.app/Contents/MacOS/`) anywhere not transient: the
    /// installed `aterm.app`, or a dev bundle `tools/dev-app.sh` assembled — installed, or
    /// left in `dist/` by `--no-install`. No bundle is ranked above another, so the laid
    /// files name whichever bundle last ran.
    Bundle,
}

/// The first bytes of every `CACHEDIR.TAG` (the Cache Directory Tagging Specification), the
/// file cargo writes at the root of every target directory. A directory carrying one says
/// its contents may be deleted and rebuilt at any time.
const CACHEDIR_TAG_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `dir` carries a cache-directory tag.
fn is_cache_dir(dir: &Path) -> bool {
    crate::metadata_io::read_bounded_regular_utf8(&dir.join("CACHEDIR.TAG"), 4096)
        .is_ok_and(|text| text.starts_with(CACHEDIR_TAG_SIGNATURE))
}

/// Whether `path` sits exactly at `…/<X>.app/Contents/MacOS/<file>`.
fn in_app_bundle(path: &Path) -> bool {
    let Some(macos) = path.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && contents
            .parent()
            .and_then(Path::extension)
            .is_some_and(|ext| ext == "app")
}

/// The [`Durability`] of the atpkg at `atpkg`, with `temp_roots` the machine's temporary
/// directories ([`temp_roots`]).
#[must_use]
pub(crate) fn durability(atpkg: &Path, temp_roots: &[PathBuf]) -> Durability {
    let s = atpkg.to_string_lossy();
    if s.contains("/AppTranslocation/")
        || s.starts_with("/Volumes/")
        || temp_roots.iter().any(|root| atpkg.starts_with(root))
        || atpkg.ancestors().skip(1).any(is_cache_dir)
    {
        return Durability::Transient;
    }
    if in_app_bundle(atpkg) {
        Durability::Bundle
    } else {
        Durability::Plain
    }
}

/// This machine's temporary directories, each as spelled and as it resolves (`/tmp` is
/// `/private/tmp` on macOS, and `$TMPDIR` sits under `/private/var/folders`): the system
/// temp dir, `/tmp` and `/var/tmp`. A root that is the filesystem root itself (a `TMPDIR=/`)
/// is dropped — it would make every path transient.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
    ] {
        if let Ok(real) = std::fs::canonicalize(&root) {
            roots.push(real);
        }
        roots.push(root);
    }
    roots.retain(|root| root.is_absolute() && root.parent().is_some());
    roots.dedup();
    roots
}

/// Which atpkg a file laid NOW names (module doc, "Which atpkg a laid file names"): the one
/// rule every writer of an atpkg path goes through — the pending stubs, the reroute stubs
/// and the `agents/` twins.
#[derive(Debug, Clone)]
pub(crate) struct Embedder {
    /// The atpkg this process offers ([`co_located_atpkg_path`]); [`Self::embed`] takes it
    /// only when it is live.
    offered: PathBuf,
    /// The machine's temporary directories ([`temp_roots`]).
    temp_roots: Vec<PathBuf>,
}

impl Embedder {
    /// The embedder of THIS process: its co-located atpkg, this machine's temp dirs.
    #[must_use]
    pub(crate) fn this_process() -> Self {
        Self {
            offered: co_located_atpkg_path(),
            temp_roots: temp_roots(),
        }
    }

    /// An embedder offering `offered`, with `temp_roots` as the temporary directories — a
    /// process stood anywhere, for a test.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn offering(offered: &Path, temp_roots: &[PathBuf]) -> Self {
        Self {
            offered: offered.to_path_buf(),
            temp_roots: temp_roots.to_vec(),
        }
    }

    /// The atpkg a file names once laid, given the one it names now (`standing`: `None` for
    /// a file not there yet, or one that names none), or `None` to name none:
    ///
    /// * a path that is not a live atpkg ([`is_live_atpkg`]), or is not absolute, is never
    ///   the answer — on either side;
    /// * a live `standing` path stays unless the offered one is live and lasts at least as
    ///   long ([`Durability`]): a scratch copy never takes a file from the installed app,
    ///   while the installed app — moved, reinstalled, or a dev bundle installed on
    ///   purpose — takes it from anything.
    #[must_use]
    pub(crate) fn embed(&self, standing: Option<&Path>) -> Option<PathBuf> {
        let live = |path: &Path| path.is_absolute() && is_live_atpkg(path);
        let offered = Some(self.offered.as_path()).filter(|p| live(p));
        let standing = standing.filter(|p| live(p));
        match (offered, standing) {
            (None, None) => None,
            (Some(offered), None) => Some(offered.to_path_buf()),
            (None, Some(standing)) => Some(standing.to_path_buf()),
            (Some(offered), Some(standing)) => {
                // Equal paths first: the steady state (the installed app re-laying its own
                // files) never walks the ancestors for a cache tag.
                let keep = offered != standing
                    && durability(offered, &self.temp_roots)
                        < durability(standing, &self.temp_roots);
                Some(if keep { standing } else { offered }.to_path_buf())
            }
        }
    }
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

/// Whether the file at `path` is a pending stub THIS module wrote — the recognition
/// gate for every rewrite and removal: a line that is [`STUB_MARKER`] (bare or behind a
/// batch `rem`). Bounded, symlink-refusing read; anything else (absent, a real shim
/// symlink, a tombstone, a hand-made file) answers `false`. A stub an older client laid
/// with its retired extra or requires line is still recognized by this first marker.
#[must_use]
pub fn is_pending_stub(path: &Path) -> bool {
    // A real Unix-era symlink shim is not even a regular file; the bounded reader
    // refuses it before content is considered.
    crate::metadata_io::read_bounded_regular_utf8(path, 64 * 1024)
        .is_ok_and(|text| text.lines().any(|l| is_marker_line(l, STUB_MARKER)))
}

/// Whether `tool` currently resolves to a pending stub in this layout — the front
/// door's second arm (`store_resolves || pending_stub_exists`).
#[must_use]
pub fn pending_stub_exists(layout: &Layout, tool: &str) -> bool {
    ToolName::new(tool).is_some_and(|t| is_pending_stub(&layout.shim(&t)))
}

/// Lay (or refresh) the pending stub for `tool`, atomically (temp `0755` + `rename(2)`,
/// the tombstone writer's shape). NEVER over anything that is not already a pending
/// stub: a resolvable shim, a tombstone, or any unrecognized file wins and the write is
/// a clean no-op — the stub is the lowest-precedence occupant of the name.
#[cfg(test)]
pub(crate) fn write_pending_stub(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    match pending_stub_executable(layout, tool)? {
        Some(file) => crate::lay::write_in_process(&file),
        None => Ok(()),
    }
}

/// The pending stub `write_pending_stub` would lay, RENDERED but not written —
/// `Ok(None)` when there is nothing to lay (an alias, a name Windows cannot embed
/// inertly, a name something else occupies), so the two roster loops render a whole pass
/// before they lay it. The precedence rule lives here, once: NEVER over anything that is
/// not already a pending stub.
pub(crate) fn pending_stub_executable(
    layout: &Layout,
    tool: &ToolName,
) -> io::Result<Option<crate::lay::Executable>> {
    if cfg!(windows) && !cmd_stub_name_safe(tool.as_str()) {
        // See `cmd_stub_name_safe`: no inert embedding exists, so no stub.
        return Ok(None);
    }
    if tool.is_alias() {
        // An alias is never a stub (module doc): it names the managed copy once
        // installed, and the plain name's stub already answers until then.
        return Ok(None);
    }
    pending_stub_executable_at(layout, tool, layout.shim(tool))
}

/// [`pending_stub_executable`] for a stub at an explicit path — the `bin/` slot, or an
/// AGENT program's `agents/` twin (2026-09-15): `agents/` is first on every PATH, so
/// a pending `claude`/`codex` laid only in `bin/` (appended LAST) never answered on a
/// machine with a brew cask, and R6's "typing the name prints the install state" did
/// not hold for the two names it matters most for (audit 2026-09-14). Same precedence
/// rule: never over anything that is not already a pending stub.
///
/// The atpkg the stub names is [`Embedder::embed`]'s answer over the one the stub standing
/// there names now: a copy of aterm with no `atpkg` beside it, or one that lasts less than
/// the atpkg already named, re-points nothing (module doc).
pub(crate) fn pending_stub_executable_at(
    layout: &Layout,
    tool: &ToolName,
    shim: PathBuf,
) -> io::Result<Option<crate::lay::Executable>> {
    let standing = match std::fs::symlink_metadata(&shim) {
        Err(_) => None, // absent: ours to claim
        Ok(_) if is_pending_stub(&shim) => Some(
            crate::metadata_io::read_bounded_regular_utf8(&shim, 64 * 1024).unwrap_or_default(),
        ),
        Ok(_) => return Ok(None), // someone else's file (shim/tombstone/hand-made): never touch
    };
    let atpkg =
        Embedder::this_process().embed(standing.as_deref().and_then(stub_named_atpkg).as_deref());
    let body = stub_content(tool, atpkg.as_deref());
    // Ours already, and byte-identical: nothing to lay — the reroute stubs' rule
    // (`reroute::lay`). Every seed runs the adoption lay and every install pass reconciles
    // twice, so a name that stays wanted-and-absent re-laid identical bytes each time. A body
    // that differs — the atpkg it names changed, or an older client's retired extra/requires
    // line — is rewritten. A macOS tag is not a difference: the store heal clears it in place
    // ([`crate::provenance::heal_store`]).
    if standing.is_some() && stub_is_current(&shim, &body) {
        return Ok(None);
    }
    if let Some(dir) = shim.parent() {
        layout.ensure_dir(dir)?;
    }
    Ok(Some(crate::lay::Executable::new(shim, body)))
}

/// Whether the pending stub at `shim` is CURRENT: exactly the bytes this pass would
/// render (`body`).
fn stub_is_current(shim: &Path, body: &str) -> bool {
    std::fs::read(shim).is_ok_and(|have| have == body.as_bytes())
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
/// the toolset. Skips programs
/// already installed and ones the user removed on purpose; failures are per-name
/// and best-effort (a stub is a courtesy, never a gate on the pass). The compiled
/// rosters carry no `system` key, so system satisfaction is the index reconcile's
/// call — the first resolve retires any stub a system install makes moot.
pub fn lay_adoption_stubs(layout: &Layout) {
    let installed = crate::ops::active_builds(layout);
    let removed = layout.removed_programs();
    let mut files = Vec::new();
    for (name, _) in DEFAULT_SET_STUB_NAMES.iter().chain(AGENT_STUB_NAMES) {
        let name = *name;
        if installed.contains_key(name) || removed.contains(name) {
            continue;
        }
        if let Some(tool) = ToolName::new(name) {
            if let Ok(Some(file)) = pending_stub_executable(layout, &tool) {
                files.push(file);
            }
            // The agent programs' stubs go FIRST on PATH too ([`pending_stub_executable_at`]).
            if is_agent_program(name)
                && let Ok(Some(twin)) =
                    pending_stub_executable_at(layout, &tool, layout.agent_shim(&tool))
            {
                files.push(twin);
            }
        }
    }
    // Best-effort like every stub, but never silent: the reason is printed once.
    if let Err(e) = files.iter().try_for_each(crate::lay::write_in_process) {
        eprintln!("atpkg: warn — pending stubs not laid: {e}");
    }
}

/// The index-resolve reconcile: `wanted` is the SIGNED set the pass will keep
/// complete (installable ∧ not-removed), `installed` the active builds. Adds/refreshes a
/// stub for every wanted absent name (embedded paths refreshed as a matter of course),
/// then sweeps every pending stub whose name is no longer wanted and missing —
/// de-listed, removed on purpose, or now installed under a name its real shims do not
/// expose.
pub fn reconcile(layout: &Layout, wanted: &BTreeSet<String>, installed: &BTreeMap<String, u64>) {
    let mut keep: BTreeSet<&str> = BTreeSet::new();
    let mut files = Vec::new();
    for name in wanted {
        if installed.contains_key(name.as_str()) {
            continue;
        }
        // The ToolName gate IS the sensitive-name refusal: an index listing `sudo`
        // gets no stub, exactly as it gets no shim.
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        match pending_stub_executable(layout, &tool) {
            Ok(Some(file)) => {
                files.push(file);
                keep.insert(name.as_str());
            }
            Ok(None) => {
                keep.insert(name.as_str());
            }
            Err(_) => {}
        }
        if is_agent_program(name)
            && let Ok(Some(twin)) =
                pending_stub_executable_at(layout, &tool, layout.agent_shim(&tool))
        {
            files.push(twin);
        }
    }
    // Every stub this reconcile adds or refreshes. A lay that fails leaves the names it
    // would have laid in `keep`, so the sweep below removes nothing over it — and the
    // reason is printed once.
    if let Err(e) = files.iter().try_for_each(crate::lay::write_in_process) {
        eprintln!("atpkg: warn — pending stubs not laid: {e}");
    }
    // Both directories a pending stub can stand in: `bin/`, and `agents/` for the
    // agent programs' twins.
    for dir in [layout.bin_dir(), layout.agents_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
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
    /// An agent program's pending stub stands in `agents/` too — first on every PATH —
    /// carries the caller's arguments through, and leaves once the program installs or
    /// is de-listed (2026-09-15). A non-agent program gets no twin.
    #[test]
    fn an_agent_programs_pending_stub_stands_in_agents_too_and_passes_arguments() {
        let l = layout("agents-stub");
        lay_adoption_stubs(&l);
        let claude = tool("claude");
        let twin = l.agent_shim(&claude);
        assert!(
            is_pending_stub(&twin),
            "claude's stub stands first on PATH: {}",
            twin.display()
        );
        assert!(is_pending_stub(&l.shim(&claude)), "and in bin/ as before");
        assert!(
            !l.agent_shim(&tool("trust")).exists(),
            "only the agent programs get a twin"
        );
        let body = std::fs::read_to_string(&twin).unwrap();
        assert!(
            body.contains("__pending 'claude' \"$@\""),
            "the caller's arguments ride through: {body}"
        );
        // Installed: the reconcile's sweep removes the twin stub (its real twin, laid by
        // activation, is what stands there next).
        let mut installed = BTreeMap::new();
        installed.insert(String::from("claude"), 1u64);
        reconcile(&l, &BTreeSet::new(), &installed);
        assert!(
            !twin.exists(),
            "an installed program's stub is swept from agents/"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

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
        reconcile(&l, &wanted, &BTreeMap::new());
        assert!(
            pending_stub_exists(&l, "trust"),
            "the plain name is stubbed"
        );
        assert!(
            std::fs::symlink_metadata(l.shim(&tool("alab-trust"))).is_err(),
            "no stub is laid under the alias name"
        );
        assert!(!pending_stub_exists(&l, "alab-trust"));
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
        );
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains(STUB_MARKER));
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
        let body = stub_content_sh(&nasty, Path::new("/x'y/atpkg"));
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
        );
        // The frame first (2026-09-18), then the batch body `@echo off` opens.
        let frame = crate::platform::cmd_frame();
        assert!(body.starts_with(&frame), "framed: {body}");
        assert!(
            body[frame.len()..].starts_with("@echo off\r\n"),
            "batch, not sh: {body}"
        );
        assert_eq!(body.matches("@goto :main").count(), 1);
        assert_eq!(body.matches("\r\n:main\r\n").count(), 1);
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

    /// THE RESUME SIMULATION for the pending stub (review finding, 2026-09-18: the stub
    /// was the one `.cmd` this crate laid without the frame). A stub of the pre-frame
    /// shape — the body alone, which is what every Windows stub was until 2026-09-18 —
    /// re-laid to its framed successor while it executes: at every line end of the old
    /// file (the only offsets `cmd` can resume at) and at every offset up to its
    /// end-of-file, the new file reads a colon label, an empty line or `@exit /b`. The
    /// longest stub the writer lays — a `MAX_PATH` atpkg path at three UTF-8 bytes a
    /// unit, embedded twice — still ends inside the padding. As the control, the same old
    /// stub over an UNFRAMED successor resumes into a command line. A model of `cmd`'s documented rules, not a run on Windows.
    #[test]
    fn a_pre_frame_cmd_stub_re_laid_while_executing_resumes_into_the_frame() {
        use crate::platform::{
            CMD_FRAME_HEAD, CMD_LEGACY_TARGET_BOUND_BYTES, CMD_PADDING_END_BYTES, cmd_frame,
            cmd_resumed_line, cmd_resumed_line_is_inert,
        };
        let frame = cmd_frame();
        let long_atpkg = std::path::PathBuf::from(format!(
            "C:\\{}\\atpkg.exe",
            "p".repeat(CMD_LEGACY_TARGET_BOUND_BYTES - "C:\\\\atpkg.exe".len())
        ));
        assert_eq!(
            long_atpkg.to_string_lossy().len(),
            CMD_LEGACY_TARGET_BOUND_BYTES
        );
        let cases = [
            (
                "a Program Files atpkg",
                stub_content_cmd(
                    &tool("trust"),
                    Path::new(r"C:\Program Files (x86)\aterm\app\atpkg.exe"),
                ),
            ),
            (
                "the longest atpkg path",
                stub_content_cmd(&tool("vendorx"), &long_atpkg),
            ),
        ];
        for (what, new) in &cases {
            assert!(new.starts_with(&frame), "{what}");
            // The pre-frame shape: the same body, laid bare.
            let old = &new[frame.len()..];
            assert!(old.starts_with("@echo off\r\n"), "{what}");
            let old = old.as_bytes();
            let new = new.as_bytes();
            assert!(
                old.len() <= CMD_PADDING_END_BYTES,
                "{what}: {} bytes",
                old.len()
            );
            let ends: Vec<usize> = old
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1)
                .collect();
            assert_eq!(ends.last().copied(), Some(old.len()), "{what}");
            // The only old line end that can fall inside the head is the first one; the
            // stub's lands exactly on the head's CRLF (11), the tightest case there is,
            // and reads empty. (`)` lines are 3 bytes, but their ends are cumulative.)
            assert_eq!(ends[0], CMD_STUB_FIRST_LINE.len(), "{what}");
            assert_eq!(
                ends[0],
                CMD_FRAME_HEAD.len() - 2,
                "{what}: on the head's CRLF"
            );
            assert_eq!(
                cmd_resumed_line(new, ends[0]).as_deref(),
                Some(""),
                "{what}: the first old line end reads the head's CRLF as empty"
            );
            assert!(
                !cmd_resumed_line_is_inert(cmd_resumed_line(new, ends[0] - 1).as_deref()),
                "{what}: one byte shorter would not be"
            );
            assert_eq!(
                cmd_resumed_line(new, 0).as_deref(),
                Some("@goto :main"),
                "{what}: the fresh run"
            );
            for offset in CMD_FRAME_HEAD.len() - 2..=old.len() {
                let line = cmd_resumed_line(new, offset);
                assert!(
                    cmd_resumed_line_is_inert(line.as_deref()),
                    "{what}: a resume at offset {offset} of {} reads {line:?}",
                    old.len()
                );
            }
            for end in &ends {
                assert!(
                    cmd_resumed_line_is_inert(cmd_resumed_line(new, *end).as_deref()),
                    "{what}: old line end {end}"
                );
            }
        }
        // THE CONTROL: the old (bare) stub over an UNFRAMED successor whose embedded
        // atpkg path moved 30 bytes longer (an app relocated) — the old end-of-file
        // offset lands 60 bytes short of the new end, inside the `echo …` line: a
        // fragment run as a command, the shape before the frame.
        let old = &cases[0].1[frame.len()..];
        let moved = stub_content_cmd(
            &tool("trust"),
            Path::new(&format!(
                r"C:\Program Files (x86)\aterm\app\{}\atpkg.exe",
                "m".repeat(29)
            )),
        );
        let bare = &moved[frame.len()..];
        assert_eq!(bare.len(), old.len() + 60);
        let line = cmd_resumed_line(bare.as_bytes(), old.len());
        assert!(
            !cmd_resumed_line_is_inert(line.as_deref()),
            "laid bare, the resume lands in the body: {line:?}"
        );
        assert_eq!(
            cmd_resumed_line(moved.as_bytes(), old.len()).as_deref(),
            Some(&":".repeat(78)[(old.len() - CMD_FRAME_HEAD.len()) % 80..]),
            "framed, the same resume reads a colon label"
        );
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
            stub_content_cmd(&tool("trust"), Path::new(r"C:\x\atpkg.exe")),
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
        // The agent programs ride along like every default-set member.
        for name in AGENT_PROGRAMS {
            assert!(pending_stub_exists(&l, name), "{name}: laid at adoption");
        }
        // A foreign file beside them survives the sweep.
        std::fs::write(l.bin_dir().join("mine"), "not a stub").unwrap();
        remove_all_stubs(&l);
        for (name, _) in DEFAULT_SET_STUB_NAMES.iter().chain(AGENT_STUB_NAMES) {
            assert!(!pending_stub_exists(&l, name), "{name} swept with the rest");
        }
        assert!(l.bin_dir().join("mine").exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A stub already on disk with the exact bytes the pass would render is NEVER re-laid,
    /// tagged or not (Phase 3, 2026-09-22): `pending_stub_executable` answers `Ok(None)`, so
    /// a steady-state seed or install pass lays nothing — no new inode. A stub whose body
    /// changed (an older client's extra line, a moved atpkg) is still rewritten.
    #[cfg(unix)]
    #[test]
    fn an_identical_stub_is_left_alone_and_a_changed_one_is_rewritten() {
        use std::os::unix::fs::MetadataExt as _;
        let l = layout("identical-skip");
        let t = tool("trust");
        let body = stub_content(&t, Embedder::this_process().embed(None).as_deref());
        let fresh = pending_stub_executable(&l, &t).unwrap();
        assert!(fresh.is_some(), "an absent name is ours to claim");
        write_pending_stub(&l, &t).unwrap();
        let shim = l.shim(&t);
        assert!(is_pending_stub(&shim));
        assert_eq!(
            std::fs::read(&shim).unwrap(),
            body.as_bytes(),
            "the writer lays exactly the bytes the renderer renders"
        );
        assert!(stub_is_current(&shim, &body), "byte-identical: current");
        // End to end through the reconcile and the adoption lay: whatever tag this
        // session's writes carry, the identical stub keeps its inode.
        let ino = std::fs::metadata(&shim).unwrap().ino();
        let wanted: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
        reconcile(&l, &wanted, &BTreeMap::new());
        lay_adoption_stubs(&l);
        assert!(pending_stub_exists(&l, "trust"), "the reconcile keeps it");
        assert!(
            pending_stub_executable(&l, &t).unwrap().is_none(),
            "byte-identical: nothing to lay, tag or no tag"
        );
        assert_eq!(
            std::fs::metadata(&shim).unwrap().ino(),
            ino,
            "an identical stub is never re-laid"
        );
        // A changed body is still a rewrite: the stub an older client laid for an extra
        // (its retired marker line) is recognized, and re-laid plain.
        let older = body.replacen(
            STUB_MARKER,
            "# atpkg pending-program stub v1\n# atpkg extra: asks consent before installing\n\
             # atpkg requires: clt",
            1,
        );
        std::fs::write(&shim, &older).unwrap();
        assert!(
            is_pending_stub(&shim),
            "an older extra stub is still a stub"
        );
        assert!(
            !stub_is_current(&shim, &body),
            "a changed body is never current"
        );
        reconcile(&l, &wanted, &BTreeMap::new());
        assert_eq!(
            std::fs::read(&shim).unwrap(),
            body.as_bytes(),
            "the plain body is what landed"
        );
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
        reconcile(&l, &wanted, &installed);
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
        reconcile(&l, &wanted, &installed);
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
        let out = run(&stub_content(&t, Some(&fake)), "/nonexistent");
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
            &stub_content(&t, Some(Path::new("/gone/after/relocation/atpkg"))),
            path_dir.to_str().unwrap(),
        );
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "path: __pending trust\n"
        );
        // 2, naming no atpkg at all (nothing live when it was laid): the same PATH answer.
        let out = run(&stub_content(&t, None), path_dir.to_str().unwrap());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "path: __pending trust\n"
        );
        // 3: nothing reachable — the static honest message, exit 127.
        let out = run(
            &stub_content(&t, Some(Path::new("/gone/atpkg"))),
            "/nonexistent",
        );
        assert_eq!(out.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&out.stderr).contains(STUB_UNREACHABLE_MSG));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The AGENT roster (owner decision 2026-09-10): `claude` and `codex`, with a
    /// shim-admissible name and a vendor-descriptive line the pending stub prints while
    /// the install is on its way.
    #[test]
    fn agent_programs_are_their_own_roster() {
        assert_eq!(AGENT_PROGRAMS, &["claude", "codex"]);
        for name in AGENT_PROGRAMS {
            assert!(is_agent_program(name));
            assert!(crate::store::shim_allowed(name));
            assert!(
                DEFAULT_SET_STUB_NAMES.iter().all(|(n, _)| n != name),
                "{name} is in two rosters"
            );
            let desc = describe(name).expect("an authored line");
            assert!(desc.contains("downloaded from"), "{name}: {desc}");
        }
        assert!(!is_agent_program("trust"));
        assert!(!is_agent_program("nonesuch"));
        // The line is descriptive of the vendor, never an ALab mark.
        assert!(describe("codex").unwrap().starts_with("OpenAI "));
        assert!(describe("claude").unwrap().starts_with("Anthropic "));
    }

    // ── which atpkg a laid file names ───────────────────────────────────────────────

    /// The classes the rule ranks, each from its own path: an app bundle, a plain binary, and
    /// the four transient shapes — a temp dir, a cargo target dir, a translocated copy, a
    /// mounted image.
    #[cfg(unix)]
    #[test]
    fn durability_ranks_a_bundle_over_a_plain_binary_over_a_transient_copy() {
        let l = layout("durability");
        let root = std::fs::canonicalize(&l.prefix).unwrap();
        let target = root.join("checkout/target.noindex");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            format!("{CACHEDIR_TAG_SIGNATURE}\n# This file is a cache directory tag.\n"),
        )
        .unwrap();
        // A file of that name WITHOUT the signature tags nothing.
        let untagged = root.join("untagged");
        std::fs::create_dir_all(&untagged).unwrap();
        std::fs::write(untagged.join("CACHEDIR.TAG"), "not a tag\n").unwrap();
        let scratch = root.join("tmp");
        let scratch_roots = [scratch.clone()];
        for (path, want) in [
            (
                root.join("Applications/aterm.app/Contents/MacOS/atpkg"),
                Durability::Bundle,
            ),
            (
                root.join("Applications/aterm (dev).app/Contents/MacOS/atpkg"),
                Durability::Bundle,
            ),
            (root.join(".local/lib/aterm/bin/atpkg"), Durability::Plain),
            (untagged.join("debug/atpkg"), Durability::Plain),
            // Not the exact bundle shape: a plain binary.
            (root.join("aterm.app/MacOS/atpkg"), Durability::Plain),
            (target.join("debug/atpkg"), Durability::Transient),
            (
                target.join("release/bundle/aterm.app/Contents/MacOS/atpkg"),
                Durability::Transient,
            ),
            (scratch.join("run/bin/atpkg"), Durability::Transient),
            (
                PathBuf::from(
                    "/private/var/folders/x/AppTranslocation/Y/d/aterm.app/Contents/MacOS/atpkg",
                ),
                Durability::Transient,
            ),
            (
                PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/atpkg"),
                Durability::Transient,
            ),
        ] {
            assert_eq!(
                durability(&path, &scratch_roots),
                want,
                "{}",
                path.display()
            );
        }
        // The machine's own temp dirs are transient, and the filesystem root is not a temp
        // dir whatever `TMPDIR` says.
        let roots = temp_roots();
        assert!(roots.iter().all(|r| r.parent().is_some()), "{roots:?}");
        assert_eq!(
            durability(&std::env::temp_dir().join("x/bin/atpkg"), &roots),
            Durability::Transient
        );
        assert_eq!(
            durability(
                Path::new("/Applications/aterm.app/Contents/MacOS/atpkg"),
                &roots
            ),
            Durability::Bundle
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// [`Embedder::embed`], case by case: a missing atpkg is never named on either side; a
    /// live one stays unless the offered one lasts at least as long; a gone one yields to
    /// anything live.
    #[cfg(unix)]
    #[test]
    fn the_embedder_names_no_missing_atpkg_and_never_trades_down() {
        let l = layout("embedder");
        let root = std::fs::canonicalize(&l.prefix).unwrap();
        let live = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let installed = live("Applications/aterm.app/Contents/MacOS/atpkg");
        let dev = live("Applications/aterm (dev).app/Contents/MacOS/atpkg");
        let store = live("home/.local/lib/aterm/bin/atpkg");
        let scratch = live("tmp/run/bin/atpkg");
        let target = root.join("checkout/target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            format!("{CACHEDIR_TAG_SIGNATURE}\n"),
        )
        .unwrap();
        let target_build = live("checkout/target/debug/atpkg");
        let gone = root.join("gone/aterm.app/Contents/MacOS/atpkg");
        let not_executable = root.join("plain-file/atpkg");
        std::fs::create_dir_all(not_executable.parent().unwrap()).unwrap();
        std::fs::write(&not_executable, "not a program").unwrap();
        let scratch_roots = [root.join("tmp")];
        let embed = |offered: &Path, standing: Option<&Path>| {
            Embedder::offering(offered, &scratch_roots).embed(standing)
        };
        // D1: the offered atpkg is not there — the standing live one stays, or none is named.
        assert_eq!(embed(&gone, Some(&installed)), Some(installed.clone()));
        assert_eq!(embed(&gone, None), None);
        assert_eq!(embed(&gone, Some(&gone)), None);
        assert_eq!(embed(&not_executable, None), None);
        assert_eq!(
            embed(Path::new("atpkg"), None),
            None,
            "a relative path is never named"
        );
        // Never trade down from a live atpkg.
        assert_eq!(
            embed(&target_build, Some(&installed)),
            Some(installed.clone())
        );
        assert_eq!(embed(&scratch, Some(&installed)), Some(installed.clone()));
        assert_eq!(embed(&store, Some(&installed)), Some(installed.clone()));
        assert_eq!(embed(&scratch, Some(&store)), Some(store.clone()));
        assert_eq!(embed(&target_build, Some(&store)), Some(store.clone()));
        // Up, or level: the offered one takes it — the installed app from a target/ build,
        // a launched dev bundle from the release, the store from a scratch copy.
        assert_eq!(
            embed(&installed, Some(&target_build)),
            Some(installed.clone())
        );
        assert_eq!(embed(&dev, Some(&installed)), Some(dev.clone()));
        assert_eq!(embed(&installed, Some(&dev)), Some(installed.clone()));
        assert_eq!(embed(&store, Some(&scratch)), Some(store.clone()));
        assert_eq!(embed(&installed, Some(&installed)), Some(installed.clone()));
        // A gone or unnamed standing path yields to anything live — the app moved, a fresh
        // prefix, a file an older client laid naming none.
        assert_eq!(embed(&installed, Some(&gone)), Some(installed.clone()));
        assert_eq!(
            embed(&target_build, Some(&gone)),
            Some(target_build.clone())
        );
        assert_eq!(embed(&installed, None), Some(installed.clone()));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// What a laid file names is read back exactly as it was written — every quoting the
    /// renderers produce, the empty value as "none", and nothing from a line this crate did
    /// not quote; the batch stub leaves its `if exist` block out when it names none.
    #[test]
    fn the_named_atpkg_reads_back_as_written() {
        let t = tool("trust");
        for path in [
            "/Applications/aterm.app/Contents/MacOS/atpkg",
            "/Users//o'brien/Application Support/aterm/bin/atpkg",
            "/it's/''/x",
        ] {
            let body = stub_content_sh(&t, Path::new(path));
            assert_eq!(
                sh_named_atpkg(&body, "ATPKG"),
                Some(PathBuf::from(path)),
                "{body}"
            );
            assert_eq!(stub_named_atpkg(&body), Some(PathBuf::from(path)));
            let twin = crate::platform::sh_selfupdate_prelude(
                "claude",
                Path::new("/p"),
                Path::new(path),
                &["update"],
            );
            assert_eq!(
                sh_named_atpkg(&twin, "__atpkg"),
                Some(PathBuf::from(path)),
                "{twin}"
            );
            let reroute = crate::reroute::stub_body_sh("cargo", Path::new(path), Path::new("/r"));
            assert_eq!(sh_named_atpkg(&reroute, "ATPKG"), Some(PathBuf::from(path)));
        }
        let cmd = stub_content_cmd(&t, Path::new(r"C:\Program Files\aterm\atpkg.exe"));
        assert_eq!(
            stub_named_atpkg(&cmd),
            Some(PathBuf::from(r"C:\Program Files\aterm\atpkg.exe"))
        );
        // Naming none.
        let none = stub_content_sh(&t, named(None));
        assert!(none.contains("\nATPKG=''\n"), "{none}");
        assert_eq!(stub_named_atpkg(&none), None);
        let cmd_none = stub_content_cmd(&t, named(None));
        assert!(!cmd_none.contains("if exist"), "{cmd_none}");
        assert!(cmd_none.contains("where atpkg"), "{cmd_none}");
        assert_eq!(stub_named_atpkg(&cmd_none), None);
        // Not our quoting: not guessed at.
        assert_eq!(sh_named_atpkg("ATPKG=/unquoted/atpkg\n", "ATPKG"), None);
        assert_eq!(sh_named_atpkg("ATPKG='/open\n", "ATPKG"), None);
        assert_eq!(sh_named_atpkg("MY_ATPKG='/x'\n", "ATPKG"), None);
    }

    // ── which atpkg a laid file names, at the real process edge ─────────────────────

    /// The env that turns [`probe_lays_every_file_as_this_copy`] on: the prefix it lays over.
    #[cfg(unix)]
    const LAY_AS_COPY_ENV: &str = "ATPKG_STUB_LAY_AS_COPY_PREFIX";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const LAY_AS_COPY_PROBE: &str = "stub::tests::probe_lays_every_file_as_this_copy";

    /// The copy's whole job: the three lays that run at every launch, session spawn and pass
    /// — the reroute stubs, the pending stubs, the `agents/` twins — over the prefix it is
    /// handed, as the binary it IS (its own `current_exe`, its own neighbours). Outside a
    /// copy (the env unset) it does nothing.
    #[cfg(unix)]
    #[test]
    fn probe_lays_every_file_as_this_copy() {
        let Some(prefix) = std::env::var_os(LAY_AS_COPY_ENV) else {
            return;
        };
        let l = Layout {
            prefix: std::path::PathBuf::from(prefix),
        };
        crate::reroute::lay(&l).expect("the reroute lay");
        lay_adoption_stubs(&l);
        crate::activate::reconcile_agents(&l);
    }

    /// Place a copy of THIS test binary at `exe` — with the `atpkg` alias beside it when
    /// `alias`, the shape every bundle and install.sh's store carry — and run
    /// [`probe_lays_every_file_as_this_copy`] from there over `l`.
    ///
    /// A copy already at `exe` is REMOVED first, never written over. macOS keeps a
    /// binary's code-signature state with the file it ran from, and a file written over
    /// after it has run can be SIGKILLed by the kernel at its next exec, before the probe
    /// runs (`unix_wait_status(9)`) — the fixture failing, not the lay. Measured 2026-09-25, every kill at the second copy to one path: copied
    /// over, 5 of 200 runs of the test below were killed (and a shell `cp` over a copy that
    /// had already run, 38 of 40); removed first, which gives the copy a new file, 0 of 200
    /// run one at a time and 0 of 200 run four at a time. The same kernel class is recorded
    /// in `caller_shell`'s outliving-program test.
    #[cfg(unix)]
    fn lay_as_copy_at(exe: &Path, alias: bool, l: &Layout) {
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        match std::fs::remove_file(exe) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("remove the previous copy at {}: {e}", exe.display()),
        }
        std::fs::copy(std::env::current_exe().unwrap(), exe).expect("copy this test binary");
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        if alias {
            let _ = std::fs::remove_file(exe.with_file_name("atpkg"));
            std::os::unix::fs::symlink(exe.file_name().unwrap(), exe.with_file_name("atpkg"))
                .unwrap();
        }
        let out = std::process::Command::new(exe)
            .args(["--exact", "--nocapture", LAY_AS_COPY_PROBE])
            .env(LAY_AS_COPY_ENV, &l.prefix)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run the copy");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the copy at {} did not run the probe: {out:?}",
            exe.display()
        );
    }

    /// Every `(file, atpkg it names)` under `prefix`'s three laid directories: the value of
    /// each file's `ATPKG=` line (a reroute or pending stub) or `__atpkg=` line (an agents
    /// twin), as written.
    #[cfg(unix)]
    fn atpkg_named_under(prefix: &Path) -> Vec<(std::path::PathBuf, String)> {
        let l = Layout {
            prefix: prefix.to_path_buf(),
        };
        let mut named = Vec::new();
        for dir in [crate::reroute::dir(&l), l.bin_dir(), l.agents_dir()] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(text) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                for line in text.lines() {
                    let line = line.trim_start();
                    if let Some(value) = line
                        .strip_prefix("ATPKG=")
                        .or_else(|| line.strip_prefix("__atpkg="))
                    {
                        named.push((entry.path(), value.to_string()));
                    }
                }
            }
        }
        named.sort();
        named
    }

    /// Rewrite every atpkg a laid file under `prefix` names to `atpkg` — the state a launch of
    /// the installed app leaves behind.
    #[cfg(unix)]
    fn name_atpkg_everywhere(prefix: &Path, atpkg: &Path) {
        let quoted = sh_single_quote(&atpkg.to_string_lossy());
        for (file, _) in atpkg_named_under(prefix) {
            let text = std::fs::read_to_string(&file).unwrap();
            let mut out = String::new();
            for line in text.split_inclusive('\n') {
                let body = line.trim_start();
                let indent = &line[..line.len() - body.len()];
                let var = ["ATPKG=", "__atpkg="]
                    .into_iter()
                    .find(|var| body.starts_with(var));
                match var {
                    Some(var) => {
                        out.push_str(indent);
                        out.push_str(var);
                        out.push_str(&quoted);
                        out.push('\n');
                    }
                    None => out.push_str(line),
                }
            }
            std::fs::write(&file, out).unwrap();
        }
    }

    /// D1 of the 2026-09-25 end-to-end run: a private copy of aterm started from a scratch
    /// directory re-laid the machine's eight reroute stubs with
    /// `ATPKG='<scratch>/bin/atpkg'` — a file that was never there, in a directory that was
    /// deleted after the run — over stubs naming the installed app's live atpkg. Every launch
    /// and session spawn runs that lay, and so does every pass for the pending stubs and the
    /// `agents/` twins, all from `<current_exe dir>/atpkg`, never asking whether it exists.
    ///
    /// Reproduced as it happened: a copy of this test binary in a scratch `bin/` with no
    /// `atpkg` beside it runs the three real lays over a prefix whose every file names a
    /// live, installed atpkg. Nothing may be re-pointed at the scratch directory, and every
    /// file still names the installed atpkg afterwards.
    ///
    /// NEGATIVE CONTROL, the legitimate re-lay: the installed app MOVES (its old atpkg gone,
    /// the app at a new path with the `atpkg` alias beside it) and every file is re-laid
    /// naming the new path; and a FRESH prefix is laid naming it too. Without this half the
    /// first half would pass for a lay that re-points nothing ever.
    #[cfg(unix)]
    #[test]
    fn a_copy_run_from_a_scratch_dir_never_re_points_a_laid_file_at_a_missing_atpkg() {
        if std::env::var_os(LAY_AS_COPY_ENV).is_some() {
            return;
        }
        let root = std::env::temp_dir().join(format!("atpkg-lay-as-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // Removed however the test ends — a failed run leaves no copies of this binary
        // (tens of MB each) behind in the temp dir.
        struct RemoveOnDrop(std::path::PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveOnDrop(root.clone());
        // Canonical, as the lay spells an atpkg path: `current_exe` resolved.
        let root = std::fs::canonicalize(&root).unwrap();
        let l = Layout {
            prefix: root.join("prefix"),
        };
        // The installed app's atpkg: live.
        let installed = root.join("Applications/aterm.app/Contents/MacOS/atpkg");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A managed claude, so the agents twin and its reroute stub are laid too.
        let build = l.build_dir("claude", 2026092501);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin/claude"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            build.join("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        crate::activate::install_shims(
            &l,
            &build,
            &["claude".to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        crate::store::mark_build_ready(&build).unwrap();
        // Every file laid once, then named at the installed app — its launch's state.
        crate::reroute::lay(&l).unwrap();
        lay_adoption_stubs(&l);
        crate::activate::reconcile_agents(&l);
        name_atpkg_everywhere(&l.prefix, &installed);
        let before = atpkg_named_under(&l.prefix);
        let quoted = sh_single_quote(&installed.to_string_lossy());
        assert!(
            before.iter().all(|(_, v)| *v == quoted),
            "the fixture names the installed atpkg everywhere: {before:#?}"
        );
        // Non-vacuous: all three kinds are there.
        let reroute_dir = crate::reroute::dir(&l);
        for (what, file) in [
            ("a reroute stub", reroute_dir.join("cargo")),
            ("a pending stub", l.shim(&tool("ty"))),
            ("the agents twin", l.agent_shim(&tool("claude"))),
        ] {
            assert!(
                before.iter().any(|(f, _)| *f == file),
                "{what} names an atpkg: {before:#?}"
            );
        }

        // THE SCRATCH COPY: `<scratch>/bin/aterm`, nothing beside it.
        let scratch = root.join("scratch/bin/aterm");
        lay_as_copy_at(&scratch, false, &l);
        let after = atpkg_named_under(&l.prefix);
        let scratch_dir = scratch.parent().unwrap().to_string_lossy().into_owned();
        let leaked: Vec<_> = after
            .iter()
            .filter(|(_, v)| v.contains(&scratch_dir))
            .collect();
        assert!(
            leaked.is_empty(),
            "a copy run from a scratch dir re-pointed laid files at an atpkg that is not \
             there: {leaked:#?}"
        );
        assert_eq!(
            after, before,
            "every laid file still names the installed atpkg"
        );

        // NEGATIVE CONTROL: the installed app moved. Its old atpkg is gone; the app runs
        // from its new path, alias beside it, and every file follows it.
        std::fs::remove_file(&installed).unwrap();
        let moved = root.join("Moved/aterm.app/Contents/MacOS/aterm");
        lay_as_copy_at(&moved, true, &l);
        let moved_atpkg = sh_single_quote(&moved.with_file_name("atpkg").to_string_lossy());
        let relaid = atpkg_named_under(&l.prefix);
        assert_eq!(relaid.len(), before.len(), "{relaid:#?}");
        assert!(
            relaid.iter().all(|(_, v)| *v == moved_atpkg),
            "the moved app re-lays every file naming its own atpkg: {relaid:#?}"
        );
        // And a fresh prefix is laid naming it from the first lay.
        let fresh = Layout {
            prefix: root.join("fresh"),
        };
        lay_as_copy_at(&moved, true, &fresh);
        let laid = atpkg_named_under(&fresh.prefix);
        assert!(
            laid.iter()
                .any(|(f, _)| *f == crate::reroute::dir(&fresh).join("cargo")),
            "{laid:#?}"
        );
        assert!(
            laid.iter().all(|(_, v)| *v == moved_atpkg),
            "a fresh install names its own atpkg: {laid:#?}"
        );
    }
}
