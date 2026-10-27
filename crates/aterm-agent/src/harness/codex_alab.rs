// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `codex-alab` package metadata, build identification, and upgrade argv reconstruction.

use std::path::{Component, Path, PathBuf};

use aterm_json::Value;

use super::codex_rollout::is_thread_id;
use super::upgrade::{ArgvRefusal, Version};

/// The manifest every Codex package carries at its root (layoutVersion 1).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// The file an ALab build of Codex (codex-alab) carries in its package root.
pub const ALAB_BUILD_JSON: &str = "alab-build.json";

/// Optional package metadata is small.
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;

fn package_text(path: &Path) -> Option<String> {
    let text = super::footer::read_small(path, MAX_PACKAGE_BYTES + 1)?;
    (text.len() as u64 <= MAX_PACKAGE_BYTES).then_some(text)
}

/// Whether the Codex `exe` runs ALAB BYTES (codex-alab).
#[must_use]
pub fn alab_bytes(exe: &Path) -> bool {
    alab_marker(exe).is_some_and(|marker| marker.is_ok_and(|m| m.file_type().is_file()))
}

/// The [`ALAB_BUILD_JSON`] look of `exe`'s package root.
pub(crate) fn alab_marker(exe: &Path) -> Option<std::io::Result<std::fs::Metadata>> {
    let (root, _) = package_root(exe)?;
    Some(std::fs::symlink_metadata(root.join(ALAB_BUILD_JSON)))
}

/// The BUILD an ALab package's `exe` runs, as its rollouts name it (`session_meta`'s
/// `cli_version`, the key of [`super::codex_steps::CODEX_MEASURED_BUILDS`]): the package's
/// CLIENT version ([`atpkg::alab::client_version`]), never its own.
///
/// From codex-alab's own version line on (codex-alab `alab/README.md`, Decision 8,
/// 2026-10-24) a package carries two versions: the manifest's `version` (and the
/// marker's) is codex-alab's own line (`0.1.0-dev.<count>+codex.<A.B.C>.g<sha9>`), and the
/// marker's `client_version` is upstream's numbering (`0.165.0-dev.<count>+g<sha9>`), which
/// upstream's code writes as every rollout's `cli_version`. So the build is the marker's
/// `client_version` where it records one, and the manifest's `version` where it records none
/// (a package from before that line has one version, which is also its client version).
/// `None`, so the tap stays unmeasured, for a marker that is not a small regular JSON object,
/// whose `client_version` is not a string, or whose `version` is not the manifest's, and
/// wherever [`atpkg::alab::client_version`] names no client version (a malformed own or
/// client version, a client version naming another count or commit than the package's own,
/// or a compound own version with no client version): such a package names no build.
#[must_use]
pub fn alab_build(exe: &Path) -> Option<String> {
    let (root, manifest) = package_root(exe)?;
    let marker_path = root.join(ALAB_BUILD_JSON);
    let marker = std::fs::symlink_metadata(&marker_path).ok()?;
    if !marker.file_type().is_file() {
        return None;
    }
    let v: Value = aterm_json::from_str(&manifest).ok()?;
    let version = v.get("version")?.as_str()?;
    let record: Value = aterm_json::from_str(&package_text(&marker_path)?).ok()?;
    let record = record.as_object()?;
    if record
        .get("version")
        .is_some_and(|own| own.as_str() != Some(version))
    {
        return None;
    }
    let client = match record.get("client_version") {
        None => None,
        Some(client) => Some(client.as_str()?),
    };
    atpkg::alab::client_version(version, client).map(str::to_string)
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

/// The version of the Codex `exe`, read from its package manifest, never by running it.
#[must_use]
pub fn package_version(exe: &Path) -> Option<Version> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = package_text(&root.join(PACKAGE_JSON))?;
        let (version, entry) = parse_package(&text)?;
        (std::fs::canonicalize(root.join(entry)).ok()? == exe).then_some(version)
    })
}

/// One `codex-package.json`.
#[must_use]
pub fn parse_package(text: &str) -> Option<(Version, PathBuf)> {
    let entry = package_entrypoint(text)?;
    let v: Value = aterm_json::from_str(text).ok()?;
    let version = Version::parse(v.get("version")?.as_str()?)?;
    Some((version, entry))
}

/// One `codex-package.json`'s entrypoint (relative to the root), whatever its version says.
#[must_use]
pub fn package_entrypoint(text: &str) -> Option<PathBuf> {
    let v: Value = aterm_json::from_str(text).ok()?;
    if v.get("layoutVersion").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let entry = Path::new(v.get("entrypoint")?.as_str()?);
    if entry
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(entry.to_path_buf())
}

/// How a Codex flag takes its value (clap, `codex --help` 0.157.1).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arity {
    /// A switch.
    None,
    /// Exactly one value.
    One,
    /// `<FILE>...`: one value or more, up to the next token that starts with `-`.
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

/// Codex's interactive options (`codex --help`, `codex resume --help`), and the hidden
/// aliases its shared options declare (`--yolo`, `--not-so-yolo`).
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
    ("-i", Arity::Many, Fate::Drop),
    ("--image", Arity::Many, Fate::Drop),
    ("--include-non-interactive", Arity::None, Fate::Drop),
    ("--last", Arity::None, Fate::Drop),
    ("--local-provider", Arity::One, Fate::Keep),
    ("-m", Arity::One, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("--no-alt-screen", Arity::None, Fate::Keep),
    ("--no-daemon", Arity::None, Fate::Keep),
    ("--not-so-yolo", Arity::None, Fate::Keep),
    ("--oss", Arity::None, Fate::Keep),
    ("-p", Arity::One, Fate::Keep),
    ("--profile", Arity::One, Fate::Keep),
    ("--remote", Arity::One, Fate::Refuse),
    ("--remote-auth-token-env", Arity::One, Fate::Refuse),
    ("-s", Arity::One, Fate::Keep),
    ("--sandbox", Arity::One, Fate::Keep),
    ("--search", Arity::None, Fate::Keep),
    ("--strict-config", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("--worktree", Arity::None, Fate::Refuse),
    ("--yolo", Arity::None, Fate::Keep),
];

pub(crate) fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Codex's subcommands; all but `resume` and `fork` are refused ([`ArgvRefusal::NotResumable`]).
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
    "migrate-nfs-db",
    "unarchive",
    "fork",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// Whether clap reads `tok` as an option.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// The flags [`rewrite_argv`] carries from `argv` (`argv[0]` first), each where `keep`
/// holds of its name and its tokens (the flag's own, then its values).
pub(crate) fn kept_flags(
    argv: &[String],
    keep: impl Fn(&str, &[String]) -> bool,
) -> Result<Vec<String>, ArgvRefusal> {
    let (mut root, child) = kept_flag_levels(argv, keep)?;
    root.extend(child);
    Ok(root)
}

/// The same flag read as [`kept_flags`], retaining the root and resume/fork
/// command levels so the record's relaunch keeps each override in its scope.
fn kept_flag_levels(
    argv: &[String],
    keep: impl Fn(&str, &[String]) -> bool,
) -> Result<(Vec<String>, Vec<String>), ArgvRefusal> {
    let mut root = Vec::new();
    let mut child = Vec::new();
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
        if fate == Fate::Keep && keep(&name, &argv[start..i]) {
            let kept = if matches!(subcommand, Some("resume" | "fork")) {
                &mut child
            } else {
                &mut root
            };
            kept.extend(argv[start..i].iter().cloned());
        }
    }
    Ok((root, child))
}

/// The argv rewrite for Codex: root flags stay before `resume`, child flags
/// after it. With no thread, the kept flags alone retain their launch order.
///
/// # Errors
/// An unknown flag, or a value attached to a flag that takes none
/// ([`ArgvRefusal::UnknownFlag`]), or a session not resumable in place
/// ([`ArgvRefusal::NotResumable`]: `exec`, `--remote`, `--worktree`, …).
pub fn rewrite_argv(argv: &[String], thread: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let (mut root, child) = kept_flag_levels(argv, |_, _| true)?;
    if thread.is_some() {
        root.push("resume".to_string());
    }
    root.extend(child);
    if let Some(thread) = thread {
        root.push(thread.to_string());
    }
    Ok(root)
}

/// `resume <kept> <thread>`, or the kept flags alone with no thread.
pub(crate) fn resume_words(kept: Vec<String>, thread: Option<&str>) -> Vec<String> {
    match thread {
        Some(t) => std::iter::once("resume".to_string())
            .chain(kept)
            .chain(std::iter::once(t.to_string()))
            .collect(),
        None => kept,
    }
}

/// The variable Codex keeps its home by.
pub const HOME_ENV: &str = "CODEX_HOME";

/// The variable `codex-alab` reads and refreshes a pooled login's `auth.json` at.
pub const AUTH_PATH_ENV: &str = "CODEX_AUTH_PATH";

/// THE ENVIRONMENT A CODEX RELAUNCH LINE MUST CARRY.
#[must_use]
pub fn line_env(home: Option<&str>) -> Vec<(&'static str, &str)> {
    home.map(|value| (HOME_ENV, value)).into_iter().collect()
}

/// Skips the separate values of the flag token just read (`i` at the next
/// token), as [`kept_flag_levels`] consumes them: one for [`Arity::One`], and
/// for [`Arity::Many`] the first whatever it looks like plus each following
/// non-option. An attached value (`--image=a.png`, `-ia.png`) takes none.
fn skip_flag_values(argv: &[String], i: &mut usize, tok: &str) {
    let attached = tok.contains('=') || (!tok.starts_with("--") && tok.chars().count() > 2);
    if attached {
        return;
    }
    match flag(tok) {
        Some((Arity::One, _)) => {
            if *i < argv.len() {
                *i += 1;
            }
        }
        Some((Arity::Many, _)) => {
            if *i < argv.len() {
                *i += 1;
            }
            while *i < argv.len() && !maybe_option(&argv[*i]) && argv[*i] != "--" {
                *i += 1;
            }
        }
        _ => {}
    }
}

/// Whether `argv` is an interactive Codex TUI's.
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
        skip_flag_values(argv, &mut i, tok);
    }
    true
}

/// The thread a TUI's argv resumes: the first positional after `resume` when it is a thread id.
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
        skip_flag_values(argv, &mut i, tok);
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
