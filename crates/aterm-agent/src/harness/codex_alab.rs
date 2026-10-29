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
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
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
    package_versions(exe).map(|(_, client)| client)
}

/// The product version of the ALab package whose entrypoint is `exe`, including its dev
/// count and source metadata. The same bounded manifest and marker validation as
/// [`alab_build`] applies; the legacy numeric [`package_version`] ordering stays separate.
#[must_use]
pub fn product_version(exe: &Path) -> Option<String> {
    package_versions(exe).map(|(product, _)| product)
}

/// Validated `(product, client)` versions of the ALab package the current development link
/// selects. These are selected package metadata, not evidence of a running process's version.
/// Both versions come from the same bounded manifest/build-record read and name one build.
#[must_use]
pub fn linked_package_versions(layout: &atpkg::Layout, program: &str) -> Option<(String, String)> {
    atpkg::alab::agent(program)?;
    let checkout = atpkg::linkmode::live_linked_checkout(layout, program).ok()??;
    let exe = atpkg::which(layout, program)?.canonicalize().ok()?;
    exe.starts_with(checkout).then_some(())?;
    package_versions(&exe)
}

fn package_versions(exe: &Path) -> Option<(String, String)> {
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
    let client = atpkg::alab::client_version(version, client)?;
    Some((version.to_owned(), client.to_owned()))
}

/// `exe`'s package root (its parent or grandparent) whose manifest names
/// `exe` itself, and that manifest's text.
fn package_root(exe: &Path) -> Option<(PathBuf, String)> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = package_text(&root.join(PACKAGE_JSON))?;
        let entry_path = root.join(package_entrypoint(&text)?);
        if !std::fs::symlink_metadata(&entry_path)
            .ok()?
            .file_type()
            .is_file()
        {
            return None;
        }
        let entry = std::fs::canonicalize(entry_path).ok()?;
        (entry == exe).then(|| (root.to_path_buf(), text))
    })
}

/// The version of the Codex `exe`, read from its package manifest, never by running it.
#[must_use]
pub fn package_version(exe: &Path) -> Option<Version> {
    let (_, text) = package_root(exe)?;
    let (version, _) = parse_package(&text)?;
    Some(version)
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

/// Codex's subcommands (`codex-rs/cli/src/main.rs` `Subcommand`, including hidden internal
/// subcommands and aliases); all but `resume` and `fork` are refused
/// ([`ArgvRefusal::NotResumable`]).
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "tcp-tunnel",
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
    "execpolicy",
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
    "cloud-tasks",
    "responses-api-proxy",
    "stdio-to-uds",
    "exec-server",
    "features",
    "help",
];

/// Whether clap reads `tok` as an option.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// Whether `argv[0]` is a `codex_arg0` helper alias (`apply_patch`, `applypatch`, `codex-linux-sandbox`).
fn arg0_helper_name(argv: &[String]) -> Option<&str> {
    argv.first()
        .and_then(|arg0| Path::new(arg0).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| matches!(*name, "apply_patch" | "applypatch" | "codex-linux-sandbox"))
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
    if let Some(helper) = arg0_helper_name(argv) {
        return Err(ArgvRefusal::NotResumable(helper.to_string()));
    }
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

/// The variable `codex-alab` reads and refreshes a pooled login's `auth.json` at, from its
/// 355ba0a940 on; a row's login stays its own under it from codex-alab's cd79a420c on
/// ([`AUTH_PATH_MARK`]; [`auth_path_unread`] names the builds it does not).
pub const AUTH_PATH_ENV: &str = "CODEX_AUTH_PATH";

/// The words codex-alab's TUI names its shared-daemon exclusion by under [`AUTH_PATH_ENV`]
/// (`alab-tui`'s `auth_path_daemon_exclusion_with`), which its binary holds from its cd79a420c
/// (2026-10-27) on, the newest of its `CODEX_AUTH_PATH` fixes that adds words a binary keeps.
/// From 355ba0a940 it read the variable, but until 6ecfc876e a keyring or `auto`
/// `cli_auth_credentials_store` (a `-c` override among them) read `$CODEX_HOME`'s shared secrets
/// or moved the named file into the OS keyring on a refresh, and until cd79a420c a shared
/// `$CODEX_HOME` daemon (`[features] daemon_auto_start = true`) ran every tab under the
/// environment of the tab that started it. Found by `git grep` in codex-alab: absent at
/// 355ba0a940, 593e51f6b and 6ecfc876e, present at cd79a420c. Two of its sync commits
/// (9bb0a0792, 92b81c0ff) hold the words but not 837738769, before which its vanilla mode
/// attached a running shared daemon; a build cut there passes. A rewording of the words in
/// codex-alab refuses its later builds here until this follows it (fail closed).
pub const AUTH_PATH_MARK: &str = "per-account auth override (CODEX_AUTH_PATH)";

/// The most of a launcher script [`auth_path_unread`] reads for the binary it execs.
const LAUNCHER_MAX: u64 = 64 * 1024;

/// Bytes read per step of [`file_holds`]'s scan.
const SCAN_CHUNK: usize = 4 << 20;

/// Why a Codex program would not run a pool row's own login ([`auth_path_unread`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPathUnread {
    /// A bare word (which the shell resolves on its own `PATH`, where an npm or Homebrew `codex`
    /// is OpenAI's), a relative path, or a script that is not atpkg's launcher: it names no
    /// build aterm can read before it runs.
    Unnamed(PathBuf),
    /// The binary it execs is not a file here.
    Missing(PathBuf),
    /// The binary it execs is not codex-alab ([`alab_bytes`]): OpenAI's builds read only
    /// `$CODEX_HOME/auth.json`.
    Vendor(PathBuf),
    /// The binary it execs is a codex-alab whose bytes do not hold [`AUTH_PATH_MARK`].
    Older(PathBuf),
    /// The binary it execs could not be read, and why.
    Unreadable(PathBuf, String),
}

impl std::fmt::Display for AuthPathUnread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unnamed(program) => write!(
                f,
                "`{}` names no Codex build aterm can read before it runs (a bare word is found \
                 on the shell's own `PATH`, where an npm or Homebrew `codex` is OpenAI's build, \
                 which reads only `$CODEX_HOME/auth.json`; a relative path is the shell's \
                 folder's; a script that is not atpkg's launcher names none)",
                program.display()
            ),
            Self::Missing(exe) => write!(f, "{} is not a file here", exe.display()),
            Self::Vendor(exe) => write!(
                f,
                "{} is not codex-alab, and OpenAI's builds (codex-old, or `codex` from npm or \
                 Homebrew) read only `$CODEX_HOME/auth.json`, never {AUTH_PATH_ENV}",
                exe.display()
            ),
            Self::Older(exe) => write!(
                f,
                "{} is a codex-alab from before its cd79a420c, the first that keeps a row's \
                 login its own under {AUTH_PATH_ENV} (an older one reads only \
                 `$CODEX_HOME/auth.json`, or can share the keyring or a daemon across logins)",
                exe.display()
            ),
            Self::Unreadable(exe, e) => write!(
                f,
                "{} could not be read to see whether it keeps {AUTH_PATH_ENV}'s login ({e})",
                exe.display()
            ),
        }
    }
}

/// Why the Codex `program` a launch types would not run a pool row's own login, which reaches
/// it only as [`AUTH_PATH_ENV`] beside the shared `$CODEX_HOME`, or `None` where it would: the
/// binary it execs (itself, or the one an atpkg launcher's last `exec '<path>' "$@"` names,
/// [`super::upgrade_catalog::twin_binary`]) is codex-alab ([`alab_bytes`]) and holds
/// [`AUTH_PATH_MARK`]. A build that does not read the variable reads and refreshes
/// `$CODEX_HOME/auth.json`, the machine's default login, so a launch under a row of its own
/// would run, and renew, on that login while the pool counts its turns as the row's.
///
/// Measured 2026-10-27 with placeholder API-key logins in scratch homes (`login status`, then
/// `login --with-api-key` reading a third key, with `CODEX_AUTH_PATH` naming a second file, and
/// with it unset or naming a missing file): codex-old 0.164.0 and 0.165.0 and Homebrew's `codex`
/// 0.151.0 (OpenAI's, team 2DC432GLL2) and codex-alab 0.1.0-dev.13633 (source 9fef22351, the
/// atpkg store's build then) each reported the `$CODEX_HOME/auth.json` key and rewrote that file
/// in all three cases, never the named one, and no byte of any of their binaries spells the
/// variable's name.
#[must_use]
pub fn auth_path_unread(program: &Path) -> Option<AuthPathUnread> {
    let Some(exe) = exec_target(program) else {
        return Some(AuthPathUnread::Unnamed(program.to_path_buf()));
    };
    if !exe.is_file() {
        return Some(AuthPathUnread::Missing(exe));
    }
    if !alab_bytes(&exe) {
        return Some(AuthPathUnread::Vendor(exe));
    }
    match holds_auth_path_mark(&exe) {
        Ok(true) => None,
        Ok(false) => Some(AuthPathUnread::Older(exe)),
        Err(e) => Some(AuthPathUnread::Unreadable(exe, e.to_string())),
    }
}

/// The one act that runs Codex again while [`auth_path_unread`] refuses a row of its own: pin
/// the roster's row on this machine's own login, `default` (the row with no `dir`), or, where
/// the roster lists none, list that login as a row first.
#[must_use]
pub fn unread_fix(default: Option<&str>) -> String {
    match default {
        Some(label) => format!("aterm harness accounts use {label} --cli codex"),
        None => "aterm harness accounts adopt-default <label> --email <e> --cli codex".to_string(),
    }
}

/// What a refusal under [`auth_path_unread`] tells the person to do: [`unread_fix`] (where the
/// roster lists no row on this machine's own login, the listing and then its pin, since a listing
/// alone leaves the pin where it is), and the pin's end (`accounts use --auto`) once atpkg serves
/// a codex-alab that holds [`AUTH_PATH_MARK`]. It starts lower-case: [`only_default_in`] joins
/// it after "; ".
#[must_use]
pub fn unread_advice(default: Option<&str>) -> String {
    let (pin, steps) = match default {
        Some(_) => (
            "pinning the account on this machine's own login",
            format!("`{}`", unread_fix(default)),
        ),
        None => (
            "listing this machine's own login as an account and pinning it",
            format!(
                "`{}`, then `{}`",
                unread_fix(None),
                unread_fix(Some("<label>"))
            ),
        ),
    };
    format!(
        "{pin} ({steps}) runs Codex on that login; once atpkg serves a codex-alab from its \
         cd79a420c on (`aterm pkg update codex` then), `{UNPIN_FIX}` lets the pool pick again"
    )
}

/// What the roster's row on this machine's own login (the one with no `dir`) can do for a
/// launch on a Codex program that keeps no row's login its own ([`auth_path_unread`]): on such a
/// program it is the only row that may serve one ([`only_default`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultRow {
    /// The roster lists no row on this machine's own login, and no row of it was ever on that
    /// login.
    Unlisted,
    /// The row `label` was on this machine's own login until a sign-in bound it to a login of
    /// its own (its binding's earlier logins, [`super::accounts::Row::was`], hold the default
    /// home), and no row of the roster is on that login now.
    Moved(String),
    /// The row, labelled so, can take the launch.
    Serves(String),
    /// The row `label` stands at its `window` limit until `until`, the instant the pool takes it
    /// again; `reset` is the reset the server named, where it named one.
    Walled {
        /// The row's label.
        label: String,
        /// The limit it stands at (`five_hour`, `seven_day`).
        window: String,
        /// The reset the server named.
        reset: Option<i64>,
        /// When the pool takes the row again.
        until: i64,
    },
    /// No login of the row `label` here is current (none is stored, or its last one on record
    /// ended): Codex's own sign-in in this machine's default home ([`DEFAULT_SIGN_IN`]) makes
    /// one.
    SignedOut {
        /// The row's label.
        label: String,
        /// Whether a login of it is stored here, one the pool's records say ended
        /// ([`super::accounts::Condition::SignedOut`]); `false` where no login file of it is
        /// ([`super::accounts::Condition::NotEnrolled`], whether none was ever made here or one
        /// was removed since), so its sign-in is not said to sign it in again
        /// ([`signed_out_says`]).
        stored: bool,
    },
    /// The row `label` cannot take a launch here now for another reason the pool names (held by
    /// another Mac's warm set, claimed elsewhere, or unclaimed where this Mac cannot record its
    /// claims and is not its home).
    Other {
        /// The row's label.
        label: String,
        /// The pool's words for why.
        why: String,
    },
}

/// Why a launch on a Codex program that keeps no row's login its own (`why`) cannot run under
/// a row other than this machine's own login, and what `default`, that login's row, does about
/// it. `row` is the row of its own the launch would have taken (the review's serving row; the
/// owner's pin is [`only_default_pinned`]'s), `None` where the pool would have picked one only
/// for being the default's alternative; a row the starter carried while the pool picks none is
/// [`only_default_carried`]'s. The words come from the default row's own condition: a wall names
/// its limit and reset and that no Codex account can run on this build until the row serves
/// again, a signed-out row Codex's own sign-in ([`signed_out_default`]), a row a sign-in moved
/// off that login that no row the pool lists runs here until a build that keeps it comes, while
/// rotation off with no pin runs a launch on that login with no row (no sign-in or pin puts the
/// row back on it; the words say that rotation off, `[harness] accounts = "off"`, also turns off
/// the accounts' sync and aterm's own sign-ins of every pool,
/// [`super::accounts_sync::accounts_sync_off`]), and only a row that can serve is offered a pin
/// ([`unread_advice`]), so no advice is one the pool's next usage read undoes.
#[must_use]
pub fn only_default(why: &AuthPathUnread, row: Option<&str>, default: &DefaultRow) -> String {
    only_default_in(why, row, default, Took::Pick)
}

/// [`only_default`] for a launch the owner's pin holds to `pinned`, a row of its own: the pin
/// stays where it is whatever the row on this machine's own login does, so that row serves only
/// once the pin on `pinned` ends, by its own pin ([`unread_fix`]) or the pin's end
/// ([`UNPIN_FIX`]). A walled row serves once its wall ends with the pin on `pinned` ended: the
/// pin's end may come at any time, its own pin only once the wall ends (a pin clears the row's
/// walls, which the next usage read would put back). A signed-out row's sign-in is followed by
/// its pin.
#[must_use]
pub fn only_default_pinned(why: &AuthPathUnread, pinned: &str, default: &DefaultRow) -> String {
    only_default_in(why, Some(pinned), default, Took::Pin)
}

/// [`only_default`] for a launch the pool picked no row for whose starter carried `row`'s own
/// login ([`AUTH_PATH_ENV`]): rotation off with no pin (`rotation_off`), or a pool that picks no
/// row for any launch here (Codex's login kept in the OS keyring,
/// [`super::accounts_pool::Seat::keyring_held`]). A row a sign-in moved off this machine's own
/// login is told the way back that works there, a starter that carries no row's
/// [`AUTH_PATH_ENV`], which takes no row and runs on that login, and is told rotation is off only
/// where it is.
#[must_use]
pub fn only_default_carried(
    why: &AuthPathUnread,
    row: &str,
    default: &DefaultRow,
    rotation_off: bool,
) -> String {
    only_default_in(why, Some(row), default, Took::Carried(rotation_off))
}

/// How the launch [`only_default_in`] words came by its row.
#[derive(Clone, Copy)]
enum Took {
    /// The pool's pick, or the review's serving row ([`only_default`]).
    Pick,
    /// The owner's pin ([`only_default_pinned`]).
    Pin,
    /// The starter's own login, under rotation off with no pin where `true`
    /// ([`only_default_carried`]).
    Carried(bool),
}

/// [`only_default`]'s words, for a launch that took its row as `took` says.
fn only_default_in(
    why: &AuthPathUnread,
    row: Option<&str>,
    default: &DefaultRow,
    took: Took,
) -> String {
    use super::usage::rfc3339_utc;
    let pinned = match (took, row) {
        (Took::Pin, Some(own)) => Some(own),
        _ => None,
    };
    let lead = match row {
        Some(label) => format!(
            "Codex account `{label}` is a row of its own, but {why}: its turns would run on this \
             machine's default login while the pool counts them as `{label}`'s"
        ),
        None => format!(
            "This Codex build runs only this machine's own login, since {why}, so no other Codex \
             account is started on it"
        ),
    };
    let then = match default {
        DefaultRow::Unlisted => format!(
            "the roster lists no row on that login; {}",
            unread_advice(None)
        ),
        DefaultRow::Moved(label) => format!(
            "`{label}`, the row that login was listed as, was signed in to a login of its own, so \
             no row of the roster is on that login now, and no Codex account the pool lists can \
             run on this build until {BUILD_SERVED}; {}",
            if let Took::Carried(rotation_off) = took {
                format!(
                    "{} a worker whose starter carries no row's `{AUTH_PATH_ENV}` takes no row \
                     and runs on this machine's own login",
                    if rotation_off {
                        "with rotation off and no account pinned, as here,"
                    } else {
                        "the pool picks no row for a Codex launch here, so"
                    }
                )
            } else {
                "with rotation off and no account pinned, a Codex launch takes no row and runs on \
                 this machine's own login, but rotation off is `[harness] accounts = \"off\"` in \
                 aterm.toml, which turns off every pool's rotation, the accounts' sync and \
                 aterm's own sign-ins and renewals of every AI CLI's accounts, Claude Code's \
                 included"
                    .to_string()
            }
        ),
        DefaultRow::Serves(label) => unread_advice(Some(label)),
        DefaultRow::Walled {
            label,
            window,
            reset,
            until,
        } => {
            let reset = match reset {
                Some(at) => format!("it resets at {}", rfc3339_utc(*at)),
                None => "no reset named".to_string(),
            };
            let until = rfc3339_utc(*until);
            match pinned {
                Some(own) => format!(
                    "`{label}`, the row on that login, is at its `{window}` limit ({reset}), and \
                     the owner's pin holds every launch to `{own}`, so no Codex account can run \
                     on this build until that pin ends and `{label}`'s wall ends at {until} \
                     (`{UNPIN_FIX}` ends the pin at any time, `{}` once that wall ends), or \
                     until {BUILD_SERVED}",
                    unread_fix(Some(label))
                ),
                None => format!(
                    "`{label}`, the row on that login, is at its `{window}` limit ({reset}), so \
                     no Codex account can run on this build until {until}, when `{label}` serves \
                     again, or until {BUILD_SERVED}"
                ),
            }
        }
        DefaultRow::SignedOut { label, stored } => match pinned {
            Some(own) => format!(
                "{}, and then its pin (`{}`) ends the owner's pin on `{own}`, which holds every \
                 launch to a row this build does not run",
                signed_out_default(label, *stored),
                unread_fix(Some(label))
            ),
            None => signed_out_default(label, *stored),
        },
        DefaultRow::Other { label, why } => format!(
            "`{label}`, the row on that login, cannot take it now ({why}), and no other Codex \
             account can run on this build until {BUILD_SERVED}"
        ),
    };
    format!("{lead}; {then}")
}

/// The owner's pin's end, which hands the Codex pool its pick again.
pub const UNPIN_FIX: &str = "aterm harness accounts use --auto --cli codex";

/// When a Codex program that keeps a row's login its own comes, in [`only_default`]'s words.
pub const BUILD_SERVED: &str =
    "atpkg serves a codex-alab from its cd79a420c on (`aterm pkg update codex` then)";

/// The sign-in of the row on this machine's own login on a Codex program that keeps no row's
/// login its own ([`auth_path_unread`]): Codex's own sign-in in its default home, which
/// `aterm ai login codex` runs while that login is signed out (or missing), rewriting
/// `~/.codex/auth.json` and leaving the row on that login. A pool sign-in (`aterm harness
/// accounts login|renew`) would bind the row to a login of its own, which such a build never
/// runs, so it refuses that row there ([`default_row_staging`]).
pub const DEFAULT_SIGN_IN: &str = "aterm ai login codex";

/// What a launch on a Codex program that keeps no row's login its own ([`auth_path_unread`])
/// says of `label`, the row on this machine's own login, while no login of it here is current
/// (none is stored, or its last one on record ended): no row the pool lists can run on that
/// build, and Codex's own sign-in in this machine's default home ([`DEFAULT_SIGN_IN`]) signs
/// that login in, keeping `label` on it, in again where a login of it is stored here (`stored`,
/// [`signed_out_says`]).
#[must_use]
pub fn signed_out_default(label: &str, stored: bool) -> String {
    let (state, sign_in) = signed_out_says(stored);
    format!(
        "`{label}`, the row on that login, is {state}, so no Codex account the pool lists can \
         run on this build: Codex's own sign-in in this machine's default home, \
         `{DEFAULT_SIGN_IN}`, signs that login {sign_in} and keeps `{label}` on it"
    )
}

/// What [`DefaultRow::SignedOut`] says of its row by whether a login of it is stored here
/// (`stored`): its state, and how Codex's own sign-in ([`DEFAULT_SIGN_IN`]) brings that ended
/// login back (`in again`), or, where none is stored (never made here, or removed since), signs
/// it in (`in`), which is true either way.
#[must_use]
pub const fn signed_out_says(stored: bool) -> (&'static str, &'static str) {
    if stored {
        ("signed out (no login of it here is current)", "in again")
    } else {
        ("not signed in here (no login of it is stored here)", "in")
    }
}

/// Why a pool sign-in (`aterm harness accounts login|renew`) of `label`, the row on this
/// machine's own login, is not made while this machine's Codex keeps no row's login its own
/// (`why`): it would bind the row to a staged login of its own, which that build never runs, so
/// no row of the pool would be left on the one login it runs. The row's sign-in there is
/// Codex's own ([`DEFAULT_SIGN_IN`]).
#[must_use]
pub fn default_row_staging(label: &str, why: &AuthPathUnread) -> String {
    format!(
        "`{label}` is the row on this machine's own login, the only login this machine's Codex \
         runs ({why}): a sign-in here would bind `{label}` to a login of its own, which that \
         build never runs, so it is not made; Codex's own sign-in in its default home, \
         `{DEFAULT_SIGN_IN}` (while that login is signed out), signs this machine's own login in \
         and keeps `{label}` on it, and this sign-in runs once {BUILD_SERVED}; nothing was run"
    )
}

/// The binary `program` execs: the one an atpkg launcher script names
/// ([`super::upgrade_catalog::twin_binary`]), else `program` itself; `None` for a bare word, a
/// relative path, and a script that names no binary so.
fn exec_target(program: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    if !program.is_absolute() {
        return None;
    }
    let Some(file) = super::footer::open_regular(program) else {
        return Some(program.to_path_buf());
    };
    let mut head = Vec::new();
    file.take(LAUNCHER_MAX + 1).read_to_end(&mut head).ok()?;
    if head.starts_with(b"#!") {
        if head.len() as u64 > LAUNCHER_MAX {
            return None;
        }
        return super::upgrade_catalog::twin_binary(std::str::from_utf8(&head).ok()?);
    }
    Some(program.to_path_buf())
}

/// Whether the file `exe` holds [`AUTH_PATH_MARK`], kept by its canonical path, size and
/// modification time for this process's life, so a build of hundreds of megabytes is read once.
fn holds_auth_path_mark(exe: &Path) -> std::io::Result<bool> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    type Key = (PathBuf, u64, std::time::SystemTime);
    static SEEN: OnceLock<Mutex<HashMap<Key, bool>>> = OnceLock::new();
    let exe = std::fs::canonicalize(exe)?;
    let meta = std::fs::metadata(&exe)?;
    let key = meta.modified().ok().map(|at| (exe.clone(), meta.len(), at));
    let seen = SEEN.get_or_init(Mutex::default);
    if let Some(named) = key.as_ref().and_then(|key| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .copied()
    }) {
        return Ok(named);
    }
    let named = file_holds(&exe, AUTH_PATH_MARK.as_bytes())?;
    if let Some(key) = key {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, named);
    }
    Ok(named)
}

/// Whether the regular file at `path` holds `needle`, read [`SCAN_CHUNK`] bytes a step.
fn file_holds(path: &Path, needle: &[u8]) -> std::io::Result<bool> {
    use std::io::Read as _;
    let Some((&first, _)) = needle.split_first() else {
        return Ok(true);
    };
    let mut file = super::footer::open_regular(path)
        .ok_or_else(|| std::io::Error::other("not a regular file"))?;
    let keep = needle.len() - 1;
    let mut buf = vec![0_u8; SCAN_CHUNK + keep];
    let mut held = 0;
    loop {
        let n = match file.read(&mut buf[held..]) {
            Ok(0) => return Ok(false),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let end = held + n;
        if buf[..end]
            .windows(needle.len())
            .any(|w| w[0] == first && w == needle)
        {
            return Ok(true);
        }
        let tail = keep.min(end);
        buf.copy_within(end - tail..end, 0);
        held = tail;
    }
}

/// A codex-alab package laid out at `root` for another crate's test: `codex-package.json`, the
/// [`ALAB_BUILD_JSON`] marker and `bin/codex`, whose bytes spell [`AUTH_PATH_MARK`] where
/// `reads_auth_path` (so [`auth_path_unread`] passes it) and elsewhere only [`AUTH_PATH_ENV`]'s
/// bare name, as a codex-alab from 355ba0a940 to before cd79a420c does. It returns the binary's
/// path; nothing runs it.
///
/// # Panics
/// A fixture file cannot be written.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn lay_package(root: &Path, reads_auth_path: bool) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).expect("package bin");
    std::fs::write(
        root.join(PACKAGE_JSON),
        r#"{"layoutVersion": 1, "version": "0.1.0-dev.1+codex.0.164.0.g000000000", "entrypoint": "bin/codex"}"#,
    )
    .expect("package manifest");
    std::fs::write(root.join(ALAB_BUILD_JSON), "{}").expect("package marker");
    let exe = root.join("bin/codex");
    let named = if reads_auth_path {
        AUTH_PATH_MARK
    } else {
        AUTH_PATH_ENV
    };
    let bytes = [
        b"\xcf\xfa\xed\xfe stand-in: CODEX_HOME ",
        named.as_bytes(),
        b"\0",
    ]
    .concat();
    std::fs::write(&exe, bytes).expect("package binary");
    exe
}

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

/// Whether `tok` is a one-shot help/version flag or a `codex_arg0` internal helper flag.
fn is_one_shot_flag(tok: &str) -> bool {
    let name = if let Some((n, _)) = tok.split_once('=')
        && n.starts_with("--")
    {
        n
    } else if !tok.starts_with("--") && tok.len() > 2 && matches!(tok.get(..2), Some("-h" | "-V")) {
        &tok[..2]
    } else {
        tok
    };
    matches!(
        name,
        "-h" | "--help"
            | "-V"
            | "--version"
            | "--codex-run-as-process-setup"
            | "--codex-run-as-apply-patch"
            | "--codex-run-as-arg0-exec-helper"
            | "--codex-run-as-fs-helper"
            | "--run-as-windows-sandbox"
            | "--__codex-windows-mxc"
    )
}

/// Whether `argv` is an interactive Codex TUI's.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    if arg0_helper_name(argv).is_some() {
        return false;
    }
    let mut subcommand: Option<&str> = None;
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            return true;
        }
        if !maybe_option(tok) {
            if subcommand.is_none() {
                if SUBCOMMANDS.contains(&tok) {
                    if tok != "resume" && tok != "fork" {
                        return false;
                    }
                    subcommand = Some(tok);
                } else {
                    subcommand = Some("");
                }
            }
            continue;
        }
        if is_one_shot_flag(tok) {
            return false;
        }
        skip_flag_values(argv, &mut i, tok);
    }
    true
}

/// The thread a TUI's argv resumes: the first positional after `resume` when it is a thread id.
#[must_use]
pub fn resumed_thread(argv: &[String]) -> Option<String> {
    if !is_tui_argv(argv) {
        return None;
    }
    let mut i = 1;
    let mut resuming = false;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            return resuming
                .then(|| argv.get(i).map(String::as_str))
                .flatten()
                .filter(|t| is_thread_id(t))
                .map(str::to_string);
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
