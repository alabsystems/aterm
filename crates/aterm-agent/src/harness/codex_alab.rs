// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CODEX-ALAB, the pure half: what aterm reads of a Codex process to know an
//! ALab build ([`alab_bytes`], the proven self-claim's image fact, the native
//! harness contract §1) and to check and type the line its crash relaunch
//! runs (the contract §4): its package ([`package_entrypoint`],
//! [`package_version`]), the flag table its argv is read by
//! ([`rewrite_argv`], [`is_tui_argv`], [`resumed_thread`]), the `CODEX_HOME`
//! the line carries ([`line_env`]) and the thread a writer lock names
//! ([`lock_thread`]). Nothing here reads the screen or decides a move: a
//! codex-alab supervises itself, and aterm keeps only the watch-only worker
//! (the crash rule, the stall and memory remedies, restore and the seamless
//! update's carry). The I/O half is `upgrade_drive`'s `codex_alab_drive`.
//!
//! Codex's files are a third party's, so every reader refuses a shape it was
//! not written for.

use std::path::{Component, Path, PathBuf};

use aterm_json::Value;

use super::codex_rollout::is_thread_id;
use super::upgrade::{ArgvRefusal, Version};

// ---------------------------------------------------------------- the package

/// The manifest every Codex package carries at its root (layoutVersion 1).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// The file an ALab build of Codex (codex-alab) carries in its package root,
/// beside [`PACKAGE_JSON`] (`alab/package.sh`, package layout v1): the mark of
/// bytes ALab built, not the vendor's.
pub const ALAB_BUILD_JSON: &str = "alab-build.json";

/// Optional package metadata is small. A malformed package must not park a
/// sweep or rollout reader on a FIFO, or allocate an unbounded manifest.
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;

fn package_text(path: &Path) -> Option<String> {
    let text = super::footer::read_small(path, MAX_PACKAGE_BYTES + 1)?;
    (text.len() as u64 <= MAX_PACKAGE_BYTES).then_some(text)
}

/// Whether the Codex `exe` runs ALAB BYTES (codex-alab): its package root
/// (the one [`package_version`] reads, whose manifest names `exe` itself) carries [`ALAB_BUILD_JSON`] as a regular file.
/// `false` for an exe with no such package and for a marker that cannot be
/// looked at. The proven self-claim's image fact (the native harness contract
/// §1): only an ALab build's process proves a claim, never another program
/// that happens to lead the foreground group.
#[must_use]
pub fn alab_bytes(exe: &Path) -> bool {
    alab_marker(exe).is_some_and(|marker| marker.is_ok_and(|m| m.file_type().is_file()))
}

/// The [`ALAB_BUILD_JSON`] look of `exe`'s package root: `None` when `exe`
/// has no package root whose manifest names it.
pub(crate) fn alab_marker(exe: &Path) -> Option<std::io::Result<std::fs::Metadata>> {
    let (root, _) = package_root(exe)?;
    Some(std::fs::symlink_metadata(root.join(ALAB_BUILD_JSON)))
}

/// The BUILD an ALab package's `exe` runs: its manifest's `version`, whole
/// (`0.164.0-dev.12521+g3d3eb12d8`), when `exe` runs ALab bytes
/// ([`alab_bytes`]: the package root whose manifest names `exe` carries
/// [`ALAB_BUILD_JSON`]); `None` for any other exe. What the Codex rollout tap
/// holds a thread's rows to (session log 5G-3, `super::codex_tap`): the
/// build writing them now, which a thread resumed or re-exec'd onto another
/// build never restates in its `session_meta`. Files only, never a run.
#[must_use]
pub fn alab_build(exe: &Path) -> Option<String> {
    let (root, manifest) = package_root(exe)?;
    let marker = std::fs::symlink_metadata(root.join(ALAB_BUILD_JSON)).ok()?;
    if !marker.file_type().is_file() {
        return None;
    }
    let v: Value = aterm_json::from_str(&manifest).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

/// `exe`'s package root (its parent or grandparent) whose manifest names
/// `exe` itself, and that manifest's text.
fn package_root(exe: &Path) -> Option<(PathBuf, String)> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = package_text(&root.join(PACKAGE_JSON))?;
        let entry = std::fs::canonicalize(root.join(package_entrypoint(&text)?)).ok()?;
        (entry == exe).then(|| (root.to_path_buf(), text))
    })
}

/// The version of the Codex `exe`, read from its package manifest, never by
/// running it (`codex --version` is unparseable and an exec can hang, holding
/// the sweep lock). The manifest must be the root whose `entrypoint` names
/// `exe` itself (`<root>/bin/codex`), so a stray parent manifest never answers.
/// `None` otherwise.
#[must_use]
pub fn package_version(exe: &Path) -> Option<Version> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = package_text(&root.join(PACKAGE_JSON))?;
        let (version, entry) = parse_package(&text)?;
        (std::fs::canonicalize(root.join(entry)).ok()? == exe).then_some(version)
    })
}

/// One `codex-package.json`: its version and entrypoint (relative to the root).
/// `None` unless `layoutVersion` is 1, the entrypoint is one
/// ([`package_entrypoint`]) and the version is one [`Version::parse`] reads.
#[must_use]
pub fn parse_package(text: &str) -> Option<(Version, PathBuf)> {
    let entry = package_entrypoint(text)?;
    let v: Value = aterm_json::from_str(text).ok()?;
    let version = Version::parse(v.get("version")?.as_str()?)?;
    Some((version, entry))
}

/// One `codex-package.json`'s entrypoint (relative to the root), whatever its
/// version says: `None` unless `layoutVersion` is 1 and the entrypoint is a
/// relative path inside the root. Which package names a binary needs no
/// version, and an ALab build's is a pre-release (`X.Y.0-dev.<n>+g<sha>`, the
/// publication scheme) that [`Version::parse`] refuses on purpose, so
/// [`alab_bytes`] reads only this; ordering builds stays [`package_version`]'s, which no ALab
/// build ever answers.
#[must_use]
pub fn package_entrypoint(text: &str) -> Option<PathBuf> {
    let v: Value = aterm_json::from_str(text).ok()?;
    if v.get("layoutVersion").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let entry = Path::new(v.get("entrypoint")?.as_str()?);
    // Names only, so the join stays under the root: on Windows a root-relative
    // `/abs/codex` is not `is_absolute` (it has no drive), yet joined to the
    // root it replaces everything but the drive, and `C:codex` keeps its own.
    if entry
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(entry.to_path_buf())
}

// ---------------------------------------------------------------- the flags

/// How a Codex flag takes its value (clap, `codex --help` 0.157.1).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arity {
    /// A switch.
    None,
    /// Exactly one value: the next token whatever it looks like, or attached
    /// (`--model=x`, `-mx`).
    One,
    /// `<FILE>...`: one value or more, up to the next token that starts with
    /// `-`.
    Many,
}

/// What the rewrite does with a flag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value.
    Drop,
    /// The relaunch is refused.
    Refuse,
}

/// Codex's interactive options (`codex --help`, `codex resume --help`). A flag
/// missing here refuses the rewrite; new flags are added once known safe. The
/// flags that keep a session out of the daemon (`-c`, `--profile`, `--oss`,
/// `--strict-config`, `--no-daemon`, the hook-trust bypass) are kept, so the
/// relaunch lands in the mode it left.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("-a", Arity::One, Fate::Keep),
    ("--add-dir", Arity::One, Fate::Keep),
    ("--all", Arity::None, Fate::Drop),
    ("--approve-for-me", Arity::None, Fate::Keep),
    ("--ask-for-approval", Arity::One, Fate::Keep),
    ("-C", Arity::One, Fate::Keep),
    ("-c", Arity::One, Fate::Keep),
    ("--cd", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    (
        "--dangerously-bypass-approvals-and-sandbox",
        Arity::None,
        Fate::Keep,
    ),
    ("--dangerously-bypass-hook-trust", Arity::None, Fate::Keep),
    ("--disable", Arity::One, Fate::Keep),
    ("--enable", Arity::One, Fate::Keep),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    // Attached to the first prompt, which the conversation already holds.
    ("-i", Arity::Many, Fate::Drop),
    ("--image", Arity::Many, Fate::Drop),
    ("--include-non-interactive", Arity::None, Fate::Drop),
    ("--last", Arity::None, Fate::Drop),
    ("--local-provider", Arity::One, Fate::Keep),
    // The model is carried, never moved: aterm reads no Codex model catalog,
    // and a launch's model is kept verbatim.
    ("-m", Arity::One, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("--no-alt-screen", Arity::None, Fate::Keep),
    ("--no-daemon", Arity::None, Fate::Keep),
    ("--oss", Arity::None, Fate::Keep),
    ("-p", Arity::One, Fate::Keep),
    ("--profile", Arity::One, Fate::Keep),
    // A TUI attached to another app server: not this machine's daemon, and
    // not a session this sweep can see the thread of.
    ("--remote", Arity::One, Fate::Refuse),
    ("--remote-auth-token-env", Arity::One, Fate::Refuse),
    ("-s", Arity::One, Fate::Keep),
    ("--sandbox", Arity::One, Fate::Keep),
    ("--search", Arity::None, Fate::Keep),
    ("--strict-config", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    // A session in a managed worktree Codex made for it: a resume in the tab
    // would not land there.
    ("--worktree", Arity::None, Fate::Refuse),
];

pub(crate) fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Codex's subcommands. Only none, `resume` and `fork` run the interactive TUI
/// a relaunch resumes; the rest are refused ([`ArgvRefusal::NotResumable`]).
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "app",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "resume",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "fork",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// Whether clap reads `tok` as an option: a leading `-` and more after it (a
/// lone `-` is a value).
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// The flags [`rewrite_argv`] carries from `argv` (`argv[0]` first),
/// verbatim and in argv order, of those whose name `keep` takes; every flag
/// is read, so a refusal stands whatever `keep` takes.
pub(crate) fn kept_flags(
    argv: &[String],
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<String>, ArgvRefusal> {
    let mut kept: Vec<String> = Vec::new();
    let mut subcommand: Option<&str> = None;
    let mut i = 1;
    let mut positional_only = false;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" && !positional_only {
            positional_only = true;
            continue;
        }
        if positional_only || !maybe_option(tok) {
            // The first positional is the subcommand when it names one, else
            // the prompt (dropped, as is every positional after it).
            if subcommand.is_none() && !positional_only {
                if SUBCOMMANDS.contains(&tok) {
                    if tok != "resume" && tok != "fork" {
                        return Err(ArgvRefusal::NotResumable(tok.to_string()));
                    }
                    subcommand = Some(tok);
                    continue;
                }
                subcommand = Some("");
            }
            continue;
        }
        // `--name=value`, `-Xvalue` (a short flag with its value attached), or
        // the flag alone.
        let (name, attached) = if let Some((n, _)) = tok.split_once('=')
            && n.starts_with("--")
        {
            (n.to_string(), true)
        } else if !tok.starts_with("--") && tok.chars().count() > 2 {
            (tok.chars().take(2).collect::<String>(), true)
        } else {
            (tok.to_string(), false)
        };
        let (arity, fate) = flag(&name).ok_or_else(|| ArgvRefusal::UnknownFlag(name.clone()))?;
        if fate == Fate::Refuse {
            return Err(ArgvRefusal::NotResumable(name));
        }
        if attached && arity == Arity::None {
            // `--no-daemon=x`, `-hV`: a shape clap refuses; so does this.
            return Err(ArgvRefusal::UnknownFlag(tok.to_string()));
        }
        let start = i - 1;
        if !attached {
            match arity {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Many => {
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(&argv[i]) && argv[i] != "--" {
                        i += 1;
                    }
                }
            }
        }
        if fate == Fate::Keep && keep(&name) {
            kept.extend(argv[start..i].iter().cloned());
        }
    }
    Ok(kept)
}

/// The argv rewrite for Codex: the TUI's argv (`argv[0]` dropped) becomes the
/// words after the executable on the relaunch line: `resume <kept flags
/// verbatim> <thread>`, or with no thread (no conversation yet) the kept flags
/// alone. The launch's subcommand, session id, first prompt and images are
/// dropped: the conversation holds them, and a `fork` has already forked.
/// What a proven self-claim's live argv is checked against
/// ([`super::self_claimed`]) and a restart's line is promised by
/// (`relaunch::carries_restart`).
///
/// # Errors
/// An unknown flag or subcommand ([`ArgvRefusal::UnknownFlag`]), or a session
/// not resumable in place ([`ArgvRefusal::NotResumable`]: `exec`, `--remote`,
/// `--worktree`, …).
pub fn rewrite_argv(argv: &[String], thread: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    Ok(resume_words(kept_flags(argv, |_| true)?, thread))
}

/// `resume <kept> <thread>`, or the kept flags alone with no thread: the
/// words after the executable on a Codex resume line.
pub(crate) fn resume_words(kept: Vec<String>, thread: Option<&str>) -> Vec<String> {
    match thread {
        Some(t) => std::iter::once("resume".to_string())
            .chain(kept)
            .chain(std::iter::once(t.to_string()))
            .collect(),
        None => kept,
    }
}

/// The variable Codex keeps its home by: its rollouts, thread writer locks,
/// login and daemon live under `$CODEX_HOME`, else `~/.codex`.
pub const HOME_ENV: &str = "CODEX_HOME";

/// THE ENVIRONMENT A CODEX RELAUNCH LINE MUST CARRY: [`HOME_ENV`] with the
/// value the TUI it replaces had in its own exec environment, verbatim, or
/// nothing where it had none (the new TUI then takes its shell's). A TUI
/// started as `CODEX_HOME=<dir> codex` (its own command line, or a wrapper's;
/// the tab's shell never had it) keeps its thread under `<dir>`: relaunched
/// without it, it resumes under `~/.codex`, where that thread does not
/// exist, as a Claude Code relaunched without its `CLAUDE_CONFIG_DIR` came
/// back on the wrong home (`relaunch`'s module doc, measured 2026-10-09).
/// Required, as the Claude lane's home and login are
/// (`relaunch::required_env`): the line is refused rather than typed without
/// it.
#[must_use]
pub fn line_env(home: Option<&str>) -> Vec<(&'static str, &str)> {
    home.map(|value| (HOME_ENV, value)).into_iter().collect()
}

/// Whether `argv` is an interactive Codex TUI's (no subcommand, `resume` or
/// `fork`), not the daemon, its updater, `exec` or another face of the binary.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            return true;
        }
        if !maybe_option(tok) {
            return !SUBCOMMANDS.contains(&tok) || tok == "resume" || tok == "fork";
        }
        let attached = tok.contains('=') || (!tok.starts_with("--") && tok.chars().count() > 2);
        if !attached {
            let name = tok;
            if matches!(flag(name), Some((Arity::One | Arity::Many, _))) {
                i += 1;
            }
        }
    }
    true
}

/// The thread a TUI's argv resumes (`codex [flags] resume [flags] <id>`): the
/// first positional after `resume` when it is a thread id ([`is_thread_id`]).
/// `None` for a plain launch, `resume --last`, a picker, or a `fork`.
#[must_use]
pub fn resumed_thread(argv: &[String]) -> Option<String> {
    let mut i = 1;
    let mut resuming = false;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            if !resuming {
                if tok != "resume" {
                    return None;
                }
                resuming = true;
                continue;
            }
            return is_thread_id(tok).then(|| tok.to_string());
        }
        let attached = tok.contains('=') || (!tok.starts_with("--") && tok.chars().count() > 2);
        if !attached && matches!(flag(tok), Some((Arity::One | Arity::Many, _))) {
            i += 1;
        }
    }
    None
}

/// The thread a writer-lock file name names (`<thread>.lock`).
#[must_use]
pub fn lock_thread(name: &str) -> Option<&str> {
    name.strip_suffix(".lock").filter(|t| is_thread_id(t))
}

#[cfg(test)]
#[path = "codex_alab_tests.rs"]
mod tests;
