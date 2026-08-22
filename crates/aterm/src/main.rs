// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm` — THE one binary.
//!
//! One name, everywhere: the transparent terminal session, the GPU window,
//! and every verb live behind this single executable. Routing is the whole
//! job of this crate — every capability is a library:
//!
//! ```text
//! aterm <verb> …            every verb in `aterm_cli::Verb`, routed above the mode
//!                           fork so it answers the same at a TTY and through a pipe
//! aterm help [topic]        the toolchain manual (aterm-cli's parser owns it)
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

    // VERBS: the one command's own powers, routed HERE — ABOVE the mode fork, so a
    // verb answers identically at a terminal and through a pipe.
    //
    // The match is EXHAUSTIVE over `aterm_cli::Verb` deliberately: it is the
    // compile-time half of the roster's guarantee. A new variant breaks THIS build
    // until it is routed — which is precisely what failed to happen for `ship`, wired
    // into the window library's parser instead of here. Below the fork, a TTY on stdin
    // selects the SESSION, whose parser knew no `ship` and rejected it as an unknown
    // option; the verb worked only when stdin happened to be a pipe.
    if let Some(verb) = aterm_cli::Verb::from_operand(&first) {
        let forwarded = rest[1..].to_vec();
        return match verb {
            aterm_cli::Verb::Ctl => aterm_ctl::main_entry(forwarded),
            aterm_cli::Verb::Pkg => atpkg::cli::main_entry(forwarded),
            aterm_cli::Verb::Fleet => aterm_agent::fleet_cli::main_entry(forwarded),
            aterm_cli::Verb::Drive => aterm_agent::drive_cli::main_entry(forwarded),
            // The release tool is a separate executable — deliberately NOT carried by
            // the app bundle — so this verb execs where its siblings call a library.
            aterm_cli::Verb::Ship => {
                let args: Vec<String> = forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                std::process::exit(aterm_gui::run_ship(&args));
            }
            // The HEADLESS update lane (round-11 audit): `aterm ctl update status`
            // needs a WINDOW process serving the control socket — a terminal-only
            // machine has none, so nothing on it could even say it was stale.
            // These read the shared ledger and run the one-shot checker
            // in-process: no socket, no GUI.
            aterm_cli::Verb::Update => update_verb(&forwarded),
            // `agents` is parsed by aterm-cli itself (it prints and exits), so routing
            // it means handing the WHOLE operand list back to that parser.
            aterm_cli::Verb::Agents => {
                let _ = aterm_cli::parse_args(rest);
                ExitCode::SUCCESS
            }
        };
    }

    // `aterm --completions <bash|zsh|fish>` — the hidden generator flag, kept
    // OUT of `--help` like its `aterm-ctl` sibling. It completes `aterm`
    // ITSELF: the installer strips the sibling binaries off PATH, so a
    // completion for `aterm-ctl` completes a command nobody has. Handled
    // BEFORE the mode fork — install.sh pipes this into a file, and a piped
    // invocation must never fall into the window path. First-token only,
    // exactly like the sibling's pre-verb flag handling.
    if first == "--completions" || first.starts_with("--completions=") {
        let shell: Option<String> = first
            .strip_prefix("--completions=")
            .map(str::to_string)
            .or_else(|| rest.get(1).map(|a| a.to_string_lossy().into_owned()));
        return aterm_ctl::front_door_completions_entry(
            shell.as_deref(),
            &front_door_verbs(),
            COMPLETION_FLAGS,
        );
    }

    // Mode-free surface BEFORE the mode fork: `help`, `-h`/`--help`,
    // `-V`/`--version`, and the diagnostic subcommands answer identically with
    // or without a TTY, and their identity is the ONE command's ("aterm X.Y",
    // never "aterm-gui X.Y") — a piped `aterm --version` must not fall into
    // the window path. parse_args prints and exits for all of these.
    let mode_free = matches!(
        first.as_str(),
        "help" | "-h" | "--help" | "-V" | "--version"
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

    // THE SESSION UPDATE LANE (round-11). The one-binary era made a terminal
    // session an ordinary launch of aterm, but only the WINDOW entry ran the
    // updater — so a terminal-only Mac never checked, never staged, and never
    // applied anything, while install.sh promised "updates: automatic". Sessions
    // now run the same background check/stage loop; the crate dedupes checkers
    // ACROSS PROCESSES (a shared flock + ledger-freshness gate), so ten tabs
    // cost the shared GitHub budget one check per interval, not ten. APPLY stays
    // with the window entry on purpose: applying re-execs the process, and the
    // trial/rollback health confirmation is anchored in the window's steady
    // state — a session must never gamble a live PTY on it. The one-line nudge
    // below (before the PTY exists — it never interleaves with a running shell)
    // is the honest bridge: it names the staged build and how to apply it.
    // Source: env overrides ($ATERM_UPDATE_OWNER/_REPO) + the compiled default —
    // the same resolution the ctl `update check` verb uses; the GUI-config
    // repoint keys are a window-side concern.
    #[cfg(target_os = "macos")]
    {
        let build = aterm_gui::running_build_number();
        if let Some(st) = aterm_update::status(build)
            && let (Some(staged_build), Some(v)) = (st.staged_build, st.staged_version.as_ref())
            && staged_build > build
        {
            eprintln!(
                "aterm: update {v} (build {staged_build}) is staged — opening the aterm \
                 window (aterm --window) applies it"
            );
        }
        aterm_update::spawn_background_check(
            build,
            aterm_update::Source::resolve(None, None),
            None,
            None,
        );
    }

    aterm_cli::session_main(quiet);
}

/// `aterm update [status|check]` — the headless update lane, served in-process
/// from the shared ledger (`status`) and the one-shot checker (`check`): no
/// control socket, no window. `aterm ctl update status` remains the machine
/// surface a controller drives against a RUNNING window; this verb is what a
/// terminal-only machine (round-11: previously unable to even report its own
/// staleness) and scripts get. Apply is deliberately absent: applying re-execs
/// a process and rides the window entry's trial/rollback confirmation — the
/// nudges name `aterm --window` as the apply path instead.
fn update_verb(rest: &[OsString]) -> ExitCode {
    // `aterm-update` logs through `aterm_log`, a no-op until a host installs a
    // logger — the window installs a file logger, but THIS lane runs with a
    // terminal attached, where stderr is the honest surface. Without it a failed
    // manual check printed nothing at all and looked identical to success.
    static STDERR_LOGGER: StderrLogger = StderrLogger;
    let _ = aterm_log::set_logger(&STDERR_LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);
    let sub = rest
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_else(|| "status".to_string());
    let build = aterm_gui::running_build_number();
    match sub.as_str() {
        "status" => {
            let Some(st) = aterm_update::status(build) else {
                println!("auto-update is macOS-only; nothing to report on this platform");
                return ExitCode::SUCCESS;
            };
            print_update_status(build, &st);
            ExitCode::SUCCESS
        }
        "check" => {
            let st = aterm_update::check_now(build, &aterm_update::Source::resolve(None, None));
            print_update_status(build, &st);
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("aterm: unknown update sub-command {other:?} (usage: aterm update [status|check])");
            ExitCode::from(2)
        }
    }
}

/// The human rendering shared by `aterm update status` and `… check`: the
/// ledger's own summary sentence, when the record was written, and the failure
/// counters only when they carry news. The stranded/failing `outcome` already
/// contains the full explanation with the copy-pasteable remedy, so it is
/// printed verbatim rather than paraphrased.
fn print_update_status(build: u64, st: &aterm_update::UpdateStatus) {
    println!("aterm update: {}", st.summary());
    if !st.installable {
        // A dev build / DMG-mounted / translocated copy: the checker deliberately
        // no-ops (nothing to swap), which otherwise looks exactly like idleness.
        println!(
            "  note: this copy is not an installed aterm.app, so checks and staging are inert here"
        );
    }
    if !st.updated_at.is_empty() {
        println!("  last completed check: {}", st.updated_at);
    }
    if st.channel_unreadable {
        println!("  this machine CANNOT read its release channel — see above for the remedy");
    }
    if st.failing_checks > 0 {
        let kind = if st.failing_checks_kind.is_empty() {
            st.failing_kind.as_str()
        } else {
            st.failing_checks_kind.as_str()
        };
        println!("  failing checks: {} consecutive ({kind})", st.failing_checks);
    }
    if st.failing_applies > 0 {
        println!(
            "  failing applies: {} — a verified build is staged but will not start",
            st.failing_applies
        );
    }
    if st.staged_build.is_some_and(|b| b > build) {
        println!("  apply it by opening the aterm window: aterm --window");
    }
}

/// Whether the managed store resolves `tool` — the IN-PROCESS `atpkg::which`
/// against the same `store::resolve_configured()` layout `pkg which` uses, mirroring
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
    atpkg::store::resolve_configured().is_some_and(|layout| atpkg::which(&layout, tool).is_some())
}

/// TTY probe for the mode fork. std's `IsTerminal` on stdin: a Finder/.app
/// launch, a pipe, and CI all report false → window; an interactive shell
/// reports true → session.
fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

/// The minimal stderr logger the headless update lane installs (see
/// [`update_verb`]): level + message, no file/line noise — these lines are for
/// a person at a terminal, not a log file.
struct StderrLogger;

impl aterm_log::Log for StderrLogger {
    fn enabled(&self, _metadata: &aterm_log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &aterm_log::Record<'_>) {
        eprintln!("{}: {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

/// The first-position verbs `--completions` offers — built FROM the tables the
/// routing above actually consults (`help` is `aterm_cli::parse_args`'s leading
/// operand, [`aterm_cli::Verb::ALL`] is the roster the exhaustive verb match
/// dispatches, and [`aterm_cli::DIAG_COMMANDS`] is the mode-free diagnostic
/// set), never a hand-maintained copy — so the completion cannot advertise a
/// verb this build does not route, or miss one it does. Its previous source,
/// the retired `VERB_BINS` list, proved the point by failing it: the `update`
/// verb landed in the dispatch match while that list kept only the original
/// four, so the completions built "from the routing" were missing a verb the
/// routing had.
fn front_door_verbs() -> Vec<&'static str> {
    let mut verbs = vec!["help"];
    for verb in aterm_cli::Verb::ALL {
        verbs.push(verb.name());
    }
    for (name, _) in aterm_cli::DIAG_COMMANDS {
        verbs.push(name);
    }
    verbs
}

/// The user-typed front-door flags `--completions` offers, with the zsh/fish
/// descriptions. The set is the flags the routing above and `aterm-cli`'s
/// parser recognize BEFORE the mode fork; `--completions` itself stays out,
/// like the sibling `aterm-ctl` flag it mirrors (hidden generator flags do not
/// complete themselves). Per-flag operand completion (the `--containment`
/// mode) is deliberately out of scope — completing the name is the bulk of
/// the value, the ctl scripts' own stated line.
const COMPLETION_FLAGS: &[(&str, &str)] = &[
    ("--window", "open the GPU window explicitly"),
    ("--session", "force the transparent shell session"),
    ("--headless", "engine + control socket, no window"),
    ("--containment", "containment mode (master|user|safety|containment)"),
    ("--sandbox", "shorthand for --containment containment"),
    ("--no-sandbox", "shorthand for --containment user"),
    ("--quiet", "suppress the interactive startup notice"),
    ("--help", "print help and exit"),
    ("--version", "print the version and exit"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The verb list the completions offer is EXACTLY the routed surface:
    /// `help`, every [`aterm_cli::Verb`] in the roster, and every
    /// [`aterm_cli::DIAG_COMMANDS`] name — nothing more (an unrouted word in a
    /// completion is an advertised 404), nothing less (a routed verb missing
    /// from completion is the drift this list exists to prevent — and it
    /// HAPPENED: `update` joined the dispatch while the retired VERB_BINS
    /// table kept the original four, so completions "built from the routing"
    /// were missing a verb the routing had). The roster cannot drift from the
    /// dispatch — the front door matches it exhaustively — so completion,
    /// dispatch, --help and the shadowing shield are one fact.
    #[test]
    fn front_door_verb_list_is_the_routing_tables() {
        let verbs = front_door_verbs();
        assert!(verbs.contains(&"help"));
        for verb in aterm_cli::Verb::ALL {
            assert!(
                verbs.contains(&verb.name()),
                "routed verb `{}` must complete",
                verb.name()
            );
        }
        for (name, _) in aterm_cli::DIAG_COMMANDS {
            assert!(verbs.contains(name), "diag `{name}` must complete");
        }
        assert_eq!(
            verbs.len(),
            1 + aterm_cli::Verb::ALL.len() + aterm_cli::DIAG_COMMANDS.len(),
            "no unrouted words: the list is the roster and the diag set, nothing else"
        );
    }

    /// The generated scripts complete `aterm` (the ONE command on PATH after
    /// install), cover the whole routed verb table and the front-door flags,
    /// and delegate `aterm ctl <TAB>` to the ctl verb set. CONTRACT with
    /// install.sh: the zsh script's FIRST line is `#compdef aterm`.
    #[test]
    fn front_door_completions_cover_the_routed_surface() {
        let verbs = front_door_verbs();
        for (shell, wiring) in [
            ("bash", "complete -F _aterm aterm\n"),
            ("zsh", "#compdef aterm\n"),
            ("fish", "complete -c aterm -f\n"),
        ] {
            let script = aterm_ctl::front_door_completion_script(shell, &verbs, COMPLETION_FLAGS)
                .expect("known shell yields a script");
            assert!(script.contains(wiring), "{shell} wires `aterm`");
            for verb in &verbs {
                assert!(script.contains(verb), "{shell} completes `{verb}`");
            }
            for (flag, _) in COMPLETION_FLAGS {
                // fish names long flags dash-less (`-l window`).
                let probe = if shell == "fish" {
                    flag.trim_start_matches('-')
                } else {
                    flag
                };
                assert!(script.contains(probe), "{shell} completes `{flag}`");
            }
            // Representative ctl verbs prove the delegation arm is present.
            for ctl_verb in ["text", "turn", "subscribe"] {
                assert!(
                    script.contains(ctl_verb),
                    "{shell} completes `aterm ctl {ctl_verb}`"
                );
            }
        }
        let zsh = aterm_ctl::front_door_completion_script("zsh", &verbs, COMPLETION_FLAGS)
            .expect("zsh yields a script");
        assert_eq!(
            zsh.lines().next(),
            Some("#compdef aterm"),
            "install.sh keys on the first line"
        );
        // Unknown shells yield no script (the entry maps that to a clear error).
        assert!(
            aterm_ctl::front_door_completion_script("powershell", &verbs, COMPLETION_FLAGS)
                .is_none()
        );
    }
}
