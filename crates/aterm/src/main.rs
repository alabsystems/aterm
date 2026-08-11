// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm` — THE one binary.
//!
//! One name, everywhere: the transparent terminal session, the GPU window,
//! and every verb live behind this single executable. Routing is the whole
//! job of this crate — every capability is a library:
//!
//! ```text
//! aterm <verb> …            ctl / pkg / fleet / drive → in-process library calls
//! aterm help [topic]        the toolchain manual (aterm-cli's parser owns it)
//! aterm agents […]          the coding-agent primer installer (aterm-cli owns it)
//! aterm <tool> …            managed-store toolchain dispatch (via `pkg`)
//! aterm            (a TTY)  the transparent session — your shell, modeled live
//! aterm         (no TTY)    the window (a Finder/.app launch has no TTY)
//! aterm --window            the window, explicitly, from anywhere
//! ```
//!
//! ARGV0 COMPAT: the bundle ships symlinks (`aterm-ctl`, `atpkg`,
//! `aterm-fleet`, `aterm-drive`, `aterm-gui`, `aterm-cli`) onto this binary,
//! and old installs symlinked `~/.local/bin/aterm` at a bundled `aterm-cli`.
//! Invoked through any of those names, main dispatches as that tool — so
//! every pre-one-binary script, PATH entry, `$ATERM_CTL` hatch, and in-app
//! Help example keeps working while exactly ONE Mach-O exists.

// GUI subsystem on Windows: rust binaries default to the CONSOLE subsystem,
// which pops a stray blank console window alongside the terminal on every
// Explorer / Start-menu launch. The window library's `attach_parent_console`
// (first thing in its entry) reattaches stdio when launched FROM a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    // Start the broad window cold-start clock before argv0 parsing or route
    // selection. The compatibility GUI-entry clock is anchored separately if
    // this dispatches to a window. Dyld/process-loader time remains excluded.
    aterm_gui::mark_rust_main_start();
    // A GUI-subsystem exe (the attribute above) has no console on Windows;
    // reattach the parent's FIRST — before ANY route prints — so help/version/
    // verbs/diag output reaches a launching console and the TTY probe below
    // sees real console handles. No-op off Windows and for Explorer launches.
    #[cfg(windows)]
    aterm_gui::attach_parent_console();
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    let rest: Vec<OsString> = if argv.is_empty() {
        Vec::new()
    } else {
        argv.split_off(1)
    };

    // --- argv0 compat aliases (bundle symlinks + old installs) -------------
    // `file_name` (not a substring match) so only a REAL alias dispatches;
    // EXE_SUFFIX is trimmed for Windows dev builds of the thin bins.
    let argv0 = argv
        .first()
        .map(|a| {
            std::path::Path::new(a)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let argv0 = argv0
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .unwrap_or(&argv0);
    match argv0 {
        "aterm-ctl" => return aterm_ctl::main_entry(rest),
        "atpkg" => return atpkg::cli::main_entry(rest),
        "aterm-fleet" => aterm_agent::fleet_cli::main_entry(rest),
        "aterm-drive" => return aterm_agent::drive_cli::main_entry(rest),
        "aterm-gui" => {
            aterm_gui::main_entry(rest);
            return ExitCode::SUCCESS;
        }
        // `aterm`, the old `aterm-cli` symlink target, and anything else
        // (a renamed copy) are all the front door.
        _ => {}
    }

    // --- the front door -----------------------------------------------------
    let first = rest
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Verbs: in-process library calls (binary-era: exec co-located siblings).
    match first.as_str() {
        "ctl" => return aterm_ctl::main_entry(rest[1..].to_vec()),
        "pkg" => return atpkg::cli::main_entry(rest[1..].to_vec()),
        "fleet" => aterm_agent::fleet_cli::main_entry(rest[1..].to_vec()),
        "drive" => return aterm_agent::drive_cli::main_entry(rest[1..].to_vec()),
        _ => {}
    }

    // Mode-free surface BEFORE the mode fork: `help`, `-h`/`--help`,
    // `-V`/`--version`, and the diagnostic subcommands answer identically with
    // or without a TTY, and their identity is the ONE command's ("aterm X.Y",
    // never "aterm-gui X.Y") — a piped `aterm --version` must not fall into
    // the window path. parse_args prints and exits for all of these.
    let mode_free = matches!(
        first.as_str(),
        "help" | "agents" | "-h" | "--help" | "-V" | "--version"
    ) || aterm_cli::DIAG_COMMANDS
        .iter()
        .any(|(name, _)| *name == first);
    if mode_free {
        let _ = aterm_cli::parse_args(rest);
        // parse_args returns only for a launch decision, which cannot happen
        // for a mode-free first token; defend anyway.
        return ExitCode::SUCCESS;
    }

    // Toolchain dispatch (`aterm <tool> …`, docs/ATERM-DISTRIBUTION-WEDGE.md §4):
    // when the managed store resolves the first operand as an installed tool,
    // run it through `pkg run` (which execs the tool — process replacement,
    // exactly like the binary era). Resolution is the IN-PROCESS `atpkg::which`
    // (a readlink on the store shim — the library call never touches stdout,
    // which is why it needs no subprocess); a non-tool falls through to the
    // normal unknown-operand usage error.
    if aterm_cli::is_tool_candidate(Some(first.as_str())) && store_resolves(&first) {
        // KNOWN LIMIT: atpkg's CLI is String-typed, so non-UTF8 tool args
        // are lossy-converted (the binary era exec'd OsStrings verbatim);
        // fixing it means OsString plumbing through atpkg::cli — tracked, not
        // silent.
        // `run <tool> -- <args…>`: the `--` guarantees the tool's own flags
        // are never parsed by `pkg run` (its parser strips exactly one
        // leading `--`) — the binary-era contract; a user's literal `--`
        // survives verbatim.
        let mut run_args: Vec<OsString> = vec![OsString::from("run"), rest[0].clone()];
        run_args.push(OsString::from("--"));
        run_args.extend(rest[1..].iter().cloned());
        return atpkg::cli::main_entry(run_args);
    }

    // Mode fork. Explicit flags first — `--session` and `--window` force a
    // mode from anywhere (both stripped here; the mode libraries don't know
    // them). Without one: window when headless is requested, when the release
    // gates probe --diagnose, or when there is no TTY (a Finder/.app launch —
    // LaunchServices attaches none; the passthrough itself is PTY-backed and
    // does not need the parent's stdio to be one, hence the explicit
    // `--session` for scripts/CI). Otherwise: the session.
    // The scan/strip stops at the first `-e`/`--command`/`--`: past that
    // boundary every token is a child command's payload (the window's `-e`
    // contract) and passes through VERBATIM. The `aterm-cli` argv0 alias
    // keeps its binary-era contract: the session regardless of TTY (old
    // installs pipe it in scripts).
    let boundary = rest
        .iter()
        .position(|a| matches!(a.to_string_lossy().as_ref(), "-e" | "--command" | "--"))
        .unwrap_or(rest.len());
    let scan = &rest[..boundary];
    let force_session =
        argv0 == "aterm-cli" || scan.iter().any(|a| a.to_string_lossy() == "--session");
    let windowish = !force_session
        && (scan.iter().any(|a| {
            matches!(
                a.to_string_lossy().as_ref(),
                "--window" | "--headless" | "--diagnose"
            )
        }) || std::env::var_os("ATERM_HEADLESS").is_some()
            || !stdin_is_terminal());
    // NOTE on the `ATERM_HEADLESS` arm above: PRESENCE, deliberately — not the
    // enabling-value test `aterm_gui::cli` applies to the same variable. The
    // window library owns the headless decision and ANNOUNCES it, including the
    // refusal when the value is `0`/`off`/empty. Routing a merely-present
    // variable here to the window mode is what lets that announcement be
    // printed at all; testing the value here would send `ATERM_HEADLESS=0` into
    // the SESSION, where nothing would ever mention it — the silent outcome
    // this whole path exists to prevent.
    let mode_args: Vec<OsString> = rest
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            let a = a.to_string_lossy();
            *i >= boundary || (a != "--window" && a != "--session")
        })
        .map(|(_, a)| a.clone())
        .collect();
    if windowish {
        aterm_gui::main_entry(mode_args);
        return ExitCode::SUCCESS;
    }

    // The session: flags → quiet, then the passthrough (never returns).
    let quiet = aterm_cli::parse_args(mode_args);
    aterm_cli::session_main(quiet);
}

/// Whether the managed store resolves `tool` — the IN-PROCESS `atpkg::which`
/// against the same `store::resolve(None)` layout `pkg which` uses, mirroring
/// the binary era's co-located `atpkg which` probe. Best-effort: an unset HOME
/// (no layout) or an unresolvable shim both mean "not a tool".
//
// This used to self-spawn `<current_exe> pkg which <tool>` with all three stdio
// streams to /dev/null, on the theory that resolution had to be sandboxed so
// its stdout stayed out of ours. It never did: `cmd_which` is `layout()` +
// `atpkg::which()` + a `println!` of the result, and only the print — which we
// simply don't do — touches stdout. The spawn cost the full startup of the 10 MB
// aterm binary on the FRONT DOOR of every `aterm <operand>` invocation, to answer
// what is one `readlink(2)` on `<prefix>/bin/<tool>`: measured warm against the
// installed bundle, `aterm pkg which <not-a-tool>` runs 5-19 ms depending on
// machine load, against a ~1.4 ms `/bin/echo` spawn baseline (and ~0.5 s cold,
// with the page cache empty).
fn store_resolves(tool: &str) -> bool {
    atpkg::store::resolve(None).is_some_and(|layout| atpkg::which(&layout, tool).is_some())
}

/// TTY probe for the mode fork. std's `IsTerminal` on stdin: a Finder/.app
/// launch, a pipe, and CI all report false → window; an interactive shell
/// reports true → session.
fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}
