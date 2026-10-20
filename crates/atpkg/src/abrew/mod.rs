// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! abrew — aterm's Homebrew, an atpkg extension
//! (`docs/DESIGN-abrew-2026-10-19.md`).
//!
//! The intended product is native Homebrew compatibility. The implemented
//! subset admits relocation-free bottles and simple cask artifacts; source
//! recipes, replacements, services and unsupported arguments delegate before
//! effects. `brew --real …`, `ABREW_REAL=1` and `abrew off` select real brew.
//! One argument-admission function preserves this boundary for every entry.

mod commands;
use commands::*;

pub mod api;
pub mod bundle;
pub mod cask;
pub mod fetch;
pub mod formula;
pub mod install;
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

fn reroute_passthrough_env() -> &'static str {
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
    if prefix::overlaps_real_brew_prefix(&prefix.root) {
        return failed(&format!(
            "abrew: refusing ABREW_PREFIX {}: it overlaps a real Homebrew prefix; abrew and real brew must own disjoint trees",
            prefix.root.display()
        ));
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

fn from_install(prefix: &Prefix, args: &[String], r: Result<(), InstallError>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(InstallError::Failed(m)) => failed(&m),
        Err(InstallError::PassThrough(reason)) => passthrough(prefix, args, Some(&reason)),
    }
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
            | "doctor"
            | "dr"
    )
}

/// Admit only argument forms whose meaning is implemented. Unsupported forms
/// reach real brew unchanged before locking or creating anything in the prefix.
fn native_args(
    cmd: &str,
    rest: &[String],
) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let unsupported = || InstallError::PassThrough(format!("'{cmd}' arguments are not native yet"));
    let unknown = || {
        if COMMANDS.contains(&cmd) {
            unsupported()
        } else {
            InstallError::PassThrough(format!("'{cmd}' is not a command abrew knows natively"))
        }
    };
    if cmd == "bundle" {
        return bundle_args(rest);
    }
    let (flags, min, max, names): (&[&str], usize, usize, bool) = match cmd {
        "install" | "reinstall" | "upgrade" => (
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
            ],
            1,
            1,
            true,
        ),
        "uninstall" | "remove" | "rm" => (
            &[
                "--formula",
                "--cask",
                "--ignore-dependencies",
                "--dry-run",
                "-n",
            ],
            1,
            1,
            true,
        ),
        "search" | "-S" => (&["--desc", "--formula", "--cask"], 1, usize::MAX, false),
        "info" => (&["--cask"], 0, usize::MAX, true),
        "desc" | "uses" | "dependents" | "options" | "linkage" => (&[], 1, usize::MAX, true),
        "home" | "homepage" => (&[], 0, usize::MAX, true),
        "cat" => (&[], 1, 1, true),
        "outdated" => (&["--formula", "--cask"], 0, 0, false),
        "cleanup" | "autoremove" => (&["--dry-run", "-n"], 0, 1, true),
        "pin" | "unpin" | "link" | "ln" | "unlink" => (&[], 1, 1, true),
        "switch" => (&[], 2, 2, true),
        "list" | "ls" => (
            &["--formula", "--cask", "--versions", "--pinned"],
            0,
            1,
            true,
        ),
        "fetch" => (&["--cask"], 1, 1, true),
        "deps" => (&["--tree", "--installed", "--direct"], 0, usize::MAX, true),
        "log" => (&[], 0, 1, true),
        "tap" => (&[], 0, 2, false),
        "untap" => (&[], 1, 1, false),
        "tap-info" => (&[], 0, usize::MAX, false),
        "help" | "--help" | "-h" => (&[], 0, 1, false),
        "completions" => (&[], 1, 1, false),
        "--prefix" => (&[], 0, 1, true),
        "analytics" => (&[], 0, 1, false),
        "on" | "off" | "status" | "--version" | "-v" | "--cellar" | "--caskroom" | "--cache"
        | "--repository" | "--env" | "env" | "formulae" | "casks" | "commands" | "config"
        | "shellenv" | "doctor" | "dr" | "developer" | "update" | "leaves" | "missing" => {
            (&[], 0, 0, false)
        }
        _ => return Err(unknown()),
    };
    if rest
        .iter()
        .any(|arg| arg.starts_with('-') && !flags.contains(&arg.as_str()))
    {
        return Err(unsupported());
    }
    let ops = operands(rest);
    if cmd == "completions"
        && !ops
            .first()
            .is_some_and(|shell| matches!(shell.as_str(), "bash" | "zsh" | "fish"))
    {
        return Err(unsupported());
    }
    if !(min..=max).contains(&ops.len()) || (names && !ops.iter().all(|s| valid_component(s))) {
        return Err(unsupported());
    }
    if has_flag(rest, "--cask", "") && has_flag(rest, "--formula", "") {
        return Err(unsupported());
    }
    if matches!(cmd, "install" | "reinstall" | "upgrade") {
        if cmd != "install" && !has_flag(rest, "--formula", "") {
            // Native cask replacement is not transactional; selecting formula
            // scope is required for native upgrade/reinstall.
            return Err(unsupported());
        }
        if !has_flag(rest, "--formula", "")
            && (has_flag(rest, "--quiet", "-q") || has_flag(rest, "--ignore-dependencies", ""))
        {
            return Err(unsupported());
        }
    }
    if matches!(cmd, "list" | "ls")
        && !ops.is_empty()
        && (has_flag(rest, "--versions", "") || has_flag(rest, "--pinned", ""))
    {
        return Err(unsupported());
    }
    if cmd == "autoremove" && !ops.is_empty() {
        return Err(unsupported());
    }
    if cmd == "deps"
        && ((has_flag(rest, "--installed", "") && (!ops.is_empty() || rest.len() != 1))
            || (!has_flag(rest, "--installed", "") && ops.is_empty())
            || (has_flag(rest, "--tree", "") && has_flag(rest, "--direct", "")))
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
        "untap" | "tap-info" if !ops.iter().all(|s| tap::parse_tap_name(s).is_some()) => {
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
        Some("status" | "remove" | "dump") if rest.len() == 1 => Ok(None),
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
            if !skipped.is_empty() || (rest[0] == "install" && entries.len() > 1) {
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
    let name = rest.iter().find(|s| !s.starts_with('-'))?;
    let want_cask = rest.iter().any(|s| s == "--cask");
    let want_formula = rest.iter().any(|s| s == "--formula");
    let result = if want_cask || (!want_formula && api.formula(name).is_err()) {
        match api.cask(name) {
            Ok(c) => cask::preflight(&c),
            Err(_) => return None,
        }
    } else {
        let opts = install::InstallOpts {
            ignore_dependencies: rest.iter().any(|s| s == "--ignore-dependencies"),
            dry_run: false,
            force: rest.iter().any(|s| matches!(s.as_str(), "--force" | "-f")),
            quiet: true,
        };
        install::preflight_plan(api, prefix, name, &opts)
    };
    match result {
        Ok(()) => None,
        Err(e @ InstallError::PassThrough(_)) => Some(from_install(prefix, args, Err(e))),
        Err(_) => None,
    }
}

fn dispatch(prefix: &Prefix, args: &[String], via_reroute: bool) -> ExitCode {
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
    if args.is_empty() {
        print!("{HELP_MAIN}");
        return ExitCode::SUCCESS;
    }
    let (cmd, rest): (&str, &[String]) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("", &[]),
    };
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
        "--prefix" => {
            if let Some(name) = rest.first() {
                println!("{}", prefix.opt(Formula::short_name(name)).display());
            } else {
                println!("{}", prefix.root.display());
            }
            return ExitCode::SUCCESS;
        }
        "--cellar" => {
            println!("{}", prefix.cellar().display());
            return ExitCode::SUCCESS;
        }
        "--caskroom" => {
            println!("{}", prefix.caskroom().display());
            return ExitCode::SUCCESS;
        }
        "--cache" => {
            println!("{}", prefix.cache().display());
            return ExitCode::SUCCESS;
        }
        "--repository" => {
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
    let _lock = if mutates(cmd) && !read_only {
        match prefix::Lock::acquire(prefix) {
            Ok(l) => Some(l),
            Err(e) => return failed(&e),
        }
    } else {
        None
    };
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
            for c in COMMANDS {
                println!("{c}");
            }
            ExitCode::SUCCESS
        }
        "completions" => {
            print!("{}", completions(rest.first().map_or("", String::as_str)));
            ExitCode::SUCCESS
        }
        "config" => cmd_config(prefix),
        "shellenv" => cmd_shellenv(prefix),
        "doctor" | "dr" => cmd_doctor(prefix),
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
        "unpin" => each_name(rest, |n| prefix.unpin(Formula::short_name(n))),
        "link" | "ln" => cmd_link(prefix, &api, args, rest, true),
        "unlink" => cmd_link(prefix, &api, args, rest, false),
        "switch" => cmd_switch(prefix, &api, args, rest),
        "list" | "ls" => cmd_list(prefix, rest),
        "fetch" => cmd_fetch(prefix, &api, args, rest),
        // graphs
        "deps" => cmd_deps(prefix, &api, rest),
        "uses" | "dependents" => cmd_uses(prefix, &api, rest),
        "leaves" => cmd_leaves(prefix, &api),
        "missing" => cmd_missing(prefix, &api),
        "options" => cmd_options(&api, rest),
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

const HELP_MAIN: &str = "\
abrew — aterm's Homebrew (docs/DESIGN-abrew-2026-10-19.md)

Usage: abrew <command> [args]

Common commands:
  install [--cask] <name>         install a supported bottle or cask
  uninstall [--cask] <name>       remove one (multi-name delegates)
  reinstall, upgrade, outdated    refresh installed packages
  update                          refresh formula data and taps
  search <text>, info <name>      look packages up
  list [--cask] [--versions]      what is installed
  doctor                          what needs attention
  deps, uses, leaves, missing     the dependency graph
  pin, unpin, link, unlink        manage kegs
  tap, untap, bundle              taps and the Homebrew fallback bundle
  services, postinstall, -s       delegate to real Homebrew
  cleanup, autoremove, fetch      housekeeping
  completions bash|zsh|fish       shell discovery

Real Homebrew, unchanged:  brew --real <command>…   ABREW_REAL=1   abrew off
Native installs are limited to relocation-free bottles and simple App/Binary
zip/tar casks. Source builds, replacements, services, batch installs and
unsupported flags/recipes run real brew with a notice, using its own prefix.
";

fn help_for(cmd: &str) -> &'static str {
    match cmd {
        "install" => {
            "Usage: abrew install [--formula|--cask] [--ignore-dependencies] [--force|-f] [--dry-run|-n] [--quiet|-q] <name>\n  Source (-s), --HEAD and multi-name forms run real Homebrew.\n"
        }
        "uninstall" | "remove" => {
            "Usage: abrew uninstall [--formula|--cask] [--ignore-dependencies] [--dry-run|-n] <name>\n"
        }
        "list" | "ls" => "Usage: abrew list [--cask] [--versions] [--pinned] [name]\n",
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
    fn unsupported_arguments_delegate_before_native_effects() {
        for (cmd, args) in [
            ("install", vec!["--HEAD", "wget"]),
            ("install", vec!["-s", "wget"]),
            ("install", vec!["wget", "curl"]),
            (
                "install",
                vec!["--cask", "--ignore-dependencies", "firefox"],
            ),
            ("info", vec!["--json", "wget"]),
            ("link", vec!["--overwrite", "wget"]),
            ("services", vec!["start", "wget"]),
            ("uninstall", vec!["--zap", "firefox"]),
            ("completions", vec!["powershell"]),
            ("completions", vec![]),
            ("postinstall", vec!["wget"]),
            ("cleanup", vec!["--prune=all"]),
            ("outdated", vec!["wget"]),
            ("list", vec!["--versions", "wget"]),
            ("pin", vec!["../outside"]),
            ("install", vec!["other/tap/wget"]),
            ("--prefix", vec!["homebrew/core/wget"]),
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
            ("install", vec!["--cask", "--dry-run", "firefox"]),
            ("cleanup", vec!["--dry-run", "wget"]),
            ("deps", vec!["--tree", "wget"]),
            ("completions", vec!["bash"]),
            ("completions", vec!["zsh"]),
            ("completions", vec!["fish"]),
            ("--prefix", vec!["wget"]),
        ] {
            assert!(native_args(cmd, &argv(&args)).is_ok(), "{cmd} {args:?}");
        }
    }

    #[test]
    fn brewfile_admission_and_check_preserve_missing_and_unsupported_outcomes() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&temp.path().join("uncreated-prefix"));
        let file = temp.path().join("Brewfile");
        let path = file.to_str().unwrap();
        std::fs::write(&file, "brew \"wget\"\ncask \"firefox\"\ntap \"me/tools\"\n").unwrap();
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
        assert!(msg(&["frobnicate"]).contains("not a command abrew knows"));
    }
}
