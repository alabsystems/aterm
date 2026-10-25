// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native brew compatibility in atpkg (`docs/DESIGN-abrew-2026-10-19.md`).

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

/// One package, version or tap path component.
pub(crate) fn valid_component(name: &str) -> bool {
    let mut parts = std::path::Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && parts.next().is_none()
        && !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+@-".contains(&c))
}

/// A package version as a single directory name.
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
    let Ok(argv) = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        return failed("abrew: native brew arguments must be valid UTF-8");
    };
    run(&argv, false)
}

const fn reroute_passthrough_env() -> &'static str {
    "__ATERM_REROUTE_PASSTHROUGH"
}

/// `aterm pkg abrew …` (the atpkg verb).
pub fn cli_entry(args: &[String]) -> ExitCode {
    run(args, false)
}

/// `atpkg __abrew …` — what the `brew` stub execs.
pub fn rerouted_entry(args: &[String]) -> ExitCode {
    run(args, true)
}

/// Runs `abrew` on `args`.
pub fn run(args: &[String], via_reroute: bool) -> ExitCode {
    if cfg!(windows) {
        eprintln!("abrew: Homebrew is macOS/Linux software; abrew does nothing on Windows");
        return ExitCode::from(1);
    }
    let prefix = Prefix::resolve();
    if let Some(w) = prefix::prefix_env_warning() {
        eprintln!("abrew: warning: {w}");
    }
    let args = command_args(args);
    let first = args.first().map(String::as_str).unwrap_or("");
    if first == stub::REAL_FLAG {
        return unsupported(
            "--real is no longer supported; brew uses atpkg's native implementation",
        );
    }
    let recovery = matches!(first, "on" | "status");
    if stub::engaged(std::env::var(stub::REAL_ENV).ok().as_deref()) && !recovery {
        return unsupported("unset ABREW_REAL to use native brew compatibility");
    }
    if stub::engaged(std::env::var(reroute_passthrough_env()).ok().as_deref()) && !recovery {
        return failed("abrew: native brew routing is disabled for this --no-reroute session");
    }
    if !crate::config::cached_abrew().enabled() && !recovery {
        return failed("abrew: native brew is disabled; set [abrew] enabled=true in aterm.toml");
    }
    if prefix.is_disabled() && !recovery {
        return failed("abrew: native brew is off; run abrew on to re-enable it");
    }
    if prefix::overlaps_real_brew_prefix(&prefix.root) {
        return failed(&format!(
            "abrew: refusing ABREW_PREFIX {}: it overlaps a real Homebrew prefix or its physical location is uncertain; abrew and real brew must own disjoint trees",
            prefix.root.display()
        ));
    }
    if via_reroute && crate::config::cached_reroute().announce() {
        eprintln!("brew → atpkg (native brew compatibility)");
    }
    dispatch(&prefix, args)
}

fn unsupported(reason: &str) -> ExitCode {
    failed(&format!("abrew: unsupported native operation: {reason}"))
}

fn not_native(_prefix: &Prefix, _args: &[String], cmd: &str) -> ExitCode {
    unsupported(&format!("'{cmd}' is not native"))
}

fn failed(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

/// Brew boolean env config is presence-based (`boolean: :set`).
pub(super) fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

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

/// `HOMEBREW_NO_INSTALL_CLEANUP`: skip removing superseded versions after install/upgrade (explicit
/// `cleanup` still works).
pub(super) fn no_install_cleanup() -> bool {
    env_set("HOMEBREW_NO_INSTALL_CLEANUP")
}

/// `HOMEBREW_NO_ENV_HINTS`: suppress environment-behaviour hints.
pub(super) fn no_env_hints() -> bool {
    env_set("HOMEBREW_NO_ENV_HINTS")
}

fn from_install(_prefix: &Prefix, _args: &[String], r: Result<(), InstallError>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(InstallError::Failed(m)) => failed(&m),
        Err(InstallError::Unsupported(reason)) => unsupported(&reason),
    }
}

fn after_native_progress(error: InstallError, completed: bool) -> InstallError {
    match error {
        InstallError::Unsupported(reason) if completed => InstallError::Failed(format!(
            "abrew: {reason}; earlier native steps may already have changed the prefix"
        )),
        error => error,
    }
}

const fn native_attempt_may_have_progress<T>(result: &Result<T, InstallError>) -> bool {
    !matches!(result, Err(InstallError::Unsupported(_)))
}

fn after_native_attempt<T>(
    possible_progress: &mut bool,
    result: Result<T, InstallError>,
) -> Result<T, InstallError> {
    *possible_progress |= native_attempt_may_have_progress(&result);
    result.map_err(|error| after_native_progress(error, *possible_progress))
}

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

fn canonical_kind_switches(rest: &[String]) -> Vec<String> {
    rest.iter()
        .map(|s| match s.as_str() {
            "--formulae" => "--formula".to_string(),
            "--casks" => "--cask".to_string(),
            _ => s.clone(),
        })
        .collect()
}

fn native_args(
    cmd: &str,
    rest: &[String],
) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    let unsupported =
        || InstallError::Unsupported(format!("'{cmd}' with these arguments is not native"));
    let unknown = || {
        const UNSUPPORTED_KNOWN: &[&str] = &[
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
        if COMMANDS.contains(&cmd) || UNSUPPORTED_KNOWN.contains(&cmd) {
            InstallError::Unsupported(format!("'{cmd}' is not native"))
        } else {
            InstallError::Unsupported(format!("'{cmd}' is not a command abrew knows natively"))
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
        cleanup_max_age(rest).map_err(|error| InstallError::Failed(format!("abrew: {error}")))?;
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
    if matches!(cmd, "list" | "ls") {
        if has_flag(rest, "--multiple", "")
            && (has_flag(rest, "--cask", "")
                || has_flag(rest, "--pinned", "")
                || has_flag(rest, "--full-name", ""))
        {
            return Err(unsupported());
        }
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
    if cmd == "deps" && !has_flag(rest, "--installed", "") && ops.is_empty() {
        return Err(unsupported());
    }
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
    let unsupported =
        || InstallError::Unsupported("bundle arguments or Brewfile rows are not native".to_owned());
    match rest.first().map(String::as_str) {
        Some("status" | "remove") if rest.len() == 1 => Ok(None),
        Some("dump") => {
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
        Some("check" | "install") | None => brewfile_args(rest.get(1..).unwrap_or(&[])),
        Some(flag) if flag.starts_with("--file") => brewfile_args(rest),
        _ => Err(unsupported()),
    }
}

fn brewfile_args(rest: &[String]) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let unsupported =
        || InstallError::Unsupported("bundle arguments or Brewfile rows are not native".to_owned());
    let mut file = None;
    let mut it = rest.iter();
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

fn prelock_admission(
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
        Err(e @ InstallError::Unsupported(_)) => Some(from_install(prefix, args, Err(e))),
        Err(_) => None,
    }
}

/// The single supported leading verbosity flag does not change the command's identity.
fn command_args(args: &[String]) -> &[String] {
    if args.len() > 1
        && matches!(args[0].as_str(), "--verbose" | "-v" | "--debug" | "-d")
        && !args[1].starts_with('-')
    {
        &args[1..]
    } else {
        args
    }
}

fn dispatch(prefix: &Prefix, args: &[String]) -> ExitCode {
    let args = command_args(args);
    if args.is_empty() {
        print!("{HELP_MAIN}");
        return ExitCode::SUCCESS;
    }
    let (cmd, rest): (&str, &[String]) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("", &[]),
    };
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    let prepared = match native_args(cmd, rest) {
        Ok(prepared) => prepared,
        Err(error) => return from_install(prefix, args, Err(error)),
    };
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
            return unsupported(
                "--repository without a tap: atpkg has no Homebrew source repository",
            );
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
    if matches!(cmd, "install" | "upgrade" | "reinstall" | "fetch") && !dry_run {
        let api0 = api::Api::new(prefix.clone());
        if let Some(code) = prelock_admission(prefix, &api0, args, rest) {
            return code;
        }
    }
    if cmd == "bundle"
        && !rest
            .first()
            .is_some_and(|s| matches!(s.as_str(), "check" | "status" | "remove" | "dump"))
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
                    if prefix
                        .installed_one(prefix::Kind::Cask, api::canonical_cask_token(n))
                        .is_some()
                    {
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
        if let Err(e @ InstallError::Unsupported(_)) = plan {
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
        prefix.restore_stale_asides();
    }
    let api = if dry_run {
        api::Api::read_only(prefix.clone())
    } else {
        api::Api::new(prefix.clone())
    };
    match cmd {
        "on" => cmd_on(prefix),
        "off" => cmd_off(prefix),
        "status" => cmd_status(prefix),
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
        "deps" => cmd_deps(prefix, &api, rest),
        "uses" | "dependents" => cmd_uses(prefix, &api, rest),
        "leaves" => cmd_leaves(prefix, &api, rest),
        "missing" => cmd_missing(prefix, &api, rest),
        "options" => cmd_options(prefix, &api, args, rest),
        "linkage" => cmd_linkage(prefix, rest),
        "log" => cmd_log(prefix, rest),
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

fn completions(shell: &str) -> String {
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
            "_abrew() {{\n  COMPREPLY=( $(compgen -W \"{words} --version --help\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") )\n}}\ncomplete -F _abrew abrew brew atpkg-abrew\n"
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

fn command_names(include_aliases: bool) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = COMMANDS.to_vec();
    if include_aliases {
        names.extend(COMMAND_ALIASES.iter().copied());
        names.sort_unstable();
    }
    names
}

const HELP_MAIN: &str = "\
abrew — native brew compatibility in atpkg

Usage: abrew <command> [args]

Common commands:
  install [--cask] <name>…        install bottles and casks
  uninstall [--cask] <name>…      remove installed packages
  reinstall, upgrade, outdated    refresh installed packages, list stale ones
  update                          refresh formula and cask data, and taps
  search <text>, info <name>      look packages up
  list [--cask] [--versions] [-l] what is installed
  doctor                          what needs attention
  deps, uses, leaves, missing     the dependency graph
  pin, unpin, link, unlink        manage kegs
  tap, untap                      manage taps
  bundle                          native Brewfile install, check and dump
  cleanup, autoremove, fetch      housekeeping
  completions bash|zsh|fish       print a shell completion script
  on, off, status                 enable, disable or inspect native brew

abrew pours bottles that need no relocation (or, on macOS with the developer
tools, `:any` relocation) and installs tar, zip, DMG and single-file casks
whose artifacts and constraints are plain data. Unsupported source builds,
post-install recipes, pkg casks, services and flags fail with their missing
native capability. atpkg never installs or invokes real Homebrew as a fallback;
--real and ABREW_REAL are unsupported. Existing legacy bundles are unused and
can be inspected or explicitly removed with bundle status / bundle remove.

Further help: abrew help <command>; abrew commands; aterm help abrew
";

fn help_for(cmd: &str) -> &'static str {
    match cmd {
        "install" => {
            "Usage: abrew install [--formula|--cask] [--ignore-dependencies] [--force|-f] [--dry-run|-n] [--quiet|-q] <name>…\n  --ignore-dependencies and --quiet are supported only with --formula. Source\n  builds (-s, --HEAD), unsupported flags and recipes fail natively; Homebrew\n  is never a fallback. --no-quarantine, --verbose and --debug are accepted\n  and change nothing. Names are planned before installation; a later failure\n  reports that earlier native steps may already have changed the prefix.\n"
        }
        "reinstall" => {
            "Usage: abrew reinstall [--formula|--cask] [--ignore-dependencies] [--dry-run|-n] [--quiet|-q] <name>…\n  --ignore-dependencies and --quiet stay native only with --formula.\n"
        }
        "upgrade" => {
            "Usage: abrew upgrade [--formula|--cask] [--greedy] [--ignore-dependencies] [--force|-f] [--dry-run|-n] [--quiet|-q] [name…]\n  --ignore-dependencies and --quiet stay native only with --formula. No names:\n  every outdated formula not pinned; --cask: outdated casks (--greedy adds the\n  ones that update themselves).\n"
        }
        "uninstall" | "remove" | "rm" => {
            "Usage: abrew uninstall [--formula|--cask] [--ignore-dependencies] [--force|-f] [--dry-run|-n] <name>…\n  --force removes every installed version, pinned or not.\n"
        }
        "list" | "ls" => {
            "Usage: abrew list [--formula|--cask] [--versions] [--multiple] [--pinned] [--full-name] [-l|--long] [-1] [name…]\n       abrew list --versions --json [--formula|--cask]\n"
        }
        "info" => {
            "Usage: abrew info [--formula|--cask] [--json[=v1|v2]] [--installed] [name…]\n       abrew info --analytics [--days 30|90|365] [--category C] [--formula|--cask] <name>…\n  C is install, install-on-request or build-error.\n"
        }
        "outdated" => {
            "Usage: abrew outdated [--formula|--cask] [--greedy] [--verbose|-v|--quiet|-q] [--json[=v2]] [name…]\n"
        }
        "options" => {
            "Usage: abrew options [--compact] [--installed] [name…]\n  No names: every formula, as in brew; with names, --installed lists the\n  installed ones in their place. Tapped formulae, and the no-name listing\n  once a tap exists, are not supported natively and fail.\n"
        }
        "cleanup" => {
            "Usage: abrew cleanup [--dry-run|-n] [--prune=<days>|--prune=all|--scrub|-s] [name…]\n  Removes old versions (only the named packages', when names are given). With no\n  names, --prune=<days> also deletes cached downloads at least that old, and\n  --prune=all or --scrub every cached download.\n"
        }
        "missing" => "Usage: abrew missing [--hide=<name>,…] [name…]\n",
        "uses" | "dependents" => {
            "Usage: abrew uses [--installed] <formula>…\n  Installed formulae that depend directly on each name; abrew reads installed\n  formulae only, so --installed changes nothing.\n"
        }
        "leaves" => {
            "Usage: abrew leaves [--installed-on-request|--installed-as-dependency]\n  Installed formulae no other installed formula depends on, optionally only\n  those installed on request, or only those installed as a dependency.\n"
        }
        "autoremove" => {
            "Usage: abrew autoremove [--dry-run|-n]\n  Uninstalls formulae installed as dependencies that no requested or pinned\n  formula still needs.\n"
        }
        "fetch" => {
            "Usage: abrew fetch [--formula|--cask] [--deps] <name>…\n  Downloads into the cache without installing; a cask needs --cask, and --deps\n  also fetches a formula's dependencies.\n"
        }
        "link" | "ln" | "unlink" => "Usage: abrew link|unlink [--dry-run|-n] <formula>\n",
        "doctor" | "dr" => {
            "Usage: abrew doctor [--quiet|-q]\n  Warns about the native prefix (unwritable, broken links, missing receipts,\n  extra versions, unlinked kegs, bad pins, bin not on PATH) and exits 1 on any;\n  --quiet drops the all-clear line. Homebrew is not a dependency.\n  `aterm pkg doctor` checks the managed `brew` stub.\n"
        }
        "tap" => {
            "Usage: abrew tap [--list-official] [<user/repo> [<URL>]]\n  No name: lists the taps abrew manages.\n"
        }
        "shellenv" => {
            "Usage: abrew shellenv [<shell>]\n  Prints ABREW_PREFIX, PATH (bin and sbin), MANPATH and INFOPATH settings for\n  <shell> or the invoking shell: fish, csh, tcsh and pwsh get their own syntax,\n  any other shell POSIX sh.\n"
        }
        "deps" => {
            "Usage: abrew deps [--tree] [--direct|--declared|-1|--1] [--installed] [--for-each] [--union] [--topological|-n] [name…]\n"
        }
        "desc" => {
            "Usage: abrew desc [--formula|--cask] <name>…\n       abrew desc [--formula|--cask] (--search|-s|--name|-n|--description|-d) <text>|/<regex>/\n"
        }
        "search" => {
            "Usage: abrew search [--formula|--cask] [--desc] <text>|/<regex>/\n  Text ignores case and all but ASCII letters, digits, @ and +; a /regex/ is\n  case-sensitive, as in brew. Shows at most 200 matches. Unsupported patterns\n  or patterns that outrun the native work budget fail (exit 1); simplify them.\n"
        }
        "bundle" => {
            "Usage: abrew bundle [install|check] [--file F]\n       abrew bundle dump [--file F] [--force]\n       abrew bundle status | remove\n  No subcommand means native Brewfile install. Unsupported rows fail before\n  installation; no Homebrew is installed or invoked. status/remove inspect\n  or explicitly remove an owned legacy Homebrew bundle, which is never used.\n"
        }
        "services" => {
            "Usage: abrew services <args>…\n  Not supported: atpkg's brew compatibility has no native service manager.\n"
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
    fn unsupported_arguments_are_refused_before_native_effects() {
        for (cmd, args) in [
            ("install", vec!["--HEAD", "wget"]),
            ("install", vec!["-s", "wget"]),
            (
                "install",
                vec!["--cask", "--ignore-dependencies", "firefox"],
            ),
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
            ("info", vec!["--analytics", "--days", "30"]),
            ("info", vec!["--analytics", "--category", "install"]),
            (
                "info",
                vec!["--analytics", "--days=30", "--category=install"],
            ),
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
            ("--cache", vec!["--os=all", "wget"]),
            ("--cache", vec!["-s", "wget"]),
            ("--cache", vec!["--HEAD", "wget"]),
            ("--repository", vec!["notatap"]),
            ("--repo", vec!["a/b/c"]),
            ("desc", vec!["-s", "-n", "wget"]),
            ("desc", vec!["--search", "--description", "wget"]),
            ("desc", vec!["-s", "/(?=http)client/"]),
            ("missing", vec!["--hide="]),
            ("missing", vec!["--hide=../outside"]),
            ("list", vec!["--pinned", "wget"]),
            ("list", vec!["--multiple", "--cask"]),
            ("list", vec!["--multiple", "--pinned"]),
            ("list", vec!["--multiple", "--full-name"]),
            ("list", vec!["--json"]),
            ("list", vec!["--versions", "--json", "wget"]),
            ("list", vec!["--versions", "--json", "-l"]),
            ("list", vec!["--versions", "--json", "--pinned"]),
            ("search", vec!["/(?=http)client/"]),
            ("search", vec![r"/(\w)\1/"]),
            ("search", vec![r"/\p{L}+/"]),
            ("pin", vec!["../outside"]),
            ("install", vec!["other/tap/wget"]),
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
                    Err(InstallError::Unsupported(_))
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
            ("search", vec!["/http", "client/"]),
            ("-S", vec!["/(?i)WGET/"]),
        ] {
            assert!(native_args(cmd, &argv(&args)).is_ok(), "{cmd} {args:?}");
        }

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

        let later = InstallError::Unsupported("later recipe needs brew".to_string());
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

        let fallback = || InstallError::Unsupported("later recipe needs brew".to_string());
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
        assert!(
            matches!(
                bundle_args(&argv(&["install", "--file", path])),
                Ok(Some(entries)) if entries.len() == 3
            ),
            "multi-entry bundle install must admit"
        );
        assert_eq!(
            dispatch(&prefix, &argv(&["bundle", "check", "--file", path])),
            ExitCode::from(1)
        );
        assert!(
            !prefix.root.exists(),
            "checking must not create or lock a prefix"
        );
        std::fs::write(&file, "brew \"wget\", args: [\"HEAD\"]\n").unwrap();
        assert!(matches!(
            bundle_args(&argv(&["install", "--file", path])),
            Err(InstallError::Unsupported(_))
        ));
        assert!(!prefix.root.exists());
        std::fs::write(&file, "brew \"other/tap/wget\"\n").unwrap();
        assert!(matches!(
            bundle_args(&argv(&["install", "--file", path])),
            Err(InstallError::Unsupported(_))
        ));
        std::fs::write(&file, "# empty Brewfile\n").unwrap();
        assert_eq!(
            dispatch(&prefix, &argv(&["bundle", "check", "--file", path])),
            ExitCode::SUCCESS
        );
        assert!(!prefix.root.exists());
        for args in [
            argv(&["bundle", "--file", path]),
            argv(&["bundle", "install", "--file", path]),
        ] {
            assert_eq!(dispatch(&prefix, &args), ExitCode::SUCCESS, "{args:?}");
            assert!(
                !prefix.homebrew_bundle().exists(),
                "native Brewfiles never lay Homebrew"
            );
        }
    }

    #[test]
    fn plural_kind_switches_canonicalize_exact_tokens_only() {
        assert_eq!(
            canonical_kind_switches(&argv(&["--formulae", "wget", "--casks"])),
            argv(&["--formula", "wget", "--cask"])
        );
        let once = canonical_kind_switches(&argv(&["--formula", "--hide=--casks", "casks"]));
        assert_eq!(canonical_kind_switches(&once), once);
        assert!(matches!(
            native_args("tap", &argv(&["--formulae"])),
            Err(InstallError::Unsupported(_))
        ));
        assert!(matches!(
            native_args("doctor", &argv(&["--casks"])),
            Err(InstallError::Unsupported(_))
        ));
    }

    #[test]
    fn native_admission_messages_are_precise() {
        let msg = |args: &[&str]| {
            let args = argv(args);
            match native_args(&args[0], &args[1..]) {
                Err(InstallError::Unsupported(m)) => m,
                other => panic!("expected native refusal, got {other:?}"),
            }
        };
        assert!(msg(&["services", "list"]).contains("'services' is not native"));
        assert!(valid_cask_version("4.2.1,20260531.1"));
        assert!(valid_cask_version("1.0"));
        assert!(!valid_cask_version("../1.0"));
        assert!(!valid_cask_version("1.0/2"));
        assert!(!valid_cask_version(""));
        let temp = aterm_tempfile::tempdir().unwrap();
        let fresh = Prefix::at(&temp.path().join("never-created"));
        assert_eq!(dispatch(&fresh, &argv(&["list"])), ExitCode::SUCCESS);
        assert!(!fresh.root.exists());
        assert_eq!(dispatch(&fresh, &argv(&["--version"])), ExitCode::SUCCESS);
        assert!(!fresh.root.exists());
        for q in [
            &["--repository", "homebrew/core"][..],
            &["--repo", "me/tools"][..],
            &["--cache"][..],
        ] {
            assert_eq!(dispatch(&fresh, &argv(q)), ExitCode::SUCCESS, "{q:?}");
            assert!(!fresh.root.exists(), "{q:?}");
        }
        assert!(msg(&["frobnicate"]).contains("not a command abrew knows"));
        assert!(msg(&["zap"]).contains("'zap' is not native"));
        assert!(msg(&["audit"]).contains("'audit' is not native"));
    }

    #[test]
    fn native_dispatch_refuses_unsupported_work_without_creating_a_prefix() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let fresh = Prefix::at(&temp.path().join("never-created"));
        for args in [
            &["--real", "list"][..],
            &["services", "list"][..],
            &["install", "--HEAD", "wget"][..],
            &[
                "bundle",
                "--version",
                "4.5.8",
                "--sha256",
                "6a5a18e6c5bea214983ec102dea3126cbc2d162540f8d93387fa9730d1f03be3",
            ][..],
            &["bundle", "--force"][..],
            &["--repository"][..],
        ] {
            assert_eq!(dispatch(&fresh, &argv(args)), ExitCode::from(1), "{args:?}");
            assert!(
                !fresh.root.exists(),
                "{args:?}: unsupported work created a prefix"
            );
        }
        for args in [
            &["--version"][..],
            &["-v"][..],
            &["env"][..],
            &["--env"][..],
        ] {
            assert_eq!(dispatch(&fresh, &argv(args)), ExitCode::SUCCESS, "{args:?}");
            assert!(!fresh.root.exists(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn alias_refuses_non_utf8_arguments_without_replacing_the_bytes() {
        use std::os::unix::ffi::OsStringExt as _;
        assert_eq!(
            main_entry(vec![OsString::from_vec(b"not-utf8-\xff".to_vec())]),
            ExitCode::from(1)
        );
    }
}
