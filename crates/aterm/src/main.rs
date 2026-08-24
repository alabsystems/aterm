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
    let first = rest
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    match alias_route(argv0, &first) {
        AliasRoute::Ctl => return aterm_ctl::main_entry(rest),
        AliasRoute::Pkg => return atpkg::cli::main_entry(rest),
        AliasRoute::Fleet => aterm_agent::fleet_cli::main_entry(rest),
        AliasRoute::Drive => return aterm_agent::drive_cli::main_entry(rest),
        AliasRoute::AliasWindowVerb => return window_verb(&first, &rest[1..]),
        AliasRoute::AliasWindow => return gui_alias_entry(rest),
        // `aterm`, the old `aterm-cli` symlink target, and anything else
        // (a renamed copy) are all the front door.
        AliasRoute::FrontDoor => {}
    }

    // --- the front door -----------------------------------------------------

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

    // PENDING-PROGRAM arm (R6): a default-set tool whose real shim has not landed
    // yet resolves to a pending STUB in the managed store — `aterm trust` moments
    // after a lean install must print the live install state (and bump trust to the
    // front of the queue), never fall through to "unknown option". Same in-process
    // dispatch as the arm above; `atpkg __pending` prints the message and exits 127.
    if aterm_cli::is_tool_candidate(Some(first.as_str())) && pending_stub_resolves(&first) {
        return atpkg::cli::main_entry(vec![OsString::from("__pending"), rest[0].clone()]);
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
    let scan = &rest[..payload_boundary(&rest)];
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
    let mode_args = strip_mode_flags(&rest);
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
fn gui_alias_entry(rest: Vec<OsString>) -> ExitCode {
    if let Some(code) = plain_launch_policy(&rest) {
        return code;
    }
    aterm_gui::main_entry(strip_flags(&rest, &["--window"]));
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
