// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native brew compatibility in atpkg (`docs/DESIGN-abrew-2026-10-19.md`).

mod commands;
mod help_texts;
mod tty;
use commands::*;

pub mod aliases;
pub mod api;
pub mod bundle;
pub mod cask;
pub mod census;
pub mod fetch;
pub mod formula;
pub mod install;
pub mod machine;
pub mod prefix;
pub mod services;
pub mod stub;
pub mod tap;
pub mod token_generator;
pub mod trust;

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

/// One non-empty path segment with no separator, NUL, `.` or `..`.
pub(crate) fn single_normal_segment(s: &str) -> bool {
    let mut parts = std::path::Path::new(s).components();
    !s.is_empty()
        && !s.contains(['/', '\\', '\0'])
        && matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && parts.next().is_none()
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
    VIA_REROUTE.with(|cell| cell.set(via_reroute));
    CURRENT_CMD.with(|cell| {
        *cell.borrow_mut() = args.first().cloned().unwrap_or_default();
    });
    CURRENT_ARGS.with(|cell| {
        *cell.borrow_mut() = args.to_vec();
    });
    PREAMBLE_DONE.with(|cell| cell.set(false));
    #[cfg(all(unix, not(test)))]
    {
        // Rust ignores SIGPIPE, which turns `abrew ... | head` into
        // a panic on EPIPE; restore the default disposition so the
        // process ends quietly with the signal, as brew does.
        // SAFETY: restoring a default signal disposition.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    if cfg!(windows) {
        eprintln!("abrew: Homebrew is macOS/Linux software; abrew does nothing on Windows");
        return ExitCode::from(1);
    }
    let prefix = Prefix::resolve();
    if let Some(w) = prefix::prefix_env_warning() {
        eprintln!("abrew: warning: {w}");
    }
    let args = expand_user_alias(&command_args(args));
    let args: &[String] = &args;
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

/// `brew --env` with abrew's own variables: brew's mode
/// selection (plain on a TTY, bash exports when piped,
/// `--shell=` to choose, `--plain` to force plain) over
/// the ABREW_/HOMEBREW_ prefix facts.
fn print_env(prefix: &Prefix, rest: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    // A bare trailing `--shell` is brew's missing-argument error.
    {
        let mut args = rest.iter().take_while(|a| a.as_str() != "--").peekable();
        while let Some(arg) = args.next() {
            if arg == "--shell" && args.peek().is_none() {
                return failed("Error: missing argument: --shell");
            }
        }
    }
    let names = operands_without_flag_values(rest, &["--shell"]);
    if !names.is_empty() {
        let api = api::Api::new(prefix.clone());
        for name in &names {
            if api.formula(name).is_err() {
                return failed(&format!(
                    "Error: {}",
                    commands::formula_unavailable(&api, name)
                ));
            }
        }
    }
    let shell = valued_flag_last(rest, "--shell");
    #[derive(PartialEq)]
    enum Mode {
        Plain,
        Bash,
        Fish,
        Csh,
    }
    let kind_of = |shell: &str| -> Mode {
        let base = shell.rsplit('/').next().unwrap_or(shell);
        let base = base.strip_prefix('-').unwrap_or(base);
        match base {
            "fish" => Mode::Fish,
            "csh" | "tcsh" => Mode::Csh,
            _ => Mode::Bash,
        }
    };
    let mode = if has_flag(rest, "--plain", "") {
        Mode::Plain
    } else {
        match shell.as_deref() {
            None if std::io::stdout().is_terminal() => Mode::Plain,
            None => Mode::Bash,
            Some("") => Mode::Plain,
            Some("auto") => {
                crate::caller_shell::invoking_shell().map_or(Mode::Bash, |shell| kind_of(&shell))
            }
            Some(shell) => kind_of(shell),
        }
    };
    let vars: Vec<(String, String)> = if via_reroute() {
        brew_build_env(prefix)
    } else {
        vec![
            (
                "ABREW_PREFIX".to_string(),
                prefix.root.display().to_string(),
            ),
            (
                "ABREW_CELLAR".to_string(),
                prefix.cellar().display().to_string(),
            ),
        ]
    };
    // Brew's plain dump drops the compiler duplicates.
    let keep_cc = vars_cc_differs(&vars);
    let plain_drop = mode == Mode::Plain && via_reroute() && !keep_cc;
    let vars: Vec<&(String, String)> = vars
        .iter()
        .filter(|(k, _)| !plain_drop || !matches!(k.as_str(), "CC" | "CXX" | "OBJC" | "OBJCXX"))
        .collect();
    let raw_ok = |value: &str| {
        !value.is_empty()
            && value.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '+')
            })
    };
    for (key, value) in vars {
        match mode {
            Mode::Plain => println!("{key}: {value}"),
            Mode::Bash => {
                if raw_ok(value) {
                    println!("export {key}={value}");
                } else {
                    println!("export {key}={}", crate::platform::sh_quote_str(value));
                }
            }
            Mode::Fish => {
                if raw_ok(value) {
                    println!("set -gx {key} {value}");
                } else {
                    let value = value
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('$', "\\$");
                    println!("set -gx {key} \"{value}\"");
                }
            }
            Mode::Csh => {
                if raw_ok(value) {
                    println!("setenv {key} {value};");
                } else {
                    println!("setenv {key} {};", crate::platform::sh_quote_str(value));
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Whether brew's plain `--env` dump keeps CC/CXX/OBJC/OBJCXX:
/// dropped only when CC equals HOMEBREW_CC.
fn vars_cc_differs(vars: &[(String, String)]) -> bool {
    let get = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    get("CC") != get("HOMEBREW_CC")
}

/// Brew's superenv build environment for `brew --env`, in
/// `BuildEnvironment::KEYS` order (macOS shape).
fn brew_build_env(prefix: &Prefix) -> Vec<(String, String)> {
    let root = prefix.root.display().to_string();
    let cc = std::env::var("HOMEBREW_CC")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "clang".to_string());
    let cxx = std::env::var("HOMEBREW_CXX")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| match cc.as_str() {
            "clang" => "clang++".to_string(),
            "gcc" => "g++".to_string(),
            "cc" => "c++".to_string(),
            other => format!("{other}++"),
        });
    let jobs = std::env::var("HOMEBREW_MAKE_JOBS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get)
                .to_string()
        });
    let dev = machine::DeveloperDir::this();
    let sdk = std::env::var("SDKROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dev.sdk_path().map(|p| p.display().to_string()));
    let git = std::env::var("HOMEBREW_GIT")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dev.tool("git").map(|p| p.display().to_string()));
    let shims = format!("{root}/Library/Homebrew/shims/mac/super");
    let mut vars: Vec<(String, String)> = vec![
        ("CC".into(), cc.clone()),
        ("CXX".into(), cxx.clone()),
        ("OBJC".into(), cc.clone()),
        ("OBJCXX".into(), cxx),
        ("HOMEBREW_CC".into(), cc),
        ("MAKEFLAGS".into(), format!("-j{jobs}")),
        ("CMAKE_PREFIX_PATH".into(), format!("{shims}:{root}")),
    ];
    if let Some(sdk) = &sdk {
        vars.push((
            "CMAKE_INCLUDE_PATH".into(),
            format!("{sdk}/System/Library/Frameworks/OpenGL.framework/Versions/Current/Headers"),
        ));
        vars.push((
            "CMAKE_LIBRARY_PATH".into(),
            format!("{sdk}/System/Library/Frameworks/OpenGL.framework/Versions/Current/Libraries"),
        ));
    }
    if let Some(major) = machine::macos_major() {
        vars.push((
            "PKG_CONFIG_LIBDIR".into(),
            format!("/usr/lib/pkgconfig:{root}/Library/Homebrew/os/mac/pkgconfig/{major}"),
        ));
    }
    vars.push(("HOMEBREW_MAKE_JOBS".into(), jobs));
    for proxy in [
        "all_proxy",
        "ftp_proxy",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ] {
        if let Ok(v) = std::env::var(proxy)
            && !v.is_empty()
        {
            vars.push((proxy.into(), v));
        }
    }
    if let Some(git) = git {
        vars.push(("HOMEBREW_GIT".into(), git));
    }
    if let Some(sdk) = sdk {
        vars.push(("HOMEBREW_SDKROOT".into(), sdk));
    }
    vars.push(("ACLOCAL_PATH".into(), format!("{root}/share/aclocal")));
    vars.push((
        "PATH".into(),
        format!("{shims}/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
    ));
    vars
}

fn unsupported(reason: &str) -> ExitCode {
    failed(&format!("abrew: unsupported native operation: {reason}"))
}

fn not_native(_prefix: &Prefix, _args: &[String], cmd: &str) -> ExitCode {
    unsupported(&format!("'{cmd}' is not native"))
}

fn failed(msg: &str) -> ExitCode {
    usage_preamble(msg);
    eprintln!("{}", tty::error_line(msg));
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

thread_local! {
    /// Whether this process was entered through the `brew` reroute
    /// (`atpkg __abrew`, what the stub execs) rather than as `abrew`.
    /// A CLI process has exactly one entry mode; `run` records it.
    static VIA_REROUTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The command being run, for brew's usage-error help preamble.
    static CURRENT_CMD: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// The full argv, for picking a subcommand's preamble.
    static CURRENT_ARGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Whether the usage preamble already printed this run.
    static PREAMBLE_DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn via_reroute() -> bool {
    VIA_REROUTE.with(std::cell::Cell::get)
}

/// The canonical command a help topic or argv0 names, resolving
/// brew's command aliases (as `Commands.path` does).
fn canonical_help_command(cmd: &str) -> &str {
    let cmd = match cmd {
        "instal" => "install",
        "uninstal" => "uninstall",
        "abv" => "info",
        "up" => "update",
        "environment" => "--env",
        "--config" => "config",
        "post_install" => "postinstall",
        other => other,
    };
    help_texts::ALIASES
        .iter()
        .find(|(alias, _)| *alias == cmd)
        .map_or(cmd, |(_, target)| target)
}

/// `brew command <cmd>…`: the file used when invoking each
/// command. Abrew's commands live in this executable, so a
/// known command prints its path; a tap's `cmd/brew-<name>`
/// file prints its own path; anything else is brew's
/// unknown-command error (earlier paths already printed).
fn cmd_command_path(prefix: &Prefix, rest: &[String]) -> ExitCode {
    for name in operands(rest) {
        let canonical = canonical_help_command(&name);
        let known = command_names(true).contains(&name.as_str())
            || command_names(true).contains(&canonical);
        if known {
            match std::env::current_exe() {
                Ok(path) => println!("{}", path.display()),
                Err(error) => return failed(&format!("abrew: {error}")),
            }
            continue;
        }
        let mut found: Option<std::path::PathBuf> = None;
        for tap in tap::list(prefix) {
            for file in [
                format!("brew-{name}.rb"),
                format!("brew-{name}.sh"),
                format!("brew-{name}"),
            ] {
                let candidate = tap.path.join("cmd").join(file);
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        match found {
            Some(path) => println!("{}", path.display()),
            None => {
                return from_install(
                    prefix,
                    &[],
                    Err(InstallError::Failed(format!(
                        "Error: Unknown command: brew {name}"
                    ))),
                );
            }
        }
    }
    ExitCode::SUCCESS
}

/// Brew's help text for a command, or the top-level help message
/// when the command has no help of its own (brew's fallback).
fn brew_help_text(cmd: &str) -> &'static str {
    let cmd = canonical_help_command(cmd);
    help_texts::TEXTS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map_or(help_texts::TOP, |(_, text)| text)
}

/// The help brew prints ahead of a usage error: subcommand
/// parents (bundle, services) print their full parser help,
/// which differs from the topic help; a subcommand-scope
/// error prints that subcommand's help, and an unknown
/// subcommand prints the topic help.
fn brew_preamble_text(cmd: &str, message: &str) -> &'static str {
    let cmd = canonical_help_command(cmd);
    if message.contains("unknown subcommand:") {
        return brew_help_text(cmd);
    }
    // Subcommand-parser errors (scope and depends_on forms)
    // print the resolved subcommand's help; token-level
    // errors print the parent's.
    let sub_named = message
        .strip_prefix("Error: Invalid usage: The `")
        .and_then(|rest| rest.split('`').next());
    let sub_from_argv = if message.starts_with("Error: Invalid usage: `--") {
        CURRENT_ARGS.with(|cell| {
            let argv = cell.borrow();
            let mut it = argv.iter().skip(1).take_while(|a| a.as_str() != "--");
            let mut found = None;
            while let Some(arg) = it.next() {
                if arg == "--file" {
                    it.next();
                } else if !arg.starts_with('-') {
                    found = Some(match arg.as_str() {
                        "upgrade" => "install".to_string(),
                        "ls" => "list".to_string(),
                        "i" => "info".to_string(),
                        other => other.to_string(),
                    });
                    break;
                }
            }
            found
        })
    } else {
        None
    };
    if let Some(sub) = sub_named.map(str::to_string).or(sub_from_argv) {
        let dotted = format!("{cmd}.{sub}");
        if let Some((_, text)) = help_texts::PREAMBLES
            .iter()
            .find(|(name, _)| *name == dotted)
        {
            return text;
        }
    }
    help_texts::PREAMBLES
        .iter()
        .find(|(name, _)| *name == cmd)
        .map_or_else(|| brew_help_text(cmd), |(_, text)| text)
}

/// Brew prints the command's help before a usage-class error
/// under the reroute: `Invalid usage:` errors get a blank line
/// after the help, OptionParser invalid/ambiguous-option
/// errors do not, and other parser errors (missing/needless
/// argument) get no help at all. Printed once per run.
pub(super) fn usage_preamble_for(cmd: &str, message: &str) {
    if !via_reroute() || PREAMBLE_DONE.with(std::cell::Cell::get) {
        return;
    }
    let blank = if message.starts_with("Error: Invalid usage:") {
        true
    } else if message.starts_with("Error: invalid option:")
        || message.starts_with("Error: ambiguous option:")
    {
        false
    } else {
        return;
    };
    PREAMBLE_DONE.with(|cell| cell.set(true));
    eprint!("{}", brew_preamble_text(cmd, message));
    if blank {
        eprintln!();
    }
}

/// The usage preamble for the command this run is executing.
pub(super) fn usage_preamble(message: &str) {
    let cmd = CURRENT_CMD.with(|cell| cell.borrow().clone());
    usage_preamble_for(&cmd, message);
}

/// A failed() for `Error: Invalid usage:` messages, carrying
/// brew's help preamble under the reroute.
pub(super) fn usage_failed(cmd: &str, message: &str) -> ExitCode {
    usage_preamble_for(cmd, message);
    failed(message)
}

fn from_install(_prefix: &Prefix, _args: &[String], r: Result<(), InstallError>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(InstallError::Failed(m)) => failed(&m),
        Err(InstallError::Unsupported(reason)) => unsupported(&reason),
        Err(InstallError::Aborted) => ExitCode::from(1),
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
    let mut out = Vec::with_capacity(rest.len());
    let mut literal = false;
    for s in rest {
        if literal {
            out.push(s.clone());
            continue;
        }
        match s.as_str() {
            "--" => {
                literal = true;
                out.push(s.clone());
            }
            "--formulae" => out.push("--formula".to_string()),
            "--casks" => out.push("--cask".to_string()),
            _ => {
                // OptionParser bundles boolean short switches (-vq,
                // -1l); admit-checking then validates each one, so
                // an undefined bundled letter is refused downstream
                // exactly where brew's parser refuses it.
                let short = s.strip_prefix('-').filter(|t| {
                    !t.starts_with('-')
                        && t.chars().count() > 1
                        && t.chars().all(|c| c.is_ascii_alphanumeric())
                });
                match short {
                    Some(t) => out.extend(t.chars().map(|c| format!("-{c}"))),
                    None => out.push(s.clone()),
                }
            }
        }
    }
    out
}

/// Brew's named-argument type phrase for the Invalid usage
/// count errors ("formula or cask", "installed formula or
/// installed cask", …), from each command's `named_args`.
fn named_arg_types(cmd: &str) -> String {
    let types: &[&str] = match cmd {
        "install" | "reinstall" | "fetch" | "cat" | "log" | "deps" | "info" | "home"
        | "homepage" | "missing" | "outdated" | "cleanup" | "unpack" => &["formula", "cask"],
        "uninstall" | "remove" | "rm" | "link" | "ln" | "unlink" | "list" | "ls" | "upgrade"
        | "tab" | "migrate" => &["installed formula", "installed cask"],
        "search" | "-S" => &["text or regex"],
        "desc" => &["formula", "cask", "text or regex"],
        "uses" | "dependents" | "options" => &["formula"],
        "zap" => &["cask"],
        "command" => &["command"],
        "formula" => &["formula"],
        "unalias" => &["alias"],
        "untap" | "tap" | "tap-info" => &["tap"],
        _ => &["named"],
    };
    match types {
        [] => "named".to_string(),
        [only] => (*only).to_string(),
        [.., last] => format!("{} or {last}", types[..types.len() - 1].join(", ")),
    }
}

/// Commands brew.sh answers itself, before Ruby's help
/// interception and argument parser ever run.
/// User aliases (`brew alias`) expand before dispatch when
/// the typed command is not a real command: brew would run
/// the alias script, abrew re-dispatches its plain
/// `brew <words>` form natively (bounded, like a shell would
/// eventually be by exec depth).
fn expand_user_alias(args: &[String]) -> Vec<String> {
    let mut args = args.to_vec();
    for _ in 0..8 {
        let Some(first) = args.first() else { break };
        if command_names(true).contains(&first.as_str())
            || COMMANDS.contains(&first.as_str())
            || matches!(first.as_str(), "on" | "off" | "status")
        {
            break;
        }
        let Some(mut words) = aliases::expansion(first) else {
            break;
        };
        if words.is_empty() {
            break;
        }
        words.extend(args.drain(1..));
        args = words;
    }
    args
}

fn shell_fast_path(cmd: &str) -> bool {
    matches!(
        cmd,
        "shellenv" | "formulae" | "casks" | "--taps" | "--repository" | "--repo"
    )
}

fn native_args(
    cmd: &str,
    rest: &[String],
) -> Result<(Option<Vec<bundle::BrewfileEntry>>, Vec<String>), InstallError> {
    let typed_rest = rest;
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    // Brew warns on its deprecated wrapper envs, then reads
    // the disabled internal-API env while fetching API files;
    // both precede the command's parser.
    if reaches_ruby_startup(cmd, rest)
        && let Some(message) =
            deprecated_env_warnings(WRAPPER_DEPRECATED_ENVS, |key| std::env::var(key).ok())
    {
        return Err(InstallError::Failed(message));
    }
    if let Some(message) = internal_api_env_error(cmd, rest, |key| std::env::var(key).ok()) {
        return Err(InstallError::Failed(message));
    }
    // Brew's shell fast paths (brew.sh handles these before
    // Ruby and its parser): every argument is positional data
    // or ignored outright, `--help` included.
    if shell_fast_path(cmd) {
        return Ok((None, typed_rest.to_vec()));
    }
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
        ];
        if COMMANDS.contains(&cmd)
            || UNSUPPORTED_KNOWN.contains(&cmd)
            || BREW_BUILTIN_COMMANDS.contains(&cmd)
            || BREW_DEVELOPER_COMMANDS.contains(&cmd)
        {
            InstallError::Unsupported(format!("'{cmd}' is not native"))
        } else {
            let dictionary: Vec<String> = command_names(true)
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            InstallError::Failed(format!(
                "Error: Invalid usage: Unknown command: brew {cmd}{}",
                commands::command_suggestion(cmd, &dictionary)
            ))
        }
    };
    // Brew's disabled switches (`odisabled` in each command's
    // parser): usage errors in brew's exact wording, before any
    // command work. `--ask` is one (ask mode is the default).
    // Direct-dialect commands brew does not have are unknown
    // commands under the reroute.
    if via_reroute() && matches!(cmd, "zap" | "switch") {
        let dictionary: Vec<String> = command_names(true)
            .iter()
            .filter(|name| !matches!(**name, "zap" | "switch"))
            .map(|s| (*s).to_string())
            .collect();
        return Err(InstallError::Failed(format!(
            "Error: Invalid usage: Unknown command: brew {cmd}{}",
            commands::command_suggestion(cmd, &dictionary)
        )));
    }
    let disabled_cmd = match cmd {
        "ls" => "list",
        "-S" => "search",
        "up" => "update",
        "abv" => "info",
        "instal" => "install",
        other => other,
    };
    if let Some(message) = disabled_env_error(disabled_cmd, |key| std::env::var(key).ok()) {
        return Err(InstallError::Failed(message));
    }
    if disabled_cmd == "bundle"
        && let Some(message) =
            deprecated_env_warnings(BUNDLE_DEPRECATED_ENVS, |key| std::env::var(key).ok())
    {
        return Err(InstallError::Failed(message));
    }
    const TRUSTED_TAP: Option<&str> = Some("the default trusted-tap behaviour");
    const NO_REPLACEMENT: Option<&str> = None;
    // Tuples are (display name, kind, replacement, alternate
    // spelling that also counts as present).
    let disabled: &[(&str, &str, Option<&str>, &str)] = match disabled_cmd {
        "install" | "upgrade" | "reinstall" => {
            &[("--ask", "switch", Some("the default behaviour"), "")]
        }
        "deps"
        | "desc"
        | "readall"
        | "search"
        | "uses"
        | "bump"
        | "livecheck"
        | "unbottled"
        | "determine-test-runners" => &[("--eval-all", "switch", TRUSTED_TAP, "")],
        "audit" => &[
            ("--eval-all", "switch", TRUSTED_TAP, ""),
            ("--[no-]signing", "switch", NO_REPLACEMENT, ""),
        ],
        "options" => &[
            ("--eval-all", "switch", TRUSTED_TAP, ""),
            ("--command", "flag", NO_REPLACEMENT, ""),
        ],
        "info" => &[
            ("--eval-all", "switch", TRUSTED_TAP, ""),
            ("--fetch-manifest", "switch", NO_REPLACEMENT, ""),
        ],
        "tap" => &[(
            "--eval-all",
            "switch",
            Some("the default tap verification behaviour"),
            "",
        )],
        "list" => &[(
            "--installed-as-dependency",
            "switch",
            Some("--no-installed-on-request"),
            "",
        )],
        "update" => &[("--merge", "switch", NO_REPLACEMENT, "")],
        "gist-logs" => &[("--with-hostname", "switch", NO_REPLACEMENT, "")],
        "bundle" => &[("--describe", "switch", Some("the default behaviour"), "")],
        "bottle" | "pr-pull" | "pr-upload" => &[("--committer", "flag", NO_REPLACEMENT, "")],
        "irb" => &[(
            "--pry",
            "switch",
            Some("the default IRB backend (Pry is largely unmaintained upstream)"),
            "",
        )],
        "tap-new" => &[("--pull-label", "flag", NO_REPLACEMENT, "")],
        "test-bot" => &[(
            "--[no-]skip-recursive-dependents",
            "switch",
            Some("the default behaviour"),
            "",
        )],
        "update-perl-resources" | "update-python-resources" => {
            &[("-s", "switch", NO_REPLACEMENT, "--silent")]
        }
        _ => &[],
    };
    for (name, kind, replacement, alt) in disabled {
        let present = if *kind == "flag" {
            has_flag_eq(rest, name)
        } else if let Some(base) = name.strip_prefix("--[no-]") {
            // Brew names the declared `--[no-]x` form whichever
            // spelling was passed.
            has_flag(rest, &format!("--{base}"), "") || has_flag(rest, &format!("--no-{base}"), "")
        } else {
            has_flag(rest, name, alt)
        };
        if present {
            let tail = match replacement {
                Some(replacement) => format!("Use {replacement} instead."),
                None => "There is no replacement.".to_string(),
            };
            return Err(InstallError::Failed(format!(
                "Error: Calling the `{name}` {kind} is disabled! {tail}"
            )));
        }
    }
    if disabled_cmd == "install" && has_flag_eq(rest, "--env") {
        return Err(InstallError::Failed(
            "Error: Calling `brew install --env` is disabled! \
             Use `env :std` in specific formula files instead."
                .to_string(),
        ));
    }
    // Flags that take values, from each command's own `flag`
    // declarations (used by the missing/needless-argument
    // parse errors).
    let value_flags: &[&str] = match cmd {
        "info" => &["--days", "--category", "--json"],
        "cleanup" => &["--prune"],
        "outdated" | "upgrade" => &["--minimum-version", "--min-version", "--severity", "--json"],
        "deps" => &["--os", "--arch", "--brewfile"],
        "fetch" | "--cache" => &["--os", "--arch", "--bottle-tag"],
        "unpack" => &["--destdir"],
        "readall" => &["--os", "--arch"],
        "install" | "reinstall" => &["--bottle-arch", "--cc", "--env"],
        "missing" => &["--hide"],
        "services" => &["--file", "--max-wait", "--sudo-service-user"],
        "options" => &["--command"],
        "log" => &["--max-count", "-n"],
        "exec" => &["--sandbox"],
        "vulns" => &["--fix-type"],
        "--env" | "env" => &["--shell"],
        "tap-info" => &["--json"],
        _ => &[],
    };
    // A required-value flag at the end of the option list is
    // OptionParser's "missing argument" error, raised while
    // parsing (ahead of the constraint checks below). --json
    // and --brewfile take optional values, so they are exempt.
    {
        let pre: Vec<&String> = rest.iter().take_while(|arg| arg.as_str() != "--").collect();
        if let Some(last) = pre.last()
            && value_flags.contains(&last.as_str())
            && !matches!(last.as_str(), "--json" | "--brewfile")
        {
            return Err(InstallError::Failed(format!(
                "Error: missing argument: {last}"
            )));
        }
    }
    // Brew's parser `conflicts` groups beyond the formula/cask
    // kind pair (handled below) and the per-command pairs
    // already encoded further down. Members are (display name,
    // alternate spelling); brew lists every present member in
    // declaration order when two or more appear.
    type ConflictMember = (&'static str, &'static [&'static str]);
    const fn m(name: &'static str) -> ConflictMember {
        (name, &[])
    }
    let conflict_groups: &[&[ConflictMember]] = match disabled_cmd {
        "outdated" => &[
            &[("--quiet", &["-q"]), ("--verbose", &["-v"]), m("--json")],
            &[m("--vulnerable"), m("--cask")],
        ],
        "leaves" => &[&[m("--installed-on-request"), m("--installed-as-dependency")]],
        "trust" | "untrust" => &[&[
            ("--tap", &["--taps"]),
            ("--formula", &["--formulae"]),
            ("--cask", &["--casks"]),
            ("--command", &["--commands"]),
        ]],
        "info" => &[
            &[m("--sizes"), m("--installed")],
            &[m("--sizes"), m("--eval-all")],
        ],
        "desc" => &[&[
            ("--search", &["-s"]),
            ("--name", &["-n"]),
            ("--description", &["-d"]),
        ]],
        "search" => &[
            &[m("--desc"), m("--pull-request")],
            &[m("--open"), m("--closed")],
        ],
        "log" => &[&[m("-1"), ("--max-count", &["-n"])]],
        "install" => &[
            &[m("--ignore-dependencies"), m("--only-dependencies")],
            &[m("--adopt"), ("--force", &["-f"])],
        ],
        "upgrade" => &[&[m("--vulnerable"), m("--cask")]],
        "fetch" => &[
            &[
                ("--build-from-source", &["-s"]),
                m("--build-bottle"),
                m("--force-bottle"),
                m("--bottle-tag"),
            ],
            &[m("--os"), m("--bottle-tag")],
            &[m("--arch"), m("--bottle-tag")],
            &[m("--all-platforms"), m("--os")],
            &[m("--all-platforms"), m("--arch")],
            &[m("--all-platforms"), m("--bottle-tag")],
        ],
        "services" => &[
            &[m("--all"), m("--file")],
            &[m("--max-wait"), m("--no-wait")],
        ],
        "vulns" => &[&[
            m("--fix-available"),
            m("--no-fix-available"),
            m("--fix-type"),
        ]],
        "--cache" => &[
            &[
                ("--build-from-source", &["-s"]),
                m("--force-bottle"),
                m("--bottle-tag"),
                m("--HEAD"),
                m("--cask"),
            ],
            &[m("--os"), m("--bottle-tag")],
            &[m("--arch"), m("--bottle-tag")],
        ],
        "--prefix" => &[&[m("--unbrewed"), m("--installed")]],
        "unpack" => &[
            &[("--git", &["-g"]), m("--patch")],
            &[("--formula", &["--formulae"]), ("--cask", &["--casks"])],
            &[("--cask", &["--casks"]), m("--patch")],
            &[("--cask", &["--casks"]), ("--git", &["-g"])],
        ],
        _ => &[],
    };
    for group in conflict_groups {
        // Brew declares the formula/cask pair before the
        // vulnerable/cask pair, so with all three present the
        // kind pair reports first (its own check below is
        // shadowed by --vulnerable's native refusal).
        if group.first().map(|(name, _)| *name) == Some("--vulnerable")
            && has_flag(rest, "--formula", "")
            && has_flag(rest, "--cask", "")
        {
            let message = if disabled_cmd == "upgrade" {
                "Error: Invalid usage: Options --cask and --formulae are mutually exclusive."
            } else {
                "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
            };
            return Err(InstallError::Failed(message.to_string()));
        }
        let present: Vec<&str> = group
            .iter()
            .filter(|(name, alts)| {
                has_flag_eq(rest, name) || alts.iter().any(|alt| has_flag_eq(rest, alt))
            })
            .map(|(name, _)| *name)
            .collect();
        if present.len() > 1 {
            return Err(InstallError::Failed(format!(
                "Error: Invalid usage: Options {} are mutually exclusive.",
                present.join(" and ")
            )));
        }
    }
    // Brew's parser `depends_on` constraints not already
    // encoded per command below: the dependent switch cannot
    // be passed without its requirement.
    let depends_on: &[(&str, &str)] = match disabled_cmd {
        "deps" => &[("--prune", "--tree")],
        "info" => &[
            ("--days", "--analytics"),
            ("--category", "--analytics"),
            ("--variations", "--json"),
        ],
        "outdated" | "upgrade" => &[("--severity", "--vulnerable")],
        "search" => &[("--open", "--pull-request"), ("--closed", "--pull-request")],
        _ => &[],
    };
    // Evaluated again after abbreviation expansion below:
    // brew's parser expands first, so `--prun` is `--prune`
    // to this gate too.
    let check_depends_on = |rest: &[String]| -> Result<(), InstallError> {
        for (switch, requires) in depends_on {
            if has_flag_eq(rest, switch) && !has_flag_eq(rest, requires) {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: `{switch}` cannot be passed without `{requires}`."
                )));
            }
        }
        Ok(())
    };
    check_depends_on(rest)?;
    if cmd == "services" {
        // Brew's services subcommands resolve like bundle's:
        // unknown names and out-of-scope switches are parser
        // errors ahead of the (delegated) execution.
        const SERVICE_SUBS: &[&str] = &[
            "cleanup", "i", "info", "kill", "list", "ls", "restart", "run", "start", "stop",
        ];
        const SERVICE_SCOPED: &[(&str, &str, &[&str])] = &[
            (
                "--all",
                "switch",
                &["info", "kill", "restart", "run", "start", "stop"],
            ),
            ("--json", "switch", &["info", "list"]),
            ("--file", "flag", &["restart", "run", "start"]),
            ("--max-wait", "flag", &["stop"]),
            ("--no-wait", "switch", &["stop"]),
            ("--keep", "switch", &["stop"]),
        ];
        // Token-level parser errors fire left-to-right ahead
        // of subcommand resolution (brew OptionParser).
        const SERVICE_SWITCHES: &[&str] = &["--all", "--json", "--no-wait", "--keep"];
        const SERVICE_FLAGS: &[&str] = &["--file", "--max-wait", "--sudo-service-user"];
        {
            let mut args = rest
                .iter()
                .take_while(|arg| arg.as_str() != "--")
                .peekable();
            while let Some(arg) = args.next() {
                if !arg.starts_with('-') || arg.as_str() == "-" {
                    continue;
                }
                let (name, has_value) = match arg.split_once('=') {
                    Some((name, _)) => (name, true),
                    None => (arg.as_str(), false),
                };
                if SERVICE_SWITCHES.contains(&name) {
                    if has_value {
                        return Err(InstallError::Failed(format!(
                            "Error: needless argument: {arg}"
                        )));
                    }
                } else if SERVICE_FLAGS.contains(&name) {
                    if !has_value && args.next_if(|_| true).is_none() {
                        return Err(InstallError::Failed(format!(
                            "Error: missing argument: {name}"
                        )));
                    }
                } else if !matches!(
                    arg.as_str(),
                    "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d"
                ) {
                    return Err(InstallError::Failed(format!(
                        "Error: invalid option: {arg}"
                    )));
                }
            }
        }
        let mut first_positional = None;
        {
            let mut args = rest.iter().take_while(|arg| arg.as_str() != "--");
            while let Some(arg) = args.next() {
                if matches!(
                    arg.as_str(),
                    "--file" | "--max-wait" | "--sudo-service-user"
                ) {
                    args.next();
                } else if !arg.starts_with('-') {
                    first_positional = Some(arg.as_str());
                    break;
                }
            }
        }
        if let Some(named) = first_positional
            && !SERVICE_SUBS.contains(&named)
        {
            return Err(InstallError::Failed(format!(
                "Error: Invalid usage: unknown subcommand: `{named}`"
            )));
        }
        let subcommand = match first_positional {
            Some("ls") | None => "list",
            Some("i") => "info",
            Some(other) => other,
        };
        for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
            let name = arg.split('=').next().unwrap_or(arg);
            if let Some((display, kind, subs)) = SERVICE_SCOPED
                .iter()
                .find(|(display, _, _)| *display == name)
                && !subs.contains(&subcommand)
            {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: The `{subcommand}` subcommand does not accept \
                     the `{display}` {kind}."
                )));
            }
        }
        // Named arguments after the subcommand (flag values
        // skipped; everything after `--` is named too).
        let mut named: Vec<&str> = Vec::new();
        {
            let mut seen_sub = first_positional.is_none();
            let mut literal = false;
            let mut args = rest.iter();
            while let Some(arg) = args.next() {
                if literal {
                    if seen_sub {
                        named.push(arg);
                    } else {
                        seen_sub = true;
                    }
                } else if arg == "--" {
                    literal = true;
                } else if matches!(
                    arg.as_str(),
                    "--file" | "--max-wait" | "--sudo-service-user"
                ) {
                    args.next();
                } else if !arg.starts_with('-') {
                    if seen_sub {
                        named.push(arg);
                    } else {
                        seen_sub = true;
                    }
                }
            }
        }
        if matches!(subcommand, "list" | "cleanup") && !named.is_empty() {
            return Err(InstallError::Failed(
                "Error: Invalid usage: This command does not take named arguments.".to_string(),
            ));
        }
        if matches!(
            subcommand,
            "info" | "kill" | "restart" | "run" | "start" | "stop"
        ) && named.is_empty()
            && !has_flag_eq(rest, "--all")
        {
            return Err(InstallError::Failed(
                "Error: Invalid usage: Formula(e) missing, please provide a formula name or use `--all`."
                    .to_string(),
            ));
        }
        // Only the read-only listing is native; service
        // control refuses here, ahead of any prefix effect.
        if !matches!(subcommand, "list" | "info") {
            return Err(InstallError::Unsupported(
                "'services' is not native".to_string(),
            ));
        }
    }
    if cmd == "trust" {
        for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
            if arg == "--json" {
                return Err(InstallError::Failed(
                    "Error: missing argument: --json".to_owned(),
                ));
            }
            if let Some(version) = arg.strip_prefix("--json=")
                && version != "v1"
            {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: invalid JSON version: {version}"
                )));
            }
        }
    }
    if cmd == "bundle" {
        // Token-level OptionParser errors first: a switch brew's
        // bundle parser never declared is invalid (or ambiguous
        // as a prefix), ahead of conflicts and scope checks.
        {
            const BUNDLE_LONGS: &[&str] = &[
                "all",
                "brews",
                "cask",
                "casks",
                "check",
                "cleanup",
                "deny-network",
                "describe",
                "file",
                "force",
                "force-cleanup",
                "formula",
                "formulae",
                "global",
                "help",
                "install",
                "jobs",
                "no-brews",
                "no-cask",
                "no-casks",
                "no-cleanup-brew",
                "no-cleanup-cask",
                "no-cleanup-tap",
                "no-describe",
                "no-dump-brew",
                "no-dump-cask",
                "no-dump-tap",
                "no-formula",
                "no-formulae",
                "no-restart",
                "no-secrets",
                "no-tap",
                "no-taps",
                "no-upgrade",
                "quiet",
                "sandbox",
                "services",
                "tap",
                "taps",
                "upgrade",
                "upgrade-formula",
                "upgrade-formulae",
                "verbose",
                "debug",
                "zap",
            ];
            const BUNDLE_EXTS: &[&str] = &[
                "mas", "vscode", "go", "cargo", "uv", "flatpak", "winget", "krew", "npm",
            ];
            let known_long = |name: &str| -> Option<bool> {
                // Some(true): known; Some(false): ambiguous prefix; None: unknown.
                let mut generated: Vec<String> = Vec::new();
                for ext in BUNDLE_EXTS {
                    generated.push((*ext).to_string());
                    generated.push(format!("no-{ext}"));
                    generated.push(format!("no-cleanup-{ext}"));
                    generated.push(format!("no-dump-{ext}"));
                }
                let all = BUNDLE_LONGS
                    .iter()
                    .map(|s| (*s).to_string())
                    .chain(generated);
                let mut prefix_matches = 0usize;
                for known in all {
                    if known == name {
                        return Some(true);
                    }
                    if known.starts_with(name) {
                        prefix_matches += 1;
                    }
                }
                match prefix_matches {
                    0 => None,
                    1 => Some(true),
                    _ => Some(false),
                }
            };
            let mut seen_sub = false;
            let mut exec_command_seen = false;
            let mut args = rest.iter().take_while(|a| a.as_str() != "--").peekable();
            let mut subcommand: Option<&str> = None;
            while let Some(arg) = args.next() {
                if exec_command_seen {
                    break;
                }
                if !arg.starts_with('-') || arg.as_str() == "-" {
                    if !seen_sub {
                        seen_sub = true;
                        subcommand = Some(arg.as_str());
                    } else if subcommand == Some("exec") {
                        exec_command_seen = true;
                    }
                    continue;
                }
                if matches!(
                    arg.as_str(),
                    "--file" | "--jobs" | "--upgrade-formulae" | "--upgrade-formula"
                ) {
                    args.next();
                    continue;
                }
                if let Some(long) = arg.strip_prefix("--") {
                    let name = long.split('=').next().unwrap_or(long);
                    match known_long(name) {
                        Some(true) => {}
                        Some(false) => {
                            return Err(InstallError::Failed(format!(
                                "Error: ambiguous option: --{name}"
                            )));
                        }
                        None => {
                            return Err(InstallError::Failed(format!(
                                "Error: invalid option: {arg}"
                            )));
                        }
                    }
                } else if arg.len() > 1
                    && !arg[1..]
                        .chars()
                        .all(|c| matches!(c, 'f' | 'g' | 'v' | 'q' | 'd' | 'h'))
                {
                    return Err(InstallError::Failed(format!(
                        "Error: invalid option: {arg}"
                    )));
                }
            }
        }
        // Brew's bundle type toggles: each enabled spelling
        // conflicts with the disabled, no-cleanup and no-dump
        // forms, registered in type order (extensions in
        // registration order), with --file/--global last.
        let present_any = |spellings: &[&str]| spellings.iter().any(|s| has_flag_eq(rest, s));
        let conflict = |a: &str, b: &str| {
            Err(InstallError::Failed(format!(
                "Error: Invalid usage: Options {a} and {b} are mutually exclusive."
            )))
        };
        // (display, spellings, type slug for cleanup/dump forms)
        type BundleToggle = (
            &'static str,
            &'static [&'static str],
            &'static str,
            &'static [&'static str],
            &'static str,
        );
        const BUILTINS: &[BundleToggle] = &[
            (
                "--formula",
                &["--formula", "--formulae", "--brews"],
                "--no-formula",
                &["--no-formula", "--no-formulae", "--no-brews"],
                "brew",
            ),
            (
                "--cask",
                &["--cask", "--casks"],
                "--no-cask",
                &["--no-cask", "--no-casks"],
                "cask",
            ),
            (
                "--tap",
                &["--tap", "--taps"],
                "--no-tap",
                &["--no-tap", "--no-taps"],
                "tap",
            ),
        ];
        for (enabled, enabled_spellings, disabled, disabled_spellings, slug) in BUILTINS {
            if present_any(enabled_spellings) && present_any(disabled_spellings) {
                return conflict(enabled, disabled);
            }
            for variant in ["--no-cleanup-", "--no-dump-"] {
                let other = format!("{variant}{slug}");
                if present_any(enabled_spellings) && has_flag_eq(rest, &other) {
                    return conflict(enabled, &other);
                }
            }
        }
        for ext in [
            "mas", "vscode", "go", "cargo", "uv", "flatpak", "winget", "krew", "npm",
        ] {
            let enabled = format!("--{ext}");
            if !has_flag_eq(rest, &enabled) {
                continue;
            }
            for variant in ["--no-", "--no-cleanup-", "--no-dump-"] {
                let other = format!("{variant}{ext}");
                if has_flag_eq(rest, &other) {
                    return conflict(&enabled, &other);
                }
            }
        }
        if has_flag_eq(rest, "--file") && has_flag(rest, "--global", "-g") {
            return conflict("--file", "--global");
        }
        // Brew's subcommand scope check: a switch declared for
        // other bundle subcommands is a usage error naming the
        // chosen subcommand (default `install`) and the switch's
        // first declared spelling. Entries are (display, kind,
        // spellings, accepting subcommands); shared switches
        // (--file/--global, globals) accept everywhere. Tokens
        // are scanned in argv order; exec stops at its command.
        const TYPE_SUBS: &[&str] = &["add", "cleanup", "dump", "list", "remove"];
        type BundleScope = (
            &'static str,
            &'static str,
            &'static [&'static str],
            &'static [&'static str],
        );
        const SCOPED: &[BundleScope] = &[
            (
                "--install",
                "switch",
                &["--install"],
                &[
                    "add", "check", "cleanup", "dump", "edit", "env", "exec", "list", "remove",
                    "sh",
                ],
            ),
            (
                "--no-upgrade",
                "switch",
                &["--no-upgrade"],
                &["check", "install"],
            ),
            ("--upgrade", "switch", &["--upgrade"], &["install"]),
            (
                "--upgrade-formulae",
                "flag",
                &["--upgrade-formulae", "--upgrade-formula"],
                &["install"],
            ),
            ("--jobs", "flag", &["--jobs"], &["install"]),
            (
                "-f",
                "switch",
                &["-f", "--force"],
                &["cleanup", "dump", "install"],
            ),
            ("--cleanup", "switch", &["--cleanup"], &["install"]),
            (
                "--force-cleanup",
                "switch",
                &["--force-cleanup"],
                &["install"],
            ),
            ("--overwrite", "switch", &["--overwrite"], &["install"]),
            ("--zap", "switch", &["--zap"], &["cleanup", "install"]),
            ("--all", "switch", &["--all"], &["cleanup", "list"]),
            (
                "--formula",
                "switch",
                &["--formula", "--formulae", "--brews"],
                TYPE_SUBS,
            ),
            ("--cask", "switch", &["--cask", "--casks"], TYPE_SUBS),
            ("--tap", "switch", &["--tap", "--taps"], TYPE_SUBS),
            (
                "--no-formula",
                "switch",
                &["--no-formula", "--no-formulae", "--no-brews"],
                &["cleanup", "dump"],
            ),
            (
                "--no-cask",
                "switch",
                &["--no-cask", "--no-casks"],
                &["cleanup", "dump"],
            ),
            (
                "--no-tap",
                "switch",
                &["--no-tap", "--no-taps"],
                &["cleanup", "dump"],
            ),
            (
                "--no-cleanup-brew",
                "switch",
                &["--no-cleanup-brew"],
                &["cleanup"],
            ),
            (
                "--no-cleanup-cask",
                "switch",
                &["--no-cleanup-cask"],
                &["cleanup"],
            ),
            (
                "--no-cleanup-tap",
                "switch",
                &["--no-cleanup-tap"],
                &["cleanup"],
            ),
            ("--no-dump-brew", "switch", &["--no-dump-brew"], &["dump"]),
            ("--no-dump-cask", "switch", &["--no-dump-cask"], &["dump"]),
            ("--no-dump-tap", "switch", &["--no-dump-tap"], &["dump"]),
            (
                "--no-describe",
                "switch",
                &["--no-describe"],
                &["add", "dump"],
            ),
            ("--no-restart", "switch", &["--no-restart"], &["dump"]),
            ("--check", "switch", &["--check"], &["env", "exec", "sh"]),
            (
                "--no-secrets",
                "switch",
                &["--no-secrets"],
                &["env", "exec", "sh"],
            ),
            ("--services", "switch", &["--services"], &["exec", "sh"]),
            ("--sandbox", "flag", &["--sandbox"], &["exec"]),
            ("--deny-network", "switch", &["--deny-network"], &["exec"]),
        ];
        const SUBCOMMANDS: &[&str] = &[
            "add", "check", "cleanup", "dump", "edit", "env", "exec", "install", "list", "remove",
            "sh", "upgrade",
        ];
        // The first positional names the subcommand; --file's
        // separate value is not positional.
        let mut first_positional = None;
        {
            let mut args = rest.iter().take_while(|arg| arg.as_str() != "--");
            while let Some(arg) = args.next() {
                if arg == "--file" {
                    args.next();
                } else if !arg.starts_with('-') {
                    first_positional = Some(arg.as_str());
                    break;
                }
            }
        }
        if let Some(named) = first_positional
            && !SUBCOMMANDS.contains(&named)
            // `status` inspects the legacy bundled Homebrew; it is
            // an abrew-native extension, unknown under the reroute.
            && !(named == "status" && !via_reroute())
        {
            return Err(InstallError::Failed(format!(
                "Error: Invalid usage: unknown subcommand: `{named}`"
            )));
        }
        // `bundle upgrade` is shorthand for `bundle install --upgrade`.
        let subcommand = match first_positional {
            Some("upgrade") | None => "install",
            Some(other) => other,
        };
        if SUBCOMMANDS.contains(&subcommand) {
            let mut seen_subcommand = false;
            for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
                if arg.as_str() == subcommand && !seen_subcommand {
                    seen_subcommand = true;
                    continue;
                }
                if seen_subcommand && subcommand == "exec" && !arg.starts_with('-') {
                    break;
                }
                let name = arg.split('=').next().unwrap_or(arg);
                if let Some((display, kind, _, subs)) = SCOPED
                    .iter()
                    .find(|(_, _, spellings, _)| spellings.contains(&name))
                {
                    if !subs.contains(&subcommand) {
                        return Err(InstallError::Failed(format!(
                            "Error: Invalid usage: The `{subcommand}` subcommand does not \
                             accept the `{display}` {kind}."
                        )));
                    }
                    continue;
                }
                // Extension type toggles are generated per extension.
                for ext in [
                    "mas", "vscode", "go", "cargo", "uv", "flatpak", "winget", "krew", "npm",
                ] {
                    let (allowed, display) = if name == format!("--{ext}") {
                        // mas/winget declare no `add` support, so brew
                        // never registers their switches there.
                        (
                            TYPE_SUBS.contains(&subcommand)
                                && !(subcommand == "add" && matches!(ext, "mas" | "winget")),
                            format!("--{ext}"),
                        )
                    } else if name == format!("--no-{ext}") {
                        (
                            matches!(subcommand, "cleanup" | "dump"),
                            format!("--no-{ext}"),
                        )
                    } else if name == format!("--no-cleanup-{ext}") {
                        (subcommand == "cleanup", format!("--no-cleanup-{ext}"))
                    } else if name == format!("--no-dump-{ext}") {
                        (subcommand == "dump", format!("--no-dump-{ext}"))
                    } else {
                        continue;
                    };
                    if !allowed {
                        return Err(InstallError::Failed(format!(
                            "Error: Invalid usage: The `{subcommand}` subcommand does not \
                             accept the `{display}` switch."
                        )));
                    }
                }
            }
        }
        if subcommand == "install"
            && has_flag_eq(rest, "--zap")
            && !has_flag_eq(rest, "--force-cleanup")
            && !env_set("HOMEBREW_BUNDLE_INSTALL_CLEANUP")
        {
            return Err(InstallError::Failed(
                "Error: Invalid usage: `--zap` cannot be passed without `--force-cleanup` or `$HOMEBREW_BUNDLE_INSTALL_CLEANUP`."
                    .to_string(),
            ));
        }
        return bundle_args(rest).map(|prepared| (prepared, rest.to_vec()));
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
                "--no-ask",
                "--yes",
                "-y",
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
                "-g",
                "--greedy-latest",
                "--greedy-auto-updates",
                "--no-ask",
                "--yes",
                "-y",
                "--minimum-version",
                "--min-version",
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
                "--zap",
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
                "--sizes",
                "--github",
                "--verbose",
                "-v",
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
        "linkage" => (&["--test", "--strict", "--reverse"], 0, usize::MAX, true),
        "uses" | "dependents" => (
            &[
                "--installed",
                "--missing",
                "--recursive",
                "--include-build",
                "--include-test",
                "--include-optional",
                "--include-implicit",
                "--skip-recommended",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
        "home" | "homepage" => (
            &["--formula", "--formulae", "--cask", "--casks"],
            0,
            usize::MAX,
            true,
        ),
        "cat" => (&["--cask", "--formula"], 1, usize::MAX, true),
        "outdated" => (
            &[
                "--formula",
                "--cask",
                "--greedy",
                "-g",
                "--greedy-latest",
                "--greedy-auto-updates",
                "--verbose",
                "-v",
                "--quiet",
                "-q",
                "--json",
                "--json=v1",
                "--json=v2",
                "--minimum-version",
                "--min-version",
            ],
            0,
            usize::MAX,
            true,
        ),
        "missing" => (&[], 0, usize::MAX, true),
        "migrate" => (
            &[
                "--force",
                "-f",
                "--dry-run",
                "-n",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
        "zap" => (&["--force", "-f", "--cask", "--casks"], 1, usize::MAX, true),
        "postinstall" => (&[], 1, usize::MAX, false),
        "which-formula" => (&["--explain"], 0, usize::MAX, true),
        "cleanup" => (&["--dry-run", "-n", "-s", "--scrub"], 0, usize::MAX, true),
        "autoremove" => (&["--dry-run", "-n"], 0, 0, true),
        // Brew's docs declares no named args but ignores any given.
        "docs" => (&[], 0, usize::MAX, false),
        "source" => (&[], 0, usize::MAX, true),
        "pin" | "unpin" => (
            &["--formula", "--formulae", "--cask", "--casks"],
            0,
            usize::MAX,
            true,
        ),
        "tab" => (
            &[
                "--installed-on-request",
                "--no-installed-on-request",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
        "link" | "ln" => (
            &[
                "--dry-run",
                "-n",
                "--force",
                "-f",
                "--overwrite",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
        "unlink" => (
            &[
                "--dry-run",
                "-n",
                "--force",
                "-f",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
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
                "-r",
                "-t",
                "--multiple",
                "--json",
            ],
            0,
            usize::MAX,
            true,
        ),
        "fetch" => (
            &[
                "--cask",
                "--formula",
                "--deps",
                "--os",
                "--arch",
                "--bottle-tag",
                "--all-platforms",
            ],
            1,
            usize::MAX,
            true,
        ),
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
                "--full-name",
                "--include-build",
                "--include-test",
                "--include-optional",
                "--include-implicit",
                "--skip-recommended",
                "--include-requirements",
                "--prune",
                "--graph",
                "--dot",
                "--annotate",
                "--missing",
                "--brewfile",
                "--os",
                "--arch",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            0,
            usize::MAX,
            true,
        ),
        "log" => (
            &[
                "-p",
                "-u",
                "--patch",
                "--stat",
                "--oneline",
                "-1",
                "-n",
                "--max-count",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            0,
            1,
            true,
        ),
        "trust" => (
            &[
                "--tap",
                "--taps",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
                "--command",
                "--commands",
                "--json=v1",
            ],
            0,
            usize::MAX,
            false,
        ),
        "untrust" => (
            &[
                "--tap",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
                "--command",
                "--commands",
            ],
            0,
            usize::MAX,
            false,
        ),
        "tap" => (
            &[
                "--list-official",
                "--repair",
                "--custom-remote",
                "--force",
                "-f",
            ],
            0,
            2,
            false,
        ),
        "untap" => (&[], 1, usize::MAX, false),
        // Not native, but its --os/--arch values validate
        // (brew errors) before the not-native refusal.
        "readall" => (&["--os", "--arch"], 0, usize::MAX, false),
        "tap-info" => (
            &["--json", "--json=v1", "--installed"],
            0,
            usize::MAX,
            false,
        ),
        "help" | "--help" | "-h" => (&[], 0, 1, false),
        "completions" => (&[], 0, 1, false),
        "--prefix" => (&["--installed", "--unbrewed"], 0, usize::MAX, true),
        "--cellar" | "--caskroom" => (&[], 0, usize::MAX, true),
        "command" => (&[], 1, usize::MAX, true),
        "formula" => (&[], 1, usize::MAX, false),
        "generate-cask-token" => (&[], 1, 1, false),
        "services" => (
            &[
                "--all",
                "--json",
                "--file",
                "--max-wait",
                "--no-wait",
                "--keep",
                "--sudo-service-user",
            ],
            0,
            usize::MAX,
            false,
        ),
        "unpack" => (
            &[
                "--destdir",
                "--patch",
                "--git",
                "-g",
                "--force",
                "-f",
                "--formula",
                "--formulae",
                "--cask",
                "--casks",
            ],
            1,
            usize::MAX,
            true,
        ),
        "command-not-found-init" => (&[], 0, 0, false),
        "alias" => (&[], 0, 1, false),
        "unalias" => (&[], 1, usize::MAX, false),
        "--cache" => (
            &["--formula", "--cask", "--os", "--arch", "--bottle-tag"],
            0,
            usize::MAX,
            true,
        ),
        "--repository" | "--repo" => (&[], 0, 1, false),
        "--taps" => (&[], 0, 0, false),
        "analytics" => (&[], 0, 1, false),
        "doctor" | "dr" => (&["--quiet", "-q", "--list-checks"], 0, usize::MAX, false),
        "shellenv" => (&[], 0, usize::MAX, false),
        "on" | "off" | "status" | "--version" | "-v" | "formulae" | "casks" | "config"
        | "update" => (&[], 0, 0, false),
        "--env" | "env" => (&["--plain", "--shell"], 0, usize::MAX, true),
        "developer" => (&[], 0, 1, false),
        "commands" => (&["--quiet", "-q", "--include-aliases"], 0, 0, false),
        "leaves" => (
            &[
                "--installed-on-request",
                "-r",
                "--installed-as-dependency",
                "-p",
            ],
            0,
            0,
            false,
        ),
        _ => return Err(unknown()),
    };
    let flag_ok = |arg: &str| {
        // Brew's parser carries the global switches on every
        // command; handlers read the ones they honor.
        matches!(
            arg,
            "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d"
        ) || flags.contains(&arg)
            || (value_flags
                .iter()
                .any(|f| arg == *f || arg.starts_with(&format!("{f}=")))
                && !(cmd == "missing" && arg.starts_with("--hide=")))
            || (cmd == "info" && (arg.starts_with("--days=") || arg.starts_with("--category=")))
            || (matches!(cmd, "--env" | "env") && arg.starts_with("--shell="))
            || (cmd == "cleanup" && arg.starts_with("--prune="))
            || (cmd == "deps"
                && (arg.starts_with("--brewfile=")
                    || arg.starts_with("--os=")
                    || arg.starts_with("--arch=")))
            || (cmd == "unpack" && arg.starts_with("--destdir="))
            || (matches!(cmd, "fetch" | "--cache")
                && (arg.starts_with("--os=")
                    || arg.starts_with("--arch=")
                    || arg.starts_with("--bottle-tag=")))
            || (matches!(cmd, "outdated" | "upgrade")
                && (arg.starts_with("--minimum-version=") || arg.starts_with("--min-version=")))
            || (cmd == "missing"
                && arg.strip_prefix("--hide=").is_some_and(|v| {
                    v.is_empty()
                        || !v.is_empty()
                            && v.split(',').all(|part| {
                                let part = part.trim();
                                !part.is_empty()
                                    && part.split('/').all(valid_component)
                                    && matches!(part.matches('/').count(), 0 | 2)
                            })
                }))
    };
    // Brew's known options for this command: abrew's tables
    // plus the flags in brew's recorded help. Used both to
    // expand OptionParser abbreviations and to tell brew's
    // invalid-option errors from native refusals below.
    let (known_longs, known_shorts) = {
        let mut longs: Vec<String> = Vec::new();
        let mut shorts: Vec<char> = Vec::new();
        let note = |tok: &str, longs: &mut Vec<String>, shorts: &mut Vec<char>| {
            if let Some(long) = tok.strip_prefix("--") {
                let long = long.split(['=', '[']).next().unwrap_or(long);
                if !long.is_empty() && !longs.iter().any(|k| k == long) {
                    longs.push(long.to_string());
                }
            } else if tok.len() == 2 && tok.starts_with('-') {
                let c = tok.chars().nth(1).unwrap();
                if !shorts.contains(&c) {
                    shorts.push(c);
                }
            }
        };
        for tok in flags.iter().copied().chain(value_flags.iter().copied()) {
            note(tok, &mut longs, &mut shorts);
        }
        for group in conflict_groups {
            for (name, alts) in *group {
                note(name, &mut longs, &mut shorts);
                for alt in *alts {
                    note(alt, &mut longs, &mut shorts);
                }
            }
        }
        if let Some((_, text)) = help_texts::TEXTS
            .iter()
            .find(|(name, _)| *name == disabled_cmd || *name == cmd)
        {
            for line in text.lines() {
                let line = line.trim_start();
                if !line.starts_with('-') {
                    continue;
                }
                for tok in line.split(|c: char| c == ',' || c.is_whitespace()) {
                    if tok.starts_with('-') {
                        note(tok, &mut longs, &mut shorts);
                    }
                }
            }
        }
        for tok in [
            "--verbose",
            "--quiet",
            "--debug",
            "--help",
            "-v",
            "-q",
            "-d",
            "-h",
        ] {
            note(tok, &mut longs, &mut shorts);
        }
        (longs, shorts)
    };
    // OptionParser expands an unambiguous long-option prefix
    // (`--j` for `--json`); rewrite tokens whose unique
    // expansion is natively admitted so handlers see the
    // full spelling. Ambiguous/unknown tokens stay as typed
    // for the error classification below.
    let expanded: Vec<String> = {
        let native_long = |name: &str| {
            flags.iter().any(|f| {
                f.strip_prefix("--")
                    .is_some_and(|long| long.split(['=', '[']).next().unwrap_or(long) == name)
            }) || value_flags
                .iter()
                .any(|f| f.strip_prefix("--") == Some(name))
                || matches!(name, "verbose" | "quiet" | "debug" | "help")
        };
        let mut out: Vec<String> = Vec::with_capacity(rest.len());
        let mut literal = false;
        let mut skip_value = false;
        for tok in rest {
            if literal || skip_value {
                skip_value = false;
                out.push(tok.clone());
                continue;
            }
            if tok == "--" {
                literal = true;
                out.push(tok.clone());
                continue;
            }
            if value_flags.contains(&tok.as_str()) {
                skip_value = true;
                out.push(tok.clone());
                continue;
            }
            let rewritten = tok.strip_prefix("--").and_then(|long| {
                let (name, value) = match long.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (long, None),
                };
                if native_long(name) || known_longs.iter().any(|k| k == name) {
                    return None;
                }
                let mut candidates = known_longs.iter().filter(|k| k.starts_with(name));
                let only = candidates.next()?;
                if candidates.next().is_some() || !native_long(only) {
                    return None;
                }
                Some(match value {
                    Some(v) => format!("--{only}={v}"),
                    None => format!("--{only}"),
                })
            });
            out.push(rewritten.unwrap_or_else(|| tok.clone()));
        }
        out
    };
    let rest: &[String] = &expanded;
    check_depends_on(rest)?;
    // `--minimum-version` (outdated/upgrade) is a one-name
    // check; brew raises the arity error when the command
    // starts, ahead of its own JSON-version handling. A blank
    // value counts as absent, like `present?`.
    if matches!(cmd, "outdated" | "upgrade")
        && minimum_version_value(rest).is_some_and(|v| v.chars().any(|c| !c.is_whitespace()))
        && commands::operands_without_flag_values(
            rest,
            &["--minimum-version", "--min-version", "--severity"],
        )
        .len()
            != 1
    {
        return Err(InstallError::Failed(
            "Error: Invalid usage: `--minimum-version` requires exactly one formula or cask argument."
                .to_string(),
        ));
    }
    // Brew validates an explicit --json=<version> per command
    // (info takes v1/v2, outdated only v2 with a dedicated v1
    // message, tap-info only v1).
    for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
        let Some(version) = arg.strip_prefix("--json=") else {
            continue;
        };
        let valid = match cmd {
            "info" => matches!(version, "v1" | "v2"),
            "outdated" => {
                if version == "v1" {
                    return Err(InstallError::Failed(
                        "Error: `brew outdated --json=v1` is no longer supported. \
                         Use brew outdated --json=v2 instead."
                            .to_string(),
                    ));
                }
                version == "v2"
            }
            "tap-info" => version == "v1",
            _ => continue,
        };
        if !valid {
            return Err(InstallError::Failed(format!(
                "Error: Invalid usage: invalid JSON version: {version}"
            )));
        }
    }
    // brew's reinstall has no `--dry-run` switch (abrew's
    // direct dialect keeps it; under the reroute brew's
    // parser error wins). Uninstall has one in brew 7.
    if via_reroute()
        && cmd == "reinstall"
        && let Some(flag) = rest
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .find(|arg| matches!(arg.as_str(), "--dry-run" | "-n"))
    {
        return Err(InstallError::Failed(format!(
            "Error: invalid option: {flag}"
        )));
    }
    // Brew validates --os/--arch simulation values when the
    // command starts (SimulateSystem.with): macOS release
    // symbols plus macos/linux, and only :arm/:intel arches.
    // OptionParser consumes the next token as the value even
    // when it spells another switch; the last spelling wins.
    if matches!(cmd, "deps" | "fetch" | "--cache" | "readall") {
        let last_value = |flag: &str| {
            let mut value = None;
            let mut args = rest.iter().take_while(|arg| arg.as_str() != "--");
            while let Some(arg) = args.next() {
                if arg == flag {
                    value = args.next().map(String::as_str);
                } else if let Some(v) = arg.strip_prefix(&format!("{flag}=")) {
                    value = Some(v);
                }
            }
            value
        };
        // --cache and fetch accept "all" (every tag).
        let cache_all = matches!(cmd, "--cache" | "fetch" | "readall");
        if cmd == "deps" && last_value("--os") == Some("all") {
            return Err(InstallError::Failed(
                "Error: Invalid usage: `brew deps --os=all` is not supported.".to_string(),
            ));
        }
        if cmd == "deps" && last_value("--arch") == Some("all") {
            return Err(InstallError::Failed(
                "Error: Invalid usage: `brew deps --arch=all` is not supported.".to_string(),
            ));
        }
        if let Some(os) = last_value("--os")
            && !(cache_all && os == "all")
            && !matches!(
                os,
                "macos"
                    | "linux"
                    | "golden_gate"
                    | "tahoe"
                    | "sequoia"
                    | "sonoma"
                    | "ventura"
                    | "monterey"
                    | "big_sur"
            )
        {
            return Err(InstallError::Failed(format!("Error: Unknown OS: {os}")));
        }
        if let Some(arch) = last_value("--arch")
            && !(cache_all && arch == "all")
            && !matches!(arch, "arm" | "intel")
        {
            return Err(InstallError::Failed(
                "Error: New arch must be :arm or :intel".to_string(),
            ));
        }
    }
    // A boolean switch passed a value is brew's OptionParser
    // "needless argument" error; value flags are exempt.
    for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
        let Some(eq) = arg.find('=') else { continue };
        let name = &arg[..eq];
        let boolean = (name.starts_with("--") || name.len() == 2 && name.starts_with('-'))
            && (flags.contains(&name)
                || matches!(
                    name,
                    "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d"
                ))
            && !value_flags.contains(&name);
        if boolean {
            return Err(InstallError::Failed(format!(
                "Error: needless argument: {arg}"
            )));
        }
    }
    // The token after a space-form value flag is its value,
    // never an option (brew's parser consumes it as such).
    let offender_at = {
        let mut at = None;
        let mut skip_value = false;
        for (i, arg) in rest.iter().enumerate() {
            if arg == "--" {
                break;
            }
            if skip_value {
                skip_value = false;
                continue;
            }
            if value_flags.contains(&arg.as_str()) {
                skip_value = true;
                continue;
            }
            if arg.starts_with('-') && arg.len() > 1 && !flag_ok(arg) {
                at = Some(i);
                break;
            }
        }
        at
    };
    if let Some(offender_at) = offender_at {
        // brew reports the token as typed (before abrew's
        // kind-switch canonicalization).
        let typed_offender = typed_rest[offender_at].as_str();
        // An option brew itself does not know for this command
        // is its OptionParser error, not a native refusal
        // (known set computed above for abbreviation expansion).
        let offender = rest[offender_at].as_str();
        if let Some(long) = offender.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            if !known_longs.iter().any(|k| k == name) {
                let expansions = known_longs.iter().filter(|k| k.starts_with(name)).count();
                let typed_name = typed_offender
                    .strip_prefix("--")
                    .and_then(|t| t.split('=').next())
                    .unwrap_or(name);
                if expansions > 1 {
                    return Err(InstallError::Failed(format!(
                        "Error: ambiguous option: --{typed_name}"
                    )));
                }
                if expansions == 0 {
                    return Err(InstallError::Failed(format!(
                        "Error: invalid option: {typed_offender}"
                    )));
                }
            }
        } else if let Some(cluster) = offender.strip_prefix('-') {
            for c in cluster.chars() {
                if !known_shorts.contains(&c) {
                    return Err(InstallError::Failed(format!("Error: invalid option: -{c}")));
                }
            }
        }
        return Err(unsupported());
    }
    if cmd == "cleanup" {
        cleanup_max_age(rest)
            .map_err(|error| InstallError::Failed(format!("Error: Invalid usage: {error}")))?;
    }
    let ops = operands_without_flag_values(rest, value_flags);
    if cmd == "completions"
        && let Some(sub) = ops.first()
        && !matches!(sub.as_str(), "state" | "link" | "unlink")
    {
        return Err(InstallError::Failed(format!(
            "Error: Invalid usage: unknown subcommand: `{sub}`"
        )));
    }
    if matches!(cmd, "developer" | "analytics")
        && let Some(sub) = ops.first()
        && !matches!(sub.as_str(), "state" | "on" | "off")
    {
        return Err(InstallError::Failed(format!(
            "Error: Invalid usage: unknown subcommand: `{sub}`"
        )));
    }
    // Abrew records no analytics to toggle.
    if cmd == "analytics" && ops.iter().any(|s| matches!(s.as_str(), "on" | "off")) {
        return Err(unsupported());
    }
    let want_formula = has_flag(rest, "--formula", "");
    let want_cask = has_flag(rest, "--cask", "");
    let valid_package_name = |s: &String| {
        // Dash-leading operands only arise after `--`; brew
        // tries to resolve them and fails with its own error,
        // so they pass admission to reach it.
        s.starts_with('-')
            || valid_component(s)
            || ((want_formula || matches!(cmd, "--prefix" | "--cellar"))
                && s.strip_prefix("homebrew/core/")
                    .is_some_and(valid_component))
            || ((want_cask || cmd == "--caskroom")
                && s.strip_prefix("homebrew/cask/")
                    .is_some_and(valid_component))
    };
    if names && !ops.iter().all(valid_package_name) {
        return Err(unsupported());
    }
    if cmd == "tab"
        && has_flag(rest, "--installed-on-request", "")
        && has_flag(rest, "--no-installed-on-request", "")
    {
        return Err(InstallError::Failed(
            "Error: Invalid usage: Options --installed-on-request and --no-installed-on-request are mutually exclusive."
                .to_string(),
        ));
    }
    if ops.len() < min {
        return Err(InstallError::Failed(format!(
            "Error: Invalid usage: This command requires {} {} {} {}.",
            if min == max { "exactly" } else { "at least" },
            min,
            named_arg_types(cmd),
            if min == 1 { "argument" } else { "arguments" }
        )));
    }
    if ops.len() > max {
        if max == 0 && !matches!(cmd, "--env" | "env" | "developer") {
            return Err(InstallError::Failed(
                "Error: Invalid usage: This command does not take named arguments.".to_string(),
            ));
        }
        if matches!(cmd, "log" | "tap") {
            return Err(InstallError::Failed(format!(
                "Error: Invalid usage: This command does not take more than {} {} {}.",
                max,
                named_arg_types(cmd),
                if max == 1 { "argument" } else { "arguments" }
            )));
        }
        return Err(unsupported());
    }
    if matches!(cmd, "tap" | "untap" | "tap-info") {
        let checked: &[String] = if cmd == "tap" {
            ops.first().map_or(&[][..], std::slice::from_ref)
        } else {
            &ops
        };
        for op in checked {
            let mut parts = op.split('/');
            let valid = matches!(
                (parts.next(), parts.next(), parts.next()),
                (Some(u), Some(r), None) if valid_component(u) && valid_component(r)
            );
            if !valid {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid tap name: '{op}'"
                )));
            }
        }
    }
    if has_flag(rest, "--cask", "") && has_flag(rest, "--formula", "") {
        // Brew's parser conflicts the kind switches on most
        // commands; the wording follows each command's own switch
        // definitions. `desc` accepts both as "either kind";
        // `search` implements its own resolution; the remaining
        // commands keep the native refusal. Plain `list` forms
        // never reach brew's parser — its Bash path prints
        // nothing — while the other list forms conflict.
        if matches!(cmd, "list" | "ls") {
            if has_flag(rest, "--json", "") && has_flag(rest, "--versions", "") {
                return Err(InstallError::Failed(
                    "Error: `--formula` and `--cask` are mutually exclusive.".to_string(),
                ));
            }
            let bash_form = ops.is_empty()
                && !has_flag(rest, "--versions", "")
                && !has_flag(rest, "--pinned", "")
                && !has_flag(rest, "--multiple", "")
                && !has_flag(rest, "--full-name", "")
                && !has_flag(rest, "--json", "");
            if !bash_form {
                return Err(InstallError::Failed(
                    "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
                        .to_string(),
                ));
            }
        }
        let exclusive = match cmd {
            "install" | "upgrade" | "reinstall" => {
                Some("Error: Invalid usage: Options --cask and --formulae are mutually exclusive.")
            }
            "info" | "outdated" | "uses" | "dependents" | "home" | "homepage" | "cat" | "deps"
            | "pin" | "unpin" | "uninstall" | "remove" | "rm" | "fetch" | "link" | "ln"
            | "unlink" | "log" | "tab" | "migrate" | "--cache" | "unpack" => {
                Some("Error: Invalid usage: Options --formula and --cask are mutually exclusive.")
            }
            "desc" | "search" | "-S" | "list" | "ls" => None,
            _ => return Err(unsupported()),
        };
        if let Some(message) = exclusive {
            return Err(InstallError::Failed(message.to_string()));
        }
    }
    if matches!(cmd, "install" | "reinstall" | "upgrade")
        && !has_flag(rest, "--formula", "")
        && (has_flag(rest, "--quiet", "-q") || has_flag(rest, "--ignore-dependencies", ""))
    {
        return Err(unsupported());
    }
    if matches!(cmd, "list" | "ls") {
        // Brew's list conflicts whose switches abrew admits, in
        // its definition order (the -r/-t and installation-kind
        // pairs name switches outside the native row). The
        // `--versions --json` forms never reach brew's parser
        // (its Bash fast path answers them), so they keep the
        // json gates below instead.
        let json_versions = has_flag(rest, "--json", "") && has_flag(rest, "--versions", "");
        let conflicts: &[(&str, &str)] = if json_versions {
            &[]
        } else {
            &[
                ("--multiple", "--cask"),
                ("--pinned", "--multiple"),
                ("--versions", "-1"),
                ("--multiple", "-1"),
                ("--pinned", "-1"),
                ("--versions", "-l"),
                ("--multiple", "-l"),
                ("--pinned", "-l"),
                ("--full-name", "--versions"),
                ("--full-name", "--multiple"),
                ("--full-name", "--pinned"),
                ("--full-name", "-l"),
            ]
        };
        for (a, b) in conflicts {
            if has_flag(rest, a, "") && has_flag(rest, b, "") {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: Options {a} and {b} are mutually exclusive."
                )));
            }
        }
        if has_flag(rest, "--json", "") && !has_flag(rest, "--versions", "") {
            return Err(InstallError::Failed(
                "Error: Invalid usage: `brew list --json` requires `--versions`.".to_string(),
            ));
        }
        if has_flag(rest, "--json", "")
            && (!ops.is_empty()
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
    if matches!(cmd, "uninstall" | "remove" | "rm")
        && has_flag(rest, "--zap", "")
        && has_flag(rest, "--formula", "")
    {
        return Err(InstallError::Failed(
            "Error: Invalid usage: Options --formula and --zap are mutually exclusive.".to_string(),
        ));
    }
    if cmd == "info" {
        let analytics = has_flag(rest, "--analytics", "");
        let json = rest
            .iter()
            .any(|s| matches!(s.as_str(), "--json" | "--json=v1" | "--json=v2"));
        // Empty `--days=`/`--category=` values are absent to
        // brew (Ruby `present?`); non-empty values validate
        // with brew's usage errors (cmd/info.rb).
        let days = valued_flag_last(rest, "--days").filter(|v| !v.is_empty());
        let category = valued_flag_last(rest, "--category").filter(|v| !v.is_empty());
        if days
            .as_deref()
            .is_some_and(|v| !matches!(v, "30" | "90" | "365"))
        {
            return Err(InstallError::Failed(
                "Error: Invalid usage: `--days` must be one of 30, 90, 365.".to_string(),
            ));
        }
        if let Some(category) = category.as_deref() {
            const FORMULA_CATEGORIES: [&str; 3] = ["install", "install-on-request", "build-error"];
            if !ops.is_empty() && !FORMULA_CATEGORIES.contains(&category) {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: `--category` must be one of {} when querying formulae.",
                    FORMULA_CATEGORIES.join(", ")
                )));
            }
            if !FORMULA_CATEGORIES.contains(&category)
                && !matches!(category, "cask-install" | "os-version")
            {
                return Err(InstallError::Failed(format!(
                    "Error: Invalid usage: `--category` must be one of {}, cask-install, os-version.",
                    FORMULA_CATEGORIES.join(", ")
                )));
            }
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
    {
        match compile_search_regex(pattern) {
            Ok(_) => {}
            // Syntax failures every regex dialect rejects take
            // brew's error; merely unsupported constructs (and
            // oversized programs) still delegate.
            Err(aterm_regex::Error::Syntax(msg))
                if [
                    "unclosed ",
                    "unopened group",
                    "repetition operator missing expression",
                    "incomplete escape sequence",
                    "invalid character class range",
                    "invalid repetition count range",
                ]
                .iter()
                .any(|marker| msg.contains(marker)) =>
            {
                return Err(InstallError::Failed(format!(
                    "Error: {} is not a valid regex.",
                    ops.join(" ")
                )));
            }
            Err(_) => return Err(unsupported()),
        }
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
    if cmd == "deps" {
        deps_brewfile_admission(rest)?;
    }
    Ok((None, rest.to_vec()))
}

/// The `--brewfile` flag's value in `rest`, if the flag is
/// present: `None` for a bare `--brewfile`, the `=` value
/// (possibly empty) otherwise. OptionParser's optional-value
/// flag never consumes the next argument, and a repeat takes
/// the last occurrence.
fn brewfile_flag_value(rest: &[String]) -> Option<Option<String>> {
    let mut value = None;
    for arg in rest.iter().take_while(|arg| arg.as_str() != "--") {
        if arg == "--brewfile" {
            value = Some(None);
        } else if let Some(v) = arg.strip_prefix("--brewfile=") {
            value = Some(Some(v.to_string()));
        }
    }
    value
}

/// The last value of a required-value flag in `rest` (`=` or
/// space form; OptionParser consumes the next token verbatim).
fn valued_flag_last(rest: &[String], flag: &str) -> Option<String> {
    let mut value = None;
    let mut it = rest.iter().take_while(|arg| arg.as_str() != "--");
    while let Some(arg) = it.next() {
        if arg == flag {
            value = it.next().cloned();
        } else if let Some(v) = arg
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
        {
            value = Some(v.to_string());
        }
    }
    value
}

/// The `--minimum-version`/`--min-version` value in `rest`
/// (brew: `args.minimum_version || args.min_version` — the
/// long spelling wins when both are given, a repeat takes the
/// last). A required-value flag consumes the next token
/// verbatim, or takes an `=` value.
fn minimum_version_value(rest: &[String]) -> Option<String> {
    fn last_value(rest: &[String], flag: &str) -> Option<String> {
        let mut value = None;
        let mut it = rest.iter().take_while(|arg| arg.as_str() != "--");
        while let Some(arg) = it.next() {
            if arg == flag {
                value = it.next().cloned();
            } else if let Some(v) = arg
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
            {
                value = Some(v.to_string());
            }
        }
        value
    }
    last_value(rest, "--minimum-version").or_else(|| last_value(rest, "--min-version"))
}

/// `deps --brewfile` is native only for Brewfiles the deps
/// parser fully reads; an unreadable file is still admitted —
/// the command itself reports brew's `No Brewfile found`.
fn deps_brewfile_admission(rest: &[String]) -> Result<(), InstallError> {
    let Some(value) = brewfile_flag_value(rest) else {
        return Ok(());
    };
    // Reading stdin here would consume it before the command
    // runs; cmd_deps stays fail-closed on rows it cannot read.
    if value.as_deref() == Some("-") {
        return Ok(());
    }
    let path = bundle::deps_brewfile_path(value.as_deref());
    let Ok(text) = aterm_uds::atomic_file::read_regular_text(&path, 4 * 1024 * 1024) else {
        return Ok(());
    };
    let (_, skipped) = bundle::parse_brewfile_deps(&text);
    if skipped.is_empty() {
        Ok(())
    } else {
        Err(InstallError::Unsupported(
            "'deps --brewfile' with these Brewfile rows is not native".to_owned(),
        ))
    }
}

fn bundle_args(rest: &[String]) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let unsupported =
        || InstallError::Unsupported("bundle arguments or Brewfile rows are not native".to_owned());
    match rest.first().map(String::as_str) {
        Some("status") if rest.len() == 1 => Ok(None),
        // Direct abrew's bare `bundle remove` removes the legacy
        // bundled Homebrew; brew's removes Brewfile entries and
        // requires names (also direct abrew with names).
        Some("remove") if rest.len() == 1 && !via_reroute() => Ok(None),
        Some("remove") => bundle_edit_args(rest.get(1..).unwrap_or(&[]), false),
        Some("add") => bundle_edit_args(rest.get(1..).unwrap_or(&[]), true),
        Some("dump") => match bundle::dump_selection(rest) {
            Some(sel) if !sel.extensions_enabled => Ok(None),
            _ => Err(unsupported()),
        },
        Some("install" | "upgrade") | None => brewfile_args(rest.get(1..).unwrap_or(&[]), false),
        Some("check") => brewfile_args(rest.get(1..).unwrap_or(&[]), true),
        Some("list") => bundle_list_args(rest.get(1..).unwrap_or(&[])),
        Some("cleanup") => {
            if rest.iter().any(|a| a == "--install") {
                return Err(unsupported());
            }
            match bundle::cleanup_selection(rest) {
                Some(sel) if !sel.extensions_enabled => Ok(None),
                _ => Err(unsupported()),
            }
        }
        Some("edit") => Ok(None),
        Some(flag) if flag.starts_with("--file") => brewfile_args(rest, false),
        _ => Err(unsupported()),
    }
}

fn bundle_list_args(rest: &[String]) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
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
        } else if matches!(
            arg.as_str(),
            "--all"
                | "--formula"
                | "--formulae"
                | "--brews"
                | "--cask"
                | "--casks"
                | "--tap"
                | "--taps"
                | "--verbose"
                | "-v"
                | "--quiet"
                | "-q"
                | "--debug"
                | "-d"
        ) || bundle::EXTENSION_KEYS
            .iter()
            .any(|k| arg.as_str() == format!("--{k}"))
        {
            // `bundle list` type/display switches.
        } else {
            return Err(unsupported());
        }
    }
    let text = bundle::read_brewfile(file.as_deref()).map_err(InstallError::Failed)?;
    if bundle::parse_brewfile_list(&text).is_none() {
        return Err(unsupported());
    }
    Ok(None)
}

/// Admission for `bundle add`/`bundle remove` (brew's Brewfile
/// editing subcommands): at least one name, at most one type
/// switch group, plain names only.
fn bundle_edit_args(
    rest: &[String],
    add: bool,
) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
    let unsupported =
        || InstallError::Unsupported("bundle arguments or Brewfile rows are not native".to_owned());
    let sub = if add { "add" } else { "remove" };
    let mut file = None;
    let mut types: Vec<&str> = Vec::new();
    let mut names = 0usize;
    let mut it = rest.iter();
    let mut after_dashdash = false;
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        if after_dashdash {
            names += 1;
        } else if a == "--" {
            after_dashdash = true;
        } else if a == "--file" && file.is_none() {
            file = Some(std::path::PathBuf::from(
                it.next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(unsupported)?,
            ));
        } else if let Some(path) = a.strip_prefix("--file=").filter(|s| !s.is_empty())
            && file.is_none()
        {
            file = Some(std::path::PathBuf::from(path));
        } else if matches!(a, "--formula" | "--formulae" | "--brews") {
            types.push("brew");
        } else if matches!(a, "--cask" | "--casks") {
            types.push("cask");
        } else if matches!(a, "--tap" | "--taps") {
            types.push("tap");
        } else if let Some(key) = bundle::EXTENSION_KEYS
            .iter()
            .find(|k| a.strip_prefix("--") == Some(k))
        {
            types.push(key);
        } else if matches!(a, "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d")
            || (add && a == "--no-describe")
        {
            // Display/add switches with no admission effect.
        } else if a.starts_with('-') && a != "-" {
            return Err(unsupported());
        } else {
            if a.contains(['"', '\n']) {
                return Err(unsupported());
            }
            names += 1;
        }
    }
    types.sort_unstable();
    types.dedup();
    if types.len() > 1 {
        return Err(InstallError::Failed(format!(
            "Error: Invalid usage: `{sub}` supports only one type of entry at a time."
        )));
    }
    if names == 0 {
        return Err(InstallError::Failed(
            "Error: Invalid usage: This command requires at least 1 named argument.".to_owned(),
        ));
    }
    if !add {
        bundle::read_brewfile(file.as_deref()).map_err(InstallError::Failed)?;
    }
    Ok(None)
}

fn brewfile_args(
    rest: &[String],
    check: bool,
) -> Result<Option<Vec<bundle::BrewfileEntry>>, InstallError> {
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
        } else if check
            && matches!(
                arg.as_str(),
                "--verbose" | "-v" | "--no-upgrade" | "--quiet" | "-q"
            )
        {
            // `bundle check` display switches.
        } else if !check
            && (matches!(
                arg.as_str(),
                "--verbose"
                    | "-v"
                    | "--quiet"
                    | "-q"
                    | "--debug"
                    | "-d"
                    | "--no-upgrade"
                    | "--upgrade"
                    | "--force"
                    | "-f"
            ) || arg.starts_with("--upgrade-formulae=")
                || arg.starts_with("--upgrade-formula=")
                || arg.starts_with("--jobs="))
        {
            // `bundle install` upgrade/display switches.
        } else if !check
            && matches!(
                arg.as_str(),
                "--upgrade-formulae" | "--upgrade-formula" | "--jobs"
            )
        {
            // Space-form values ride along untouched.
            let _ = it.next();
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
        ask: false,
    };
    let mut result = Ok(());
    let mut any = false;
    for name in rest.iter().filter(|s| !s.starts_with('-')) {
        any = true;
        // Upgrading a name that is not installed is the command's
        // own error, not a plan to admit or delegate.
        if opts.upgrade {
            let short = Formula::short_name(name);
            let formula_installed = prefix.installed_one(prefix::Kind::Formula, short).is_some()
                || api
                    .formula(name)
                    .ok()
                    .or_else(|| tap::lookup(prefix, name))
                    .is_some_and(|f| {
                        prefix
                            .installed_one(prefix::Kind::Formula, &f.name)
                            .is_some()
                    });
            if !formula_installed
                && prefix
                    .installed_one(prefix::Kind::Cask, api::canonical_cask_token(short))
                    .is_none()
            {
                continue;
            }
        }
        result = if want_cask || (!want_formula && api.formula(name).is_err()) {
            match api.cask(name) {
                Ok(c) => cask::preflight(prefix, &c, machine::Machine::this())
                    .and_then(|()| cask::preflight_deps(api, prefix, &c)),
                Err(_) => continue,
            }
        } else {
            // A plain install of an outdated, linked formula is
            // brew's install-upgrade: admit its replacement here
            // the way the command will run it.
            let name_opts = if args.first().is_some_and(|a| a == "install")
                && !opts.force
                && !env_set("HOMEBREW_NO_INSTALL_UPGRADE")
                && outdated_linked_formula(prefix, api, name)
                    .is_some_and(|(f, _)| !prefix.is_pinned(&f.name))
            {
                install::InstallOpts {
                    upgrade: true,
                    ..opts
                }
            } else {
                opts
            };
            install::preflight_plan(api, prefix, name, &name_opts)
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
fn command_args(args: &[String]) -> std::borrow::Cow<'_, [String]> {
    // Brew's shell entry shifts a leading `-v` to the end of the
    // argument list (so `brew -v info x` is `brew info x -v`);
    // a lone `-v` stays the version command.
    if args.len() > 1 && args[0] == "-v" {
        let mut rotated: Vec<String> = args[1..].to_vec();
        rotated.push("-v".to_string());
        std::borrow::Cow::Owned(rotated)
    } else {
        std::borrow::Cow::Borrowed(args)
    }
}

fn dispatch(prefix: &Prefix, args: &[String]) -> ExitCode {
    let args = command_args(args);
    let args: &[String] = &args;
    if args.is_empty() {
        // Under the `brew` reroute, no arguments is brew's usage
        // error: the help message on stderr, exit 1.
        if via_reroute() {
            eprint!("{}", help_texts::TOP);
            return ExitCode::from(1);
        }
        print!("{HELP_MAIN}");
        return ExitCode::SUCCESS;
    }
    // Brew's shell entry treats a leading --version (or a lone
    // -v, after its shift trick) as the version command and
    // ignores everything after it.
    if matches!(args[0].as_str(), "--version" | "-v") {
        println!("abrew {} (aterm)", aterm_stamp::app_version());
        return ExitCode::SUCCESS;
    }
    // Only global flags and no command is the no-arguments
    // case in brew (help), not an unknown command.
    let global_only = args.iter().all(|arg| {
        matches!(arg.as_str(), "--verbose" | "--quiet" | "--debug")
            || (arg.len() > 1
                && arg.starts_with('-')
                && !arg.starts_with("--")
                && arg[1..].chars().all(|c| matches!(c, 'v' | 'q' | 'd')))
    });
    if global_only {
        if via_reroute() {
            eprint!("{}", help_texts::TOP);
            return ExitCode::from(1);
        }
        print!("{HELP_MAIN}");
        return ExitCode::SUCCESS;
    }
    let (cmd, rest): (&str, &[String]) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("", &[]),
    };
    let cmd = match cmd {
        "instal" => "install",
        "uninstal" => "uninstall",
        "abv" => "info",
        "up" => "update",
        "environment" => "--env",
        "--config" => "config",
        "post_install" => "postinstall",
        other => other,
    };
    let canonical = canonical_kind_switches(rest);
    let rest: &[String] = &canonical;
    if via_reroute() {
        // Brew's help surface, intercepted before admission as
        // brew.rb does: `help <topic>`, and `--help`/`-h` anywhere
        // in a command's arguments (before `--`).
        if matches!(cmd, "--help" | "-h" | "help") {
            // The topic is the first argument; `--help`/`-h` there
            // (or no argument) means the top-level message, while
            // dashed commands like `--cache` are real topics.
            let topic = rest
                .first()
                .filter(|arg| !matches!(arg.as_str(), "--help" | "-h" | "help"));
            return match topic {
                None => {
                    print!("{}", help_texts::TOP);
                    ExitCode::SUCCESS
                }
                Some(topic) => {
                    let known = help_texts::TEXTS
                        .iter()
                        .any(|(name, _)| *name == canonical_help_command(topic));
                    if known {
                        print!("{}", brew_help_text(topic));
                        ExitCode::SUCCESS
                    } else {
                        let suggestion = commands::command_suggestion(
                            topic,
                            &command_names(true)
                                .iter()
                                .map(|n| n.to_string())
                                .collect::<Vec<_>>(),
                        );
                        usage_failed(
                            topic,
                            &format!(
                                "Error: Invalid usage: Unknown command: brew {topic}{suggestion}"
                            ),
                        )
                    }
                }
            };
        }
        let help_flag = rest
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
        if help_flag
            && !shell_fast_path(cmd)
            && help_texts::TEXTS
                .iter()
                .any(|(name, _)| *name == canonical_help_command(cmd))
        {
            print!("{}", brew_help_text(cmd));
            return ExitCode::SUCCESS;
        }
    } else {
        // Direct abrew answers `--help`/`-h` on a known command
        // with its own usage text, like `abrew help <cmd>`.
        let help_flag = rest
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
        if help_flag
            && !shell_fast_path(cmd)
            && command_names(true).contains(&canonical_help_command(cmd))
        {
            print!("{}", help_for(cmd));
            return ExitCode::SUCCESS;
        }
    }
    let (prepared, admitted) = match native_args(cmd, rest) {
        Ok(pair) => pair,
        Err(error) => return from_install(prefix, args, Err(error)),
    };
    let rest: &[String] = &admitted;
    match cmd {
        "--version" | "-v" if rest.is_empty() => {
            println!("abrew {} (aterm)", aterm_stamp::app_version());
            return ExitCode::SUCCESS;
        }
        "--help" | "-h" | "help" => {
            let topic = rest.first().map(String::as_str).unwrap_or("");
            if !topic.is_empty()
                && !matches!(topic, "--help" | "-h" | "help")
                && !command_names(true).contains(&canonical_help_command(topic))
            {
                let dictionary: Vec<String> = command_names(true)
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                return failed(&format!(
                    "Error: Invalid usage: Unknown command: brew {topic}{}",
                    commands::command_suggestion(topic, &dictionary)
                ));
            }
            print!("{}", help_for(topic));
            return ExitCode::SUCCESS;
        }
        "--prefix" | "--cellar" | "--caskroom" => {
            let names = operands(rest);
            if cmd == "--prefix" && has_flag(rest, "--unbrewed", "") {
                if !names.is_empty() {
                    return usage_failed(
                        cmd,
                        "Error: Invalid usage: `--unbrewed` does not take a formula argument.",
                    );
                }
                return unsupported(
                    "--prefix --unbrewed lists files Homebrew did not install; an abrew prefix has no Homebrew repository checkout to audit",
                );
            }
            let installed_only = cmd == "--prefix" && has_flag(rest, "--installed", "");
            if installed_only && names.is_empty() {
                return usage_failed(
                    cmd,
                    "Error: Invalid usage: `--installed` requires a formula argument.",
                );
            }
            if !names.is_empty() {
                let api0 = api::Api::new(prefix.clone());
                return commands::cmd_prefix_queries(prefix, &api0, cmd, &names, installed_only);
            }
            let bare = match cmd {
                "--prefix" => prefix.root.clone(),
                "--cellar" => prefix.cellar(),
                _ => prefix.caskroom(),
            };
            println!("{}", bare.display());
            return ExitCode::SUCCESS;
        }
        "command" => return cmd_command_path(prefix, rest),
        "formula" => return commands::cmd_formula_path(prefix, rest),
        "generate-cask-token" => {
            let api0 = api::Api::new(prefix.clone());
            return commands::cmd_generate_cask_token(&api0, rest);
        }
        "command-not-found-init" => return commands::cmd_command_not_found_init(),
        "services" => return services::cmd_services(prefix, rest),
        "postinstall" => {
            let api0 = api::Api::new(prefix.clone());
            return commands::cmd_postinstall(prefix, &api0, rest);
        }
        "unpack" => {
            let api0 = api::Api::new(prefix.clone());
            return commands::cmd_unpack(prefix, &api0, args, rest);
        }
        "alias" => {
            return match aliases::add_or_show(prefix, operands(rest).first().map(String::as_str)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => failed(&message),
            };
        }
        "unalias" => {
            return match aliases::remove(prefix, &operands(rest)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => failed(&message),
            };
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
            if rest.is_empty() {
                return unsupported(
                    "--repository without a tap: atpkg has no Homebrew source repository",
                );
            }
            for name in rest {
                match tap::parse_tap_name(name) {
                    Some((user, repo)) => println!(
                        "{}",
                        prefix
                            .taps_dir()
                            .join(user)
                            .join(format!("homebrew-{repo}"))
                            .display()
                    ),
                    None => {
                        return from_install(
                            prefix,
                            args,
                            Err(InstallError::Failed(format!(
                                "Error: Invalid tap name: {name}"
                            ))),
                        );
                    }
                }
            }
            return ExitCode::SUCCESS;
        }
        "--env" | "env" => return print_env(prefix, rest),
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
                    if bundle::entry_installed(prefix, entry) {
                        Ok(())
                    } else {
                        install::preflight_plan(&api0, prefix, n, &opts)
                    }
                }
                bundle::BrewfileEntry::Cask(n) => {
                    if bundle::entry_installed(prefix, entry) {
                        Ok(())
                    } else if n.contains('/') && !n.starts_with("homebrew/cask/") {
                        Err(InstallError::Unsupported(format!(
                            "qualified cask {n} needs its tap recipe"
                        )))
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
        "search" | "-S" => cmd_search(prefix, &api, rest),
        "info" => cmd_info(prefix, &api, args, rest),
        "desc" => cmd_desc(prefix, &api, rest),
        "home" | "homepage" => cmd_home(&api, rest),
        "docs" => cmd_docs(),
        "source" => cmd_source(&api, rest),
        "cat" => cmd_cat(prefix, &api, rest),
        "formulae" => list_index(prefix, &api, false),
        "casks" => list_index(prefix, &api, true),
        "commands" => {
            if has_flag(rest, "--include-aliases", "") && !has_flag(rest, "--quiet", "-q") {
                eprintln!(
                    "Error: Invalid usage: `--include-aliases` cannot be passed without `--quiet`."
                );
                return ExitCode::from(1);
            }
            if has_flag(rest, "--quiet", "-q") {
                let names: Vec<String> = command_names(has_flag(rest, "--include-aliases", ""))
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                commands::print_columns_pub(&names);
            } else {
                let mut first = true;
                for (title, names) in [
                    ("Built-in commands", BREW_BUILTIN_COMMANDS),
                    ("Built-in developer commands", BREW_DEVELOPER_COMMANDS),
                ] {
                    if !first {
                        println!();
                    }
                    first = false;
                    println!("{}", tty::ohai(title));
                    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
                    commands::print_columns_pub(&names);
                }
            }
            ExitCode::SUCCESS
        }
        "completions" => commands::cmd_completions(prefix, rest),
        "config" => cmd_config(prefix),
        "shellenv" => cmd_shellenv(prefix, rest),
        "doctor" | "dr" => cmd_doctor(prefix, &api, rest),
        "analytics" => commands::cmd_analytics(),
        "developer" => commands::cmd_developer(prefix, rest),
        "install" => cmd_install(prefix, &api, args, rest),
        "uninstall" | "remove" | "rm" => cmd_uninstall(prefix, &api, args, rest),
        "zap" => cmd_zap(prefix, &api, args, rest),
        "reinstall" => cmd_reinstall(prefix, &api, args, rest),
        "upgrade" => cmd_upgrade(prefix, &api, args, rest),
        "outdated" => cmd_outdated(prefix, &api, rest),
        "update" => cmd_update(prefix, &api),
        "cleanup" => cmd_cleanup(prefix, &api, rest),
        "autoremove" => cmd_autoremove(prefix, &api, rest),
        "pin" => {
            if operands(rest).is_empty() {
                eprintln!(
                    "Error: Invalid usage: This command requires at least 1 installed formula or installed cask argument."
                );
                return ExitCode::from(1);
            }
            let want_cask = has_flag(rest, "--cask", "");
            let want_formula = has_flag(rest, "--formula", "");
            each_name(rest, |n| {
                let short_owned =
                    commands::canonical_installed_name(prefix, &api, Formula::short_name(n), false);
                let short = short_owned.as_str();
                let token_owned = commands::canonical_installed_name(
                    prefix,
                    &api,
                    api::canonical_cask_token(short),
                    true,
                );
                let token = token_owned.as_str();
                if !want_cask && prefix.installed_one(prefix::Kind::Formula, short).is_some() {
                    if prefix.pinned_version(short).is_some() {
                        tty::opoo!("Warning: {short} already pinned");
                        Ok(())
                    } else {
                        prefix.pin(short)
                    }
                } else if !want_formula && prefix.installed_one(prefix::Kind::Cask, token).is_some()
                {
                    if prefix.cask_pinned_version(token).is_some() {
                        tty::opoo!("Warning: {token} already pinned");
                        Ok(())
                    } else {
                        let pinned = prefix.pin_cask(token);
                        if pinned.is_ok() && api.cask(token).is_ok_and(|c| c.auto_updates) {
                            tty::opoo!(
                                "Warning: {token} has `auto_updates true` and may update itself outside Homebrew despite being pinned."
                            );
                        }
                        pinned
                    }
                } else if want_cask {
                    if commands::package_definitively_unknown(prefix, &api, token, false, true) {
                        Err(format!("Error: {}", commands::cask_unavailable(token)))
                    } else {
                        Err(format!("Error: {token} not installed"))
                    }
                } else if want_formula {
                    if commands::package_definitively_unknown(prefix, &api, n, true, false) {
                        Err(format!("Error: {}", commands::formula_unavailable(&api, n)))
                    } else {
                        Err(format!("Error: {short} not installed"))
                    }
                } else if commands::package_definitively_unknown(prefix, &api, n, true, true) {
                    Err(format!("Error: {}", commands::formula_unavailable(&api, n)))
                } else {
                    Err(format!("Error: {short} not installed"))
                }
            })
        }
        "unpin" => cmd_unpin(prefix, &api, rest),
        "tab" => cmd_tab(prefix, &api, rest),
        "which-formula" => cmd_which_formula(prefix, &api, rest),
        "link" | "ln" => cmd_link(prefix, &api, args, rest, true),
        "unlink" => cmd_link(prefix, &api, args, rest, false),
        "switch" => cmd_switch(prefix, &api, args, rest),
        "list" | "ls" => cmd_list(prefix, &api, rest),
        "fetch" => cmd_fetch(prefix, &api, args, rest),
        "deps" => cmd_deps(prefix, &api, rest),
        "uses" | "dependents" => cmd_uses(prefix, &api, rest),
        "leaves" => cmd_leaves(prefix, &api, rest),
        "missing" => cmd_missing(prefix, &api, rest),
        "migrate" => cmd_migrate(prefix, &api, args, rest),
        "options" => cmd_options(prefix, &api, args, rest),
        "linkage" => cmd_linkage(prefix, &api, rest),
        "log" => commands::cmd_log(prefix, &api, rest),
        "tap" => cmd_tap(prefix, rest),
        "trust" => cmd_trust(prefix, &api, rest),
        "untrust" => cmd_untrust(prefix, &api, rest),
        "untap" => {
            let names = operands(rest);
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            match tap::remove_all(prefix, &refs) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => failed(&e),
            }
        }
        "tap-info" => cmd_tap_info(prefix, &api, rest),
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
    // Brew's parser: `--` ends switch parsing; what follows
    // are named operands verbatim, dashes and all.
    let mut out = Vec::new();
    let mut literal = false;
    for s in rest {
        if literal {
            out.push(s.clone());
        } else if s == "--" {
            literal = true;
        } else if !s.starts_with('-') {
            out.push(s.clone());
        }
    }
    out
}

fn has_flag(rest: &[String], long: &str, short: &str) -> bool {
    rest.iter()
        .take_while(|s| s.as_str() != "--")
        .any(|s| s == long || (!short.is_empty() && s == short))
}

/// Whether brew reaches its Ruby startup for this invocation
/// (shell helpers in `bin/brew`/`brew.sh` and the abrew stub
/// controls never do, and unknown commands die first): the
/// shared gate for the startup env behaviours below — the
/// deprecated wrapper-variable warnings and the disabled
/// internal-API env, both ahead of argument parsing.
fn reaches_ruby_startup(cmd: &str, rest: &[String]) -> bool {
    let canonical = match cmd {
        "ls" => "list",
        "up" => "update",
        other => other,
    };
    // Shell paths (brew.sh / ShellCommand) and the abrew
    // stub controls never fetch API files.
    if matches!(
        canonical,
        "--version"
            | "-v"
            | "--help"
            | "-h"
            | "help"
            | "--prefix"
            | "--cellar"
            | "--caskroom"
            | "--repository"
            | "--repo"
            | "--taps"
            | "shellenv"
            | "update"
            | "which-formula"
            | "exec"
            | "on"
            | "off"
            | "status"
    ) {
        return false;
    }
    // Plain `list` and bare `tap`/`--cache` are shell forms;
    // their Ruby forms fetch.
    if canonical == "list"
        && operands(rest).is_empty()
        && !rest.iter().any(|arg| {
            matches!(
                arg.split('=').next().unwrap_or(arg),
                "--versions" | "--pinned" | "--multiple" | "--full-name" | "--json"
            )
        })
    {
        return false;
    }
    if canonical == "tap" && rest.is_empty() {
        return false;
    }
    if canonical == "--cache"
        && operands_without_flag_values(rest, &["--os", "--arch", "--bottle-tag"]).is_empty()
        && valued_flag_last(rest, "--os").is_none()
        && valued_flag_last(rest, "--arch").is_none()
    {
        return false;
    }
    COMMANDS.contains(&cmd)
        || BREW_BUILTIN_COMMANDS.contains(&cmd)
        || BREW_DEVELOPER_COMMANDS.contains(&cmd)
        || command_names(true).contains(&cmd)
}

/// `HOMEBREW_USE_INTERNAL_API` is a disabled boolean env
/// brew reads while fetching API files at Ruby-command
/// startup, ahead of argument parsing; any non-blank value
/// (including `0`/`false`) errors.
fn internal_api_env_error(
    cmd: &str,
    rest: &[String],
    get: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if !reaches_ruby_startup(cmd, rest) {
        return None;
    }
    get("HOMEBREW_USE_INTERNAL_API")
        .filter(|value| value.chars().any(|c| !c.is_whitespace()))
        .map(|_| {
            "Error: Calling HOMEBREW_USE_INTERNAL_API is disabled! \
             Use the default behaviour instead."
                .to_string()
        })
}

/// Print brew's warnings for deprecated (warn-tier) env vars
/// in read order; under developer mode the first present one
/// is an error instead (`disable_for_developers`), returned
/// for the caller to fail with. Any non-blank value counts.
fn deprecated_env_warnings(
    vars: &[(&str, &str)],
    get: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let present =
        |key: &str| get(key).is_some_and(|value| value.chars().any(|c| !c.is_whitespace()));
    let developer = present("HOMEBREW_DEVELOPER");
    for (name, replacement) in vars {
        if present(name) {
            let message = format!("Calling {name} is deprecated! Use {replacement} instead.");
            if developer {
                return Some(format!("Error: {message}"));
            }
            tty::opoo!("Warning: {message}");
        }
    }
    None
}

/// The deprecated wrapper envs brew reads for every Ruby
/// command (`check_deprecated_bash_variables`), in read order.
const WRAPPER_DEPRECATED_ENVS: &[(&str, &str)] = &[
    ("HOMEBREW_FORCE_BREW_WRAPPER", "your wrapper directly"),
    (
        "HOMEBREW_FORCE_BREW_WRAPPER_HELP_MESSAGE",
        "custom help in your wrapper",
    ),
    (
        "HOMEBREW_NO_FORCE_BREW_WRAPPER",
        "an environment without $HOMEBREW_NO_FORCE_BREW_WRAPPER",
    ),
];

/// The deprecated bundle cleanup envs the bundle parser reads.
const BUNDLE_DEPRECATED_ENVS: &[(&str, &str)] = &[
    (
        "HOMEBREW_BUNDLE_INSTALL_CLEANUP",
        "`brew bundle cleanup` after `brew bundle install`",
    ),
    (
        "HOMEBREW_BUNDLE_FORCE_INSTALL_CLEANUP",
        "`brew bundle cleanup --force` after `brew bundle install`",
    ),
];

/// Brew's disabled environment variables for the switches
/// above: brew's parser reads the switch's env var while
/// building, so the env error precedes any switch error.
/// `HOMEBREW_EVAL_ALL` errors with any non-empty value;
/// `HOMEBREW_ASK` is boolean (`0`/`false` stay quiet).
fn disabled_env_error(cmd: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let present = |key: &str| get(key).filter(|value| !value.is_empty());
    let truthy = |key: &str| {
        present(key).is_some_and(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
    };
    if matches!(
        cmd,
        "deps"
            | "desc"
            | "info"
            | "options"
            | "readall"
            | "search"
            | "uses"
            | "audit"
            | "bump"
            | "determine-test-runners"
            | "livecheck"
            | "unbottled"
    ) && present("HOMEBREW_EVAL_ALL").is_some()
    {
        return Some(
            "Error: Calling HOMEBREW_EVAL_ALL is disabled! \
             Use the default trusted-tap behaviour instead."
                .to_string(),
        );
    }
    if matches!(cmd, "install" | "upgrade" | "reinstall") {
        if truthy("HOMEBREW_ASK") {
            return Some(
                "Error: Calling HOMEBREW_ASK is disabled! Use the default behaviour instead."
                    .to_string(),
            );
        }
        // The shared cask switches read these while their
        // parsers build; presence alone (even `0`) errors.
        for key in [
            "HOMEBREW_CASK_OPTS_BINARIES",
            "HOMEBREW_CASK_OPTS_REQUIRE_SHA",
        ] {
            if present(key).is_some() {
                return Some(format!(
                    "Error: Calling {key} is disabled! Use HOMEBREW_CASK_OPTS instead."
                ));
            }
        }
    }
    if cmd == "bundle" {
        // Every bundle subcommand reads these (counter-vars
        // suppress their pair); brew reads them in this order.
        if truthy("HOMEBREW_BUNDLE_NO_SECRETS") && !truthy("HOMEBREW_BUNDLE_SECRETS") {
            return Some(
                "Error: Calling HOMEBREW_BUNDLE_NO_SECRETS is disabled! \
                 Use the default behaviour instead."
                    .to_string(),
            );
        }
        if truthy("HOMEBREW_BUNDLE_DESCRIBE") && !truthy("HOMEBREW_BUNDLE_NO_DESCRIBE") {
            return Some(
                "Error: Calling HOMEBREW_BUNDLE_DESCRIBE is disabled! \
                 Use the default behaviour instead."
                    .to_string(),
            );
        }
        if present("HOMEBREW_BUNDLE_DUMP_DESCRIBE").is_some() {
            return Some(
                "Error: Calling HOMEBREW_BUNDLE_DUMP_DESCRIBE is disabled! \
                 Use HOMEBREW_BUNDLE_DESCRIBE instead."
                    .to_string(),
            );
        }
    }
    None
}

/// Like `has_flag` for value flags, which also arrive as
/// `--flag=value`.
fn has_flag_eq(rest: &[String], long: &str) -> bool {
    rest.iter()
        .take_while(|s| s.as_str() != "--")
        .any(|s| s == long || s.starts_with(&format!("{long}=")))
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
    "docs",
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
    "migrate",
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
    "source",
    "status",
    "switch",
    "tab",
    "tap",
    "tap-info",
    "uninstall",
    "unlink",
    "unpin",
    "untap",
    "update",
    "upgrade",
    "uses",
    "which-formula",
    "zap",
];

/// Brew 7.0's built-in command inventory, in brew's own listing
/// order (what `commands` shows; every name routes natively or
/// delegates, so the list is the honest CLI surface).
const BREW_BUILTIN_COMMANDS: &[&str] = &[
    "--cache",
    "--caskroom",
    "--cellar",
    "--env",
    "--prefix",
    "--repository",
    "--taps",
    "--version",
    "alias",
    "analytics",
    "as-brew-user",
    "as-console-user",
    "autoremove",
    "bundle",
    "casks",
    "cleanup",
    "command-not-found-init",
    "command",
    "commands",
    "completions",
    "config",
    "deps",
    "desc",
    "developer",
    "docs",
    "doctor",
    "exec",
    "fetch",
    "formulae",
    "gist-logs",
    "help",
    "home",
    "info",
    "install",
    "leaves",
    "link",
    "list",
    "log",
    "mcp-server",
    "migrate",
    "missing",
    "nodenv-sync",
    "options",
    "outdated",
    "pin",
    "postinstall",
    "pyenv-sync",
    "rbenv-sync",
    "readall",
    "reinstall",
    "sandbox-exec",
    "search",
    "services",
    "setup-ruby",
    "shellenv",
    "source",
    "tab",
    "tap-info",
    "tap",
    "trust",
    "unalias",
    "uninstall",
    "unlink",
    "unpin",
    "untap",
    "untrust",
    "update-if-needed",
    "update-reset",
    "update",
    "upgrade",
    "uses",
    "version-install",
    "vulns",
    "which-formula",
];
const BREW_DEVELOPER_COMMANDS: &[&str] = &[
    "aspell-dictionaries",
    "audit",
    "benchmark",
    "bottle",
    "bump-cask-pr",
    "bump-compatibility-version",
    "bump-formula-pr",
    "bump-revision",
    "bump-unversioned-casks",
    "bump",
    "cat",
    "check-bottle-modification",
    "check-ci-status",
    "contributions",
    "create",
    "debugger",
    "determine-rebottle-runners",
    "edit",
    "extract",
    "find-appcast",
    "formula",
    "generate-cask-token",
    "generate-man-completions",
    "generate-zap",
    "install-bundler-gems",
    "irb",
    "lgtm",
    "linkage",
    "livecheck",
    "prof",
    "rubocop",
    "ruby",
    "rubydoc",
    "sh",
    "style",
    "tap-new",
    "test-bot",
    "test",
    "tests",
    "typecheck",
    "unbottled",
    "unpack",
    "update-perl-resources",
    "update-python-resources",
    "update-test",
    "vendor-gems",
    "verify",
    "which-update",
];
/// Aliases brew adds under `--include-aliases` (beyond abrew's own
/// `COMMAND_ALIASES` subset used for dispatch).
const BREW_COMMAND_ALIASES: &[&str] = &[
    "--config",
    "--repo",
    "-S",
    "-v",
    "abv",
    "dr",
    "environment",
    "homepage",
    "instal",
    "lc",
    "ln",
    "ls",
    "post_install",
    "remove",
    "rm",
    "tc",
    "uninstal",
    "up",
    "x",
];

fn command_names(include_aliases: bool) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = BREW_BUILTIN_COMMANDS
        .iter()
        .chain(BREW_DEVELOPER_COMMANDS)
        .copied()
        .collect();
    if include_aliases {
        names.extend(BREW_COMMAND_ALIASES.iter().copied());
    }
    names.sort_unstable();
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
  pin, unpin, link, unlink, tab   manage kegs
  tap, untap                      manage taps
  bundle                          native Brewfile install, check and dump
  cleanup, autoremove, fetch      housekeeping
  completions [state|link|unlink] tap shell-completion linking
  on, off, status                 enable, disable or inspect native brew

abrew pours bottles that need no relocation (or, on macOS with the developer
tools, `:any` relocation) and installs tar, zip, DMG and single-file casks
whose artifacts and constraints are plain data. Unsupported source builds,
post-install recipes, pkg casks, service control and flags fail with their missing
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
        "migrate" => {
            "Usage: abrew migrate [--force] [--dry-run] [--formula|--cask] <oldname>…\n  Migrates renamed formulae and casks to their new names: moves the cellar\n  or caskroom, retargets links, pin and opt records, and leaves the old\n  rack or caskroom as a symlink.\n"
        }
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
        "link" | "ln" | "unlink" => {
            "Usage: abrew link|unlink [--dry-run|-n] [--force|-f] [--formula|--cask] <formula|cask>\n"
        }
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
            "Usage: abrew services [list] [--json]\n       abrew services info (<formula>|--all) [--json]\n  List and inspect launchd services of formulae. Starting and\n  stopping services is not native.\n"
        }
        "tab" => {
            "Usage: abrew tab [--formula|--cask] (--installed-on-request|--no-installed-on-request) <name>…\n  Marks installed packages as installed on request or not, which is what\n  autoremove keeps or removes.\n"
        }
        "which-formula" => {
            "Usage: abrew which-formula [--explain] <command>…\n  Shows which formula provides each command, from the public executables\n  database; --explain phrases it as install advice.\n"
        }
        "docs" => {
            "Usage: abrew docs\n  Opens the Homebrew documentation at https://docs.brew.sh in a browser.\n"
        }
        "source" => {
            "Usage: abrew source [<formula>…]\n  Opens each formula's source repository in a browser, derived from its\n  head, stable or homepage URL; no name opens Homebrew's own repository.\n"
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
    fn brew_help_texts_cover_commands_and_aliases() {
        assert!(help_texts::TOP.starts_with("Example usage:\n  brew search"));
        let install = brew_help_text("install");
        assert!(
            install.starts_with("Usage: brew install [options] formula|cask"),
            "{install:.60}"
        );
        assert!(install.ends_with("Show this message.\n"));
        // Aliases resolve to the canonical command's text.
        assert_eq!(brew_help_text("ls"), brew_help_text("list"));
        assert_eq!(brew_help_text("instal"), install);
        // Unknown topics fall back to the top-level message.
        assert_eq!(brew_help_text("frobnicate"), help_texts::TOP);
    }

    #[test]
    fn command_names_merge_aliases_sorted() {
        let plain = command_names(false);
        assert_eq!(
            plain.len(),
            BREW_BUILTIN_COMMANDS.len() + BREW_DEVELOPER_COMMANDS.len()
        );
        assert!(!plain.contains(&"ls"));
        assert!(plain.windows(2).all(|w| w[0] < w[1]), "plain is sorted");
        let merged = command_names(true);
        assert_eq!(merged.len(), plain.len() + BREW_COMMAND_ALIASES.len());
        assert!(merged.windows(2).all(|w| w[0] < w[1]), "merged is sorted");
        for alias in BREW_COMMAND_ALIASES {
            assert!(merged.contains(alias), "{alias} listed");
            assert!(!plain.contains(alias), "{alias} not duplicated");
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
            ("info", vec!["--analytics", "--json", "wget"]),
            ("info", vec!["--analytics", "--installed", "wget"]),
            ("info", vec!["--analytics", "--days", "30"]),
            ("info", vec!["--analytics", "--category", "install"]),
            (
                "info",
                vec!["--analytics", "--days=30", "--category=install"],
            ),
            ("link", vec!["--HEAD", "wget"]),
            ("services", vec!["start", "wget"]),
            ("--cache", vec!["-s", "wget"]),
            ("--cache", vec!["--HEAD", "wget"]),
            ("desc", vec!["-s", "/(?=http)client/"]),
            ("missing", vec!["--hide=../outside"]),
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
            ("completions", vec![]),
            ("completions", vec!["state"]),
            ("completions", vec!["link"]),
            ("completions", vec!["unlink"]),
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
            ("pin", vec![]),
            ("pin", vec!["wget", "curl"]),
            ("unpin", vec![]),
            ("unpin", vec!["wget", "curl"]),
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
            ("info", vec!["--analytics", "wget", "--days="]),
            ("info", vec!["--analytics", "wget", "--category="]),
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
            // Brew's shell fast paths: arguments are data or
            // ignored, help flags included.
            ("shellenv", vec!["--help"]),
            ("shellenv", vec!["--bogus", "fish"]),
            ("formulae", vec!["--help"]),
            ("casks", vec!["--bogus"]),
            ("--taps", vec!["anything"]),
            ("--repository", vec!["--help"]),
            ("--repository", vec!["notatap"]),
            ("--repo", vec!["a/b/c"]),
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
    fn deprecated_envs_escalate_only_for_developers() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        // Absent or blank: nothing (and nothing printed).
        assert!(deprecated_env_warnings(WRAPPER_DEPRECATED_ENVS, env(&[])).is_none());
        assert!(
            deprecated_env_warnings(
                WRAPPER_DEPRECATED_ENVS,
                env(&[("HOMEBREW_FORCE_BREW_WRAPPER", "")])
            )
            .is_none()
        );
        // Developer mode turns the first present warning, in
        // read order, into the error.
        assert_eq!(
            deprecated_env_warnings(
                WRAPPER_DEPRECATED_ENVS,
                env(&[
                    ("HOMEBREW_DEVELOPER", "1"),
                    ("HOMEBREW_NO_FORCE_BREW_WRAPPER", "0")
                ])
            )
            .as_deref(),
            Some(
                "Error: Calling HOMEBREW_NO_FORCE_BREW_WRAPPER is deprecated! \
                 Use an environment without $HOMEBREW_NO_FORCE_BREW_WRAPPER instead."
            )
        );
        assert_eq!(
            deprecated_env_warnings(
                BUNDLE_DEPRECATED_ENVS,
                env(&[
                    ("HOMEBREW_DEVELOPER", "1"),
                    ("HOMEBREW_BUNDLE_FORCE_INSTALL_CLEANUP", "1"),
                    ("HOMEBREW_BUNDLE_INSTALL_CLEANUP", "1"),
                ])
            )
            .as_deref(),
            Some(
                "Error: Calling HOMEBREW_BUNDLE_INSTALL_CLEANUP is deprecated! \
                 Use `brew bundle cleanup` after `brew bundle install` instead."
            ),
            "install-cleanup is read before its force sibling"
        );
    }

    #[test]
    fn internal_api_env_error_scopes_like_brew() {
        let set = |_: &str| Some("1".to_string());
        let err = "Error: Calling HOMEBREW_USE_INTERNAL_API is disabled! \
                   Use the default behaviour instead.";
        // Ruby commands fetch API files before parsing.
        assert_eq!(
            internal_api_env_error("info", &argv(&["jq"]), set).as_deref(),
            Some(err)
        );
        assert_eq!(
            internal_api_env_error("deps", &argv(&["--quiet", "--verbose"]), set).as_deref(),
            Some(err),
            "the fetch precedes parser conflict errors"
        );
        // Falsy-but-present values still error (boolean :set);
        // blank stays quiet.
        assert!(internal_api_env_error("info", &argv(&[]), |_| Some("0".to_string())).is_some());
        assert!(internal_api_env_error("info", &argv(&[]), |_| Some(String::new())).is_none());
        assert!(internal_api_env_error("info", &argv(&[]), |_| None).is_none());
        // Shell paths and unknown commands never reach it.
        assert!(internal_api_env_error("--prefix", &argv(&["jq"]), set).is_none());
        assert!(internal_api_env_error("--version", &argv(&[]), set).is_none());
        assert!(internal_api_env_error("which-formula", &argv(&["jq"]), set).is_none());
        assert!(internal_api_env_error("no-such-cmd", &argv(&[]), set).is_none());
        assert!(internal_api_env_error("list", &argv(&[]), set).is_none());
        assert!(internal_api_env_error("list", &argv(&["--versions"]), set).is_some());
        assert!(internal_api_env_error("tap", &argv(&[]), set).is_none());
        assert!(internal_api_env_error("--cache", &argv(&[]), set).is_none());
        assert!(internal_api_env_error("--cache", &argv(&["jq"]), set).is_some());
    }

    #[test]
    fn unique_option_abbreviations_expand() {
        let (_, rest) = native_args("info", &argv(&["--j=v1", "wget"])).unwrap();
        assert_eq!(rest, argv(&["--json=v1", "wget"]));
        let (_, rest) = native_args("list", &argv(&["--pin"])).unwrap();
        assert_eq!(rest, argv(&["--pinned"]));
        // Ambiguous prefixes stay typed (brew error) and a
        // value after a value-flag is never rewritten.
        assert!(matches!(
            native_args("list", &argv(&["--p"])),
            Err(InstallError::Failed(_))
        ));
        let (_, rest) = native_args("deps", &argv(&["--os", "linux", "--tre", "wget"])).unwrap();
        assert_eq!(rest, argv(&["--os", "linux", "--tree", "wget"]));
    }

    #[test]
    fn deps_os_arch_simulation_values_admit() {
        let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
            Err(InstallError::Failed(m)) => m,
            other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
        };
        assert_eq!(
            failed("deps", &["--os=all", "curl"]),
            "Error: Invalid usage: `brew deps --os=all` is not supported."
        );
        assert_eq!(
            failed("deps", &["--arch=all", "curl"]),
            "Error: Invalid usage: `brew deps --arch=all` is not supported."
        );
        assert!(matches!(
            native_args("deps", &argv(&["--os=linux", "--arch=intel", "curl"])),
            Ok((None, _))
        ));
        assert!(matches!(
            native_args("deps", &argv(&["--os", "sequoia", "curl"])),
            Ok((None, _))
        ));
    }

    #[test]
    fn minimum_version_arity_is_a_usage_error() {
        let arity = "Error: Invalid usage: `--minimum-version` requires exactly one formula or cask argument.";
        let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
            Err(InstallError::Failed(m)) => m,
            other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
        };
        assert_eq!(failed("outdated", &["--minimum-version=1.0"]), arity);
        assert_eq!(
            failed("outdated", &["--min-version", "1.0", "jq", "wget"]),
            arity
        );
        assert!(matches!(
            native_args("outdated", &argv(&["--minimum-version=1.0", "jq"])),
            Ok((None, _))
        ));
        assert!(matches!(
            native_args("outdated", &argv(&["--minimum-version", "1.0", "jq"])),
            Ok((None, _))
        ));
        // A blank value is absent: no arity demand.
        assert!(matches!(
            native_args("outdated", &argv(&["--minimum-version=", "jq", "wget"])),
            Ok((None, _))
        ));
        // upgrade shares the arity error and the native flag.
        assert_eq!(failed("upgrade", &["--minimum-version=1.0"]), arity);
        assert!(matches!(
            native_args("upgrade", &argv(&["--minimum-version=1.0", "jq"])),
            Ok((None, _))
        ));
        // The long spelling wins over the alias, blank included.
        assert!(matches!(
            native_args(
                "outdated",
                &argv(&["--min-version=1.0", "--minimum-version=", "jq", "wget"])
            ),
            Ok((None, _))
        ));
    }

    #[test]
    fn deps_brewfile_rows_gate_admission() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let file = temp.path().join("Brewfile");
        let flag = format!("--brewfile={}", file.to_str().unwrap());
        // Options and other entry types are read past: deps
        // uses only the brew/cask names.
        std::fs::write(
            &file,
            "tap \"me/tools\"\nbrew \"wget\", link: false\ncask \"firefox\"\nmas \"Xcode\", id: 497799835\nvscode \"dbaeumer.vscode-eslint\"\ncask_args appdir: \"/Applications\"\n",
        )
        .unwrap();
        assert!(matches!(
            native_args("deps", &argv(&[&flag])),
            Ok((None, _))
        ));
        // Ruby the line parser cannot read delegates.
        std::fs::write(&file, "brew \"wget\" if OS.mac?\n").unwrap();
        assert!(matches!(
            native_args("deps", &argv(&[&flag])),
            Err(InstallError::Unsupported(_))
        ));
        // A missing file is admitted: the command reports
        // brew's own "No Brewfile found".
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(
            native_args("deps", &argv(&[&flag])),
            Ok((None, _))
        ));
    }

    #[test]
    fn bundle_edit_admission_requires_names_and_one_type() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let file = temp.path().join("Brewfile");
        std::fs::write(&file, "brew \"wget\"\n").unwrap();
        let flag = format!("--file={}", file.to_str().unwrap());
        let failed = |args: &[&str]| match native_args("bundle", &argv(args)) {
            Err(InstallError::Failed(m)) => m,
            other => panic!("expected Failed, got {other:?}"),
        };
        assert_eq!(
            failed(&["add"]),
            "Error: Invalid usage: This command requires at least 1 named argument."
        );
        assert_eq!(
            failed(&["add", "--formula", "--cask", "wget"]),
            "Error: Invalid usage: `add` supports only one type of entry at a time."
        );
        assert_eq!(
            failed(&["add", "--mas", "Xcode"]),
            "Error: Invalid usage: The `add` subcommand does not accept the `--mas` switch."
        );
        assert!(matches!(
            native_args("bundle", &argv(&["add", "wget"])),
            Ok((None, _))
        ));
        assert!(matches!(
            native_args("bundle", &argv(&["remove", &flag, "wget"])),
            Ok((None, _))
        ));
        assert_eq!(
            failed(&["remove", &flag]),
            "Error: Invalid usage: This command requires at least 1 named argument."
        );
        // Bare remove (direct dialect) stays the payload verb.
        assert!(matches!(
            native_args("bundle", &argv(&["remove"])),
            Ok((None, _))
        ));
    }

    #[test]
    fn plural_kind_switches_canonicalize_exact_tokens_only() {
        assert_eq!(
            canonical_kind_switches(&argv(&["--formulae", "wget", "--casks"])),
            argv(&["--formula", "wget", "--cask"])
        );
        let once = canonical_kind_switches(&argv(&["--formula", "--hide=--casks", "casks"]));
        assert_eq!(canonical_kind_switches(&once), once);
        assert_eq!(
            native_args("tap", &argv(&["--formulae"])),
            Err(InstallError::Failed(
                "Error: invalid option: --formulae".to_string()
            ))
        );
        assert_eq!(
            native_args("doctor", &argv(&["--casks"])),
            Err(InstallError::Failed(
                "Error: invalid option: --casks".to_string()
            ))
        );
    }

    #[test]
    fn kind_switch_conflicts_are_brew_usage_errors() {
        let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
            Err(InstallError::Failed(m)) => m,
            other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
        };
        assert_eq!(
            failed("install", &["--formula", "--cask", "acl"]),
            "Error: Invalid usage: Options --cask and --formulae are mutually exclusive."
        );
        assert_eq!(
            failed("install", &["--ask", "acl"]),
            "Error: Calling the `--ask` switch is disabled! Use the default behaviour instead."
        );
        // Brew's other `odisabled` switches: same parser refusal,
        // replacement text verbatim or "no replacement".
        assert_eq!(
            failed("deps", &["--eval-all", "acl"]),
            "Error: Calling the `--eval-all` switch is disabled! Use the default trusted-tap behaviour instead."
        );
        assert_eq!(
            failed("tap", &["--eval-all"]),
            "Error: Calling the `--eval-all` switch is disabled! Use the default tap verification behaviour instead."
        );
        assert_eq!(
            failed("info", &["--fetch-manifest", "acl"]),
            "Error: Calling the `--fetch-manifest` switch is disabled! There is no replacement."
        );
        assert_eq!(
            failed("ls", &["--installed-as-dependency"]),
            "Error: Calling the `--installed-as-dependency` switch is disabled! Use --no-installed-on-request instead."
        );
        assert_eq!(
            failed("options", &["--command=install"]),
            "Error: Calling the `--command` flag is disabled! There is no replacement."
        );
        assert_eq!(
            failed("up", &["--merge"]),
            "Error: Calling the `--merge` switch is disabled! There is no replacement."
        );
        assert_eq!(
            failed("install", &["--env=std", "acl"]),
            "Error: Calling `brew install --env` is disabled! Use `env :std` in specific formula files instead."
        );
        // Developer commands carry the same parser refusals even
        // though the commands themselves are not native.
        assert_eq!(
            failed("audit", &["--no-signing", "acl"]),
            "Error: Calling the `--[no-]signing` switch is disabled! There is no replacement."
        );
        assert_eq!(
            failed("bottle", &["--committer=x"]),
            "Error: Calling the `--committer` flag is disabled! There is no replacement."
        );
        assert_eq!(
            failed("update-python-resources", &["--silent", "acl"]),
            "Error: Calling the `-s` switch is disabled! There is no replacement."
        );
        assert_eq!(
            failed("test-bot", &["--skip-recursive-dependents"]),
            "Error: Calling the `--[no-]skip-recursive-dependents` switch is disabled! Use the default behaviour instead."
        );
        // Parser conflict groups: every present member is listed
        // in declaration order, short spellings by long name.
        assert_eq!(
            failed("outdated", &["-v", "--json"]),
            "Error: Invalid usage: Options --verbose and --json are mutually exclusive."
        );
        assert_eq!(
            failed("outdated", &["--quiet", "--json", "--verbose"]),
            "Error: Invalid usage: Options --quiet and --verbose and --json are mutually exclusive."
        );
        assert_eq!(
            failed(
                "leaves",
                &["--installed-on-request", "--installed-as-dependency"]
            ),
            "Error: Invalid usage: Options --installed-on-request and --installed-as-dependency are mutually exclusive."
        );
        assert_eq!(
            failed("log", &["-1", "--max-count=2", "acl"]),
            "Error: Invalid usage: Options -1 and --max-count are mutually exclusive."
        );
        assert_eq!(
            failed("trust", &["--tap", "--formula", "--cask", "x/y"]),
            "Error: Invalid usage: Options --tap and --formula and --cask are mutually exclusive."
        );
        // Bundle toggles: canonical display names, --file last,
        // and --describe itself is a disabled switch.
        assert_eq!(
            failed("bundle", &["list", "--describe"]),
            "Error: Calling the `--describe` switch is disabled! Use the default behaviour instead."
        );
        assert_eq!(
            failed("bundle", &["check", "--formulae", "--no-brews"]),
            "Error: Invalid usage: Options --formula and --no-formula are mutually exclusive."
        );
        assert_eq!(
            failed("bundle", &["check", "--cask", "--no-cleanup-cask"]),
            "Error: Invalid usage: Options --cask and --no-cleanup-cask are mutually exclusive."
        );
        assert_eq!(
            failed("bundle", &["list", "--mas", "--no-dump-mas"]),
            "Error: Invalid usage: Options --mas and --no-dump-mas are mutually exclusive."
        );
        assert_eq!(
            failed("bundle", &["list", "--file=x", "--global"]),
            "Error: Invalid usage: Options --file and --global are mutually exclusive."
        );
        // Bundle subcommand switch scope, in argv order.
        assert_eq!(
            failed("bundle", &["list", "--no-describe"]),
            "Error: Invalid usage: The `list` subcommand does not accept the `--no-describe` switch."
        );
        assert_eq!(
            failed("bundle", &["list", "--force"]),
            "Error: Invalid usage: The `list` subcommand does not accept the `-f` switch."
        );
        assert_eq!(
            failed("bundle", &["env", "--formula"]),
            "Error: Invalid usage: The `env` subcommand does not accept the `--formula` switch."
        );
        assert_eq!(
            failed("bundle", &["check", "--no-mas"]),
            "Error: Invalid usage: The `check` subcommand does not accept the `--no-mas` switch."
        );
        assert_eq!(
            failed("bundle", &["--no-describe"]),
            "Error: Invalid usage: The `install` subcommand does not accept the `--no-describe` switch."
        );
        // Unknown subcommands (bundle and services) and
        // services switch scope.
        assert_eq!(
            failed("bundle", &["frobnicate"]),
            "Error: Invalid usage: unknown subcommand: `frobnicate`"
        );
        assert_eq!(
            failed("bundle", &["--file", "Brewfile", "frobnicate"]),
            "Error: Invalid usage: unknown subcommand: `frobnicate`"
        );
        assert_eq!(
            failed("services", &["frobnicate"]),
            "Error: Invalid usage: unknown subcommand: `frobnicate`"
        );
        assert_eq!(
            failed("services", &["--file=x"]),
            "Error: Invalid usage: The `list` subcommand does not accept the `--file` flag."
        );
        assert_eq!(
            failed("services", &["stop", "--json"]),
            "Error: Invalid usage: The `stop` subcommand does not accept the `--json` switch."
        );
        // Parser depends_on constraints.
        assert_eq!(
            failed("deps", &["--prune", "acl"]),
            "Error: Invalid usage: `--prune` cannot be passed without `--tree`."
        );
        assert!(native_args("deps", &argv(&["--prune", "--tree", "acl"])).is_ok());
        assert_eq!(
            failed("info", &["--variations", "acl"]),
            "Error: Invalid usage: `--variations` cannot be passed without `--json`."
        );
        assert_eq!(
            failed("search", &["--open", "acl"]),
            "Error: Invalid usage: `--open` cannot be passed without `--pull-request`."
        );
        // Abbreviations expand before the gate, as in brew.
        assert_eq!(
            failed("deps", &["--prun", "acl"]),
            "Error: Invalid usage: `--prune` cannot be passed without `--tree`."
        );
        assert_eq!(
            failed("info", &["--cat", "install"]),
            "Error: Invalid usage: `--category` cannot be passed without `--analytics`."
        );
        // Boolean switches reject =values (needless argument),
        // and explicit JSON versions are validated per command.
        assert_eq!(
            failed("outdated", &["--quiet=x"]),
            "Error: needless argument: --quiet=x"
        );
        assert_eq!(
            failed("outdated", &["-q=x"]),
            "Error: needless argument: -q=x"
        );
        assert_eq!(
            failed("desc", &["--search=x", "acl"]),
            "Error: needless argument: --search=x"
        );
        assert_eq!(
            failed("outdated", &["--json=v1"]),
            "Error: `brew outdated --json=v1` is no longer supported. Use brew outdated --json=v2 instead."
        );
        assert_eq!(
            failed("tap-info", &["--json=v2", "homebrew/core"]),
            "Error: Invalid usage: invalid JSON version: v2"
        );
        assert!(native_args("cleanup", &argv(&["--prune=30"])).is_ok());
        // Required-value flags at the end of the option list.
        assert_eq!(
            failed("outdated", &["--severity"]),
            "Error: missing argument: --severity"
        );
        assert_eq!(
            failed("deps", &["--os", "--"]),
            "Error: missing argument: --os"
        );
        assert!(
            !matches!(
                native_args("deps", &argv(&["--brewfile"])),
                Err(InstallError::Failed(ref m)) if m.contains("missing argument")
            ),
            "optional-value --brewfile never lacks an argument"
        );
        // --os/--arch simulation values are validated as brew's
        // SimulateSystem validates them.
        assert_eq!(
            failed("deps", &["--os=plan9", "acl"]),
            "Error: Unknown OS: plan9"
        );
        assert_eq!(
            failed("deps", &["--os", "--tree", "acl"]),
            "Error: Unknown OS: --tree"
        );
        assert_eq!(
            failed("fetch", &["--arch=arm64", "acl"]),
            "Error: New arch must be :arm or :intel"
        );
        // Universally invalid regex syntax is brew's error,
        // not a delegation.
        assert_eq!(
            failed("search", &["/[/"]),
            "Error: /[/ is not a valid regex."
        );
        assert_eq!(
            failed("search", &["/+/"]),
            "Error: /+/ is not a valid regex."
        );
        assert_eq!(
            failed("desc", &["--search", "/(abc/"]),
            "Error: /(abc/ is not a valid regex."
        );
        assert!(native_args("outdated", &argv(&["--json"])).is_ok());
        assert!(
            native_args("deps", &argv(&["--", "--eval-all"])).is_ok(),
            "a disabled spelling after -- is a named argument"
        );
        assert!(native_args("install", &argv(&["--no-ask", "acl"])).is_ok());
        assert!(native_args("install", &argv(&["-y", "acl"])).is_ok());
        assert!(native_args("upgrade", &argv(&["--yes"])).is_ok());
        assert_eq!(
            failed("info", &["--formula", "--cask", "acl"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert_eq!(
            failed("uninstall", &["--zap", "--formula", "hello"]),
            "Error: Invalid usage: Options --formula and --zap are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--multiple", "--cask", "--pinned"]),
            "Error: Invalid usage: Options --multiple and --cask are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--multiple", "--pinned", "--full-name"]),
            "Error: Invalid usage: Options --pinned and --multiple are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--versions", "-l", "--full-name"]),
            "Error: Invalid usage: Options --versions and -l are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--full-name", "--multiple", "--versions"]),
            "Error: Invalid usage: Options --full-name and --versions are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--pinned", "-1"]),
            "Error: Invalid usage: Options --pinned and -1 are mutually exclusive."
        );
        // Named pinned listings are native.
        assert!(native_args("list", &argv(&["--pinned", "hello"])).is_ok());
        assert!(native_args("list", &argv(&["--pinned", "--versions", "hello"])).is_ok());
        // desc and search accept both kind switches (each handler
        // resolves them brew's way); plain list forms print
        // nothing on brew's Bash path, the rest conflict.
        assert!(native_args("desc", &argv(&["--formula", "--cask", "acl"])).is_ok());
        assert!(native_args("search", &argv(&["--formula", "--cask", "acl"])).is_ok());
        assert!(native_args("list", &argv(&["--formula", "--cask"])).is_ok());
        assert!(native_args("list", &argv(&["--formula", "--cask", "-l"])).is_ok());
        assert_eq!(
            failed("list", &["--formula", "--cask", "hello"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--formula", "--cask", "--versions"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert_eq!(
            failed("list", &["--formula", "--cask", "--json", "--versions"]),
            "Error: `--formula` and `--cask` are mutually exclusive."
        );
        // deps --installed/--missing is the handler's usage error.
        assert!(native_args("deps", &argv(&["--installed", "--missing"])).is_ok());
        assert_eq!(
            failed("pin", &["--formula", "--cask", "acl"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert!(native_args("pin", &argv(&["--cask", "caffeine"])).is_ok());
        assert!(native_args("tab", &argv(&["--installed-on-request", "wget"])).is_ok());
        assert!(native_args("which-formula", &argv(&["git"])).is_ok());
        assert!(native_args("which-formula", &argv(&["--explain", "g++"])).is_ok());
        assert!(native_args("which-formula", &argv(&[])).is_ok());
        assert!(matches!(
            native_args(
                "tab",
                &argv(&["--installed-on-request", "--no-installed-on-request", "wget"])
            ),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: Options --installed-on-request and --no-installed-on-request are mutually exclusive."
        ));
        assert!(matches!(
            native_args("tab", &argv(&["--formula", "--cask", "--installed-on-request", "wget"])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        ));
        assert!(matches!(
            native_args("tab", &argv(&[])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: This command requires at least 1 installed formula or installed cask argument."
        ));
        assert!(native_args("unpin", &argv(&["--formula", "acl"])).is_ok());
        assert_eq!(
            failed("link", &["--formula", "--cask", "acl"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert!(native_args("link", &argv(&["--cask", "caffeine"])).is_ok());
        assert!(native_args("link", &argv(&["--force", "acl"])).is_ok());
        assert!(native_args("unlink", &argv(&["--formula", "-n", "acl"])).is_ok());
        assert!(native_args("link", &argv(&["--overwrite", "acl"])).is_ok());
        assert_eq!(
            native_args("unlink", &argv(&["--overwrite", "acl"])),
            Err(InstallError::Failed(
                "Error: invalid option: --overwrite".to_string()
            ))
        );
        assert!(matches!(
            native_args("link", &argv(&["--HEAD", "acl"])),
            Err(InstallError::Unsupported(_))
        ));
        assert_eq!(
            failed("log", &["--formula", "--cask", "acl"]),
            "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
        );
        assert!(native_args("log", &argv(&["--cask", "caffeine"])).is_ok());
    }

    #[test]
    fn deps_graph_forms_are_admitted() {
        // The browser form is native; the --tree/--graph conflict is
        // the handler's brew usage error, not an admission refusal.
        assert!(native_args("deps", &argv(&["--graph", "jq"])).is_ok());
        assert!(native_args("deps", &argv(&["--graph", "--dot", "jq"])).is_ok());
        assert!(native_args("deps", &argv(&["--tree", "--graph", "jq"])).is_ok());
    }

    #[test]
    fn bundled_short_switches_expand_like_optionparser() {
        assert_eq!(
            canonical_kind_switches(&argv(&["-vq", "acl", "--", "-1l"])),
            argv(&["-v", "-q", "acl", "--", "-1l"])
        );
        assert!(native_args("info", &argv(&["-vq", "acl"])).is_ok());
        assert!(native_args("list", &argv(&["-1l"])).is_ok());
        assert!(native_args("info", &argv(&["-vz", "acl"])).is_err());
    }

    #[test]
    fn brew_build_env_key_order_and_cc_drop_rule() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        let vars = brew_build_env(&prefix);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(&keys[..5], &["CC", "CXX", "OBJC", "OBJCXX", "HOMEBREW_CC"]);
        assert_eq!(keys.last(), Some(&"PATH"));
        assert!(!vars_cc_differs(&vars));
        let custom = vec![
            ("CC".to_string(), "gcc-14".to_string()),
            ("HOMEBREW_CC".to_string(), "clang".to_string()),
        ];
        assert!(vars_cc_differs(&custom));
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
        assert!(msg(&["services", "start", "wget"]).contains("'services' is not native"));
        let cat_args = argv(&["--analytics", "wget", "--category=other"]);
        assert_eq!(
            native_args("info", &cat_args),
            Err(InstallError::Failed(
                "Error: Invalid usage: `--category` must be one of install, \
                 install-on-request, build-error when querying formulae."
                    .replace("                 ", "")
            ))
        );
        assert_eq!(
            native_args("cleanup", &argv(&["--prune=soon"])),
            Err(InstallError::Failed(
                "Error: Invalid usage: `--prune` expects an integer or `all`.".to_string()
            ))
        );
        let days_args = argv(&["--analytics", "wget", "--days=40"]);
        assert_eq!(
            native_args("info", &days_args),
            Err(InstallError::Failed(
                "Error: Invalid usage: `--days` must be one of 30, 90, 365.".to_string()
            ))
        );
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
        assert_eq!(
            dispatch(&fresh, &argv(&["--version", "extra"])),
            ExitCode::SUCCESS,
            "a leading --version ignores everything after it"
        );
        assert_eq!(
            dispatch(&fresh, &argv(&["--verbose"])),
            ExitCode::SUCCESS,
            "global flags alone are the no-arguments help case"
        );
        assert_eq!(dispatch(&fresh, &argv(&["-v", "-v"])), ExitCode::SUCCESS);
        assert_eq!(
            dispatch(&fresh, &argv(&["help", "frobnicate"])),
            ExitCode::from(1),
            "help for an unknown topic is brew's unknown-command error"
        );
        assert!(!fresh.root.exists());
        for q in [
            &["--repository", "homebrew/core"][..],
            &["--repo", "me/tools"][..],
            &["--cache"][..],
        ] {
            assert_eq!(dispatch(&fresh, &argv(q)), ExitCode::SUCCESS, "{q:?}");
            assert!(!fresh.root.exists(), "{q:?}");
        }
        assert!(matches!(
            native_args("frobnicate", &argv(&[])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: Unknown command: brew frobnicate"
        ));
        // Real brew commands abrew does not implement are known,
        // not unknown: they refuse as not native (and delegate
        // under the reroute) instead of claiming no such command.
        assert!(native_args("readall", &argv(&[])).is_ok());
        assert_eq!(
            native_args("readall", &argv(&["--os=plan9"])),
            Err(InstallError::Failed("Error: Unknown OS: plan9".to_string()))
        );
        assert!(matches!(
            native_args("vulns", &argv(&[])),
            Err(InstallError::Unsupported(m)) if m == "'vulns' is not native"
        ));
        assert!(native_args("missing", &argv(&["--hide="])).is_ok());
        assert!(matches!(
            native_args("search", &argv(&[])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: This command requires at least 1 text or regex argument."
        ));
        assert!(matches!(
            native_args("autoremove", &argv(&["wget"])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: This command does not take named arguments."
        ));
        assert!(matches!(
            native_args("info", &argv(&["--json=v9", "wget"])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: invalid JSON version: v9"
        ));
        assert!(matches!(
            native_args("untap", &argv(&["notatap"])),
            Err(InstallError::Failed(m)) if m == "Error: Invalid tap name: 'notatap'"
        ));
        assert!(matches!(
            native_args("list", &argv(&["--json"])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: `brew list --json` requires `--versions`."
        ));
        assert!(msg(&["audit"]).contains("'audit' is not native"));
    }

    #[test]
    fn native_dispatch_refuses_unsupported_work_without_creating_a_prefix() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let fresh = Prefix::at(&temp.path().join("never-created"));
        for args in [
            &["--real", "list"][..],
            &["services", "start", "wget"][..],
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

    #[test]
    fn subcommand_admission_shapes() {
        assert!(native_args("developer", &argv(&[])).is_ok());
        assert!(native_args("developer", &argv(&["on"])).is_ok());
        assert!(matches!(
            native_args("developer", &argv(&["bogus"])),
            Err(InstallError::Failed(_))
        ));
        assert!(native_args("analytics", &argv(&[])).is_ok());
        assert!(native_args("analytics", &argv(&["state"])).is_ok());
        assert!(matches!(
            native_args("analytics", &argv(&["on"])),
            Err(InstallError::Unsupported(_))
        ));
        assert!(matches!(
            native_args("analytics", &argv(&["bogus"])),
            Err(InstallError::Failed(_))
        ));
    }

    #[test]
    fn disabled_env_vars_match_brew() {
        let env = |pairs: &[(&str, &str)]| {
            let owned: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        // Any non-empty HOMEBREW_EVAL_ALL errors on eval commands…
        for value in ["1", "0", "false"] {
            assert_eq!(
                disabled_env_error("deps", env(&[("HOMEBREW_EVAL_ALL", value)])),
                Some(
                    "Error: Calling HOMEBREW_EVAL_ALL is disabled! Use the default trusted-tap behaviour instead."
                        .to_string()
                )
            );
        }
        // …but an empty value is quiet, and other commands never read it.
        assert_eq!(
            disabled_env_error("deps", env(&[("HOMEBREW_EVAL_ALL", "")])),
            None
        );
        assert_eq!(
            disabled_env_error("list", env(&[("HOMEBREW_EVAL_ALL", "1")])),
            None
        );
        assert_eq!(
            disabled_env_error("tap", env(&[("HOMEBREW_EVAL_ALL", "1")])),
            None
        );
        // HOMEBREW_ASK is boolean: truthy errors on install family only.
        assert_eq!(
            disabled_env_error("install", env(&[("HOMEBREW_ASK", "1")])),
            Some(
                "Error: Calling HOMEBREW_ASK is disabled! Use the default behaviour instead."
                    .to_string()
            )
        );
        for quiet in ["0", "false", ""] {
            assert_eq!(
                disabled_env_error("upgrade", env(&[("HOMEBREW_ASK", quiet)])),
                None
            );
        }
        assert_eq!(
            disabled_env_error("deps", env(&[("HOMEBREW_ASK", "1")])),
            None
        );
        // Cask-opts envs: presence (even `0`) errors, after ASK.
        assert_eq!(
            disabled_env_error("reinstall", env(&[("HOMEBREW_CASK_OPTS_BINARIES", "0")])),
            Some(
                "Error: Calling HOMEBREW_CASK_OPTS_BINARIES is disabled! Use HOMEBREW_CASK_OPTS instead."
                    .to_string()
            )
        );
        assert_eq!(
            disabled_env_error(
                "install",
                env(&[
                    ("HOMEBREW_ASK", "1"),
                    ("HOMEBREW_CASK_OPTS_REQUIRE_SHA", "1")
                ])
            ),
            Some(
                "Error: Calling HOMEBREW_ASK is disabled! Use the default behaviour instead."
                    .to_string()
            )
        );
        // Bundle envs: boolean with counter-vars, secrets first.
        assert_eq!(
            disabled_env_error(
                "bundle",
                env(&[
                    ("HOMEBREW_BUNDLE_DESCRIBE", "1"),
                    ("HOMEBREW_BUNDLE_NO_SECRETS", "1")
                ])
            ),
            Some(
                "Error: Calling HOMEBREW_BUNDLE_NO_SECRETS is disabled! Use the default behaviour instead."
                    .to_string()
            )
        );
        assert_eq!(
            disabled_env_error(
                "bundle",
                env(&[
                    ("HOMEBREW_BUNDLE_DESCRIBE", "1"),
                    ("HOMEBREW_BUNDLE_NO_DESCRIBE", "1")
                ])
            ),
            None
        );
        assert_eq!(
            disabled_env_error("bundle", env(&[("HOMEBREW_BUNDLE_DUMP_DESCRIBE", "0")])),
            Some(
                "Error: Calling HOMEBREW_BUNDLE_DUMP_DESCRIBE is disabled! Use HOMEBREW_BUNDLE_DESCRIBE instead."
                    .to_string()
            )
        );
        assert_eq!(
            disabled_env_error("list", env(&[("HOMEBREW_BUNDLE_DESCRIBE", "1")])),
            None
        );
    }

    #[test]
    fn completions_unknown_subcommand_is_invalid_usage() {
        assert!(matches!(
            native_args("completions", &argv(&["bogus"])),
            Err(InstallError::Failed(m)) if m == "Error: Invalid usage: unknown subcommand: `bogus`"
        ));
    }

    #[test]
    fn double_dash_ends_switch_parsing() {
        let rest: Vec<String> = ["--json=v2", "--", "-v", "hello", "--cask"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(operands(&rest), ["-v", "hello", "--cask"]);
        assert!(has_flag(&rest, "--json=v2", ""));
        assert!(!has_flag(&rest, "--cask", ""));
        assert!(!has_flag(&rest, "-v", ""));
    }
}
