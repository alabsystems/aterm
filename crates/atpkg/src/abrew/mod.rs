// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! abrew — aterm's Homebrew, an atpkg extension
//! (`docs/DESIGN-abrew-2026-10-19.md`).
//!
//! The intended product is native Homebrew compatibility. The implemented
//! subset admits relocation-free bottles and simple cask artifacts; source
//! recipes, services and unsupported arguments delegate before
//! effects. `brew --real …`, `ABREW_REAL=1` and `abrew off` select real brew.
//! One argument-admission function preserves this boundary for every entry.

mod commands;
use commands::*;

pub mod api;
pub mod bundle;
pub mod cask;
pub mod census;
pub mod fetch;
pub mod formula;
pub mod install;
pub mod machine;
pub mod prefix;
pub mod stub;
pub mod tap;

use std::ffi::OsString;
use std::process::ExitCode;

use formula::Formula;
use install::InstallError;
use prefix::Prefix;

/// One package, version or tap path component. Paths and shell syntax are
/// not names; callers validate each component before joining it to a prefix.
pub(crate) fn valid_component(name: &str) -> bool {
    let mut parts = std::path::Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && parts.next().is_none()
        && !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+@-".contains(&c))
}

/// A package version as a single directory name. Brew versions use
/// commas (`4.2.1,20260531.1`), which are path-safe everywhere abrew
/// runs; separators, colons and leading dots still refuse.
pub(crate) fn valid_cask_version(version: &str) -> bool {
    !version.is_empty()
        && !version.starts_with('.')
        && version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+@-,".contains(&c))
}

pub(crate) fn valid_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    matches!(parts.len(), 1 | 3) && parts.into_iter().all(valid_component)
}

/// `abrew` / `atpkg-abrew` argv0 and the dev binary.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    // Escapes are decided on the raw argv so a delegated command reaches
    // real brew byte-unchanged (non-UTF8 arguments included).
    if escape_requested_os(&args) {
        let rest: Vec<&OsString> = if args
            .first()
            .is_some_and(|a| a.as_os_str() == stub::REAL_FLAG)
        {
            args.iter().skip(1).collect()
        } else {
            args.iter().collect()
        };
        return passthrough_os(&Prefix::resolve(), &rest, None);
    }
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    run(&argv, false)
}

/// Whether the raw argv asks for real brew: a leading `--real` (the only
/// position either layer strips), `ABREW_REAL`, or a `--no-reroute`
/// session (`__ATERM_REROUTE_PASSTHROUGH`).
fn escape_requested_os(args: &[OsString]) -> bool {
    let first = args
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    if first == "on" {
        return false;
    }
    first == stub::REAL_FLAG
        || stub::engaged(std::env::var(stub::REAL_ENV).ok().as_deref())
        || stub::engaged(std::env::var(reroute_passthrough_env()).ok().as_deref())
}

const fn reroute_passthrough_env() -> &'static str {
    "__ATERM_REROUTE_PASSTHROUGH"
}

/// `aterm pkg abrew …` (the atpkg verb).
pub fn cli_entry(args: &[String]) -> ExitCode {
    run(args, false)
}

/// `atpkg __abrew …` — what the `brew` stub execs. Announces once
/// (unless `[reroute] announce = false`), then runs the engine.
pub fn rerouted_entry(args: &[String]) -> ExitCode {
    run(args, true)
}

/// The engine. `via_reroute` is true when the caller typed `brew`.
pub fn run(args: &[String], via_reroute: bool) -> ExitCode {
    if cfg!(windows) {
        eprintln!("abrew: Homebrew is macOS/Linux software; abrew does nothing on Windows");
        return ExitCode::from(1);
    }
    let prefix = Prefix::resolve();
    if let Some(w) = prefix::prefix_env_warning() {
        eprintln!("abrew: warning: {w}");
    }
    // Escapes (design §1) win before anything else, except turning
    // abrew back on.
    let first = args.first().map(String::as_str).unwrap_or("");
    if first == stub::REAL_FLAG {
        return passthrough(&prefix, args.get(1..).unwrap_or(&[]), None);
    }
    if stub::engaged(std::env::var(stub::REAL_ENV).ok().as_deref()) && first != "on" {
        return passthrough(&prefix, args, None);
    }
    if stub::engaged(std::env::var(reroute_passthrough_env()).ok().as_deref()) && first != "on" {
        return passthrough(&prefix, args, Some("this session is --no-reroute"));
    }
    if !crate::config::cached_abrew().enabled() && !matches!(first, "on" | "status") {
        return passthrough(&prefix, args, Some("[abrew] enabled=false in aterm.toml"));
    }
    if prefix.is_disabled() && !matches!(first, "on" | "status") {
        return passthrough(&prefix, args, Some("abrew is off (abrew on re-enables)"));
    }
    // Only native work needs an abrew-owned tree. Explicit and disabled
    // delegation above must remain usable even for a refused native prefix.
    if prefix::overlaps_real_brew_prefix(&prefix.root) {
        return failed(&format!(
            "abrew: refusing ABREW_PREFIX {}: it overlaps a real Homebrew prefix or its physical location is uncertain; abrew and real brew must own disjoint trees",
            prefix.root.display()
        ));
    }
    if via_reroute && crate::config::cached_reroute().announce() {
        eprintln!("brew → abrew (aterm) — `brew --real …` runs real Homebrew");
    }
    dispatch(&prefix, args, via_reroute)
}

/// Run the real brew (bundled first) with `args`, stdio inherited.
/// 127 when there is no real brew to run.
fn passthrough(prefix: &Prefix, args: &[String], notice: Option<&str>) -> ExitCode {
    let Some(brew) = stub::resolve_real_brew(prefix) else {
        eprintln!(
            "abrew: no real Homebrew found{}\n  lay the bundled one with: abrew bundle",
            notice.map_or(String::new(), |n| format!(" ({n})"))
        );
        return ExitCode::from(127);
    };
    if let Some(n) = notice {
        eprintln!("abrew: {n}; running real brew ({})", brew.display());
    }
    let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let refs: Vec<&OsString> = os_args.iter().collect();
    run_real_brew(&brew, &refs)
}

/// Run an already-resolved real brew with byte-faithful argv.
fn run_real_brew(brew: &std::path::Path, args: &[&OsString]) -> ExitCode {
    let status = aterm_uds::spawnfd::command(brew)
        .args(args.iter().copied())
        .env(stub::REROUTED_ENV, "1")
        .status();
    match status {
        Ok(s) => ExitCode::from(u8::try_from(s.code().unwrap_or(2)).unwrap_or(2)),
        Err(e) => {
            eprintln!("abrew: cannot run {}: {e}", brew.display());
            ExitCode::from(127)
        }
    }
}

/// [`passthrough`] for raw OS argv (the argv0 escape path).
fn passthrough_os(prefix: &Prefix, args: &[&OsString], notice: Option<&str>) -> ExitCode {
    let Some(brew) = stub::resolve_real_brew(prefix) else {
        eprintln!(
            "abrew: no real Homebrew found{}\n  lay the bundled one with: abrew bundle",
            notice.map_or(String::new(), |n| format!(" ({n})"))
        );
        return ExitCode::from(127);
    };
    if let Some(n) = notice {
        eprintln!("abrew: {n}; running real brew ({})", brew.display());
    }
    run_real_brew(&brew, args)
}

/// A command abrew does not implement natively (decision 3A).
fn not_native(prefix: &Prefix, args: &[String], cmd: &str) -> ExitCode {
    passthrough(prefix, args, Some(&format!("'{cmd}' is not native yet")))
}

fn failed(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

/// Brew boolean env config is presence-based (`boolean: :set`).
pub(super) fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// The success badge brew prints after an install: suppressed by
/// `HOMEBREW_NO_EMOJI`, overridable via `HOMEBREW_INSTALL_BADGE`.
fn badge_from(no_emoji: bool, badge: Option<String>) -> Option<String> {
    if no_emoji {
        None
    } else {
        Some(
            badge
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "🍺".to_string()),
        )
    }
}

pub(super) fn install_badge() -> Option<String> {
    badge_from(
        env_set("HOMEBREW_NO_EMOJI"),
        std::env::var("HOMEBREW_INSTALL_BADGE").ok(),
    )
}

/// `HOMEBREW_NO_INSTALL_CLEANUP`: skip removing superseded versions
/// after install/upgrade (explicit `cleanup` still works).
pub(super) fn no_install_cleanup() -> bool {
    env_set("HOMEBREW_NO_INSTALL_CLEANUP")
}

/// `HOMEBREW_NO_ENV_HINTS`: suppress environment-behaviour hints.
pub(super) fn no_env_hints() -> bool {
    env_set("HOMEBREW_NO_ENV_HINTS")
}

fn from_install(prefix: &Prefix, args: &[String], r: Result<(), InstallError>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(InstallError::Failed(m)) => failed(&m),
        Err(InstallError::PassThrough(reason)) => passthrough(prefix, args, Some(&reason)),
    }
}

/// A later fallback must not replay possibly retained native work in another prefix.
fn after_native_progress(error: InstallError, completed: bool) -> InstallError {
    match error {
        InstallError::PassThrough(reason) if completed => InstallError::Failed(format!(
            "{reason}; an earlier native attempt may have retained changes; refusing whole-command delegation"
        )),
        error => error,
    }
}

/// A failed native attempt may already have retained an upgraded dependency.
/// Keep a direct PassThrough eligible for the existing initial fallback;
/// an ordinary failure conservatively forbids later whole-argv replay too.
const fn native_attempt_may_have_progress<T>(result: &Result<T, InstallError>) -> bool {
    !matches!(result, Err(InstallError::PassThrough(_)))
}

/// Carry one actual native attempt through a dependency walk. A failed
/// attempt can retain work too; a later fallback must honor that boundary.
fn after_native_attempt<T>(
    possible_progress: &mut bool,
    result: Result<T, InstallError>,
) -> Result<T, InstallError> {
    *possible_progress |= native_attempt_may_have_progress(&result);
    result.map_err(|error| after_native_progress(error, *possible_progress))
}

/// True for commands that mutate the prefix (they take the lock).
fn mutates(cmd: &str) -> bool {
    matches!(
        cmd,
        "install"
            | "on"
            | "off"
            | "ln"
            | "uninstall"
            | "remove"
            | "rm"
            | "reinstall"
            | "upgrade"
            | "update"
            | "cleanup"
            | "autoremove"
            | "pin"
            | "unpin"
            | "link"
            | "unlink"
            | "switch"
            | "tap"
            | "untap"
            | "bundle"
            | "fetch"
    )
}

/// brew spells the kind switches two ways (`--formula`/`--formulae`,
/// `--cask`/`--casks`) on every kind-taking command. Admission and
/// handlers read only the singular tokens, so plural spellings are
/// canonicalized in the `rest` copy they see. The original `args`
/// keep the user's spelling for passthrough to real brew. Exact
/// tokens only: these switches take no `=value`, and a leading-dash
/// token was never an operand.
fn canonical_kind_switches(rest: &[String]) -> Vec<String> {
    rest.iter()
        .map(|s| match s.as_str() {
            "--formulae" => "--formula".to_string(),
            "--casks" => "--cask".to_string(),
            _ => s.clone(),
        })
        .collect()
}

/// Admit only argument forms whose meaning is implemented. Unsupported forms
/// reach real brew unchanged before locking or creating anything in the prefix.
fn native_args(
    cmd: &str,
    rest: &[String],
) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    let unsupported = || InstallError::PassThrough(format!("'{cmd}' arguments are not native yet"));
    let unknown = || {
        // Real brew verbs abrew delegates whole (developer commands,
        // zap, the shell/ruby escapes) are KNOWN, just not native:
        // the message must not call them unknown commands. They stay
        // out of COMMANDS, which is the natively-dispatched roster
        // shell completions are generated from.
        const DELEGATED_KNOWN: &[&str] = &[
            "audit",
            "bottle",
            "bump",
            "bump-cask-pr",
            "bump-formula-pr",
            "bump-revision",
            "create",
            "edit",
            "extract",
            "irb",
            "livecheck",
            "postinstall",
            "ruby",
            "sh",
            "style",
            "test",
            "unpack",
            "zap",
        ];
        if COMMANDS.contains(&cmd) || DELEGATED_KNOWN.contains(&cmd) {
            unsupported()
        } else {
            InstallError::PassThrough(format!("'{cmd}' is not a command abrew knows natively"))
        }
    };
    if cmd == "bundle" {
        return bundle_args(rest);
    }
    let (flags, min, max, names): (&[&str], usize, usize, bool) = match cmd {
        "install" | "reinstall" => (
            &[
                "--formula",
                "--cask",
                "--ignore-dependencies",
                "--force",
                "-f",
                "--dry-run",
                "-n",
                "--quiet",
                "-q",
                "--verbose",
                "-v",
                "--debug",
                "-d",
                "--no-quarantine",
            ],
            1,
            usize::MAX,
            true,
        ),
        // Bare `upgrade` (all outdated formulae) is brew's common form
        // and stays native; it must never delegate to a real brew that
        // would upgrade a different prefix.
        "upgrade" => (
            &[
                "--formula",
                "--cask",
                "--ignore-dependencies",
                "--force",
                "-f",
                "--dry-run",
                "-n",
                "--quiet",
                "-q",
                "--greedy",
            ],
            0,
            usize::MAX,
            true,
        ),
        "uninstall" | "remove" | "rm" => (
            &[
                "--formula",
                "--cask",
                "--ignore-dependencies",
                "--dry-run",
                "-n",
                "--force",
                "-f",
            ],
            1,
            usize::MAX,
            true,
        ),
        "search" | "-S" => (&["--desc", "--formula", "--cask"], 1, usize::MAX, false),
        "info" => (
            &[
                "--cask",
                "--formula",
                "--json",
                "--json=v1",
                "--json=v2",
                "--installed",
                "--analytics",
                "--days",
                "--category",
            ],
            0,
            usize::MAX,
            true,
        ),
        // Search-mode operands are free text, not package names;
        // named-mode operands are validated by the desc rule below.
        "desc" => (
            &[
                "--formula",
                "--cask",
                "--search",
                "-s",
                "--name",
                "-n",
                "--description",
                "-d",
            ],
            1,
            usize::MAX,
            false,
        ),
        "options" => (&["--compact", "--installed"], 0, usize::MAX, true),
        "linkage" => (&[], 1, usize::MAX, true),
        // Native uses scans the installed census, so --installed is
        // exactly its semantics; --recursive still delegates.
        "uses" | "dependents" => (&["--installed"], 1, usize::MAX, true),
        "home" | "homepage" => (&[], 0, usize::MAX, true),
        "cat" => (&["--cask", "--formula"], 1, 1, true),
        "outdated" => (
            &[
                "--formula",
                "--cask",
                "--greedy",
                "--verbose",
                "-v",
                "--quiet",
                "-q",
                "--json",
                "--json=v1",
                "--json=v2",
            ],
            0,
            usize::MAX,
            true,
        ),
        "missing" => (&[], 0, usize::MAX, true),
        "cleanup" => (&["--dry-run", "-n", "-s", "--scrub"], 0, usize::MAX, true),
        "autoremove" => (&["--dry-run", "-n"], 0, 1, true),
        "pin" | "unpin" => (&[], 1, 1, true),
        "link" | "ln" | "unlink" => (&["--dry-run", "-n"], 1, 1, true),
        "switch" => (&[], 2, 2, true),
        "list" | "ls" => (
            &[
                "--formula",
                "--cask",
                "--versions",
                "--pinned",
                "--full-name",
                "--long",
                "-l",
                "-1",
                "--multiple",
                "--json",
            ],
            0,
            usize::MAX,
            true,
        ),
        "fetch" => (&["--cask", "--formula", "--deps"], 1, usize::MAX, true),
        // brew spells the direct switch four ways (-1, --direct,
        // --declared, --1); all four are the one admitted meaning.
        "deps" => (
            &[
                "--tree",
                "--installed",
                "--direct",
                "--1",
                "-1",
                "--declared",
                "--for-each",
                "--union",
                "--topological",
                "-n",
            ],
            0,
            usize::MAX,
            true,
        ),
        "log" => (&[], 0, 1, true),
        "tap" => (&["--list-official"], 0, 2, false),
        "untap" => (&[], 1, 1, false),
        "tap-info" => (&[], 0, usize::MAX, false),
        "help" | "--help" | "-h" => (&[], 0, 1, false),
        "completions" => (&[], 1, 1, false),
        "--prefix" | "--cellar" | "--caskroom" => (&[], 0, 1, true),
        // The named forms answer for this prefix's own cache/taps;
        // brew's platform spellings (--os/--arch/-s/--HEAD and kin)
        // are not in the flag set and keep delegating.
        "--cache" => (&["--formula", "--cask"], 0, usize::MAX, true),
        "--repository" | "--repo" => (&[], 0, 1, false),
        "--taps" => (&[], 0, 0, false),
        "analytics" => (&[], 0, 1, false),
        "doctor" | "dr" => (&["--quiet", "-q"], 0, 0, false),
        "shellenv" => (&[], 0, 1, false),
        "on" | "off" | "status" | "--version" | "-v" | "--env" | "env" | "formulae" | "casks"
        | "config" | "developer" | "update" => (&[], 0, 0, false),
        "commands" => (&["--quiet", "--include-aliases"], 0, 0, false),
        "leaves" => (
            &["--installed-on-request", "--installed-as-dependency"],
            0,
            0,
            false,
        ),
        _ => return Err(unknown()),
    };
    let flag_ok = |arg: &str| {
        flags.contains(&arg)
            || (cmd == "info"
                && (arg
                    .strip_prefix("--days=")
                    .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    || arg.strip_prefix("--category=").is_some_and(valid_component)))
            || (cmd == "cleanup"
                && arg.strip_prefix("--prune=").is_some_and(|v| {
                    v == "all" || (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                }))
            || (cmd == "missing"
                && arg.strip_prefix("--hide=").is_some_and(|v| {
                    !v.is_empty()
                        && v.split(',').all(|part| {
                            // Short or tap-qualified names only: every
                            // segment a safe component (no `..` travel).
                            let part = part.trim();
                            !part.is_empty()
                                && part.split('/').all(valid_component)
                                && matches!(part.matches('/').count(), 0 | 2)
                        })
                }))
    };
    if rest.iter().any(|arg| arg.starts_with('-') && !flag_ok(arg)) {
        return Err(unsupported());
    }
    if cmd == "cleanup" {
        // Recognized numeric ages must fit before layout/locking or cleanup;
        // they are a native refusal, never a reason to run a different prefix.
        cleanup_max_age(rest).map_err(|error| InstallError::Failed(error.to_string()))?;
    }
    let ops = if cmd == "info" {
        operands_without_flag_values(rest, &["--days", "--category"])
    } else {
        operands(rest)
    };
    if cmd == "completions"
        && !ops
            .first()
            .is_some_and(|shell| matches!(shell.as_str(), "bash" | "zsh" | "fish"))
    {
        return Err(unsupported());
    }
    // Keep an explicit tap's kind through admission. Most handlers shorten
    // names and infer their kind from flags or the installed census; a cask
    // qualifier must never become permission to mutate a same-named formula,
    // or vice versa. Without a matching flag, delegate before metadata or
    // locking. The core --prefix query is already formula-only.
    let want_formula = has_flag(rest, "--formula", "");
    let want_cask = has_flag(rest, "--cask", "");
    let valid_package_name = |s: &String| {
        valid_component(s)
            || ((want_formula || matches!(cmd, "--prefix" | "--cellar"))
                && s.strip_prefix("homebrew/core/")
                    .is_some_and(valid_component))
            || ((want_cask || cmd == "--caskroom")
                && s.strip_prefix("homebrew/cask/")
                    .is_some_and(valid_component))
    };
    if !(min..=max).contains(&ops.len()) || (names && !ops.iter().all(valid_package_name)) {
        return Err(unsupported());
    }
    if has_flag(rest, "--cask", "") && has_flag(rest, "--formula", "") {
        return Err(unsupported());
    }
    if matches!(cmd, "install" | "reinstall" | "upgrade")
        && !has_flag(rest, "--formula", "")
        && (has_flag(rest, "--quiet", "-q") || has_flag(rest, "--ignore-dependencies", ""))
    {
        return Err(unsupported());
    }
    if matches!(cmd, "list" | "ls") && !ops.is_empty() && has_flag(rest, "--pinned", "") {
        return Err(unsupported());
    }
    // --multiple conflicts with --cask/--pinned in brew, and brew's
    // --full-name branch would swallow it, so those delegate.
    if matches!(cmd, "list" | "ls") {
        if has_flag(rest, "--multiple", "")
            && (has_flag(rest, "--cask", "")
                || has_flag(rest, "--pinned", "")
                || has_flag(rest, "--full-name", ""))
        {
            return Err(unsupported());
        }
        // brew's --json list is the fast path only: --versions,
        // no names, no other modifiers. Everything else delegates.
        if has_flag(rest, "--json", "")
            && (!has_flag(rest, "--versions", "")
                || !ops.is_empty()
                || has_flag(rest, "--multiple", "")
                || has_flag(rest, "--pinned", "")
                || has_flag(rest, "--full-name", "")
                || has_flag(rest, "--long", "-l")
                || rest.iter().any(|s| s == "-1"))
        {
            return Err(unsupported());
        }
    }
    if cmd == "autoremove" && !ops.is_empty() {
        return Err(unsupported());
    }
    // With no root, only the installed inventory (plain, --direct or
    // --tree over every installed formula) is implemented.
    if cmd == "deps" && !has_flag(rest, "--installed", "") && ops.is_empty() {
        return Err(unsupported());
    }
    // info --analytics: named packages only here (the no-names
    // global form reads a separate analytics endpoint); --days and
    // --category depend on it in brew, and it does not combine
    // with --json/--installed.
    if cmd == "info" {
        let analytics = has_flag(rest, "--analytics", "");
        let json = rest
            .iter()
            .any(|s| matches!(s.as_str(), "--json" | "--json=v1" | "--json=v2"));
        let days = flag_value(rest, "--days").map_err(|()| unsupported())?;
        let category = flag_value(rest, "--category").map_err(|()| unsupported())?;
        if days
            .as_deref()
            .is_some_and(|v| !matches!(v, "30" | "90" | "365"))
            || category
                .as_deref()
                .is_some_and(|v| !matches!(v, "install" | "install-on-request" | "build-error"))
        {
            return Err(unsupported());
        }
        if (days.is_some() || category.is_some()) && !analytics {
            return Err(unsupported());
        }
        if analytics && (json || has_flag(rest, "--installed", "") || ops.is_empty()) {
            return Err(unsupported());
        }
    }
    // desc's three search modes conflict in brew (it errors there,
    // so combined spellings delegate), and named mode keeps the
    // package-name validation the row's `names` flag would have done.
    let desc_modes = if cmd == "desc" {
        [
            has_flag(rest, "--search", "-s"),
            has_flag(rest, "--name", "-n"),
            has_flag(rest, "--description", "-d"),
        ]
        .iter()
        .filter(|b| **b)
        .count()
    } else {
        0
    };
    if desc_modes > 1 || (cmd == "desc" && desc_modes == 0 && !ops.iter().all(valid_package_name)) {
        return Err(unsupported());
    }
    // brew's `/regex/` search form runs natively when the bounded
    // first-party engine accepts the pattern. Constructs it refuses
    // keep delegating: substring-matching the literal pattern would
    // print a wrong "no matches", and search reads the upstream
    // catalog, not prefix state, so delegating those spellings is
    // safe. Detection mirrors brew: the space-joined query.
    if (matches!(cmd, "search" | "-S") || desc_modes == 1)
        && let Some(pattern) = search_regex_pattern(&ops.join(" "))
        && compile_search_regex(pattern).is_err()
    {
        return Err(unsupported());
    }
    if cmd == "switch" && !ops.get(1).is_some_and(|s| valid_component(s)) {
        return Err(unsupported());
    }
    match cmd {
        "tap"
            if ops
                .first()
                .is_some_and(|s| tap::parse_tap_name(s).is_none()) =>
        {
            return Err(unsupported());
        }
        "untap" | "tap-info" | "--repository" | "--repo"
            if !ops.iter().all(|s| tap::parse_tap_name(s).is_some()) =>
        {
            return Err(unsupported());
        }
        "analytics"
            if ops
                .first()
                .is_some_and(|s| !matches!(s.as_str(), "off" | "state")) =>
        {
            return Err(unsupported());
        }
        _ => {}
    }
    Ok(None)
}

fn bundle_args(rest: &[String]) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let unsupported = || {
        InstallError::PassThrough("bundle arguments or Brewfile rows are not native yet".to_owned())
    };
    match rest.first().map(String::as_str) {
        Some("status" | "remove") if rest.len() == 1 => Ok(None),
        Some("dump") => {
            // dump [--file <path>|--file=<path>] [--force]
            let mut file = false;
            let mut force = false;
            let mut it = rest[1..].iter();
            while let Some(arg) = it.next() {
                if arg == "--file" && !file {
                    file = true;
                    if !it.next().is_some_and(|s| !s.is_empty()) {
                        return Err(unsupported());
                    }
                } else if arg.strip_prefix("--file=").is_some_and(|s| !s.is_empty()) && !file {
                    file = true;
                } else if matches!(arg.as_str(), "--force" | "-f") && !force {
                    force = true;
                } else {
                    return Err(unsupported());
                }
            }
            Ok(None)
        }
        Some("check" | "install") => {
            let mut file = None;
            let mut it = rest[1..].iter();
            while let Some(arg) = it.next() {
                if arg == "--file" && file.is_none() {
                    file = Some(std::path::PathBuf::from(
                        it.next()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(unsupported)?,
                    ));
                } else if let Some(path) = arg.strip_prefix("--file=").filter(|s| !s.is_empty())
                    && file.is_none()
                {
                    file = Some(std::path::PathBuf::from(path));
                } else {
                    return Err(unsupported());
                }
            }
            let text = bundle::read_brewfile(file.as_deref()).map_err(InstallError::Failed)?;
            let (entries, skipped) = bundle::parse_brewfile(&text);
            if !skipped.is_empty() {
                return Err(unsupported());
            }
            Ok(Some(entries))
        }
        _ => {
            let mut it = rest.iter();
            let (mut version, mut sha, mut force) = (false, false, false);
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--version" if !version => {
                        version = true;
                        if !it.next().is_some_and(|s| valid_component(s)) {
                            return Err(unsupported());
                        }
                    }
                    "--sha256" if !sha => {
                        sha = true;
                        if !it.next().is_some_and(|s| {
                            s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
                        }) {
                            return Err(unsupported());
                        }
                    }
                    "--force" | "-f" if !force => force = true,
                    _ => return Err(unsupported()),
                }
            }
            Ok(None)
        }
    }
}

/// Pre-lock recipe delegation for install-shaped commands: resolve the
/// plan read-only; a PassThrough delegates now, lock-free. Returns the
/// exit code to use, or None to proceed natively under the lock.
fn prelock_delegation(
    prefix: &Prefix,
    api: &api::Api,
    args: &[String],
    rest: &[String],
) -> Option<ExitCode> {
    let want_cask = rest.iter().any(|s| s == "--cask");
    let want_formula = rest.iter().any(|s| s == "--formula");
    let opts = install::InstallOpts {
        ignore_dependencies: rest.iter().any(|s| s == "--ignore-dependencies"),
        dry_run: false,
        force: rest.iter().any(|s| matches!(s.as_str(), "--force" | "-f")),
        quiet: true,
        upgrade: args.first().is_some_and(|a| a == "upgrade"),
        reinstall: args.first().is_some_and(|a| a == "reinstall"),
        as_dependency: false,
    };
    // Every operand is planned: a batch delegates whole (before any
    // effect) if any member would, so a multi-name install can never
    // half-land natively and then replay in another prefix.
    let mut result = Ok(());
    let mut any = false;
    for name in rest.iter().filter(|s| !s.starts_with('-')) {
        any = true;
        result = if want_cask || (!want_formula && api.formula(name).is_err()) {
            match api.cask(name) {
                Ok(c) => cask::preflight(prefix, &c, machine::Machine::this())
                    .and_then(|()| cask::preflight_deps(api, prefix, &c)),
                Err(_) => continue,
            }
        } else {
            install::preflight_plan(api, prefix, name, &opts)
        };
        if result.is_err() {
            break;
        }
    }
    if !any {
        return None;
    }
    match result {
        Ok(()) => None,
        Err(e @ InstallError::PassThrough(_)) => Some(from_install(prefix, args, Err(e))),
        Err(_) => None,
    }
}

fn dispatch(prefix: &Prefix, args_in: &[String], via_reroute: bool) -> ExitCode {
    // Brew's leading global verbosity flags (`brew -v install x`) are
    // no-ops here; drop them when a command follows (a bare `-v` is
    // still the version spelling handled below).
    let stripped: Vec<String>;
    let args: &[String] = if args_in.len() > 1
        && matches!(args_in[0].as_str(), "--verbose" | "-v" | "--debug" | "-d")
        && !args_in[1].starts_with('-')
    {
        stripped = args_in[1..].to_vec();
        &stripped
    } else {
        args_in
    };
    // Typed `brew bundle` means Brewfile-install in brew, not abrew's
    // lay-the-fallback spelling: a rerouted bare `bundle` delegates.
    if via_reroute && args.len() == 1 && args.first().is_some_and(|a| a == "bundle") {
        return passthrough(
            prefix,
            args,
            Some("bare `brew bundle` is Homebrew's Brewfile install"),
        );
    }
    // Typed `brew --version` feeds Homebrew version parsers; abrew's
    // own version spelling would break them, so rerouted --version delegates.
    if via_reroute
        && args
            .first()
            .is_some_and(|a| matches!(a.as_str(), "--version" | "-v"))
    {
        return passthrough(prefix, args, None);
    }
    // Typed `brew --env` asks for Homebrew's BUILD environment
    // (CC, MAKEFLAGS, CMAKE paths under its prefix) to build
    // against real brew; abrew's two ABREW_ lines would silently
    // give a build the wrong environment. Direct `abrew --env`
    // keeps the native answer.
    if via_reroute
        && args
            .first()
            .is_some_and(|a| matches!(a.as_str(), "--env" | "env"))
    {
        return passthrough(prefix, args, None);
    }
    if args.is_empty() {
        print!("{HELP_MAIN}");
        return ExitCode::SUCCESS;
    }
    let (cmd, rest): (&str, &[String]) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("", &[]),
    };
    // Handlers see the same canonical kind switches admission saw;
    // `args` above stays byte-identical for passthrough.
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    let prepared = match native_args(cmd, rest) {
        Ok(prepared) => prepared,
        Err(error) => return from_install(prefix, args, Err(error)),
    };
    // Global brew queries spelled as flags.
    match cmd {
        "--version" | "-v" if rest.is_empty() => {
            println!("abrew {} (aterm)", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        "--help" | "-h" | "help" => {
            print!(
                "{}",
                help_for(rest.first().map(String::as_str).unwrap_or(""))
            );
            return ExitCode::SUCCESS;
        }
        "--prefix" | "--cellar" | "--caskroom" => {
            if let Some(name) = rest.first() {
                let api0 = api::Api::new(prefix.clone());
                return commands::cmd_prefix_query(prefix, &api0, cmd, name);
            }
            let bare = match cmd {
                "--prefix" => prefix.root.clone(),
                "--cellar" => prefix.cellar(),
                _ => prefix.caskroom(),
            };
            println!("{}", bare.display());
            return ExitCode::SUCCESS;
        }
        "--cache" => {
            if operands(rest).is_empty() {
                println!("{}", prefix.cache().display());
                return ExitCode::SUCCESS;
            }
            let api0 = api::Api::new(prefix.clone());
            return commands::cmd_cache_query(prefix, &api0, args, rest);
        }
        "--taps" => {
            println!("{}", prefix.taps_dir().display());
            return ExitCode::SUCCESS;
        }
        "--repository" | "--repo" => {
            // Admission validated the operand as a tap name. Like
            // brew, the path prints whether or not the tap stands.
            if let Some(name) = rest.first()
                && let Some((user, repo)) = tap::parse_tap_name(name)
            {
                println!(
                    "{}",
                    prefix
                        .taps_dir()
                        .join(user)
                        .join(format!("homebrew-{repo}"))
                        .display()
                );
                return ExitCode::SUCCESS;
            }
            println!("{}", prefix.homebrew_bundle().display());
            return ExitCode::SUCCESS;
        }
        "--env" | "env" => {
            println!("ABREW_PREFIX={}", prefix.root.display());
            println!("ABREW_CELLAR={}", prefix.cellar().display());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let dry_run = rest
        .iter()
        .any(|s| matches!(s.as_str(), "--dry-run" | "-n"));
    let read_only = (cmd == "bundle"
        && rest
            .first()
            .is_some_and(|s| matches!(s.as_str(), "check" | "status" | "dump")))
        || (cmd == "tap" && rest.is_empty())
        || dry_run;
    // Recipe delegation happens BEFORE the writer lock (and its prefix
    // skeleton): real brew must never run while abrew holds its own lock,
    // and a delegated dry-run creates nothing.
    if matches!(cmd, "install" | "upgrade" | "reinstall" | "fetch") && !dry_run {
        let api0 = api::Api::new(prefix.clone());
        if let Some(code) = prelock_delegation(prefix, &api0, args, rest) {
            return code;
        }
    }
    // A multi-entry `bundle install` is a batch like multi-name
    // install: plan every not-yet-installed entry read-only, so a
    // non-native member delegates the whole Brewfile before any
    // entry lands in this prefix.
    if cmd == "bundle"
        && rest.first().is_some_and(|s| s == "install")
        && let Some(entries) = &prepared
    {
        let api0 = api::Api::new(prefix.clone());
        let opts = install::InstallOpts {
            quiet: true,
            ..install::InstallOpts::default()
        };
        let mut plan = Ok(());
        for entry in entries {
            plan = match entry {
                bundle::BrewfileEntry::Tap(_) => Ok(()),
                bundle::BrewfileEntry::Brew(n) => {
                    if prefix
                        .installed_one(prefix::Kind::Formula, Formula::short_name(n))
                        .is_some()
                    {
                        Ok(())
                    } else {
                        install::preflight_plan(&api0, prefix, n, &opts)
                    }
                }
                bundle::BrewfileEntry::Cask(n) => {
                    if prefix.installed_one(prefix::Kind::Cask, n).is_some() {
                        Ok(())
                    } else {
                        match api0.cask(n) {
                            Ok(c) => cask::preflight(prefix, &c, machine::Machine::this())
                                .and_then(|()| cask::preflight_deps(&api0, prefix, &c)),
                            Err(e) => Err(InstallError::Failed(e)),
                        }
                    }
                }
            };
            if plan.is_err() {
                break;
            }
        }
        if let Err(e @ InstallError::PassThrough(_)) = plan {
            return from_install(prefix, args, Err(e));
        }
    }
    let _lock = if mutates(cmd) && !read_only {
        match prefix::Lock::acquire(prefix) {
            Ok(l) => Some(l),
            Err(e) => return failed(&e),
        }
    } else {
        None
    };
    if _lock.is_some() {
        // Recover only an owned aside's payload; anonymous scratch stays.
        prefix.restore_stale_asides();
    }
    let api = if dry_run {
        api::Api::read_only(prefix.clone())
    } else {
        api::Api::new(prefix.clone())
    };
    match cmd {
        // abrew's own switches
        "on" => cmd_on(prefix),
        "off" => cmd_off(prefix),
        "status" => cmd_status(prefix),
        // discovery
        "search" | "-S" => cmd_search(&api, rest),
        "info" => cmd_info(prefix, &api, args, rest),
        "desc" => cmd_desc(&api, rest),
        "home" | "homepage" => cmd_home(&api, rest),
        "cat" => cmd_cat(prefix, rest),
        "formulae" => list_index(&api, false),
        "casks" => list_index(&api, true),
        "commands" => {
            for c in command_names(has_flag(rest, "--include-aliases", "")) {
                println!("{c}");
            }
            ExitCode::SUCCESS
        }
        "completions" => {
            print!("{}", completions(rest.first().map_or("", String::as_str)));
            ExitCode::SUCCESS
        }
        "config" => cmd_config(prefix),
        "shellenv" => cmd_shellenv(prefix, rest),
        "doctor" | "dr" => cmd_doctor(prefix, rest),
        "analytics" => {
            println!("abrew: analytics are always off; abrew records nothing");
            ExitCode::SUCCESS
        }
        "developer" => {
            println!("Developer mode: off (abrew has no developer mode)");
            ExitCode::SUCCESS
        }
        // lifecycle
        "install" => cmd_install(prefix, &api, args, rest),
        "uninstall" | "remove" | "rm" => cmd_uninstall(prefix, &api, args, rest),
        "reinstall" => cmd_reinstall(prefix, &api, args, rest),
        "upgrade" => cmd_upgrade(prefix, &api, args, rest),
        "outdated" => cmd_outdated(prefix, &api, rest),
        "update" => cmd_update(prefix, &api),
        "cleanup" => cmd_cleanup(prefix, &api, rest),
        "autoremove" => cmd_autoremove(prefix, &api, rest),
        "pin" => each_name(rest, |n| prefix.pin(Formula::short_name(n))),
        "unpin" => cmd_unpin(prefix, &api, rest),
        "link" | "ln" => cmd_link(prefix, &api, args, rest, true),
        "unlink" => cmd_link(prefix, &api, args, rest, false),
        "switch" => cmd_switch(prefix, &api, args, rest),
        "list" | "ls" => cmd_list(prefix, rest),
        "fetch" => cmd_fetch(prefix, &api, args, rest),
        // graphs
        "deps" => cmd_deps(prefix, &api, rest),
        "uses" | "dependents" => cmd_uses(prefix, &api, rest),
        "leaves" => cmd_leaves(prefix, &api, rest),
        "missing" => cmd_missing(prefix, &api, rest),
        "options" => cmd_options(prefix, &api, args, rest),
        "linkage" => cmd_linkage(prefix, rest),
        "log" => cmd_log(prefix, rest),
        // taps and bundle
        "tap" => cmd_tap(prefix, rest),
        "untap" => each_name(rest, |n| tap::remove(prefix, n)),
        "tap-info" => cmd_tap_info(prefix, rest),
        "bundle" => cmd_bundle(prefix, &api, args, rest, prepared),
        _ => not_native(prefix, args, cmd),
    }
}

fn each_name(rest: &[String], f: impl Fn(&str) -> Result<(), String>) -> ExitCode {
    let names: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|s| !s.starts_with('-'))
        .collect();
    if names.is_empty() {
        eprintln!("abrew: no name given");
        return ExitCode::from(2);
    }
    for n in names {
        if let Err(e) = f(n) {
            return failed(&e);
        }
    }
    ExitCode::SUCCESS
}

/// Operands (non-flag args) of a command.
fn operands(rest: &[String]) -> Vec<String> {
    rest.iter()
        .filter(|s| !s.starts_with('-'))
        .cloned()
        .collect()
}

fn has_flag(rest: &[String], long: &str, short: &str) -> bool {
    rest.iter()
        .any(|s| s == long || (!short.is_empty() && s == short))
}

/// Shell completion scripts generated from the one command roster:
/// `abrew completions bash|zsh|fish` (brew's `completions` shape).
fn completions(shell: &str) -> String {
    // Only natively-dispatched verbs complete; delegated spellings
    // (services/postinstall) stay out so completion never promises native.
    let words = COMMANDS
        .iter()
        .filter(|c| !matches!(**c, "services" | "postinstall"))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    match shell {
        "zsh" => format!(
            "#compdef abrew\n_abrew() {{\n  local -a cmds=({words})\n  _arguments '1:command:($cmds)' '*::arg:_normal'\n}}\n_abrew\n"
        ),
        "fish" => format!(
            "complete -c abrew -n '__fish_use_subcommand' -a '{words}'\ncomplete -c brew -n '__fish_use_subcommand' -a '{words}'\n"
        ),
        _ => format!(
            "_abrew() {{\n  COMPREPLY=( $(compgen -W \"{words} --real --version --help\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") )\n}}\ncomplete -F _abrew abrew brew atpkg-abrew\n"
        ),
    }
}

const COMMANDS: &[&str] = &[
    "analytics",
    "autoremove",
    "bundle",
    "casks",
    "cat",
    "cleanup",
    "commands",
    "completions",
    "config",
    "deps",
    "desc",
    "developer",
    "doctor",
    "env",
    "fetch",
    "formulae",
    "help",
    "home",
    "info",
    "install",
    "leaves",
    "link",
    "linkage",
    "list",
    "log",
    "missing",
    "off",
    "on",
    "options",
    "outdated",
    "pin",
    "postinstall",
    "reinstall",
    "search",
    "services",
    "shellenv",
    "status",
    "switch",
    "tap",
    "tap-info",
    "uninstall",
    "unlink",
    "unpin",
    "untap",
    "update",
    "upgrade",
    "uses",
];

/// Alternate spellings dispatch accepts for a roster command, as
/// brew's `commands --include-aliases` lists them: merged into the
/// same sorted listing, never in the plain one.
const COMMAND_ALIASES: &[&str] = &[
    "--repo",
    "-S",
    "-v",
    "dependents",
    "dr",
    "homepage",
    "ln",
    "ls",
    "remove",
    "rm",
];

/// The `commands` listing: the roster, with aliases merged and the
/// whole set sorted when asked (brew's shape in both forms).
fn command_names(include_aliases: bool) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = COMMANDS.to_vec();
    if include_aliases {
        names.extend(COMMAND_ALIASES.iter().copied());
        names.sort_unstable();
    }
    names
}

const HELP_MAIN: &str = "\
abrew — aterm's Homebrew (docs/DESIGN-abrew-2026-10-19.md)

Usage: abrew <command> [args]

Common commands:
  install [--cask] <name>         install a supported bottle or cask
  uninstall [--cask] <name>…      remove installed packages
  reinstall, upgrade, outdated    refresh installed packages
  update                          refresh formula data and taps
  search <text>, info <name>      look packages up
  list [--cask] [--versions] [-l] what is installed
  doctor                          what needs attention
  deps, uses, leaves, missing     the dependency graph
  pin, unpin, link, unlink        manage kegs
  tap, untap, bundle              taps and the Homebrew fallback bundle
  services, postinstall, -s       delegate to real Homebrew
  cleanup, autoremove, fetch      housekeeping
  completions bash|zsh|fish       shell discovery

Real Homebrew, unchanged:  brew --real <command>…   ABREW_REAL=1   abrew off
Native installs are limited to relocation-free bottles and zip/tar casks
whose artifacts and constraints are plain data. Source builds, services and
unsupported flags/recipes run real brew with a notice, using its own prefix.
";

fn help_for(cmd: &str) -> &'static str {
    match cmd {
        "install" => {
            "Usage: abrew install [--formula|--cask] [--ignore-dependencies] [--force|-f] [--dry-run|-n] [--quiet|-q] [--no-quarantine] <name>…\n  Source (-s) and --HEAD forms run real Homebrew. Multi-name installs plan\n  every name first; a batch with a non-native member delegates whole.\n"
        }
        "uninstall" | "remove" => {
            "Usage: abrew uninstall [--formula|--cask] [--ignore-dependencies] [--dry-run|-n] <name>…\n"
        }
        "list" | "ls" => {
            "Usage: abrew list [--formula|--cask] [--versions] [--multiple] [--pinned] [--full-name] [-l|--long] [name…]\n       abrew list --versions --json [--formula|--cask]\n"
        }
        "info" => {
            "Usage: abrew info [--formula|--cask] [--json[=v1|v2]] [--installed] [name…]\n       abrew info --analytics [--days N] [--category C] [--formula|--cask] <name>…\n"
        }
        "outdated" => {
            "Usage: abrew outdated [--formula|--cask] [--greedy] [--verbose|-v|--quiet|-q] [--json[=v1|v2]] [name…]\n"
        }
        "options" => {
            "Usage: abrew options [--compact] [--installed] [name…]\n  No names evaluates every formula, as in brew; tapped formulae run real Homebrew.\n"
        }
        "cleanup" => {
            "Usage: abrew cleanup [--dry-run|-n] [--prune=<days>|--prune=all|--scrub|-s] [name…]\n  With names, old-version cleanup is scoped and shared cached downloads are kept.\n  Omit names to prune downloads with --prune or --scrub.\n"
        }
        "missing" => "Usage: abrew missing [--hide=<name>,…] [name…]\n",
        "deps" => {
            "Usage: abrew deps [--tree] [--direct|--declared|-1|--1] [--installed] [--for-each] [--union] [--topological|-n] [name…]\n"
        }
        "desc" => {
            "Usage: abrew desc [--formula|--cask] <name>…\n       abrew desc [--formula|--cask] (--search|-s|--name|-n|--description|-d) <text>|/<regex>/\n"
        }
        "search" => {
            "Usage: abrew search [--formula|--cask] [--desc] <text>|/<regex>/\n  Regex search is case-sensitive, like brew; patterns the native engine refuses run real Homebrew.\n"
        }
        "bundle" => {
            "Usage: abrew bundle [--version T --sha256 H] [--force] | status | remove | dump | install|check [--file Brewfile] (cleanup delegates to real brew)\n  With no subcommand (direct abrew only), lays the pinned Homebrew under the abrew prefix (design §5). Typed as `brew bundle`, it runs real Homebrew.\n"
        }
        "services" => {
            "Usage: abrew services <Homebrew arguments>\n  Delegates unchanged to real Homebrew; no native service manager is installed.\n"
        }
        _ => HELP_MAIN,
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn badge_follows_brew_env_rules() {
        assert_eq!(badge_from(false, None).as_deref(), Some("🍺"));
        assert_eq!(
            badge_from(false, Some(String::new())).as_deref(),
            Some("🍺")
        );
        assert_eq!(
            badge_from(false, Some("✅".to_string())).as_deref(),
            Some("✅")
        );
        assert_eq!(badge_from(true, None), None);
        assert_eq!(badge_from(true, Some("✅".to_string())), None);
    }

    #[test]
    fn command_names_merge_aliases_sorted() {
        let plain = command_names(false);
        assert_eq!(plain.len(), COMMANDS.len());
        assert!(!plain.contains(&"ls"));
        let merged = command_names(true);
        assert_eq!(merged.len(), COMMANDS.len() + COMMAND_ALIASES.len());
        assert!(merged.windows(2).all(|w| w[0] < w[1]), "merged is sorted");
        for alias in COMMAND_ALIASES {
            assert!(merged.contains(alias), "{alias} listed");
            assert!(!COMMANDS.contains(alias), "{alias} not duplicated");
        }
    }

    #[test]
    fn unsupported_arguments_delegate_before_native_effects() {
        for (cmd, args) in [
            ("install", vec!["--HEAD", "wget"]),
            ("install", vec!["-s", "wget"]),
            (
                "install",
                vec!["--cask", "--ignore-dependencies", "firefox"],
            ),
            // info --analytics: the global form, dependent flags
            // without it, and json/installed combos delegate.
            ("info", vec!["--analytics"]),
            ("info", vec!["--days=30", "wget"]),
            ("info", vec!["--category", "install", "wget"]),
            ("info", vec!["--analytics", "--json", "wget"]),
            ("info", vec!["--analytics", "--installed", "wget"]),
            ("info", vec!["--analytics", "wget", "--days=40"]),
            ("info", vec!["--analytics", "--days", "40", "wget"]),
            ("info", vec!["--analytics", "wget", "--days"]),
            ("info", vec!["--analytics", "wget", "--days="]),
            ("info", vec!["--analytics", "--days", "wget"]),
            ("info", vec!["--analytics", "--days", "--formula", "wget"]),
            ("info", vec!["--analytics", "wget", "--category=other"]),
            ("info", vec!["--analytics", "--category", "other", "wget"]),
            ("info", vec!["--analytics", "wget", "--category"]),
            ("info", vec!["--analytics", "wget", "--category="]),
            (
                "info",
                vec!["--analytics", "--category", "--days=30", "wget"],
            ),
            // Values alone are not named packages; the global form still delegates.
            ("info", vec!["--analytics", "--days", "30"]),
            ("info", vec!["--analytics", "--category", "install"]),
            (
                "info",
                vec!["--analytics", "--days=30", "--category=install"],
            ),
            // Conflicting/repeated value options have no native precedence policy.
            (
                "info",
                vec!["--analytics", "--days=30", "--days=90", "wget"],
            ),
            (
                "info",
                vec![
                    "--analytics",
                    "--category=install",
                    "--category",
                    "build-error",
                    "wget",
                ],
            ),
            ("link", vec!["--overwrite", "wget"]),
            ("services", vec!["start", "wget"]),
            ("uninstall", vec!["--zap", "firefox"]),
            ("completions", vec!["powershell"]),
            ("completions", vec![]),
            ("postinstall", vec!["wget"]),
            ("cleanup", vec!["--prune=soon"]),
            // brew's platform spellings for --cache stay delegated,
            // as does a non-tap repository operand.
            ("--cache", vec!["--os=all", "wget"]),
            ("--cache", vec!["-s", "wget"]),
            ("--cache", vec!["--HEAD", "wget"]),
            ("--repository", vec!["notatap"]),
            ("--repo", vec!["a/b/c"]),
            // desc's search modes conflict in brew; combined modes
            // and refused regex constructs delegate whole.
            ("desc", vec!["-s", "-n", "wget"]),
            ("desc", vec!["--search", "--description", "wget"]),
            ("desc", vec!["-s", "/(?=http)client/"]),
            ("missing", vec!["--hide="]),
            ("missing", vec!["--hide=../outside"]),
            ("list", vec!["--pinned", "wget"]),
            // Brew's --multiple conflicts delegate whole.
            ("list", vec!["--multiple", "--cask"]),
            ("list", vec!["--multiple", "--pinned"]),
            ("list", vec!["--multiple", "--full-name"]),
            ("list", vec!["--json"]),
            ("list", vec!["--versions", "--json", "wget"]),
            ("list", vec!["--versions", "--json", "-l"]),
            ("list", vec!["--versions", "--json", "--pinned"]),
            // Regex constructs the bounded native engine refuses
            // (look-around, backreferences, \p{…}) delegate whole.
            ("search", vec!["/(?=http)client/"]),
            ("search", vec![r"/(\w)\1/"]),
            ("search", vec![r"/\p{L}+/"]),
            ("pin", vec!["../outside"]),
            ("install", vec!["other/tap/wget"]),
            // Even when only the opposite kind is installed, its short name
            // must not reach a mutating handler's census-based inference.
            ("uninstall", vec!["homebrew/cask/wget"]),
            ("uninstall", vec!["homebrew/core/firefox"]),
            ("uninstall", vec!["--formula", "homebrew/cask/wget"]),
            ("uninstall", vec!["--cask", "homebrew/core/firefox"]),
            ("pin", vec!["homebrew/cask/wget"]),
            ("link", vec!["homebrew/cask/wget"]),
            ("reinstall", vec!["homebrew/cask/wget"]),
            ("install", vec!["homebrew/core/wget"]),
            ("--prefix", vec!["homebrew/cask/wget"]),
            ("uninstall", vec!["--formula", "wget", "homebrew/cask/wget"]),
        ] {
            assert!(
                matches!(
                    native_args(cmd, &argv(&args)),
                    Err(InstallError::PassThrough(_))
                ),
                "{cmd} {args:?}"
            );
        }
        for (cmd, args) in [
            (
                "install",
                vec!["--formula", "--ignore-dependencies", "wget"],
            ),
            ("install", vec!["wget", "curl"]),
            ("uninstall", vec!["wget", "curl"]),
            ("upgrade", vec!["wget", "curl"]),
            ("upgrade", vec!["--cask", "firefox", "vlc"]),
            ("reinstall", vec!["--formula", "--force", "wget", "curl"]),
            ("reinstall", vec!["--cask", "--dry-run", "firefox", "vlc"]),
            ("install", vec!["--cask", "--dry-run", "firefox"]),
            ("cleanup", vec!["--dry-run", "wget"]),
            ("cleanup", vec!["wget", "tree"]),
            ("deps", vec!["--tree", "wget"]),
            ("deps", vec!["--installed"]),
            ("deps", vec!["--installed", "--tree", "wget"]),
            ("deps", vec!["--installed", "--direct", "wget"]),
            ("deps", vec!["--installed", "--tree"]),
            ("deps", vec!["--installed", "--direct"]),
            ("deps", vec!["--installed", "--tree", "--direct"]),
            ("deps", vec!["--tree", "--direct", "wget"]),
            // brew's four spellings of the one direct switch.
            ("deps", vec!["--1", "wget"]),
            ("deps", vec!["-1", "wget"]),
            ("deps", vec!["--declared", "wget"]),
            ("deps", vec!["--installed", "--1"]),
            ("deps", vec!["--installed", "-1", "wget"]),
            ("deps", vec!["--for-each", "wget", "curl"]),
            ("deps", vec!["--for-each", "--direct", "wget"]),
            ("deps", vec!["--for-each", "--installed"]),
            ("deps", vec!["--union", "wget", "curl"]),
            ("deps", vec!["-n", "wget"]),
            ("deps", vec!["--topological", "--direct", "wget"]),
            ("deps", vec!["--union", "-n", "wget", "curl"]),
            ("completions", vec!["bash"]),
            ("completions", vec!["zsh"]),
            ("completions", vec!["fish"]),
            ("--prefix", vec!["wget"]),
            ("--prefix", vec!["homebrew/core/wget"]),
            ("--cache", vec![]),
            ("--cache", vec!["wget"]),
            ("--cache", vec!["wget", "curl"]),
            ("--cache", vec!["--cask", "firefox"]),
            ("--cache", vec!["--formulae", "homebrew/core/wget"]),
            ("--repository", vec![]),
            ("--repository", vec!["homebrew/core"]),
            ("--repo", vec!["me/tools"]),
            ("install", vec!["--formula", "homebrew/core/wget"]),
            ("uninstall", vec!["--formula", "homebrew/core/wget"]),
            ("uninstall", vec!["--cask", "homebrew/cask/firefox"]),
            ("install", vec!["--cask", "homebrew/cask/firefox"]),
            ("fetch", vec!["--formula", "--deps", "homebrew/core/wget"]),
            ("fetch", vec!["--cask", "homebrew/cask/firefox"]),
            ("--cellar", vec!["wget"]),
            ("--cellar", vec!["homebrew/core/wget"]),
            ("uninstall", vec!["--force", "wget"]),
            ("uninstall", vec!["-f", "wget"]),
            ("link", vec!["--dry-run", "wget"]),
            ("link", vec!["-n", "wget"]),
            ("unlink", vec!["--dry-run", "wget"]),
            ("list", vec!["wget", "curl"]),
            ("list", vec!["--versions", "wget"]),
            ("list", vec!["--versions", "wget", "curl"]),
            ("list", vec!["--multiple"]),
            ("list", vec!["--multiple", "wget", "curl"]),
            ("list", vec!["--versions", "--multiple"]),
            ("list", vec!["--versions", "--json"]),
            ("list", vec!["--versions", "--json", "--formula"]),
            ("list", vec!["--versions", "--json", "--casks"]),
            ("list", vec!["--full-name"]),
            ("list", vec!["--full-name", "--versions"]),
            ("outdated", vec!["wget"]),
            ("outdated", vec!["--json=v2", "wget", "tree"]),
            ("list", vec!["-l"]),
            ("list", vec!["--long", "wget"]),
            ("missing", vec!["wget"]),
            ("missing", vec!["--hide=openssl@3,icu4c"]),
            ("cat", vec!["--cask", "firefox"]),
            ("cat", vec!["--cask", "homebrew/cask/firefox"]),
            ("--caskroom", vec!["firefox"]),
            ("--caskroom", vec!["homebrew/cask/firefox"]),
            ("desc", vec!["--cask", "firefox"]),
            ("desc", vec!["--formula", "wget"]),
            ("desc", vec!["--search", "http", "client"]),
            ("desc", vec!["-s", "/^lib.*@\\d$/"]),
            ("desc", vec!["--name", "wget"]),
            ("desc", vec!["-n", "/wget/"]),
            ("desc", vec!["--description", "retriever"]),
            ("desc", vec!["-d", "--casks", "browser"]),
            ("leaves", vec!["--installed-as-dependency"]),
            ("options", vec![]),
            ("options", vec!["--compact", "wget"]),
            ("options", vec!["--installed"]),
            ("options", vec!["--installed", "wget"]),
            ("outdated", vec!["--json=v2"]),
            ("outdated", vec!["--json", "--formula"]),
            ("info", vec!["--json=v2", "wget"]),
            ("info", vec!["--analytics", "wget"]),
            ("info", vec!["--analytics", "--days=30", "wget"]),
            (
                "info",
                vec!["--analytics", "--days", "90", "--cask", "firefox"],
            ),
            ("info", vec!["--analytics", "--category", "install", "wget"]),
            ("info", vec!["--analytics", "--days=365", "wget"]),
            ("info", vec!["--analytics", "wget", "--days", "30"]),
            (
                "info",
                vec!["--analytics", "--category=install-on-request", "wget"],
            ),
            (
                "info",
                vec!["--analytics", "wget", "--category", "build-error"],
            ),
            (
                "info",
                vec![
                    "--analytics",
                    "--category=install",
                    "--days",
                    "90",
                    "--formula",
                    "homebrew/core/wget",
                ],
            ),
            ("info", vec!["--installed", "--json"]),
            ("doctor", vec!["--quiet"]),
            ("shellenv", vec!["fish"]),
            ("tap", vec!["--list-official"]),
            ("cleanup", vec!["--prune=all"]),
            ("cleanup", vec!["--prune=30", "-n"]),
            ("cleanup", vec!["-s"]),
            // brew's plural kind spellings canonicalize to the
            // singular switches admission and handlers read.
            ("list", vec!["--formulae"]),
            ("list", vec!["--casks", "-1"]),
            ("search", vec!["--casks", "firefox"]),
            ("search", vec!["--formulae", "/^wget$/"]),
            ("install", vec!["--formulae", "wget"]),
            ("install", vec!["--casks", "firefox"]),
            ("uninstall", vec!["--casks", "firefox"]),
            ("outdated", vec!["--formulae"]),
            ("upgrade", vec!["--casks", "firefox"]),
            ("search", vec!["/wget.+/"]),
            ("search", vec!["--desc", "/http.*/"]),
            ("search", vec!["/^lib.*@\\d$/"]),
            // brew joins operands before detecting the /regex/ form.
            ("search", vec!["/http", "client/"]),
            ("-S", vec!["/(?i)WGET/"]),
        ] {
            assert!(native_args(cmd, &argv(&args)).is_ok(), "{cmd} {args:?}");
        }

        // The last representable day count remains exact; the next one and
        // an overflowing decimal refuse before native dispatch can take a lock.
        let max_days = 213_503_982_334_601u64;
        let max_arg = format!("--prune={max_days}");
        assert_eq!(
            cleanup_max_age(&argv(&[&max_arg])),
            Ok(Some(Some(std::time::Duration::from_secs(
                18_446_744_073_709_526_400
            ))))
        );
        assert!(native_args("cleanup", &argv(&[&max_arg])).is_ok());
        for args in [
            vec![],
            vec!["--prune=0"],
            vec!["--prune=30"],
            vec!["--prune=all"],
            vec!["--scrub"],
            vec!["-s", "--prune=30"],
        ] {
            assert!(native_args("cleanup", &argv(&args)).is_ok(), "{args:?}");
        }
        assert_eq!(cleanup_max_age(&[]), Ok(None));
        assert_eq!(
            cleanup_max_age(&argv(&["--prune=0"])),
            Ok(Some(Some(std::time::Duration::ZERO)))
        );
        assert_eq!(
            cleanup_max_age(&argv(&["--prune=30"])),
            Ok(Some(Some(std::time::Duration::from_secs(2_592_000))))
        );
        for args in [
            vec!["--prune=all"],
            vec!["--scrub"],
            vec!["-s", "--prune=30"],
        ] {
            assert_eq!(cleanup_max_age(&argv(&args)), Ok(Some(None)), "{args:?}");
        }
        assert_eq!(
            cleanup_max_age(&argv(&["--prune=0", "--prune=30"])),
            Ok(Some(Some(std::time::Duration::ZERO))),
            "the first valid prune value keeps its existing precedence"
        );
        for bad in [
            "--prune=213503982334602",
            "--prune=18446744073709551615",
            "--prune=18446744073709551616",
        ] {
            for args in [vec![bad], vec!["-s", bad], vec!["--prune=all", bad]] {
                let args = argv(&args);
                assert!(cleanup_max_age(&args).is_err(), "{args:?}");
                assert!(
                    matches!(native_args("cleanup", &args), Err(InstallError::Failed(_))),
                    "{args:?}"
                );
            }
        }
        for bad in ["--prune=", "--prune=-1", "--prune=+1", "--prune=soon"] {
            assert!(cleanup_max_age(&argv(&[bad])).is_err(), "{bad}");
        }

        let later = InstallError::PassThrough("later recipe needs brew".to_string());
        assert_eq!(after_native_progress(later.clone(), false), later);
        let refused = after_native_progress(later, true);
        assert!(matches!(
            refused,
            InstallError::Failed(reason) if reason.contains("later recipe needs brew")
        ));
        for completed in [false, true] {
            let native = InstallError::Failed("native refusal".to_string());
            assert_eq!(after_native_progress(native.clone(), completed), native);
        }

        // An inner dependency plan can retain progress and return Failed.
        // Accumulating that failure must not authorize a later member to
        // replay the whole command. A direct initial fallback stays allowed.
        let fallback = || InstallError::PassThrough("later recipe needs brew".to_string());
        let initial: Result<(), _> = Err(fallback());
        assert!(!native_attempt_may_have_progress(&initial));
        assert_eq!(
            after_native_progress(fallback(), native_attempt_may_have_progress(&initial)),
            fallback()
        );
        for earlier in [
            Ok(()),
            Err(InstallError::Failed("retained dependency work".to_string())),
        ] {
            let possible_progress = native_attempt_may_have_progress(&earlier);
            assert!(possible_progress);
            assert!(matches!(
                after_native_progress(fallback(), possible_progress),
                InstallError::Failed(_)
            ));
            assert_eq!(
                after_native_progress(
                    InstallError::Failed("ordinary failure".to_string()),
                    possible_progress
                ),
                InstallError::Failed("ordinary failure".to_string())
            );
        }

        // Recursive cask dependencies share this same boundary. Empty or
        // already-installed walks record no attempt; an initial fallback
        // remains unchanged. A successful F1 or a failed partial attempt
        // makes F2's later fallback a local failure.
        let mut possible_progress = false;
        let initial: Result<(), _> = after_native_attempt(&mut possible_progress, Err(fallback()));
        assert_eq!(initial, Err(fallback()));
        assert!(!possible_progress);
        for first in [
            Ok(()),
            Err(InstallError::Failed("retained dependency work".to_string())),
        ] {
            let mut possible_progress = false;
            let expected = first.clone();
            assert_eq!(
                after_native_attempt(&mut possible_progress, first),
                expected
            );
            assert!(possible_progress);
            let later: Result<(), _> =
                after_native_attempt(&mut possible_progress, Err(fallback()));
            assert!(matches!(later, Err(InstallError::Failed(_))));
            assert!(possible_progress);
        }
    }

    #[test]
    fn brewfile_admission_and_check_preserve_missing_and_unsupported_outcomes() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&temp.path().join("uncreated-prefix"));
        let file = temp.path().join("Brewfile");
        let path = file.to_str().unwrap();
        std::fs::write(&file, "brew \"wget\"\ncask \"firefox\"\ntap \"me/tools\"\n").unwrap();
        // Multi-entry bundle install is admitted (prelock plans the
        // whole batch; a non-native member delegates before effects).
        assert!(
            matches!(
                bundle_args(&argv(&["install", "--file", path])),
                Ok(Some(entries)) if entries.len() == 3
            ),
            "multi-entry bundle install must admit"
        );
        assert_eq!(
            dispatch(&prefix, &argv(&["bundle", "check", "--file", path]), false),
            ExitCode::from(1)
        );
        assert!(
            !prefix.root.exists(),
            "checking must not create or lock a prefix"
        );
        std::fs::write(&file, "brew \"wget\", args: [\"HEAD\"]\n").unwrap();
        assert!(matches!(
            bundle_args(&argv(&["install", "--file", path])),
            Err(InstallError::PassThrough(_))
        ));
        assert!(!prefix.root.exists());
        std::fs::write(&file, "brew \"other/tap/wget\"\n").unwrap();
        assert!(matches!(
            bundle_args(&argv(&["install", "--file", path])),
            Err(InstallError::PassThrough(_))
        ));
        std::fs::write(&file, "# empty Brewfile\n").unwrap();
        assert_eq!(
            dispatch(&prefix, &argv(&["bundle", "check", "--file", path]), false),
            ExitCode::SUCCESS
        );
        assert!(!prefix.root.exists());
    }

    #[test]
    fn plural_kind_switches_canonicalize_exact_tokens_only() {
        assert_eq!(
            canonical_kind_switches(&argv(&["--formulae", "wget", "--casks"])),
            argv(&["--formula", "wget", "--cask"])
        );
        // Idempotent, and only the exact switch tokens change: a
        // value-carrying or operand-shaped token is untouched.
        let once = canonical_kind_switches(&argv(&["--formula", "--hide=--casks", "casks"]));
        assert_eq!(canonical_kind_switches(&once), once);
        // Canonicalization never manufactures admission: a command
        // that takes no kind switch still delegates with plurals.
        assert!(matches!(
            native_args("tap", &argv(&["--formulae"])),
            Err(InstallError::PassThrough(_))
        ));
        assert!(matches!(
            native_args("doctor", &argv(&["--casks"])),
            Err(InstallError::PassThrough(_))
        ));
    }

    #[test]
    fn escape_and_admission_messages_are_precise() {
        use std::ffi::OsString;
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(escape_requested_os(&os(&["--real", "install"])));
        // `off` is a toggle command, not an escape; `on` never escapes.
        assert!(!escape_requested_os(&os(&["on"])));
        assert!(!escape_requested_os(&os(&["install", "--real"])));
        assert!(!escape_requested_os(&os(&[])));
        // A known-but-delegated command and an unknown one say so distinctly.
        let msg = |args: &[&str]| {
            let args = argv(args);
            match native_args(&args[0], &args[1..]) {
                Err(InstallError::PassThrough(m)) => m,
                other => panic!("expected passthrough, got {other:?}"),
            }
        };
        assert!(msg(&["services", "list"]).contains("not native yet"));
        assert!(valid_cask_version("4.2.1,20260531.1"));
        assert!(valid_cask_version("1.0"));
        assert!(!valid_cask_version("../1.0"));
        assert!(!valid_cask_version("1.0/2"));
        assert!(!valid_cask_version(""));
        // Read-only dispatches on a fresh prefix create nothing.
        let temp = aterm_tempfile::tempdir().unwrap();
        let fresh = Prefix::at(&temp.path().join("never-created"));
        assert_eq!(dispatch(&fresh, &argv(&["list"]), false), ExitCode::SUCCESS);
        assert!(!fresh.root.exists());
        assert_eq!(
            dispatch(&fresh, &argv(&["--version"]), false),
            ExitCode::SUCCESS
        );
        assert!(!fresh.root.exists());
        // Named global queries answer from paths alone, creating
        // nothing: bare and tap-named repository, bare cache.
        for q in [
            &["--repository"][..],
            &["--repository", "homebrew/core"][..],
            &["--repo", "me/tools"][..],
            &["--cache"][..],
        ] {
            assert_eq!(
                dispatch(&fresh, &argv(q), false),
                ExitCode::SUCCESS,
                "{q:?}"
            );
            assert!(!fresh.root.exists(), "{q:?}");
        }
        assert!(msg(&["frobnicate"]).contains("not a command abrew knows"));
        // Known brew verbs that delegate whole are not "unknown".
        assert!(msg(&["zap"]).contains("arguments are not native yet"));
        assert!(msg(&["audit"]).contains("arguments are not native yet"));
    }
}
