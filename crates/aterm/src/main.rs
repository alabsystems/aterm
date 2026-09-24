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
//! every pre-one-binary script, PATH entry, `$ATERM_CTL` hatch, and in-app
//! Help example keeps working while exactly ONE Mach-O exists.

// GUI subsystem on Windows: rust binaries default to the CONSOLE subsystem,
// which pops a stray blank console window alongside the terminal on every
// Explorer / Start-menu launch. The window library's `attach_parent_console`
// (first thing in its entry) reattaches stdio when launched FROM a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::ffi::OsString;
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
    eprintln!(
        "aterm {verb}: not available on this platform. The fabric bridge is Unix-only \
         (Unix-domain sockets, inherited descriptors); nothing was started."
    );
    ExitCode::FAILURE
}

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
        AliasRoute::AliasWindowVerb => return window_verb(&first, rest.get(1..).unwrap_or(&[])),
        AliasRoute::AliasWindow => return gui_alias_entry(rest),
        // `aterm`, the old `aterm-cli` symlink target, and anything else
        // (a renamed copy) are all the front door.
        AliasRoute::FrontDoor => {}
    }

    // --- the front door -----------------------------------------------------

    // THE HIDDEN HELPER VERBS, routed by NAME and not by argv0. The untracked lane
    // submits a launchd job that runs THIS binary on `__stage-payload` / `__lay-files`,
    // and since the provenance fix it runs an untagged binary IN PLACE — under whatever
    // name it has on disk, which for the shipped app is `…/Contents/MacOS/aterm`. The
    // argv0 alias only fires for a binary literally named `atpkg` (a byte COPY is), so
    // the in-place arm arrived here and died in the mode fork: measured 2026-09-13 on the
    // installed bundle binary, exit 2 and `aterm-gui: unknown option '__lay-files'`, which
    // made `aterm pkg install` from the shipped app refuse with a fresh message. They are
    // dispatched above the verb match for the same reason `atpkg`'s own CLI dispatches
    // them above its: they are unlisted, they are not `aterm_cli::Verb`s, and no roster
    // or help surface may grow a row for them.
    if first == atpkg::stage_helper::HIDDEN_VERB
        || first == atpkg::lay::HIDDEN_VERB
        || first == atpkg::seam::HIDDEN_VERB
    {
        return atpkg::cli::main_entry(rest.to_vec());
    }

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
            aterm_cli::Verb::NewTab | aterm_cli::Verb::NewWindow | aterm_cli::Verb::SplitPane => {
                window_verb(verb.name(), &forwarded)
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
        if let Some(code) = plain_launch_policy(&rest) {
            return code;
        }
        aterm_gui::main_entry(mode_args);
        return ExitCode::SUCCESS;
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
    // (`Layout::ensure_agents_dir` — the window's mkdir/mode rule: one `mkdir`, mode by
    // prefix shape, never a wait — plus a symlink/file refusal the window's
    // `spawn::managed_agents_dir` does not yet make: it warns and still hands a linked
    // `agents/`, this lane hands nothing) and hands it to `session_main` as
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
    // `[update]` owner/repo repoint (`aterm_gui::configured_update_source`, 2026-09-14;
    // no env override since 2026-09-23) — the same resolution the window's loop and the
    // ctl `update check` verb use. "Check for updates automatically" off (`[update]
    // enabled = false`, Settings ▸ Terminal ▸ Updates) makes the call a no-op, exactly as
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
    //     old; no pass installing now or attempted anywhere on the machine in the last
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
    //     INTERACTIVE launch: stdin a terminal and no `ATERM_SESSION_MODEL` — a harness
    //     driving the session over pipes (the integration tests, a driver's `--session`
    //     child) must never provision the machine's real prefix as a side effect
    //     (2026-09-10 review: `targo test -p aterm` rewrote the owner's status.toml).
    // THE HOST SETTINGS ARE NOT THE PACKAGE MANAGER'S TO GATE. A session launch — this
    // binary run from another terminal, or over ssh — could only apply them as a side
    // effect of a package pass that was due, so a Mac with `[packages] enabled = false`
    // (then also spelled `auto_update = false`, or an environment kill switch), or simply
    // a pass that ran an hour ago, never got them from this lane at all. They take no store lock, need no index and
    // no network, and `machine apply` prints nothing when nothing changed — but they walk
    // `$HOME`, so the lane runs them ONCE A DAY, on its own stamp beside `status.toml`
    // (`machine-apply.stamp`), not at every launch. Interactive launches only, for the
    // same reason the pass above is gated that way: a harness driving a session over
    // pipes must not touch the real machine.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    if cfg!(target_os = "macos")
        && session_lane_is_interactive()
        && let Some(layout) = layout.as_ref()
        && machine_apply_due(layout, now)
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

    session_lane(quiet)
}

/// THE `$ATERM_AGENTS_DIR` DECISION, pure over the resolved layout (2026-09-18):
/// `Some(dir)` — the absolute managed `<prefix>/agents/`, ensured to exist as a real
/// directory by [`atpkg::store::Layout::ensure_agents_dir`] — is what the session is
/// handed; `None` when there is no layout (no `$HOME`), when the directory could not
/// be created or is a symlink/file (said on stderr ONCE, [`agents_dir_refusal_line`]),
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
            "aterm: managed agents dir not created (the package prefix {} is not an absolute path — is `$HOME` set?); the managed `claude`/`codex` are NOT in front of PATH in this session",
            layout.prefix.display()
        );
        return None;
    }
    match layout.ensure_agents_dir() {
        Ok(dir) => dir.to_str().map(str::to_owned),
        Err(error) => {
            eprintln!("{}", agents_dir_refusal_line(&layout.agents_dir(), &error));
            None
        }
    }
}

/// The one stderr line for a refused `agents/`, with the remedy that is TRUE for what
/// is there: a symlink or a regular file at `agents/` must be removed by hand — `aterm
/// pkg repair` reaches the directory through the same `ensure_dir` and refuses the same
/// entry rather than replacing it (`activate::reconcile_agents`), so naming repair
/// there would send the user in a loop; anything else (the `mkdir` refused: a system
/// prefix without root, an unowned prefix) is what `repair` re-lays, as root where the
/// prefix needs it. `error` already starts with the path ([`atpkg::store::Layout::ensure_agents_dir`]).
fn agents_dir_refusal_line(dir: &std::path::Path, error: &str) -> String {
    let remedy = match std::fs::symlink_metadata(dir) {
        Ok(md) if md.file_type().is_symlink() || !md.is_dir() => {
            "remove that entry by hand, then `aterm pkg repair` lays the directory and the twins"
        }
        _ => "`aterm pkg repair` re-lays it (a system prefix needs root)",
    };
    format!(
        "aterm: managed agents dir not created ({error}); the managed `claude`/`codex` are NOT in front of PATH in this session — {remedy}"
    )
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
/// child: stdin is a terminal and no `ATERM_SESSION_MODEL` is set (the development
/// seam that arms the session's VT model — `aterm_types::dev_seam!`, read by no shipped
/// binary). Only such a launch may spawn the detached toolchain pass — a piped launch is
/// a test or a driver, and a test must never mutate the machine's real package prefix.
fn session_lane_is_interactive() -> bool {
    stdin_is_terminal() && aterm_cli::session_model_seam().is_none()
}

/// How often a terminal session runs `aterm pkg machine apply`: once a day, on the lane's
/// own claim. The window runs it as it opens, a package pass after an edit to `[machine]`
/// (or an apply that did not finish), and Settings' Apply now at once.
const MACHINE_APPLY_EVERY_SECS: u64 = 24 * 60 * 60;

/// Whether this launch runs the daily `aterm pkg machine apply`: it claims the lane's
/// once-a-day slot in `machine-apply.stamp` (Phase 3). The walk of `$HOME` it costs used
/// to run at EVERY session launch — every tab.
fn machine_apply_due(layout: &atpkg::store::Layout, now_unix: i64) -> bool {
    aterm_update_core::pkg_check::claim(
        &layout.machine_apply_stamp(),
        now_unix,
        MACHINE_APPLY_EVERY_SECS,
    )
}

/// Whether this launch spawns the detached pass: the MACHINE-WIDE rule the window's loop
/// runs its six-hour walk on ([`aterm_update_core::pkg_check::full_pass_owed`]) over the
/// stamps every lane reads ([`atpkg::status::pass_stamps`]: the outcome the last pass
/// recorded, never `updated_at`; a pass installing now, not queued behind), then the
/// session lane's own claim, so tabs opened in the same moment spawn one pass. The derived
/// model `AtpkgFullPassRule` states the rule — and proves it never spawns inside a
/// rate-limit hold it does not read; atpkg's `status` conformance binds this reading to it.
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
/// ([`machine_apply_due`]).
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
/// ([`aterm_update_core::pkg_check::pass_flags`]) — the window's own. Pure for the test.
fn session_pass_args(layout: &atpkg::store::Layout) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["pkg".into(), "update".into()];
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
/// Windows is unit-testable: the shipped install is SEVERAL IDENTICAL COPIES of
/// this binary (`aterm.exe`, `aterm-gui.exe`, `aterm-ctl.exe`, …), the Start-Menu
/// shortcut targets `aterm-gui.exe`, and the taskbar jump list is committed by
/// whichever copy is running — so the windowing verbs and the routing policy have
/// to work under the alias, not only under `aterm.exe`.
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
    /// parser instead is how a taskbar row becomes `unknown option 'new-window'`
    /// against a console that does not exist.
    AliasWindowVerb,
    /// `aterm-gui …` — the window, with the plain-launch routing policy applied
    /// (see [`gui_alias_entry`]).
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
        "aterm-gui" => match aterm_cli::Verb::from_operand(first) {
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
fn gui_alias_entry(rest: Vec<OsString>) -> ExitCode {
    if let Some(code) = plain_launch_policy(&rest) {
        return code;
    }
    aterm_gui::main_entry(strip_flags(&take_no_reroute(&rest), &["--window"]));
    ExitCode::SUCCESS
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
/// `Some(code)` when the running instance served it; `None` to go on and open a
/// window here.
///
/// The eligibility gate is `plain_launch_is_policy_eligible` and it is
/// deliberately narrow: `-e`, `--headless`/`$ATERM_HEADLESS`, `--diagnose` and an
/// update successor's inherited argv all carry instructions a forwarded tab
/// cannot honour, so they fail closed to spawning. See that function for the
/// case-by-case reasoning. Under the shipped default this returns `None` without
/// dialing anything.
fn plain_launch_policy(argv: &[OsString]) -> Option<ExitCode> {
    let env = aterm_cli::LaunchEnv {
        updated_from: std::env::var_os("ATERM_UPDATED_FROM").is_some(),
        // PRESENCE, matching the mode fork's own test for the same variable, so
        // "this launch is headless-shaped" means one thing in both places.
        headless: std::env::var_os("ATERM_HEADLESS").is_some(),
    };
    let request = plain_launch_request(argv, env)?;
    route_and_maybe_forward(&request)
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
/// EXIT CODES, which matter more here than they look. This binary is
/// GUI-subsystem on Windows (see the crate attribute), so the console it prints
/// to is the parent's, reattached by `attach_parent_console` before ANY route
/// runs — including this one. That reattachment deliberately restores the
/// parent's own redirected handles (the `aterm --version > out.txt` invariant
/// `build.ps1` depends on), and nothing here disturbs it: this route only ever
/// writes to the already-resolved `stderr`, and it returns an `ExitCode` rather
/// than calling `process::exit`, so `main`'s normal teardown still runs.
///   * `0` — the tab/window/pane was opened (forwarded or spawned).
///   * `1` — the running instance answered `ERR`; its text is on stderr.
///   * `2` — a grammar error (unknown option, missing `<dir>`, bad directory).
fn window_verb(verb: &str, args: &[OsString]) -> ExitCode {
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
    if let Some(code) = route_and_maybe_forward(&request) {
        return code;
    }
    // The SPAWN route. `split-pane` lands here when nothing was reachable (or
    // under the default policy): a brand-new window is a single pane, so there
    // is nothing to split. Say so rather than open a window that silently is not
    // what was asked for — `wt split-pane` under `useNew` has exactly this
    // outcome, and quietly is the wrong way to have it.
    if request.intent == aterm_cli::LaunchIntent::SplitPane {
        eprintln!(
            "aterm: no running aterm to split — opening a new window instead \
             (a fresh window is one pane; `aterm split-pane` again inside it splits that)"
        );
    }
    aterm_gui::main_entry(request.window_args());
    ExitCode::SUCCESS
}

/// Decide the route for `request` and, when it is `Forward`, perform it.
///
/// Returns `Some(code)` when the request was answered by the running instance
/// (the process should exit with that code) and `None` when the caller should
/// go on to start a window itself. The two impure inputs — the effective policy
/// and whether an instance answered — are gathered here and handed to the pure
/// [`aterm_cli::route_launch`], so the decision itself stays testable.
///
/// The reachability probe and the forward are two separate dials, so an instance
/// can die in between. That race resolves to `None` (spawn), not an error: the
/// operator asked for a terminal and a transport failure is not a reason to
/// refuse one. An `ERR` reply is the opposite case — the instance IS there and
/// REFUSED — and is reported as a failure, because spawning a window then would
/// contradict the policy the operator chose AND could double-open if the refusal
/// was partial.
fn route_and_maybe_forward(request: &aterm_cli::WindowRequest) -> Option<ExitCode> {
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
    let route = aterm_cli::route_launch(request.intent, behavior, sock.is_some());
    if route != aterm_cli::WindowRoute::Forward {
        return None;
    }
    let sock = sock?;
    let line = match request.control_request() {
        Ok(line) => line,
        Err(message) => {
            eprintln!("aterm: {message}");
            return Some(ExitCode::from(2));
        }
    };
    match aterm_ctl::front_door_send(&sock, &line) {
        // `spawn` replies `OK <sid>`. The sid is deliberately NOT printed: `wt
        // new-tab` prints nothing, and a shell prompt is not a log.
        Ok(reply) if reply.starts_with("OK") => Some(ExitCode::SUCCESS),
        Ok(reply) => {
            eprintln!("aterm: the running aterm refused: {reply}");
            Some(ExitCode::FAILURE)
        }
        Err(error) => {
            // Raced (the instance exited between the probe and the dial), or the
            // socket wedged. Fall back to starting one — with a line saying why,
            // so an operator who set `attach` is never left wondering why a
            // second window appeared.
            eprintln!("aterm: could not reach the running aterm ({error}); opening a new window");
            None
        }
    }
}

/// The effective `windowing_behavior`: `$ATERM_WINDOWING_BEHAVIOR`, else the
/// `aterm.toml` key, else the default. An unrecognized spelling warns ONCE and
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
        println!(
            "usage: aterm update [status|check] [-v] | identity\nLinux: aterm update enable [--proof-dir DIR] | apply | rollback\nBootstrap: aterm update install --target ABS/aterm --proof-dir DIR --candidate FILE\nLinux updates replace only the on-disk executable; running sessions are never restarted."
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
            _ => Err("usage: aterm update enable [--proof-dir DIR] | apply | rollback".into()),
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
                eprintln!(
                    "aterm: unknown update argument {arg:?} (usage: aterm update [status|check] \
                     [-v])"
                );
                return ExitCode::from(2);
            }
        }
    }
    let checking = sub.as_deref() == Some("check");
    let st = match sub.as_deref().unwrap_or("status") {
        "status" => aterm_update::status(build),
        "check" => {
            // A check can download a whole release: say it is working, to a person only.
            if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
                eprintln!("Checking for updates\u{2026}");
            }
            let provider: aterm_update::CheckSettingsProvider =
                std::sync::Arc::new(aterm_gui::configured_update_settings);
            Some(aterm_update::check_now_with_settings(build, &provider))
        }
        other => {
            eprintln!(
                "aterm: unknown update sub-command {other:?} (usage: aterm update \
                 [status|check] [-v])"
            );
            return ExitCode::from(2);
        }
    };
    // `None` when this platform has no updater, or `HOME` is unset, or the updater's
    // private staging directory cannot be made — so the platform is not the only reason.
    let Some(st) = st else {
        println!(
            "Nothing to report: this copy of aterm can\u{2019}t update itself here (no updater \
             on this platform, or HOME is not set)."
        );
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
/// `installs_by_itself` is `[update] auto_apply`, `automatic_checks` `[update] enabled`.
/// Pure for the test.
fn update_summary(
    version: &str,
    build: u64,
    st: &aterm_update::UpdateStatus,
    now: i64,
    installs_by_itself: bool,
    automatic_checks: bool,
) -> (String, Option<String>) {
    if !st.enabled {
        // Only a Linux copy reaches here (macOS always has an updater; elsewhere there
        // is no ledger at all): not enrolled, or automatic updates are off. The
        // ledger's sentence names the remedy (`aterm update enable`).
        let remedy = st.outcome.trim();
        return (
            format!("aterm {version} doesn\u{2019}t update itself on this machine"),
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
    if !st.installable {
        // A dev build, or a copy run from the disk image or a quarantined download: the
        // checker deliberately does nothing here, which otherwise reads as idleness.
        return (
            format!(
                "This copy of aterm {version} can\u{2019}t update itself \u{2014} only aterm.app \
                 installed in Applications does"
            ),
            None,
        );
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
            format!("update checks keep failing: {}", check_trouble_words(class))
        };
        return (
            format!("aterm {version} \u{b7} {trouble}; aterm keeps trying"),
            Some(UPDATE_LOG_HINT.to_string()),
        );
    }
    if st.failing_checks > 0 {
        return (
            format!(
                "aterm {version} \u{b7} the last check didn\u{2019}t finish: {}; aterm will try \
                 again",
                check_trouble_words(class)
            ),
            Some(UPDATE_LOG_HINT.to_string()),
        );
    }
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at).map_or_else(
        || "not checked yet".to_string(),
        |at| format!("checked {}", ago_words(at, now)),
    );
    let mut line = format!("aterm {version} is up to date \u{b7} {checked}");
    if !automatic_checks {
        line.push_str(
            " \u{b7} automatic checks are off (Settings \u{25b8} Terminal \u{25b8} Updates)",
        );
    }
    (line, None)
}

/// A failing check CLASS (the updater's health-ledger names) in a person's words.
fn check_trouble_words(kind: &str) -> &'static str {
    match kind {
        "network" => "the update server can\u{2019}t be reached",
        "manifest" => "the newest release couldn\u{2019}t be verified",
        "pipeline" => "downloads aren\u{2019}t finishing",
        "stage" => "a download couldn\u{2019}t be prepared",
        _ => "the check failed",
    }
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
    if let Some(native) = &st.linux {
        println!(
            "  Linux: running build {build}, installed build {}",
            native.installed_build
        );
        if let Some(staged) = native.staged_build {
            println!(
                "  verified Linux build {staged} staged; apply explicitly with: aterm update apply"
            );
        }
        if let Some(phase) = &native.trial_phase {
            println!(
                "  trial: {phase}, starts={}, healthy={}",
                native.trial_starts, native.trial_healthy
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
            rescues: 0,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
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
            update_summary("0.91.0", 100, st, now, auto_apply, automatic)
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
            line.ends_with(
                "automatic checks are off (Settings \u{25b8} Terminal \u{25b8} Updates)"
            ),
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
        let mut never = update_status();
        never.updated_at = String::new();
        assert_eq!(
            say(&never, true, true).0,
            "aterm 0.91.0 is up to date \u{b7} not checked yet"
        );

        // LINUX (main's native delivery, merged 2026-09-23): a copy that is not
        // enrolled says so plainly and hands on the ledger's remedy, and a staged
        // Linux build names the verb that installs it — never "only on macOS".
        let mut unenrolled = update_status();
        unenrolled.enabled = false;
        unenrolled.outcome = "Linux self-update is not enrolled; run aterm update enable \
                              explicitly for this installed copy"
            .into();
        let (line, trouble) = say(&unenrolled, true, true);
        assert_eq!(
            line,
            "aterm 0.91.0 doesn\u{2019}t update itself on this machine"
        );
        assert!(trouble.is_some_and(|t| t.contains("aterm update enable")));
        let mut native = update_status();
        native.linux = Some(aterm_update::LinuxUpdateStatus {
            installed_build: 100,
            staged_build: Some(101),
            staged_version: Some("0.92.0".into()),
            staged_commit: None,
            trial_phase: None,
            trial_starts: 0,
            trial_healthy: false,
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
    /// (2026-09-22) the gate is their own daily claim ([`machine_apply_due`]), not every
    /// launch. A scrape, in the idiom of atpkg's own placement test, because the
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
            .rfind("&& machine_apply_due(layout, now)")
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
    /// hours since the last success, and no pass attempted anywhere in the last five
    /// minutes — then the lane's own claim, so the second of two tabs opened together
    /// spawns nothing. A fresh success, or a sibling's attempt a minute ago, is no pass.
    /// (A record that shows only FAILED attempts is the next test's.)
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
    /// through Phase 3): a record whose passes never succeed — an unserved triple exits 2
    /// and stamps no success — owes nothing for the interval after its last attempt, as the
    /// lane's old attempt-age rule answered; the spacing alone would have let a tab opened
    /// five minutes on spawn another detached pass. And a pass INSTALLING now is not queued
    /// behind: no unobserved waiter per tab.
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

    /// NO TAB SPAWNS INSIDE A RATE-LIMIT HOLD (§3.2 of the 2026-09-22 design), and reads
    /// none to get there: a hold is recorded only with a pass's end — as atpkg's own writer
    /// lays it here, a success a day old, then a rate-limited pass that failed, or one the
    /// cache stood in for — and that end holds the tab's rule an interval, past any reset.
    #[test]
    fn a_rate_limit_hold_is_outlasted_by_the_pass_that_recorded_it() {
        use aterm_update_core::pkg_check::{FULL_PASS_INTERVAL_SECS, PassOutcome, rfc3339_to_unix};
        let walk = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
        for (label, outcome) in [
            ("held-failed", PassOutcome::Failed),
            ("held-ok", PassOutcome::Ok),
        ] {
            let layout = scratch_layout(label);
            let ended = "2026-09-22T13:00:00Z";
            let end = rfc3339_to_unix(ended).expect("stamp");
            let success = if outcome == PassOutcome::Ok {
                ended
            } else {
                "2026-09-21T13:00:00Z"
            };
            atpkg::status::stamp_success(&layout, success).unwrap();
            atpkg::status::stamp_pass_end(
                &layout,
                ended,
                outcome,
                atpkg::status::MeteredHold::Until(end + 20 * 60),
            )
            .unwrap();
            for at in [end + 60, end + 20 * 60, end + walk - 1] {
                assert!(!session_pass_due(&layout, at), "{label} at +{}", at - end);
            }
            assert!(
                session_pass_due(&layout, end + walk),
                "{label}: an interval on"
            );
            let _ = std::fs::remove_dir_all(&layout.prefix);
        }
    }

    /// The `[machine]` one-shot runs once a day from the session lane, not at every
    /// launch: the first launch claims the day, the rest of it spawns nothing.
    #[test]
    fn the_machine_one_shot_is_claimed_once_a_day() {
        let layout = scratch_layout("machine");
        let now = 1_790_000_000_i64;
        let day = i64::try_from(MACHINE_APPLY_EVERY_SECS).unwrap();
        assert!(machine_apply_due(&layout, now));
        assert!(!machine_apply_due(&layout, now + 60));
        assert!(!machine_apply_due(&layout, now + day - 1));
        assert!(machine_apply_due(&layout, now + day));
        assert!(layout.machine_apply_stamp().is_file());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The detached pass runs on the window's argv — the lock wait and the progress file
    /// under the prefix — so a contended pass queues, and a window follows its progress.
    #[test]
    fn the_detached_pass_runs_on_the_windows_argv() {
        let layout = atpkg::store::Layout {
            prefix: std::path::PathBuf::from("/p"),
        };
        let words: Vec<String> = session_pass_args(&layout)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            words,
            [
                "pkg",
                "update",
                "--wait-lock",
                "1800",
                "--progress-file",
                "/p/progress.json"
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
        let refusal = linked.ensure_agents_dir().expect_err("refused");
        let line = agents_dir_refusal_line(&linked.agents_dir(), &refusal);
        assert_eq!(
            line.matches(&linked.agents_dir().display().to_string())
                .count(),
            1,
            "{line}"
        );
        assert!(!line.contains("update directory"), "{line}");
        assert!(line.contains("is a symlink; refusing"), "{line}");
        assert!(line.contains("remove that entry by hand"), "{line}");
        assert!(!line.contains("re-lays it"), "{line}");
        // A regular file at agents/: the same by-hand remedy.
        let filed = atpkg::store::Layout {
            prefix: scratch.join("filed"),
        };
        std::fs::create_dir_all(&filed.prefix).unwrap();
        std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
        let refusal = filed.ensure_agents_dir().expect_err("refused");
        let line = agents_dir_refusal_line(&filed.agents_dir(), &refusal);
        assert!(line.contains("exists and is not a directory"), "{line}");
        assert!(line.contains("remove that entry by hand"), "{line}");
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
        let line = agents_dir_refusal_line(&layout.agents_dir(), &refusal);
        assert_eq!(
            line.matches(&layout.agents_dir().display().to_string())
                .count(),
            1,
            "{line}"
        );
        assert!(line.contains("`aterm pkg repair` re-lays it"), "{line}");
        assert!(!line.contains("by hand"), "{line}");
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
