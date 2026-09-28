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
//! aterm            (a TTY)  the transparent session — your shell, passed through
//! aterm         (no TTY)    the window (a Finder/.app launch has no TTY)
//! aterm --window            the window, explicitly, from anywhere
//! ```
//!
//! ARGV0 COMPAT: the bundle ships symlinks (`aterm-ctl`, `atpkg`,
//! `aterm-fleet`, `aterm-drive`, `aterm-gui`, `aterm-cli`) onto this binary,
//! and old installs symlinked `~/.local/bin/aterm` at a bundled `aterm-cli`.
//! Invoked through any of those names, main dispatches as that tool — so
//! every pre-one-binary script, PATH entry, sibling `aterm-ctl` lookup, and in-app
//! Help example keeps working while exactly ONE Mach-O exists.

// NO `windows_subsystem = "windows"` HERE — this file is the CONSOLE image.
//
// The PE subsystem is a per-FILE header field, and Windows shells key on it:
// a console-subsystem child is waited for, a GUI-subsystem one is not. This
// binary carried the attribute until 2026-09-22 so an Explorer / Start-menu
// launch would not flash a console, and every installed CLI name was a
// hardlink of it — so from pwsh and cmd NO `aterm` verb was waited for. Measured
// on the installed 0.90.0: `Measure-Command {aterm help}` = 18 ms with the
// output painted over the returned prompt, `aterm --version 1>$null` panicked
// with "failed printing to stdout: The pipe is being closed (os error 232)"
// (the shell had already closed its end), and from a NON-interactive pwsh host
// `aterm ctl <verb>` returned nothing at all (0/10). A scheduled task or a
// script calling `aterm` got empty output.
//
// Windows therefore needs TWO file objects of the same code (one crate, one
// `main`): `aterm.exe`, built from this root, CONSOLE subsystem — every CLI
// alias hardlinks to it — and `aterm-gui.exe`, built from `src/windowed.rs`
// (the `aterm-windowed` bin target, which `#[path]`-includes this file as a
// module and adds only the attribute), WINDOWED subsystem, for the Start Menu,
// the Explorer verb, the jump list and the pinned tile. The console image
// never runs a window in-process on Windows: it hands the window to the
// windowed sibling (`run_window` / `windowed_sibling`) and returns, so the
// prompt comes back. The one-binary doctrine on macOS/Linux is untouched: the
// extra bin target builds there as a std-only stub that includes none of this
// file (see `windowed.rs`).

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::ExitCode;

/// `aterm link` and `aterm fabric` where the fabric bridge cannot exist.
///
/// The bridge is Unix-domain sockets and descriptors inherited at fixed numbers
/// (`aterm_uds::spawnfd`) end to end (`crates/aterm-link`, `vendor/astream`;
/// on macOS a launchd-kept broker), so `crates/aterm` carries it under
/// `[target.'cfg(unix)'.dependencies]` and this answers in its place — for
/// both verbs that live in that crate — on every other target. The verbs STAY
/// on the roster — the roster is one list on every platform and `aterm help`
/// must not lie about what the binary knows — and refuse by name, with the
/// reason, instead of failing to exist: an operator who runs the enable script
/// from a Mac and then types the verb here learns why in one line, not from
/// an "unknown verb".
#[cfg(not(unix))]
fn link_unavailable(verb: &str) -> ExitCode {
    eprintln!("aterm {verb}: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

// `pub(crate)`, not private: `src/windowed.rs` includes this file as a module
// and calls this `main` from its own — a parent module cannot reach a child's
// private items, and rustc accepts any visibility on a bin root's `main`.
pub(crate) fn main() -> ExitCode {
    // Start the broad window cold-start clock before argv0 parsing or route
    // selection. The compatibility GUI-entry clock is anchored separately if
    // this dispatches to a window. Dyld/process-loader time remains excluded.
    aterm_gui::mark_rust_main_start();
    // The WINDOWED image (`src/windowed.rs` shares this body) starts with no
    // console on Windows; reattach the parent's FIRST — before ANY route prints
    // — so help/version/verbs/diag output reaches a launching console and the
    // TTY probe below sees real console handles. The console image finds its
    // inherited handles present and this is a no-op there, as it is off
    // Windows and for Explorer launches. The answer (did it attach?) matters
    // only to the window library's own entry, which asks again itself.
    #[cfg(windows)]
    let _ = aterm_gui::attach_parent_console();
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
    let first = rest
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    match alias_route(argv0, &first) {
        AliasRoute::Ctl => return aterm_ctl::main_entry(rest),
        AliasRoute::Pkg => return atpkg::cli::main_entry(rest),
        AliasRoute::Fleet => return aterm_agent::fleet_cli::main_entry(rest),
        AliasRoute::Drive => return aterm_agent::drive_cli::main_entry(rest),
        AliasRoute::Link => {
            #[cfg(unix)]
            return aterm_link::cli::dispatch(
                &rest
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            );
            #[cfg(not(unix))]
            return link_unavailable("link");
        }
        // `get(1..)` not `rest[1..]`: this arm is only reached with a first
        // token in hand, but that is an argument the verifier cannot follow
        // from here, and it refuted the index (measured 2026-09-09).
        // `ThisProcess`: the alias names ARE the windowed image on Windows
        // (`aterm-gui.exe` / a dev tree's `aterm-windowed.exe`), so a spawned
        // window runs here — handing it to "the windowed sibling" would be a
        // detached copy of ourselves, one hop for nothing.
        AliasRoute::AliasWindowVerb => {
            return window_verb(
                &first,
                rest.get(1..).unwrap_or(&[]),
                WindowHost::ThisProcess,
            );
        }
        AliasRoute::AliasWindow => return gui_alias_entry(rest),
        // `aterm`, the old `aterm-cli` symlink target, and anything else
        // (a renamed copy) are all the front door.
        AliasRoute::FrontDoor => {}
    }

    // --- the front door -----------------------------------------------------

    // THE RETIRED HOOK SHAPE `<aterm> hook run <event> …` — what `aterm link hook
    // install` wrote on 2026-09-14 before 7bcb0503a spelled it `link hook run`. It
    // is not a verb and joins no roster: it reaches the fabric bridge's retired
    // `hook` verb (aterm-link's `retired_hook`, b58cde423: exit 0, nothing on
    // stdout, its one stderr line, stdin drained), because below the fork it is
    // the window parser's `unknown option`, exit 2 — a BLOCK to Claude Code — and
    // a project `.claude/settings*.json` the primer sweep never reads may still
    // hold it. Only `hook run`: a bare `aterm hook` stays the parser's.
    #[cfg(unix)]
    if first == "hook" && rest.get(1).is_some_and(|a| a == "run") {
        return aterm_link::cli::dispatch(
            &rest
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<String>>(),
        );
    }

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
        // `get(1..)` for the reason recorded on the `AliasWindowVerb` arm.
        let forwarded = rest.get(1..).unwrap_or(&[]).to_vec();
        return match verb {
            aterm_cli::Verb::Ctl => aterm_ctl::main_entry(forwarded),
            // Session connections (SESSION_CONNECTIONS.md §6.1): the human
            // front door for the standing pull/push wiring — presentation over
            // the same control-socket verbs `ctl` speaks, hence it lives in
            // aterm-ctl and routes here beside its sibling.
            aterm_cli::Verb::Conn => aterm_ctl::conn_main_entry(forwarded),
            aterm_cli::Verb::Pkg => atpkg::cli::main_entry(forwarded),
            aterm_cli::Verb::Fleet => aterm_agent::fleet_cli::main_entry(forwarded),
            aterm_cli::Verb::Drive => aterm_agent::drive_cli::main_entry(forwarded),
            // The fabric bridge. `dispatch` takes `String`s because every one of
            // its operands is a subject, a path or a principal — all of which
            // the control protocol already defines as UTF-8 — and lossy is the
            // right conversion for an argument that is about to be rejected by
            // name if it is not one of those.
            #[cfg(unix)]
            aterm_cli::Verb::Link => aterm_link::cli::dispatch(
                &forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            ),
            // The owner's view of the fabric: it lives beside the bridge it
            // reports on (`aterm-link`'s `fabric` module), and takes `String`s
            // for the reason `link` does — and is gated the same way, for the
            // same reason: it lives in the unix-only crate.
            #[cfg(unix)]
            aterm_cli::Verb::Fabric => aterm_link::fabric::main(
                &forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            ),
            #[cfg(not(unix))]
            aterm_cli::Verb::Link => link_unavailable("link"),
            #[cfg(not(unix))]
            aterm_cli::Verb::Fabric => link_unavailable("fabric"),
            // The release tool is a separate executable — deliberately NOT carried by
            // the app bundle — so this verb execs where its siblings call a library.
            aterm_cli::Verb::Ship => {
                let args: Vec<String> = forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                // RETURN the tool's status rather than `std::process::exit`-ing
                // it. A `-> !` call leaves rustc no successor block, so the
                // enclosing body's scope teardown lands on an `unreachable` — and
                // the verifier, which has no body for an out-of-bundle callee,
                // cannot see that the call diverges and REFUTED that unreachable
                // inside `main` (measured 2026-09-09, `targo trust build
                // --allow-l0-gaps -p aterm`). A return has no such path to
                // discharge, so the obligation disappears instead of needing a
                // proof, and this arm becomes what every other arm already is: a
                // route that hands `main` an `ExitCode`. `run_ship` narrows to
                // 0..=255 itself (`ExitStatus::code()` on a normal exit, 1 for a
                // signal or a failed spawn), so the status a publishing machine
                // sees is unchanged; a value outside that range — which
                // `status.code()` does not produce where `ship` runs — becomes the
                // refusal code 2 rather than being truncated into a lie.
                ExitCode::from(u8::try_from(aterm_gui::run_ship(&args)).unwrap_or(2))
            }
            // The HEADLESS update lane (round-11 audit): `aterm ctl update status`
            // needs a WINDOW process serving the control socket — a terminal-only
            // machine has none, so nothing on it could even say it was stale.
            // These read the shared ledger and run the one-shot checker
            // in-process: no socket, no GUI.
            aterm_cli::Verb::Update => update_verb(&forwarded),
            // The Claude Code harness's read views (docs/DESIGN-aterm-wrapper-
            // 2026-09-17.md §0.4). Routed here beside its siblings for the reason
            // the comment above this match gives: `aterm harness usage` is typed
            // at a prompt, where stdin is a TTY, and the retired `aterm harness hook
            // …` — still run through `/bin/sh` by a bridge an older build
            // installed, with the payload on a pipe — must reach the same code
            // and its silent exit 0 (decision "B").
            aterm_cli::Verb::Harness => aterm_agent::harness::cli::main_entry(forwarded),
            // `agents` is parsed by aterm-cli itself (it prints and exits), so routing
            // it means handing the WHOLE operand list back to that parser.
            aterm_cli::Verb::Agents => {
                let _ = aterm_cli::parse_args(rest);
                ExitCode::SUCCESS
            }
            // The wt-shaped WINDOWING grammar (S12). Routed HERE, above the mode
            // fork, for the same reason as every other verb: `aterm new-tab` is
            // typed at a prompt, where a TTY on stdin would otherwise select the
            // SESSION and its parser would reject the word as an unknown option
            // — the exact `ship` failure this dispatch table exists to prevent.
            // `WindowedSibling`: typed at a prompt, this is the CONSOLE image on
            // Windows, and a window it has to spawn belongs in the windowed
            // sibling (see the file header) so the prompt comes back at once.
            aterm_cli::Verb::NewTab | aterm_cli::Verb::NewWindow | aterm_cli::Verb::SplitPane => {
                window_verb(verb.name(), &forwarded, WindowHost::WindowedSibling)
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
        // `--version` names the build the updater's start probe looks for
        // (2026-09-14); the number is aterm-gui's stamp, handed over here.
        aterm_cli::set_running_build(aterm_gui::running_build_number());
        let _ = aterm_cli::parse_args(rest);
        // parse_args returns only for a launch decision, which cannot happen
        // for a mode-free first token; defend anyway.
        return ExitCode::SUCCESS;
    }

    // Toolchain dispatch (`aterm <tool> …`, docs/ATERM-DISTRIBUTION-WEDGE.md §4):
    // when the managed store resolves the first operand as an installed tool,
    // run it through `pkg run` (which execs the tool — process replacement,
    // exactly like the binary era). Resolution is IN-PROCESS: `atpkg::which` (a
    // readlink on the store shim) and, for a word with no shim, the lost-shim
    // probe (`store_resolves`) — neither touches stdout, which is why they need
    // no subprocess; a non-tool falls through to the normal unknown-operand
    // usage error.
    if aterm_cli::is_tool_candidate(Some(first.as_str())) && store_resolves(&first) {
        // KNOWN LIMIT: atpkg's CLI is String-typed, so non-UTF8 tool args
        // are lossy-converted (the binary era exec'd OsStrings verbatim);
        // fixing it means OsString plumbing through atpkg::cli — tracked, not
        // silent.
        // `run <tool> -- <args…>`: the `--` guarantees the tool's own flags
        // are never parsed by `pkg run` (its parser strips exactly one
        // leading `--`) — the binary-era contract; a user's literal `--`
        // survives verbatim.
        // `split_first` rather than `rest[0]` / `rest[1..]`: `first` above is
        // `rest.first().…unwrap_or_default()`, so an empty `rest` reaches here as
        // the empty string and the two indexes are unreachable — but only by a
        // chain the verifier cannot follow, and it refuted both bounds checks
        // (measured 2026-09-09, `targo trust build --allow-l0-gaps -p aterm`).
        // Destructuring carries the non-emptiness in the type instead of in an
        // argument nobody can see from here.
        if let Some((tool, tool_args)) = rest.split_first() {
            let mut run_args: Vec<OsString> = vec![OsString::from("run"), tool.clone()];
            run_args.push(OsString::from("--"));
            run_args.extend(tool_args.iter().cloned());
            return atpkg::cli::main_entry(run_args);
        }
    }

    // PENDING-PROGRAM arm (R6): a default-set tool whose real shim has not landed
    // yet resolves to a pending STUB in the managed store — `aterm trust` moments
    // after a lean install must print the live install state (and bump trust to the
    // front of the queue), never fall through to "unknown option". Same in-process
    // dispatch as the arm above; `atpkg __pending` prints the message and exits 127 —
    // or, for a person at a terminal while a pass installs the tool, waits and runs it,
    // so the tool's own arguments ride along exactly as the stub's `"$@"` carries them.
    if aterm_cli::is_tool_candidate(Some(first.as_str())) && pending_stub_resolves(&first) {
        // `split_first()` for the reason recorded on the arm above: the index is
        // unreachable with an empty `rest`, but not by a chain the verifier can follow.
        let Some((tool, tool_args)) = rest.split_first() else {
            return ExitCode::from(2);
        };
        let mut pending_args: Vec<OsString> = vec![OsString::from("__pending"), tool.clone()];
        pending_args.extend(tool_args.iter().cloned());
        return atpkg::cli::main_entry(pending_args);
    }

    // A name the compiled rosters know (`atpkg::stub::describe`) that nothing in the
    // store answers — removed, never laid, or no layout — gets `pkg run`'s own line
    // ("atpkg: ty is not installed (fix: aterm pkg install ty)", exit 127), never the
    // session or window parser's "unknown command ty".
    if aterm_cli::is_tool_candidate(Some(first.as_str())) && atpkg::stub::describe(&first).is_some()
    {
        let Some((tool, tool_args)) = rest.split_first() else {
            return ExitCode::from(2);
        };
        let mut run_args: Vec<OsString> = vec![OsString::from("run"), tool.clone()];
        run_args.push(OsString::from("--"));
        run_args.extend(tool_args.iter().cloned());
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
    // `get(..b).unwrap_or(rest)` rather than `rest[..b]`: `payload_boundary`
    // returns `position(..).unwrap_or(rest.len())`, always in range, but that is
    // a property of `position` the verifier does not have — and it refuted both
    // the bare index AND a `.min(rest.len())` clamp (measured 2026-09-09;
    // comparison is opaque to it here). `get` has no panic path to discharge,
    // so the obligation disappears instead of needing a proof. The fallback is
    // unreachable and returns the whole slice, which is what the clamp meant.
    let scan = rest.get(..payload_boundary(&rest)).unwrap_or(&rest);
    let force_session =
        argv0 == "aterm-cli" || scan.iter().any(|a| a.to_string_lossy() == "--session");
    let windowish = !force_session
        && (scan.iter().any(|a| {
            matches!(
                a.to_string_lossy().as_ref(),
                "--window" | "--headless" | "--diagnose"
            )
        }) || !stdin_is_terminal());
    let mode_args = take_no_reroute(&strip_mode_flags(&rest));
    if windowish {
        // SINGLE-INSTANCE ROUTING (S12), applied to a PLAIN window launch —
        // Explorer / the Start menu / a pinned tile / `aterm --window` with
        // nothing else asked of it. Under the shipped default
        // (`windowing_behavior = "new_window"`) this is a no-op and the launch
        // proceeds exactly as it did before the key existed. The same call sits
        // in `gui_alias_entry`, because on Windows the shortcut this machine
        // actually launches names an ALIAS copy of this binary.
        //
        // `rest`, NOT `scan`: the mode fork's scan stops at the `-e`/`--` payload
        // boundary, so handing it to the gate would present `aterm -e vim` as an
        // empty (maximally eligible) argument list. See `plain_launch_request`.
        let spawn = match plain_launch_policy(&rest) {
            ControlFlow::Break(code) => return code,
            ControlFlow::Continue(spawn) => spawn,
        };
        // What the windowed sibling is told, when it is the one to open this
        // window (see `WindowHost`): the policy's own decision when it made one
        // — `new-window`, so the sibling cannot route it a second time — and
        // otherwise the launch's own argument list, `--no-reroute` included,
        // which the sibling's gate refuses exactly as this one just did.
        let handoff = match &spawn {
            Some(request) => spawn_handoff_argv(request),
            None => rest.clone(),
        };
        // `scan`, not `mode_args`: the host decision reads the flags the mode
        // fork read, before the strip, and never a `-e` payload's tokens.
        return run_window(mode_args, window_host_for(scan), &handoff);
    }

    // THE NESTED-SESSION GUARD (Windows, 2026-09-22 audit, defect b). Every tab
    // of the window is a shell with `ATERM_CHILD=1` in its environment, and a
    // bare `aterm` typed there used to start the transparent SESSION lane
    // nested inside the tab: the outer tab's keystrokes then arrived in the
    // inner passthrough as literal win32-input-mode records
    // (`[69;18;101;1;0;1_cho hi`, a PSReadLine ParserError), no command ever
    // ran, and a stray `exit` closed the OUTER window. On Windows a tab is not
    // a place to nest a console session, so a BARE launch becomes `aterm
    // new-tab` for the cwd, handed to the ENCLOSING instance (the tab's own
    // `ATERM_PARENT_SESSION_ID` is how `aterm_ctl::front_door_instance` finds
    // it), and says so in one stderr line. Bare is `rest` empty — not one
    // token: any token at all — `--sandbox`, `--containment <mode>`,
    // `--no-reroute`, a `-e` payload, a typo — is a request the session parser
    // must honour or refuse, never one to drop for a plain tab (review
    // 2026-09-27: `aterm --sandbox` opened an UNcontained tab and exited 0;
    // measured after the fix, `aterm --bogus` in a tab is the parser's usage
    // error, exit 2, and no tab). An explicit `--session` still nests — that is what
    // the flag is for — and so does the `aterm-cli` argv0 alias, whose
    // binary-era contract is "the session regardless". Unix is untouched: a
    // nested session there is the documented way to run one.
    #[cfg(windows)]
    if nests_inside_aterm(
        rest.is_empty(),
        force_session,
        std::env::var_os("ATERM_CHILD").is_some(),
        stdin_is_terminal(),
    ) {
        return nested_new_tab();
    }

    // The session: flags → quiet, then the passthrough (never returns).
    let quiet = aterm_cli::parse_args(mode_args);

    // THE SESSION SAYS NOTHING UNASKED ON STDERR (Phase 2, 2026-09-22): its log is the
    // window's `aterm.log`, and atpkg's unasked notices (a config it cannot read, a
    // prefix it will not use, a lay that is not provenance-clean) are records in it —
    // both set up here, before the lane's first atpkg call and its first thread.
    aterm_gui::install_session_log();
    atpkg::notice::to_host_log();
    // ONE read of aterm.toml's `[packages]` and ONE layout for the whole lane — the
    // agents handoff, the reroute seam, the package pass and the reroute thread's lay
    // policy all read these. Resolving per consumer re-read the file each time, and a
    // malformed table was reported once per read (2026-09-22 review: five copies).
    let packages = atpkg::config::cached();
    let layout = atpkg::store::resolve_from(packages);

    // THE MANAGED `agents/` HANDOFF (2026-09-18, closing R3 of 2026-09-16). The
    // session used to DERIVE `<prefix>/agents` as the sibling of `$ATERM_REROUTE_DIR`
    // — handed only when the reroute is engaged — so `--no-reroute`, the escape hatch
    // for the UPSTREAM RUST NAMES, also dropped the managed `claude`/`codex`
    // front-insert, and nothing named the directory
    // unless an enclosing shell that sourced the hook had exported `$ATPKG_AGENTS`.
    // Now the one binary that links atpkg resolves the layout, ENSURES the directory
    // (`Layout::ensure_agents_dir`, the rule the window's `spawn::managed_agents_dir`
    // shares: one `mkdir`, mode by prefix shape, never a wait, and a symlink or file at
    // `agents/` refused and handed by neither) and hands it to `session_main` as
    // `$ATERM_AGENTS_DIR` on EVERY lane, engaged reroute or not. No export means
    // NOT SET: an inherited stray from an enclosing session is cleared, so "absent
    // or empty" reads as "none" in the session (`aterm-cli::managed_agents_dir`).
    // Never `ATPKG_AGENTS`: the shell integration keys its hook-sourcing on that
    // being unset. Same trusted-launcher discipline as the reroute handoff below:
    // set here, single-threaded, before the reroute lay and the update checker
    // spawn the lane's first threads.
    hand_agents_dir(layout.as_ref());

    // THE REROUTE SEAM of the session lane (`docs/DESIGN-toolchain-reroute-2026-09-07.md`
    // §"Reaching PATH" 1): resolve the configured store, lay the session-scoped stubs
    // of the upstream Rust names (idempotent, eight tiny files, never over a foreign
    // file — so the very first session is covered before `atpkg seed` ever runs), and
    // hand the directory to `session_main` as `$ATERM_REROUTE_DIR`. The environment is
    // the handoff because aterm-cli links atpkg for its tests only (its Cargo.toml:
    // "the router composes these crates, aterm-cli does not call atpkg"); the session
    // reads it at its own edge, puts it FIRST on the child PATH (move-to-front) and
    // re-exports it for the shell integration. Same trusted-launcher discipline as
    // `take_no_reroute`: set HERE, before the update checker below spawns the first
    // thread. `--no-reroute` above (which `take_no_reroute` turns into the internal
    // `__ATERM_REROUTE_PASSTHROUGH` marker, and clears an inherited one without it) ⇒
    // nothing laid, nothing handed over, and the session prepends and exports nothing. On Windows `lay` lays nothing and the directory
    // does not exist, so the handoff is skipped there too (TARGET).
    if !atpkg::reroute::engaged(
        std::env::var(atpkg::reroute::PASSTHROUGH_ENV)
            .ok()
            .as_deref(),
    ) && let Some(layout) = layout.clone()
    {
        // A SESSION SPAWN NEVER WAITS ON A LAUNCHD JOB (2026-09-14, the perf
        // audit) — the window entry's twin, and the same measured cost: when
        // `lay` has files to lay it takes the launchd round trip plus a byte
        // copy of the helper, 452 ms median non-bundled and 730 ms from the
        // shipped .app. Here that sits in front of the shell of a new tab.
        //
        // What the session needs synchronously is the DIRECTORY, because that is
        // what goes on its PATH; laying the stubs into it is the same work
        // whenever it runs, so it runs beside the shell instead of before it. A
        // directory not made and a failed lay are LOG lines, never stderr: the same
        // condition with the same remedy, and nothing prints into a shell the user did
        // not ask to update (Phase 2, 2026-09-22); `aterm pkg doctor` names an unlaid
        // reroute. (A recorded decline lays nothing and says nothing: that is the
        // user's own instruction, and `lay` still honours it.)
        let dir = layout.reroute_dir();
        if let Err(error) = layout.ensure_dir(&dir) {
            aterm_log::warn!(
                "aterm: reroute dir not created ({error}); the upstream Rust names are NOT rerouted in this session — `aterm pkg doctor` explains (aterm help reroute)"
            );
        }
        if dir.is_dir() {
            aterm_log::env::set(atpkg::reroute::REROUTE_DIR_ENV, &dir);
        }
        let _ = std::thread::Builder::new()
            .name("aterm-reroute-lay".into())
            .spawn(move || {
                if let Err(error) = atpkg::reroute::lay(&layout) {
                    aterm_log::warn!(
                        "aterm: reroute stubs not laid ({error}); the upstream Rust names are NOT rerouted in this session — `aterm pkg doctor` explains (aterm help reroute)"
                    );
                }
            });
    }

    // THE SESSION UPDATE LANE (round-11). The one-binary era made a terminal
    // session an ordinary launch of aterm, but only the WINDOW entry ran the
    // updater — so a terminal-only Mac never checked, never staged, and never
    // applied anything, while install.sh promised "updates: automatic". Sessions
    // now run the same background check/stage loop; the crate dedupes checkers
    // ACROSS PROCESSES (a shared flock + ledger-freshness gate), so ten tabs
    // cost the shared GitHub budget one check per interval, not ten. On macOS,
    // APPLY stays with the window entry: applying re-execs the process, and the
    // trial/rollback health confirmation is anchored in the window's steady
    // state. Linux replaces only the on-disk binary; this session lane checks
    // and stages without replacing it or gambling a live PTY. A staged build is
    // said nowhere here (Phase 2, 2026-09-22: the one-line nudge printed at every
    // launch went): `aterm update status` answers when asked. Source: the compiled
    // channel, which only a development build lets
    // `[update]` owner/repo repoint (`aterm_gui::configured_update_settings`, 2026-09-14;
    // no env override since 2026-09-23) — the same resolution the window's loop and the
    // ctl `update check` verb use. "Check for updates automatically" off (`[update]
    // enabled = false`, Settings ▸ Software Update) makes the call a no-op, exactly as
    // it does for the window (`aterm_update::automatic`); `aterm update check` still runs.
    // Resolve on the checker thread each cycle so a config reload also changes
    // the channel of an already-running session.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if session_lane_is_interactive() {
        let build = aterm_gui::running_build_number();
        aterm_update::spawn_background_check_with_settings(
            build,
            std::sync::Arc::new(aterm_gui::configured_update_settings),
            None,
            None,
        );
    }

    // THE TOOLCHAIN'S OWN CHECK, from the session lane (R3/R4, 2026-09-10). Only
    // the WINDOW ran `atpkg update` — a terminal-only Mac never provisioned or
    // updated its packages. SILENT (Phase 2, 2026-09-22): nothing it does prints, and a
    // spawn that fails is a log line. What it does (Phase 3, 2026-09-23):
    //
    //  -  under the MACHINE-WIDE rule the window's loop runs its six-hour walk on
    //     (`pkg_check::full_pass_owed`: never succeeded, or the last success six hours
    //     old; no pass installing now or ended anywhere on the machine in the last
    //     five minutes, and none that failed in the last six hours, so a pass that keeps
    //     failing is retried once per interval, not by every tab, and no tab queues a
    //     waiter behind a pass in flight), a DETACHED one-shot `aterm pkg update` — its own process group,
    //     stdio on /dev/null, never waited for — claimed first (`pkg_check::claim` on
    //     `session-pass.stamp`), so tabs opened together start one pass, not one each.
    //     It runs on the window's argv (`pkg_check::pass_flags`: `--wait-lock`, and
    //     `--progress-file` under the prefix, so a window follows it), and names no
    //     spawner (`SPAWNER_DETACHED`): its detaching `sh` exits at once, and a waiter
    //     that read that as its window going would stand aside with 75 — a contended
    //     pass WAITS instead. Gated on the switch the window reads — `[packages]
    //     enabled`, Automatic updates (the retired `auto_update` folded in; there is no
    //     environment kill switch since 2026-09-23) — and on this being an
    //     INTERACTIVE launch: stdin a terminal — a harness
    //     driving the session over pipes (the integration tests, a driver's `--session`
    //     child) must never provision the machine's real prefix as a side effect
    //     (2026-09-10 review: `targo test -p aterm` rewrote the owner's status.toml).
    // THE HOST SETTINGS ARE NOT THE PACKAGE MANAGER'S TO GATE. A session launch — this
    // binary run from another terminal, or over ssh — could only apply them as a side
    // effect of a package pass that was due, so a Mac with `[packages] enabled = false`
    // (then also spelled `auto_update = false`, or an environment kill switch), or simply
    // a pass that ran an hour ago, never got them from this lane at all. They take no store lock, need no index and
    // no network, and `machine apply` prints nothing when nothing changed — but they walk
    // `$HOME`, so they run ONCE A DAY, on the slot a window's launch claims too
    // (`atpkg::machine::launch_apply_due`), not at every launch. Interactive launches only,
    // for the same reason the pass above is gated that way: a harness driving a session
    // over pipes must not touch the real machine.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    if cfg!(target_os = "macos")
        && session_lane_is_interactive()
        && let Some(layout) = layout.as_ref()
        && atpkg::machine::launch_apply_due(layout, now)
    {
        spawn_detached_machine_apply();
    }
    if let Some(layout) = layout.as_ref()
        && packages.enabled()
        && session_lane_is_interactive()
        && session_pass_due(layout, now)
    {
        spawn_detached_pkg_update(layout);
    }

    // THE VENDOR HEAD WATCH, from the session lane (gap #28, 2026-09-26). Only the window
    // ran it, so a Mac with no aterm window open found a new Claude Code or Codex release
    // at the index cadence of hours. Every interactive session now starts the watch's own
    // loop on a thread ([`start_session_head_watch`]); the store's seat lets ONE of them
    // watch, and only while no window does — a window that opens takes the watch over, and
    // when this process exits the thread goes with it and the kernel hands the seat on.
    // Gated like the pass above: `[packages] enabled` (read live from then on) and an
    // interactive launch — a harness driving a session over pipes must never reach the
    // vendors, or start a pass on the machine's real prefix.
    if let Some(layout) = layout.as_ref()
        && packages.enabled()
        && session_lane_is_interactive()
    {
        start_session_head_watch(layout, packages);
    }

    session_lane(quiet)
}

/// The session's head watch ([`atpkg::vendor_direct::watch::run_host`]): the window's
/// cadence, wake grace and re-offer leases, `[packages]` read live, and each moved head's
/// pass run as this binary's `pkg update <program> --head-watch` — the window's own
/// targeted pass — DETACHED like [`spawn_detached_pkg_update`]'s, so it outlives the
/// session and is never reaped in the shell's place. Its lines are `aterm.log` records;
/// a thread that could not start is one more. Nothing it does prints.
fn start_session_head_watch(
    layout: &atpkg::store::Layout,
    packages: &atpkg::config::PackagesConfig,
) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let gate = atpkg::vendor_direct::watch::Gate {
        on: packages.enabled(),
        exclude: packages.exclude().to_vec(),
    };
    let lead = vec![std::ffi::OsString::from("pkg")];
    match atpkg::vendor_direct::watch::run_host(layout, gate, exe, lead, |line| {
        aterm_log::info!("{line}");
    }) {
        // Runs for the rest of this process; its handle is never needed to stop it.
        Some(Ok(_running)) => {}
        Some(Err(error)) => {
            aterm_log::warn!("aterm: could not start the vendor head watch: {error}");
        }
        // The manager is off, or its registry a local directory: no vendor to watch.
        None => {}
    }
}

/// Who a refused `agents/` leaves without the managed agents, in the session's words.
const SESSION_REACH: &str = "this session";

/// THE `$ATERM_AGENTS_DIR` DECISION, pure over the resolved layout (2026-09-18):
/// `Some(dir)` — the absolute managed `<prefix>/agents/`, ensured to exist as a real
/// directory by [`atpkg::store::Layout::ensure_agents_dir`] — is what the session is
/// handed; `None` when there is no layout (no `$HOME`), when the directory could not
/// be created or is a symlink/file (said on stderr ONCE,
/// [`atpkg::store::agents_dir_refusal_line`]),
/// or when its path is not UTF-8. The reroute switch is deliberately NOT an input: the
/// escape hatch is for the upstream Rust names, never for the managed agent programs.
fn agents_dir_handoff(layout: Option<&atpkg::store::Layout>) -> Option<String> {
    let layout = layout?;
    // A RELATIVE prefix is never handed and never created: an empty `$HOME` used to
    // resolve `Library/Application Support/aterm/pkg` against the cwd, and the first
    // cut of this handoff laid `<cwd>/Library/…/agents` inside whatever directory the
    // session was started in (found in the repository root, 2026-09-18). `home_dir`
    // refuses that `$HOME` now; this guard is the seam's own promise, kept whatever
    // resolves the layout.
    if !layout.prefix.is_absolute() {
        eprintln!(
            "aterm: managed agents dir not created (the package prefix {} is not an absolute path); the managed `claude`/`codex` are NOT in front of PATH in this session",
            layout.prefix.display()
        );
        return None;
    }
    match layout.ensure_agents_dir() {
        Ok(dir) => dir.to_str().map(str::to_owned),
        Err(error) => {
            eprintln!(
                "{}",
                atpkg::store::agents_dir_refusal_line(
                    &layout.agents_dir(),
                    &error,
                    layout.is_system_prefix(),
                    SESSION_REACH
                )
            );
            None
        }
    }
}

/// Establish [`agents_dir_handoff`]'s answer in THIS process's environment as
/// [`atpkg::reroute::AGENTS_DIR_ENV`] — set to the directory, or REMOVED (never left
/// as an inherited stray) when there is none — through the workspace's one
/// lock-scoped env helper, before the session's first thread, exactly as the
/// reroute handoff is established.
fn hand_agents_dir(layout: Option<&atpkg::store::Layout>) {
    match agents_dir_handoff(layout) {
        Some(dir) => aterm_log::env::set(atpkg::reroute::AGENTS_DIR_ENV, dir),
        None => aterm_log::env::unset(atpkg::reroute::AGENTS_DIR_ENV),
    }
}

/// Whether this `--session` launch is a PERSON's terminal rather than a harness's
/// child: stdin is a terminal. Only such a launch may spawn the detached toolchain
/// pass — a piped launch is a test or a driver, and a test must never mutate the
/// machine's real package prefix.
fn session_lane_is_interactive() -> bool {
    stdin_is_terminal()
}

/// Whether this launch spawns the detached pass: the MACHINE-WIDE rule the window's loop
/// runs its six-hour walk on ([`aterm_update_core::pkg_check::full_pass_owed`]) over the
/// stamps every lane reads ([`atpkg::status::pass_stamps`]: the outcome the last pass
/// recorded, never `updated_at`; a pass installing now, not queued behind), then the
/// session lane's own claim, so tabs opened in the same moment spawn one pass. The derived
/// model `AtpkgFullPassRule` states the rule — no pass back to back, no success read as a
/// failure, no owed pass held back; there is no rate-limit hold to read, as the pass makes
/// no metered request (owner ruling R3) — and atpkg's `status` conformance binds this
/// reading to it.
fn session_pass_due(layout: &atpkg::store::Layout, now_unix: i64) -> bool {
    use aterm_update_core::pkg_check;
    pkg_check::full_pass_owed(&atpkg::status::pass_stamps(layout, now_unix), now_unix).is_some()
        && pkg_check::claim(
            &layout.session_pass_stamp(),
            now_unix,
            pkg_check::PASS_SPACING_SECS,
        )
}

/// One DETACHED `aterm pkg machine apply` — the lock-free host settings, once a day
/// ([`atpkg::machine::launch_apply_due`]).
///
/// Separate from [`spawn_detached_pkg_update`] because the two are gated differently on
/// purpose: a package pass is due or it is not, and a user may switch it off entirely;
/// the `[machine]` settings are the doctor's, they take no store lock, and their own
/// `[machine]` table is where a user switches them off. Detached and never waited for,
/// like the pass, so a session is never blocked by it.
fn spawn_detached_machine_apply() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args = ["pkg", "machine", "apply"].map(std::ffi::OsString::from);
    if let Err(error) = spawn_detached(exe.as_os_str(), &args, None) {
        aterm_log::warn!(
            "aterm: could not start the background `aterm pkg machine apply`: {error}"
        );
    }
}

/// One DETACHED `aterm pkg update` (R4): this very binary, the `pkg` verb on the window's
/// own argv ([`session_pass_args`]), its own process group so the session's exit (and
/// the SIGHUP that follows it) cannot take the pass down mid-install, stdio on
/// `/dev/null` (atpkg records its own `status.toml`, and its progress file is the one a
/// window tails), and the child never waited for. A spawn that fails is a log line, never
/// stderr — a session must never be blocked, or spoken over, by its package manager.
fn spawn_detached_pkg_update(layout: &atpkg::store::Layout) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let detached = (atpkg::cli::SPAWNER_PID_ENV, atpkg::cli::SPAWNER_DETACHED);
    if let Err(error) = spawn_detached(exe.as_os_str(), &session_pass_args(layout), Some(detached))
    {
        aterm_log::warn!("aterm: could not start the background `aterm pkg update` pass: {error}");
    }
}

/// The detached pass's argv: `pkg update` and the flags every scheduled pass carries
/// ([`aterm_update_core::pkg_check::pass_flags`]) — the window's own — and, like the
/// window's whole pass, [`atpkg::cli::DEFER_BUSY_FLIP_FLAG`]: nobody typed this pass, so a
/// Trust toolchain it would move is staged and flipped only when nothing is using the one
/// it replaces ([`atpkg::quiet`]). Pure for the test.
fn session_pass_args(layout: &atpkg::store::Layout) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "pkg".into(),
        "update".into(),
        atpkg::cli::DEFER_BUSY_FLIP_FLAG.into(),
    ];
    args.extend(aterm_update_core::pkg_check::pass_flags(Some(
        &layout.progress_file(),
    )));
    args
}

/// Start `program args` DETACHED: stdio on `/dev/null`, its own process group, and
/// NOT this process's child. The session lane goes on to run
/// `aterm_cli::session_main` in this same process, and its unix driver reaps the
/// shell with `waitpid(-1)`: a finished pass left as OUR zombie could be reaped in
/// the shell's place, and `aterm` exited with the pass's status instead of the
/// shell's (2026-09-12 audit K9; reliably on Linux, ~6% of exits on macOS). So on
/// unix a `/bin/sh` middle process — the group leader — backgrounds the program and
/// exits at once, and reaping it here re-parents the pass to launchd/init.
fn spawn_detached(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let mut command = std::process::Command::new("/bin/sh");
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        let status = command
            .args(["-c", "\"$@\" &", "sh"])
            .arg(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "the detaching shell {status}"
            )))
        }
    }
    #[cfg(not(unix))]
    {
        let mut command = std::process::Command::new(program);
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
    }
}

/// The SESSION route as a function that RETURNS an `ExitCode`, like every other
/// route `main` dispatches to.
///
/// It exists for one reason, and it is the same one as the `ship` arm's return
/// above. `aterm_cli::session_main` is `-> !` — it owns the process from here to
/// `exit` — and rustc lowers a call to a diverging callee with NO successor
/// block, which leaves the enclosing body's scope teardown sitting on an
/// `unreachable`. The verifier has no body for an out-of-bundle callee, so it
/// cannot see that the call diverges, and it REFUTED that unreachable inside
/// `main` (measured 2026-09-09, `targo trust build --allow-l0-gaps -p aterm`;
/// dropping the semicolon and writing an explicit tail were both refuted too —
/// what `main` has and this function does not is droppable locals still live at
/// the call). Here there is nothing left to tear down, so no `unreachable` is
/// emitted at all and the obligation disappears rather than needing a proof. The
/// call itself is then reported as unmodelled MIR (`Call … targets []`), which is
/// the honest verdict for a callee whose body is not in the bundle.
///
/// `#[inline(never)]` pins the property the fix rests on: inlined back into
/// `main`, `main`'s teardown returns and so does the refutation.
#[inline(never)]
fn session_lane(quiet: bool) -> ExitCode {
    aterm_cli::session_main(quiet)
}

// ---------------------------------------------------------------------------
// ARGV0 ALIAS DISPATCH
// ---------------------------------------------------------------------------

/// How an invocation arriving under an argv0 ALIAS name is served.
///
/// Extracted from `main` as a pure decision so the one case that matters most on
/// Windows is unit-testable: the shipped install is ONE CODE under several names
/// — the console image `aterm.exe` with the CLI names (`aterm-ctl.exe`,
/// `atpkg.exe`, …) hardlinked onto it, and the windowed image `aterm-gui.exe`
/// beside it (see the file header) — the Start-Menu shortcut targets
/// `aterm-gui.exe`, and the taskbar jump list is committed by whichever image is
/// running — so the windowing verbs and the routing policy have to work under
/// the alias, not only under `aterm.exe`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum AliasRoute {
    /// `aterm-ctl` — the control client.
    Ctl,
    /// `atpkg` — the package manager.
    Pkg,
    /// `aterm-fleet` — fleet federation.
    Fleet,
    /// `aterm-drive` — the agent drive CLI.
    Drive,
    /// `aterm-link` — the fabric bridge. It is an alias and not merely a verb
    /// because a bridge is spawned BY NAME out of `[fabric] command`, which is
    /// split on whitespace and exec'd, so the string an operator writes has to
    /// be a path to something executable. Without this arm that path fell
    /// through to `FrontDoor` and opened a WINDOW.
    Link,
    /// `aterm-gui <new-tab|new-window|split-pane> …` — a WINDOWING VERB typed at
    /// (or, far more often, committed into the jump list by) an alias copy. It is
    /// routed exactly as `aterm <verb>` is; handing it to the window's own flag
    /// parser instead is how a taskbar row becomes `unknown command 'new-window'`
    /// against a console that does not exist.
    AliasWindowVerb,
    /// `aterm-gui …` — the window, with the plain-launch routing policy applied
    /// (see [`gui_alias_entry`]). `aterm-windowed` is the same arm: it is the
    /// cargo target name of the windowed image, which a dev tree runs under
    /// that name and `build.ps1` renames to `aterm-gui.exe` for the shipped
    /// folder.
    AliasWindow,
    /// Not an alias: `aterm`, the old `aterm-cli` symlink target, a renamed copy.
    FrontDoor,
}

/// The alias decision: argv0's file stem (already `EXE_SUFFIX`-trimmed) plus the
/// first operand, which is what separates a windowing verb from a window flag.
fn alias_route(argv0: &str, first: &str) -> AliasRoute {
    match argv0 {
        "aterm-ctl" => AliasRoute::Ctl,
        "atpkg" => AliasRoute::Pkg,
        "aterm-fleet" => AliasRoute::Fleet,
        "aterm-drive" => AliasRoute::Drive,
        "aterm-link" => AliasRoute::Link,
        "aterm-gui" | "aterm-windowed" => match aterm_cli::Verb::from_operand(first) {
            Some(verb) if verb.is_windowing() => AliasRoute::AliasWindowVerb,
            // Every OTHER verb stays out of the alias on purpose: `aterm-gui`'s
            // binary-era contract is "the window", and `aterm-gui ctl …` was
            // never a thing anyone could have scripted. Only the verbs that open
            // a terminal — the ones the shell itself launches from the jump list
            // — are lifted into it.
            _ => AliasRoute::AliasWindow,
        },
        _ => AliasRoute::FrontDoor,
    }
}

/// The `aterm-gui` argv0 alias: the WINDOW mode, reached through the same
/// plain-launch policy and the same mode-flag stripping the front door applies.
///
/// It used to be a bare `aterm_gui::main_entry(rest)`, and that was a hole with
/// two live consequences on the shipped Windows install, where this alias is what
/// the Start-Menu shortcut actually launches:
///
/// * the `windowing_behavior` policy never ran for the launcher every real launch
///   goes through, so the headline feature was unreachable from the Start menu;
/// * `--window` — the command line `RegisterApplicationRestart` registers, and the
///   documented "give me the window" flag — reached the window's parser, which
///   knows no such option, so an OS-driven relaunch of this copy exited 2.
///
/// Running the policy fixes the first; stripping `--window` fixes the second.
///
/// ONLY `--window` is stripped, not both mode flags. `--session` asks for a mode
/// this alias cannot serve — the alias IS the window — and it has always been
/// answered with the window parser's `unknown option '--session'`, exit 2.
/// Silently swallowing a mode request would be a worse answer than refusing it,
/// so that one is left exactly where it was.
///
/// `--no-reroute` is consumed here as well (`take_no_reroute`): the alias IS the
/// window, and the window must honour it exactly as the front door does.
///
/// The window runs IN THIS PROCESS on every platform: on Windows the alias
/// names are the windowed image itself (`aterm-gui.exe`, or `aterm-windowed.exe`
/// in a dev tree), which is exactly where a window belongs — see [`run_window`]
/// for the console image's opposite answer. That is also why nothing is handed
/// anywhere (the empty handoff below).
fn gui_alias_entry(rest: Vec<OsString>) -> ExitCode {
    if let ControlFlow::Break(code) = plain_launch_policy(&rest) {
        return code;
    }
    run_window(
        strip_flags(&take_no_reroute(&rest), &["--window"]),
        WindowHost::ThisProcess,
        &[],
    )
}

// ---------------------------------------------------------------------------
// WHERE A WINDOW RUNS: THIS PROCESS, OR THE WINDOWED SIBLING (Windows)
// ---------------------------------------------------------------------------

/// Which PROCESS serves a window this front door has decided to open.
///
/// Off Windows the two are the same thing: the one binary runs the window
/// in-process, as it always has. On Windows the answer depends on which of the
/// two images (see the file header) is running and on what was asked.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum WindowHost {
    /// Run the window here. Right when this image IS the windowed one (the
    /// `aterm-gui` / `aterm-windowed` argv0 aliases), and when the launch is
    /// headless- or diagnose-shaped: the release gates and every harness READ
    /// its output through the pipes they gave it, so it must stay in the
    /// process they spawned.
    ThisProcess,
    /// The CONSOLE image on Windows, asked for a real window: check the
    /// window's arguments HERE, then start the windowed sibling detached and
    /// return at once, so the shell prompt comes back (see [`run_window`]).
    /// Falls back to [`WindowHost::ThisProcess`] — out loud — when there is no
    /// current sibling image beside this one.
    WindowedSibling,
}

/// The host for a window-shaped FRONT-DOOR launch (`aterm --window`, the no-TTY
/// launch), decided from the pre-payload `scan` the mode fork itself read.
///
/// Pure over the argument list — headless is the `--headless` FLAG, the one
/// spelling the mode fork reads (the `ATERM_HEADLESS` environment twin is
/// retired) — so the table is unit-testable. Off Windows every answer is
/// `ThisProcess`; the enum exists so the Windows rule is readable in one place
/// rather than scattered as `cfg`s.
///
/// Only `--headless` and `--diagnose` keep the WINDOW in this process. Every
/// other flag the window's parser answers by printing — `--help`, `--version`,
/// the listings, the registry verbs — and every usage error it refuses with is
/// answered on the sibling route too, in this process and on this console,
/// because that route's first step is that very parser run here
/// (`aterm_gui::check_window_args`). A mirrored list of those flags stood here
/// until 2026-09-27, and any flag added to the parser without it printed into
/// the detached sibling's NUL.
fn window_host_for(scan: &[OsString]) -> WindowHost {
    if !cfg!(windows) {
        return WindowHost::ThisProcess;
    }
    let in_process = scan
        .iter()
        .any(|a| matches!(a.to_string_lossy().as_ref(), "--headless" | "--diagnose"));
    if in_process {
        WindowHost::ThisProcess
    } else {
        WindowHost::WindowedSibling
    }
}

/// Run the WINDOW for `args` where `host` says, and return the route's exit code.
///
/// `WindowedSibling` on Windows is the console image handing the window to
/// `aterm-gui.exe` next to itself and returning 0 while the window is still
/// coming up — the `wt`/`code` shape a shell user expects. `handoff` is the
/// argument list the sibling is started with, which is NOT `args`: the sibling
/// is the whole front door under its alias name, so it is told either the
/// routing decision already made ([`spawn_handoff_argv`]) or the launch's own
/// argument list, never a stripped remainder it would route a second time.
///
/// Before anything is started the window's own parser runs here
/// (`aterm_gui::check_window_args`), because the sibling's stdio is NUL: a
/// usage error (`aterm --window --font-px abc`) printed there reached nobody
/// and the console image exited 0 with no window (review 2026-09-27). Now it
/// prints here and exits 2, as it always did in-process, and a print-and-exit
/// flag prints here too.
///
/// The fallback to an in-process window when there is no current sibling is
/// SAID on stderr: a silent fallback would look exactly like the shipped layout
/// working, on a folder where it is not laid out.
fn run_window(args: Vec<OsString>, host: WindowHost, handoff: &[OsString]) -> ExitCode {
    #[cfg(windows)]
    if host == WindowHost::WindowedSibling {
        aterm_gui::check_window_args(args.clone());
        match windowed_sibling::launch(handoff) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(why) => eprintln!("aterm: {why}; opening the window in this console process"),
        }
    }
    #[cfg(not(windows))]
    let _ = (host, handoff);
    aterm_gui::main_entry(args);
    ExitCode::SUCCESS
}

/// The argument list the windowed sibling is started with for a window the
/// routing policy has already decided to SPAWN: `new-window [-d <dir>]`.
///
/// `new-window` because it is the one verb the policy never redirects
/// (`aterm_cli::route_launch`), so the sibling — the whole front door under
/// its alias name — cannot turn the decision into something else. Handed only
/// `-d <dir>`, it took the plain-launch route and ran the policy a SECOND time,
/// and under `windowing_behavior = "attach"` with an instance up that made
/// `aterm new-window`, and the jump list's New Window row, open a TAB (review
/// 2026-09-27). The directory is `window_args`' own, already absolute.
fn spawn_handoff_argv(request: &aterm_cli::WindowRequest) -> Vec<OsString> {
    let mut argv = vec![OsString::from(aterm_cli::Verb::NewWindow.name())];
    argv.extend(request.window_args());
    argv
}

/// The windowed sibling image on Windows: finding it and starting it detached.
#[cfg(windows)]
mod windowed_sibling {
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    // Tiny FFI, in the style of crates/aterm-pty/src/windows/ffi.rs: the two
    // calls that keep the shell's pipes out of the detached window, plus the
    // query only the test's probe reads (gated with it: a release build of
    // either image warned `GetHandleInformation` is never used, 2026-09-22).
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> isize;
        #[cfg(test)]
        fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    /// `STD_INPUT_HANDLE` / `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE`.
    const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];
    /// `HANDLE_FLAG_INHERIT`.
    const HANDLE_FLAG_INHERIT: u32 = 0x1;

    /// `DETACHED_PROCESS`: the child gets NO console — not ours, and not a new
    /// one — which is what a windowed image wants from a shell launch.
    ///
    /// NOT `CREATE_NEW_PROCESS_GROUP` (0x200), the other "detach" flag: it
    /// starts the child with Ctrl+C DISABLED, and that setting is inherited by
    /// every shell the window then spawns, so no tab could interrupt a command.
    pub(super) const DETACHED_PROCESS: u32 = 0x0000_0008;

    /// How much OLDER than this image a build tree's windowed image may be and
    /// still count as the same build.
    ///
    /// A STALE sibling is the hazard (review 2026-09-27): `cargo run -p aterm`
    /// and `cargo build --bin aterm` rebuild the console image alone, and the
    /// window a developer then asks for would open in the `aterm-windowed.exe`
    /// of some earlier build — old window code, while validating new. But the
    /// two bins of ONE `-p aterm` build are linked in parallel and land in
    /// either order: measured 2026-09-27 in a debug tree, `aterm-windowed.exe`
    /// 6 ms OLDER than `aterm.exe`. So "older at all" would refuse half of all
    /// fresh builds; a minute absorbs the link spread (release links run the
    /// same fat LTO concurrently), and a stale sibling trails by a whole
    /// edit-and-rebuild cycle.
    const SAME_BUILD_SPREAD: Duration = Duration::from_secs(60);

    /// Whether a build tree's windowed image, last written at `sibling`, is too
    /// old to open a window for this image, last written at `this`. Pure, for
    /// the table test; unreadable times are the caller's (they hand off).
    pub(super) fn is_stale(sibling: SystemTime, this: SystemTime) -> bool {
        this.duration_since(sibling)
            .is_ok_and(|older_by| older_by > SAME_BUILD_SPREAD)
    }

    /// Whether `image` carries the cargo TARGET name of the windowed image —
    /// only a build tree has one (`build.ps1` and `install.ps1` lay it as
    /// `aterm-gui.exe`), and only a build tree can hold a stale one.
    fn is_build_tree_image(image: &Path) -> bool {
        image
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("aterm-windowed.exe"))
    }

    /// Stop this process's std handles from being INHERITED by the sibling.
    ///
    /// MEASURED 2026-09-22 on the first build of the split: `$o = & aterm.exe
    /// --window` from pwsh returned from `aterm.exe` at once and then HUNG until
    /// the window was closed. `Stdio::null()` only sets the child's own three
    /// std handles to NUL; `CreateProcess` is still called with
    /// `bInheritHandles = TRUE` (it must be, to pass those), and that hands the
    /// child EVERY inheritable handle in our table — and a shell's stdout /
    /// stderr pipes are inheritable by construction, since we inherited them.
    /// The detached window then held the write end of the shell's pipe and the
    /// shell waited for EOF on it. Clearing `HANDLE_FLAG_INHERIT` on our copies
    /// touches only our handle table; nothing else in this process opens an
    /// inheritable handle before this point, and the process exits right after
    /// the spawn. Best-effort: a handle that refuses the change is left as is,
    /// which is exactly the measured behaviour and not worse.
    pub(super) fn stop_inheriting_std_handles() {
        // SAFETY: handle queries and a flag write on handles this process owns;
        // no pointers cross the boundary.
        unsafe {
            for kind in STD_HANDLES {
                let handle = GetStdHandle(kind);
                if handle != 0 && handle != -1 {
                    let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }

    /// Whether any of this process's std handles is still inheritable —
    /// the observation behind [`stop_inheriting_std_handles`]'s test.
    #[cfg(test)]
    pub(super) fn a_std_handle_is_inheritable() -> bool {
        // SAFETY: handle queries writing one `u32` into a local.
        unsafe {
            STD_HANDLES.into_iter().any(|kind| {
                let handle = GetStdHandle(kind);
                let mut flags = 0u32;
                handle != 0
                    && handle != -1
                    && GetHandleInformation(handle, &mut flags) != 0
                    && flags & HANDLE_FLAG_INHERIT != 0
            })
        }
    }

    /// Start the windowed sibling with `args`, detached, stdio on NUL, and
    /// return once it is running. `Err` names the reason in a sentence the
    /// caller prints before falling back to an in-process window.
    ///
    /// WHICH file is `aterm_gui::windowed_front_door_beside`'s answer — the one
    /// rule the jump list and the Explorer verb use too: `aterm-gui.exe` in an
    /// install or `build.ps1` folder, `aterm-windowed.exe` in a build tree,
    /// whose `aterm-gui.exe` is crate aterm-gui's thin dev bin (the window
    /// library with none of this front door) and is never taken. A build
    /// tree's image must also be as new as this one ([`is_stale`]).
    ///
    /// Stdio on NUL is what keeps the sibling OFF this console: it then finds
    /// three real (device) handles, so its `attach_parent_console` attaches
    /// nothing and no startup line of its can reach the prompt the shell has
    /// already redrawn (defect c of the 2026-09-22 audit). Nothing is waited for
    /// — the window's lifetime is its own — and nothing of the shell's is
    /// carried into it either ([`stop_inheriting_std_handles`]), so a script
    /// that captured our output gets its pipe's EOF the moment we exit.
    pub(super) fn launch(args: &[OsString]) -> Result<(), String> {
        use std::os::windows::process::CommandExt as _;
        let exe =
            std::env::current_exe().map_err(|e| format!("cannot locate this executable ({e})"))?;
        let Some(sibling) = aterm_gui::windowed_front_door_beside(&exe) else {
            return Err(format!(
                "no windowed image (aterm-gui.exe, or aterm-windowed.exe in a build tree) \
                 beside {}",
                exe.display()
            ));
        };
        let written = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if is_build_tree_image(&sibling)
            && let (Some(sibling_at), Some(this_at)) = (written(&sibling), written(&exe))
            && is_stale(sibling_at, this_at)
        {
            return Err(format!(
                "{} is older than {}",
                sibling.display(),
                exe.display()
            ));
        }
        stop_inheriting_std_handles();
        std::process::Command::new(&sibling)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED_PROCESS)
            .spawn()
            .map(drop)
            .map_err(|e| format!("could not start {} ({e})", sibling.display()))
    }
}

/// THE NESTED-SESSION DECISION (Windows): whether a session-shaped launch was
/// typed inside an aterm tab and should open a tab there instead of nesting.
///
/// Pure, so the table is a unit test. `bare` is a launch with NO argument at
/// all — the one shape the defect was: any flag, a `-e`/`--command`/`--` child
/// command line, or a typo is a request the session parser must serve or
/// refuse, never one to drop for a plain tab; `force_session` is the `aterm-cli`
/// argv0 alias (and an explicit `--session`, though that one is never bare) —
/// "nest, I know"; `aterm_child` is the tab marker the window sets for every
/// child; `stdin_tty` is the mode fork's own probe — a piped launch inside a
/// tab is a harness, and a harness gets what it asked for.
///
/// Compiled off Windows only for its test: the guard that consults it is
/// Windows-only, and Unix keeps nesting (a nested session there is the
/// documented way to run one).
#[cfg(any(windows, test))]
fn nests_inside_aterm(bare: bool, force_session: bool, aterm_child: bool, stdin_tty: bool) -> bool {
    bare && !force_session && aterm_child && stdin_tty
}

/// What the guarded launch does instead of nesting: `aterm new-tab -d <cwd>`,
/// FORCED to the enclosing instance — not routed by `windowing_behavior`, whose
/// shipped default (`new_window`) would open a second window for a launch that
/// plainly meant "a terminal here". Unreachable instance (a tab whose window
/// died, a socket switched off): a new window, said out loud, through the same
/// sibling handoff every other window takes.
#[cfg(windows)]
fn nested_new_tab() -> ExitCode {
    const ALREADY_INSIDE: &str =
        "aterm: opened a new tab in this aterm window (`aterm --session` nests a session instead)";
    let request = match aterm_cli::parse_window_request(
        "new-tab",
        &[OsString::from("-d"), OsString::from(".")],
        resolve_dir_absolute,
    ) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("aterm: {message}");
            return ExitCode::from(2);
        }
    };
    let forwarded = aterm_ctl::front_door_instance().and_then(|sock| {
        let line = request.control_request().ok()?;
        Some(aterm_ctl::front_door_send(&sock, &line))
    });
    match forwarded {
        Some(Ok(reply)) if reply.starts_with("OK") => {
            eprintln!("{ALREADY_INSIDE}");
            ExitCode::SUCCESS
        }
        Some(Ok(reply)) => {
            eprintln!("aterm: this aterm window refused a new tab: {reply}");
            ExitCode::FAILURE
        }
        Some(Err(error)) => {
            eprintln!("aterm: could not reach this aterm window ({error}); opening a new window");
            run_window(
                request.window_args(),
                WindowHost::WindowedSibling,
                &spawn_handoff_argv(&request),
            )
        }
        None => {
            eprintln!("aterm: could not reach this aterm window; opening a new window");
            run_window(
                request.window_args(),
                WindowHost::WindowedSibling,
                &spawn_handoff_argv(&request),
            )
        }
    }
}

/// The index of the `-e`/`--command`/`--` PAYLOAD BOUNDARY, or `rest.len()`.
/// Past it every token belongs to a child command line and is neither scanned
/// nor stripped — the window's `-e` contract.
fn payload_boundary(rest: &[OsString]) -> usize {
    rest.iter()
        .position(|a| matches!(a.to_string_lossy().as_ref(), "-e" | "--command" | "--"))
        .unwrap_or(rest.len())
}

/// The two MODE flags: they select which mode runs, and the mode libraries do
/// not know them.
const MODE_FLAGS: &[&str] = &["--window", "--session"];

/// `rest` with `flags` removed — but only BEFORE the payload boundary: a `--` or
/// `-e` payload is a child command line and passes through verbatim, a
/// `--window` inside it included.
fn strip_flags(rest: &[OsString], flags: &[&str]) -> Vec<OsString> {
    let boundary = payload_boundary(rest);
    rest.iter()
        .enumerate()
        .filter(|(i, a)| *i >= boundary || !flags.contains(&a.to_string_lossy().as_ref()))
        .map(|(_, a)| a.clone())
        .collect()
}

/// `rest` with both [`MODE_FLAGS`] removed — the front door's own strip, applied
/// once the fork has read them.
fn strip_mode_flags(rest: &[OsString]) -> Vec<OsString> {
    strip_flags(rest, MODE_FLAGS)
}

/// `--no-reroute`: restore the upstream Rust names in this session (`aterm help
/// reroute`; `docs/DESIGN-toolchain-reroute-2026-09-07.md` §"Reaching PATH" 3) — THE
/// spelling of that escape (the `ATERM_NO_REROUTE` variable that was its twin is gone,
/// 2026-09-23: "NOT ENV VARS those are for development"). Consumed by
/// [`take_no_reroute`] exactly like a mode flag: stripped before the payload boundary
/// (neither mode library knows it) and, when present, turned into the INTERNAL marker
/// [`atpkg::reroute::PASSTHROUGH_ENV`] in THIS process's environment — so both lanes
/// read one answer, and every child inherits it.
const NO_REROUTE_FLAG: &str = "--no-reroute";

/// `rest` with [`NO_REROUTE_FLAG`] stripped, and the marker ESTABLISHED to match:
/// [`atpkg::reroute::PASSTHROUGH_ENV`]`=1` when the flag was present, and REMOVED when
/// it was not — an inherited marker (this launch typed inside a `--no-reroute`
/// session) is protocol from another launch, never an instruction to this one, so a
/// launch without the flag always gets the reroute. Trusted-launcher idiom (the
/// `--containment` precedent in aterm-cli's parser): single-threaded startup, before
/// the update checker's thread and before any PTY byte flows, through the workspace's
/// one lock-scoped env helper. Inside a `-e`/`--` payload the token belongs to the
/// child command line and is neither read nor stripped. A launch carrying the flag is
/// deliberately NOT a "plain" launch for the single-instance routing policy (which
/// reads the unstripped `rest`): a tab forwarded to another instance would not carry
/// the environment the flag asked for, so it opens here.
fn take_no_reroute(rest: &[OsString]) -> Vec<OsString> {
    // Same form, same reason as the `scan` slice above.
    if rest
        .get(..payload_boundary(rest))
        .unwrap_or(rest)
        .iter()
        .any(|a| a.to_string_lossy() == NO_REROUTE_FLAG)
    {
        aterm_log::env::set(atpkg::reroute::PASSTHROUGH_ENV, "1");
    } else {
        aterm_log::env::unset(atpkg::reroute::PASSTHROUGH_ENV);
    }
    strip_flags(rest, &[NO_REROUTE_FLAG])
}

/// The request a PLAIN window launch would forward, or `None` when this launch
/// must not be routed by policy at all.
///
/// `argv` is THE WHOLE ARGUMENT LIST — every token, `-e` payload included — and
/// that word is the point. The mode fork works from `scan`, which stops AT the
/// payload boundary, so `aterm -e vim` reaches a gate handed that slice as an
/// EMPTY list: the barest, most obviously-forwardable launch there is. The gate's
/// documented `-e` exclusion would then never fire, and under `attach` a launch
/// carrying a child command line would forward into a tab that cannot run it and
/// exit 0 with the command silently dropped. The boundary is refused here too, so
/// the rule holds however this is called.
///
/// Apart from resolving `-d` against the real filesystem this is pure, which is
/// what lets the payload rule be pinned by a unit test.
fn plain_launch_request(
    argv: &[OsString],
    env: aterm_cli::LaunchEnv,
) -> Option<aterm_cli::WindowRequest> {
    if payload_boundary(argv) != argv.len() {
        return None;
    }
    if !aterm_cli::plain_launch_is_policy_eligible(argv, env) {
        return None;
    }
    let dir = plain_launch_dir(argv).ok()?;
    Some(aterm_cli::WindowRequest {
        intent: aterm_cli::LaunchIntent::Plain,
        dir,
        split: aterm_cli::SplitOrientation::default(),
    })
}

/// SINGLE-INSTANCE ROUTING (S12) for a PLAIN window launch — Explorer, the Start
/// menu, a pinned tile, `aterm --window` with nothing else asked of it.
/// `Break(code)` when the running instance served it; `Continue` to go on and
/// open a window here — carrying the routed request when the policy DECIDED
/// that (the console image on Windows hands the decision, not the launch, to
/// the windowed sibling: [`spawn_handoff_argv`]), and `None` when the launch
/// was not the policy's to route at all.
///
/// The eligibility gate is `plain_launch_is_policy_eligible` and it is
/// deliberately narrow: `-e`, `--headless`, `--diagnose` and an
/// update successor's inherited argv all carry instructions a forwarded tab
/// cannot honour, so they fail closed to spawning. See that function for the
/// case-by-case reasoning. Under the shipped default this spawns without
/// dialing anything.
fn plain_launch_policy(
    argv: &[OsString],
) -> ControlFlow<ExitCode, Option<aterm_cli::WindowRequest>> {
    let env = aterm_cli::LaunchEnv {
        updated_from: std::env::var_os("ATERM_UPDATED_FROM").is_some(),
    };
    let Some(request) = plain_launch_request(argv, env) else {
        return ControlFlow::Continue(None);
    };
    match route_and_maybe_forward(&request) {
        ControlFlow::Break(code) => ControlFlow::Break(code),
        ControlFlow::Continue(_) => ControlFlow::Continue(Some(request)),
    }
}

// ---------------------------------------------------------------------------
// THE WINDOWING VERBS AND THE ROUTING POLICY (S12 / design §5)
// ---------------------------------------------------------------------------

/// `aterm new-tab | new-window | split-pane [-d <dir>] [-H|-V]` — the wt-shaped
/// front door.
///
/// The grammar and the routing rule are both PURE and live in `aterm-cli`
/// ([`aterm_cli::parse_window_request`] / [`aterm_cli::route_launch`]); this
/// function is only the impure half — resolving a directory against the real
/// filesystem, asking whether an instance is reachable, and performing whichever
/// of the two routes came back.
///
/// EXIT CODES, which matter more here than they look. Under the WINDOWED image
/// on Windows (`aterm-gui.exe new-window` from the jump list) the console this
/// prints to is the parent's, reattached by `attach_parent_console` before ANY
/// route runs — including this one. That reattachment deliberately restores
/// the parent's own redirected handles (the `aterm --version > out.txt`
/// invariant `build.ps1` depends on), and nothing here disturbs it: this route
/// only ever writes to the already-resolved `stderr`, and it returns an
/// `ExitCode` rather than calling `process::exit`, so `main`'s normal teardown
/// still runs. Under the console image the shell simply waits for these codes.
///   * `0` — the tab/window/pane was opened (forwarded or spawned).
///   * `1` — the running instance answered `ERR`; its text is on stderr.
///   * `2` — a grammar error (unknown option, missing `<dir>`, bad directory).
///
/// `host` is where a SPAWNED window runs (see [`WindowHost`]): the console image
/// hands it to the windowed sibling — as `new-window`, the decision this
/// function just made ([`spawn_handoff_argv`]) — and the windowed image runs it
/// here.
fn window_verb(verb: &str, args: &[OsString], host: WindowHost) -> ExitCode {
    // `-h`/`--help` on a verb answers with that verb's own synopsis rather than
    // the "unknown option" the strict grammar would otherwise produce. Every
    // other front-door verb forwards `--help` to the tool it dispatches to and
    // gets help; these dispatch to no tool, so the front door owns the answer.
    // First position only, exactly like the sibling verbs' pre-verb flags.
    if args
        .first()
        .is_some_and(|a| matches!(a.to_string_lossy().as_ref(), "-h" | "--help"))
    {
        println!("{}", aterm_cli::window_verb_usage(verb));
        if let Some(v) = aterm_cli::Verb::from_operand(verb) {
            for line in v.blurb() {
                println!("    {line}");
            }
        }
        return ExitCode::SUCCESS;
    }
    let request = match aterm_cli::parse_window_request(verb, args, resolve_dir_absolute) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("aterm: {message}");
            return ExitCode::from(2);
        }
    };
    let why = match route_and_maybe_forward(&request) {
        ControlFlow::Break(code) => return code,
        ControlFlow::Continue(why) => why,
    };
    // The SPAWN route. A brand-new window is a single pane, so a `split-pane` that
    // lands here has nothing to split. Say why rather than open a window that
    // silently is not what was asked for — `wt split-pane` under `useNew` has
    // exactly this outcome, and quietly is the wrong way to have it.
    if request.intent == aterm_cli::LaunchIntent::SplitPane
        && let Some(line) = split_pane_spawn_line(why)
    {
        eprintln!("{line}");
    }
    run_window(request.window_args(), host, &spawn_handoff_argv(&request))
}

/// Why a request the running instance did not answer opens a window here.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum SpawnHere {
    /// Nothing was forwarded by choice: `windowing_behavior = new_window` (the probe is
    /// skipped), or the verb itself asks for a new window.
    Policy,
    /// `attach`, and no running aterm answered the probe.
    NoInstance,
    /// `attach`, one answered, and the forward then failed — already said on stderr.
    Unreachable,
}

/// The line a `split-pane` that opens a new window prints, or `None` when
/// [`route_and_maybe_forward`] already said why (a second line would repeat "opening a
/// new window").
fn split_pane_spawn_line(why: SpawnHere) -> Option<&'static str> {
    match why {
        SpawnHere::Policy => Some(
            "aterm: windowing_behavior is new_window, so split-pane opens a new window; \
             set it to \"attach\" in aterm.toml to split the running aterm",
        ),
        SpawnHere::NoInstance => Some("aterm: no running aterm to split; opening a new window"),
        SpawnHere::Unreachable => None,
    }
}

/// Decide the route for `request` and, when it is `Forward`, perform it.
///
/// Returns `Break(code)` when the request was answered by the running instance
/// (the process should exit with that code) and `Continue(why)` when the caller
/// should go on to start a window itself. The two impure inputs — the effective policy
/// and whether an instance answered — are gathered here and handed to the pure
/// [`aterm_cli::route_launch`], so the decision itself stays testable.
///
/// The reachability probe and the forward are two separate dials, so an instance
/// can die in between. That race resolves to `Continue` (spawn), not an error: the
/// operator asked for a terminal and a transport failure is not a reason to
/// refuse one. An `ERR` reply is the opposite case — the instance IS there and
/// REFUSED — and is reported as a failure, because spawning a window then would
/// contradict the policy the operator chose AND could double-open if the refusal
/// was partial.
fn route_and_maybe_forward(request: &aterm_cli::WindowRequest) -> ControlFlow<ExitCode, SpawnHere> {
    let behavior = effective_windowing_behavior();
    // The probe is skipped entirely under `new_window`: it costs a connect
    // attempt on the front door of every launch, and its answer cannot change
    // the route. `route_launch` is still consulted with `false`, so the table in
    // its tests remains the single description of the rule.
    let should_probe = behavior == aterm_cli::WindowingBehavior::Attach;
    let sock = if should_probe {
        aterm_ctl::front_door_instance()
    } else {
        None
    };
    let reachable = sock.is_some();
    let route = aterm_cli::route_launch(request.intent, behavior, reachable);
    let Some(sock) = sock.filter(|_| route == aterm_cli::WindowRoute::Forward) else {
        return ControlFlow::Continue(if should_probe && !reachable {
            SpawnHere::NoInstance
        } else {
            SpawnHere::Policy
        });
    };
    let line = match request.control_request() {
        Ok(line) => line,
        Err(message) => {
            eprintln!("aterm: {message}");
            return ControlFlow::Break(ExitCode::from(2));
        }
    };
    match aterm_ctl::front_door_send(&sock, &line) {
        // `spawn` replies `OK <sid>`. The sid is deliberately NOT printed: `wt
        // new-tab` prints nothing, and a shell prompt is not a log.
        Ok(reply) if reply.starts_with("OK") => ControlFlow::Break(ExitCode::SUCCESS),
        Ok(reply) => {
            // The reason, not the wire's `ERR ` token.
            let reason = reply.strip_prefix("ERR ").unwrap_or(&reply);
            eprintln!("aterm: the running aterm refused: {reason}");
            ControlFlow::Break(ExitCode::FAILURE)
        }
        Err(error) => {
            // Raced (the instance exited between the probe and the dial), or the
            // socket wedged. Fall back to starting one — with a line saying why,
            // so an operator who set `attach` is never left wondering why a
            // second window appeared.
            eprintln!("aterm: could not reach the running aterm ({error}); opening a new window");
            ControlFlow::Continue(SpawnHere::Unreachable)
        }
    }
}

/// The effective `windowing_behavior`: the `aterm.toml` key, else the default. An unrecognized spelling warns ONCE and
/// falls back — silently treating a typo as `attach` would move where every
/// terminal on the machine opens.
fn effective_windowing_behavior() -> aterm_cli::WindowingBehavior {
    let raw = aterm_gui::windowing_behavior_setting();
    match raw.as_deref() {
        None => aterm_cli::WindowingBehavior::NewWindow,
        Some(value) => match aterm_cli::WindowingBehavior::parse(value) {
            Some(behavior) => behavior,
            None => {
                eprintln!(
                    "aterm: windowing_behavior {value:?} is not new_window or attach; \
                     using new_window"
                );
                aterm_cli::WindowingBehavior::NewWindow
            }
        },
    }
}

/// Resolve one `-d <dir>` operand to the ABSOLUTE native path a forwarded
/// request must carry.
///
/// Absolute because the request may be served by a process whose working
/// directory is elsewhere entirely — a relative `-d src` forwarded verbatim
/// would open the running instance's `src`, not the caller's. `std::path::
/// absolute` rather than `canonicalize`: on Windows the latter returns the
/// `\\?\C:\…` extended-length form, which is a legal path but an ugly one to
/// hand a shell as its cwd (and one some shells' own prompt logic mishandles),
/// and it resolves symlinks the operator may have deliberately used.
///
/// The directory is checked HERE, before anything is dialed, so `aterm new-tab
/// -d nope` fails the same way under both policies with the same wording the
/// window library's own `-d` uses.
fn resolve_dir_absolute(raw: &str) -> Result<String, String> {
    let path = std::path::Path::new(raw);
    let absolute = std::path::absolute(path).map_err(|e| format!("cannot resolve {raw}: {e}"))?;
    if !absolute.is_dir() {
        return Err(format!("not a directory: {raw}"));
    }
    Ok(absolute.to_string_lossy().into_owned())
}

/// The `-d <dir>` of a PLAIN window launch, resolved to an absolute path.
///
/// The scan itself is `aterm_cli::plain_launch_dir_operand`, which shares its
/// flag set with the eligibility gate — the two must never disagree about what a
/// directory flag looks like, and that set is deliberately only the spellings the
/// WINDOW's own parser accepts (see `PLAIN_LAUNCH_DIR_FLAGS`). This function adds
/// the impure half: resolving the value against the real filesystem.
///
/// `Err` means the operand cannot be resolved — the caller then declines to
/// route by policy and lets the ordinary spawn path report it, so there is
/// exactly ONE "not a directory" message and it is the window library's, which
/// is where `-d` has always been validated.
fn plain_launch_dir(scan: &[OsString]) -> Result<Option<String>, ()> {
    match aterm_cli::plain_launch_dir_operand(scan) {
        Some(value) => resolve_dir_absolute(&value).map(Some).map_err(|_| ()),
        None => Ok(None),
    }
}

/// `aterm update [status|check] [-v]` — the headless update lane, served in-process
/// from the shared ledger (`status`) and the one-shot checker (`check`): no control
/// socket, no window. `aterm ctl update status` remains the machine surface a controller
/// drives against a RUNNING window; this verb is what a terminal-only machine (round-11:
/// previously unable to even report its own staleness) and scripts get. On macOS apply
/// is deliberately absent: applying re-execs a process and rides the window entry's
/// trial/rollback confirmation. Linux replaces only the on-disk executable, so there
/// `enable`, `apply`, `rollback` and the installers' `install` are verbs here; and
/// `identity` prints the running binary's compiled identity on every platform.
///
/// It says ONE plain line ([`update_summary`]) — `aterm 0.91.0 is up to date · checked
/// 12 min ago` — and, when something is wrong, one more on stderr saying where the rest
/// is (a `check` at a terminal first says it is checking, on stderr). `-v` adds the
/// ledger's own detail ([`print_update_detail`]). `check` exits nonzero when the copy
/// cannot update or its checks are failing; `status` when there is no ledger to read.
fn update_verb(rest: &[OsString]) -> ExitCode {
    // The updater logs through `aterm_log`. Its records go to aterm.log with the
    // window's and the session's, never to this terminal: a check used to print every
    // INFO record raw ("INFO: aterm-update: authoritative v0.91.0 was signed by machine
    // m3 …", 2026-09-23 audit). What went wrong reaches the terminal as the plain line
    // below, and the whole record is in Settings ▸ Messages and the file.
    aterm_gui::install_session_log();
    let build = aterm_gui::running_build_number();
    let first = rest.first().map(|a| a.to_string_lossy().into_owned());
    if first.as_deref() == Some("identity") && rest.len() == 1 {
        return match aterm_update::binary_identity_json(&aterm_gui::running_binary_identity()) {
            Ok(identity) => {
                println!("{identity}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("aterm update: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if matches!(first.as_deref(), Some("--help" | "-h" | "help")) && rest.len() == 1 {
        // The verbs THIS binary answers: the Linux delivery's `enable | apply |
        // rollback` exist only there (a Mac refuses them), and `install` is the
        // installer's own step (tools/install.sh), never typed — as is `enable
        // --proof-dir`, whose signed release files only an installer holds.
        println!("{UPDATE_USAGE}");
        #[cfg(target_os = "linux")]
        println!(
            "Linux: aterm update enable | apply | rollback\n\
             Updates replace only the on-disk executable; running sessions are never \
             restarted."
        );
        return ExitCode::SUCCESS;
    }
    #[cfg(target_os = "linux")]
    if let Some(sub) = first
        .as_deref()
        .filter(|sub| matches!(*sub, "enable" | "apply" | "rollback" | "install"))
    {
        let result = match sub {
            "enable" if rest.len() <= 1 => aterm_update::linux::enable(build, None),
            "enable" if rest.len() == 3 && rest[1] == "--proof-dir" => {
                aterm_update::linux::enable(build, Some(std::path::Path::new(&rest[2])))
            }
            "apply" if rest.len() == 1 => aterm_update::linux::apply(),
            "rollback" if rest.len() == 1 => aterm_update::linux::rollback(),
            "install"
                if rest.len() == 7
                    && rest[1] == "--target"
                    && rest[3] == "--proof-dir"
                    && rest[5] == "--candidate" =>
            {
                aterm_update::linux::install_release(
                    std::path::Path::new(&rest[2]),
                    std::path::Path::new(&rest[6]),
                    std::path::Path::new(&rest[4]),
                )
            }
            _ => Err("usage: aterm update enable | apply | rollback".into()),
        };
        return match result {
            Ok(message) => {
                println!("aterm update: {message}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("aterm update: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let mut verbose = false;
    let mut sub = None;
    for arg in rest.iter().map(|a| a.to_string_lossy().into_owned()) {
        match arg.as_str() {
            "-v" | "--verbose" => verbose = true,
            _ if sub.is_none() && !arg.starts_with('-') => sub = Some(arg),
            _ => {
                eprintln!("aterm: unknown update argument {arg:?} ({UPDATE_USAGE})");
                return ExitCode::from(2);
            }
        }
    }
    let checking = sub.as_deref() == Some("check");
    let st = match sub.as_deref().unwrap_or("status") {
        "status" => aterm_update::status(build),
        // Windows has no updater at all yet (design §7, W8 open): there is nothing to
        // check, and `check_now` is only the unsupported-platform stub, whose sentence
        // names no way to update. Answer as `status` does — the line that names the
        // lane that DOES update a Windows copy (audit 2026-09-22).
        "check" if cfg!(windows) => None,
        "check" => {
            // A check can download a whole release: say it is working, to a person only
            // — and only on a copy that checks (an installed aterm.app, an enrolled Linux
            // copy). A dev, disk-image or quarantined copy answers at once with its
            // refusal, and a Linux install waiting for its first window skips the check;
            // announcing a check there is a line about work that never starts.
            if std::io::IsTerminal::is_terminal(&std::io::stderr())
                && aterm_update::status(build).is_some_and(|st| {
                    st.enabled && st.installable && !linux_install_waits_for_window(&st)
                })
            {
                eprintln!("Checking for updates\u{2026}");
            }
            let provider: aterm_update::CheckSettingsProvider =
                std::sync::Arc::new(aterm_gui::configured_update_settings);
            Some(aterm_update::check_now_with_settings(build, &provider))
        }
        other => {
            eprintln!("aterm: unknown update sub-command {other:?} ({UPDATE_USAGE})");
            return ExitCode::from(2);
        }
    };
    // `None` off macOS and Linux (no updater), or on a Mac when the ledger under
    // `~/Library/Application Support/aterm/Updates` cannot be reached: `HOME` unset, or
    // the directory not private (`ensure_private_dir`). One cause per line, never a
    // hedge between them.
    let Some(st) = st else {
        println!("{}", update_unreadable_line(aterm_gui::running_version()));
        return ExitCode::FAILURE;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let (line, trouble) = update_summary(
        aterm_gui::running_version(),
        build,
        &st,
        now,
        aterm_update_core::settings::update_auto_apply(),
        aterm_update::automatic(),
        &DevCopy::of_this_copy(checking),
    );
    aterm_log::info!("aterm update: {line}");
    println!("{line}");
    if let Some(trouble) = trouble {
        eprintln!("{trouble}");
    }
    if verbose {
        print_update_detail(build, &st);
    }
    // A check that could not do its job says so in its exit status too (a script's
    // only reading): the copy cannot update, its channel is unreadable, or checks fail.
    if checking
        && (!st.enabled || !st.installable || st.channel_unreadable || st.failing_checks > 0)
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The verbs every platform's `aterm update` answers; the Linux-only ones are
/// listed under it by `--help` on Linux alone.
const UPDATE_USAGE: &str = "usage: aterm update [status|check] [-v] | identity";

/// What `aterm update` knows of THIS copy's dev mark (`tools/dev-app.sh`,
/// `aterm_update::running_is_dev_marked`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum DevCopy {
    /// Not dev-marked: the updater's own ledger speaks for it.
    No,
    /// Dev-marked, so the updater leaves it alone. `lag` is where it stands against
    /// the public channel when the channel was read — ONE read-only HEAD of the
    /// evergreen appcast (`aterm_update::dev_channel`) — and `None` when it was not
    /// (automatic checks off on `status`, or the channel unreachable, which says
    /// nothing). `shared_writes`: what it writes of the shared user state only the
    /// release writes unattended, and what puts it back
    /// (`aterm_gui::dev_build_shared_writes` — a dev build whose bundle is named
    /// `aterm.app` runs those writers), `None` for one that does not. Read whether or
    /// not the channel was: it is this copy's own fact.
    Marked {
        lag: Option<aterm_update::dev_channel::DevLag>,
        shared_writes: Option<aterm_gui::DevSharedWrites>,
    },
}

impl DevCopy {
    /// THIS copy, read now: the dev mark and, for a dev build, its standing — asked of
    /// the channel on a typed `check`, and on `status` only while automatic checks are
    /// on (the one switch that keeps aterm off the network by itself).
    fn of_this_copy(asked_check: bool) -> Self {
        if !aterm_update::running_is_dev_marked() {
            return Self::No;
        }
        let lag = if asked_check || aterm_update::automatic() {
            aterm_update::dev_channel::standing(aterm_gui::running_version())
        } else {
            None
        };
        Self::Marked {
            lag,
            shared_writes: aterm_gui::dev_build_shared_writes(),
        }
    }
}

/// What a dev build named `aterm.app` does to the release beside it — what it writes and
/// the release's verbs that put it back (`aterm_gui::dev_build_shared_writes`) — said on
/// this stderr line of `aterm update`, as the window's row says it when it starts.
fn dev_shared_writes_line(writes: aterm_gui::DevSharedWrites) -> String {
    let aterm_gui::DevSharedWrites { what, repair } = writes;
    format!(
        "This dev build writes {what}, which only the release writes by itself, because its \
         bundle is named aterm.app \u{2014} rebuild it with tools/dev-app.sh (it installs as \
         aterm (dev).app) and remove this copy, then run {repair} from the release."
    )
}

/// What `aterm update` says when there is no ledger to read: off macOS and Linux
/// that is the platform; on a Mac it is `HOME` unset or the updates directory not
/// being a private one of the user's (`ensure_private_dir`: a real directory, owned,
/// mode 0700, not a symlink) — the two ways `Staging::resolve` answers `None`.
/// On Windows (no updater yet, so `check` lands here too) the line names the lane
/// that does update a Windows copy: the platform-only sentence sent a reader to
/// wait for an updater that does not exist (audit 2026-09-22).
fn update_unreadable_line(version: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        let why = if std::env::var_os("HOME").is_none() {
            "HOME is not set"
        } else {
            "~/Library/Application Support/aterm/Updates isn\u{2019}t a private directory of \
             yours (mode 0700, not a symlink)"
        };
        format!("aterm {version} can\u{2019}t read its update ledger: {why}")
    }
    #[cfg(windows)]
    {
        format!(
            "aterm {version} doesn\u{2019}t update itself on Windows yet \u{2014} to update it, \
             run {} in your aterm checkout",
            aterm_cli::WINDOWS_UPDATE_LANE
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        format!("aterm {version} doesn\u{2019}t update itself on this platform")
    }
}

/// Where a trouble line sends the reader for the rest.
#[cfg(target_os = "macos")]
const UPDATE_LOG_HINT: &str =
    "Details are in Settings \u{25b8} Messages, and in ~/Library/Logs/aterm/aterm.log.";
/// Where a trouble line sends the reader for the rest.
#[cfg(not(target_os = "macos"))]
const UPDATE_LOG_HINT: &str =
    "Details are in Settings \u{25b8} Messages, and in ~/.local/state/aterm/logs/aterm.log.";

/// What `aterm update status|check` says: ONE plain line for stdout, and — when
/// something is wrong — one for stderr. The version a person knows (the build number
/// is `--version`'s and About's); what is on offer and when it installs; when the last
/// check completed, relative. The updater's own decision sentence, the lane and the
/// counters stay in `aterm ctl update status`, `-v` and the log (2026-09-23 audit).
/// `installs_by_itself` is `[update] auto_apply`, `automatic_checks` `[update] enabled`,
/// `dev` whether the running bundle carries the dev mark (`tools/dev-app.sh`) and, for a
/// dev build, where it stands against the public channel ([`DevCopy`]).
/// Pure for the test.
fn update_summary(
    version: &str,
    build: u64,
    st: &aterm_update::UpdateStatus,
    now: i64,
    installs_by_itself: bool,
    automatic_checks: bool,
    dev: &DevCopy,
) -> (String, Option<String>) {
    if !st.enabled {
        // Only a Linux copy reaches here (macOS always has an updater; elsewhere there
        // is no ledger at all): not enrolled, or enrollment never finished — per copy,
        // since each executable keeps its own record. The ledger's sentence names the
        // remedy (`aterm update enable`) or why this copy's path can't be updated.
        let remedy = st.outcome.trim();
        return (
            format!("This copy of aterm {version} doesn\u{2019}t update itself"),
            (!remedy.is_empty()).then(|| remedy.to_string()),
        );
    }
    // A Linux copy stages on disk and replaces only its executable: a staged build is
    // applied with `aterm update apply` (upstream's Linux delivery, d0a4c2930).
    if let Some(staged) = st
        .linux
        .as_ref()
        .and_then(|native| {
            native
                .staged_build
                .map(|b| (b, native.staged_version.clone()))
        })
        .filter(|(b, _)| *b > build)
    {
        let next = staged.1.unwrap_or_else(|| format!("build {}", staged.0));
        return (
            format!("aterm {next} is downloaded \u{2014} `aterm update apply` installs it"),
            None,
        );
    }
    // A replaced Linux executable is final once an aterm window starts from it; until
    // then a check reads no channel, so "up to date" would be a claim nothing tested.
    if linux_install_waits_for_window(st) {
        return (
            format!(
                "aterm {version} is installed \u{2014} launch an aterm window once to finish; \
                 update checks wait until then"
            ),
            None,
        );
    }
    if !st.installable {
        // The checker deliberately does nothing here, which otherwise reads as idleness.
        // A dev-marked bundle (`tools/dev-app.sh`, in /Applications or not) is left
        // alone on purpose and has nowhere to move: say that, not "move it" — and, when
        // the channel was read, how far behind it (gap #30: a weeks-old dev bundle ran
        // old code with nothing saying so). A `target/` binary, a disk-image launch and
        // a quarantined download get the remedy.
        return if let DevCopy::Marked { lag, shared_writes } = dev {
            let standing = lag
                .as_ref()
                .map(|lag| format!(", {}", lag.words()))
                .unwrap_or_default();
            (
                format!(
                    "This copy of aterm {version} is a dev build{standing} \u{2014} the \
                     updater leaves it alone"
                ),
                shared_writes.map(dev_shared_writes_line),
            )
        } else {
            (
                format!(
                    "This copy of aterm {version} can\u{2019}t update itself \u{2014} only \
                     aterm.app installed in Applications does"
                ),
                None,
            )
        };
    }
    if let Some(staged) = st.staged_build.filter(|staged| *staged > build) {
        let next = st
            .staged_version
            .clone()
            .unwrap_or_else(|| format!("build {staged}"));
        return if st.failing_applies > 0 {
            let tries = if st.failing_applies == 1 {
                "1 try".to_string()
            } else {
                format!("{} tries", st.failing_applies)
            };
            (
                format!("aterm {next} is downloaded but didn\u{2019}t install ({tries})"),
                Some(UPDATE_LOG_HINT.to_string()),
            )
        } else if installs_by_itself {
            // The window installs it (a terminal session never replaces itself).
            (
                format!(
                    "aterm {next} is downloaded and installs within a minute while an aterm \
                     window is open"
                ),
                None,
            )
        } else {
            (
                format!(
                    "aterm {next} is downloaded \u{2014} install it from Settings \u{25b8} \
                     Software Update"
                ),
                None,
            )
        };
    }
    if st.channel_unreadable {
        // The ledger's sentence here is the remedy only an operator can apply.
        let remedy = st.outcome.trim();
        return (
            format!("aterm {version} can\u{2019}t check for updates on this machine"),
            Some(if remedy.is_empty() {
                UPDATE_LOG_HINT.to_string()
            } else {
                remedy.to_string()
            }),
        );
    }
    let class = if st.failing_checks_kind.is_empty() {
        st.failing_kind.as_str()
    } else {
        st.failing_checks_kind.as_str()
    };
    if st.failing_persistent {
        // The one failure a person has to act on: a build that predates the channel's
        // keys, whose ledger sentence prescribes the reinstall.
        if class == "manifest" && st.outcome.contains("reinstall") {
            return (
                format!("aterm {version} is too old to check for updates"),
                Some(
                    "Reinstall aterm from the current release (drag it from the release DMG)."
                        .to_string(),
                ),
            );
        }
        let trouble = if class == "apply" {
            // An install streak whose build is gone: not a check failure.
            "the last updates didn\u{2019}t install".to_string()
        } else {
            match check_trouble_words(class) {
                Some(words) => format!("update checks keep failing: {words}"),
                None => "update checks keep failing".to_string(),
            }
        };
        return (
            format!("aterm {version} \u{b7} {trouble}; aterm keeps trying"),
            Some(UPDATE_LOG_HINT.to_string()),
        );
    }
    if st.failing_checks > 0 {
        let why = check_trouble_words(class)
            .map(|words| format!(": {words}"))
            .unwrap_or_default();
        return (
            format!(
                "aterm {version} \u{b7} the last check didn\u{2019}t finish{why}; aterm will try \
                 again"
            ),
            Some(UPDATE_LOG_HINT.to_string()),
        );
    }
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at);
    let mut line = if st.linux.as_ref().is_some_and(|native| native.refused_newer) {
        // A Linux copy that rolled a newer build back runs an older one on purpose;
        // `-v`'s decision line names the build. "Up to date" would hide it. It still
        // checks, and installs any release newer than the refused one: say when.
        let refused = format!(
            "aterm {version} \u{b7} a newer release was rolled back on this copy and won\u{2019}t \
             install again"
        );
        match checked {
            Some(checked) => format!("{refused} \u{b7} checked {}", ago_words(checked, now)),
            None => refused,
        }
    } else if let Some(checked) = checked {
        format!(
            "aterm {version} is up to date \u{b7} checked {}",
            ago_words(checked, now)
        )
    } else {
        // No completed check yet: "up to date" would be a claim nothing has tested.
        format!("aterm {version} hasn\u{2019}t checked for updates yet")
    };
    if !automatic_checks {
        line.push_str(" \u{b7} automatic checks are off (Settings \u{25b8} Software Update)");
    }
    (line, None)
}

/// A failing check CLASS (the updater's health-ledger names) in a person's words;
/// `None` for a class that names no cause (Linux records every failure as one class).
fn check_trouble_words(kind: &str) -> Option<&'static str> {
    match kind {
        "network" => Some("the update server can\u{2019}t be reached"),
        "manifest" => Some("the newest release couldn\u{2019}t be verified"),
        "pipeline" => Some("downloads aren\u{2019}t finishing"),
        "stage" => Some("a download couldn\u{2019}t be prepared"),
        _ => None,
    }
}

/// The launches a pending Linux install has spent of its budget, as the -v line's
/// tail; nothing before the first.
fn trial_launches_words(starts: u32) -> String {
    if starts == 0 {
        String::new()
    } else {
        format!(
            " ({starts} of {} launches used before it rolls back)",
            aterm_update::LINUX_TRIAL_LAUNCHES
        )
    }
}

/// Whether a replaced Linux executable still waits for its first window launch
/// (`aterm_update::linux::confirm`), during which checks and applies wait too.
fn linux_install_waits_for_window(st: &aterm_update::UpdateStatus) -> bool {
    st.linux.as_ref().is_some_and(|native| {
        native.trial_phase.as_deref() == Some("Installed") && !native.trial_healthy
    })
}

/// How long before `now` the instant `at` was: `just now`, `12 min ago`, `3 h ago`,
/// `2 days ago` (Unix seconds both; a time ahead of `now` is `just now`).
fn ago_words(at: i64, now: i64) -> String {
    let age = now.saturating_sub(at);
    if age < 60 {
        "just now".to_string()
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86_400 {
        format!("{} h ago", age / 3600)
    } else if age < 2 * 86_400 {
        "1 day ago".to_string()
    } else {
        format!("{} days ago", age / 86_400)
    }
}

/// `aterm update … -v`: the ledger's own detail under the plain line — the updater's
/// last decision, when the last check completed, the failure counters when they carry
/// news, and the apply lane's own words (2026-09-14, audit OBS-7: the reason, not just
/// the count).
fn print_update_detail(build: u64, st: &aterm_update::UpdateStatus) {
    println!("  last decision: {}", st.summary());
    // The running and installed builds are one file from this CLI; the ledger's
    // decision above already says when the installed one is newer.
    if let Some(native) = &st.linux {
        // The plain line above already names the verb that installs it.
        if let Some(staged) = native.staged_build {
            match &native.staged_version {
                Some(version) => println!("  staged: {version} (build {staged})"),
                None => println!("  staged: build {staged}"),
            }
        }
        if linux_install_waits_for_window(st) {
            println!(
                "  waiting for an aterm window to launch{}",
                trial_launches_words(native.trial_starts)
            );
        }
    }
    if !st.updated_at.is_empty() {
        println!("  last completed check: {}", st.updated_at);
    }
    if st.failing_checks > 0 {
        let kind = if st.failing_checks_kind.is_empty() {
            st.failing_kind.as_str()
        } else {
            st.failing_checks_kind.as_str()
        };
        println!(
            "  failing checks: {} consecutive ({kind})",
            st.failing_checks
        );
    }
    if st.failing_applies > 0 {
        println!(
            "  failing applies: {} — a verified build is staged but will not start",
            st.failing_applies
        );
    }
    if let Some(report) = aterm_update::apply_lane_report(build) {
        if !report.last_failure.is_empty() {
            let target = if report.last_failure_target_build > 0 {
                format!(" (build {})", report.last_failure_target_build)
            } else {
                String::new()
            };
            println!("  last apply failure{target}: {}", report.last_failure);
        }
        if !report.last_refusal.is_empty() {
            let when = if report.last_refusal_at.is_empty() {
                String::new()
            } else {
                format!(" at {}", report.last_refusal_at)
            };
            println!("  apply lane{when}: {}", report.last_refusal);
        }
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
//
// A live build whose own-name shim is LOST resolves too (`atpkg::cli::live_without_shim`):
// `pkg run` execs it from the store and names the repair. Before, `aterm ty` over a lost
// `bin/ty` fell through to "aterm-gui: unknown option 'ty'" (measured on m3, 2026-09-23).
// Its cost for a word that is no tool at all — the common case here — is one more
// `stat` (no `store/<word>` directory ends it); only a program the store holds reads
// further.
fn store_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured().is_some_and(|layout| {
        atpkg::which(&layout, tool).is_some()
            || atpkg::cli::live_without_shim(&layout, tool).is_some()
    })
}

/// Whether the managed store holds a PENDING stub for `tool` — the front door's
/// second arm, checked only after [`store_resolves`] says no (a real shim always
/// outranks a stub). Same configured layout, same best-effort posture; the check is
/// one bounded read of `bin/<tool>` gated on the stub's marker line.
fn pending_stub_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured()
        .is_some_and(|layout| atpkg::stub::pending_stub_exists(&layout, tool))
}

/// TTY probe for the mode fork. std's `IsTerminal` on stdin: a Finder/.app
/// launch, a pipe, and CI all report false → window; an interactive shell
/// reports true → session.
fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
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
    (
        "--containment",
        "containment mode (master|user|safety|containment)",
    ),
    ("--sandbox", "shorthand for --containment containment"),
    ("--no-sandbox", "shorthand for --containment user"),
    (
        "--no-reroute",
        "restore the upstream Rust names in this session (see aterm help reroute)",
    ),
    ("--quiet", "suppress the interactive startup notice"),
    ("--help", "print help and exit"),
    ("--version", "print the version and exit"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn update_status() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            linux: None,
            enabled: true,
            installable: true,
            current_build: 100,
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            staged_dmg_sha256: None,
            changelog: None,
            outcome: "up to date (latest release build 100) \u{b7} checks every 30 min".into(),
            updated_at: "2026-09-23T12:00:00Z".into(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            failing_since: String::new(),
            failing_persistent: false,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
    }

    /// A `split-pane` that opens a new window says the reason that is TRUE for the route
    /// it took: under `new_window` nothing was probed, so it names the setting, never "no
    /// running aterm"; under `attach` with no answer it says there is none; after a failed
    /// forward it adds nothing, since the forward already said "opening a new window".
    #[test]
    fn split_pane_says_why_it_opens_a_window_once() {
        let policy = split_pane_spawn_line(SpawnHere::Policy).expect("says the setting");
        assert!(
            policy.contains("windowing_behavior is new_window"),
            "{policy}"
        );
        assert!(policy.contains("\"attach\""), "{policy}");
        assert!(!policy.contains("no running aterm"), "{policy}");
        assert_eq!(
            split_pane_spawn_line(SpawnHere::NoInstance),
            Some("aterm: no running aterm to split; opening a new window")
        );
        assert_eq!(split_pane_spawn_line(SpawnHere::Unreachable), None);
    }

    /// Windows has no updater, so `aterm update status` AND `check` land on this line
    /// (audit 2026-09-22: 0.90.0 answered with the macOS sentences, 0.94.0 with "on
    /// this platform"; neither named a way to update). It says the state, then the one
    /// lane that updates a Windows copy — the same words `aterm help update` prints —
    /// and never an app bundle Windows does not have.
    #[cfg(windows)]
    #[test]
    fn windows_update_line_names_the_lane_that_updates_it() {
        let line = update_unreadable_line("0.94.0");
        assert_eq!(
            line,
            format!(
                "aterm 0.94.0 doesn\u{2019}t update itself on Windows yet \u{2014} to update \
                 it, run {} in your aterm checkout",
                aterm_cli::WINDOWS_UPDATE_LANE
            )
        );
        assert!(!line.contains("aterm.app"), "{line}");
        assert!(!line.contains("macOS"), "{line}");
    }

    /// `aterm update status|check` SAYS ONE PLAIN LINE (2026-09-23 audit): the version a
    /// person knows and what is true, relative — never the updater's decision sentence,
    /// whose lane and token jargon ("checking over the unmetered web lane … every
    /// update-token rung") the verb used to print verbatim, and never a build number.
    /// Trouble adds one stderr line saying where the rest is.
    #[test]
    fn the_update_verb_says_one_plain_line() {
        let checked =
            aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
        let now = checked + 12 * 60;
        let say = |st: &aterm_update::UpdateStatus, auto_apply: bool, automatic: bool| {
            update_summary("0.91.0", 100, st, now, auto_apply, automatic, &DevCopy::No)
        };
        let healthy = update_status();
        assert_eq!(
            say(&healthy, true, true),
            (
                "aterm 0.91.0 is up to date \u{b7} checked 12 min ago".to_string(),
                None
            )
        );
        let (line, _) = say(&healthy, true, false);
        assert!(
            line.ends_with("automatic checks are off (Settings \u{25b8} Software Update)"),
            "{line}"
        );

        let mut staged = update_status();
        staged.staged_build = Some(101);
        staged.staged_version = Some("0.92.0".into());
        assert_eq!(
            say(&staged, true, true).0,
            "aterm 0.92.0 is downloaded and installs within a minute while an aterm window \
             is open"
        );
        assert!(
            say(&staged, false, true)
                .0
                .contains("Settings \u{25b8} Software Update")
        );
        staged.failing_applies = 2;
        let (line, trouble) = say(&staged, true, true);
        assert_eq!(
            line,
            "aterm 0.92.0 is downloaded but didn\u{2019}t install (2 tries)"
        );
        assert!(trouble.is_some_and(|t| t.contains("Settings \u{25b8} Messages")));

        let mut failing = update_status();
        failing.failing_checks = 1;
        failing.failing_checks_kind = "network".into();
        failing.outcome = "update check deferred: curl exit 6 (attempt 1)".into();
        let (line, trouble) = say(&failing, true, true);
        assert_eq!(
            line,
            "aterm 0.91.0 \u{b7} the last check didn\u{2019}t finish: the update server \
             can\u{2019}t be reached; aterm will try again"
        );
        assert!(trouble.is_some_and(|t| t.contains("aterm.log")));

        // A persistent streak: in words, and the one a person must act on says so.
        failing.failing_persistent = true;
        failing.failing_checks = 3;
        let (line, trouble) = say(&failing, true, true);
        assert_eq!(
            line,
            "aterm 0.91.0 \u{b7} update checks keep failing: the update server can\u{2019}t \
             be reached; aterm keeps trying"
        );
        assert!(trouble.is_some_and(|t| t.contains("Settings \u{25b8} Messages")));
        let mut stale = failing.clone();
        stale.failing_checks_kind = "manifest".into();
        stale.outcome = "FAILING (3 consecutive checks since 2026-09-23T10:00:00Z): this \
                         build's trust anchor cannot verify the channel's releases \u{2014} \
                         reinstall aterm from the current release"
            .into();
        let (line, trouble) = say(&stale, true, true);
        assert_eq!(line, "aterm 0.91.0 is too old to check for updates");
        assert!(trouble.is_some_and(|t| t.starts_with("Reinstall aterm")));
        failing.failing_persistent = false;
        failing.failing_checks = 1;

        for (line, _) in [say(&healthy, true, true), say(&failing, true, true)] {
            for jargon in ["lane", "rung", "token", "build 100", "curl"] {
                assert!(!line.contains(jargon), "{jargon}: {line}");
            }
        }
        // Never checked: not "up to date", which no check has established.
        let mut never = update_status();
        never.updated_at = String::new();
        assert_eq!(
            say(&never, true, true).0,
            "aterm 0.91.0 hasn\u{2019}t checked for updates yet"
        );
        assert_eq!(
            say(&never, true, false).0,
            "aterm 0.91.0 hasn\u{2019}t checked for updates yet \u{b7} automatic checks are \
             off (Settings \u{25b8} Software Update)"
        );

        // A copy the updater does not replace: a dev-marked bundle is left alone on
        // purpose and is told so (it may well sit in /Applications — "move it" would be
        // wrong); a `target/` binary, a disk-image or quarantined launch get the remedy.
        let mut inert = update_status();
        inert.installable = false;
        assert_eq!(
            say(&inert, true, true).0,
            "This copy of aterm 0.91.0 can\u{2019}t update itself \u{2014} only aterm.app \
             installed in Applications does"
        );
        let unread = DevCopy::Marked {
            lag: None,
            shared_writes: None,
        };
        let (dev, trouble) = update_summary("0.91.0", 100, &inert, now, true, true, &unread);
        assert_eq!(
            dev,
            "This copy of aterm 0.91.0 is a dev build \u{2014} the updater leaves it alone"
        );
        assert!(trouble.is_none());
        assert!(!dev.contains("Applications"));
        // …and, where the channel was read (gap #30), how far behind the newest release
        // it is — or that it is at it, or newer: the words are the dev channel's own.
        let at = |lag: aterm_update::dev_channel::DevLag,
                  shared_writes: Option<aterm_gui::DevSharedWrites>| {
            update_summary(
                "0.91.0",
                100,
                &inert,
                now,
                true,
                true,
                &DevCopy::Marked {
                    lag: Some(lag),
                    shared_writes,
                },
            )
        };
        use aterm_update::dev_channel::DevLag;
        let behind = DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(2),
        };
        assert_eq!(
            at(behind.clone(), None),
            (
                "This copy of aterm 0.91.0 is a dev build, 2 releases behind aterm v0.93.0 \
                 \u{2014} the updater leaves it alone"
                    .to_string(),
                None
            )
        );
        assert_eq!(
            at(
                DevLag::Current {
                    latest: "v0.91.0".into()
                },
                None
            )
            .0,
            "This copy of aterm 0.91.0 is a dev build, at aterm v0.91.0, the newest release \
             \u{2014} the updater leaves it alone"
        );
        assert_eq!(
            at(
                DevLag::Ahead {
                    latest: "v0.90.0".into()
                },
                None
            )
            .0,
            "This copy of aterm 0.91.0 is a dev build, newer than aterm v0.90.0, the newest \
             release \u{2014} the updater leaves it alone"
        );
        // A dev build named aterm.app runs the release's unattended writers: stderr says
        // what it writes and what puts it back — for the primer `aterm agents install`,
        // since `aterm pkg repair` never touches it.
        let primer = aterm_gui::DevSharedWrites {
            what: "the agent primer",
            repair: "`aterm agents install`",
        };
        let (line, trouble) = at(behind, Some(primer));
        assert!(line.contains("2 releases behind"), "{line}");
        let trouble = trouble.expect("what it writes is said");
        assert!(
            trouble.starts_with("This dev build writes the agent primer, which only the release"),
            "{trouble}"
        );
        assert!(trouble.contains("tools/dev-app.sh"), "{trouble}");
        assert!(
            trouble.ends_with("then run `aterm agents install` from the release."),
            "{trouble}"
        );
        // …and says it where the channel was NOT read (checks off, offline) too: the
        // writes are this copy's own fact.
        let (line, trouble) = update_summary(
            "0.91.0",
            100,
            &inert,
            now,
            true,
            false,
            &DevCopy::Marked {
                lag: None,
                shared_writes: Some(primer),
            },
        );
        assert_eq!(
            line,
            "This copy of aterm 0.91.0 is a dev build \u{2014} the updater leaves it alone"
        );
        assert!(
            trouble.is_some_and(|t| t.starts_with("This dev build writes the agent primer")),
            "the writes are said with no standing"
        );
        // A copy that is not dev-marked is never told a standing, whatever it is.
        assert_eq!(
            update_summary("0.91.0", 100, &inert, now, true, true, &DevCopy::No).0,
            say(&inert, true, true).0
        );

        // LINUX (main's native delivery, merged 2026-09-23): a copy that is not
        // enrolled says so plainly and hands on the ledger's remedy, and a staged
        // Linux build names the verb that installs it — never "only on macOS".
        let mut unenrolled = update_status();
        unenrolled.enabled = false;
        unenrolled.outcome = "Run `aterm update enable` to turn on updates for this copy".into();
        let (line, trouble) = say(&unenrolled, true, true);
        assert_eq!(
            line,
            "This copy of aterm 0.91.0 doesn\u{2019}t update itself"
        );
        assert!(trouble.is_some_and(|t| t.contains("aterm update enable")));
        // A replaced executable waits for its first window, and its checks with it:
        // never "up to date" over a check that read no channel.
        let mut waiting = update_status();
        waiting.linux = Some(aterm_update::LinuxUpdateStatus {
            installed_build: 100,
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            trial_phase: Some("Installed".into()),
            trial_starts: 0,
            trial_healthy: false,
            refused_newer: false,
        });
        assert_eq!(
            say(&waiting, true, true),
            (
                "aterm 0.91.0 is installed \u{2014} launch an aterm window once to finish; \
                 update checks wait until then"
                    .to_string(),
                None
            )
        );
        // -v: nothing about the budget right after the install, then how much is used.
        assert_eq!(trial_launches_words(0), "");
        assert_eq!(
            trial_launches_words(1),
            " (1 of 3 launches used before it rolls back)"
        );
        waiting.linux.as_mut().expect("linux").trial_healthy = true;
        assert!(say(&waiting, true, true).0.contains("is up to date"));
        // A copy that rolled a newer build back runs an older one on purpose: never
        // "up to date" over a ledger whose `-v` line says the newer one was refused.
        let mut refused = waiting.clone();
        let native = refused.linux.as_mut().expect("linux");
        native.trial_phase = None;
        native.refused_newer = true;
        // It still checks (a release newer than the refused one installs): say when.
        assert_eq!(
            say(&refused, true, true),
            (
                "aterm 0.91.0 \u{b7} a newer release was rolled back on this copy and \
                 won\u{2019}t install again \u{b7} checked 12 min ago"
                    .to_string(),
                None
            )
        );
        assert!(say(&refused, true, false).0.ends_with(
            "won\u{2019}t install again \u{b7} checked 12 min ago \u{b7} automatic checks \
                 are off (Settings \u{25b8} Software Update)"
        ));
        // No completed check on record: the rolled-back fact alone, no time.
        refused.updated_at = String::new();
        assert_eq!(
            say(&refused, true, true).0,
            "aterm 0.91.0 \u{b7} a newer release was rolled back on this copy and \
             won\u{2019}t install again"
        );
        // A Linux failure carries one class that names no cause: no tautology after it.
        let mut linux_failing = update_status();
        linux_failing.failing_checks = 1;
        linux_failing.failing_checks_kind = "linux-update".into();
        assert_eq!(
            say(&linux_failing, true, true).0,
            "aterm 0.91.0 \u{b7} the last check didn\u{2019}t finish; aterm will try again"
        );
        linux_failing.failing_persistent = true;
        assert_eq!(
            say(&linux_failing, true, true).0,
            "aterm 0.91.0 \u{b7} update checks keep failing; aterm keeps trying"
        );
        let mut native = update_status();
        native.linux = Some(aterm_update::LinuxUpdateStatus {
            installed_build: 100,
            staged_build: Some(101),
            staged_version: Some("0.92.0".into()),
            staged_commit: None,
            trial_phase: None,
            trial_starts: 0,
            trial_healthy: false,
            refused_newer: false,
        });
        assert_eq!(
            say(&native, true, true),
            (
                "aterm 0.92.0 is downloaded \u{2014} `aterm update apply` installs it".to_string(),
                None
            )
        );
    }

    /// THE HOST SETTINGS RUN FROM AN INTERACTIVE SESSION LAUNCH — once a day, on their own
    /// stamp — not only when a package pass happens to be due.
    ///
    /// This lane exists because only the WINDOW entry ran the updater; the same gap
    /// applied to the `[machine]` settings, which are not the package manager's to gate:
    /// they take no store lock, need no index and no network, and a Mac with
    /// `[packages] enabled = false` (then also `auto_update = false`, or an environment
    /// kill switch, both gone since 2026-09-23, or simply a pass that ran an hour ago)
    /// got them from this lane never. Since Phase 3
    /// (2026-09-22) the gate is the daily claim ([`atpkg::machine::launch_apply_due`]),
    /// not every launch. A scrape, in the idiom of atpkg's own placement test, because the
    /// alternative is spawning a real detached child in a unit test.
    #[test]
    fn the_session_lane_applies_the_machine_settings_outside_every_package_gate() {
        let src = include_str!("main.rs");
        let start = src
            .find("\nfn session_entry")
            .or_else(|| src.find("spawn_detached_machine_apply();"))
            .expect("the session lane");
        let call = src
            .find("spawn_detached_machine_apply();")
            .expect("the session lane applies the [machine] settings");
        let gate = src
            .find("if let Some(layout) = layout.as_ref()\n        && packages.enabled()")
            .expect("the package-pass gate");
        assert!(
            call < gate,
            "the host settings must not sit inside the package manager's gate"
        );
        assert!(start <= call);
        let daily = src[..call]
            .rfind("&& atpkg::machine::launch_apply_due(layout, now)")
            .expect("the one-shot is gated on its daily claim");
        assert!(
            !src[daily..call].contains("packages.enabled()"),
            "the daily claim is the only gate between it and the call"
        );
        // And the argv is the lock-free verb, not a pass.
        let spawner = src
            .find("fn spawn_detached_machine_apply()")
            .expect("the spawner");
        let body = &src[spawner..spawner + 600];
        assert!(
            body.contains(r#"["pkg", "machine", "apply"]"#),
            "the lane runs `aterm pkg machine apply`: {body}"
        );
    }

    /// THE SESSION LANE STARTS THE VENDOR HEAD WATCH (gap #28) behind the pass's own two
    /// gates — `[packages] enabled` and an interactive launch, so a harness driving a
    /// session over pipes never reaches a vendor — and runs its passes as this binary's
    /// `pkg` verb. The watch itself (the seat, the window's precedence, the detached pass)
    /// is atpkg's and tested there; this pins only that the session starts it. A scrape,
    /// in this module's idiom: the alternative is a real session reaching the network.
    #[test]
    fn the_session_lane_starts_the_head_watch_behind_the_pass_gates() {
        let src = include_str!("main.rs");
        let call = src
            .find("start_session_head_watch(layout, packages);")
            .expect("the session lane starts the head watch");
        let gate = src[..call]
            .rfind("if let Some(layout) = layout.as_ref()")
            .expect("its gate");
        let guard = &src[gate..call];
        assert!(guard.contains("&& packages.enabled()"), "{guard}");
        assert!(
            guard.contains("&& session_lane_is_interactive()"),
            "{guard}"
        );
        assert!(
            src[call..].contains("session_lane(quiet)"),
            "started before the session takes the process"
        );
        let body = &src[src
            .find("fn start_session_head_watch(")
            .expect("the starter")..];
        let body = &body[..body.find("\n}\n").expect("its end")];
        assert!(
            body.contains(r#"vec![std::ffi::OsString::from("pkg")]"#),
            "{body}"
        );
        assert!(
            body.contains("atpkg::vendor_direct::watch::run_host("),
            "{body}"
        );
    }

    /// A scratch store for the lane's due rules.
    fn scratch_layout(label: &str) -> atpkg::store::Layout {
        let prefix = std::env::temp_dir().join(format!(
            "aterm-session-lane-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).expect("scratch prefix");
        atpkg::store::Layout { prefix }
    }

    /// THE SESSION LANE'S DUE RULE (Phase 3): the machine-wide rule — never checked or six
    /// hours since the last success, and no pass ended anywhere in the last five minutes —
    /// then the lane's own claim, so the second of two tabs opened together spawns nothing.
    /// A fresh success, or a sibling's pass that failed a minute ago, is no pass; a vendor
    /// door's later write (`updated_at` alone) is no pass either. (A record that shows only
    /// FAILED passes is the next test's.)
    #[test]
    fn the_session_pass_is_due_on_the_machine_rule_and_claimed_once() {
        let layout = scratch_layout("pass");
        let now = 1_790_000_000_i64;
        assert!(
            session_pass_due(&layout, now),
            "never checked: the first tab"
        );
        assert!(
            !session_pass_due(&layout, now + 1),
            "the second tab, a second later: claimed"
        );
        assert!(
            session_pass_due(&layout, now + 5 * 60),
            "still never checked, five minutes on: the next tab tries again"
        );
        // A success an hour ago: nothing owed, and nothing claimed.
        std::fs::write(
            layout.status(),
            "schema = 1\nupdated_at = \"2026-09-21T13:00:00Z\"\n\
             last_success_at = \"2026-09-21T13:00:00Z\"\n",
        )
        .unwrap();
        let success =
            aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-21T13:00:00Z").expect("stamp");
        assert!(!session_pass_due(&layout, success + 3600));
        let walk = i64::try_from(aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS).unwrap();
        assert!(!session_pass_due(&layout, success + walk - 1));
        assert!(session_pass_due(&layout, success + walk), "six hours on");
        // A vendor door wrote a row an hour after that success: no pass, nothing held back.
        std::fs::write(
            layout.status(),
            "schema = 1\nupdated_at = \"2026-09-21T14:00:00Z\"\n\
             last_success_at = \"2026-09-21T13:00:00Z\"\n",
        )
        .unwrap();
        let claim = i64::try_from(aterm_update_core::pkg_check::PASS_SPACING_SECS).unwrap();
        assert!(
            session_pass_due(&layout, success + walk + claim),
            "six hours on, past the last claim: owed"
        );
        // A sibling's pass ended a minute ago and recorded a failure: the walk waits for what
        // it left, and then the interval after it, not five minutes (the next test).
        std::fs::write(
            layout.status(),
            "schema = 1\nupdated_at = \"2026-09-22T13:30:00Z\"\n\
             last_success_at = \"2026-09-21T13:00:00Z\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-22T13:30:00Z\"\n",
        )
        .unwrap();
        let attempt =
            aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-22T13:30:00Z").expect("stamp");
        assert!(!session_pass_due(&layout, attempt + 60));
        assert!(!session_pass_due(&layout, attempt + 5 * 60));
        assert!(
            session_pass_due(&layout, attempt + walk),
            "the negative control: six hours after it, it is owed again"
        );
        // THE MISREAD, FIXED (2026-09-23): the same `updated_at` from a vendor head-watch
        // write, the last pass the success itself — no failed pass, and no pass a minute
        // ago: the walk is owed at once. The lane read "written after the last success" as
        // a failed pass and held the tab's pass six hours from the write.
        std::fs::write(
            layout.status(),
            "schema = 1\nupdated_at = \"2026-09-22T13:30:00Z\"\n\
             last_success_at = \"2026-09-21T13:00:00Z\"\nlast_pass = \"ok\"\n\
             last_pass_at = \"2026-09-21T13:00:00Z\"\n",
        )
        .unwrap();
        assert!(session_pass_due(&layout, attempt + 5 * 60));
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A PASS THAT KEEPS FAILING IS NOT RESPAWNED BY EVERY TAB (the 2026-09-10 rule, kept
    /// through Phase 3): a record whose passes never reach the index — a proxy that refuses
    /// the download host stamps a failure each time — owes nothing for the interval after
    /// its last failure; the spacing alone would have let a tab opened five minutes on spawn
    /// another detached pass. And a pass INSTALLING now is not queued behind: no unobserved
    /// waiter per tab.
    #[test]
    fn a_failing_or_running_pass_is_not_respawned_by_every_tab() {
        let layout = scratch_layout("failing");
        let attempt =
            aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-22T13:00:00Z").expect("stamp");
        std::fs::write(
            layout.status(),
            "schema = 1\nupdated_at = \"2026-09-22T13:00:00Z\"\n\
             outcome = \"update failed: index unreachable\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-22T13:00:00Z\"\n",
        )
        .unwrap();
        for minutes in [5, 10, 60, 120, 359] {
            assert!(
                !session_pass_due(&layout, attempt + minutes * 60),
                "a tab {minutes} min after the failed pass"
            );
        }
        assert!(
            session_pass_due(&layout, attempt + 6 * 3600),
            "the negative control: six hours on, one tab tries again"
        );
        // A live writer in the progress file: a pass is installing, so no tab spawns.
        let fresh = scratch_layout("running");
        let now = attempt + 7 * 3600;
        std::fs::write(
            fresh.progress_file(),
            format!(
                "{{\"v\":{},\"pid\":{},\"heartbeat_unix\":{now}}}",
                atpkg::progress::PROGRESS_VERSION,
                std::process::id()
            ),
        )
        .unwrap();
        assert!(
            !session_pass_due(&fresh, now),
            "never checked, but installing"
        );
        std::fs::remove_file(fresh.progress_file()).unwrap();
        assert!(
            session_pass_due(&fresh, now),
            "the negative control: nothing running"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&fresh.prefix);
    }

    /// The detached pass runs on the window's argv — the lock wait and the progress file
    /// under the prefix — so a contended pass queues, and a window follows its progress;
    /// and nobody typed it, so a busy toolchain's flip waits for quiet (`--defer-busy-flip`).
    #[test]
    fn the_detached_pass_runs_on_the_windows_argv() {
        let layout = atpkg::store::Layout {
            prefix: std::path::PathBuf::from("/p"),
        };
        let words: Vec<String> = session_pass_args(&layout)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The progress file is `layout.progress_file()`, a native join: `/p/…` on
        // Unix and `/p\…` on Windows, so the expected spelling is per host.
        let progress = if cfg!(windows) {
            r"/p\progress.json"
        } else {
            "/p/progress.json"
        };
        assert_eq!(
            words,
            [
                "pkg",
                "update",
                "--defer-busy-flip",
                "--wait-lock",
                "1800",
                "--progress-file",
                progress
            ]
        );
    }

    /// THE DETACHED PASS IS NOT THIS PROCESS'S CHILD (2026-09-12). The session lane
    /// runs `aterm_cli::session_main` in this same process, and its unix driver
    /// reaps the shell with `waitpid(-1)`: a finished `aterm pkg update` left as
    /// our zombie could be reaped in the shell's place, and `aterm` then exited
    /// with the pkg pass's status instead of the shell's. So the pass must be
    /// re-parented away from us — and still sit in its own process group, so the
    /// session's SIGHUP cannot take it down mid-install. And it is told it was detached
    /// on purpose (Phase 3), so its lock wait does not read the `sh` exiting as its
    /// window going.
    #[test]
    #[cfg(unix)]
    fn a_detached_pass_is_neither_our_child_nor_in_our_process_group() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-detached-pass-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let pid_file = dir.join("pid");
        let env_file = dir.join("env");
        let script = format!(
            "echo \"$ATPKG_SPAWNER_PID\" > '{1}' && echo $$ > '{0}.tmp' && mv '{0}.tmp' '{0}' \
             && exec sleep 30",
            pid_file.display(),
            env_file.display()
        );
        let args = [std::ffi::OsString::from("-c"), script.into()];
        spawn_detached(
            std::ffi::OsStr::new("/bin/sh"),
            &args,
            Some(("ATPKG_SPAWNER_PID", atpkg::cli::SPAWNER_DETACHED)),
        )
        .expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pid: libc::pid_t = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                break text.trim().parse().expect("a pid");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pass never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let errno = std::io::Error::last_os_error().raw_os_error();
        let pgid = unsafe { libc::getpgid(pid) };
        let ours = unsafe { libc::getpgid(0) };
        unsafe { libc::kill(pid, libc::SIGKILL) };
        if reaped == 0 {
            unsafe { libc::waitpid(pid, &mut status, 0) };
        }
        let env = std::fs::read_to_string(&env_file).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            env.trim(),
            atpkg::cli::SPAWNER_DETACHED,
            "the pass is told it was detached on purpose"
        );
        assert_eq!(
            (reaped, errno),
            (-1, Some(libc::ECHILD)),
            "the detached pass is still this process's child"
        );
        assert_ne!(
            pgid, ours,
            "the detached pass shares the session's process group"
        );
    }

    /// The reroute markers (`__ATERM_REROUTE_PASSTHROUGH`, which `take_no_reroute` sets
    /// and clears, and `ATERM_AGENTS_DIR`) are process-global and several tests here
    /// read or set them; they take this lock so a parallel test never sees the other's
    /// value.
    static NO_REROUTE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `-d` must reach the running instance as an ABSOLUTE native path: the
    /// process that serves the request has its own working directory, so a
    /// relative operand forwarded verbatim would open somewhere else entirely.
    ///
    /// The relative leg deliberately uses `.` against the process's real cwd
    /// (which `cargo test` sets to the crate dir) rather than mutating the
    /// environment — `set_current_dir` is process-global and this suite runs
    /// threaded.
    #[test]
    fn a_directory_operand_resolves_to_an_absolute_native_path() {
        let cwd = std::env::current_dir().expect("a working directory");
        let here = resolve_dir_absolute(".").expect("`.` is a directory");
        assert_eq!(std::path::Path::new(&here), cwd.as_path());
        assert!(std::path::Path::new(&here).is_absolute());

        let absolute = resolve_dir_absolute(&cwd.to_string_lossy()).expect("an absolute directory");
        assert_eq!(std::path::Path::new(&absolute), cwd.as_path());

        // NOT the extended-length `\\?\C:\…` form: that is what `canonicalize`
        // would hand back, and it is a poor thing to give a shell as its cwd.
        assert!(!absolute.starts_with(r"\\?\"), "{absolute}");
    }

    /// A `-d` that is not a directory fails BEFORE any socket is dialed, with
    /// the same wording the window library's own `-d` uses — one message, one
    /// meaning, whichever route the launch was going to take.
    #[test]
    fn a_directory_operand_that_is_not_a_directory_is_refused() {
        let not_a_dir = std::path::Path::new(file!()).to_string_lossy().into_owned();
        let err = resolve_dir_absolute(&not_a_dir).expect_err("a source file is not a directory");
        assert!(err.starts_with("not a directory:"), "{err}");
        let missing = resolve_dir_absolute("definitely-not-here-9f3a").expect_err("missing");
        assert!(missing.starts_with("not a directory:"), "{missing}");
    }

    /// The plain-launch scan finds `-d` in the spellings the WINDOW parses, and
    /// reports an unusable one as `Err` so the caller declines to route by
    /// policy and lets the ordinary window path print the error.
    #[test]
    fn a_plain_launch_carries_its_directory_or_declines_to_route() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        assert_eq!(plain_launch_dir(&osv(&["--window"])), Ok(None));
        let cwd = std::env::current_dir().expect("a working directory");
        for spelling in [
            osv(&["--window", "-d", "."]),
            osv(&["--working-directory", "."]),
        ] {
            let found = plain_launch_dir(&spelling).expect("resolvable");
            assert_eq!(
                found.as_deref().map(std::path::Path::new),
                Some(cwd.as_path()),
                "{spelling:?}"
            );
        }
        assert_eq!(
            plain_launch_dir(&osv(&["-d", "definitely-not-here-9f3a"])),
            Err(())
        );
    }

    /// A CHILD COMMAND LINE IS NEVER ROUTED BY POLICY. `spawn` cannot carry one,
    /// so a forwarded `aterm -e vim` opens an empty tab, exits 0, and drops the
    /// command without a word.
    ///
    /// The trap, executable: the mode fork's own `scan` stops AT the payload
    /// boundary, so `-e vim` truncates to an EMPTY argument list — the barest,
    /// most obviously-eligible launch there is. Feeding the gate that slice makes
    /// its documented `-e` exclusion unreachable, which is why
    /// `plain_launch_request` takes the WHOLE argv and refuses the boundary
    /// itself.
    #[test]
    fn a_child_command_payload_is_never_routed_by_policy() {
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        aterm_log::env::unset(atpkg::reroute::PASSTHROUGH_ENV);
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let env = aterm_cli::LaunchEnv::default();
        for argv in [
            osv(&["-e", "vim"]),
            osv(&["--window", "-e", "vim"]),
            osv(&["--command", "vim"]),
            osv(&["--", "sh", "-c", "echo hi"]),
            osv(&["--window", "-d", ".", "-e", "vim"]),
        ] {
            assert_eq!(
                plain_launch_request(&argv, env),
                None,
                "{argv:?} carries a payload a forwarded tab cannot run"
            );
        }
        // The truncated slice IS eligible — that is the whole hazard.
        let dash_e = osv(&["-e", "vim"]);
        assert_eq!(payload_boundary(&dash_e), 0);
        assert!(
            plain_launch_request(&dash_e[..0], env).is_some(),
            "the pre-boundary scan of `-e vim` is an empty, maximally eligible argv — \
             the gate must never be handed it"
        );
        // …and a genuinely bare launch still routes.
        assert!(plain_launch_request(&osv(&[]), env).is_some());
        assert!(plain_launch_request(&osv(&["--window"]), env).is_some());
    }

    /// THE ALIAS TABLE. The one that matters is the `aterm-gui` row: on the
    /// shipped Windows install every sibling name is an identical copy of this
    /// binary, the Start-Menu shortcut targets `aterm-gui.exe`, and the taskbar
    /// jump list is committed by whichever copy is running — so
    /// `aterm-gui.exe new-window` is a command line the SHELL issues, from a
    /// launcher with no console to print an error to. It must route as a verb.
    #[test]
    fn the_gui_alias_routes_the_windowing_verbs_and_nothing_else() {
        for verb in aterm_cli::Verb::ALL {
            let expected = if verb.is_windowing() {
                AliasRoute::AliasWindowVerb
            } else {
                AliasRoute::AliasWindow
            };
            assert_eq!(
                alias_route("aterm-gui", verb.name()),
                expected,
                "aterm-gui {}",
                verb.name()
            );
        }
        // A bare launch and the window's own flags stay the window.
        for operand in ["", "--window", "-d", "--headless", "--diagnose"] {
            assert_eq!(
                alias_route("aterm-gui", operand),
                AliasRoute::AliasWindow,
                "aterm-gui {operand:?}"
            );
        }
        // The other aliases are untouched by the windowing grammar: `aterm-ctl
        // new-tab` is a ctl invocation, and ctl gets to say so itself.
        assert_eq!(alias_route("aterm-ctl", "new-tab"), AliasRoute::Ctl);
        assert_eq!(alias_route("atpkg", "new-tab"), AliasRoute::Pkg);
        assert_eq!(alias_route("aterm-fleet", ""), AliasRoute::Fleet);
        assert_eq!(alias_route("aterm-drive", ""), AliasRoute::Drive);
        // A bridge is spawned by name out of `[fabric] command`; before this
        // arm existed that name opened a window instead of serving.
        assert_eq!(alias_route("aterm-link", ""), AliasRoute::Link);
        assert_eq!(alias_route("aterm-link", "serve"), AliasRoute::Link);
        // And the front door is still the front door under every other name.
        for name in ["aterm", "aterm-cli", "my-renamed-aterm"] {
            assert_eq!(
                alias_route(name, "new-window"),
                AliasRoute::FrontDoor,
                "{name}"
            );
            assert_eq!(alias_route(name, ""), AliasRoute::FrontDoor, "{name}");
        }
    }

    /// `aterm-windowed` — the cargo target name of the WINDOWED image, which a
    /// dev tree runs under that name before `build.ps1` renames it to
    /// `aterm-gui.exe` — routes exactly as `aterm-gui` does, verb for verb: the
    /// jump list committed by a dev-tree window names that image.
    #[test]
    fn the_windowed_target_name_is_the_gui_alias() {
        for verb in aterm_cli::Verb::ALL {
            assert_eq!(
                alias_route("aterm-windowed", verb.name()),
                alias_route("aterm-gui", verb.name()),
                "aterm-windowed {}",
                verb.name()
            );
        }
        assert_eq!(
            alias_route("aterm-windowed", "new-window"),
            AliasRoute::AliasWindowVerb
        );
        for operand in ["", "--window", "-d", "--headless", "--diagnose"] {
            assert_eq!(
                alias_route("aterm-windowed", operand),
                AliasRoute::AliasWindow,
                "aterm-windowed {operand:?}"
            );
        }
    }

    /// THE TWO IMAGES ARE ONE BODY (Windows, 2026-09-22). The PE subsystem is a
    /// per-file header field, so the console image (this file) must carry NO
    /// `windows_subsystem` attribute, and the windowed image (`windowed.rs`)
    /// must carry it and include THIS file — not a copy of it — as its body.
    /// And every window the front door opens must go through the one call that
    /// knows which image is running (`run_window`): a second direct
    /// `aterm_gui::main_entry` call is a window that would run in the console
    /// process and hold the shell's prompt. A scrape, in the idiom of the
    /// session-lane tests above.
    #[test]
    fn the_windowed_image_is_this_file_plus_the_subsystem_attribute() {
        let console = include_str!("main.rs");
        let windowed = include_str!("windowed.rs");
        let code_lines = |src: &str| -> Vec<String> {
            src.lines()
                .map(str::trim_start)
                .filter(|l| !l.starts_with("//"))
                .map(str::to_owned)
                .collect()
        };
        assert!(
            !code_lines(console)
                .iter()
                .any(|l| l.starts_with("#![") && l.contains("windows_subsystem")),
            "the console image must not carry a subsystem attribute"
        );
        assert!(
            code_lines(windowed).iter().any(|l| {
                l.starts_with("#![cfg_attr(") && l.contains("windows_subsystem = \"windows\"")
            }),
            "the windowed image carries the GUI subsystem"
        );
        assert!(
            code_lines(windowed)
                .iter()
                .any(|l| l == "#[path = \"main.rs\"]"),
            "the windowed image shares this file's body"
        );
        // ... on Windows only: off Windows the include would make every release
        // build link the whole front door twice (review 2026-09-27).
        let windowed_code = code_lines(windowed);
        let include = windowed_code
            .iter()
            .position(|l| l == "#[path = \"main.rs\"]")
            .expect("the include line, asserted above");
        assert_eq!(
            windowed_code
                .get(include.wrapping_sub(1))
                .map(String::as_str),
            Some("#[cfg(all(windows, not(test)))]"),
            "the front door is included into the windowed image on Windows alone"
        );
        // The needle is assembled so THIS line does not count itself (the
        // first run of the test found 2: the call, and this literal).
        let entry_call = concat!("aterm_gui::", "main_entry(");
        assert_eq!(
            code_lines(console)
                .iter()
                .filter(|l| l.contains(entry_call))
                .count(),
            1,
            "every window route goes through `run_window`"
        );
    }

    /// THE HOST TABLE for a window-shaped front-door launch. On Windows the
    /// console image hands a real window to the windowed sibling, and keeps
    /// the WINDOW in-process only for the launches whose output a harness
    /// reads through its pipes: `--headless` (its one spelling) and
    /// `--diagnose`. A flag the window's parser answers by printing takes the
    /// sibling route too — whose first step is that parser, run in this
    /// process (`aterm_gui::check_window_args`), so it still prints here; the
    /// end-to-end proof is `tests/windows_console_image.rs`. Off Windows there
    /// is one image and one answer.
    #[test]
    fn a_console_window_launch_goes_to_the_sibling_unless_a_harness_reads_it() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let window = if cfg!(windows) {
            WindowHost::WindowedSibling
        } else {
            WindowHost::ThisProcess
        };
        assert_eq!(window_host_for(&osv(&[])), window, "the no-TTY launch");
        assert_eq!(window_host_for(&osv(&["--window"])), window);
        assert_eq!(
            window_host_for(&osv(&["--window", "-d", ".", "--hold"])),
            window
        );
        for flag in [
            "--help",
            "--version",
            "--list-fonts",
            "--install-context-menu",
        ] {
            assert_eq!(
                window_host_for(&osv(&["--window", flag])),
                window,
                "{flag} is answered by the parser run before the handoff"
            );
        }
        for flag in ["--headless", "--diagnose"] {
            assert_eq!(
                window_host_for(&osv(&["--window", flag])),
                WindowHost::ThisProcess,
                "{flag}: a harness reads this process's output"
            );
        }
    }

    /// A window the routing policy decided to SPAWN is handed to the windowed
    /// sibling as `new-window [-d <dir>]` — the verb the policy never
    /// redirects — so the sibling, the whole front door under the `aterm-gui`
    /// alias, cannot route it a second time. Handed a bare `[-d <dir>]` it
    /// took the plain-launch route, whose gate ACCEPTS that list, and under
    /// `attach` with an instance up `aterm new-window` opened a TAB (review
    /// 2026-09-27). Every intent, with and without a directory.
    #[test]
    fn a_spawn_is_handed_to_the_sibling_as_new_window_never_as_a_plain_launch() {
        for intent in [
            aterm_cli::LaunchIntent::NewTab,
            aterm_cli::LaunchIntent::NewWindow,
            aterm_cli::LaunchIntent::SplitPane,
            aterm_cli::LaunchIntent::Plain,
        ] {
            for dir in [None, Some(String::from(r"C:\work dir"))] {
                let request = aterm_cli::WindowRequest {
                    intent,
                    dir: dir.clone(),
                    split: aterm_cli::SplitOrientation::Horizontal,
                };
                let argv = spawn_handoff_argv(&request);
                let (verb, rest) = argv.split_first().expect("the verb leads");
                assert_eq!(verb.to_string_lossy(), "new-window", "{intent:?}");
                assert_eq!(
                    alias_route("aterm-gui", "new-window"),
                    AliasRoute::AliasWindowVerb
                );
                let reparsed =
                    aterm_cli::parse_window_request("new-window", rest, |raw| Ok(raw.to_owned()))
                        .expect("the sibling's grammar accepts the handoff");
                assert_eq!(reparsed.intent, aterm_cli::LaunchIntent::NewWindow);
                assert_eq!(reparsed.dir, dir, "{intent:?}: the directory travels");
                assert_eq!(
                    aterm_cli::route_launch(
                        reparsed.intent,
                        aterm_cli::WindowingBehavior::Attach,
                        true
                    ),
                    aterm_cli::WindowRoute::Spawn,
                    "attach with an instance up still spawns"
                );
                assert!(
                    !aterm_cli::plain_launch_is_policy_eligible(
                        &argv,
                        aterm_cli::LaunchEnv::default()
                    ),
                    "the handoff is never a plain launch the policy could forward"
                );
            }
        }
    }

    /// THE NESTED-SESSION TABLE (defect b of the 2026-09-22 audit): a BARE
    /// launch typed inside a tab opens a tab; the `aterm-cli` alias, a launch
    /// carrying anything at all (`--sandbox`, `--containment`, a `-e` payload,
    /// a typo — review 2026-09-27: those were silently dropped for a plain
    /// tab), a launch outside aterm, and a piped launch each keep the session
    /// they asked for.
    #[test]
    fn a_bare_launch_inside_a_tab_opens_a_tab_and_anything_else_nests() {
        assert!(nests_inside_aterm(true, false, true, true));
        assert!(
            !nests_inside_aterm(false, false, true, true),
            "flags, a -e payload or a typo go to the session parser"
        );
        assert!(
            !nests_inside_aterm(true, true, true, true),
            "aterm-cli nests on purpose"
        );
        assert!(
            !nests_inside_aterm(true, false, false, true),
            "not inside aterm at all"
        );
        assert!(
            !nests_inside_aterm(true, false, true, false),
            "a piped launch is a harness and gets what it asked for"
        );
    }

    /// WHICH windowed image the console image hands a window to, against a
    /// real folder: the shipped name in an install folder, only as a FILE; the
    /// cargo target name first; and in a build tree (cargo's `.fingerprint`
    /// beside the exe) never the `aterm-gui.exe` there, which is crate
    /// aterm-gui's thin dev bin — the window library with none of this front
    /// door. The rule itself is `aterm_gui`'s, shared with the jump list and
    /// the Explorer verb. And the detach flag is `DETACHED_PROCESS`, never
    /// `CREATE_NEW_PROCESS_GROUP` (0x200), which would start the window — and
    /// every shell under it — with Ctrl+C off.
    #[cfg(windows)]
    #[test]
    fn the_windowed_sibling_is_the_shipped_image_or_a_build_trees_own() {
        let dir = scratch_dir("windowed-sibling");
        let exe = dir.join("aterm.exe");
        let found = || aterm_gui::windowed_front_door_beside(&exe);
        assert_eq!(found(), None);
        std::fs::create_dir_all(dir.join("aterm-gui.exe")).expect("a decoy directory");
        assert_eq!(found(), None, "a directory of that name is not an image");
        std::fs::remove_dir(dir.join("aterm-gui.exe")).expect("drop the decoy");
        std::fs::write(dir.join("aterm-gui.exe"), b"").expect("the shipped name");
        assert_eq!(found(), Some(dir.join("aterm-gui.exe")));
        std::fs::create_dir_all(dir.join(".fingerprint")).expect("a build tree");
        assert_eq!(
            found(),
            None,
            "a build tree's aterm-gui.exe is the thin dev bin, never the sibling"
        );
        std::fs::write(dir.join("aterm-windowed.exe"), b"").expect("the target name");
        assert_eq!(found(), Some(dir.join("aterm-windowed.exe")));
        assert_eq!(windowed_sibling::DETACHED_PROCESS, 0x8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A BUILD TREE'S windowed image stands for this build only when it is as
    /// new as this image, give or take the two bins' link spread (measured
    /// 6 ms, in either order): `cargo run -p aterm` relinks the console image
    /// alone, and a window handed to the stale sibling runs old code while a
    /// developer validates new (review 2026-09-27).
    #[cfg(windows)]
    #[test]
    fn a_build_trees_windowed_image_older_than_this_one_is_not_used() {
        use std::time::{Duration, SystemTime};
        let now = SystemTime::now();
        let before = |secs: u64| now - Duration::from_secs(secs);
        assert!(
            !windowed_sibling::is_stale(now, now),
            "same instant: one build"
        );
        assert!(
            !windowed_sibling::is_stale(now - Duration::from_millis(6), now),
            "the measured link spread of one build"
        );
        assert!(
            !windowed_sibling::is_stale(now + Duration::from_secs(30), now),
            "newer than this image"
        );
        assert!(
            !windowed_sibling::is_stale(before(60), now),
            "within a minute"
        );
        assert!(
            windowed_sibling::is_stale(before(61), now),
            "past the spread"
        );
        assert!(
            windowed_sibling::is_stale(before(4 * 24 * 3600), now),
            "an earlier build's"
        );
    }

    /// THE SHELL'S PIPES STAY OUT OF THE DETACHED WINDOW. Under `cargo test` the
    /// harness hands this process inheritable pipes as its std handles — the
    /// same shape a capturing shell hands `aterm.exe` — and after the call none
    /// of them may be inheritable any more, or a `$o = & aterm --window` would
    /// hang until the window closed (measured; see the function). Harmless to
    /// the harness: the flag governs children, and this test spawns none.
    #[cfg(windows)]
    #[test]
    fn the_std_handles_are_not_inheritable_once_the_sibling_is_about_to_start() {
        windowed_sibling::stop_inheriting_std_handles();
        assert!(
            !windowed_sibling::a_std_handle_is_inheritable(),
            "a std handle would still be carried into the detached window"
        );
    }

    /// THE OS-DRIVEN RELAUNCH. `RegisterApplicationRestart` fires after a
    /// Restart-Manager reboot AND — `dwFlags = 0`, deliberately — after a crash
    /// or a hang, and the command line it registers comes straight back through
    /// this router. It must be a request the routing POLICY never redirects: with
    /// `windowing_behavior = "attach"` and any sibling instance alive, a
    /// forwardable relaunch opens a TAB IN THE SIBLING, so the crashed window
    /// never returns and the WM_QUERYENDSESSION-persisted session manifest is
    /// never restored — the whole reason the relaunch exists. `--window`, the
    /// flag it used to register, is an ordinary policy-eligible plain launch.
    ///
    /// It must ALSO survive the argv0 alias, because the relaunched image is
    /// whatever `current_exe()` was, which on the shipped Windows install is
    /// normally `aterm-gui.exe`.
    #[cfg(windows)]
    #[test]
    fn the_os_restart_command_line_is_the_one_verb_policy_never_forwards() {
        let line = aterm_gui::OS_RESTART_COMMAND_LINE;
        let verb = aterm_cli::Verb::from_operand(line)
            .unwrap_or_else(|| panic!("{line:?} must be a routed front-door verb"));
        assert!(verb.is_windowing(), "{line:?}");
        for behavior in [
            aterm_cli::WindowingBehavior::NewWindow,
            aterm_cli::WindowingBehavior::Attach,
        ] {
            for reachable in [true, false] {
                assert_eq!(
                    aterm_cli::route_launch(
                        aterm_cli::LaunchIntent::NewWindow,
                        behavior,
                        reachable
                    ),
                    aterm_cli::WindowRoute::Spawn,
                    "an OS relaunch must come back as a WINDOW under {behavior:?}"
                );
            }
        }
        assert_eq!(verb, aterm_cli::Verb::NewWindow);
        // …and it routes under the name the shipped shortcut actually launches.
        assert_eq!(
            alias_route("aterm-gui", line),
            AliasRoute::AliasWindowVerb,
            "the relaunched image is current_exe(), normally aterm-gui.exe"
        );
    }

    /// The mode flags are stripped for the alias exactly as they are for the
    /// front door — `RegisterApplicationRestart` and every "give me the window"
    /// script spell it `--window`, and the window's own parser rejects it as an
    /// unknown option. Past an `-e`/`--` payload boundary nothing is touched.
    #[test]
    fn the_mode_flags_are_stripped_before_the_window_but_never_inside_a_payload() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        assert_eq!(strip_mode_flags(&osv(&["--window"])), osv(&[]));
        assert_eq!(
            strip_mode_flags(&osv(&["--window", "-d", "/tmp"])),
            osv(&["-d", "/tmp"])
        );
        assert_eq!(strip_mode_flags(&osv(&["--session"])), osv(&[]));
        // The payload is a child command line, verbatim, `--window` included.
        assert_eq!(
            strip_mode_flags(&osv(&["--window", "-e", "sh", "--window"])),
            osv(&["-e", "sh", "--window"])
        );
        assert_eq!(payload_boundary(&osv(&["-d", "/tmp"])), 2);
        assert_eq!(payload_boundary(&osv(&["--window", "--", "x"])), 1);
        // The `aterm-gui` alias strips ONLY `--window`: `--session` names a mode
        // this alias cannot serve, and refusing it out loud (the window parser's
        // `unknown option`) beats swallowing it.
        assert_eq!(
            strip_flags(&osv(&["--window", "--session"]), &["--window"]),
            osv(&["--session"])
        );
    }

    /// `--no-reroute` is consumed like a mode flag — stripped for both lanes (neither
    /// mode library knows it), never inside a payload — and ESTABLISHES the internal
    /// `__ATERM_REROUTE_PASSTHROUGH=1` marker in the process environment: the one reading
    /// both lanes and every child share. A launch WITHOUT the flag clears an inherited
    /// marker: the flag is the one spelling of the escape (the `ATERM_NO_REROUTE`
    /// variable is gone, 2026-09-23), so no environment left over from another launch
    /// can make this one skip the reroute. The flag also completes.
    #[test]
    fn no_reroute_is_stripped_before_dispatch_and_sets_the_marker() {
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let env = atpkg::reroute::PASSTHROUGH_ENV;
        let engaged = || atpkg::reroute::engaged(std::env::var(env).ok().as_deref());
        // This test OWNS the variable for its duration (the lock above serializes
        // it with the routing tests that read it) and clears it on the way out
        // (and first, in case the developer is running the suite under
        // `aterm --no-reroute`).
        aterm_log::env::unset(env);
        // An INHERITED marker is not an instruction: a launch without the flag clears it.
        aterm_log::env::set(env, "1");
        assert_eq!(take_no_reroute(&osv(&["--window"])), osv(&["--window"]));
        assert!(
            !engaged(),
            "no flag, no escape — whatever the environment carried"
        );
        // The retired variable is inert: nothing reads it.
        aterm_log::env::set("ATERM_NO_REROUTE", "1");
        assert_eq!(take_no_reroute(&osv(&[])), osv(&[]));
        assert!(!engaged(), "ATERM_NO_REROUTE is not the escape any more");
        aterm_log::env::unset("ATERM_NO_REROUTE");
        // A payload token is the child's: neither stripped nor read.
        assert_eq!(
            take_no_reroute(&osv(&["-e", "sh", "--no-reroute"])),
            osv(&["-e", "sh", "--no-reroute"])
        );
        assert!(!engaged(), "a payload token must not engage the escape");
        // Before the boundary: stripped, and the environment carries it.
        assert_eq!(
            take_no_reroute(&osv(&["--window", "--no-reroute", "-d", "/tmp"])),
            osv(&["--window", "-d", "/tmp"])
        );
        assert!(engaged(), "the flag must establish {env}=1");
        // …and a flagged launch is never forwarded to a running instance: a forwarded
        // tab would not carry the marker.
        assert_eq!(
            plain_launch_request(
                &osv(&["--window", "--no-reroute"]),
                aterm_cli::LaunchEnv::default()
            ),
            None,
            "a --no-reroute launch fails closed to a local spawn"
        );
        aterm_log::env::unset(env);
        assert!(
            COMPLETION_FLAGS
                .iter()
                .any(|(flag, _)| *flag == NO_REROUTE_FLAG),
            "the flag completes"
        );
    }

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

    /// A fresh scratch directory for one test, named by pid and nanos.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-front-door-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// THE `$ATERM_AGENTS_DIR` HANDOFF (2026-09-18, closing R3): a resolved layout
    /// yields the absolute managed `agents/`, CREATED by the call (private in a `$HOME`
    /// prefix) — the window's mkdir/mode rule (`spawn::managed_agents_dir`), so the
    /// TTY session on a fresh machine has the directory before atpkg lays a twin; a
    /// second call is a no-op with the same answer; NO layout (an unset `$HOME`) hands
    /// nothing; and an ENGAGED reroute escape changes nothing — the switch is not an
    /// input, which is the whole point of the handoff (`--no-reroute` restores the
    /// upstream Rust names, never the managed `claude`/`codex`).
    #[test]
    fn the_agents_dir_is_handed_on_every_lane_regardless_of_the_reroute_switch() {
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scratch = scratch_dir("agents-handoff");
        let layout = atpkg::store::Layout {
            prefix: scratch.join("pkg"),
        };
        assert_eq!(agents_dir_handoff(None), None, "no layout, nothing handed");
        assert!(
            !layout.agents_dir().exists(),
            "a fresh prefix has no agents/"
        );
        let handed = agents_dir_handoff(Some(&layout)).expect("created, hence handed");
        assert_eq!(handed, layout.agents_dir().to_str().unwrap());
        assert!(std::path::Path::new(&handed).is_absolute());
        assert!(
            std::fs::symlink_metadata(&handed).unwrap().is_dir(),
            "a real directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&handed).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "a $HOME prefix's directory is private");
        }
        assert_eq!(
            agents_dir_handoff(Some(&layout)).as_deref(),
            Some(handed.as_str()),
            "a second call is a no-op with the same answer"
        );
        // The reroute escape engaged (the marker `--no-reroute` establishes): still handed.
        let env = atpkg::reroute::PASSTHROUGH_ENV;
        let before = std::env::var_os(env);
        for value in ["1", "yes"] {
            aterm_log::env::set(env, value);
            assert!(atpkg::reroute::engaged(std::env::var(env).ok().as_deref()));
            assert_eq!(
                agents_dir_handoff(Some(&layout)).as_deref(),
                Some(handed.as_str()),
                "{env}={value} must not drop the managed agents dir"
            );
        }
        match before {
            Some(v) => aterm_log::env::set(env, v),
            None => aterm_log::env::unset(env),
        }
        // And what `hand_agents_dir` establishes is exactly that value — or NOTHING:
        // an inherited stray is removed, never left for the session to trust.
        let var = atpkg::reroute::AGENTS_DIR_ENV;
        assert_eq!(var, "ATERM_AGENTS_DIR", "the contract's spelling");
        hand_agents_dir(Some(&layout));
        assert_eq!(std::env::var(var).ok().as_deref(), Some(handed.as_str()));
        hand_agents_dir(None);
        assert_eq!(
            std::env::var_os(var),
            None,
            "no layout ⇒ not set, not empty"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A prefix that REFUSES the `mkdir` (read-only, owned by us) hands nothing —
    /// said on stderr, never a nonexistent entry first on the session's PATH — and
    /// creates nothing; a symlink at `agents/` is refused the same way and left alone.
    /// `hand_agents_dir` then REMOVES an inherited value rather than passing it on.
    /// Root ignores mode bits, so the read-only leg is skipped there.
    #[cfg(unix)]
    #[test]
    fn a_refused_agents_dir_hands_nothing_and_clears_an_inherited_stray() {
        use std::os::unix::fs::PermissionsExt as _;
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scratch = scratch_dir("agents-refused");
        let var = atpkg::reroute::AGENTS_DIR_ENV;
        // A symlink at agents/ — even one resolving to a real directory: refused.
        let real = scratch.join("real-agents");
        std::fs::create_dir_all(&real).unwrap();
        let linked = atpkg::store::Layout {
            prefix: scratch.join("linked"),
        };
        std::fs::create_dir_all(&linked.prefix).unwrap();
        std::os::unix::fs::symlink(&real, linked.agents_dir()).unwrap();
        assert!(linked.agents_dir().is_dir(), "the link resolves");
        assert_eq!(agents_dir_handoff(Some(&linked)), None);
        assert!(
            std::fs::symlink_metadata(linked.agents_dir())
                .unwrap()
                .file_type()
                .is_symlink(),
            "left alone"
        );
        // The line names the path ONCE, never `ensure_private_dir`'s `update directory`
        // noun, and does not send the user to a `repair` that refuses the same link.
        let refusal_line = |dir: &std::path::Path, error: &str, system: bool| {
            atpkg::store::agents_dir_refusal_line(dir, error, system, SESSION_REACH)
        };
        let refusal = linked.ensure_agents_dir().expect_err("refused");
        let line = refusal_line(&linked.agents_dir(), &refusal, false);
        assert_eq!(
            line.matches(&linked.agents_dir().display().to_string())
                .count(),
            1,
            "{line}"
        );
        assert!(!line.contains("update directory"), "{line}");
        assert!(line.contains("is a symlink; refusing"), "{line}");
        assert!(line.contains("in this session —"), "{line}");
        assert!(
            line.ends_with("— remove that symlink, then run `aterm pkg repair`"),
            "{line}"
        );
        // A system prefix changes nothing here: the entry must go first either way.
        assert_eq!(refusal_line(&linked.agents_dir(), &refusal, true), line);
        // A regular file at agents/: the same by-hand remedy.
        let filed = atpkg::store::Layout {
            prefix: scratch.join("filed"),
        };
        std::fs::create_dir_all(&filed.prefix).unwrap();
        std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
        let refusal = filed.ensure_agents_dir().expect_err("refused");
        let line = refusal_line(&filed.agents_dir(), &refusal, false);
        assert!(line.contains("exists and is not a directory"), "{line}");
        assert!(
            line.ends_with("— remove that file, then run `aterm pkg repair`"),
            "{line}"
        );
        aterm_log::env::set(var, real.to_str().unwrap());
        hand_agents_dir(Some(&linked));
        assert_eq!(
            std::env::var_os(var),
            None,
            "an inherited stray must not survive a refusal"
        );
        // SAFETY: getuid() takes no arguments and cannot fail.
        if unsafe { libc::getuid() } == 0 {
            eprintln!("running as root; a read-only prefix refuses nothing — skipping that leg");
            let _ = std::fs::remove_dir_all(&scratch);
            return;
        }
        let layout = atpkg::store::Layout {
            prefix: scratch.join("pkg"),
        };
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(agents_dir_handoff(Some(&layout)), None);
        assert!(!layout.agents_dir().exists(), "nothing created");
        // A refused mkdir: the path once, and `repair` IS the remedy (root where needed).
        let refusal = layout.ensure_agents_dir().expect_err("refused");
        let line = refusal_line(&layout.agents_dir(), &refusal, false);
        assert_eq!(
            line.matches(&layout.agents_dir().display().to_string())
                .count(),
            1,
            "{line}"
        );
        assert!(line.ends_with("— run `aterm pkg repair`"), "{line}");
        assert!(!line.contains("remove that"), "{line}");
        // On a root-owned prefix the line says so instead of hedging.
        assert!(
            refusal_line(&layout.agents_dir(), &refusal, true)
                .ends_with("— run `aterm pkg repair` as root"),
            "{line}"
        );
        aterm_log::env::set(var, real.to_str().unwrap());
        hand_agents_dir(Some(&layout));
        assert_eq!(std::env::var_os(var), None);
        // Writable again: created, private, handed.
        std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        hand_agents_dir(Some(&layout));
        assert_eq!(
            std::env::var(var).ok().as_deref(),
            layout.agents_dir().to_str()
        );
        assert_eq!(
            std::fs::metadata(layout.agents_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        aterm_log::env::unset(var);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The handoff is established in the session lane OUTSIDE the reroute gate — before
    /// it, and before the lane's first background thread — so an engaged
    /// `--no-reroute` cannot skip it and no thread races the `setenv`. A scrape, in
    /// the idiom of `the_session_lane_applies_the_machine_settings_outside_every_package_gate`.
    #[test]
    fn the_agents_dir_handoff_sits_outside_the_reroute_gate() {
        let src = include_str!("main.rs");
        let handoff = src
            .find("hand_agents_dir(layout.as_ref());")
            .expect("the session lane hands the agents dir");
        let quiet = src
            .find("let quiet = aterm_cli::parse_args(mode_args);")
            .expect("the session lane's start");
        let gate = src
            .find("if !atpkg::reroute::engaged(")
            .expect("the session lane's reroute gate");
        assert!(
            quiet < handoff && handoff < gate,
            "the handoff sits in the session lane BEFORE the reroute gate, never inside it"
        );
        let lay = src
            .find(".name(\"aterm-reroute-lay\".into())")
            .expect("the gate's background lay");
        assert!(
            handoff < lay,
            "established single-threaded, before the lane's first thread"
        );
    }
}
