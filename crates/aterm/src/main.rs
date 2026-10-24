// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: the one binary.

#![cfg_attr(all(unix, trust_verify), feature(register_tool))]
#![cfg_attr(all(unix, trust_verify), register_tool(trust))]

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::ExitCode;

use aterm_cli::{AliasRoute, alias_route};

macro_rules! println {
    () => {
        ::aterm_uds::stdout::print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {{
        let mut line = ::std::format!($($arg)*);
        line.push('\n');
        let _ = ::aterm_uds::stdout::write_all(line.as_bytes());
    }};
}

/// `aterm link` and `aterm fabric` where the fabric bridge cannot exist: `aterm-link` is a
/// `cfg(unix)` dependency, so the verbs stay on the roster and refuse by name.
#[cfg(not(unix))]
fn link_unavailable(verb: &str) -> ExitCode {
    eprintln!("aterm {verb}: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

/// `aterm machines` where it cannot exist: `aterm-machines` drives macOS's own tools and is a
/// `cfg(target_os = "macos")` dependency, so the verb stays on the roster and refuses by name.
#[cfg(not(target_os = "macos"))]
fn machines_unavailable() -> ExitCode {
    eprintln!("aterm machines: not available on this platform (macOS only)");
    ExitCode::FAILURE
}

/// `aterm secrets` where it cannot exist: the vault keeps its secrets private with POSIX file
/// modes (0600 files in a 0700 `~/.secrets`), which Windows lacks, so `aterm-secrets` is a
/// `cfg(unix)` dependency and the verb refuses by name (docs/DESIGN-secrets-2026-10-20.md §12).
#[cfg(not(unix))]
fn secrets_unavailable() -> ExitCode {
    eprintln!("aterm secrets: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

const CTL_LOCAL: aterm_ctl::LocalVerbs = aterm_ctl::LocalVerbs {
    cast_drift: Some(ctl_cast_drift),
    #[cfg(unix)]
    fleet_survey: Some(aterm_agent::survey_fleet),
    #[cfg(not(unix))]
    fleet_survey: None,
    #[cfg(unix)]
    fleet_drive: Some(aterm_agent::drive_fleet),
    #[cfg(not(unix))]
    fleet_drive: None,
};

fn ctl_cast_drift(job: &aterm_ctl::CastDriftJob<'_>) -> Result<String, String> {
    use aterm_control::cast_drift::{self as drift, Cut, DriftArgs, LiveScreen, Undo};
    let mut args = DriftArgs::parse(job.args).map_err(str::to_string)?;
    args.undo = match job.peer {
        aterm_ctl::DriftPeer::PreUndo => Undo::Drop,
        aterm_ctl::DriftPeer::Unknown => Undo::Match,
    };
    let mut cast =
        aterm_control::asciicast::parse(job.cast).map_err(|e| format!("cast drift: {e}"))?;
    if let Some(until) = job.until {
        cast = cast.until(until);
    }
    let live = job.screen.map(|rows| LiveScreen {
        rows: rows.to_vec(),
        cursor: job.cursor,
        alt: job.alt,
    });
    let started = std::time::Instant::now();
    let mut report = if job.cut == aterm_ctl::DriftCut::Racy {
        drift::analyze_replay(
            &cast.header,
            &cast.events,
            job.alt,
            &args,
            drift::ResizePolicy::Native,
        )
    } else {
        drift::analyze(
            &cast.header,
            &cast.events,
            live.as_ref(),
            &args,
            drift::ResizePolicy::Native,
        )
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    report.settle_cut(match job.cut {
        aterm_ctl::DriftCut::Quiescent => Cut::Quiescent,
        aterm_ctl::DriftCut::Racy => Cut::Racy,
        aterm_ctl::DriftCut::File => Cut::File,
    });
    Ok(report.render(ms, Some("client")))
}

const BUILD_STAMP: aterm_types::version::BuildStamp = aterm_types::version::BuildStamp {
    app_version: env!("CARGO_PKG_VERSION"),
    git_commit: env!("ATERM_GIT_COMMIT"),
    git_commit_full: env!("ATERM_GIT_COMMIT_FULL"),
    git_dirty: env!("ATERM_GIT_DIRTY"),
    build_time: env!("ATERM_BUILD_TIME"),
    build_number: env!("ATERM_BUILD_NUMBER"),
    dev_commits: env!("ATERM_DEV_COMMITS"),
    compiler_version_line: env!("ATERM_COMPILER_VERSION_LINE"),
    compiler_commit: env!("ATERM_COMPILER_COMMIT"),
    compiler_host: env!("ATERM_COMPILER_HOST"),
    compiler_trust_version: env!("ATERM_COMPILER_TRUST_VERSION"),
    compiler_flavor: env!("ATERM_COMPILER_FLAVOR"),
    build_profile: env!("ATERM_BUILD_PROFILE"),
    trust_verify: env!("ATERM_TRUST_VERIFY"),
    release_build: option_env!("ATERM_RELEASE_BUILD").is_some(),
};

/// The executable's inherited-lane ownership admission. The Link library parses first and invokes
/// this provider only for inherited `serve`; observer and unrelated verbs never take descriptors
/// 3/4.
#[cfg(unix)]
fn dispatch_link(args: &[String]) -> ExitCode {
    /// The startup take of the launcher's two bridge lanes: the one unsafe call this executable
    /// makes, whose `# Safety` contract (exclusive transfer of descriptors 3/4, no concurrent
    /// close or replacement) only this startup route can keep.
    #[cfg_attr(trust_verify, trust::skip)]
    fn take_startup_lanes() -> std::io::Result<aterm_link::bridge::InheritedLanes> {
        // SAFETY: the launcher transfers eligible lanes exclusively to this startup route, and
        // `dispatch_link` is the only route that reaches this function. No other owner retains
        // them, and no thread or callback closes or replaces either number during adoption.
        // Missing or ineligible lanes are checked refusals; the once-only guard alone would not
        // establish ownership.
        let (verb, push) = unsafe { aterm_uds::spawnfd::adopt_startup_bridge_lanes() }?;
        Ok(aterm_link::bridge::InheritedLanes { verb, push })
    }
    aterm_link::cli::dispatch_with_inherited_lanes(args, take_startup_lanes)
}

pub(crate) fn main() -> ExitCode {
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    aterm_types::version::install(BUILD_STAMP);
    aterm_gui::mark_rust_main_start();
    #[cfg(windows)]
    let _ = aterm_gui::attach_parent_console();
    let mut args_os = std::env::args_os();
    let argv0_os: Option<OsString> = args_os.next();
    let rest: Vec<OsString> = args_os.collect();

    let argv0 = argv0_os
        .as_ref()
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
        AliasRoute::Ctl => return aterm_ctl::main_entry_with(rest, CTL_LOCAL),
        AliasRoute::Askpass => return aterm_ctl::askpass_main_entry(rest),
        #[cfg(target_os = "macos")]
        AliasRoute::MachinesAskpass => return aterm_machines::askpass::main(&rest),
        #[cfg(not(target_os = "macos"))]
        AliasRoute::MachinesAskpass => return machines_unavailable(),
        AliasRoute::Pkg => return atpkg::cli::main_entry(rest),
        AliasRoute::Abrew => return atpkg::abrew::main_entry(rest),
        AliasRoute::Agh => return agh::cli::main_entry(rest),
        AliasRoute::Fleet => return aterm_agent::fleet_cli::main_entry(rest),
        AliasRoute::Drive => return aterm_agent::drive_cli::main_entry(rest),
        AliasRoute::OpenUrl => return aterm_agent::harness::open_url::main_entry(rest),
        AliasRoute::Amacs => return aterm_cli::amacs_launch::run(rest),
        AliasRoute::Link => {
            #[cfg(unix)]
            return dispatch_link(
                &rest
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            );
            #[cfg(not(unix))]
            return link_unavailable("link");
        }
        AliasRoute::AliasWindowVerb => {
            return window_verb(
                &first,
                rest.get(1..).unwrap_or(&[]),
                WindowHost::ThisProcess,
            );
        }
        AliasRoute::AliasWindow => return gui_alias_entry(rest),
        AliasRoute::FrontDoor => {}
    }

    if first == aterm_containment::consent::OPEN_PROBE_VERB {
        return aterm_containment::consent::run_open_probe_verb(rest.get(1..).unwrap_or(&[]));
    }

    #[cfg(unix)]
    if first == "hook" && rest.get(1).is_some_and(|a| a == "run") {
        return dispatch_link(
            &rest
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<String>>(),
        );
    }

    if let Some(verb) = aterm_cli::Verb::from_operand(&first) {
        let forwarded = rest.get(1..).unwrap_or(&[]).to_vec();
        return match verb {
            aterm_cli::Verb::Ctl => aterm_ctl::main_entry_with(forwarded, CTL_LOCAL),
            aterm_cli::Verb::Conn => aterm_ctl::conn_main_entry(forwarded),
            aterm_cli::Verb::ResourceRun => aterm_ctl::resource_run_main_entry(forwarded),
            aterm_cli::Verb::Pkg => {
                #[cfg(target_os = "macos")]
                if aterm_machines::cli::pkg_retires(&forwarded) {
                    aterm_machines::cli::retire_lan_agent();
                }
                atpkg::cli::main_entry(forwarded)
            }
            aterm_cli::Verb::Agh => agh::cli::main_entry(forwarded),
            aterm_cli::Verb::Fleet => aterm_agent::fleet_cli::main_entry(forwarded),
            aterm_cli::Verb::Drive => aterm_agent::drive_cli::main_entry(forwarded),
            #[cfg(unix)]
            aterm_cli::Verb::Link => dispatch_link(
                &forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            ),
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
            aterm_cli::Verb::Ship => {
                let args: Vec<String> = forwarded
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                ExitCode::from(u8::try_from(aterm_gui::run_ship(&args)).unwrap_or(2))
            }
            aterm_cli::Verb::Update => update_verb(&forwarded),
            aterm_cli::Verb::Harness => aterm_agent::harness::cli::main_entry(forwarded),
            aterm_cli::Verb::Ai => aterm_agent::ai::cli::main_entry(forwarded),
            #[cfg(target_os = "macos")]
            aterm_cli::Verb::Machines => aterm_machines::cli::main_entry(forwarded),
            #[cfg(not(target_os = "macos"))]
            aterm_cli::Verb::Machines => machines_unavailable(),
            #[cfg(target_os = "macos")]
            aterm_cli::Verb::Init => aterm_machines::init::main_entry(
                forwarded,
                &doctor::init_base,
                &aterm_cli::macos_doctor::full_disk_access,
            ),
            #[cfg(not(target_os = "macos"))]
            aterm_cli::Verb::Init => {
                eprintln!("aterm init: local macOS setup is available on macOS only");
                ExitCode::from(2)
            }
            #[cfg(unix)]
            aterm_cli::Verb::Secrets => {
                let first = forwarded
                    .first()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned);
                let code = aterm_secrets::cli::main_entry(forwarded);
                if code == ExitCode::SUCCESS
                    && matches!(
                        first.as_deref(),
                        Some("join" | "sync" | "set" | "init" | "create" | "edit")
                    )
                    && let Ok(env) = aterm_agent::harness::cli::Env::from_process()
                    && let (Some(home), Some(roster_path)) =
                        (env.home.as_deref(), env.accounts_roster())
                {
                    let _ = aterm_agent::harness::accounts_sync::sync_accounts_for_init(
                        home,
                        &env.state,
                        &roster_path,
                        env.now,
                    );
                    let _ = aterm_agent::harness::accounts_sync::sync_cli_accounts_for_init(
                        home,
                        &env.state,
                        &roster_path,
                        env.now,
                    );
                }
                code
            }
            #[cfg(not(unix))]
            aterm_cli::Verb::Secrets => secrets_unavailable(),
            aterm_cli::Verb::Keeper => {
                aterm_keeper::cli::main_entry(forwarded, aterm_types::version::app_version())
            }
            aterm_cli::Verb::Caffeine => {
                aterm_gui::install_session_log();
                aterm_caffeine::cli::main_entry(forwarded, aterm_types::version::app_version())
            }
            aterm_cli::Verb::Agents => {
                let _ = aterm_cli::parse_args(rest);
                ExitCode::SUCCESS
            }
            aterm_cli::Verb::Theme => aterm_cli::theme_main_entry(forwarded, &WindowThemes),
            aterm_cli::Verb::Edit => aterm_cli::amacs_launch::run(forwarded),
            aterm_cli::Verb::NewTab | aterm_cli::Verb::NewWindow | aterm_cli::Verb::SplitPane => {
                window_verb(verb.name(), &forwarded, WindowHost::WindowedSibling)
            }
        };
    }

    if let Some(theme_args) = aterm_cli::list_themes_alias(&first, rest.get(1..).unwrap_or(&[])) {
        return aterm_cli::theme_main_entry(theme_args, &WindowThemes);
    }

    if aterm_cli::asks_completions(&first) {
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

    if routes_to_doctor(&first, rest.get(1..).unwrap_or(&[])) {
        return doctor::main_entry(rest.get(1..).unwrap_or(&[]));
    }

    if aterm_cli::mode_free(&first) {
        let _ = aterm_cli::parse_args(rest);
        return ExitCode::SUCCESS;
    }

    if first == "amacs" {
        return aterm_cli::amacs_launch::run(rest.get(1..).unwrap_or(&[]).to_vec());
    }

    if aterm_cli::is_tool_candidate(Some(first.as_str())) && store_resolves(&first) {
        // `run <tool> -- <args…>`: `pkg run` strips exactly one leading `--`, so the tool's
        // own flags are never parsed by it and a user's `--` survives. `split_first`, not
        // `rest[0]`/`rest[1..]`: the verifier refuted both bounds checks.
        if let Some((tool, tool_args)) = rest.split_first() {
            let mut run_args: Vec<OsString> = vec![OsString::from("run"), tool.clone()];
            run_args.push(OsString::from("--"));
            run_args.extend(tool_args.iter().cloned());
            return atpkg::cli::main_entry(run_args);
        }
    }

    if aterm_cli::is_tool_candidate(Some(first.as_str())) && pending_stub_resolves(&first) {
        let Some((tool, tool_args)) = rest.split_first() else {
            return ExitCode::from(2);
        };
        let mut pending_args: Vec<OsString> = vec![OsString::from("__pending"), tool.clone()];
        pending_args.extend(tool_args.iter().cloned());
        return atpkg::cli::main_entry(pending_args);
    }

    if aterm_cli::is_tool_candidate(Some(first.as_str()))
        && (atpkg::stub::describe(&first).is_some() || atpkg::stub::program_of(&first) != first)
    {
        let Some((tool, tool_args)) = rest.split_first() else {
            return ExitCode::from(2);
        };
        let mut run_args: Vec<OsString> = vec![OsString::from("run"), tool.clone()];
        run_args.push(OsString::from("--"));
        run_args.extend(tool_args.iter().cloned());
        return atpkg::cli::main_entry(run_args);
    }

    let scan = rest.get(..payload_boundary(&rest)).unwrap_or(&rest);
    let force_session =
        argv0 == "aterm-cli" || scan.iter().any(|a| a.to_string_lossy() == "--session");
    let windowish = is_windowish(scan, force_session, stdin_is_terminal());
    let mode_args = take_no_reroute(&strip_mode_flags(&rest));
    if windowish {
        let spawn = match plain_launch_policy(&rest) {
            ControlFlow::Break(code) => return code,
            ControlFlow::Continue(spawn) => spawn,
        };
        let handoff = match &spawn {
            Some(request) => spawn_handoff_argv(request),
            None => rest.clone(),
        };
        return run_window(mode_args, window_host_for(scan), &handoff);
    }

    #[cfg(windows)]
    if nests_inside_aterm(
        rest.is_empty(),
        force_session,
        std::env::var_os("ATERM_CHILD").is_some(),
        stdin_is_terminal(),
    ) {
        return nested_new_tab();
    }

    let quiet = aterm_cli::parse_args(mode_args);

    aterm_gui::install_session_log();
    atpkg::notice::to_host_log();
    let packages = atpkg::config::cached();
    let layout = atpkg::store::resolve_from(packages);

    hand_agents_dir(layout.as_ref());
    if let Some(bin) = layout.as_ref().and_then(|layout| {
        atpkg::session_bin_dir(layout, packages, aterm_types::dirs::home_dir().as_deref())
    }) {
        aterm_log::env::set(
            "PATH",
            atpkg::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &bin),
        );
    }

    if !atpkg::reroute::engaged(
        std::env::var(atpkg::reroute::PASSTHROUGH_ENV)
            .ok()
            .as_deref(),
    ) && let Some(layout) = layout.clone()
    {
        let dir = layout.reroute_dir();
        if let Err(error) = layout.ensure_dir(&dir) {
            aterm_log::warn!(
                "aterm: reroute dir not created ({error}); this session does not reroute the upstream toolchain names (cargo, rustc, z3, …) — `aterm pkg doctor` explains (aterm help reroute)"
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
                        "aterm: reroute stubs not laid ({error}); this session does not reroute the upstream toolchain names (cargo, rustc, z3, …) — `aterm pkg doctor` explains (aterm help reroute)"
                    );
                }
            });
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if session_lane_is_interactive() {
        let build = aterm_gui::running_build_number();
        #[cfg(target_os = "linux")]
        start_linux_session_trial(build);
        aterm_update::spawn_background_check_with_settings(
            build,
            std::sync::Arc::new(aterm_gui::configured_update_settings),
            None,
            None,
            None,
        );
    }

    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    if cfg!(target_os = "macos")
        && session_lane_is_interactive()
        && let Some(layout) = layout.as_ref()
        && atpkg::machine::launch_apply_due(layout, now).runs(log_launch_apply_stood_aside)
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

    if let Some(layout) = layout.as_ref()
        && packages.enabled()
        && session_lane_is_interactive()
    {
        start_session_head_watch(layout, packages);
    }

    #[cfg(target_os = "macos")]
    if session_lane_is_interactive() {
        let _ = std::thread::Builder::new()
            .name("aterm-machines-retire".into())
            .spawn(aterm_machines::cli::retire_lan_agent);
    }

    session_lane(quiet)
}

/// A terminal session's part in a replaced Linux executable's trial: count this start
/// ([`aterm_update::linux_session_started`], before the shell spawns, so a session that dies
/// starting counts) and, for a start of the pending trial, confirm it
/// ([`aterm_update::confirm_linux_session`]) once the shell's first output reached the
/// terminal ([`aterm_cli::on_first_shell_output`]).
#[cfg(target_os = "linux")]
fn start_linux_session_trial(build: u64) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static SPOKE: AtomicBool = AtomicBool::new(false);
    static CONFIRMER: OnceLock<std::thread::Thread> = OnceLock::new();
    let commit = aterm_gui::running_binary_identity().commit;
    if !aterm_update::linux_session_started(build, &commit) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-linux-trial".into())
        .spawn(move || {
            defer_the_verdict_while_silent(&SPOKE, build, &commit);
            park_until_the_shell_spoke(&SPOKE);
            for attempt in 1..=3 {
                match aterm_update::confirm_linux_session(build, &commit) {
                    aterm_update::LinuxSessionConfirm::Done => {
                        aterm_log::info!(
                            "aterm-update: this session confirmed the installed update"
                        );
                        return;
                    }
                    aterm_update::LinuxSessionConfirm::Failed => {}
                }
                if attempt < 3 {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            aterm_log::warn!(
                "aterm-update: this session could not confirm the installed update; the next \
                 start of aterm tries again"
            );
        });
    match spawned {
        Ok(handle) => {
            let _ = CONFIRMER.set(handle.thread().clone());
            aterm_cli::on_first_shell_output(|| {
                SPOKE.store(true, Ordering::Release);
                if let Some(confirmer) = CONFIRMER.get() {
                    confirmer.unpark();
                }
            });
        }
        Err(error) => aterm_log::warn!(
            "aterm-update: the installed update's confirmation could not start: {error}"
        ),
    }
}

/// Park until `spoke` says the shell has printed (the [`aterm_cli::on_first_shell_output`] hook
/// sets it and unparks this thread), with no deadline: a session that never prints proves
/// nothing.
#[cfg(target_os = "linux")]
fn park_until_the_shell_spoke(spoke: &std::sync::atomic::AtomicBool) {
    while !spoke.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::park();
    }
}

/// A silent start still up defers the trial's verdict, once: with no shell output within
/// [`aterm_update::LINUX_SESSION_SILENT_DEFER`], ask the update lane to move the trial's stamp
/// ([`aterm_update::defer_linux_session_verdict`]), retried while the shell stays silent.
#[cfg(target_os = "linux")]
fn defer_the_verdict_while_silent(spoke: &std::sync::atomic::AtomicBool, build: u64, commit: &str) {
    if park_until_spoke_or_silent_for(spoke, aterm_update::LINUX_SESSION_SILENT_DEFER) {
        return;
    }
    if aterm_update::defer_linux_session_verdict(build, commit, |pause| {
        !park_until_spoke_or_silent_for(spoke, pause)
    }) {
        aterm_log::info!(
            "aterm-update: this session's shell has printed nothing yet; the installed \
             update's trial waits one more window before its verdict"
        );
    }
}

/// Park the calling thread until `spoke` says the shell has printed, or until `silent`
/// has passed with nothing: `true` when it spoke.
#[cfg(target_os = "linux")]
fn park_until_spoke_or_silent_for(
    spoke: &std::sync::atomic::AtomicBool,
    silent: std::time::Duration,
) -> bool {
    let until = std::time::Instant::now() + silent;
    loop {
        if spoke.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::park_timeout(left);
    }
}

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
        Some(Ok(_running)) => {}
        Some(Err(error)) => {
            aterm_log::warn!("aterm: could not start the vendor head watch: {error}");
        }
        None => {}
    }
}

const SESSION_REACH: &str = "this session";

fn agents_dir_handoff(layout: Option<&atpkg::store::Layout>) -> Option<String> {
    let layout = layout?;
    if !layout.prefix.is_absolute() {
        eprintln!(
            "aterm: the managed agent CLIs (`claude`, `codex`, …) are not first on PATH: package \
             prefix {} is not absolute",
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

fn hand_agents_dir(layout: Option<&atpkg::store::Layout>) {
    match agents_dir_handoff(layout) {
        Some(dir) => aterm_log::env::set(atpkg::reroute::AGENTS_DIR_ENV, dir),
        None => aterm_log::env::unset(atpkg::reroute::AGENTS_DIR_ENV),
    }
}

fn session_lane_is_interactive() -> bool {
    stdin_is_terminal()
}

fn session_pass_due(layout: &atpkg::store::Layout, now_unix: i64) -> bool {
    use aterm_update_core::pkg_check;
    pkg_check::full_pass_owed(&atpkg::status::pass_stamps(layout, now_unix), now_unix).is_some()
        && pkg_check::claim(
            &layout.session_pass_stamp(),
            now_unix,
            pkg_check::PASS_SPACING_SECS,
        )
}

fn log_launch_apply_stood_aside(why: &'static str) {
    aterm_log::info!("{why}");
}

fn spawn_detached_machine_apply() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args = ["pkg", "machine", "apply"].map(std::ffi::OsString::from);
    if let Err(error) = atpkg::platform::spawn_detached(exe.as_os_str(), &args, None) {
        aterm_log::warn!(
            "aterm: could not start the background `aterm pkg machine apply`: {error}"
        );
    }
}

fn spawn_detached_pkg_update(layout: &atpkg::store::Layout) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let detached = (atpkg::cli::SPAWNER_PID_ENV, atpkg::cli::SPAWNER_DETACHED);
    let spawn = if atpkg::adoption::first_install_ahead(layout) {
        atpkg::platform::spawn_detached_waited
    } else {
        atpkg::platform::spawn_detached
    };
    if let Err(error) = spawn(exe.as_os_str(), &session_pass_args(layout), Some(detached)) {
        aterm_log::warn!("aterm: could not start the background `aterm pkg update` pass: {error}");
    }
}

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

/// The session route as a function that returns an `ExitCode`, like every other route.
#[inline(never)]
fn session_lane(quiet: bool) -> ExitCode {
    aterm_cli::session_main(quiet)
}

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

/// Which process serves a window the front door decided to open: off Windows always this one;
/// on Windows it depends on the running image and on what was asked ([`window_host_for`]).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum WindowHost {
    /// Run the window here: in the windowed image (the `aterm-gui`/`aterm-windowed` aliases),
    /// and for a headless- or diagnose-shaped launch, whose output the release gates and
    /// harnesses read through their pipes.
    ThisProcess,
    /// The console image on Windows, asked for a real window: check the window's arguments
    /// here, start the windowed sibling detached and return at once, so the prompt comes back
    /// ([`run_window`]).
    WindowedSibling,
}

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

fn run_window(args: Vec<OsString>, host: WindowHost, handoff: &[OsString]) -> ExitCode {
    #[cfg(windows)]
    if host == WindowHost::WindowedSibling {
        aterm_gui::check_window_args(args.clone());
        match windowed_sibling::launch(handoff) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(why) => {
                eprintln!(
                    "aterm: {why}; the window opens in this console, which returns when it closes"
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (host, handoff);
    let first_run = wire_window(&args);
    #[expect(
        clippy::disallowed_methods,
        reason = "the one window door: every window route reaches the GUI here"
    )]
    aterm_gui::main_entry_with_first_run(args, first_run);
    ExitCode::SUCCESS
}

/// What the window's process wires before the GUI starts: on macOS the LAN agent's retirement
/// (windowed launches only, never `--headless` or `--diagnose`), the setup worker's signals
/// ([`watch_setup_signals`]) and System State's handlers, and everywhere the resource runner.
/// Returns the first-run setup action where this window may run it, for [`run_window`] to hand
/// the GUI; a test drives the same wiring.
fn wire_window(args: &[OsString]) -> Option<fn() -> bool> {
    #[cfg(target_os = "macos")]
    if !args
        .iter()
        .any(|a| matches!(a.to_string_lossy().as_ref(), "--headless" | "--diagnose"))
    {
        let _ = std::thread::Builder::new()
            .name("aterm-machines-retire".into())
            .spawn(aterm_machines::cli::retire_lan_agent);
    }
    #[cfg(target_os = "macos")]
    let first_run = {
        let restoring_window = std::env::var_os(aterm_update::RELAUNCH_OF_RUNNING_ENV).is_some();
        let setup_args = first_run_window_args(args, restoring_window);
        first_run_window_eligible(setup_args, doctor_startup_env())
            .then_some(apply_first_run_doctor as fn() -> bool)
    };
    #[cfg(not(target_os = "macos"))]
    let first_run = {
        let _ = args;
        None
    };
    #[cfg(target_os = "macos")]
    watch_setup_signals();
    #[cfg(target_os = "macos")]
    aterm_gui::set_initial_setup_handlers(aterm_gui::InitialSetupHandlers {
        apply: restart_initial_setup,
        check: check_initial_setup,
        restore: restore_initial_setup,
        status: initial_setup_status,
        observe: observe_initial_setup,
        stop: || {
            stop_initial_setup();
        },
        settle: settle_initial_setup,
        sync_on: turn_sync_on,
    });
    aterm_gui::register_resource_runner();
    first_run
}

/// What keeps a launch from first-run setup, which belongs to a window started by the person at
/// this Mac: a private state root (a laboratory's) or CI, a scope outside the person's, or ssh.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default)]
struct DoctorStartupEnv {
    isolated: bool,
    outside_person_scope: bool,
    /// Started from an ssh login: the person is at another machine, so setup waits for a window
    /// started at this Mac.
    ssh: bool,
}

#[cfg(target_os = "macos")]
fn doctor_startup_env() -> DoctorStartupEnv {
    DoctorStartupEnv {
        isolated: std::env::var_os("ATERM_STATE_HOME").is_some()
            || std::env::var_os("CI").is_some(),
        outside_person_scope: aterm_cli::macos_doctor::lasting_scope_refusal().is_some(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
    }
}

/// A new GUI window has its own setup surface even when it inherits markers from the shell that
/// opened it, or from its update predecessor.
#[cfg(any(target_os = "macos", test))]
fn first_run_window_eligible(args: &[OsString], env: DoctorStartupEnv) -> bool {
    if env.isolated || env.outside_person_scope || env.ssh {
        return false;
    }
    let args = strip_mode_flags(args);
    let args = strip_flags(&args, &[NO_REROUTE_FLAG]);
    aterm_gui::initial_setup_window_args(&args)
}

/// A running-window successor does not execute its carried command, but its launch flags still
/// describe its scope: a relaunch hint cannot erase a private socket or another ineligible prefix.
#[cfg(any(target_os = "macos", test))]
fn first_run_window_args(args: &[OsString], restoring_window: bool) -> &[OsString] {
    if restoring_window {
        &args[..payload_boundary(args)]
    } else {
        args
    }
}

#[cfg(target_os = "macos")]
fn apply_first_run_doctor() -> bool {
    match std::thread::Builder::new()
        .name("aterm-workstation-setup".into())
        .spawn(|| {
            // A launch that finds the ALab signal widens the plan aterm chose by itself on an
            // earlier one (System State design §3.5); a plan a person shaped, stopped, restored
            // or (on an older aterm) cancelled stays as it is.
            if let Err(why) = aterm_cli::macos_doctor::upgrade_provisional_setup() {
                aterm_log::warn!(
                    "first-run macOS doctor: the automatic plan upgrade did not finish: {why}"
                );
            }
            let mut password = None;
            let (prepared, sent) = prepare_first_run_doctor(|initial, on_begin| {
                let mut answer = aterm_gui::await_initial_setup_answer_while(
                    initial,
                    || {
                        startup_offer_withdrawn(|| {
                            aterm_cli::macos_doctor::initial_setup_finished().unwrap_or(false)
                        })
                    },
                    on_begin,
                );
                password = answer.password.take();
                answer
            });
            run_prepared_setup(prepared, password, sent);
        }) {
        Ok(_) => true,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor worker could not start: {why}");
            false
        }
    }
}

/// Whether the startup's offer, still waiting for the page's answer, is withdrawn: a person's
/// choice made meanwhile waits for the claim the startup holds (it answers for this launch,
/// design §3.3), or setup ended elsewhere (`finished`).
#[cfg(target_os = "macos")]
fn startup_offer_withdrawn(finished: impl FnOnce() -> bool) -> bool {
    choice_awaits_claim() || finished()
}

/// Reserve one offer across app processes before showing it; `offer` shows the plan on System
/// State and waits for the page's answer. Account probes, native permission prompts and
/// preference writes follow the page's admission of the plan.
#[cfg(target_os = "macos")]
fn prepare_first_run_doctor(
    offer: impl FnMut(aterm_types::macos_setup::Plan, StartupHandoff) -> aterm_gui::SetupAnswer,
) -> (PreparedSetup<StartupClaim>, StartupSent) {
    let mut host = LiveSetupStartup {
        offer,
        receipt: None,
        authorization: None,
        last_failure: None,
        observation: None,
        offered: None,
        chosen: false,
        vault_typed: false,
        sent: StartupSent::default(),
    };
    let prepared = match prepare_setup(&mut host) {
        Ok(prepared) => prepared,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor could not start: {why}");
            PreparedSetup::Unavailable
        }
    };
    (prepared, host.sent)
}

/// The startup's admission as Stop and a quit know it: its number ([`SETUP_SENT`]), taken as the
/// startup begins (for a resumed plan) and again as it offers its plan, before the page can show
/// it; and, once the page has answered, the admission under way ([`UnderWay`]) until its run
/// registers. The request shares its guard slot, so the window records that handoff
/// synchronously even when the worker has not read the answer yet.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct StartupSent {
    sent: u64,
    under_way: std::sync::Arc<std::sync::Mutex<Option<UnderWay>>>,
}

#[cfg(target_os = "macos")]
type StartupHandoff = Box<dyn FnOnce() + Send>;

#[cfg(target_os = "macos")]
impl StartupSent {
    /// Number the admission the startup sends now: a resume, or an offer.
    fn send(&mut self) {
        self.under_way = Default::default();
        self.sent = send_admission();
    }

    /// The worker read the page's answer. Idempotent with the window's handoff.
    fn answered(&mut self) {
        self.handoff()();
    }

    /// Sent with the request: a successful Begin delivery invokes this before
    /// the window publishes busy or can receive Stop. If the worker already
    /// registered its run, dropping the callback drops its last guard owner.
    fn handoff(&self) -> StartupHandoff {
        let sent = self.sent;
        let under_way = self.under_way.clone();
        Box::new(move || {
            let mut under_way = under_way
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if under_way.is_none() {
                *under_way = Some(UnderWay::new(sent));
            }
        })
    }
}

/// The startup coordinator reads only aterm's state until an offer authorizes
/// setup.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct SetupAdmission {
    revision: u64,
    completed: bool,
}

#[cfg(any(target_os = "macos", test))]
trait SetupStartup {
    type Claim;

    /// Number the admission the startup sends now (`SETUP_SENT`), so a Stop pressed after it
    /// covers the run it admits (`StartupSent`).
    fn send(&mut self) {}
    /// The page answered the offer: the admission is under way (`UnderWay`)
    /// until its run registers (`run_prepared_setup`).
    fn answered(&mut self) {}
    fn bind_authorization(&mut self) -> Result<u64, String>;
    fn revision(&mut self) -> Result<u64, String>;
    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    fn finished(&mut self) -> Result<bool, String>;
    /// Whether a person's choice made in this process waits for the claim
    /// this coordinator holds (`run_setup_restart`).
    fn choice_awaits_claim(&mut self) -> bool {
        false
    }
    fn admission(&mut self) -> Result<Option<SetupAdmission>, String>;
    fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String>;
    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision;
    fn validate(&mut self, _: &aterm_types::macos_setup::Plan) -> Result<(), String> {
        Ok(())
    }
    fn authorize(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String>;
    fn authorization_pending(&self) -> bool;
    /// Wait before writing the page's answer again, holding the claim. The
    /// wait ends the moment a person's choice made in this process waits for
    /// that claim (`run_setup_restart`), or already does: `true` then, and the
    /// claim goes to the choice. `false` once the wait ran out without one.
    fn wait_holding_claim(&mut self) -> bool;
    /// Wait before trying again, holding no claim.
    fn backoff(&mut self);
    fn report_failure(&mut self, why: &str);
    fn report_rejection(&mut self, why: &str) {
        self.report_failure(why);
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupStartup<F> {
    /// Shows the plan on System State and waits for the page's answer.
    offer: F,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    authorization: Option<aterm_cli::macos_doctor::InitialSetupAuthorization>,
    last_failure: Option<String>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
    /// The plan this launch offered and whether aterm chose it by itself: admitted
    /// unchanged, it is saved as provisional (System State design §3.5).
    offered: Option<(aterm_types::macos_setup::Plan, bool)>,
    /// A group choice or Join makes even an unchanged offered plan the chooser's.
    chosen: bool,
    /// An explicit Join binds the vault current at admission, rather than the offer's.
    vault_typed: bool,
    /// This launch's admission as Stop and a quit know it.
    sent: StartupSent,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupAnswerKind {
    Automatic,
    Person,
    Join,
}

#[cfg(target_os = "macos")]
impl<F> LiveSetupStartup<F> {
    fn answer_kind(&self, plan: &aterm_types::macos_setup::Plan) -> StartupAnswerKind {
        if self.vault_typed {
            StartupAnswerKind::Join
        } else if !self.chosen
            && self
                .offered
                .as_ref()
                .is_some_and(|(offered, provisional)| *provisional && offered == plan)
        {
            StartupAnswerKind::Automatic
        } else {
            StartupAnswerKind::Person
        }
    }
}

#[cfg(target_os = "macos")]
impl<F: FnMut(aterm_types::macos_setup::Plan, StartupHandoff) -> aterm_gui::SetupAnswer>
    SetupStartup for LiveSetupStartup<F>
{
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }

    fn answered(&mut self) {
        self.sent.answered();
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        let authorization = aterm_cli::macos_doctor::InitialSetupAuthorization::capture()?;
        let revision = authorization.revision();
        self.authorization = Some(authorization);
        Ok(revision)
    }

    fn revision(&mut self) -> Result<u64, String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::initial_setup_revision()
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        StartupClaim::claim()
    }

    fn finished(&mut self) -> Result<bool, String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::initial_setup_finished()
    }

    fn choice_awaits_claim(&mut self) -> bool {
        choice_awaits_claim()
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        aterm_cli::macos_doctor::initial_setup_admission().map(|admission| {
            admission.map(|admission| SetupAdmission {
                revision: admission.revision,
                completed: admission.completed,
            })
        })
    }

    fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        // Scoped to this Mac: the whole profile on an ALab Mac, aterm's own
        // permissions elsewhere; its probe of the ALab vault runs on this worker.
        let (plan, provisional) = self
            .authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?
            .offer_scoped()?;
        self.offered = Some((plan.clone(), provisional));
        Ok(plan)
    }

    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision {
        let mut answer = (self.offer)(initial, self.sent.handoff());
        self.receipt = answer.receipt.take();
        self.chosen = answer.chosen;
        self.vault_typed = answer.vault_typed;
        answer.decision
    }

    fn validate(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<(), String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::validate_initial_setup_plan(plan)
    }

    fn authorize(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        let kind = self.answer_kind(plan);
        let authorization = self
            .authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?;
        let observed = match kind {
            StartupAnswerKind::Automatic => authorization.persist_automatic_observed(plan),
            StartupAnswerKind::Person => authorization.persist_observed(plan),
            StartupAnswerKind::Join => authorization.persist_join_observed(plan),
        };
        self.observation = observed.observation;
        let revision = observed.result?;
        if revision.is_some() {
            clear_initial_setup_rows();
            if let Some(receipt) = self.receipt.take() {
                receipt.accept();
            }
        }
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        Ok(revision)
    }

    fn authorization_pending(&self) -> bool {
        self.authorization
            .as_ref()
            .is_some_and(aterm_cli::macos_doctor::InitialSetupAuthorization::pending)
    }

    fn wait_holding_claim(&mut self) -> bool {
        await_waiting_choice(SETUP_RETRY_WAIT)
    }

    fn backoff(&mut self) {
        std::thread::sleep(SETUP_RETRY_WAIT);
    }

    fn report_failure(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }

    fn report_rejection(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
        aterm_log::warn!("initial setup review was not accepted: {why}");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod startup_answer_tests {
    use super::*;
    use aterm_types::macos_setup::Plan;

    fn answered(
        chosen: bool,
        vault_typed: bool,
    ) -> LiveSetupStartup<impl FnMut(Plan, StartupHandoff) -> aterm_gui::SetupAnswer> {
        let plan = Plan::default();
        let mut host = LiveSetupStartup {
            offer: move |plan, _| aterm_gui::SetupAnswer {
                chosen,
                vault_typed,
                ..aterm_gui::WorkstationSetupDecision::Begin(plan).into()
            },
            receipt: None,
            authorization: None,
            last_failure: None,
            observation: None,
            offered: Some((plan.clone(), true)),
            chosen: false,
            vault_typed: false,
            sent: StartupSent::default(),
        };
        assert_eq!(
            host.offer(plan.clone()),
            aterm_gui::WorkstationSetupDecision::Begin(plan)
        );
        host
    }

    #[test]
    fn a_same_plan_startup_choice_is_not_a_provisional_automatic_answer() {
        assert_eq!(
            answered(false, false).answer_kind(&Plan::default()),
            StartupAnswerKind::Automatic
        );
        assert_eq!(
            answered(true, false).answer_kind(&Plan::default()),
            StartupAnswerKind::Person
        );
    }

    #[test]
    fn a_startup_join_uses_the_explicit_admission_path() {
        assert_eq!(
            answered(true, true).answer_kind(&Plan::default()),
            StartupAnswerKind::Join
        );
    }
}

#[cfg(any(target_os = "macos", test))]
enum PreparedSetup<C> {
    Authorized { claim: C, revision: u64 },
    Finished,
    Unavailable,
}

/// Retry a step of state I/O on the window's background worker, reporting each failure and
/// backing off, until it succeeds.
#[cfg(any(target_os = "macos", test))]
fn retry_setup_step<H: SetupStartup, T>(
    host: &mut H,
    mut step: impl FnMut(&mut H) -> Result<T, String>,
) -> T {
    loop {
        match step(host) {
            Ok(value) => return value,
            Err(why) => {
                host.report_failure(&why);
                host.backoff();
            }
        }
    }
}

/// Offer the plan and keep the page's answer across transient failures. An unavailable window
/// gives up its claim before waiting, so another app process, or a reopened window here, can
/// offer setup instead.
#[cfg(any(target_os = "macos", test))]
fn prepare_setup<H: SetupStartup>(host: &mut H) -> Result<PreparedSetup<H::Claim>, String> {
    use aterm_gui::WorkstationSetupDecision;
    host.send();
    let revision = host
        .bind_authorization()
        .inspect_err(|why| host.report_failure(why))?;
    enum BeforeOffer {
        Initial(aterm_types::macos_setup::Plan),
        Resume(u64),
        Finished,
        Superseded,
    }
    loop {
        let Some(claim) = retry_setup_step(host, H::claim) else {
            if retry_setup_step(host, H::finished) {
                return Ok(PreparedSetup::Finished);
            }
            host.backoff();
            continue;
        };
        let ready = (|| {
            if host.revision()? != revision {
                return Ok(BeforeOffer::Superseded);
            }
            if host.finished()? {
                return Ok(BeforeOffer::Finished);
            }
            if let Some(admission) = host.admission()? {
                if admission.revision != revision {
                    return Ok(BeforeOffer::Superseded);
                }
                return Ok(if admission.completed {
                    BeforeOffer::Finished
                } else {
                    BeforeOffer::Resume(admission.revision)
                });
            }
            host.initial().map(BeforeOffer::Initial)
        })();
        let initial = match ready {
            Ok(BeforeOffer::Initial(initial)) => initial,
            Ok(BeforeOffer::Resume(revision)) => {
                return Ok(PreparedSetup::Authorized { claim, revision });
            }
            Ok(BeforeOffer::Finished) => return Ok(PreparedSetup::Finished),
            Ok(BeforeOffer::Superseded) => return Ok(PreparedSetup::Unavailable),
            Err(why) => {
                drop(claim);
                host.report_failure(&why);
                host.backoff();
                continue;
            }
        };
        // A person chose on the page while this read the plan in force: their
        // choice answers for this launch, so it gets the claim, and no offer
        // is shown over it. If it is refused, the offer comes back.
        if host.choice_awaits_claim() {
            drop(claim);
            host.backoff();
            continue;
        }
        host.send();
        match host.offer(initial) {
            WorkstationSetupDecision::Begin(plan) => {
                host.answered();
                return Ok(finish_setup_choice(host, claim, plan));
            }
            WorkstationSetupDecision::Unavailable => {
                drop(claim);
                if retry_setup_step(host, H::finished) {
                    return Ok(PreparedSetup::Finished);
                }
                host.backoff();
            }
        }
    }
}

/// Once the page has answered, retain that plan and its claim through write failures without
/// asking again. A person's choice made meanwhile gets the claim the moment it waits for it,
/// before or during a wait, and its admission retires this review.
#[cfg(any(target_os = "macos", test))]
fn finish_setup_choice<H: SetupStartup>(
    host: &mut H,
    claim: H::Claim,
    plan: aterm_types::macos_setup::Plan,
) -> PreparedSetup<H::Claim> {
    if let Err(why) = host.validate(&plan) {
        host.report_rejection(&why);
        return PreparedSetup::Unavailable;
    }
    let mut claim = Some(claim);
    loop {
        let Some(held) = claim.take() else {
            match host.claim() {
                Ok(Some(held)) => claim = Some(held),
                Ok(None) if retry_setup_step(host, H::finished) => {
                    return PreparedSetup::Finished;
                }
                Ok(None) => host.backoff(),
                Err(why) => {
                    host.report_failure(&why);
                    host.backoff();
                }
            }
            continue;
        };
        match host.authorize(&plan) {
            Ok(None) => return PreparedSetup::Unavailable,
            // Empty authorization already durably commits completion.
            Ok(Some(_)) if plan.selection.is_empty() => return PreparedSetup::Finished,
            Ok(Some(revision)) => {
                return PreparedSetup::Authorized {
                    claim: held,
                    revision,
                };
            }
            Err(why) if !host.authorization_pending() => {
                host.report_rejection(&why);
                return PreparedSetup::Unavailable;
            }
            Err(why) => {
                host.report_failure(&why);
                if host.wait_holding_claim() {
                    drop(held);
                    host.backoff();
                } else {
                    claim = Some(held);
                }
            }
        }
    }
}

/// Run what the startup prepared, its admission `sent` as Stop and a quit know it
/// ([`prepare_first_run_doctor`]).
#[cfg(target_os = "macos")]
fn run_prepared_setup(
    prepared: PreparedSetup<StartupClaim>,
    password: Option<aterm_secrets::SecretBytes>,
    sent: StartupSent,
) {
    match prepared {
        PreparedSetup::Authorized { claim, revision } => {
            // Registered at once, while the claim makes this revision the one in force and
            // before the run waits for the slot: a Stop pressed from now, or while the page's
            // answer was being admitted, stops it. A quit waited for this.
            register_admitted_run(revision, sent.sent, false);
            drop(sent);
            // One lock order, the setup slot before the automatic claim: never wait for the slot
            // holding the claim, or a person's choice, which holds the slot while it waits for
            // the claim, never gets it. The retry loop claims again once it holds the slot.
            let (claim, busy) = match take_initial_setup_slot() {
                Some(busy) => (claim.into_run(), busy),
                None => {
                    drop(claim);
                    (None, wait_for_initial_setup_slot())
                }
            };
            aterm_gui::publish_initial_setup_status(initial_setup_status());
            (setup_io().run)(claim, revision, password, busy);
        }
        PreparedSetup::Finished => {
            aterm_gui::publish_initial_setup_status(initial_setup_status());
            finish_withheld_preferences();
        }
        PreparedSetup::Unavailable => {}
    }
}

/// At the launch of a window whose setup has finished, and once this aterm holds Full Disk
/// Access, clear what setup left alone in another app's container for want of it (Notification
/// Center's widgets), logging only. While it may hold the profile lock it holds System State's
/// busy slot, so the page offers no Apply, Check or Restore that would fail on that lock; a launch
/// with nothing owed reads only aterm's record and takes no slot. Without the grant it changes and
/// asks nothing; a try that did not verify is retried at a later launch once it is due.
#[cfg(target_os = "macos")]
fn finish_withheld_preferences() {
    match finish_withheld_preferences_with(aterm_cli::macos_doctor::finish_withheld_preferences) {
        Ok(Some(report)) if report.needs_attention() => aterm_log::warn!(
            "setup follow-up with Full Disk Access: {}",
            report.to_text()
        ),
        Ok(Some(report)) => aterm_log::info!(
            "setup follow-up with Full Disk Access: {}",
            report.to_text()
        ),
        Ok(None) => {}
        Err(why) => aterm_log::warn!("setup follow-up with Full Disk Access: {why}"),
    }
}

#[cfg(target_os = "macos")]
type WithheldReservation = (PreferenceReadings, InitialSetupBusy);

#[cfg(target_os = "macos")]
fn finish_withheld_preferences_with<T>(
    follow_up: impl FnOnce(fn() -> WithheldReservation) -> T,
) -> T {
    follow_up(|| {
        let busy = wait_for_initial_setup_slot();
        // Tuple fields drop in order: retire and refresh the widget reading
        // before another operation can take the setup slot.
        (
            PreferenceReadings::begin(vec![aterm_types::macos_setup::Subject::Setting(
                aterm_types::macos_setup::Setting::Widgets,
            )]),
            busy,
        )
    })
}

/// The background setup worker of a window or a session. It reports to the log
/// and the window's message center only, never stderr: in the session lane that
/// is the tab, and a thread writing there lands over whatever program owns it.
#[cfg(target_os = "macos")]
fn run_first_run_doctor(
    claim: Option<aterm_cli::macos_doctor::AutomaticSetupClaim>,
    revision: u64,
    password: Option<aterm_secrets::SecretBytes>,
    busy: InitialSetupBusy,
) {
    // A process that may never change the person's profile (its state root, uid
    // and HOME are fixed for its life) has nothing to retry, and an idle
    // session must not wake each minute to learn that again.
    if let Some(why) = aterm_cli::macos_doctor::lasting_scope_refusal() {
        aterm_log::info!("first-run macOS doctor: skipped: {why}");
        clear_setup_run(revision);
        drop(claim);
        drop(busy);
        return;
    }
    // Registered as it was admitted ([`register_admitted_run`]), and again by
    // the loop before each pass once its revision reads current, never here:
    // a startup run that waited for the slot while a person's choice admitted
    // a newer run must not take that run's place for a Stop (`register`).
    with_setup_run(revision, || {
        run_setup_retry(
            &mut LiveSetupRetry {
                revision,
                password,
                last_failure: None,
                success_reported: false,
                wakes_seen: SETUP_WAKES.count(),
                park_due_seen: SetupRetryDeadlines::default(),
            },
            claim,
            busy,
            revision,
        );
    });
}

#[cfg(target_os = "macos")]
fn with_setup_run<T>(revision: u64, run: impl FnOnce() -> T) -> T {
    struct Registration(u64);
    impl Drop for Registration {
        fn drop(&mut self) {
            // A newer run may already hold the slot this worker gave up.
            clear_setup_run(self.0);
        }
    }
    let _registration = Registration(revision);
    run()
}

/// The scheduler owns the claim and UI reservation; the live host alone holds
/// the password and reaches the operating system.
#[cfg(any(target_os = "macos", test))]
trait SetupRetryHost {
    type Claim;
    type Busy;

    fn revision(&mut self) -> Result<u64, String>;
    fn finished(&mut self) -> Result<bool, String>;
    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    fn busy(&mut self) -> Self::Busy;
    /// This run is the one a Stop stops from now (`SETUP_RUN`): called before
    /// each pass, holding the slot and the claim, once its revision read
    /// current, so a run a newer one replaced never takes its place.
    fn register(&mut self);
    fn apply(&mut self) -> Result<PassNext, String>;
    fn backoff(&mut self);
    /// Wait until something a parked run waits on may have moved: a person's
    /// grant, Check again, a new plan, Stop, System State coming to the front, or
    /// a vault approval. No pass runs meanwhile, however long that takes.
    fn park(&mut self);
    /// Whether this run still retries by itself what a pass of it fails: `true` before a pass the
    /// loop runs again should it fail, `false` before a park. While it is `true` a failure is the
    /// page's alone; the menu line and the tab's attention say it once it is `false` (System
    /// State design §3.5, §3.6).
    fn retrying(&mut self, retrying: bool);
    fn failure(&mut self, why: &str);
    fn completed(&mut self);
}

/// What one setup pass left, as the retry loop reads it
/// (`aterm_cli::macos_doctor::SetupNext`).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassNext {
    /// Nothing is left for this pass to do.
    Done,
    /// Something changes by itself (a service starting, the network): try again.
    Retry,
    /// Only a person moves what is left: no pass changes it until they do.
    Park,
}

/// Passes in a row that only retried before the loop parks instead: what a pass
/// retries changes by itself within minutes, and a run that never settles must
/// not wake every minute for the life of the app.
#[cfg(any(target_os = "macos", test))]
const SETUP_RETRY_LIMIT: u32 = 10;

/// How the retry loop idles after a pass, and its new count of passes in a row
/// that only retried.
#[cfg(any(target_os = "macos", test))]
fn idle_after(next: PassNext, retries: u32) -> (PassNext, u32) {
    match next {
        PassNext::Retry if retries + 1 < SETUP_RETRY_LIMIT => (PassNext::Retry, retries + 1),
        PassNext::Retry | PassNext::Park => (PassNext::Park, 0),
        PassNext::Done => (PassNext::Retry, 0),
    }
}

#[cfg(any(target_os = "macos", test))]
fn release_setup_pass<C, B>(claim: &mut Option<C>, busy: &mut Option<B>) {
    drop(claim.take());
    drop(busy.take());
}

/// Give the slot and the claim back and wait before trying a pass again. Nothing the run failed
/// counts as retried meanwhile: what it waits on (a claim another process holds, a failing read)
/// may last, and its failure must reach the menu line (System State design §3.6).
#[cfg(any(target_os = "macos", test))]
fn back_off_before_a_pass<H: SetupRetryHost>(
    host: &mut H,
    claim: &mut Option<H::Claim>,
    busy: &mut Option<H::Busy>,
) {
    host.retrying(false);
    release_setup_pass(claim, busy);
    host.backoff();
}

/// The retry loop's host, which says the run no longer retries anything once
/// the loop is left: by a return, or by a panic in a pass unwinding through it.
#[cfg(any(target_os = "macos", test))]
struct StandDown<'h, H: SetupRetryHost>(&'h mut H);

#[cfg(any(target_os = "macos", test))]
impl<H: SetupRetryHost> Drop for StandDown<'_, H> {
    fn drop(&mut self) {
        self.0.retrying(false);
    }
}

/// The worker of admitted run `revision`, holding the setup slot (`busy`) and,
/// when it came with one, the automatic claim. Lock order is the slot, then
/// the claim, as a person's restart takes them: this worker never waits for the
/// slot while it holds the claim, so a choice made meanwhile is never refused
/// for it. A claim held elsewhere gives the slot back and backs off.
#[cfg(any(target_os = "macos", test))]
fn run_setup_retry<H: SetupRetryHost>(
    host: &mut H,
    claim: Option<H::Claim>,
    busy: H::Busy,
    revision: u64,
) {
    let stand_down = StandDown(host);
    let host = &mut *stand_down.0;
    let mut busy = Some(busy);
    let mut claim = claim;
    let mut retries = 0;
    loop {
        match host.finished() {
            Ok(true) => {
                host.completed();
                return;
            }
            Ok(false) => {}
            Err(why) => {
                host.failure(&why);
                back_off_before_a_pass(host, &mut claim, &mut busy);
                continue;
            }
        }
        match host.revision() {
            Ok(current) if current != revision => return,
            Ok(_) => {}
            Err(why) => {
                host.failure(&why);
                back_off_before_a_pass(host, &mut claim, &mut busy);
                continue;
            }
        }
        if busy.is_none() {
            busy = Some(host.busy());
            // A Stop or Restore may have committed while this worker waited.
            // Recheck completion and revision with the slot held.
            continue;
        }
        if claim.is_none() {
            match host.claim() {
                Ok(Some(acquired)) => claim = Some(acquired),
                Ok(None) => {
                    back_off_before_a_pass(host, &mut claim, &mut busy);
                    continue;
                }
                Err(why) => {
                    host.failure(&why);
                    back_off_before_a_pass(host, &mut claim, &mut busy);
                    continue;
                }
            }
            match host.revision() {
                Ok(current) if current != revision => return,
                Ok(_) => {}
                Err(why) => {
                    host.failure(&why);
                    back_off_before_a_pass(host, &mut claim, &mut busy);
                    continue;
                }
            }
        }
        host.register();
        // A failure is retried unless this pass is the last of a run that
        // only retried: the rule `idle_after` parks by.
        host.retrying(idle_after(PassNext::Retry, retries).0 == PassNext::Retry);
        let next = host.apply().unwrap_or_else(|why| {
            host.failure(&why);
            PassNext::Retry
        });
        match host.finished() {
            Ok(true) => {
                host.completed();
                return;
            }
            Ok(false) => {}
            Err(why) => host.failure(&why),
        }
        release_setup_pass(&mut claim, &mut busy);
        let (idle, count) = idle_after(next, retries);
        retries = count;
        if idle == PassNext::Park {
            host.retrying(false);
            host.park();
        } else {
            host.backoff();
        }
    }
}

/// Park without holding a setup lease. A local fact that changes or an explicit
/// wake ends the wait; unchanged readings never run another setup pass.
#[cfg(any(target_os = "macos", test))]
fn wait_for_setup_park_change<M: PartialEq>(
    mut mark: impl FnMut() -> M,
    mut ready: impl FnMut(&M) -> bool,
    mut still_waiting: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> M {
    let before = mark();
    if ready(&before) {
        return before;
    }
    while still_waiting() {
        wait();
        let current = mark();
        if ready(&current) || current != before {
            return current;
        }
    }
    before
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Default)]
struct SetupRetryDeadlines {
    secrets: Option<u64>,
    mac_settings: Option<u64>,
}

#[cfg(any(target_os = "macos", test))]
impl SetupRetryDeadlines {
    fn unseen(self, secrets: Option<u64>, mac_settings: Option<u64>) -> bool {
        secrets.is_some_and(|due| Some(due) != self.secrets)
            || mac_settings.is_some_and(|due| Some(due) != self.mac_settings)
    }

    fn consume(&mut self, secrets: Option<u64>, mac_settings: Option<u64>) {
        if secrets.is_some() {
            self.secrets = secrets;
        }
        if mac_settings.is_some() {
            self.mac_settings = mac_settings;
        }
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupRetry {
    revision: u64,
    password: Option<aterm_secrets::SecretBytes>,
    last_failure: Option<String>,
    success_reported: bool,
    /// The last wake this worker answered: set when a pass begins, so a wake
    /// during it is not lost, and by each backoff that a wake ended.
    wakes_seen: u64,
    /// Each source's last consumed deadline: an unchanged due attempt cannot
    /// wake every park, nor hide a newly due attempt from another source.
    park_due_seen: SetupRetryDeadlines,
}

#[cfg(target_os = "macos")]
impl SetupRetryHost for LiveSetupRetry {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;

    fn revision(&mut self) -> Result<u64, String> {
        aterm_cli::macos_doctor::initial_setup_revision()
    }
    fn finished(&mut self) -> Result<bool, String> {
        aterm_cli::macos_doctor::initial_setup_finished()
    }
    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        (setup_io().claim_automatic)()
    }
    fn busy(&mut self) -> Self::Busy {
        wait_for_initial_setup_slot()
    }
    fn register(&mut self) {
        // The pass holds the slot and the claim of the current revision: this
        // is the run a Stop stops, even after a choice that cleared the
        // registration was refused. Its Stop is saved by a worker this thread
        // starts, never the UI thread.
        set_setup_run(Some(self.revision));
        start_stop_worker();
    }
    fn apply(&mut self) -> Result<PassNext, String> {
        clear_initial_setup_rows();
        self.wakes_seen = SETUP_WAKES.count();
        let observed = with_setup_activity(|| {
            aterm_cli::macos_doctor::run_initial_setup_pass(
                self.revision,
                &mut self.password,
                SETUP_HOOKS,
            )
        });
        let Some((report, next)) = observed
            .result
            .inspect_err(|why| initial_setup_error(why.clone(), observed.observation.clone()))?
        else {
            return Ok(PassNext::Done);
        };
        if let Ok(env) = aterm_agent::harness::cli::Env::from_process()
            && let (Some(home), Some(roster_path)) = (env.home.as_deref(), env.accounts_roster())
        {
            let _ = aterm_agent::harness::accounts_sync::sync_accounts_for_init(
                home,
                &env.state,
                &roster_path,
                env.now,
            );
            let _ = aterm_agent::harness::accounts_sync::sync_cli_accounts_for_init(
                home,
                &env.state,
                &roster_path,
                env.now,
            );
        }
        let text = report.to_text();
        remember_initial_setup_rows(&report, None, observed.observation);
        if report.needs_attention() {
            self.failure(&text);
        } else if !self.success_reported {
            aterm_log::info!("first-run macOS doctor: {text}");
            self.success_reported = true;
        }
        Ok(match next {
            aterm_cli::macos_doctor::SetupNext::Done => PassNext::Done,
            aterm_cli::macos_doctor::SetupNext::Retry => PassNext::Retry,
            aterm_cli::macos_doctor::SetupNext::Park => PassNext::Park,
        })
    }
    fn backoff(&mut self) {
        // A wake (Check again, Stop, a new plan, a person's grant) ends it early,
        // once: a read or a claim that keeps failing waits a minute each time.
        SETUP_WAKES.back_off(&mut self.wakes_seen, std::time::Duration::from_secs(60));
    }
    fn park(&mut self) {
        // A person wakes what waits on them. The local mark also sees a vault
        // version change or, only while selected Secrets is pending, a local
        // interface change or an existing retry deadline. Mac settings also
        // wake for their second automatic try. Reading it once a minute makes
        // no socket, vault request or setup pass.
        let seen = self.wakes_seen;
        let due_seen = self.park_due_seen;
        let mut previous = None;
        let mark = wait_for_setup_park_change(
            || {
                let mut mark = aterm_cli::macos_doctor::initial_setup_park_mark();
                if let Some(known) = &previous {
                    mark.retain_known_from(known);
                }
                previous = Some(mark.clone());
                mark
            },
            |mark| due_seen.unseen(mark.retry_due(), mark.mac_settings_retry_due()),
            || SETUP_WAKES.count() == seen,
            || {
                SETUP_WAKES.wait(seen, std::time::Duration::from_secs(60));
            },
        );
        self.park_due_seen
            .consume(mark.retry_due(), mark.mac_settings_retry_due());
    }
    fn retrying(&mut self, retrying: bool) {
        set_setup_retrying(self.revision, retrying);
    }
    fn failure(&mut self, why: &str) {
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }
    fn completed(&mut self) {}
}

/// The admitted revision whose worker still retries by itself what its passes
/// fail (`SetupRetryHost::retrying`), or 0: published as
/// `SetupStatus::retrying`, which keeps a failure off the menu line and the
/// tab's attention meanwhile (System State design §3.6).
#[cfg(target_os = "macos")]
static SETUP_RETRYING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Note whether `revision`'s worker still retries, and publish the status when
/// that changed. Clearing leaves another revision's mark alone: a worker a
/// newer run replaced never speaks for it.
#[cfg(target_os = "macos")]
fn set_setup_retrying(revision: u64, retrying: bool) {
    use std::sync::atomic::Ordering;
    let changed = if retrying {
        SETUP_RETRYING.swap(revision, Ordering::AcqRel) != revision
    } else {
        SETUP_RETRYING
            .compare_exchange(revision, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    };
    if changed {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

#[cfg(test)]
mod setup_retry_tests {
    use super::{
        PassNext, SETUP_RETRY_LIMIT, SetupRetryHost, SetupWakes, begin_check_again, idle_after,
        run_setup_retry,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    struct State {
        claimed: bool,
        busy: bool,
        finished: bool,
        revision: u64,
        applied: Vec<u64>,
        /// The revisions registered for a Stop, pass by pass.
        registered: Vec<u64>,
        drops: Vec<&'static str>,
    }

    struct Lease {
        kind: &'static str,
        state: Rc<RefCell<State>>,
    }

    impl Drop for Lease {
        fn drop(&mut self) {
            let mut state = self.state.borrow_mut();
            state.drops.push(self.kind);
            if self.kind == "claim" {
                state.claimed = false;
            } else {
                assert!(
                    !state.claimed,
                    "release the claim before publishing an idle UI"
                );
                state.busy = false;
            }
        }
    }

    struct Fake {
        state: Rc<RefCell<State>>,
        initial_error: bool,
        finished_error: bool,
        restart_during_backoff: bool,
        restart_during_claim: bool,
        finished_during_busy: bool,
        completions: usize,
        backoffs: usize,
        failures: usize,
        /// What each scripted pass leaves.
        pass: PassNext,
        parks: usize,
        /// Claims to find held elsewhere (a person's restart) before one is had.
        claims_held: usize,
        /// Backoffs before the worker must have ended; with
        /// `restart_during_backoff`, the last one replaces its revision.
        backoff_limit: usize,
        /// What the loop said, pass by pass, of retrying a failure.
        retrying: Vec<bool>,
        /// What it last said of retrying as each backoff began.
        retrying_at_backoff: Vec<Option<bool>>,
        /// The pass panics.
        panic_in_apply: bool,
    }

    fn fixture() -> (Fake, Lease, Lease) {
        let state = Rc::new(RefCell::new(State {
            claimed: true,
            busy: true,
            finished: false,
            revision: 1,
            applied: Vec::new(),
            registered: Vec::new(),
            drops: Vec::new(),
        }));
        (
            Fake {
                state: Rc::clone(&state),
                initial_error: false,
                finished_error: false,
                restart_during_backoff: false,
                restart_during_claim: false,
                finished_during_busy: false,
                completions: 0,
                backoffs: 0,
                failures: 0,
                pass: PassNext::Retry,
                parks: 0,
                claims_held: 0,
                backoff_limit: 1,
                retrying: Vec::new(),
                retrying_at_backoff: Vec::new(),
                panic_in_apply: false,
            },
            Lease {
                kind: "claim",
                state: Rc::clone(&state),
            },
            Lease {
                kind: "busy",
                state,
            },
        )
    }

    impl SetupRetryHost for Fake {
        type Claim = Lease;
        type Busy = Lease;

        fn revision(&mut self) -> Result<u64, String> {
            if std::mem::take(&mut self.initial_error) {
                Err("the initial revision could not be read".into())
            } else {
                Ok(self.state.borrow().revision)
            }
        }
        fn finished(&mut self) -> Result<bool, String> {
            if std::mem::take(&mut self.finished_error) {
                Err("a temporary completion read failure".into())
            } else {
                Ok(self.state.borrow().finished)
            }
        }
        fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
            let mut state = self.state.borrow_mut();
            assert!(
                state.busy && !state.claimed,
                "the slot comes before the claim"
            );
            if self.claims_held > 0 {
                self.claims_held -= 1;
                return Ok(None);
            }
            state.claimed = true;
            if self.restart_during_claim {
                state.revision += 1;
            }
            Ok(Some(Lease {
                kind: "claim",
                state: Rc::clone(&self.state),
            }))
        }
        fn busy(&mut self) -> Self::Busy {
            let mut state = self.state.borrow_mut();
            assert!(
                !state.claimed && !state.busy,
                "never wait for the slot while holding the claim"
            );
            state.busy = true;
            if self.finished_during_busy {
                state.finished = true;
            }
            Lease {
                kind: "busy",
                state: Rc::clone(&self.state),
            }
        }
        fn register(&mut self) {
            let mut state = self.state.borrow_mut();
            assert!(state.claimed && state.busy, "registered holding both");
            let revision = state.revision;
            state.registered.push(revision);
        }
        fn apply(&mut self) -> Result<PassNext, String> {
            assert!(!self.panic_in_apply, "a pass panics");
            let mut state = self.state.borrow_mut();
            assert!(state.claimed && state.busy);
            let revision = state.revision;
            assert_eq!(
                state.registered.last(),
                Some(&revision),
                "a pass runs as the registered run"
            );
            state.applied.push(revision);
            Ok(self.pass)
        }
        fn park(&mut self) {
            let mut state = self.state.borrow_mut();
            assert!(
                !state.claimed && !state.busy,
                "a parked run must leave the form usable"
            );
            self.parks += 1;
            assert_eq!(
                self.parks, 1,
                "a stale worker must end without parking again"
            );
            if self.restart_during_backoff {
                state.revision += 1;
            }
        }
        fn backoff(&mut self) {
            let mut state = self.state.borrow_mut();
            assert!(
                !state.claimed && !state.busy,
                "idle retries must leave the form usable"
            );
            self.backoffs += 1;
            self.retrying_at_backoff.push(self.retrying.last().copied());
            assert!(
                self.backoffs <= self.backoff_limit,
                "a stale worker must end without another retry"
            );
            if self.restart_during_backoff && self.backoffs == self.backoff_limit {
                state.revision += 1;
            }
        }
        fn retrying(&mut self, retrying: bool) {
            self.retrying.push(retrying);
        }
        fn failure(&mut self, _: &str) {
            self.failures += 1;
        }
        fn completed(&mut self) {
            assert!(
                self.finished_during_busy,
                "other scripted runs remain pending until replaced"
            );
            self.completions += 1;
        }
    }

    #[test]
    fn a_stop_or_restore_committed_while_acquiring_busy_prevents_a_stale_retry() {
        let (mut host, claim, busy) = fixture();
        host.finished_error = true;
        host.finished_during_busy = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert!(
            state.applied.is_empty(),
            "recheck completion after acquiring busy"
        );
        assert_eq!(host.completions, 1);
        assert_eq!(host.backoffs, 1);
        assert_eq!(state.drops, ["claim", "busy", "busy"]);
        assert!(!state.claimed && !state.busy);
    }

    #[test]
    fn pending_setup_releases_claim_and_busy_before_a_same_plan_rerun_retires_it() {
        let (mut host, claim, busy) = fixture();
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert_eq!(state.applied, [1]);
        assert_eq!(state.drops, ["claim", "busy"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.backoffs, 1);
    }

    #[test]
    fn a_pass_waiting_on_a_person_parks_instead_of_retrying_every_minute() {
        let (mut host, claim, busy) = fixture();
        host.pass = PassNext::Park;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert_eq!(state.applied, [1]);
        assert_eq!((host.parks, host.backoffs), (1, 0));
        assert!(!state.claimed && !state.busy);
    }

    /// Every pass the loop would run again on failure says the run retries; the last pass of a
    /// run that only retried, and the park, say it no longer does. A pass that waits on a person
    /// parks at once.
    #[test]
    fn a_run_says_it_retries_a_failure_until_it_parks() {
        let passes = SETUP_RETRY_LIMIT as usize;
        let (mut host, claim, busy) = fixture();
        // No backoff replaces the revision; the park does, which ends the worker.
        host.backoff_limit = passes;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        assert_eq!(host.state.borrow().applied.len(), passes);
        assert_eq!((host.backoffs, host.parks), (passes - 1, 1));
        let mut said = vec![true; passes - 1];
        said.extend([false, false, false]);
        assert_eq!(
            host.retrying, said,
            "retried, the last pass, the park, the end"
        );

        let (mut host, claim, busy) = fixture();
        host.pass = PassNext::Park;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        assert_eq!(
            host.retrying,
            [true, false, false],
            "a person's step parks at once"
        );
    }

    /// A panicking pass unwinds through `StandDown`, which says the run no longer retries; a
    /// plain reset after the loop would never run on that path.
    #[test]
    fn a_pass_that_panics_leaves_nothing_counted_as_retried() {
        let (mut host, claim, busy) = fixture();
        host.panic_in_apply = true;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_setup_retry(&mut host, Some(claim), busy, 1);
        }));
        assert!(unwound.is_err(), "PRECONDITION: the pass panicked");
        assert_eq!(host.retrying, [true, false], "the unwind stands it down");
        let state = host.state.borrow();
        assert!(
            !state.claimed && !state.busy,
            "the claim and the slot go too"
        );
    }

    /// Each backoff while another process holds the claim begins with the retry flag down; the
    /// backoff after a pass keeps it up.
    #[test]
    fn a_run_waiting_for_a_claim_held_elsewhere_retries_nothing_meanwhile() {
        let (mut host, claim, busy) = fixture();
        host.claims_held = 2;
        host.backoff_limit = 4;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        assert_eq!(host.state.borrow().applied, [1, 1]);
        assert_eq!(
            host.retrying_at_backoff,
            [Some(true), Some(false), Some(false), Some(true)],
            "after the pass, the held claim twice, after the next pass"
        );
    }

    #[test]
    fn retries_that_never_settle_end_in_a_park_and_a_park_resets_the_count() {
        let mut retries = 0;
        for pass in 1..SETUP_RETRY_LIMIT {
            let (idle, count) = idle_after(PassNext::Retry, retries);
            assert_eq!(idle, PassNext::Retry, "pass {pass} backs off");
            retries = count;
        }
        assert_eq!(idle_after(PassNext::Retry, retries), (PassNext::Park, 0));
        assert_eq!(idle_after(PassNext::Park, 3), (PassNext::Park, 0));
        assert_eq!(idle_after(PassNext::Done, 3), (PassNext::Retry, 0));
    }

    #[test]
    fn a_route_change_wakes_a_bounded_retry_run_without_another_pass_while_parked() {
        use super::wait_for_setup_park_change;
        use std::cell::Cell;

        struct Routed {
            inner: Fake,
            pending_secrets: bool,
            waits: usize,
        }

        impl SetupRetryHost for Routed {
            type Claim = Lease;
            type Busy = Lease;

            fn revision(&mut self) -> Result<u64, String> {
                self.inner.revision()
            }
            fn finished(&mut self) -> Result<bool, String> {
                self.inner.finished()
            }
            fn claim(&mut self) -> Result<Option<Lease>, String> {
                self.inner.claim()
            }
            fn busy(&mut self) -> Lease {
                self.inner.busy()
            }
            fn register(&mut self) {
                self.inner.register();
            }
            fn apply(&mut self) -> Result<PassNext, String> {
                let result = self.inner.apply();
                if self.inner.parks != 0 {
                    // The route or explicit wake let the next pass finish.
                    self.inner.state.borrow_mut().finished = true;
                }
                result
            }
            fn backoff(&mut self) {
                self.inner.backoff();
            }
            fn park(&mut self) {
                self.inner.park();
                let state = Rc::clone(&self.inner.state);
                let network = Cell::new(1);
                let wakes = Cell::new(0);
                wait_for_setup_park_change(
                    // Membership remains Unconfigured. Only pending selected
                    // Secrets adds a route to the CLI's opaque park mark.
                    || (1, self.pending_secrets.then(|| network.get())),
                    |_| false,
                    || wakes.get() == 0,
                    || {
                        self.waits += 1;
                        assert!(self.waits <= 3, "the wake must end this wait");
                        let state = state.borrow();
                        assert!(!state.claimed && !state.busy);
                        assert_eq!(state.applied.len(), SETUP_RETRY_LIMIT as usize);
                        if self.waits == 2 {
                            network.set(2);
                        } else if self.waits == 3 {
                            // A permissions-only run ignores that route. Its
                            // unchanged mark waits until an explicit wake.
                            assert!(!self.pending_secrets);
                            wakes.set(1);
                        }
                    },
                );
            }
            fn retrying(&mut self, retrying: bool) {
                self.inner.retrying(retrying);
            }
            fn failure(&mut self, why: &str) {
                self.inner.failure(why);
            }
            fn completed(&mut self) {
                self.inner.completions += 1;
            }
        }

        for pending_secrets in [true, false] {
            let (mut inner, claim, busy) = fixture();
            inner.backoff_limit = SETUP_RETRY_LIMIT as usize;
            let mut host = Routed {
                inner,
                pending_secrets,
                waits: 0,
            };
            run_setup_retry(&mut host, Some(claim), busy, 1);
            assert_eq!(host.waits, if pending_secrets { 2 } else { 3 });
            assert_eq!(host.inner.parks, 1);
            assert_eq!(host.inner.backoffs, SETUP_RETRY_LIMIT as usize - 1);
            assert_eq!(
                host.inner.state.borrow().applied.len(),
                SETUP_RETRY_LIMIT as usize + 1
            );
            assert_eq!(host.inner.completions, 1);
        }
    }

    #[test]
    fn a_parked_retry_deadline_wakes_once_and_an_advanced_deadline_wakes_again() {
        use super::{SetupRetryDeadlines, wait_for_setup_park_change};
        use std::cell::Cell;

        let mut seen = SetupRetryDeadlines::default();
        let first = wait_for_setup_park_change(
            || (Some(100), None),
            |mark| seen.unseen(mark.0, mark.1),
            || true,
            || panic!("an already-due attempt must wake at park entry"),
        );
        seen.consume(first.0, first.1);

        let waits = Cell::new(0);
        wait_for_setup_park_change(
            || (Some(100), None),
            |mark| seen.unseen(mark.0, mark.1),
            || waits.get() < 2,
            || waits.set(waits.get() + 1),
        );
        assert_eq!(waits.get(), 2, "the same stale due token stays parked");

        let next = wait_for_setup_park_change(
            || (Some(100), Some(600)),
            |mark| seen.unseen(mark.0, mark.1),
            || true,
            || panic!("a stale Secrets token must not hide a new Mac settings deadline"),
        );
        seen.consume(next.0, next.1);
        seen.consume(Some(100), None);
        assert!(
            !seen.unseen(Some(100), Some(600)),
            "spent once, even if absent briefly"
        );

        let later = wait_for_setup_park_change(
            || (Some(200), Some(600)),
            |mark| seen.unseen(mark.0, mark.1),
            || true,
            || panic!("the next Secrets attempt owns its own new deadline"),
        );
        seen.consume(later.0, later.1);
        assert!(!seen.unseen(Some(200), Some(600)));
        assert!(seen.unseen(Some(200), Some(1_200)));
    }

    #[test]
    fn a_same_network_retry_wakes_when_due_while_a_held_attempt_has_no_timer() {
        use super::wait_for_setup_park_change;
        use std::cell::Cell;

        for timed in [true, false] {
            let waits = Cell::new(0);
            let mark = wait_for_setup_park_change(
                || (1, (timed && waits.get() >= 2).then_some(100)),
                |mark| mark.1.is_some(),
                || waits.get() < 3,
                || waits.set(waits.get() + 1),
            );
            assert_eq!(waits.get(), if timed { 2 } else { 3 });
            assert_eq!(mark.1, timed.then_some(100));
        }
    }

    #[test]
    fn a_revision_changed_during_reacquisition_prevents_any_apply() {
        let (mut host, claim, busy) = fixture();
        host.finished_error = true;
        host.restart_during_claim = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert!(state.applied.is_empty());
        assert_eq!(state.drops, ["claim", "busy", "claim", "busy"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.failures, 1);
    }

    #[test]
    fn a_revision_read_error_releases_resources_before_retrying_the_admitted_run() {
        let (mut host, claim, busy) = fixture();
        host.initial_error = true;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert!(state.applied.is_empty());
        assert_eq!(state.drops, ["claim", "busy"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.failures, 1);
        assert_eq!(host.backoffs, 1);
    }

    #[test]
    fn authorization_fence_does_not_adopt_a_run_replaced_before_retry_entry() {
        let (mut host, claim, busy) = fixture();
        host.state.borrow_mut().revision = 2;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        assert!(
            host.state.borrow().applied.is_empty(),
            "a late baseline read must not adopt revision 2 with revision 1's password"
        );
    }

    #[test]
    fn the_slot_comes_before_the_claim_and_a_held_claim_gives_the_slot_back() {
        // The worker holds the slot but not the claim, which a person's
        // restart holds: that restart claims while holding the slot, so the
        // worker must give the slot back rather than keep it and wait.
        let (mut host, claim, busy) = fixture();
        drop(claim);
        host.state.borrow_mut().drops.clear();
        host.claims_held = 1;
        host.backoff_limit = 2;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, None, busy, 1);
        let state = host.state.borrow();
        assert_eq!(state.applied, [1]);
        assert_eq!(state.drops, ["busy", "claim", "busy"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.backoffs, 2);
    }

    #[test]
    fn a_run_registers_for_stop_only_once_its_revision_is_current() {
        // The startup's run waited for the slot while a person's choice
        // admitted revision 2 and registered it: the stale run ends without
        // taking revision 2's place, so a Stop still stops revision 2.
        let (mut host, claim, busy) = fixture();
        drop(claim);
        host.state.borrow_mut().revision = 2;
        run_setup_retry(&mut host, None, busy, 1);
        let state = host.state.borrow();
        assert!(
            state.registered.is_empty(),
            "a replaced run never registers"
        );
        assert!(state.applied.is_empty());
        drop(state);
        // The current run registers before each pass, holding the slot and
        // the claim.
        let (mut host, claim, busy) = fixture();
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let state = host.state.borrow();
        assert_eq!(state.registered, [1]);
        assert_eq!(state.applied, [1]);
    }

    #[test]
    fn a_backoff_returns_early_once_per_wake() {
        let wakes = SetupWakes::new();
        let mut seen = wakes.count();
        // A wake during the pass (Check again, a reading that found a fix)
        // ends the first backoff at once; the one-minute limit is a hang
        // detector, never reached.
        wakes.raise();
        assert!(wakes.back_off(&mut seen, std::time::Duration::from_secs(60)));
        // With no new wake the next backoff waits its whole limit, so a read
        // or claim that keeps failing never spins the loop.
        assert!(!wakes.back_off(&mut seen, std::time::Duration::from_millis(1)));
        wakes.raise();
        assert!(wakes.back_off(&mut seen, std::time::Duration::from_secs(60)));
    }

    #[test]
    fn check_again_says_its_reading_and_its_check_apart() {
        let order = RefCell::new(Vec::new());
        // A pass holds the setup slot: the forced reading was queued, but the
        // check was refused. Check again started; nothing is labelled a check.
        let started = begin_check_again(
            || {
                order.borrow_mut().push("observe");
                true
            },
            || order.borrow_mut().push("wake"),
            || false,
        );
        assert_eq!(*order.borrow(), ["observe", "wake"]);
        assert_eq!(
            started,
            aterm_gui::CheckStarted {
                reading: true,
                check: false
            }
        );
        let started = begin_check_again(|| false, || {}, || true);
        assert_eq!(
            started,
            aterm_gui::CheckStarted {
                reading: false,
                check: true
            }
        );
    }
}

#[cfg(target_os = "macos")]
fn report_first_run_doctor_failure(last_failure: &mut Option<String>, text: &str) {
    if last_failure.as_deref() == Some(text) {
        return;
    }
    aterm_log::warn!("first-run macOS doctor: {text}; automatic setup retries while aterm is open");
    *last_failure = Some(text.to_string());
}

#[cfg(target_os = "macos")]
static INITIAL_SETUP_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "macos")]
static INITIAL_SETUP_ROWS: std::sync::Mutex<InitialSetupRows> =
    std::sync::Mutex::new(InitialSetupRows {
        observation: None,
        rows: Vec::new(),
        check: None,
    });

#[cfg(target_os = "macos")]
#[derive(Default)]
struct InitialSetupRows {
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
    rows: Vec<aterm_gui::SetupStatusRow>,
    check: Option<aterm_gui::SetupCheck>,
}

#[cfg(target_os = "macos")]
fn clear_initial_setup_rows() {
    if let Ok(mut last) = INITIAL_SETUP_ROWS.lock() {
        *last = InitialSetupRows::default();
    }
    aterm_gui::publish_initial_setup_status(initial_setup_status());
}

#[cfg(target_os = "macos")]
struct InitialSetupBusy {
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
}

#[cfg(target_os = "macos")]
impl InitialSetupBusy {
    fn new() -> Self {
        Self {
            observation: aterm_cli::macos_doctor::initial_setup_observation().ok(),
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_for_initial_setup_slot() -> InitialSetupBusy {
    while INITIAL_SETUP_BUSY
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    InitialSetupBusy::new()
}

#[cfg(target_os = "macos")]
impl Drop for InitialSetupBusy {
    fn drop(&mut self) {
        INITIAL_SETUP_BUSY.store(false, std::sync::atomic::Ordering::Release);
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

/// How long the startup coordinator waits before it tries a failed read or write again.
#[cfg(target_os = "macos")]
const SETUP_RETRY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Automatic claims this process's startup coordinator holds, or is asking
/// for, without the setup slot ([`StartupClaim`]).
#[cfg(target_os = "macos")]
static STARTUP_CLAIMS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many times the startup coordinator has asked for the claim: a choice that found the claim
/// taken with [`STARTUP_CLAIMS`] at zero both before and after its ask still knows, when this
/// moved, that the startup held it meanwhile.
#[cfg(target_os = "macos")]
static STARTUP_CLAIM_ASKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A person's choices waiting for that claim ([`AwaitingClaim`]).
#[cfg(target_os = "macos")]
static CHOICES_AWAITING_CLAIM: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// Raised when a person's choice comes to wait for the claim: it ends the
/// startup's wait before it writes its answer again ([`await_waiting_choice`]).
#[cfg(target_os = "macos")]
static CHOICE_AWAITS_CLAIM: std::sync::Condvar = std::sync::Condvar::new();

#[cfg(target_os = "macos")]
fn choices_awaiting_claim() -> std::sync::MutexGuard<'static, usize> {
    CHOICES_AWAITING_CLAIM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The automatic claim while the startup coordinator (`prepare_setup`) holds
/// it without the setup slot: reading the plan in force, offering it, admitting
/// the page's answer. A person's choice that finds the claim taken while one is
/// counted waits for it rather than be refused (`run_setup_restart`); counted
/// from before the claim is asked for, so such a choice never misses it.
#[cfg(target_os = "macos")]
struct StartupClaim(Option<aterm_cli::macos_doctor::AutomaticSetupClaim>);

#[cfg(target_os = "macos")]
impl StartupClaim {
    fn claim() -> Result<Option<Self>, String> {
        STARTUP_CLAIMS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        STARTUP_CLAIM_ASKS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut held = Self(None);
        held.0 = (setup_io().claim_automatic)()?;
        Ok(held.0.is_some().then_some(held))
    }

    /// The claim, from now held with the setup slot by the run it admitted.
    fn into_run(mut self) -> Option<aterm_cli::macos_doctor::AutomaticSetupClaim> {
        self.0.take()
    }
}

#[cfg(target_os = "macos")]
impl Drop for StartupClaim {
    fn drop(&mut self) {
        // The claim goes before the count does: while it is held, it is counted.
        drop(self.0.take());
        STARTUP_CLAIMS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// A person's choice waiting for the startup coordinator's claim, which lets
/// it go rather than offer a plan over the choice ([`choice_awaits_claim`]).
#[cfg(target_os = "macos")]
struct AwaitingClaim;

#[cfg(target_os = "macos")]
impl AwaitingClaim {
    fn new() -> Self {
        *choices_awaiting_claim() += 1;
        CHOICE_AWAITS_CLAIM.notify_all();
        Self
    }
}

#[cfg(target_os = "macos")]
impl Drop for AwaitingClaim {
    fn drop(&mut self) {
        let mut waiting = choices_awaiting_claim();
        *waiting = waiting.saturating_sub(1);
    }
}

#[cfg(target_os = "macos")]
fn choice_awaits_claim() -> bool {
    *choices_awaiting_claim() > 0
}

/// Wait, at most `limit`, until a person's choice waits for the claim the
/// startup holds. Whether one does: at once when one already did.
#[cfg(target_os = "macos")]
fn await_waiting_choice(limit: std::time::Duration) -> bool {
    CHOICE_AWAITS_CLAIM
        .wait_timeout_while(choices_awaiting_claim(), limit, |waiting| *waiting == 0)
        .map_or_else(
            |poison| *poison.into_inner().0 > 0,
            |(waiting, _)| *waiting > 0,
        )
}

/// Whether this process's startup coordinator holds the automatic claim, or
/// is asking for it ([`StartupClaim`]).
#[cfg(target_os = "macos")]
fn startup_holds_claim() -> bool {
    STARTUP_CLAIMS.load(std::sync::atomic::Ordering::Acquire) > 0
}

/// What the running setup pass is doing, for System State's busy rows, and the
/// stage events a reading must not straddle.
#[cfg(target_os = "macos")]
struct SetupActivity {
    /// Raised at every stage start and end.
    epoch: u64,
    /// The settings whose stage runs now.
    settings: Vec<aterm_types::macos_setup::Setting>,
    /// The rows that stage can change.
    subjects: Vec<aterm_types::macos_setup::Subject>,
    /// The epoch of the last stage event that could change each row.
    touched: Vec<(aterm_types::macos_setup::Subject, u64)>,
}

#[cfg(target_os = "macos")]
impl SetupActivity {
    fn mark(
        &mut self,
        settings: &[aterm_types::macos_setup::Setting],
        subjects: &[aterm_types::macos_setup::Subject],
        running: bool,
    ) {
        self.epoch += 1;
        self.settings = if running {
            settings.to_vec()
        } else {
            Vec::new()
        };
        self.subjects = if running {
            subjects.to_vec()
        } else {
            Vec::new()
        };
        for subject in subjects {
            match self.touched.iter_mut().find(|(row, _)| row == subject) {
                Some(touched) => touched.1 = self.epoch,
                None => self.touched.push((*subject, self.epoch)),
            }
        }
    }
}

#[cfg(target_os = "macos")]
static SETUP_ACTIVITY: std::sync::Mutex<SetupActivity> = std::sync::Mutex::new(SetupActivity {
    epoch: 0,
    settings: Vec::new(),
    subjects: Vec::new(),
    touched: Vec::new(),
});

/// The admitted revision whose worker is alive (running or parked) in this process.
#[cfg(target_os = "macos")]
static SETUP_RUN: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// The revision a person asked to stop (System State's Stop).
#[cfg(target_os = "macos")]
static SETUP_STOP: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Every admission this process sends, numbered in the order it is sent: a
/// person's choice the moment it is made (on the UI thread, so in order with
/// every press of Stop), and this launch's startup when it offers its plan
/// (before the page can show it) or resumes the plan in force.
#[cfg(target_os = "macos")]
static SETUP_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The newest admission ([`SETUP_SENT`]) a Stop covers: a run admitted from
/// one sent before a press of Stop, or before a choice made while a pass ran,
/// is stopped the moment it registers ([`register_admitted_run`]) or is given
/// back ([`forget_setup_choice`]). Zero covers none. Written and read under
/// [`SETUP_RUN`]; a quit reads it to know which admissions it waits for.
#[cfg(target_os = "macos")]
static STOP_COVERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number the admission being sent now ([`SETUP_SENT`]).
#[cfg(target_os = "macos")]
fn send_admission() -> u64 {
    SETUP_SENT.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
}

/// Whether a Stop covers admission `sent` ([`STOP_COVERS`]).
#[cfg(target_os = "macos")]
fn stop_covers(sent: u64) -> bool {
    sent <= STOP_COVERS.load(std::sync::atomic::Ordering::Acquire)
}

/// The durable half of a Stop ([`stop_setup_run`]). Its request is saved on
/// the Stop's own worker, never on the thread that asked, which is the UI's: a
/// save flushes the disk, which a loaded Mac holds up for tens of
/// milliseconds. The worker is started when a run registers, on the setup
/// worker's thread, so the UI thread only hands the request over. A quit
/// waits for the save, and for an admission a Stop covers to register its run
/// ([`settle_setup_stop`]).
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
struct StopSaves {
    /// The run whose Stop waits for the worker.
    wanted: Option<u64>,
    /// The run whose Stop the worker is saving now.
    saving: Option<u64>,
    /// The run whose Stop is saved.
    saved: Option<u64>,
    /// The worker was started: it lives as long as the process, waiting for
    /// `wanted`.
    worker: bool,
    /// The admissions under way ([`UnderWay`]), by number ([`SETUP_SENT`]).
    under_way: Vec<u64>,
}

#[cfg(target_os = "macos")]
impl StopSaves {
    /// What a quit still waits for, a Stop covering admissions through
    /// `covers` ([`STOP_COVERS`]): a save going or handed to the worker, or an
    /// admission the Stop covers that has not registered its run yet.
    fn owed(&self, covers: u64) -> bool {
        self.saving.is_some()
            || (self.wanted.is_some() && self.worker)
            || self.under_way.iter().any(|sent| *sent <= covers)
    }
}

#[cfg(target_os = "macos")]
static STOP_SAVES: std::sync::Mutex<StopSaves> = std::sync::Mutex::new(StopSaves {
    wanted: None,
    saving: None,
    saved: None,
    worker: false,
    under_way: Vec::new(),
});

/// An admission under way, from the moment it is sent ([`send_admission`])
/// until its run registers ([`register_admitted_run`]) or it is let go. A quit
/// waits for one a Stop covers, so the run it admits is stopped durably before
/// the process ends ([`settle_setup_stop`]).
#[cfg(target_os = "macos")]
struct UnderWay(u64);

#[cfg(target_os = "macos")]
impl UnderWay {
    fn new(sent: u64) -> Self {
        stop_saves().under_way.push(sent);
        Self(sent)
    }
}

#[cfg(target_os = "macos")]
impl Drop for UnderWay {
    fn drop(&mut self) {
        stop_saves().under_way.retain(|sent| *sent != self.0);
        STOP_SAVES_MOVED.notify_all();
    }
}

/// Raised when a Stop is handed to the worker, when a save ends, and when an
/// admission under way registers its run or is let go.
#[cfg(target_os = "macos")]
static STOP_SAVES_MOVED: std::sync::Condvar = std::sync::Condvar::new();

/// How long a quit waits for what a Stop owes: an admission's one write and the Stop's save of a
/// small file, whose flushes a loaded Mac holds up for at most about a tenth of a second.
#[cfg(target_os = "macos")]
const STOP_SETTLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(target_os = "macos")]
fn stop_saves() -> std::sync::MutexGuard<'static, StopSaves> {
    STOP_SAVES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wakes a parked or backing-off run: raised by Check again, Stop, a new plan, and
/// a reading that finds a row a person fixed.
#[cfg(target_os = "macos")]
static SETUP_WAKES: SetupWakes = SetupWakes::new();

/// A count of wakes and the condition a parked or backing-off worker waits on.
#[cfg(any(target_os = "macos", test))]
struct SetupWakes {
    count: std::sync::Mutex<u64>,
    raised: std::sync::Condvar,
}

#[cfg(any(target_os = "macos", test))]
impl SetupWakes {
    const fn new() -> Self {
        Self {
            count: std::sync::Mutex::new(0),
            raised: std::sync::Condvar::new(),
        }
    }

    fn count(&self) -> u64 {
        self.count.lock().map_or(0, |count| *count)
    }

    fn raise(&self) {
        if let Ok(mut count) = self.count.lock() {
            *count = count.wrapping_add(1);
        }
        self.raised.notify_all();
    }

    /// Wait until a wake after `seen`, or `limit` passes. `true` when woken.
    fn wait(&self, seen: u64, limit: std::time::Duration) -> bool {
        let Ok(count) = self.count.lock() else {
            std::thread::sleep(limit);
            return false;
        };
        self.raised
            .wait_timeout_while(count, limit, |count| *count == seen)
            .is_ok_and(|(_, waited)| !waited.timed_out())
    }

    /// A retry's backoff: wait for a wake after `seen`, or `limit`, then take
    /// every wake so far as answered, so the next backoff waits again rather
    /// than returning at once on the wake this one returned on. `true` when woken.
    fn back_off(&self, seen: &mut u64, limit: std::time::Duration) -> bool {
        let woken = self.wait(*seen, limit);
        *seen = self.count();
        woken
    }
}

/// The person's latest plan change: a queued one that a later change overtook is dropped.
#[cfg(target_os = "macos")]
static SETUP_APPLY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(target_os = "macos")]
const SETUP_HOOKS: aterm_cli::macos_doctor::SetupHooks = aterm_cli::macos_doctor::SetupHooks {
    stop: setup_stop_requested,
    stage: setup_stage,
    on_screen: aterm_types::macos_setup::system_state_front,
};

/// What System State's worker does outside this process: the doctor's records
/// and readings of the Mac. A test of this binary puts its own in [`SETUP_IO`],
/// so it drives the worker's real entry points (a choice, a Stop, a reading)
/// without reading the Mac or writing the person's state root.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct SetupIo {
    /// Whether this process may never change the person's profile
    /// (`aterm_cli::macos_doctor::lasting_scope_refusal`).
    scope_refused: fn() -> bool,
    /// A read-only reading of rows for a plan, the network's too when asked
    /// (`aterm_cli::macos_doctor::observe_initial_setup`).
    read: fn(
        &aterm_types::macos_setup::Plan,
        &[aterm_types::macos_setup::Subject],
        bool,
    ) -> Vec<aterm_types::macos_setup::Observation>,
    /// Save a person's Stop of a run, durably, on the Stop's worker
    /// (`aterm_cli::macos_doctor::request_initial_setup_stop`).
    save_stop: fn(u64) -> Result<(), String>,
    /// Take the automatic claim for this launch's startup or a run's worker
    /// (`aterm_cli::macos_doctor::claim_automatic_setup`).
    claim_automatic: fn() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String>,
    /// Take it for a person's choice, even after setup ended
    /// (`aterm_cli::macos_doctor::claim_initial_setup_rerun`).
    claim_rerun: fn() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String>,
    /// Durably admit a person's choice, and whether its vault was typed
    /// (`aterm_cli::macos_doctor::InitialSetupRestart::persist_observed`).
    persist: fn(
        &mut aterm_cli::macos_doctor::InitialSetupRestart,
        &aterm_types::macos_setup::Plan,
        bool,
    ) -> aterm_cli::macos_doctor::InitialSetupObserved<Option<u64>>,
    /// The worker of the run a choice or this launch's startup admitted, on
    /// that thread until it ends ([`run_first_run_doctor`]).
    run: fn(
        Option<aterm_cli::macos_doctor::AutomaticSetupClaim>,
        u64,
        Option<aterm_secrets::SecretBytes>,
        InitialSetupBusy,
    ),
}

#[cfg(target_os = "macos")]
impl SetupIo {
    const LIVE: Self = Self {
        scope_refused: || aterm_cli::macos_doctor::lasting_scope_refusal().is_some(),
        read: aterm_cli::macos_doctor::observe_initial_setup,
        save_stop: aterm_cli::macos_doctor::request_initial_setup_stop,
        claim_automatic: aterm_cli::macos_doctor::claim_automatic_setup,
        claim_rerun: aterm_cli::macos_doctor::claim_initial_setup_rerun,
        persist: aterm_cli::macos_doctor::InitialSetupRestart::persist_observed,
        run: run_first_run_doctor,
    };
}

/// The worker's [`SetupIo`]: the live one, except in a test that set its own.
#[cfg(target_os = "macos")]
static SETUP_IO: std::sync::RwLock<SetupIo> = std::sync::RwLock::new(SetupIo::LIVE);

#[cfg(target_os = "macos")]
fn setup_io() -> SetupIo {
    *SETUP_IO
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(target_os = "macos")]
fn set_setup_run(revision: Option<u64>) {
    if let Ok(mut run) = SETUP_RUN.lock() {
        *run = revision;
    }
}

#[cfg(target_os = "macos")]
fn clear_setup_run(revision: u64) {
    if let Ok(mut run) = SETUP_RUN.lock()
        && *run == Some(revision)
    {
        *run = None;
    }
}

#[cfg(target_os = "macos")]
fn wake_setup() {
    SETUP_WAKES.raise();
}

/// What the setup worker hears from the window, registered once before the GUI
/// starts: a run parked on System State (the Accounts stage) resumes when the
/// page comes to the front, and an access step the window recorded (handed,
/// declined, triggered, granted) moves the Access rows and a run parked on the
/// Permissions stage at once (System State design §6). A caller that timed out
/// behind another dialog also retries once its last guard ends.
#[cfg(target_os = "macos")]
fn watch_setup_signals() {
    aterm_types::macos_setup::on_system_state_front(wake_setup);
    aterm_types::macos_setup::access_record::on_append(access_step_recorded);
    aterm_types::macos_setup::on_macos_dialog_waiter_ready(wake_setup);
}

/// The window wrote a line to the access record (System State design §6): a step it handed to
/// macOS, a Not now, the Local Network trigger, a grant it verified or the debugger's outcome.
/// The Access rows are read again so the page and the menu line follow it, and a run parked on
/// the Permissions stage looks at the record again.
#[cfg(target_os = "macos")]
fn access_step_recorded(
    _revision: u64,
    _access: aterm_types::macos_setup::Access,
    _outcome: aterm_types::macos_setup::AccessOutcome,
) {
    use aterm_types::macos_setup::{Access, Subject};
    request_observation(None, Access::ALL.map(Subject::Access).to_vec(), false);
    wake_setup();
}

#[cfg(target_os = "macos")]
fn setup_stop_requested(revision: u64) -> bool {
    SETUP_STOP.lock().is_ok_and(|stop| *stop == Some(revision))
}

/// A stage of the running pass started or ended: its rows paint busy while it
/// runs, and they are read again once it ends.
#[cfg(target_os = "macos")]
fn setup_stage(event: aterm_cli::macos_doctor::StageEvent<'_>) {
    if let Ok(mut activity) = SETUP_ACTIVITY.lock() {
        activity.mark(event.settings, event.subjects, event.running);
    }
    aterm_gui::publish_initial_setup_status(initial_setup_status());
    if !event.running {
        request_observation(None, event.subjects.to_vec(), false);
    }
}

/// No stage runs now (a pass ended, possibly through an error inside a stage).
#[cfg(target_os = "macos")]
fn end_setup_activity() {
    if let Ok(mut activity) = SETUP_ACTIVITY.lock()
        && (!activity.settings.is_empty() || !activity.subjects.is_empty())
    {
        // An early error missed the normal end hook. Retire readings taken
        // during that stage just as the hook would, before clearing its rows.
        let subjects = std::mem::take(&mut activity.subjects);
        activity.mark(&[], &subjects, false);
    }
}

#[cfg(target_os = "macos")]
fn with_setup_activity<T>(pass: impl FnOnce() -> T) -> T {
    struct Activity;
    impl Drop for Activity {
        fn drop(&mut self) {
            // An error or panic may have missed the stage's normal end hook.
            end_setup_activity();
            // Every row is read again after a pass (System State design §3.5).
            request_observation(None, aterm_types::macos_setup::Subject::all(), false);
        }
    }
    let _activity = Activity;
    pass()
}

/// One queued read of System State rows.
#[cfg(target_os = "macos")]
struct ObserveRequest {
    /// The page's plan; `None` keeps the last one.
    plan: Option<aterm_types::macos_setup::Plan>,
    /// [`ObservedRows::choices`] when this request was made: a person's
    /// newer choice retires both the named plan and broader Check scope.
    choices: u64,
    subjects: Vec<aterm_types::macos_setup::Subject>,
    /// Check again: read GitHub repositories even while it is not managed.
    forced: bool,
}

#[cfg(target_os = "macos")]
struct ObserveQueue {
    running: bool,
    next: Option<ObserveRequest>,
}

#[cfg(target_os = "macos")]
static OBSERVE_QUEUE: std::sync::Mutex<ObserveQueue> = std::sync::Mutex::new(ObserveQueue {
    running: false,
    next: None,
});

/// The latest reading of each row, keyed by row, with the generation the page
/// compares and the plan the readings were taken for.
#[cfg(target_os = "macos")]
struct ObservedRows {
    rows: Vec<aterm_types::macos_setup::Observation>,
    generation: u64,
    /// The vault source the Secrets and Repositories readings share; a reading for another
    /// source clears both before adding its rows.
    vault_url: Option<String>,
    plan: Option<aterm_types::macos_setup::Plan>,
    /// Raised by each person's choice ([`begin_setup_choice`]) and each
    /// refusal that lets one go ([`forget_setup_choice`]): a plan named or
    /// resolved before it, by a reading queued or in flight then, never
    /// replaces the choice's.
    choices: u64,
    /// How many access steps the current revision had recorded at the last reading.
    access_steps: usize,
}

#[cfg(target_os = "macos")]
static OBSERVED_ROWS: std::sync::Mutex<ObservedRows> = std::sync::Mutex::new(ObservedRows {
    rows: Vec::new(),
    generation: 0,
    vault_url: None,
    plan: None,
    choices: 0,
    access_steps: 0,
});

/// Queue a read-only reading of `subjects` on its own worker, never the setup
/// slot. Requests made while one runs merge into the next.
#[cfg(target_os = "macos")]
fn request_observation(
    plan: Option<aterm_types::macos_setup::Plan>,
    subjects: Vec<aterm_types::macos_setup::Subject>,
    forced: bool,
) -> bool {
    queue_observation(ObserveRequest {
        plan,
        choices: OBSERVED_ROWS.lock().map_or(0, |rows| rows.choices),
        subjects,
        forced,
    })
}

/// Enqueue the captured request; another choice or request may have arrived
/// between capturing its scope and acquiring the queue.
#[cfg(target_os = "macos")]
fn queue_observation(request: ObserveRequest) -> bool {
    let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
        return false;
    };
    if queue.running {
        queue.next = Some(match queue.next.take() {
            None => request,
            Some(mut next) => {
                // Broader Check scope belongs to its own choice and plan.
                // A newer choice or another named plan retires it; a late
                // request from an older choice cannot put it back.
                if request.choices >= next.choices {
                    let replaced = request.choices != next.choices
                        || request
                            .plan
                            .as_ref()
                            .is_some_and(|plan| next.plan.as_ref() != Some(plan));
                    if replaced {
                        next.plan = request.plan;
                        next.choices = request.choices;
                        next.forced = request.forced;
                    } else {
                        next.forced |= request.forced;
                    }
                }
                for subject in request.subjects {
                    if !next.subjects.contains(&subject) {
                        next.subjects.push(subject);
                    }
                }
                next
            }
        });
        return true;
    }
    queue.running = true;
    drop(queue);
    let spawned = std::thread::Builder::new()
        .name("aterm-setup-observe".into())
        .spawn(move || {
            let mut request = request;
            loop {
                observe_rows(request);
                let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
                    return;
                };
                match queue.next.take() {
                    Some(next) => request = next,
                    None => {
                        queue.running = false;
                        return;
                    }
                }
            }
        });
    if let Err(why) = spawned {
        aterm_log::warn!("System State could not read its rows: {why}");
        if let Ok(mut queue) = OBSERVE_QUEUE.lock() {
            queue.running = false;
        }
        return false;
    }
    true
}

/// Read, keep what no stage changed meanwhile, and publish the new readings.
#[cfg(target_os = "macos")]
fn observe_rows(request: ObserveRequest) {
    use aterm_types::macos_setup::Setting;
    let Some((plan, choices)) = observed_plan(request.plan, request.choices) else {
        return;
    };
    let forced = request.forced && request.choices == choices;
    let network = forced || plan.selection.contains(Setting::Repositories);
    // Whether this login is signed in to Messages or FaceTime is the person's
    // own account state: read only while that group is set up, or on Check
    // again, as the repositories are (System State design §3.5).
    let subjects: Vec<_> = request
        .subjects
        .iter()
        .copied()
        .filter(|subject| match subject {
            aterm_types::macos_setup::Subject::Setting(setting)
                if setting.group() == aterm_types::macos_setup::Group::Accounts =>
            {
                forced || plan.selection.contains(*setting)
            }
            _ => true,
        })
        .collect();
    let started = SETUP_ACTIVITY.lock().map_or(0, |activity| activity.epoch);
    let read = (setup_io().read)(&plan, &subjects, network);
    let access_steps = aterm_cli::macos_doctor::initial_setup_access_revision()
        .ok()
        .zip(aterm_types::dirs::state_dir())
        .map_or(0, |(revision, dir)| {
            aterm_types::macos_setup::access_record::read(&dir, revision).len()
        });
    let (changed, wake) = {
        // Hold activity through publication, in Restore's lock order, so a reader checked just
        // before a change cannot slip its old rows back after that change retired them.
        let Ok(activity) = SETUP_ACTIVITY.lock() else {
            return;
        };
        let Ok(mut rows) = OBSERVED_ROWS.lock() else {
            return;
        };
        // A choice (or its refusal) retires the whole reading, not just the
        // plan it names: its vault, account state and access count were read
        // for the earlier plan. Read again under the current choice without
        // carrying a superseded Check's forced personal/network scope.
        if rows.choices != choices {
            drop(rows);
            drop(activity);
            request_observation(None, request.subjects, false);
            return;
        }
        let source = plan.vault_url.trim();
        let mut changed = rows.vault_url.as_deref() != Some(source);
        if changed {
            rows.rows.retain(|row| !row.subject.uses_vault_source());
            rows.vault_url = Some(source.to_owned());
        }
        rows.plan = Some(plan);
        // A row a person fixed, or an access step the window recorded, is what a
        // parked run waits for.
        let mut wake = rows.access_steps != access_steps;
        rows.access_steps = access_steps;
        // A reading taken while its own stage or Restore ran may be half of a change.
        for seen in read.into_iter().filter(|seen| {
            !activity.subjects.contains(&seen.subject)
                && activity
                    .touched
                    .iter()
                    .all(|(row, epoch)| *row != seen.subject || *epoch <= started)
        }) {
            match rows.rows.iter_mut().find(|row| row.subject == seen.subject) {
                Some(row) if *row == seen => {}
                Some(row) => {
                    wake |= !row.standing.settled() && seen.standing.settled();
                    *row = seen;
                    changed = true;
                }
                None => {
                    rows.rows.push(seen);
                    changed = true;
                }
            }
        }
        if changed {
            rows.generation += 1;
        }
        (changed, wake)
    };
    if changed {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
    if wake {
        wake_setup();
    }
}

/// Start a read-only, prompt-free reading of every System State row for `plan`.
#[cfg(target_os = "macos")]
fn observe_initial_setup(plan: aterm_types::macos_setup::Plan) -> bool {
    request_observation(Some(plan), aterm_types::macos_setup::Subject::all(), false)
}

/// System State's Turn On Sync ([`aterm_gui::InitialSetupHandlers::sync_on`]): on its own worker
/// (`launchctl` can take seconds), installs the subscriber the person turned off again, as
/// `aterm secrets agent on` does, which removes the record of their choice; then the Secrets row
/// is read again. A failed install leaves the record, so the row still reads Sync off, and logs
/// why.
#[cfg(target_os = "macos")]
fn turn_sync_on() -> bool {
    use aterm_secrets::service::{self, Job};
    use aterm_types::macos_setup::{Setting, Subject};
    std::thread::Builder::new()
        .name("aterm-sync-on".into())
        .spawn(|| {
            let installed = aterm_secrets::client::Paths::live().and_then(|paths| {
                service::install(
                    &paths.home,
                    Job::Subscriber,
                    &service::program_for_service()?,
                    &["secrets".into(), "watch".into()],
                )
            });
            match installed {
                Ok(line) => aterm_log::info!("system state: Turn On Sync: {line}"),
                Err(why) => aterm_log::warn!("system state: Turn On Sync failed: {why}"),
            }
            request_observation(None, vec![Subject::Setting(Setting::Secrets)], false);
        })
        .is_ok()
}

/// The admitted revision whose worker is alive in this process, if any.
#[cfg(all(target_os = "macos", test))]
fn setup_run() -> Option<u64> {
    SETUP_RUN.lock().ok().and_then(|run| *run)
}

/// System State's Stop ([`aterm_gui::InitialSetupHandlers::stop`]): the run
/// going now stops at its next stage boundary, and so does the run of every
/// plan sent before the press that is still being admitted, the moment it
/// registers; their plans stay ([`stop_runs_sent_through`]). `false` while no
/// run is registered here to stop now. On the UI thread this takes three
/// uncontended locks and wakes the Stop's worker: no file is touched, and no
/// thread is started unless the worker could not be when the run registered.
#[cfg(target_os = "macos")]
fn stop_initial_setup() -> bool {
    stop_runs_sent_through(SETUP_SENT.load(std::sync::atomic::Ordering::Acquire))
}

/// Stop the run registered now ([`stop_setup_run`]), and cover every
/// admission sent through `through` ([`STOP_COVERS`]): a run one of them admits
/// later is stopped as it registers. `false` while no run is registered.
#[cfg(target_os = "macos")]
fn stop_runs_sent_through(through: u64) -> bool {
    let Ok(run) = SETUP_RUN.lock() else {
        return false;
    };
    STOP_COVERS.fetch_max(through, std::sync::atomic::Ordering::AcqRel);
    let Some(revision) = *run else {
        return false;
    };
    stop_setup_run(revision);
    drop(run);
    wake_setup();
    true
}

/// Register admitted run `revision` as the one a Stop stops ([`SETUP_RUN`]),
/// the moment its revision is durable, before its worker takes a step: a Stop
/// that covers the admission it came from (`sent`, [`stop_covers`]), or
/// `overtaken` (a later choice made while it was being admitted), stops it
/// before its first stage. Callers hold the automatic claim, so no newer run
/// can have registered first.
#[cfg(target_os = "macos")]
fn register_admitted_run(revision: u64, sent: u64, overtaken: bool) {
    start_stop_worker();
    if let Ok(mut run) = SETUP_RUN.lock() {
        *run = Some(revision);
        if overtaken || stop_covers(sent) {
            stop_setup_run(revision);
        }
    }
}

/// Stop run `revision`: at once for its pass in this process, which reads
/// [`SETUP_STOP`] at each stage boundary, and durably by a request naming the
/// revision, which the Stop's worker saves (`<state>/doctor/stop-request.json`)
/// and every reader of the run's end honors, so a quit after the Stop never
/// resumes the run (design §3.3); the pass folds it into the run's record
/// under the lock it holds. Callers hold [`SETUP_RUN`]: the lock order is
/// [`SETUP_RUN`], then [`SETUP_STOP`], then [`STOP_SAVES`].
#[cfg(target_os = "macos")]
fn stop_setup_run(revision: u64) {
    if let Ok(mut stop) = SETUP_STOP.lock() {
        *stop = Some(revision);
    }
    save_setup_stop(revision);
}

/// Hand run `revision`'s Stop to the Stop's worker, unless it is saved or being
/// saved. One whose save failed is handed again by the next ask, and by a quit
/// or an update's handoff ([`settle_setup_stop`]); the pass folds the Stop into
/// the run's record itself at its next stage boundary.
#[cfg(target_os = "macos")]
fn save_setup_stop(revision: u64) {
    {
        let mut saves = stop_saves();
        if saves.saved == Some(revision) || saves.saving == Some(revision) {
            return;
        }
        saves.wanted = Some(revision);
    }
    STOP_SAVES_MOVED.notify_all();
    start_stop_worker();
}

/// Start the Stop's worker unless it runs. A setup worker does as its run
/// registers, so that a Stop on the UI thread only wakes it.
#[cfg(target_os = "macos")]
fn start_stop_worker() {
    {
        let mut saves = stop_saves();
        if saves.worker {
            return;
        }
        saves.worker = true;
    }
    if let Err(why) = std::thread::Builder::new()
        .name("aterm-setup-stop".into())
        .spawn(save_wanted_stops)
    {
        aterm_log::warn!("System State could not start saving its Stop: {why}");
        stop_saves().worker = false;
        STOP_SAVES_MOVED.notify_all();
    }
}

/// The Stop's worker: save each Stop handed to it, for the life of the process.
#[cfg(target_os = "macos")]
fn save_wanted_stops() {
    loop {
        let revision = {
            let mut saves = STOP_SAVES_MOVED
                .wait_while(stop_saves(), |saves| saves.wanted.is_none())
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(revision) = saves.wanted.take() else {
                continue;
            };
            saves.saving = Some(revision);
            revision
        };
        let saved = (setup_io().save_stop)(revision)
            .inspect_err(|why| aterm_log::warn!("System State could not save its Stop: {why}"))
            .is_ok();
        {
            let mut saves = stop_saves();
            saves.saving = None;
            if saved {
                saves.saved = Some(revision);
            }
        }
        STOP_SAVES_MOVED.notify_all();
    }
}

/// The app is quitting, or handing off to an update: save once more a Stop
/// whose save failed, then wait, at most `limit`, for what a Stop owes the
/// records ([`StopSaves::owed`]): an admission it covers registering its run,
/// stopped, and the Stop's worker saving it. At once when nothing is owed.
/// Whether everything a Stop asked for is saved.
#[cfg(target_os = "macos")]
fn settle_setup_stop(limit: std::time::Duration) -> bool {
    if let Some(revision) = SETUP_STOP.lock().ok().and_then(|stop| *stop) {
        save_setup_stop(revision);
    }
    let covers = STOP_COVERS.load(std::sync::atomic::Ordering::Acquire);
    let waited = STOP_SAVES_MOVED
        .wait_timeout_while(stop_saves(), limit, |saves| saves.owed(covers))
        .is_ok_and(|(saves, _)| !saves.owed(covers));
    // A run that registered meanwhile was stopped: the Stop to read is the
    // one asked for last, read without holding the worker's lock (the order
    // is [`SETUP_STOP`], then [`STOP_SAVES`]).
    let stopped = SETUP_STOP.lock().ok().and_then(|stop| *stop);
    waited && stopped.is_none_or(|stopped| stop_saves().saved == Some(stopped))
}

/// [`aterm_gui::InitialSetupHandlers::settle`]: the app is quitting.
#[cfg(target_os = "macos")]
fn settle_initial_setup() {
    if !settle_setup_stop(STOP_SETTLE_LIMIT) {
        aterm_log::warn!("System State quit before what its Stop owes the records was saved");
    }
}

/// A person's choice from the moment it is made until it is admitted or
/// refused ([`begin_setup_choice`], [`forget_setup_choice`]).
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct SetupChoice {
    plan: aterm_types::macos_setup::Plan,
    /// The choice's number ([`SETUP_APPLY_GENERATION`]).
    generation: u64,
    /// Its admission's number ([`SETUP_SENT`]).
    sent: u64,
    /// The run whose registration the choice dropped when it took the slot.
    replaced: Option<u64>,
    /// [`ObservedRows::choices`] once this choice was made.
    choices: u64,
}

/// What a person's choice changes the moment it is made, before its admission
/// starts (design §3.3, §3.5). Its admission is numbered ([`SETUP_SENT`]), in
/// order with every press of Stop. Observation follows its plan from now, so a
/// group left as is is no longer read for, whatever a reading queued or in
/// flight named before. `replacing` (the choice holds the setup slot, so no
/// pass runs) also drops the registration of the run it replaces: a Stop
/// pressed meanwhile stops the run the choice admits as it registers, or the
/// run given back if the choice is refused.
#[cfg(target_os = "macos")]
fn begin_setup_choice(
    plan: &aterm_types::macos_setup::Plan,
    replacing: bool,
    generation: u64,
) -> SetupChoice {
    let replaced = if replacing {
        SETUP_RUN.lock().ok().and_then(|mut run| run.take())
    } else {
        None
    };
    let choices = OBSERVED_ROWS.lock().map_or(0, |mut rows| {
        rows.choices += 1;
        rows.plan = Some(plan.clone());
        rows.choices
    });
    SetupChoice {
        plan: plan.clone(),
        generation,
        sent: send_admission(),
        replaced,
        choices,
    }
}

/// A choice that was not admitted: the run it unregistered is again the one a Stop stops, unless
/// another registered since, and observation goes back to the plan in force unless a later choice
/// named its own. A later choice or a Stop made while this one was being admitted found no run to
/// stop, so the run given back stops now, before its next step. Called holding the setup slot, so
/// that run takes no step first.
#[cfg(target_os = "macos")]
fn forget_setup_choice(choice: &SetupChoice) {
    let mut stopped = false;
    if let Some(run) = choice.replaced
        && let Ok(mut registered) = SETUP_RUN.lock()
    {
        if registered.is_none() {
            *registered = Some(run);
        }
        if *registered == Some(run)
            && (setup_choice_overtaken(choice.generation) || stop_covers(choice.sent))
        {
            stop_setup_run(run);
            stopped = true;
        }
    }
    if stopped {
        wake_setup();
    }
    if let Ok(mut rows) = OBSERVED_ROWS.lock()
        && rows.choices == choice.choices
    {
        rows.choices += 1;
        rows.plan = None;
    }
}

/// The plan a reading is taken for, and [`ObservedRows::choices`] then: the
/// plan it names, unless a person chose since it was named, else the last
/// choice's or page's plan, else the plan in force.
#[cfg(target_os = "macos")]
fn observed_plan(
    named: Option<aterm_types::macos_setup::Plan>,
    named_under: u64,
) -> Option<(aterm_types::macos_setup::Plan, u64)> {
    let (cached, choices) = OBSERVED_ROWS
        .lock()
        .map_or((None, 0), |rows| (rows.plan.clone(), rows.choices));
    named
        .filter(|_| named_under == choices)
        .or(cached)
        .or_else(|| aterm_cli::macos_doctor::automatic_plan().ok())
        .map(|plan| (plan, choices))
}

/// Fill the rows' readings and the running stage into `status`.
#[cfg(target_os = "macos")]
fn add_setup_readings(status: &mut aterm_gui::SetupStatus) {
    if let Ok(rows) = OBSERVED_ROWS.lock() {
        let order = aterm_types::macos_setup::Subject::all();
        let mut observed = rows.rows.clone();
        observed.sort_by_key(|row| order.iter().position(|subject| *subject == row.subject));
        status.observed = observed;
        status.observed_generation = rows.generation;
        status.observed_vault_url = rows.vault_url.clone();
    }
    if let Ok(activity) = SETUP_ACTIVITY.lock() {
        status.running = activity.settings.clone();
    }
}

#[cfg(target_os = "macos")]
fn initial_setup_status() -> aterm_gui::SetupStatus {
    let snapshot = aterm_cli::macos_doctor::initial_setup_snapshot();
    let busy = INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire);
    let mut status = match INITIAL_SETUP_ROWS.lock() {
        Ok(last) => initial_setup_status_with(snapshot, busy, &last),
        Err(_) => initial_setup_status_with(snapshot, busy, &InitialSetupRows::default()),
    };
    status.retrying = SETUP_RETRYING.load(std::sync::atomic::Ordering::Acquire) != 0;
    add_setup_readings(&mut status);
    status
}

#[cfg(target_os = "macos")]
fn initial_setup_status_with(
    snapshot: Result<
        aterm_cli::macos_doctor::InitialSetupSnapshot,
        aterm_cli::macos_doctor::InitialSetupSnapshotError,
    >,
    busy: bool,
    last: &InitialSetupRows,
) -> aterm_gui::SetupStatus {
    let (plan, finished, permissions, mut rows, observation) = match snapshot {
        Ok(snapshot) => (
            Some(snapshot.plan),
            snapshot.finished,
            // The access lanes key their once-per-approval steps on the revision
            // that admitted the Permissions group, which a change to another
            // group keeps (System State design §6).
            Some(aterm_gui::InitialSetupPermissionPolicy {
                revision: snapshot.access_revision,
                allowed: snapshot.permissions_allowed,
            }),
            snapshot
                .rows
                .into_iter()
                .map(|row| aterm_gui::SetupStatusRow {
                    name: row.name,
                    status: row.status,
                    detail: row.detail,
                })
                .collect::<Vec<_>>(),
            Some(snapshot.observation),
        ),
        Err(error) => (
            None,
            false,
            None,
            vec![aterm_gui::SetupStatusRow {
                name: "Setup state".into(),
                status: match error {
                    aterm_cli::macos_doctor::InitialSetupSnapshotError::Updating => "pending",
                    aterm_cli::macos_doctor::InitialSetupSnapshotError::Read(_) => "error",
                }
                .into(),
                detail: error.to_string(),
            }],
            None,
        ),
    };
    let check = if last.observation.is_some() && last.observation == observation {
        rows.extend(last.rows.iter().cloned());
        last.check.clone()
    } else {
        None
    };
    aterm_gui::SetupStatus {
        plan,
        permissions,
        busy,
        finished,
        rows,
        check,
        ..aterm_gui::SetupStatus::default()
    }
}

#[cfg(target_os = "macos")]
fn remember_initial_setup_rows(
    report: &aterm_cli::macos_doctor::Report,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    publish_initial_setup_rows(initial_setup_report_rows(report), checked_plan, observation);
}

#[cfg(target_os = "macos")]
fn initial_setup_report_rows(
    report: &aterm_cli::macos_doctor::Report,
) -> Vec<aterm_gui::SetupStatusRow> {
    let checked = report.action == aterm_cli::macos_doctor::Action::Check;
    report
        .rows
        .iter()
        .map(|row| {
            // Setup's blocked steps are explained by their observed row, but
            // Restore's are operation results: even an in-place preference
            // can have an unknown or inaccessible original. Keep that reason
            // visible independently of the current plan's selected groups. A
            // setting back that loads at the next login (`LOGIN`) is no
            // failure and passes as itself.
            let not_restored = report.action == aterm_cli::macos_doctor::Action::Restore
                && row.status == "blocked";
            aterm_gui::SetupStatusRow {
                name: if not_restored {
                    format!("Restore: {}", row.name)
                } else if checked {
                    format!("Check: {}", row.name)
                } else {
                    row.name.clone()
                },
                status: if not_restored {
                    "failed".into()
                } else {
                    row.status.clone()
                },
                detail: row.detail.clone(),
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn initial_setup_error(
    detail: String,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    publish_initial_setup_error(detail, None, observation);
}

#[cfg(target_os = "macos")]
fn publish_initial_setup_error(
    detail: String,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    publish_initial_setup_rows(
        vec![aterm_gui::SetupStatusRow {
            name: if checked_plan.is_some() {
                "Check"
            } else {
                "Setup"
            }
            .into(),
            status: "failed".into(),
            detail,
        }],
        checked_plan,
        observation,
    );
}

#[cfg(target_os = "macos")]
fn publish_initial_setup_rows(
    rows: Vec<aterm_gui::SetupStatusRow>,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    if let Ok(mut last) = INITIAL_SETUP_ROWS.lock() {
        *last = if let Some(plan) = checked_plan {
            InitialSetupRows {
                observation,
                check: Some(aterm_gui::SetupCheck {
                    plan: plan.clone(),
                    rows,
                }),
                ..InitialSetupRows::default()
            }
        } else {
            InitialSetupRows {
                observation,
                rows,
                check: None,
            }
        };
    }
    aterm_gui::publish_initial_setup_status(initial_setup_status());
}

/// Take the setup slot if nobody holds it, without waiting or reading files.
#[cfg(target_os = "macos")]
fn take_initial_setup_slot_now() -> bool {
    INITIAL_SETUP_BUSY
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// The setup slot if nobody holds it now (a worker thread's: it reads records).
#[cfg(target_os = "macos")]
fn take_initial_setup_slot() -> Option<InitialSetupBusy> {
    take_initial_setup_slot_now().then(InitialSetupBusy::new)
}

#[cfg(target_os = "macos")]
fn spawn_initial_setup_operation(
    name: &str,
    run: impl FnOnce(InitialSetupBusy) + Send + 'static,
) -> bool {
    aterm_cli::macos_doctor::lasting_scope_refusal().is_none()
        && take_initial_setup_slot_now()
        && spawn_holding_initial_setup_slot(name, run)
}

/// Spawn `run` on a worker for the setup slot the caller took.
#[cfg(target_os = "macos")]
fn spawn_holding_initial_setup_slot(
    name: &str,
    run: impl FnOnce(InitialSetupBusy) + Send + 'static,
) -> bool {
    if let Ok(mut last) = INITIAL_SETUP_ROWS.lock() {
        *last = InitialSetupRows::default();
    }
    match std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            run(InitialSetupBusy::new());
        }) {
        Ok(_) => true,
        Err(why) => {
            INITIAL_SETUP_BUSY.store(false, std::sync::atomic::Ordering::Release);
            aterm_log::warn!("Setup worker could not start: {why}");
            initial_setup_error(format!("Setup worker could not start: {why}"), None);
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn restart_initial_setup(
    plan: aterm_types::macos_setup::Plan,
    vault_typed: bool,
    password: Option<aterm_secrets::SecretBytes>,
    receipt: aterm_gui::InitialSetupApplyReceipt,
) -> bool {
    if (setup_io().scope_refused)() {
        return false;
    }
    let generation = SETUP_APPLY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    // This launch's startup holds the automatic claim, or is asking for it:
    // the choice waits for it, counted from now, so the startup never shows
    // its offer over the choice, however soon it was about to (design §3.3).
    let awaiting = startup_holds_claim().then(AwaitingClaim::new);
    if take_initial_setup_slot_now() {
        // No pass runs: a run, if any, is parked or backing off, and this
        // choice replaces it once admitted.
        let choice = begin_setup_choice(&plan, true, generation);
        let refused = choice.clone();
        let under_way = UnderWay::new(choice.sent);
        let started = spawn_holding_initial_setup_slot("aterm-setup-apply", move |busy| {
            admit_setup_change(
                &choice,
                vault_typed,
                password,
                receipt,
                busy,
                awaiting,
                under_way,
            );
        });
        if !started {
            // No worker took the choice: it is refused as soon as it was made.
            forget_setup_choice(&refused);
        }
        return started;
    }
    // A choice takes effect at once (System State design §3.3): the run that is going stops at
    // its next step and this plan starts as the next run; a run sent before this choice that has
    // not registered yet (this launch's startup's, waiting for the slot) stops as it registers.
    // A later change overtakes this one; its dropped receipt keeps the page's input.
    let choice = begin_setup_choice(&plan, false, generation);
    let refused = choice.clone();
    stop_runs_sent_through(choice.sent - 1);
    let under_way = UnderWay::new(choice.sent);
    let started = std::thread::Builder::new()
        .name("aterm-setup-apply".into())
        .spawn(move || {
            let busy = wait_for_initial_setup_slot();
            if setup_choice_overtaken(generation) {
                return;
            }
            if let Ok(mut last) = INITIAL_SETUP_ROWS.lock() {
                *last = InitialSetupRows::default();
            }
            admit_setup_change(
                &choice,
                vault_typed,
                password,
                receipt,
                busy,
                awaiting,
                under_way,
            );
        })
        .inspect_err(|why| aterm_log::warn!("Setup worker could not start: {why}"))
        .is_ok();
    if !started {
        forget_setup_choice(&refused);
    }
    started
}

/// A later choice was made after the one numbered `generation`.
#[cfg(target_os = "macos")]
fn setup_choice_overtaken(generation: u64) -> bool {
    SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire) != generation
}

/// Durably admit a person's `choice` as the next run under the automatic claim,
/// holding the setup slot (`busy`), and run it on this thread until it ends.
/// A refused choice is let go ([`forget_setup_choice`]) before the slot is: the
/// run it gives back takes no step before a later choice can stop it.
/// `awaiting` counts the choice as waiting for the startup's claim from the
/// moment it was made, and `under_way` counts its admission for a quit until
/// its run registers or it is let go.
#[cfg(target_os = "macos")]
fn admit_setup_change(
    choice: &SetupChoice,
    vault_typed: bool,
    password: Option<aterm_secrets::SecretBytes>,
    receipt: aterm_gui::InitialSetupApplyReceipt,
    busy: InitialSetupBusy,
    awaiting: Option<AwaitingClaim>,
    under_way: UnderWay,
) {
    // A group choice makes the plan the person's for good; Set up now leaves a
    // provisional plan provisional (design §3.5).
    let restart = if receipt.chosen() {
        aterm_cli::macos_doctor::InitialSetupRestart::chosen()
    } else {
        aterm_cli::macos_doctor::InitialSetupRestart::default()
    };
    let mut host = LiveSetupRestart {
        restart,
        vault_typed,
        observation: busy.observation.clone(),
        generation: choice.generation,
        sent: choice.sent,
        awaiting,
        under_way: Some(under_way),
        startup_held: false,
    };
    if let Some(busy) = run_setup_restart(&mut host, &choice.plan, password, receipt, busy) {
        forget_setup_choice(choice);
        drop(busy);
    }
    // Let go after `forget_setup_choice`: a quit waiting for this admission
    // then waits for the Stop of the run it gave back.
    drop(host);
}

#[cfg(any(target_os = "macos", all(test, unix)))]
trait SetupRestartHost {
    type Claim;
    type Busy;
    type Password;
    type Receipt;

    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    /// Whether this process's startup coordinator held the automatic claim
    /// ([`StartupClaim`]) when the last ask found it taken: it lets go by itself.
    fn startup_holds_claim(&self) -> bool;
    /// Wait a moment before asking for the claim again.
    fn await_claim(&mut self);
    fn persist(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String>;
    fn pending(&self) -> bool;
    fn backoff(&mut self);
    fn admitted(
        &mut self,
        revision: u64,
        claim: Self::Claim,
        busy: Self::Busy,
        password: Self::Password,
        receipt: Self::Receipt,
    );
    fn failure(&mut self, why: &str);
}

/// Admit a person's `plan` holding the setup slot (`busy`). The slot comes back
/// when the plan was not admitted, its receipt dropped (refused); an admitted
/// plan hands it to its run.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn run_setup_restart<H: SetupRestartHost>(
    host: &mut H,
    plan: &aterm_types::macos_setup::Plan,
    password: H::Password,
    receipt: H::Receipt,
    busy: H::Busy,
) -> Option<H::Busy> {
    let claim = loop {
        match host.claim() {
            Ok(Some(claim)) => break claim,
            // This process's own startup coordinator holds it while it reads the
            // plan in force or admits the offer's answer. It lets go by itself:
            // before it offers, before it waits out a failed write, or once its
            // run finds the slot this choice holds. A person's choice waits for
            // it rather than be refused (§3.3).
            Ok(None) if host.startup_holds_claim() => host.await_claim(),
            Ok(None) => {
                host.failure("Another setup is running. Try again when it finishes.");
                return Some(busy);
            }
            Err(why) => {
                host.failure(&why);
                return Some(busy);
            }
        }
    };
    loop {
        match host.persist(plan) {
            Ok(Some(revision)) => {
                host.admitted(revision, claim, busy, password, receipt);
                return None;
            }
            Ok(None) => return Some(busy),
            Err(why) => {
                host.failure(&why);
                if !host.pending() {
                    return Some(busy);
                }
                host.backoff();
            }
        }
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupRestart {
    restart: aterm_cli::macos_doctor::InitialSetupRestart,
    /// The person typed the plan's vault in System State ([`aterm_gui::SetupAnswer`]).
    vault_typed: bool,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
    /// The choice's number ([`SETUP_APPLY_GENERATION`]).
    generation: u64,
    /// Its admission's number ([`SETUP_SENT`]).
    sent: u64,
    /// Held while the choice waits for the startup coordinator's claim.
    awaiting: Option<AwaitingClaim>,
    /// Held until the choice's run registers, or it is let go.
    under_way: Option<UnderWay>,
    /// The startup coordinator held the claim, or asked for it, around the
    /// last ask that found it taken.
    startup_held: bool,
}

#[cfg(target_os = "macos")]
impl SetupRestartHost for LiveSetupRestart {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;
    type Password = Option<aterm_secrets::SecretBytes>;
    type Receipt = aterm_gui::InitialSetupApplyReceipt;

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        // Read before and after the ask: a startup that held the claim during
        // it was counted from before it asked until after it let go.
        let before = startup_holds_claim();
        let asks = STARTUP_CLAIM_ASKS.load(std::sync::atomic::Ordering::Acquire);
        let claimed = (setup_io().claim_rerun)();
        self.startup_held = before
            || startup_holds_claim()
            || STARTUP_CLAIM_ASKS.load(std::sync::atomic::Ordering::Acquire) != asks;
        if !matches!(claimed, Ok(None)) {
            self.awaiting = None;
        }
        claimed
    }

    fn startup_holds_claim(&self) -> bool {
        self.startup_held
    }

    fn await_claim(&mut self) {
        self.awaiting.get_or_insert_with(AwaitingClaim::new);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    fn persist(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        let observed = (setup_io().persist)(&mut self.restart, plan, self.vault_typed);
        self.observation = observed.observation;
        observed.result
    }

    fn pending(&self) -> bool {
        self.restart.pending()
    }

    fn backoff(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    fn admitted(
        &mut self,
        revision: u64,
        claim: Self::Claim,
        busy: Self::Busy,
        password: Self::Password,
        receipt: Self::Receipt,
    ) {
        // Registered at once, holding the slot and the claim: a Stop from now stops this run,
        // saved by the worker started here. A choice or a Stop made while this plan was being
        // admitted stops it before its first stage.
        register_admitted_run(revision, self.sent, setup_choice_overtaken(self.generation));
        drop(self.under_way.take());
        clear_initial_setup_rows();
        receipt.accept();
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        // A worker parked on the replaced revision wakes, finds it gone, and ends.
        wake_setup();
        (setup_io().run)(Some(claim), revision, password, busy);
    }

    fn failure(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
    }
}

#[cfg(all(test, unix))]
mod setup_restart_tests {
    use super::{SetupRestartHost, run_setup_restart};
    use aterm_types::macos_setup::{Plan, Selection};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Default)]
    struct State {
        alive: Vec<&'static str>,
        drops: Vec<&'static str>,
        accepted: usize,
        refused: usize,
    }

    struct Lease {
        name: &'static str,
        state: Rc<RefCell<State>>,
    }

    impl Lease {
        fn new(name: &'static str, state: &Rc<RefCell<State>>) -> Self {
            state.borrow_mut().alive.push(name);
            Self {
                name,
                state: Rc::clone(state),
            }
        }
    }

    impl Drop for Lease {
        fn drop(&mut self) {
            let mut state = self.state.borrow_mut();
            state.alive.retain(|name| *name != self.name);
            state.drops.push(self.name);
        }
    }

    struct Password {
        secret: aterm_secrets::SecretBytes,
        _lease: Lease,
    }

    struct Receipt {
        lease: Lease,
        accepted: bool,
    }

    impl Drop for Receipt {
        fn drop(&mut self) {
            if !self.accepted {
                self.lease.state.borrow_mut().refused += 1;
            }
        }
    }

    struct Host {
        state: Rc<RefCell<State>>,
        outcomes: VecDeque<Admission>,
        pending: bool,
        plans: Vec<Plan>,
        claims: usize,
        /// Who holds the claim at each look before it is free.
        held: VecDeque<Holder>,
        /// The last look found this process's startup coordinator holding it.
        startup_holding: bool,
        awaits: usize,
        backoffs: usize,
        dispatches: Vec<u64>,
        failures: Vec<String>,
        /// The slot came back: the plan was not admitted.
        slot_back: bool,
    }

    #[derive(Clone, Copy)]
    enum Holder {
        /// This process's startup coordinator ([`super::StartupClaim`]).
        Startup,
        /// Another aterm process, or `aterm doctor`.
        Elsewhere,
    }

    enum Admission {
        Pending(&'static str),
        Rejected(&'static str),
        Admitted(u64),
        Superseded,
    }

    impl SetupRestartHost for Host {
        type Claim = Lease;
        type Busy = Lease;
        type Password = Password;
        type Receipt = Receipt;

        fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
            self.claims += 1;
            assert!(
                !self.state.borrow().alive.contains(&"claim"),
                "retain the original automatic claim"
            );
            if let Some(holder) = self.held.pop_front() {
                self.startup_holding = matches!(holder, Holder::Startup);
                return Ok(None);
            }
            Ok(Some(Lease::new("claim", &self.state)))
        }

        fn startup_holds_claim(&self) -> bool {
            self.startup_holding
        }

        fn await_claim(&mut self) {
            // The choice keeps its slot, receipt and password while it waits.
            assert_eq!(self.state.borrow().alive.len(), 3);
            self.awaits += 1;
        }

        fn persist(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
            assert_eq!(self.state.borrow().alive.len(), 4);
            self.plans.push(plan.clone());
            let outcome = self
                .outcomes
                .pop_front()
                .expect("scripted admission attempt");
            self.pending = matches!(outcome, Admission::Pending(_));
            match outcome {
                Admission::Pending(why) | Admission::Rejected(why) => Err(why.into()),
                Admission::Admitted(revision) => Ok(Some(revision)),
                Admission::Superseded => Ok(None),
            }
        }

        fn pending(&self) -> bool {
            self.pending
        }

        fn backoff(&mut self) {
            assert!(self.pending);
            assert_eq!(self.state.borrow().alive.len(), 4);
            assert_eq!(self.state.borrow().accepted, 0);
            assert_eq!(self.state.borrow().refused, 0);
            self.backoffs += 1;
        }

        fn admitted(
            &mut self,
            revision: u64,
            claim: Self::Claim,
            busy: Self::Busy,
            password: Self::Password,
            mut receipt: Self::Receipt,
        ) {
            assert_eq!(self.state.borrow().alive.len(), 4);
            assert_eq!(password.secret.as_slice(), b"retained password");
            self.dispatches.push(revision);
            receipt.accepted = true;
            self.state.borrow_mut().accepted += 1;
            drop(claim);
            drop(busy);
            drop(password);
            drop(receipt);
        }

        fn failure(&mut self, why: &str) {
            self.failures.push(why.into());
        }
    }

    fn run(plan: &Plan, outcomes: Vec<Admission>) -> Host {
        let host = run_held(plan, Vec::new(), outcomes);
        assert_eq!(host.state.borrow().drops[0], "claim");
        host
    }

    /// [`run`] where the claim is held by `held`, look by look, before it is free.
    fn run_held(plan: &Plan, held: Vec<Holder>, outcomes: Vec<Admission>) -> Host {
        let state = Rc::new(RefCell::new(State::default()));
        let password = Password {
            secret: aterm_secrets::SecretBytes::from(b"retained password".as_slice()),
            _lease: Lease::new("password", &state),
        };
        let receipt = Receipt {
            lease: Lease::new("receipt", &state),
            accepted: false,
        };
        let busy = Lease::new("busy", &state);
        let mut host = Host {
            state,
            outcomes: outcomes.into(),
            pending: false,
            plans: Vec::new(),
            claims: 0,
            held: held.into(),
            startup_holding: false,
            awaits: 0,
            backoffs: 0,
            dispatches: Vec::new(),
            failures: Vec::new(),
            slot_back: false,
        };
        let slot = run_setup_restart(&mut host, plan, password, receipt, busy);
        // A refusal's receipt is dropped before its slot comes back.
        assert_eq!(slot.is_some(), host.state.borrow().alive == ["busy"]);
        host.slot_back = slot.is_some();
        drop(slot);
        assert!(host.state.borrow().alive.is_empty());
        host
    }

    #[test]
    fn a_choice_waits_for_the_startup_claim_here_and_is_refused_for_anothers() {
        // This process's startup coordinator holds the claim while it reads
        // the plan in force: the choice waits, keeping its slot, and is
        // admitted once the coordinator lets go.
        let host = run_held(
            &Plan::default(),
            vec![Holder::Startup, Holder::Startup],
            vec![Admission::Admitted(5)],
        );
        assert_eq!(host.claims, 3);
        assert_eq!(host.awaits, 2);
        assert_eq!(host.dispatches, [5]);
        assert!(host.failures.is_empty());
        assert_eq!(host.state.borrow().accepted, 1);
        assert!(!host.slot_back);
        // Another process's setup, or `aterm doctor`, refuses it at once.
        let host = run_held(&Plan::default(), vec![Holder::Elsewhere], Vec::new());
        assert_eq!((host.claims, host.awaits), (1, 0));
        assert!(host.plans.is_empty() && host.dispatches.is_empty());
        assert_eq!(
            host.failures,
            ["Another setup is running. Try again when it finishes."]
        );
        assert_eq!(host.state.borrow().refused, 1);
        assert!(host.slot_back, "a refused choice gets its slot back");
    }

    #[test]
    fn manual_final_admission_retains_the_original_choice_until_confirmation() {
        let plan = Plan {
            vault_url: "https://vault.example/reviewed".into(),
            ..Plan::default()
        };
        let host = run(
            &plan,
            vec![
                Admission::Pending("published final admission could not flush"),
                Admission::Pending("the same final admission is still unconfirmed"),
                Admission::Admitted(8),
            ],
        );
        assert_eq!(host.dispatches, [8]);
        assert_eq!(host.plans, vec![plan; 3]);
        assert_eq!(host.claims, 1);
        assert_eq!(host.backoffs, 2);
        assert_eq!(host.failures.len(), 2);
        assert_eq!(host.state.borrow().accepted, 1);
        assert!(!host.slot_back);
        assert_eq!(host.state.borrow().refused, 0);
    }

    #[test]
    fn rejected_manual_admission_does_not_accept_an_older_same_plan() {
        let host = run(
            &Plan::default(),
            vec![Admission::Rejected("pre-final write refused")],
        );
        assert!(host.dispatches.is_empty());
        assert_eq!(host.plans.len(), 1);
        assert_eq!(host.backoffs, 0);
        assert_eq!(host.state.borrow().accepted, 0);
        assert_eq!(host.state.borrow().refused, 1);
        assert!(host.slot_back);
    }

    #[test]
    fn a_superseded_manual_attempt_never_dispatches_or_accepts_its_receipt() {
        for pending in [false, true] {
            let mut outcomes = Vec::new();
            if pending {
                outcomes.push(Admission::Pending("the final admission needs confirmation"));
            }
            outcomes.push(Admission::Superseded);
            let host = run(&Plan::default(), outcomes);
            assert!(host.dispatches.is_empty());
            assert_eq!(host.plans.len(), 1 + usize::from(pending));
            assert_eq!(host.failures.len(), usize::from(pending));
            assert_eq!(host.backoffs, usize::from(pending));
            assert_eq!(host.state.borrow().accepted, 0);
            assert_eq!(host.state.borrow().refused, 1);
            assert!(host.slot_back);
        }
    }

    #[test]
    fn a_confirmed_empty_manual_plan_acknowledges_its_exact_revision() {
        let plan = Plan {
            selection: Selection::none(),
            ..Plan::default()
        };
        let host = run(
            &plan,
            vec![
                Admission::Pending("empty admission was published before its flush failed"),
                Admission::Admitted(12),
            ],
        );
        assert_eq!(host.dispatches, [12]);
        assert_eq!(host.plans, [plan.clone(), plan]);
        assert_eq!(host.backoffs, 1);
        assert_eq!(host.state.borrow().accepted, 1);
        assert!(!host.slot_back);
        assert_eq!(host.state.borrow().refused, 0);
    }
}

#[cfg(target_os = "macos")]
fn check_initial_setup(plan: aterm_types::macos_setup::Plan) -> aterm_gui::CheckStarted {
    // Check again reads every row, GitHub repositories included, and lets a run
    // parked on a person look again (System State design §3.5).
    let reading = plan.clone();
    begin_check_again(
        move || {
            // Spotlight is searched anew for the Rust in Spotlight row: the search the
            // reading and the check would share otherwise, found or failed, stands
            // five minutes.
            aterm_machines::init_spotlight::search_again();
            request_observation(
                Some(reading),
                aterm_types::macos_setup::Subject::all(),
                true,
            )
        },
        wake_setup,
        move || {
            spawn_initial_setup_operation("aterm-setup-check", move |_busy| {
                let observed = aterm_cli::macos_doctor::check_initial_setup_observed(&plan);
                match observed.result {
                    Ok(report) => {
                        remember_initial_setup_rows(&report, Some(&plan), observed.observation)
                    }
                    Err(why) => publish_initial_setup_error(why, Some(&plan), observed.observation),
                }
            })
        },
    )
}

/// Check again: a forced reading of every row, a wake for a parked run, and the
/// check on the setup slot, each said apart (design §3.2). A pass holding the
/// slot refuses the check: Check again still started, since its reading ran,
/// but the window never labels that pass "Checking this Mac…".
#[cfg(any(target_os = "macos", test))]
fn begin_check_again(
    observe: impl FnOnce() -> bool,
    wake: impl FnOnce(),
    check: impl FnOnce() -> bool,
) -> aterm_gui::CheckStarted {
    let reading = observe();
    if !reading {
        aterm_log::info!("system state: the forced reading did not start");
    }
    wake();
    aterm_gui::CheckStarted {
        reading,
        check: check(),
    }
}

#[cfg(target_os = "macos")]
fn with_restore_readings<T>(restore: impl FnOnce() -> T) -> T {
    use aterm_types::macos_setup::{Group, Setting, Subject};
    let _readings = PreferenceReadings::begin(
        Setting::ALL
            .into_iter()
            .filter(|setting| matches!(setting.group(), Group::Input | Group::Desktop))
            .map(Subject::Setting)
            .collect(),
    );
    restore()
}

/// A preference operation holds the setup slot. Retire its old readings before
/// any write, and read them again on success, error or unwind once its effects
/// are no longer in flight.
#[cfg(target_os = "macos")]
struct PreferenceReadings {
    subjects: Vec<aterm_types::macos_setup::Subject>,
}

#[cfg(target_os = "macos")]
impl PreferenceReadings {
    fn begin(subjects: Vec<aterm_types::macos_setup::Subject>) -> Self {
        let guard = Self { subjects };
        {
            let mut activity = SETUP_ACTIVITY
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // These subjects change outside a setup stage: keep the operation's
            // own label, without a run's meter or Stop.
            activity.mark(&[], &guard.subjects, true);
            let mut rows = OBSERVED_ROWS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let before = rows.rows.len();
            rows.rows
                .retain(|row| !guard.subjects.contains(&row.subject));
            if rows.rows.len() != before {
                rows.generation += 1;
            }
        }
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        guard
    }
}

#[cfg(target_os = "macos")]
impl Drop for PreferenceReadings {
    fn drop(&mut self) {
        SETUP_ACTIVITY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mark(&[], &self.subjects, false);
        request_observation(None, std::mem::take(&mut self.subjects), false);
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

#[cfg(target_os = "macos")]
fn restore_initial_setup() -> bool {
    spawn_initial_setup_operation("aterm-setup-restore", |_busy| {
        let observed = with_restore_readings(|| {
            aterm_cli::macos_doctor::run_observed(aterm_cli::macos_doctor::Action::Restore)
        });
        match observed.result {
            Ok(report) => remember_initial_setup_rows(&report, None, observed.observation),
            Err(why) => initial_setup_error(why, observed.observation),
        }
    })
}

#[cfg(all(test, target_os = "macos"))]
mod initial_setup_result_tests {
    use super::*;
    use aterm_cli::macos_doctor::{
        InitialSetupObservation, InitialSetupSnapshot, InitialSetupSnapshotError,
        initial_setup_snapshot_at,
    };
    use aterm_types::macos_setup::Plan;
    use std::path::Path;

    fn publish_decision(root: &Path, state: &str) {
        let next = root.join("decision.next");
        std::fs::write(
            &next,
            format!(r#"{{"profile":"macos-workstation-v1","state":"{state}"}}"#),
        )
        .unwrap();
        std::fs::rename(next, root.join("first-run.json")).unwrap();
    }

    fn snapshot(root: &Path) -> InitialSetupSnapshot {
        initial_setup_snapshot_at(root, Plan::default()).unwrap()
    }

    fn cached(observation: Option<InitialSetupObservation>, checked: bool) -> InitialSetupRows {
        let row = aterm_gui::SetupStatusRow {
            name: "Earlier result".into(),
            status: "blocked".into(),
            detail: "approval code OLD42".into(),
        };
        if checked {
            InitialSetupRows {
                observation,
                check: Some(aterm_gui::SetupCheck {
                    plan: Plan::default(),
                    rows: vec![row],
                }),
                ..InitialSetupRows::default()
            }
        } else {
            InitialSetupRows {
                observation,
                rows: vec![row],
                ..InitialSetupRows::default()
            }
        }
    }

    fn has_old_result(status: &aterm_gui::SetupStatus) -> bool {
        status
            .rows
            .iter()
            .chain(status.check.iter().flat_map(|check| check.rows.iter()))
            .any(|row| row.detail.contains("OLD42"))
    }

    #[test]
    fn cached_setup_results_follow_the_exact_record_publication() {
        for checked in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            publish_decision(root.path(), "pending");
            let origin = snapshot(root.path());
            let last = cached(Some(origin.observation.clone()), checked);
            let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
            assert!(
                has_old_result(&status),
                "an unchanged poll keeps its result"
            );

            publish_decision(root.path(), "restored");
            let stopped = snapshot(root.path());
            assert_eq!(stopped.revision, origin.revision);
            let status = initial_setup_status_with(Ok(stopped), false, &last);
            assert!(!has_old_result(&status));
            assert!(status.finished);
            assert!(!status.permissions.unwrap().allowed);

            // A delayed publisher must retain its original context, even if a
            // newer state is already visible when it finishes publishing.
            let delayed = cached(Some(origin.observation), checked);
            assert!(!has_old_result(&initial_setup_status_with(
                Ok(snapshot(root.path())),
                false,
                &delayed,
            )));
        }
    }

    #[test]
    fn a_stale_snapshot_cannot_evict_a_newer_setup_result() {
        let root = aterm_tempfile::tempdir().unwrap();
        publish_decision(root.path(), "pending");
        let old = snapshot(root.path());
        publish_decision(root.path(), "restored");
        let current = snapshot(root.path());
        let last = cached(Some(current.observation), false);

        // The slow reader collected the old state before the worker published
        // this result. It can filter that result, but must not delete it.
        assert!(!has_old_result(&initial_setup_status_with(
            Ok(old),
            false,
            &last,
        )));
        assert!(has_old_result(&initial_setup_status_with(
            Ok(snapshot(root.path())),
            false,
            &last,
        )));
    }

    #[test]
    fn unknown_setup_state_never_borrows_an_old_results_context() {
        let root = aterm_tempfile::tempdir().unwrap();
        publish_decision(root.path(), "pending");
        let last = cached(Some(snapshot(root.path()).observation), true);
        for error in [
            InitialSetupSnapshotError::Updating,
            InitialSetupSnapshotError::Read("current records cannot be read".into()),
        ] {
            let status = initial_setup_status_with(Err(error), true, &last);
            assert!(!has_old_result(&status));
            assert!(status.permissions.is_none());
            assert!(status.busy);
            assert_eq!(status.rows.len(), 1);
        }
        let unbound = cached(None, false);
        assert!(!has_old_result(&initial_setup_status_with(
            Ok(snapshot(root.path())),
            false,
            &unbound,
        )));
    }

    /// A Restore row for a setting back as saved that loads at the next login is no failure:
    /// it reaches the page as itself, which says to log out without counting it failed.
    #[test]
    fn a_restore_row_back_at_the_next_login_reaches_the_page_as_itself() {
        use aterm_cli::macos_doctor::{Action, LOGIN, Report, Row};
        let report = Report {
            action: Action::Restore,
            rows: vec![Row {
                name: "Caps Lock key".into(),
                status: LOGIN.into(),
                detail: "log out and back in to load the saved mappings".into(),
            }],
            manual_steps: Vec::new(),
            backup: std::path::PathBuf::from("original-preferences.json"),
        };
        let rows = initial_setup_report_rows(&report);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.status.as_str()))
                .collect::<Vec<_>>(),
            [("Caps Lock key", "login")]
        );
    }

    #[test]
    fn a_blocked_restore_remains_visible_as_a_bound_operation_failure() {
        use aterm_cli::macos_doctor::{Action, Report, Row};
        const WHY: &str = "The original Widgets value could not be read; it was left unchanged.";
        for action in [Action::Apply, Action::Check, Action::Restore] {
            let root = aterm_tempfile::tempdir().unwrap();
            publish_decision(root.path(), "restored");
            let current = snapshot(root.path());
            let report = Report {
                action,
                rows: vec![Row {
                    name: "Widgets".into(),
                    status: "blocked".into(),
                    detail: WHY.into(),
                }],
                manual_steps: Vec::new(),
                backup: root.path().join("original-preferences.json"),
            };
            let rows = initial_setup_report_rows(&report);
            let last = if action == Action::Check {
                InitialSetupRows {
                    observation: Some(current.observation),
                    check: Some(aterm_gui::SetupCheck {
                        plan: current.plan,
                        rows,
                    }),
                    ..InitialSetupRows::default()
                }
            } else {
                InitialSetupRows {
                    observation: Some(current.observation),
                    rows,
                    ..InitialSetupRows::default()
                }
            };
            let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
            let row = status
                .rows
                .iter()
                .chain(status.check.iter().flat_map(|check| check.rows.iter()))
                .find(|row| row.detail == WHY)
                .expect("the result belongs to the unchanged publication");
            let expected = match action {
                Action::Apply => ("Widgets", "blocked"),
                Action::Check => ("Check: Widgets", "blocked"),
                Action::Restore => ("Restore: Widgets", "failed"),
            };
            assert_eq!((row.name.as_str(), row.status.as_str()), expected);
            assert_eq!(
                report.rows[0].status, "blocked",
                "CLI semantics stay intact"
            );
            assert!(report.needs_attention());

            publish_decision(root.path(), "cancelled");
            let next = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
            assert!(
                next.rows.iter().all(|row| row.detail != WHY)
                    && next
                        .check
                        .as_ref()
                        .is_none_or(|check| check.rows.iter().all(|row| row.detail != WHY)),
                "an older Restore result cannot follow a newer publication"
            );
        }
    }

    use aterm_types::macos_setup::{Group, Observation, Setting, Subject};
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    /// The tests below share the worker's process-wide state: its registration,
    /// its readings, the setup slot and [`SETUP_IO`].
    static SETUP_GLOBALS: Mutex<()> = Mutex::new(());

    fn globals() -> MutexGuard<'static, ()> {
        SETUP_GLOBALS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A wait for something that must happen: a hang detector, not a budget.
    const HANG: Duration = Duration::from_secs(60);

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + HANG;
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What the worker asked of the outside under [`TestIo`], and how it answers.
    struct Script {
        /// Readings started: their plan, their rows, and whether they used the
        /// network.
        reads: Vec<(Plan, Vec<Subject>, bool)>,
        /// Replies bound to each reading when it starts, before a held read
        /// can be overtaken by a choice.
        readings: VecDeque<Vec<Observation>>,
        /// Readings wait while this is set.
        hold: bool,
        /// Stop saves begun: the run each names and the thread it ran on.
        stop_attempts: Vec<(u64, std::thread::ThreadId)>,
        /// Stop saves that landed.
        stops: Vec<u64>,
        /// Stop saves wait while this is set, as a loaded disk holds them.
        stop_hold: bool,
        /// Stop saves that fail before one lands.
        stop_failures: usize,
        /// A person's choice asks for the claim only once this is clear.
        claim_hold: bool,
        /// Admissions of a person's choice begun.
        persists: usize,
        /// Admissions wait while this is set.
        persist_hold: bool,
        /// What each admission does in turn: admits its choice as this
        /// revision, or refuses it (`None`, and when none is left).
        admits: VecDeque<Option<u64>>,
        /// A run that registers while a choice is being refused.
        registers: Option<u64>,
        /// The runs admitted choices started.
        runs: Vec<u64>,
        /// The revision in force: the last one admitted.
        revision: u64,
        /// The startup's read of the plan in force waits while this is set.
        initial_hold: bool,
        /// The startup is reading the plan in force, holding its claim.
        startup_reading: bool,
        /// Offers the startup showed.
        offers: usize,
        /// The real request sent its answer, before the startup reads it.
        startup_answer_delivered: bool,
        /// Hold the offer after delivery, before it returns to the coordinator.
        startup_answer_hold: bool,
        /// What each write of the page's answer does in turn: admits it as
        /// this revision (`Ok(Some)`), finds it retired (`Ok(None)`), or
        /// fails while the startup keeps its claim (`Err`).
        authorizations: VecDeque<Result<Option<u64>, String>>,
        /// Writes of the page's answer begun.
        authorizing: usize,
        /// Those writes wait while this is set.
        authorize_hold: bool,
        /// The startup's waits before it writes its answer again, holding
        /// its claim, begun.
        retry_waits: usize,
        /// The startup's waits holding no claim last while this is set, as
        /// a minute does.
        startup_backoff_hold: bool,
        /// What the startup reported failing.
        startup_failures: Vec<String>,
    }

    impl Script {
        const NEW: Self = Self {
            reads: Vec::new(),
            readings: VecDeque::new(),
            hold: false,
            stop_attempts: Vec::new(),
            stops: Vec::new(),
            stop_hold: false,
            stop_failures: 0,
            claim_hold: false,
            persists: 0,
            persist_hold: false,
            admits: VecDeque::new(),
            registers: None,
            runs: Vec::new(),
            revision: 1,
            initial_hold: false,
            startup_reading: false,
            offers: 0,
            startup_answer_delivered: false,
            startup_answer_hold: false,
            authorizations: VecDeque::new(),
            authorizing: 0,
            authorize_hold: false,
            retry_waits: 0,
            startup_backoff_hold: false,
            startup_failures: Vec::new(),
        };
    }

    static SCRIPT: Mutex<Script> = Mutex::new(Script::NEW);
    static SCRIPT_MOVED: Condvar = Condvar::new();

    fn script() -> MutexGuard<'static, Script> {
        SCRIPT.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the script and wake whatever waits on it.
    fn change(edit: impl FnOnce(&mut Script)) {
        edit(&mut script());
        SCRIPT_MOVED.notify_all();
    }

    /// Wait while `held` says so (a hang detector bounds it).
    fn wait_while(
        mut script: MutexGuard<'static, Script>,
        held: impl Fn(&Script) -> bool,
    ) -> MutexGuard<'static, Script> {
        let deadline = Instant::now() + HANG;
        while held(&script) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            script = SCRIPT_MOVED
                .wait_timeout(script, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        script
    }

    fn release_readings() {
        change(|script| script.hold = false);
    }

    fn observe_idle() -> bool {
        OBSERVE_QUEUE.lock().is_ok_and(|queue| !queue.running)
    }

    /// No Stop waits for the Stop's worker, nor is being saved.
    fn stop_worker_idle() -> bool {
        let saves = stop_saves();
        saves.saving.is_none() && saves.wanted.is_none()
    }

    fn reset_worker_state() {
        set_setup_run(None);
        *SETUP_STOP.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // The worker, once started, lives on for the next test.
        let mut saves = stop_saves();
        *saves = StopSaves {
            worker: saves.worker,
            ..StopSaves::default()
        };
        drop(saves);
        OBSERVED_ROWS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .plan = None;
    }

    /// The folder [`TestIo`]'s claims are taken in.
    static CLAIM_ROOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

    /// The worker's outside, scripted for a test that holds [`globals`]: it
    /// reads no row of the Mac and writes nothing to the person's state root.
    /// Its automatic claim is the real lock, in a scratch folder. Dropped, it
    /// waits for the readings, the choice and the Stop the test started to end
    /// before the live [`SetupIo`] is back.
    struct TestIo {
        _claims: aterm_tempfile::TempDir,
    }

    impl TestIo {
        fn install() -> Self {
            let claims = aterm_tempfile::tempdir().unwrap();
            *CLAIM_ROOT.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(claims.path().to_owned());
            *script() = Script::NEW;
            *SETUP_IO.write().unwrap_or_else(PoisonError::into_inner) = SetupIo {
                scope_refused: || false,
                read: Self::read,
                save_stop: Self::save_stop,
                claim_automatic: Self::claim,
                claim_rerun: Self::claim_rerun,
                persist: Self::persist,
                run: Self::run,
            };
            reset_worker_state();
            Self { _claims: claims }
        }

        fn read(plan: &Plan, subjects: &[Subject], network: bool) -> Vec<Observation> {
            let mut script = script();
            script
                .reads
                .push((plan.clone(), subjects.to_vec(), network));
            let reading = script.readings.pop_front().unwrap_or_default();
            SCRIPT_MOVED.notify_all();
            drop(wait_while(script, |script| script.hold));
            reading
        }

        fn save_stop(revision: u64) -> Result<(), String> {
            let mut script = script();
            script
                .stop_attempts
                .push((revision, std::thread::current().id()));
            SCRIPT_MOVED.notify_all();
            let mut script = wait_while(script, |script| script.stop_hold);
            if script.stop_failures > 0 {
                script.stop_failures -= 1;
                return Err("the disk is full".into());
            }
            script.stops.push(revision);
            SCRIPT_MOVED.notify_all();
            Ok(())
        }

        fn claim() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String> {
            let root = CLAIM_ROOT
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .expect("TestIo's claim folder");
            aterm_cli::macos_doctor::claim_initial_setup_rerun_at(&root)
        }

        fn claim_rerun() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String> {
            drop(wait_while(script(), |script| script.claim_hold));
            Self::claim()
        }

        fn persist(
            _: &mut aterm_cli::macos_doctor::InitialSetupRestart,
            _: &Plan,
            _: bool,
        ) -> aterm_cli::macos_doctor::InitialSetupObserved<Option<u64>> {
            let mut script = script();
            script.persists += 1;
            SCRIPT_MOVED.notify_all();
            let mut script = wait_while(script, |script| script.persist_hold);
            let admitted = script.admits.pop_front().flatten();
            let registers = script.registers;
            if let Some(revision) = admitted {
                script.revision = revision;
            }
            drop(script);
            let result = match admitted {
                Some(revision) => Ok(Some(revision)),
                None => {
                    if let Some(revision) = registers {
                        set_setup_run(Some(revision));
                    }
                    Err("the plan could not be saved".into())
                }
            };
            aterm_cli::macos_doctor::InitialSetupObserved {
                result,
                observation: None,
            }
        }

        fn run(
            claim: Option<aterm_cli::macos_doctor::AutomaticSetupClaim>,
            revision: u64,
            _: Option<aterm_secrets::SecretBytes>,
            busy: InitialSetupBusy,
        ) {
            change(|script| script.runs.push(revision));
            drop(claim);
            drop(busy);
        }
    }

    impl Drop for TestIo {
        fn drop(&mut self) {
            change(|script| {
                script.hold = false;
                script.stop_hold = false;
                script.claim_hold = false;
                script.persist_hold = false;
                script.initial_hold = false;
                script.authorize_hold = false;
                script.startup_backoff_hold = false;
                script.startup_answer_hold = false;
            });
            let deadline = Instant::now() + HANG;
            while !(observe_idle()
                && stop_worker_idle()
                && !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire))
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            *SETUP_IO.write().unwrap_or_else(PoisonError::into_inner) = SetupIo::LIVE;
            reset_worker_state();
        }
    }

    /// Every group set up but Workspace and Messages & FaceTime, the two whose
    /// readings use the network and the person's own account state.
    fn left_as_is() -> Plan {
        let mut plan = Plan::default();
        for setting in [
            Setting::Spotlight,
            Setting::Secrets,
            Setting::Repositories,
            Setting::Messages,
            Setting::FaceTime,
        ] {
            plan.selection.set(setting, false);
        }
        plan
    }

    #[test]
    fn stop_hands_its_save_to_a_worker_and_a_quit_waits_for_it() {
        let _globals = globals();
        let _io = TestIo::install();
        // No run is registered here: nothing stops, and nothing is saved.
        assert!(!stop_initial_setup());
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(script().stop_attempts.is_empty());
        // Run 41 is going, and the disk is slow. Stop returns while its save
        // waits: it never runs on the thread that asked, the UI's.
        change(|script| script.stop_hold = true);
        set_setup_run(Some(41));
        assert!(stop_initial_setup());
        // The pass in this process stops at its next boundary from memory.
        assert!(setup_stop_requested(41));
        wait_until("the Stop's worker begins its save", || {
            !script().stop_attempts.is_empty()
        });
        let (revision, thread) = script().stop_attempts[0];
        assert_eq!(revision, 41);
        assert_ne!(thread, std::thread::current().id());
        // Stop pressed again: the save in flight is not begun again.
        assert!(stop_initial_setup());
        // A quit waits for the save.
        assert!(
            !settle_setup_stop(Duration::ZERO),
            "the save is still going"
        );
        let quit = std::thread::spawn(|| settle_setup_stop(HANG));
        change(|script| script.stop_hold = false);
        assert!(quit.join().unwrap());
        assert_eq!(script().stops, [41]);
        // A saved Stop is saved once per run.
        assert!(stop_initial_setup());
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stop_attempts.len(), 1);
    }

    #[test]
    fn a_stop_whose_save_failed_is_saved_on_the_next_ask_or_at_quit() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| script.stop_failures = 1);
        set_setup_run(Some(41));
        assert!(stop_initial_setup());
        wait_until("the failed save ends", stop_worker_idle);
        assert_eq!(script().stop_attempts.len(), 1);
        assert!(script().stops.is_empty());
        // Stop pressed again: the save is made again.
        assert!(stop_initial_setup());
        wait_until("the second save ends", stop_worker_idle);
        assert_eq!(script().stops, [41]);
        assert_eq!(script().stop_attempts.len(), 2);
        // A save that fails just before a quit is made again by the quit.
        change(|script| script.stop_failures = 1);
        set_setup_run(Some(42));
        assert!(stop_initial_setup());
        wait_until("the failed save ends", stop_worker_idle);
        assert_eq!(script().stops, [41]);
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stops, [41, 42]);
    }

    #[test]
    fn a_choice_made_while_a_replacing_one_is_admitted_stops_the_run_given_back() {
        let _globals = globals();
        let _io = TestIo::install();
        // Run 7 is parked. The person's first choice takes the free slot and
        // drops run 7's registration; its admission is slow, and refuses it.
        set_setup_run(Some(7));
        change(|script| {
            script.persist_hold = true;
            script.admits = [None, Some(9)].into();
        });
        let (receipt, first) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        wait_until("the first choice's admission begins", || {
            script().persists == 1
        });
        // The person chooses again meanwhile: no run is registered, so the
        // second choice stops nothing yet, and waits for the slot.
        let (receipt, second) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(Plan::default(), false, None, receipt));
        assert!(!setup_stop_requested(7));
        // The first is refused: run 7 is given back, stopped before the slot
        // is free for it to take a step, and the second choice runs next.
        change(|script| script.persist_hold = false);
        assert_eq!(first.recv_timeout(HANG), Ok(false));
        assert_eq!(second.recv_timeout(HANG), Ok(true));
        assert!(setup_stop_requested(7));
        assert_eq!(setup_run(), Some(9));
        // The admitted choice runs once its receipt is accepted.
        wait_until("run 9 starts", || !script().runs.is_empty());
        assert_eq!(script().runs, [9]);
        wait_until("run 7's Stop is saved", stop_worker_idle);
        assert_eq!(script().stops, [7]);
    }

    /// This launch's startup as `prepare_setup` drives it, on the live claim
    /// ([`StartupClaim`]), the live count of choices waiting for it
    /// ([`choice_awaits_claim`]) and the live numbering of what it sends
    /// ([`StartupSent`]); its reads and writes are [`TestIo`]'s script.
    struct Startup {
        /// Its offer waits for the page's answer until it is withdrawn
        /// ([`startup_offer_withdrawn`]); otherwise nobody answers it.
        awaits_answer: bool,
        /// The page's answer, when the page answers at once.
        answer: Option<Plan>,
        sent: StartupSent,
    }

    impl SetupStartup for Startup {
        type Claim = StartupClaim;

        fn send(&mut self) {
            self.sent.send();
        }
        fn answered(&mut self) {
            self.sent.answered();
        }
        fn bind_authorization(&mut self) -> Result<u64, String> {
            Ok(1)
        }
        fn revision(&mut self) -> Result<u64, String> {
            Ok(script().revision)
        }
        fn claim(&mut self) -> Result<Option<StartupClaim>, String> {
            StartupClaim::claim()
        }
        fn finished(&mut self) -> Result<bool, String> {
            Ok(false)
        }
        fn choice_awaits_claim(&mut self) -> bool {
            choice_awaits_claim()
        }
        fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
            Ok(None)
        }
        fn initial(&mut self) -> Result<Plan, String> {
            change(|script| script.startup_reading = true);
            let mut script = wait_while(script(), |script| script.initial_hold);
            script.startup_reading = false;
            Ok(Plan::default())
        }
        fn offer(&mut self, initial: Plan) -> aterm_gui::WorkstationSetupDecision {
            change(|script| script.offers += 1);
            if let Some(answer) = self.answer.take() {
                if script().startup_answer_hold {
                    return aterm_gui::drive_initial_setup_answer_for_test(
                        initial,
                        self.sent.handoff(),
                        |deliver| {
                            assert!(deliver(
                                aterm_gui::WorkstationSetupDecision::Begin(answer).into()
                            ));
                            change(|script| script.startup_answer_delivered = true);
                            drop(wait_while(script(), |script| script.startup_answer_hold));
                        },
                    )
                    .decision;
                }
                return aterm_gui::WorkstationSetupDecision::Begin(answer);
            }
            if self.awaits_answer {
                wait_until("the offer is withdrawn for the choice", || {
                    startup_offer_withdrawn(|| false)
                });
            }
            aterm_gui::WorkstationSetupDecision::Unavailable
        }
        fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
            change(|script| script.authorizing += 1);
            let mut script = wait_while(script(), |script| script.authorize_hold);
            let written = script
                .authorizations
                .pop_front()
                .expect("a write of the answer was scripted");
            if let Ok(Some(revision)) = written {
                script.revision = revision;
            }
            written
        }
        fn authorization_pending(&self) -> bool {
            true
        }
        fn wait_holding_claim(&mut self) -> bool {
            change(|script| script.retry_waits += 1);
            // Twice the hang detector: a choice that had to wait it out
            // fails the test before the wait ends.
            await_waiting_choice(HANG * 2)
        }
        fn backoff(&mut self) {
            drop(wait_while(script(), |script| script.startup_backoff_hold));
            std::thread::sleep(Duration::from_millis(20));
        }
        fn report_failure(&mut self, why: &str) {
            change(|script| script.startup_failures.push(why.to_owned()));
        }
    }

    /// The startup, on its own thread: what it prepared, and, as the window's
    /// worker does next, that run, started ([`run_prepared_setup`]).
    fn startup_with(
        awaits_answer: bool,
        answer: Option<Plan>,
    ) -> std::thread::JoinHandle<&'static str> {
        std::thread::spawn(move || {
            let mut host = Startup {
                awaits_answer,
                answer,
                sent: StartupSent::default(),
            };
            let prepared = prepare_setup(&mut host);
            let outcome = match &prepared {
                Ok(PreparedSetup::Authorized { .. }) => "authorized",
                Ok(PreparedSetup::Finished) => "finished",
                Ok(PreparedSetup::Unavailable) => "unavailable",
                Err(_) => "failed",
            };
            if let Ok(prepared) = prepared {
                run_prepared_setup(prepared, None, host.sent);
            }
            outcome
        })
    }

    fn startup(awaits_answer: bool) -> std::thread::JoinHandle<&'static str> {
        startup_with(awaits_answer, None)
    }

    #[test]
    fn a_choice_made_while_the_startup_reads_the_plan_waits_for_its_claim_and_no_offer_shows() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| {
            script.initial_hold = true;
            script.claim_hold = true;
            script.admits = [Some(9)].into();
        });
        let startup = startup(false);
        wait_until("the startup reads the plan in force", || {
            script().startup_reading
        });
        assert!(startup_holds_claim());
        // The person chooses meanwhile. The choice waits for the startup's
        // claim, counted from the moment it is made, before its worker asks.
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert!(choice_awaits_claim());
        // The startup lets its claim go rather than offer over the choice;
        // the choice is admitted, and the startup's run is superseded.
        change(|script| {
            script.initial_hold = false;
            script.claim_hold = false;
        });
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        assert_eq!(startup.join().unwrap(), "unavailable");
        assert_eq!(script().offers, 0, "no offer is shown over the choice");
        // The admitted choice runs once its receipt is accepted.
        wait_until("run 9 starts", || !script().runs.is_empty());
        assert_eq!(script().runs, [9]);
        assert!(!choice_awaits_claim() && !startup_holds_claim());
    }

    #[test]
    fn an_offer_waiting_for_its_answer_is_withdrawn_for_a_choice_made_meanwhile() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| script.admits = [Some(9)].into());
        let startup = startup(true);
        wait_until("the startup offers its plan", || script().offers == 1);
        // The offer has not reached the page yet when the person chooses.
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        assert_eq!(startup.join().unwrap(), "unavailable");
        assert_eq!(script().offers, 1, "the offer is not shown again");
        // The admitted choice runs once its receipt is accepted.
        wait_until("run 9 starts", || !script().runs.is_empty());
        assert_eq!(script().runs, [9]);
        assert!(script().startup_failures.is_empty());
    }

    /// A choice waits for at most one read or one write of the startup's (design §3.3): one made
    /// while the startup waits to write its failed answer again gets the claim at once.
    #[test]
    fn a_choice_made_while_the_startup_waits_to_write_its_answer_again_gets_the_claim_at_once() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| {
            script.authorizations = [
                Err("another doctor profile pass is running".into()),
                Ok(None),
            ]
            .into();
            script.startup_backoff_hold = true;
            script.admits = [Some(9)].into();
        });
        let startup = startup_with(false, Some(Plan::default()));
        wait_until("the startup waits to write its answer again", || {
            script().retry_waits == 1
        });
        assert!(startup_holds_claim());
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        // Admitted while the startup's wait (a minute) has not run out.
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        assert_eq!(script().authorizing, 1, "the answer was not written first");
        wait_until("run 9 starts", || !script().runs.is_empty());
        // The startup's wait ends: it takes the claim again, and finds its
        // answer retired by the choice's admission.
        change(|script| script.startup_backoff_hold = false);
        assert_eq!(startup.join().unwrap(), "unavailable");
        assert_eq!(script().authorizing, 2);
        assert_eq!(script().runs, [9]);
        assert_eq!(
            script().startup_failures,
            ["another doctor profile pass is running"]
        );
    }

    /// A Stop pressed while a choice that replaces a parked run is being admitted holds for the
    /// run the choice admits, or for the run a refusal gives back; a quit meanwhile waits until
    /// that Stop is saved.
    #[test]
    fn a_stop_pressed_while_a_choice_is_admitted_holds_for_its_run_and_a_quit_waits_for_it() {
        let _globals = globals();
        let _io = TestIo::install();
        for admitted in [Some(9), None] {
            wait_until("the slot is free", || {
                !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
            });
            reset_worker_state();
            set_setup_run(Some(7));
            change(|script| {
                script.persist_hold = true;
                script.admits = [admitted].into();
                script.persists = 0;
                script.stop_attempts.clear();
                script.stops.clear();
                script.runs.clear();
            });
            let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
            assert!(restart_initial_setup(left_as_is(), false, None, receipt));
            wait_until("the choice's admission begins", || script().persists == 1);
            // Stop: the choice took run 7's registration, so nothing stops now.
            assert!(!stop_initial_setup());
            assert!(script().stop_attempts.is_empty());
            // A quit now waits for what the Stop owes.
            assert!(
                !settle_setup_stop(Duration::ZERO),
                "a quit waits for the admission the Stop covers"
            );
            let quit = std::thread::spawn(|| settle_setup_stop(HANG));
            change(|script| script.persist_hold = false);
            assert_eq!(answer.recv_timeout(HANG), Ok(admitted.is_some()));
            assert!(quit.join().unwrap(), "the quit saw the Stop saved");
            let stopped = admitted.unwrap_or(7);
            assert_eq!(setup_run(), Some(stopped));
            assert!(setup_stop_requested(stopped));
            assert_eq!(script().stops, [stopped]);
            if admitted.is_some() {
                wait_until("run 9 starts, to stop at its first boundary", || {
                    !script().runs.is_empty()
                });
                assert_eq!(script().runs, [9]);
            }
        }
    }

    /// The same while a pass runs: the choice stops run 7 and waits for the slot; its own run
    /// starts stopped as it registers, and a quit before then waits for its Stop.
    #[test]
    fn a_stop_pressed_while_a_choice_waits_for_the_pass_ahead_holds_for_its_run() {
        let _globals = globals();
        let _io = TestIo::install();
        set_setup_run(Some(7));
        assert!(take_initial_setup_slot_now(), "run 7's pass holds the slot");
        change(|script| script.admits = [Some(9)].into());
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert!(setup_stop_requested(7), "the choice stops the pass ahead");
        assert!(stop_initial_setup(), "Stop: run 7 is the run going");
        // The Stop's worker saves the newest Stop handed to it: run 9's,
        // handed before it takes run 7's, would stand for both.
        wait_until("run 7's Stop is saved", || script().stops == [7]);
        assert!(!settle_setup_stop(Duration::ZERO));
        let quit = std::thread::spawn(|| settle_setup_stop(HANG));
        // Run 7's stage ends: it lets the slot go.
        clear_setup_run(7);
        INITIAL_SETUP_BUSY.store(false, std::sync::atomic::Ordering::Release);
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        assert!(quit.join().unwrap());
        assert!(setup_stop_requested(9), "run 9 starts stopped");
        assert_eq!(script().stops, [7, 9]);
    }

    /// Stop pressed while the startup writes the page's answer: its run starts stopped as it
    /// registers, and a quit made meanwhile waits for that.
    #[test]
    fn a_stop_pressed_while_the_startups_answer_is_admitted_holds_for_its_run() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| {
            script.authorizations = [Ok(Some(8))].into();
            script.authorize_hold = true;
        });
        let startup = startup_with(false, Some(Plan::default()));
        wait_until("the startup writes the page's answer", || {
            script().authorizing == 1
        });
        assert!(!stop_initial_setup(), "no run is registered yet");
        assert!(!settle_setup_stop(Duration::ZERO));
        let quit = std::thread::spawn(|| settle_setup_stop(HANG));
        change(|script| script.authorize_hold = false);
        assert!(quit.join().unwrap());
        assert_eq!(startup.join().unwrap(), "authorized");
        assert!(setup_stop_requested(8), "run 8 starts stopped");
        assert_eq!(script().stops, [8]);
        assert_eq!(script().runs, [8]);
    }

    #[test]
    fn a_startup_answer_handoff_keeps_its_stop_owed_before_the_worker_reads_the_answer() {
        let _globals = globals();
        let _io = TestIo::install();
        change(|script| {
            script.authorizations = [Ok(Some(8))].into();
            script.startup_answer_hold = true;
        });
        let startup = startup_with(false, Some(Plan::default()));
        wait_until("the request delivers the page's answer", || {
            script().startup_answer_delivered
        });
        assert!(!stop_initial_setup(), "no run is registered yet");
        let settled_before_return = settle_setup_stop(Duration::ZERO);
        let quit = std::thread::spawn(|| settle_setup_stop(HANG));
        change(|script| script.startup_answer_hold = false);
        assert_eq!(startup.join().unwrap(), "authorized");
        assert!(quit.join().unwrap());
        wait_until("run 8's Stop is saved", || script().stops == [8]);
        assert!(setup_stop_requested(8));
        assert_eq!(script().runs, [8]);
        assert!(
            !settled_before_return,
            "the quit must wait for a delivered answer before the worker reads it"
        );
    }

    #[test]
    fn an_unanswered_startup_answer_handoff_leaves_nothing_for_a_quit_to_wait_for() {
        let _globals = globals();
        let _io = TestIo::install();
        let mut sent = StartupSent::default();
        sent.send();
        let answer = aterm_gui::drive_initial_setup_answer_for_test(
            Plan::default(),
            sent.handoff(),
            |deliver| {
                assert!(!stop_initial_setup());
                assert!(settle_setup_stop(Duration::ZERO));
                assert!(deliver(
                    aterm_gui::WorkstationSetupDecision::Unavailable.into()
                ));
            },
        );
        assert_eq!(
            answer.decision,
            aterm_gui::WorkstationSetupDecision::Unavailable
        );
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(script().stop_attempts.is_empty());
    }

    /// A Stop covers only what was sent before it: pressed before the startup offers or before a
    /// choice is made, it stops neither's run, and a quit owes nothing for them.
    #[test]
    fn a_stop_pressed_before_a_plan_is_sent_never_stops_its_run() {
        let _globals = globals();
        let _io = TestIo::install();
        assert!(!stop_initial_setup());
        change(|script| script.authorizations = [Ok(Some(8))].into());
        assert_eq!(
            startup_with(false, Some(Plan::default())).join().unwrap(),
            "authorized"
        );
        assert_eq!(script().runs, [8]);
        assert!(!setup_stop_requested(8));
        clear_setup_run(8);
        assert!(!stop_initial_setup());
        change(|script| script.admits = [Some(9)].into());
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        wait_until("run 9 starts", || script().runs.len() == 2);
        assert!(!setup_stop_requested(9));
        assert!(settle_setup_stop(Duration::ZERO), "nothing is owed");
        assert!(script().stop_attempts.is_empty());
    }

    #[test]
    fn a_choice_outranks_every_reading_queued_or_in_flight_when_it_is_made() {
        let _globals = globals();
        for refresh in 0..3 {
            let _io = TestIo::install();
            // The page is shown with every group set up: its reading reads GitHub
            // over the network, and Messages and FaceTime. Check again queues its
            // broader read behind that one before the person's newer choice.
            let shown = Plan::default();
            script().hold = true;
            assert!(observe_initial_setup(shown.clone()));
            wait_until("the page's reading starts", || script().reads.len() == 1);
            assert!(request_observation(
                Some(shown.clone()),
                Subject::all(),
                true
            ));
            // Meanwhile the person leaves Workspace and Messages & FaceTime as is.
            let left = left_as_is();
            script().admits = [Some(9)].into();
            let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
            assert!(restart_initial_setup(left.clone(), false, None, receipt));
            assert_eq!(answer.recv_timeout(HANG), Ok(true));
            assert_eq!(setup_run(), Some(9));
            match refresh {
                0 => {}
                1 => assert!(observe_initial_setup(left.clone())),
                _ => assert!(request_observation(None, Subject::all(), false)),
            }
            // The readings end, and then a stage's end asks for one naming no plan.
            release_readings();
            wait_until("the queued reading ends", || {
                script().reads.len() == 2 && observe_idle()
            });
            assert!(request_observation(None, Subject::all(), false));
            wait_until("the stage's reading ends", || {
                script().reads.len() == 3 && observe_idle()
            });
            let reads = std::mem::take(&mut script().reads);
            // Only the reading in flight when the person chose was taken for the
            // page's plan. Neither it nor the page's queued one put that plan back.
            assert_eq!((&reads[0].0, reads[0].2), (&shown, true));
            for (plan, subjects, network) in &reads[1..] {
                assert_eq!(plan, &left);
                assert!(!network, "GitHub is never read for a group left as is");
                assert!(
                    !subjects.iter().any(|subject| matches!(
                        subject,
                        Subject::Setting(setting) if setting.group() == Group::Accounts
                    )),
                    "nor is the person's account state"
                );
            }
            assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(left));
        }
    }

    #[test]
    fn a_queued_checks_scope_belongs_to_its_reviewed_plan() {
        let _globals = globals();
        let _io = TestIo::install();
        struct QueueLease;
        impl Drop for QueueLease {
            fn drop(&mut self) {
                let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                queue.running = false;
                queue.next = None;
            }
        }
        OBSERVE_QUEUE.lock().unwrap().running = true;
        let _queue = QueueLease;
        for edited in [false, true] {
            let checked = left_as_is();
            let mut reviewed = checked.clone();
            if edited {
                reviewed.vault_url = "http://another-vault.local:7484/v1/team".into();
            }
            assert!(request_observation(Some(checked), Subject::all(), true));
            assert!(observe_initial_setup(reviewed.clone()));
            let request = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
            observe_rows(request);
            let reads = std::mem::take(&mut script().reads);
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].0, reviewed);
            assert_eq!(
                reads[0].2, !edited,
                "only the reviewed Check may read GitHub"
            );
            assert_eq!(
                reads[0].1.iter().any(|subject| matches!(
                    subject,
                    Subject::Setting(setting) if setting.group() == Group::Accounts
                )),
                !edited,
                "account reads cannot transfer to another reviewed plan"
            );
            // A fresh Check of that exact edited plan still reads all rows,
            // even when an ordinary stage refresh coalesces behind it.
            assert!(request_observation(
                Some(reviewed.clone()),
                Subject::all(),
                true
            ));
            assert!(request_observation(None, Subject::all(), false));
            let request = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
            observe_rows(request);
            let reads = std::mem::take(&mut script().reads);
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].0, reviewed);
            assert!(reads[0].2);
            assert!(reads[0].1.contains(&Subject::Setting(Setting::Messages)));
            assert!(reads[0].1.contains(&Subject::Setting(Setting::FaceTime)));
        }
    }

    #[test]
    fn a_late_old_check_cannot_relabel_a_newer_queued_read() {
        let _globals = globals();
        let _io = TestIo::install();
        struct QueueLease;
        impl Drop for QueueLease {
            fn drop(&mut self) {
                let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                queue.running = false;
                queue.next = None;
            }
        }
        OBSERVE_QUEUE.lock().unwrap().running = true;
        let _queue = QueueLease;
        for check in [false, true] {
            // Capture Check A before a newer choice, then let it acquire the
            // queue only after the new choice's own read is already waiting.
            let old = ObserveRequest {
                plan: Some(left_as_is()),
                choices: OBSERVED_ROWS.lock().unwrap().choices,
                subjects: Subject::all(),
                forced: true,
            };
            let mut reviewed = left_as_is();
            reviewed.vault_url = "http://newer-vault.local:7484/v1/team".into();
            let generation = SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire);
            let _choice = begin_setup_choice(&reviewed, false, generation);
            assert!(request_observation(
                Some(reviewed.clone()),
                Subject::all(),
                check
            ));
            assert!(queue_observation(old));
            let request = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
            observe_rows(request);
            let reads = std::mem::take(&mut script().reads);
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].0, reviewed);
            assert_eq!(reads[0].2, check);
            assert_eq!(
                reads[0].1.contains(&Subject::Setting(Setting::Messages)),
                check
            );
            assert_eq!(
                reads[0].1.contains(&Subject::Setting(Setting::FaceTime)),
                check
            );
        }
    }

    #[test]
    fn an_in_flight_observation_cannot_publish_after_a_new_choice() {
        use aterm_types::macos_setup::Standing;

        let _globals = globals();
        let _io = TestIo::install();
        // Park only the queue's dispatcher: this reader still runs the real
        // observation entry, and a replacement read can be inspected before
        // it runs. Release the queue before TestIo waits for its workers.
        struct QueueLease;
        impl Drop for QueueLease {
            fn drop(&mut self) {
                let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                queue.running = false;
                queue.next = None;
            }
        }
        {
            let mut queue = OBSERVE_QUEUE.lock().unwrap();
            assert!(!queue.running);
            assert!(queue.next.is_none());
            queue.running = true;
        }
        let _queue = QueueLease;
        let secrets = Subject::Setting(Setting::Secrets);
        let repositories = Subject::Setting(Setting::Repositories);
        let reading = |subject, standing, note: &str| Observation {
            subject,
            standing,
            now: None,
            wanted: subject.wanted().to_owned(),
            note: Some(note.to_owned()),
            detail: None,
        };
        let shown = Plan::default();
        let subjects = Subject::all();
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(shown.clone());
            rows.rows = vec![
                reading(secrets, Standing::Waiting, "earlier vault reading"),
                reading(
                    repositories,
                    Standing::Waiting,
                    "earlier repository reading",
                ),
            ];
            rows.access_steps = 7;
            rows.choices
        };
        let current = reading(
            secrets,
            Standing::Waiting,
            "the newly selected vault did not answer",
        );
        change(|script| {
            script.hold = true;
            script.readings = [
                vec![
                    reading(
                        secrets,
                        Standing::NeedsYou,
                        "approval for the old vault: OLD42",
                    ),
                    reading(
                        repositories,
                        Standing::InPlace,
                        "old vault credentials worked",
                    ),
                ],
                vec![current.clone()],
            ]
            .into();
        });
        // On failure, release and join the reader before QueueLease and
        // TestIo drop; no delayed worker may outlive its inert I/O.
        struct Reader(Option<std::thread::JoinHandle<()>>);
        impl Drop for Reader {
            fn drop(&mut self) {
                release_readings();
                if let Some(reader) = self.0.take() {
                    let _ = reader.join();
                }
            }
        }
        let requested = subjects.clone();
        let mut reader = Reader(Some(std::thread::spawn(move || {
            observe_rows(ObserveRequest {
                plan: Some(shown),
                choices,
                subjects: requested,
                forced: true,
            });
        })));
        wait_until("the old Check's reading starts", || {
            script().reads.len() == 1
        });
        let mut chosen = Plan {
            vault_url: "http://new-vault.local:7484/v1/team".into(),
            ..Plan::default()
        };
        for setting in [Setting::Repositories, Setting::Messages, Setting::FaceTime] {
            chosen.selection.set(setting, false);
        }
        script().admits = [Some(9)].into();
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(chosen.clone(), true, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        wait_until("the new plan's inert run ends", || {
            !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
        });
        let before = {
            let rows = OBSERVED_ROWS.lock().unwrap();
            (rows.rows.clone(), rows.generation, rows.access_steps)
        };
        let wakes = SETUP_WAKES.count();
        release_readings();
        reader.0.take().unwrap().join().unwrap();
        {
            let rows = OBSERVED_ROWS.lock().unwrap();
            assert_eq!(rows.plan, Some(chosen.clone()));
            assert_eq!(
                rows.rows, before.0,
                "the old vault cannot publish under the new plan"
            );
            assert_eq!(rows.generation, before.1);
            assert_eq!(rows.access_steps, before.2);
        }
        assert_eq!(
            SETUP_WAKES.count(),
            wakes,
            "an old result cannot wake the new run"
        );
        let next = OBSERVE_QUEUE
            .lock()
            .unwrap()
            .next
            .take()
            .expect("a fresh reading is queued");
        assert_eq!(next.subjects, subjects);
        assert!(
            !next.forced,
            "a superseded Check cannot force old personal reads"
        );
        observe_rows(next);
        let reads = &script().reads;
        assert_eq!(reads.len(), 2);
        assert_eq!(reads[1].0, chosen);
        assert!(!reads[1].2, "the new plan leaves repositories unmanaged");
        assert!(!reads[1].1.iter().any(|subject| matches!(
            subject,
            Subject::Setting(Setting::Messages | Setting::FaceTime)
        )));
        assert_eq!(OBSERVED_ROWS.lock().unwrap().rows[0], current);
    }

    #[test]
    fn an_interrupted_pass_retires_held_readings_before_releasing_its_leases() {
        use aterm_types::macos_setup::Standing;

        let _globals = globals();
        for panics in [false, true] {
            let _io = TestIo::install();
            struct QueueLease;
            impl Drop for QueueLease {
                fn drop(&mut self) {
                    let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                    queue.running = false;
                    queue.next = None;
                }
            }
            OBSERVE_QUEUE.lock().unwrap().running = true;
            let _queue = QueueLease;
            struct Reader(Option<std::thread::JoinHandle<()>>);
            impl Drop for Reader {
                fn drop(&mut self) {
                    release_readings();
                    if let Some(reader) = self.0.take() {
                        let _ = reader.join();
                    }
                }
            }
            // This drops after the pass helper, before the real claim and busy
            // slot. Record the boundary without panicking during the unwind.
            struct Boundary<'a>(&'a mut Option<(bool, bool, bool)>);
            impl Drop for Boundary<'_> {
                fn drop(&mut self) {
                    *self.0 = Some((
                        SETUP_ACTIVITY.lock().unwrap().subjects.is_empty(),
                        OBSERVE_QUEUE.lock().unwrap().next.is_some(),
                        INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire),
                    ));
                }
            }
            let preference = Subject::Setting(Setting::Dock);
            let independent = Subject::Setting(Setting::Spotlight);
            let seen = |subject, standing| Observation {
                standing,
                ..Observation::unknown(subject)
            };
            let failed = seen(preference, Standing::Failed);
            let unaffected = seen(independent, Standing::NeedsYou);
            let fresh = seen(preference, Standing::Differs);
            let plan = left_as_is();
            let choices = {
                let mut rows = OBSERVED_ROWS.lock().unwrap();
                rows.plan = Some(plan.clone());
                rows.vault_url = Some(plan.vault_url.clone());
                rows.rows = vec![failed.clone(), Observation::unknown(independent)];
                rows.choices
            };
            observe_rows(ObserveRequest {
                plan: Some(plan.clone()),
                choices,
                subjects: Vec::new(),
                forced: false,
            });
            change(|script| {
                script.reads.clear();
                script.hold = true;
                script.readings = [
                    vec![seen(preference, Standing::InPlace), unaffected.clone()],
                    vec![fresh.clone()],
                ]
                .into();
            });
            let mut reader = Reader(None);
            let mut boundary = None;
            let mut wakes = 0;
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert!(take_initial_setup_slot_now());
                let _busy = InitialSetupBusy { observation: None };
                let _claim = TestIo::claim().unwrap().unwrap();
                let _boundary = Boundary(&mut boundary);
                with_setup_activity(|| {
                    setup_stage(aterm_cli::macos_doctor::StageEvent {
                        settings: &[Setting::Dock],
                        subjects: &[preference],
                        running: true,
                    });
                    reader.0 = Some(std::thread::spawn(move || {
                        observe_rows(ObserveRequest {
                            plan: Some(plan),
                            choices,
                            subjects: vec![preference, independent],
                            forced: false,
                        });
                    }));
                    wait_until("the mid-stage reading starts", || script().reads.len() == 1);
                    wakes = SETUP_WAKES.count();
                    assert!(!panics, "injected setup stage panic");
                    Err::<(), _>("injected progress save error")
                })
            }));
            assert_eq!(unwound.is_err(), panics);
            if let Ok(result) = unwound {
                assert_eq!(result, Err("injected progress save error"));
            }
            assert_eq!(
                boundary,
                Some((true, true, true)),
                "cleanup and refresh precede the release of the setup slot"
            );
            assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
            assert!(TestIo::claim().unwrap().is_some(), "the claim was released");
            release_readings();
            reader.0.take().unwrap().join().unwrap();
            {
                let rows = OBSERVED_ROWS.lock().unwrap();
                assert!(rows.rows.contains(&failed), "the old success was retired");
                assert!(rows.rows.contains(&unaffected));
            }
            assert_eq!(SETUP_WAKES.count(), wakes, "no false recovery wake");
            let refresh = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
            assert!(!refresh.forced);
            assert_eq!(refresh.subjects, Subject::all());
            observe_rows(refresh);
            assert!(OBSERVED_ROWS.lock().unwrap().rows.contains(&fresh));
            assert_eq!(script().reads.len(), 2);
        }
    }

    #[test]
    fn an_unwound_worker_retires_only_its_own_registration() {
        let _globals = globals();
        let _io = TestIo::install();
        for panics in [false, true] {
            for replacement in [None, Some(42)] {
                set_setup_run(Some(41));
                let result = std::panic::catch_unwind(|| {
                    with_setup_run(41, || {
                        if let Some(replacement) = replacement {
                            set_setup_run(Some(replacement));
                        }
                        assert!(!panics, "injected setup worker panic");
                    });
                });
                assert_eq!(result.is_err(), panics);
                assert_eq!(setup_run(), replacement);
            }
        }
    }

    #[test]
    fn preference_readings_retire_cached_and_held_reads() {
        use aterm_types::macos_setup::Standing;

        let _globals = globals();
        let _io = TestIo::install();
        // Keep the automatic refresh queued until the old read has returned.
        struct QueueLease;
        impl Drop for QueueLease {
            fn drop(&mut self) {
                let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                queue.running = false;
                queue.next = None;
            }
        }
        OBSERVE_QUEUE.lock().unwrap().running = true;
        let _queue = QueueLease;
        struct Reader(Option<std::thread::JoinHandle<()>>);
        impl Drop for Reader {
            fn drop(&mut self) {
                release_readings();
                if let Some(reader) = self.0.take() {
                    let _ = reader.join();
                }
            }
        }
        let independent = Subject::Setting(Setting::Secrets);
        let seen = |subject, standing| Observation {
            standing,
            ..Observation::unknown(subject)
        };
        let plan = Plan::default();
        for (follow_up, failed) in [(false, false), (false, true), (true, false), (true, true)] {
            let preference = Subject::Setting(if follow_up {
                Setting::Widgets
            } else {
                Setting::CapsLock
            });
            let old = seen(
                preference,
                if follow_up && !failed {
                    Standing::Differs
                } else {
                    Standing::InPlace
                },
            );
            let vault = seen(independent, Standing::NeedsYou);
            let restored = seen(
                preference,
                if follow_up && !failed {
                    Standing::InPlace
                } else {
                    Standing::Differs
                },
            );
            let choices = {
                let mut rows = OBSERVED_ROWS.lock().unwrap();
                rows.plan = Some(plan.clone());
                rows.vault_url = Some(plan.vault_url.clone());
                rows.rows = vec![old.clone(), vault.clone()];
                rows.choices
            };
            change(|script| {
                script.reads.clear();
                script.hold = true;
                script.readings = [vec![old, vault.clone()], vec![restored.clone()]].into();
            });
            let reading_plan = plan.clone();
            let mut reader = Reader(Some(std::thread::spawn(move || {
                observe_rows(ObserveRequest {
                    plan: Some(reading_plan),
                    choices,
                    subjects: vec![preference, independent],
                    forced: false,
                });
            })));
            wait_until("the preference read starts", || script().reads.len() == 1);
            let mut during = Vec::new();
            let mut effect = || {
                during = OBSERVED_ROWS.lock().unwrap().rows.clone();
                if failed {
                    Err("partial preference change")
                } else {
                    Ok(())
                }
            };
            let result = if follow_up {
                finish_withheld_preferences_with(|reserve| {
                    let _reserved = reserve();
                    effect()
                })
            } else {
                with_restore_readings(effect)
            };
            assert_eq!(result.is_err(), failed);
            release_readings();
            reader.0.take().unwrap().join().unwrap();
            let (retained, source) = {
                let rows = OBSERVED_ROWS.lock().unwrap();
                (rows.rows.clone(), rows.vault_url.clone())
            };
            assert!(
                !retained.iter().any(|row| row.subject == preference),
                "an old preference read must not publish after a change (follow-up: {follow_up})"
            );
            assert!(retained.contains(&vault));
            assert_eq!(source.as_ref(), Some(&plan.vault_url));
            assert!(
                !during.iter().any(|row| row.subject == preference),
                "the changing preference is unread while reserved (follow-up: {follow_up})"
            );
            assert!(during.contains(&vault));
            assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
            let fresh = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
            assert!(!fresh.forced);
            assert!(fresh.plan.is_none(), "refresh uses the plan in force");
            assert!(fresh.subjects.contains(&preference));
            if follow_up {
                assert_eq!(fresh.subjects, [Subject::Setting(Setting::Widgets)]);
            } else {
                assert!(
                    fresh
                        .subjects
                        .iter()
                        .all(|subject| matches!(subject.group(), Group::Input | Group::Desktop))
                );
            }
            observe_rows(fresh);
            let rows = OBSERVED_ROWS.lock().unwrap();
            assert!(rows.rows.contains(&restored));
            assert!(rows.rows.contains(&vault));
            assert_eq!(script().reads.len(), 2);
        }
    }

    #[test]
    fn preference_readings_unwind_without_leaving_activity_or_busy() {
        let _globals = globals();
        let _io = TestIo::install();
        for follow_up in [false, true] {
            OBSERVE_QUEUE.lock().unwrap().running = true;
            let result = std::panic::catch_unwind(|| {
                if follow_up {
                    finish_withheld_preferences_with(|reserve| {
                        let _reserved = reserve();
                        panic!("injected follow-up panic");
                    });
                } else {
                    assert!(take_initial_setup_slot_now());
                    let _busy = InitialSetupBusy { observation: None };
                    with_restore_readings(|| panic!("injected restore panic"));
                }
            });
            assert!(result.is_err());
            assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
            assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
            let fresh = {
                let mut queue = OBSERVE_QUEUE.lock().unwrap();
                queue.running = false;
                queue.next.take()
            };
            assert!(
                fresh.is_some(),
                "an unwound preference change is read again"
            );
        }
    }

    #[test]
    fn a_follow_up_that_never_reserves_leaves_its_readings_alone() {
        let _globals = globals();
        let _io = TestIo::install();
        let widgets = Observation::unknown(Subject::Setting(Setting::Widgets));
        let source = "https://example.invalid/current-vault".to_owned();
        {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.rows = vec![widgets.clone()];
            rows.vault_url = Some(source.clone());
        }
        let epoch = SETUP_ACTIVITY.lock().unwrap().epoch;
        let generation = OBSERVED_ROWS.lock().unwrap().generation;
        // The CLI calls reserve only once it has found owed work and Full
        // Disk Access. Both earlier exits take this same unreserved path.
        assert_eq!(finish_withheld_preferences_with(|_| None::<()>), None);
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(SETUP_ACTIVITY.lock().unwrap().epoch, epoch);
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert_eq!(rows.rows, [widgets]);
        assert_eq!(rows.vault_url, Some(source));
        assert_eq!(rows.generation, generation);
        assert!(script().reads.is_empty());
        let queue = OBSERVE_QUEUE.lock().unwrap();
        assert!(!queue.running && queue.next.is_none());
    }

    #[test]
    fn a_partial_observation_of_another_vault_retires_both_source_rows() {
        use aterm_types::macos_setup::{Access, Standing};

        let _globals = globals();
        let _io = TestIo::install();
        let caps = Subject::Setting(Setting::CapsLock);
        let secrets = Subject::Setting(Setting::Secrets);
        let repositories = Subject::Setting(Setting::Repositories);
        let access = Subject::Access(Access::FullDiskAccess);
        let reading = |subject, standing, note: &str| Observation {
            subject,
            standing,
            now: None,
            wanted: subject.wanted().to_owned(),
            note: Some(note.to_owned()),
            detail: None,
        };
        let old = Plan::default();
        let kept = reading(caps, Standing::InPlace, "independent keyboard reading");
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.rows = vec![
                kept.clone(),
                reading(secrets, Standing::NeedsYou, "old source approval OLD42"),
                reading(
                    repositories,
                    Standing::Failed,
                    "old source authentication failure",
                ),
            ];
            rows.plan = Some(old.clone());
            rows.vault_url = Some(old.vault_url);
            rows.choices
        };
        let current = Plan {
            vault_url: "  http://current-vault.local:7484/v1/team  ".into(),
            ..Plan::default()
        };
        change(|script| {
            script.readings = [
                vec![reading(
                    access,
                    Standing::Differs,
                    "access reading for current plan",
                )],
                vec![reading(
                    repositories,
                    Standing::Waiting,
                    "current repository reading",
                )],
            ]
            .into();
        });
        observe_rows(ObserveRequest {
            plan: Some(current.clone()),
            choices,
            subjects: vec![access],
            forced: false,
        });
        {
            let rows = OBSERVED_ROWS.lock().unwrap();
            assert_eq!(rows.vault_url.as_deref(), Some(current.vault_url.trim()));
            assert!(rows.rows.contains(&kept));
            assert!(rows.rows.iter().any(|row| row.subject == access));
            assert!(!rows.rows.iter().any(|row| row.subject.uses_vault_source()));
        }
        // A later partial repository read cannot relabel the old Secrets
        // code as belonging to the current source.
        observe_rows(ObserveRequest {
            plan: None,
            choices,
            subjects: vec![repositories],
            forced: false,
        });
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert!(rows.rows.contains(&kept));
        assert!(rows.rows.iter().any(|row| row.subject == repositories));
        assert!(!rows.rows.iter().any(|row| row.subject == secrets));
        assert!(!rows.rows.iter().any(|row| {
            row.note
                .as_deref()
                .is_some_and(|note| note.contains("OLD42"))
        }));
    }

    #[test]
    fn a_refused_choice_gives_stop_back_the_run_it_unregistered() {
        let _globals = globals();
        let _io = TestIo::install();
        // Run 7 is parked, so the person's choice takes the free slot and
        // drops run 7's registration; then it is refused (another aterm or
        // `aterm doctor` holds the claim, or the plan could not be saved).
        set_setup_run(Some(7));
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(false));
        wait_until("the refused choice is let go", || {
            OBSERVED_ROWS.lock().unwrap().choices == choices + 2
        });
        // Run 7 is the run a Stop stops again: pressed now, it stops run 7.
        assert_eq!(setup_run(), Some(7));
        assert!(stop_initial_setup());
        assert!(setup_stop_requested(7));
        wait_until("run 7's Stop is saved", stop_worker_idle);
        assert_eq!(script().stops, [7]);
        // Readings go back to the plan in force.
        assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, None);

        // A run that registered while the choice was refused keeps its place.
        reset_worker_state();
        set_setup_run(Some(7));
        script().registers = Some(8);
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(false));
        wait_until("the refused choice is let go", || {
            OBSERVED_ROWS.lock().unwrap().choices == choices + 2
        });
        assert_eq!(setup_run(), Some(8));
    }

    /// The window's wiring (`wire_window`) registers the access-record watcher (System State
    /// design §6): a line written to the record wakes a run parked on the Permissions stage and
    /// queues a reading of every Access row. A reading already running stands in, so this test
    /// reads no grant of this Mac; the record is a scratch folder's.
    #[test]
    fn an_access_step_on_record_wakes_the_run_and_reads_the_access_rows() {
        use aterm_types::macos_setup::{Access, AccessOutcome, Subject, access_record};
        let _globals = SETUP_GLOBALS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        {
            let mut queue = OBSERVE_QUEUE.lock().unwrap();
            assert!(!queue.running, "no reading runs in this test binary");
            queue.running = true;
            queue.next = None;
        }
        // The wiring `run_window` does before the GUI starts, as a headless
        // window's (no LAN agent retirement): the watcher must be part of it.
        let _ = wire_window(&[OsString::from("--headless")]);
        let woken = SETUP_WAKES.count();
        let state = aterm_tempfile::tempdir().unwrap();
        access_record::append(
            state.path(),
            7,
            Access::FullDiskAccess,
            AccessOutcome::Granted,
        )
        .unwrap();
        let queued = {
            let mut queue = OBSERVE_QUEUE.lock().unwrap();
            queue.running = false;
            queue.next.take()
        };
        assert!(SETUP_WAKES.count() > woken, "a parked run is woken");
        let queued = queued.expect("a reading of the Access rows is asked for");
        assert_eq!(queued.plan, None, "the page's plan is kept");
        assert_eq!(queued.subjects, Access::ALL.map(Subject::Access).to_vec());
        assert!(!queued.forced);
    }

    #[test]
    fn a_timed_out_dialog_waiter_wakes_setup_after_the_last_guard_ends() {
        use aterm_types::macos_setup::MacosDialog;
        use std::cell::Cell;
        use std::time::Duration;

        let _globals = globals();
        let _ = wire_window(&[OsString::from("--headless")]);
        for released_before_park in [true, false] {
            let first = MacosDialog::up();
            let last = MacosDialog::up();
            let seen = SETUP_WAKES.count();
            assert!(MacosDialog::when_none_up(Duration::from_nanos(1)).is_none());
            drop(first);
            assert_eq!(SETUP_WAKES.count(), seen, "one dialog remains up");

            let mut last = Some(last);
            if released_before_park {
                drop(last.take());
            }
            let waits = Cell::new(0);
            wait_for_setup_park_change(
                || (),
                |_| false,
                || SETUP_WAKES.count() == seen,
                || {
                    assert!(
                        !released_before_park && waits.get() == 0,
                        "a released dialog must wake this wait without a focus or grant change"
                    );
                    waits.set(waits.get() + 1);
                    drop(last.take());
                },
            );
            assert_eq!(waits.get(), usize::from(!released_before_park));
            assert_eq!(SETUP_WAKES.count(), seen + 1);
        }
    }

    #[test]
    fn dialog_ownership_without_a_blocking_waiter_leaves_setup_parked() {
        use aterm_types::macos_setup::MacosDialog;
        use std::cell::Cell;
        use std::time::Duration;

        let _globals = globals();
        let _ = wire_window(&[OsString::from("--headless")]);
        let seen = SETUP_WAKES.count();
        let own = MacosDialog::when_none_up(Duration::from_nanos(1)).unwrap();
        assert!(MacosDialog::when_none_up(Duration::ZERO).is_none());
        drop(own);
        assert_eq!(SETUP_WAKES.count(), seen);

        // Neither a successful stage's own dialog nor an immediate UI slot
        // probe moves an unanswered Accessibility row. It still needs a person.
        let waits = Cell::new(0);
        wait_for_setup_park_change(
            || (),
            |_| false,
            || waits.get() < 2,
            || {
                assert_eq!(SETUP_WAKES.count(), seen);
                waits.set(waits.get() + 1);
            },
        );
        assert_eq!(waits.get(), 2);
    }

    #[test]
    fn a_choice_that_holds_the_slot_leaves_stop_to_the_run_it_admits() {
        let _globals = globals();
        let _io = TestIo::install();
        // Run 7 is parked. A choice made while a pass runs stops that pass.
        set_setup_run(Some(7));
        // No later choice overtakes these two.
        let generation = SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire);
        let running = begin_setup_choice(&Plan::default(), false, generation);
        assert_eq!((setup_run(), running.replaced), (Some(7), None));
        // A choice that took the slot replaces run 7 once admitted. Until its own run registers
        // there is nothing for a Stop to take; a Stop pressed meanwhile stops that run as it
        // registers, or run 7 as a refusal gives it back.
        let holding = begin_setup_choice(&Plan::default(), true, generation);
        assert_eq!((setup_run(), holding.replaced), (None, Some(7)));
        // Refusing the earlier choice keeps the later one's plan; refusing the choice that holds
        // the slot gives run 7 back.
        forget_setup_choice(&running);
        assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(Plan::default()));
        forget_setup_choice(&holding);
        assert_eq!(setup_run(), Some(7));
        assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, None);
    }
}

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

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> isize;
        #[cfg(test)]
        fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];
    const HANDLE_FLAG_INHERIT: u32 = 0x1;

    const SAME_BUILD_SPREAD: Duration = Duration::from_secs(60);

    /// Whether a build tree's windowed image, last written at `sibling`, is too old to open a
    /// window for this image, last written at `this`.
    pub(super) fn is_stale(sibling: SystemTime, this: SystemTime) -> bool {
        this.duration_since(sibling)
            .is_ok_and(|older_by| older_by > SAME_BUILD_SPREAD)
    }

    fn is_build_tree_image(image: &Path) -> bool {
        image
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("aterm-windowed.exe"))
    }

    /// Stop this process's std handles from being inherited by the sibling.
    pub(super) fn stop_inheriting_std_handles() {
        // SAFETY: handle queries and a flag write on handles this process owns; no pointers
        // cross the boundary.
        unsafe {
            for kind in STD_HANDLES {
                let handle = GetStdHandle(kind);
                if handle != 0 && handle != -1 {
                    let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }

    /// Whether any of this process's std handles is still inheritable.
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

    /// Start the windowed sibling with `args`, detached, stdio on NUL, and return once it is
    /// running.
    pub(super) fn launch(args: &[OsString]) -> Result<(), String> {
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
            let release = exe
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|dir| dir.eq_ignore_ascii_case("release"));
            return Err(format!(
                "{} is older than {}; rebuild with targo --unverified build{} -p aterm",
                sibling.display(),
                exe.display(),
                if release { " --release" } else { "" }
            ));
        }
        stop_inheriting_std_handles();
        let mut sibling_cmd = aterm_uds::spawnfd::command(&sibling);
        aterm_uds::spawn::set_gui_image_flags(&mut sibling_cmd);
        sibling_cmd
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| format!("could not start {} ({e})", sibling.display()))
    }
}

/// Whether a session-shaped launch typed inside an aterm tab opens a tab there instead of
/// nesting (Windows).
#[cfg(any(windows, test))]
fn nests_inside_aterm(bare: bool, force_session: bool, aterm_child: bool, stdin_tty: bool) -> bool {
    bare && !force_session && aterm_child && stdin_tty
}

/// What the guarded launch does instead of nesting: `aterm new-tab -d <cwd>`, forced to the
/// enclosing instance rather than routed by `windowing_behavior` (whose default, `new_window`,
/// would open a second window for a launch that meant "a terminal here").
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
            eprintln!(
                "{}",
                spawn_refusal_line("this aterm window", "refused a new tab", &reply)
            );
            ExitCode::FAILURE
        }
        Some(Err(error)) => {
            let delivered = aterm_ctl::front_door_delivered(&error);
            eprintln!(
                "{}",
                forward_error_line("this aterm window", delivered, &error.to_string(), "tab")
            );
            if delivered {
                return ExitCode::FAILURE;
            }
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

fn payload_boundary(rest: &[OsString]) -> usize {
    rest.iter()
        .position(|a| matches!(a.to_string_lossy().as_ref(), "-e" | "--command" | "--"))
        .unwrap_or(rest.len())
}

const MODE_FLAGS: &[&str] = &["--window", "--session"];

fn strip_flags(rest: &[OsString], flags: &[&str]) -> Vec<OsString> {
    let boundary = payload_boundary(rest);
    rest.iter()
        .enumerate()
        .filter(|(i, a)| *i >= boundary || !flags.contains(&a.to_string_lossy().as_ref()))
        .map(|(_, a)| a.clone())
        .collect()
}

fn strip_mode_flags(rest: &[OsString]) -> Vec<OsString> {
    strip_flags(rest, MODE_FLAGS)
}

const NO_REROUTE_FLAG: &str = "--no-reroute";

fn take_no_reroute(rest: &[OsString]) -> Vec<OsString> {
    aterm_log::env::unset(atpkg::reroute::ANNOUNCED_ENV);
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

fn window_verb(verb: &str, args: &[OsString], host: WindowHost) -> ExitCode {
    // `-h`/`--help` in first position answers with the verb's synopsis rather than the strict
    // grammar's "unknown option": these verbs dispatch to no tool that could answer it.
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
    /// `attach`, one answered the probe, and the forward's dial then failed —
    /// already said on stderr.
    Unreachable,
}

const fn split_pane_spawn_line(why: SpawnHere) -> Option<&'static str> {
    match why {
        SpawnHere::Policy => Some(
            "aterm: windowing_behavior is new_window, so split-pane opens a new window; \
             set it to \"attach\" in aterm.toml to split the running aterm",
        ),
        SpawnHere::NoInstance => Some("aterm: no running aterm to split; opening a new window"),
        SpawnHere::Unreachable => None,
    }
}

fn route_and_maybe_forward(request: &aterm_cli::WindowRequest) -> ControlFlow<ExitCode, SpawnHere> {
    let behavior = effective_windowing_behavior();
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
        Ok(reply) if reply.starts_with("OK") => ControlFlow::Break(ExitCode::SUCCESS),
        Ok(reply) => {
            eprintln!(
                "{}",
                spawn_refusal_line("the running aterm", "refused", &reply)
            );
            ControlFlow::Break(ExitCode::FAILURE)
        }
        Err(error) => {
            let delivered = aterm_ctl::front_door_delivered(&error);
            eprintln!(
                "{}",
                forward_error_line(
                    "the running aterm",
                    delivered,
                    &error.to_string(),
                    intent_noun(request.intent)
                )
            );
            if delivered {
                ControlFlow::Break(ExitCode::FAILURE)
            } else {
                ControlFlow::Continue(SpawnHere::Unreachable)
            }
        }
    }
}

const fn intent_noun(intent: aterm_cli::LaunchIntent) -> &'static str {
    match intent {
        aterm_cli::LaunchIntent::SplitPane => "pane",
        aterm_cli::LaunchIntent::NewWindow => "window",
        aterm_cli::LaunchIntent::NewTab | aterm_cli::LaunchIntent::Plain => "tab",
    }
}

fn forward_error_line(who: &str, delivered: bool, error: &str, noun: &str) -> String {
    if delivered {
        format!("aterm: {who} {error}; the {noun} may still open there")
    } else {
        format!("aterm: {who} is not answering; opening a new window")
    }
}

fn spawn_refusal_line(who: &str, refused: &str, reply: &str) -> String {
    let reason = reply.strip_prefix("ERR ").unwrap_or(reply);
    if reason == "auth" || reason.starts_with("auth;") {
        format!(
            "aterm: {who} did not accept this command's authentication; \
             `aterm new-window` opens a window here"
        )
    } else if reason.starts_with("usage: spawn") {
        format!(
            "aterm: {who} is a different aterm version; restart it, \
             or `aterm new-window` opens a window here"
        )
    } else {
        format!("aterm: {who} {refused}: {reason}")
    }
}

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

fn resolve_dir_absolute(raw: &str) -> Result<String, String> {
    let path = std::path::Path::new(raw);
    let absolute = std::path::absolute(path).map_err(|e| format!("cannot resolve {raw}: {e}"))?;
    if !absolute.is_dir() {
        return Err(format!("not a directory: {raw}"));
    }
    Ok(absolute.to_string_lossy().into_owned())
}

fn plain_launch_dir(scan: &[OsString]) -> Result<Option<String>, ()> {
    match aterm_cli::plain_launch_dir_operand(scan) {
        Some(value) => resolve_dir_absolute(&value).map(Some).map_err(|_| ()),
        None => Ok(None),
    }
}

fn update_verb(rest: &[OsString]) -> ExitCode {
    aterm_gui::install_session_log();
    let build = aterm_gui::running_build_number();
    let first = rest.first().map(|a| a.to_string_lossy().into_owned());
    let words = rest
        .iter()
        .filter(|a| !matches!(a.to_str(), Some("-v" | "--verbose")))
        .collect::<Vec<_>>();
    if words.len() == 1 && words[0] == "identity" {
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
        println!("{UPDATE_USAGE}");
        for line in UPDATE_HELP {
            println!("{line}");
        }
        #[cfg(target_os = "linux")]
        for line in UPDATE_HELP_LINUX {
            println!("{line}");
        }
        println!("`aterm help update` has the rest.");
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
            _ => Err("usage: enable | apply | rollback".into()),
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
                // A word that is not ours (`claude --wait-lock 60`, `apply` on a Mac)
                // says what it is, and one of ours out of place says so, whatever follows it.
                if let Some(word) = sub.as_deref().filter(|w| !matches!(*w, "status" | "check")) {
                    let line = update_misplaced_line(word)
                        .unwrap_or_else(|| update_unknown_line(word, store_resolves));
                    eprintln!("{line}");
                    return ExitCode::from(2);
                }
                eprintln!("aterm update: unknown argument {arg:?} ({UPDATE_USAGE})");
                return ExitCode::from(2);
            }
        }
    }
    let checking = sub.as_deref() == Some("check");
    let st = match sub.as_deref().unwrap_or("status") {
        "status" => aterm_update::status(build),
        "check" if cfg!(windows) => None,
        "check" => {
            // A check can download a whole release.
            if std::io::IsTerminal::is_terminal(&std::io::stderr())
                && aterm_update::status(build).is_some_and(|st| {
                    st.enabled && st.installable && !linux_install_waits_for_a_start(&st)
                })
            {
                eprintln!("Checking for updates\u{2026}");
            }
            let provider: aterm_update::CheckSettingsProvider =
                std::sync::Arc::new(aterm_gui::configured_update_settings);
            Some(aterm_update::check_now_with_settings(build, &provider))
        }
        other => {
            let line = update_misplaced_line(other)
                .unwrap_or_else(|| update_unknown_line(other, store_resolves));
            eprintln!("{line}");
            return ExitCode::from(2);
        }
    };
    let Some(st) = st else {
        println!(
            "{}",
            update_unreadable_line(aterm_types::version::app_version())
        );
        return if no_ledger_fails(checking) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    };
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let lane = aterm_update::apply_lane_report(build);
    let (line, trouble) = update_summary(
        aterm_types::version::app_version(),
        build,
        &st,
        lane.as_ref(),
        now,
        aterm_update_core::settings::update_auto_apply(),
        aterm_update::automatic(),
        &DevCopy::of_this_copy(checking),
        &update_log_hint(),
    );
    aterm_log::info!("aterm update: {line}");
    println!("{line}");
    if let Some(trouble) = trouble {
        eprintln!("{trouble}");
    }
    if verbose {
        print_update_detail(&st, lane.as_ref());
    }
    if checking
        && (!st.enabled || !st.installable || st.channel_unreadable || st.failing_checks > 0)
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

const fn no_ledger_fails(checking: bool) -> bool {
    checking || cfg!(target_os = "macos")
}

const UPDATE_USAGE: &str = if cfg!(target_os = "linux") {
    "usage: aterm update [status|check] [-v] | identity | enable | apply | rollback"
} else {
    "usage: aterm update [status|check] [-v] | identity"
};

const UPDATE_HELP: &[&str] = &[
    "  status     this copy's version and where it stands, one line (the default)",
    "  check      check now (a newer release downloads), then print that line;",
    "             exits 1 when this copy cannot update or checking fails",
    "  -v         add the updater's detail: last decision, last check, failures",
    "  identity   the running binary's compiled identity, as JSON",
];

/// The Linux delivery's own words, under [`UPDATE_HELP`] on Linux alone.
#[cfg(target_os = "linux")]
const UPDATE_HELP_LINUX: &[&str] = &[
    "  enable     turn on updates for this installed copy",
    "  apply      install the update already downloaded",
    "  rollback   go back to the previous executable",
    "An update replaces only the executable on disk; running sessions keep running.",
];

/// A word of ours in the wrong place: `identity` with more after it, or on Linux a delivery
/// word after a flag (that route takes the word only first).
fn update_misplaced_line(word: &str) -> Option<String> {
    if word == "identity" {
        return Some(format!(
            "aterm update: identity takes no arguments ({UPDATE_USAGE})"
        ));
    }
    (cfg!(target_os = "linux") && matches!(word, "enable" | "apply" | "rollback"))
        .then(|| format!("aterm update: {word} goes first, without -v ({UPDATE_USAGE})"))
}

fn update_unknown_line(word: &str, is_package: impl FnOnce(&str) -> bool) -> String {
    if !cfg!(target_os = "linux") && matches!(word, "enable" | "apply" | "rollback") {
        let mac = if cfg!(target_os = "macos") && word == "apply" {
            " \u{2014} on a Mac, `aterm ctl update apply` or Settings \u{25b8} Software Update \
             installs a downloaded update"
        } else {
            ""
        };
        return format!("aterm update: {word} is Linux-only{mac}");
    }
    if is_package(word) {
        return format!("aterm update: {word} is a package \u{2014} run aterm pkg update {word}");
    }
    format!("aterm update: unknown sub-command {word:?} ({UPDATE_USAGE})")
}

/// What `aterm update` knows of this copy's dev mark (`ship dev-app`,
/// `aterm_update::running_is_dev_marked`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum DevCopy {
    /// Not dev-marked: the updater's own ledger speaks for it.
    No,
    /// Dev-marked, so the updater leaves it alone. `lag`: its standing against the public
    /// channel when that was read (one read-only HEAD of the evergreen appcast,
    /// `aterm_update::dev_channel`), else `None`.
    Marked {
        lag: Option<aterm_update::dev_channel::DevLag>,
        shared_writes: Option<aterm_gui::DevSharedWrites>,
    },
    /// Not dev-marked, and running from where the updater never replaces a copy: the disk
    /// image, a translocated download, or a plain binary (`which_copy::running_posture`).
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            dead_code,
            reason = "which_copy::running_posture, its one constructor, is macOS-only"
        )
    )]
    Inert(aterm_update::which_copy::InstallPosture),
}

impl DevCopy {
    fn of_this_copy(asked_check: bool) -> Self {
        if !aterm_update::running_is_dev_marked() {
            #[cfg(target_os = "macos")]
            if let Some(posture) = aterm_update::which_copy::running_posture()
                && posture != aterm_update::which_copy::InstallPosture::Installed
            {
                return Self::Inert(posture);
            }
            return Self::No;
        }
        let lag = if asked_check || aterm_update::automatic() {
            aterm_update::dev_channel::standing(aterm_types::version::app_version())
        } else {
            None
        };
        Self::Marked {
            lag,
            shared_writes: aterm_gui::dev_build_shared_writes(),
        }
    }
}

fn dev_shared_writes_line(writes: aterm_gui::DevSharedWrites) -> String {
    let aterm_gui::DevSharedWrites { what, repair } = writes;
    format!(
        "This dev build writes {what}, which only the release writes by itself, because its \
         bundle is named aterm.app \u{2014} rebuild it with `targo --unverified ship dev-app` (it \
         installs as aterm (dev).app) and remove this copy, then run {repair} from the release."
    )
}

fn update_unreadable_line(version: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "aterm {version} can\u{2019}t read its update state: {}",
            aterm_update::ledger_unreadable_cause()
        )
    }
    #[cfg(windows)]
    {
        format!(
            "aterm {version} doesn\u{2019}t update itself on Windows \u{2014} to update it, \
             in your aterm checkout run {}",
            aterm_cli::WINDOWS_UPDATE_LANE
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        format!("aterm {version} doesn\u{2019}t update itself on this platform")
    }
}

fn update_log_hint() -> String {
    let log = aterm_types::dirs::logs_dir().map(|dir| dir.join("aterm.log"));
    update_log_hint_for(log.as_deref(), aterm_types::dirs::home_dir().as_deref())
}

fn update_log_hint_for(log: Option<&std::path::Path>, home: Option<&std::path::Path>) -> String {
    let Some(log) = log else {
        return "Details are in Settings \u{25b8} Messages.".to_string();
    };
    let shown = match home.and_then(|home| log.strip_prefix(home).ok()) {
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => log.display().to_string(),
    };
    format!("Details are in Settings \u{25b8} Messages, and in {shown}.")
}

fn staged_tries(staged: u64, lane: Option<&aterm_update::ApplyLaneReport>) -> u32 {
    lane.filter(|lane| staged > 0 && lane.last_failure_target_build == staged)
        .map_or(0, |lane| lane.failures_for_target)
}

/// What `aterm update status|check` says: one plain line for stdout and, when something is
/// wrong, one for stderr.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gathered fact; passing them in is what keeps the line pure"
)]
fn update_summary(
    version: &str,
    build: u64,
    st: &aterm_update::UpdateStatus,
    lane: Option<&aterm_update::ApplyLaneReport>,
    now: i64,
    installs_by_itself: bool,
    automatic_checks: bool,
    dev: &DevCopy,
    log_hint: &str,
) -> (String, Option<String>) {
    const CHECKS_OFF: &str = " \u{b7} automatic checks are off (Settings \u{25b8} Software Update)";
    if !st.enabled {
        let remedy = st.outcome.trim();
        return (
            format!("This copy of aterm {version} doesn\u{2019}t update itself"),
            (!remedy.is_empty()).then(|| remedy.to_string()),
        );
    }
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
    if let Some(native) = st
        .linux
        .as_ref()
        .filter(|_| linux_install_waits_for_a_start(st))
    {
        let installed = if !native.installed_version.is_empty() {
            native.installed_version.clone()
        } else if native.installed_build == build {
            version.to_string()
        } else {
            format!("build {}", native.installed_build)
        };
        return (
            format!(
                "aterm {installed} is installed \u{2014} the next aterm you start finishes it; \
                 update checks wait until then"
            ),
            None,
        );
    }
    if !st.installable {
        use aterm_update::which_copy::InstallPosture;
        let line = match dev {
            DevCopy::Marked { lag, shared_writes } => {
                let standing = lag
                    .as_ref()
                    .map(|lag| format!(", {}", lag.words()))
                    .unwrap_or_default();
                return (
                    format!(
                        "This copy of aterm {version} is a dev build{standing} \u{2014} the \
                         updater leaves it alone"
                    ),
                    shared_writes.map(dev_shared_writes_line),
                );
            }
            DevCopy::Inert(InstallPosture::MountedImage) => format!(
                "aterm {version} is running from a mounted volume \u{2014} drag aterm to \
                 Applications and open it from there"
            ),
            DevCopy::Inert(InstallPosture::Translocated) => format!(
                "aterm {version} is running from a temporary read-only copy \u{2014} move \
                 aterm into Applications and open it again"
            ),
            DevCopy::Inert(InstallPosture::NotABundle) => format!(
                "aterm {version} is a plain binary, not an app bundle \u{2014} it never \
                 updates itself"
            ),
            DevCopy::Inert(InstallPosture::Installed) | DevCopy::No => {
                format!("This copy of aterm {version} can\u{2019}t update itself")
            }
        };
        return (line, None);
    }
    if let Some(staged) = st.staged_build.filter(|staged| *staged > build) {
        let next = st
            .staged_version
            .clone()
            .unwrap_or_else(|| format!("build {staged}"));
        let failed = staged_tries(staged, lane);
        return if failed > 0 {
            let tries = if failed == 1 {
                "1 try".to_string()
            } else {
                format!("{failed} tries")
            };
            (
                format!("aterm {next} is downloaded but didn\u{2019}t install ({tries})"),
                Some(log_hint.to_string()),
            )
        } else if installs_by_itself {
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
        let remedy = st.outcome.trim();
        return (
            format!("aterm {version} can\u{2019}t check for updates on this machine"),
            Some(if remedy.is_empty() {
                log_hint.to_string()
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
            "the last updates didn\u{2019}t install".to_string()
        } else {
            match check_trouble_words(class) {
                Some(words) => format!("update checks keep failing: {words}"),
                None => "update checks keep failing".to_string(),
            }
        };
        let next = if automatic_checks {
            "; aterm keeps trying"
        } else {
            CHECKS_OFF
        };
        return (
            format!("aterm {version} \u{b7} {trouble}{next}"),
            Some(log_hint.to_string()),
        );
    }
    if st.failing_checks > 0 {
        let why = check_trouble_words(class)
            .map(|words| format!(": {words}"))
            .unwrap_or_default();
        let next = if automatic_checks {
            "; aterm will try again"
        } else {
            CHECKS_OFF
        };
        return (
            format!("aterm {version} \u{b7} the last check didn\u{2019}t finish{why}{next}"),
            Some(log_hint.to_string()),
        );
    }
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at);
    let mut line = if st.linux.as_ref().is_some_and(|native| native.refused_newer) {
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
        format!("aterm {version} hasn\u{2019}t checked for updates yet")
    };
    if !automatic_checks {
        line.push_str(CHECKS_OFF);
    }
    (line, None)
}

fn check_trouble_words(kind: &str) -> Option<&'static str> {
    match kind {
        "network" => Some("the update server can\u{2019}t be reached"),
        "manifest" => Some("the newest release couldn\u{2019}t be verified"),
        "pipeline" => Some("downloads aren\u{2019}t finishing"),
        "stage" => Some("a download couldn\u{2019}t be prepared"),
        _ => None,
    }
}

fn trial_launches_words(starts: u32) -> String {
    if starts == 0 {
        String::new()
    } else {
        let budget = aterm_update::LINUX_TRIAL_LAUNCHES;
        format!(
            " ({} of {budget} launches used before it rolls back)",
            starts.min(budget)
        )
    }
}

fn linux_install_waits_for_a_start(st: &aterm_update::UpdateStatus) -> bool {
    st.linux.as_ref().is_some_and(|native| {
        native.trial_phase.as_deref() == Some("Installed") && !native.trial_healthy
    })
}

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

fn print_update_detail(
    st: &aterm_update::UpdateStatus,
    lane: Option<&aterm_update::ApplyLaneReport>,
) {
    println!("  last decision: {}", st.summary());
    if let Some(native) = &st.linux {
        // The plain line above already names the verb that installs it.
        if let Some(staged) = native.staged_build {
            match &native.staged_version {
                Some(version) => println!("  staged: {version} (build {staged})"),
                None => println!("  staged: build {staged}"),
            }
        }
        if linux_install_waits_for_a_start(st) {
            println!(
                "  waiting for aterm to start from it{}",
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
    let failed = st
        .staged_build
        .map_or(0, |staged| staged_tries(staged, lane));
    if failed > 0 {
        println!(
            "  failing applies: {failed} — a verified build is staged but didn\u{2019}t install"
        );
    }
    if let Some(report) = lane {
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
            println!("  install held back{when}: {}", report.last_refusal);
        }
        if !report.trial_wait.is_empty() {
            println!("  update trial: {}", report.trial_wait);
        }
    }
}

fn store_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured().is_some_and(|layout| {
        atpkg::which(&layout, tool).is_some()
            || atpkg::cli::live_without_shim(&layout, tool).is_some()
    })
}

fn pending_stub_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured()
        .is_some_and(|layout| atpkg::stub::pending_stub_exists(&layout, tool))
}

fn is_windowish(scan: &[OsString], force_session: bool, stdin_tty: bool) -> bool {
    !force_session
        && (scan.iter().any(|a| {
            matches!(
                a.to_string_lossy().as_ref(),
                "--window" | "--headless" | "--diagnose"
            )
        }) || !stdin_tty)
}

fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

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

struct WindowThemes;

impl aterm_cli::ThemeHost for WindowThemes {
    fn list(&self, request: &aterm_cli::ThemeListRequest) -> String {
        let format = match request.format {
            aterm_cli::ThemeListFormat::Text => aterm_gui::ThemeListFormat::Text,
            aterm_cli::ThemeListFormat::Tsv => aterm_gui::ThemeListFormat::Tsv,
        };
        aterm_gui::theme_list_report(request.appearance, format)
    }

    fn lookup(&self, id: &str) -> Result<aterm_types::scheme::CatalogTheme, String> {
        aterm_gui::theme_catalog_entry(id)
    }

    fn measure(&self, background: aterm_types::Rgb) -> aterm_types::Appearance {
        aterm_gui::theme_measured_appearance(background)
    }

    fn font_available(&self, family: &str) -> Option<bool> {
        Some(aterm_gui::theme_font_available(family))
    }
}

const COMPLETION_FLAGS: &[(&str, &str)] = &[
    ("--window", "open the GPU window explicitly"),
    ("--session", "force the transparent shell session"),
    ("--headless", "engine + control socket, no window"),
    (
        "--diagnose",
        "print diagnostics (version, build, renderer, config) and exit",
    ),
    (
        "--containment",
        "containment mode (user|safety|containment)",
    ),
    ("--sandbox", "shorthand for --containment containment"),
    ("--no-sandbox", "shorthand for --containment user"),
    (
        "--no-reroute",
        "restore the upstream names (cargo, rustc, z3, …) for this launch (aterm help reroute)",
    ),
    ("--quiet", "hide the session startup line"),
    (
        "--verbose",
        "print a one-line session summary to stderr on exit",
    ),
    ("--help", "print help and exit"),
    ("--version", "print the version and exit"),
    (
        "--completions",
        "print the bash, zsh or fish completion script",
    ),
];

fn routes_to_doctor(first: &str, rest: &[OsString]) -> bool {
    first == "doctor" && !rest.iter().any(|arg| arg == "-h" || arg == "--help")
}

#[path = "doctor.rs"]
mod doctor;

/// Compiler-provenance probe: `build.rs` `include!`s it to stamp [`BUILD_STAMP`]'s values; this
/// test-only mount runs the same code against real `-vV` fixtures.
#[cfg(test)]
mod compiler_probe;

#[cfg(test)]
mod tests {

    mod setup_startup {
        use super::super::{PreparedSetup, SetupAdmission, SetupStartup, prepare_setup};
        use aterm_gui::WorkstationSetupDecision::{self, Begin, Unavailable};
        use aterm_types::macos_setup::{Plan, Selection};
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;
        use std::rc::Rc;

        fn plan(selection: Selection) -> Plan {
            Plan {
                selection,
                ..Plan::default()
            }
        }

        struct Claim(Rc<Cell<usize>>);

        impl Drop for Claim {
            fn drop(&mut self) {
                self.0.set(self.0.get() - 1);
            }
        }

        #[derive(Default)]
        struct Host {
            revisions: VecDeque<Result<u64, &'static str>>,
            claims: VecDeque<Result<bool, &'static str>>,
            finished: VecDeque<Result<bool, &'static str>>,
            known_admissions: VecDeque<Result<Option<SetupAdmission>, &'static str>>,
            offers: VecDeque<WorkstationSetupDecision>,
            initial: VecDeque<Result<Plan, &'static str>>,
            authorizations: VecDeque<Result<(), &'static str>>,
            admissions: VecDeque<Option<u64>>,
            offered: Vec<Plan>,
            authorized: Vec<Plan>,
            active: Rc<Cell<usize>>,
            held_during_backoff: Vec<usize>,
            failures: Vec<String>,
            bind_calls: usize,
            claim_calls: usize,
            offer_calls: usize,
            invalid_plan: bool,
            pending_authorization: bool,
            terminal_authorization_error: bool,
            /// Whether a person's choice waits for the claim, look by look.
            choice_awaits: VecDeque<bool>,
            /// Waits before a retried write of the page's answer, each holding
            /// the claim.
            retry_waits: usize,
        }

        impl SetupStartup for Host {
            type Claim = Claim;

            fn choice_awaits_claim(&mut self) -> bool {
                self.choice_awaits.pop_front().unwrap_or(false)
            }

            fn bind_authorization(&mut self) -> Result<u64, String> {
                self.bind_calls += 1;
                assert_eq!(self.bind_calls, 1, "capture the startup review once");
                self.revision()
            }

            fn revision(&mut self) -> Result<u64, String> {
                self.revisions
                    .pop_front()
                    .unwrap_or(Ok(0))
                    .map_err(str::to_owned)
            }

            fn claim(&mut self) -> Result<Option<Claim>, String> {
                self.claim_calls += 1;
                assert_eq!(self.active.get(), 0, "never claim over our existing lease");
                self.claims
                    .pop_front()
                    .expect("unexpected claim attempt")
                    .map(|acquired| {
                        acquired.then(|| {
                            self.active.set(1);
                            Claim(Rc::clone(&self.active))
                        })
                    })
                    .map_err(str::to_owned)
            }

            fn finished(&mut self) -> Result<bool, String> {
                self.finished
                    .pop_front()
                    .expect("unexpected marker read")
                    .map_err(str::to_owned)
            }

            fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
                assert_eq!(self.active.get(), 1, "admission is read under our claim");
                self.known_admissions
                    .pop_front()
                    .unwrap_or(Ok(None))
                    .map_err(str::to_owned)
            }

            fn initial(&mut self) -> Result<Plan, String> {
                self.initial
                    .pop_front()
                    .unwrap_or(Ok(Plan::default()))
                    .map_err(str::to_owned)
            }

            fn offer(&mut self, initial: Plan) -> WorkstationSetupDecision {
                self.offer_calls += 1;
                self.offered.push(initial);
                assert_eq!(self.active.get(), 1, "offer requires an exclusive claim");
                self.offers.pop_front().expect("unexpected offer")
            }

            fn authorize(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
                assert_eq!(self.active.get(), 1, "authorization keeps the setup claim");
                self.authorized.push(plan.clone());
                let result = self.authorizations.pop_front().unwrap_or(Ok(()));
                self.pending_authorization = result.is_err() && !self.terminal_authorization_error;
                result.map_err(str::to_owned)?;
                Ok(self.admissions.pop_front().unwrap_or(Some(1)))
            }

            fn authorization_pending(&self) -> bool {
                self.pending_authorization
            }

            fn validate(&mut self, _: &Plan) -> Result<(), String> {
                if self.invalid_plan {
                    Err("the selected vault URL is invalid".into())
                } else {
                    Ok(())
                }
            }

            fn wait_holding_claim(&mut self) -> bool {
                assert_eq!(self.active.get(), 1, "this wait holds the claim");
                self.retry_waits += 1;
                self.choice_awaits_claim()
            }

            fn backoff(&mut self) {
                self.held_during_backoff.push(self.active.get());
            }

            fn report_failure(&mut self, why: &str) {
                self.failures.push(why.to_owned());
            }
        }

        #[test]
        fn a_window_retries_initial_claim_errors_before_its_first_offer() {
            let mut host = Host {
                claims: [Err("temporary state I/O"), Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(result, PreparedSetup::Authorized { .. }));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 1);
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.failures, ["temporary state I/O"]);
            assert_eq!(host.active.get(), 1, "worker inherits the live claim");
            drop(result);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn a_choice_made_while_the_plan_is_read_gets_the_claim_before_any_offer() {
            let mut host = Host {
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false)].into(),
                choice_awaits: [true, false].into(),
                offers: [Begin(Plan::default())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(result, PreparedSetup::Authorized { .. }));
            // It let the claim go before it waited, and offered only once no
            // choice waited for it.
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 1);
        }

        fn assert_pre_offer_failure_releases_claim(initial_read: bool) {
            let selected = plan(Selection::from_bits(1).unwrap());
            let mut host = Host {
                claims: [Ok(true), Ok(true)].into(),
                finished: if initial_read {
                    [Ok(false), Ok(false), Ok(false)].into()
                } else {
                    [
                        Err("pre-offer completion read failed"),
                        Ok(false),
                        Ok(false),
                    ]
                    .into()
                },
                initial: if initial_read {
                    [Err("pre-offer plan read failed"), Ok(selected.clone())].into()
                } else {
                    [Ok(selected.clone())].into()
                },
                offers: [Begin(selected.clone())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(result, PreparedSetup::Authorized { .. }));
            assert_eq!(
                host.held_during_backoff,
                [0],
                "before a decision, manual repair must be able to claim setup"
            );
            assert_eq!(host.claim_calls, 2, "reload only under a fresh claim");
            assert_eq!(host.offered.as_slice(), std::slice::from_ref(&selected));
            assert_eq!(host.authorized, [selected]);
            assert_eq!(host.active.get(), 1, "the admitted worker keeps its claim");
            drop(result);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn pre_offer_failure_releases_claim_after_a_plan_read_error() {
            assert_pre_offer_failure_releases_claim(true);
        }

        #[test]
        fn pre_offer_failure_releases_claim_after_a_completion_read_error() {
            assert_pre_offer_failure_releases_claim(false);
        }

        #[test]
        fn a_new_manual_run_during_startup_backoff_prevents_another_offer() {
            for unavailable_offer in [false, true] {
                let mut host = Host {
                    revisions: [Ok(7), Ok(7), Ok(8)].into(),
                    claims: [Ok(true), Ok(true)].into(),
                    finished: [Ok(false), Ok(false)].into(),
                    initial: if unavailable_offer {
                        [Ok(Plan::default())].into()
                    } else {
                        [Err("initial source unreadable")].into()
                    },
                    offers: if unavailable_offer {
                        [Unavailable].into()
                    } else {
                        VecDeque::new()
                    },
                    ..Host::default()
                };
                assert!(matches!(
                    prepare_setup(&mut host).unwrap(),
                    PreparedSetup::Unavailable
                ));
                assert_eq!(host.held_during_backoff, [0]);
                assert_eq!(host.claim_calls, 2);
                assert_eq!(host.offer_calls, usize::from(unavailable_offer));
                assert!(
                    host.authorized.is_empty(),
                    "never replace the manual worker's password"
                );
                assert!(host.revisions.is_empty());
                assert_eq!(host.active.get(), 0);
            }
        }

        #[test]
        fn an_admitted_pending_run_resumes_without_another_offer() {
            let selected = Plan {
                selection: Selection::from_bits(1 << 10).unwrap(),
                vault_url: "http://saved.local/v1/team".into(),
            };
            let mut host = Host {
                revisions: [Ok(7), Ok(7)].into(),
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(false)].into(),
                known_admissions: [Ok(Some(SetupAdmission {
                    revision: 7,
                    completed: false,
                }))]
                .into(),
                initial: [Ok(selected.clone())].into(),
                offers: [Begin(selected.clone())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert_eq!(
                host.offer_calls, 0,
                "an admitted choice needs no second offer"
            );
            assert!(
                host.authorized.is_empty(),
                "keep the live worker's admitted revision"
            );
            assert!(matches!(
                result,
                PreparedSetup::Authorized { revision: 7, .. }
            ));
            assert_eq!(host.initial, [Ok(selected)]);
            assert_eq!(host.active.get(), 1);
            drop(result);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn startup_admission_read_errors_never_fall_through_to_an_offer() {
            let mut host = Host {
                revisions: [Ok(7), Ok(7), Ok(7)].into(),
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false)].into(),
                known_admissions: [
                    Err("admission snapshot changed"),
                    Ok(Some(SetupAdmission {
                        revision: 7,
                        completed: false,
                    })),
                ]
                .into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(
                result,
                PreparedSetup::Authorized { revision: 7, .. }
            ));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.held_during_backoff, [0]);
            drop(result);
            assert_eq!(host.offer_calls, 0);
            assert!(host.authorized.is_empty());
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn startup_admission_confirmation_cannot_adopt_a_newer_run() {
            for (revision, completed) in [(8, false), (8, true), (7, true)] {
                let mut host = Host {
                    revisions: [Ok(7), Ok(7)].into(),
                    claims: [Ok(true)].into(),
                    finished: [Ok(false)].into(),
                    known_admissions: [Ok(Some(SetupAdmission {
                        revision,
                        completed,
                    }))]
                    .into(),
                    ..Host::default()
                };
                let result = prepare_setup(&mut host).unwrap();
                if revision == 7 {
                    assert!(matches!(result, PreparedSetup::Finished));
                } else {
                    assert!(matches!(result, PreparedSetup::Unavailable));
                }
                assert_eq!(host.offer_calls, 0);
                assert!(host.authorized.is_empty());
                assert_eq!(host.active.get(), 0);
            }
        }

        struct AuthorizationTicket {
            publication: u64,
            target: Option<u64>,
            pending: bool,
        }

        impl AuthorizationTicket {
            fn bind(publication: u64) -> Self {
                Self {
                    publication,
                    target: None,
                    pending: true,
                }
            }
        }

        #[derive(Default)]
        struct ReviewState {
            publication: u64,
            revision: u64,
            finished: bool,
            binds: usize,
            claims: usize,
            offers: usize,
            writes: usize,
            accepted: usize,
            backoffs: usize,
            failures: usize,
        }

        struct ReviewHost {
            state: Rc<RefCell<ReviewState>>,
            active: Rc<Cell<usize>>,
            authorization: Option<AuthorizationTicket>,
            selected: Plan,
            replace_during_offer: bool,
            fail_publication: bool,
            fail_confirmations: usize,
        }

        impl SetupStartup for ReviewHost {
            type Claim = Claim;

            fn bind_authorization(&mut self) -> Result<u64, String> {
                let mut state = self.state.borrow_mut();
                state.binds += 1;
                assert_eq!(state.binds, 1);
                self.authorization = Some(AuthorizationTicket::bind(state.publication));
                Ok(state.revision)
            }

            fn revision(&mut self) -> Result<u64, String> {
                Ok(self.state.borrow().revision)
            }

            fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
                assert_eq!(self.active.get(), 0);
                self.active.set(1);
                self.state.borrow_mut().claims += 1;
                Ok(Some(Claim(Rc::clone(&self.active))))
            }

            fn finished(&mut self) -> Result<bool, String> {
                assert_eq!(
                    self.state.borrow().offers,
                    0,
                    "no Plan-only completion shortcut"
                );
                Ok(self.state.borrow().finished)
            }

            fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
                assert_eq!(self.state.borrow().offers, 0);
                Ok(None)
            }

            fn initial(&mut self) -> Result<Plan, String> {
                Ok(self.selected.clone())
            }

            fn offer(&mut self, selected: Plan) -> WorkstationSetupDecision {
                let mut state = self.state.borrow_mut();
                state.offers += 1;
                if self.replace_during_offer {
                    state.revision += 1;
                    state.publication += 1;
                }
                Begin(selected)
            }

            fn authorize(&mut self, selected: &Plan) -> Result<Option<u64>, String> {
                assert_eq!(selected, &self.selected);
                assert_eq!(self.active.get(), 1);
                let ticket = self.authorization.as_mut().expect("carry the bound review");
                let mut state = self.state.borrow_mut();
                if ticket.publication != state.publication {
                    ticket.pending = false;
                    return Ok(None);
                }
                if ticket.target.is_none() {
                    if state.finished {
                        ticket.pending = false;
                        return Ok(None);
                    }
                    state.revision += 1;
                    state.publication += 1;
                    state.writes += 1;
                    ticket.publication = state.publication;
                    ticket.target = Some(state.revision);
                    state.finished = selected.selection.is_empty();
                    if std::mem::take(&mut self.fail_publication) {
                        return Err("owned admission published before its flush failed".into());
                    }
                } else if self.fail_confirmations > 0 {
                    self.fail_confirmations -= 1;
                    return Err("owned admission observation is not durable yet".into());
                }
                ticket.pending = false;
                state.accepted += 1;
                Ok(ticket.target)
            }

            fn authorization_pending(&self) -> bool {
                self.authorization
                    .as_ref()
                    .is_some_and(|ticket| ticket.pending)
            }

            fn wait_holding_claim(&mut self) -> bool {
                assert_eq!(self.active.get(), 1);
                let mut state = self.state.borrow_mut();
                assert_eq!(state.accepted, 0);
                state.backoffs += 1;
                false
            }

            fn backoff(&mut self) {
                unreachable!("no choice waits for this claim");
            }

            fn report_failure(&mut self, _: &str) {
                self.state.borrow_mut().failures += 1;
            }
        }

        fn review_host(selected: Plan) -> ReviewHost {
            ReviewHost {
                state: Rc::new(RefCell::new(ReviewState::default())),
                active: Rc::new(Cell::new(0)),
                authorization: None,
                selected,
                replace_during_offer: false,
                fail_publication: false,
                fail_confirmations: 0,
            }
        }

        #[test]
        fn a_changed_offer_is_rejected_without_acknowledging_or_reauthorizing_it() {
            for changed in [false, true] {
                let mut host = review_host(Plan::default());
                host.replace_during_offer = changed;
                let prepared = prepare_setup(&mut host).unwrap();
                assert_eq!(matches!(prepared, PreparedSetup::Unavailable), changed);
                assert_eq!(
                    matches!(prepared, PreparedSetup::Authorized { .. }),
                    !changed
                );
                let state = host.state.borrow();
                assert_eq!(state.binds, 1);
                assert_eq!(state.claims, 1);
                assert_eq!(state.offers, 1);
                assert_eq!(state.writes, usize::from(!changed));
                assert_eq!(state.accepted, usize::from(!changed));
                assert_eq!(state.backoffs, 0);
                drop(state);
                drop(prepared);
                assert_eq!(host.active.get(), 0);
            }
        }

        #[test]
        fn an_owned_empty_admission_is_confirmed_without_a_later_completion_shortcut() {
            let mut host = review_host(plan(Selection::none()));
            host.fail_publication = true;
            host.fail_confirmations = 1;
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Finished
            ));
            let state = host.state.borrow();
            assert_eq!(state.binds, 1);
            assert_eq!(state.claims, 1);
            assert_eq!(state.offers, 1);
            assert_eq!(state.writes, 1);
            assert_eq!(state.revision, 1);
            assert_eq!(state.accepted, 1);
            assert_eq!(state.backoffs, 2);
            assert_eq!(state.failures, 2);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn an_unadmitted_pending_run_remains_reviewable_through_same_revision_recovery() {
            let selected = plan(Selection::from_bits(1).unwrap());
            for recover_read in [false, true] {
                let mut host = Host {
                    revisions: [Ok(7), Ok(7), Ok(7)].into(),
                    claims: [Ok(true), Ok(true)].into(),
                    finished: [Ok(false), Ok(false), Ok(false)].into(),
                    initial: if recover_read {
                        [Err("temporary source read"), Ok(selected.clone())].into()
                    } else {
                        [Ok(selected.clone())].into()
                    },
                    offers: [Begin(selected.clone())].into(),
                    admissions: [Some(8)].into(),
                    ..Host::default()
                };
                let result = prepare_setup(&mut host).unwrap();
                assert!(matches!(
                    result,
                    PreparedSetup::Authorized { revision: 8, .. }
                ));
                assert_eq!(host.offered.as_slice(), std::slice::from_ref(&selected));
                assert_eq!(host.authorized.as_slice(), std::slice::from_ref(&selected));
                assert_eq!(host.claim_calls, if recover_read { 2 } else { 1 });
                assert_eq!(
                    host.held_during_backoff,
                    if recover_read { vec![0] } else { Vec::new() }
                );
                drop(result);
                assert_eq!(host.active.get(), 0);
            }
        }

        #[test]
        fn an_unreadable_startup_identity_cannot_be_adopted_after_a_manual_repair() {
            let mut host = Host {
                revisions: [Err("startup identity unreadable"), Ok(8)].into(),
                ..Host::default()
            };
            assert_eq!(
                prepare_setup(&mut host).err().as_deref(),
                Some("startup identity unreadable")
            );
            assert_eq!(host.revisions, [Ok(8)]);
            assert_eq!(host.claim_calls, 0);
            assert_eq!(host.offer_calls, 0);
            assert!(host.held_during_backoff.is_empty());
            assert_eq!(host.failures.len(), 1);
        }

        #[test]
        fn authorization_fence_returns_invalid_review_without_retry_or_authorization() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                invalid_plan: true,
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Unavailable
            ));
            assert!(host.authorized.is_empty());
            assert!(host.held_during_backoff.is_empty());
            assert_eq!(host.active.get(), 0);
            assert_eq!(host.failures, ["the selected vault URL is invalid"]);
        }

        #[test]
        fn authorization_fence_hands_off_the_committed_revision_before_any_later_read() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false), Err("completion read failed"), Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                admissions: [Some(7)].into(),
                ..Host::default()
            };
            let prepared = prepare_setup(&mut host).unwrap();
            assert!(matches!(
                prepared,
                PreparedSetup::Authorized { revision: 7, .. }
            ));
            assert_eq!(host.authorized, [Plan::default()]);
            assert!(host.held_during_backoff.is_empty());
            assert!(host.failures.is_empty());
            assert_eq!(host.finished, [Err("completion read failed"), Ok(false)]);
            drop(prepared);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn a_waiting_window_retries_a_failed_marker_read_then_takes_over() {
            let mut host = Host {
                claims: [Ok(false), Ok(true)].into(),
                finished: [
                    Err("temporary marker read"),
                    Ok(false),
                    Ok(false),
                    Ok(false),
                    Ok(false),
                ]
                .into(),
                offers: [Begin(Plan::default())].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Authorized { .. }
            ));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 1);
            assert_eq!(host.held_during_backoff, [0, 0]);
            assert_eq!(host.failures, ["temporary marker read"]);
        }

        #[test]
        fn begin_keeps_its_claim_and_decision_while_its_owned_write_recovers() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                authorizations: [Err("owned authorization write failed"), Ok(())].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Authorized { .. }
            ));
            assert_eq!(host.offer_calls, 1, "never repeat an accepted offer");
            assert_eq!(host.claim_calls, 1);
            // It holds the claim through one wait, which a person's choice
            // would end at once (no choice came), and through no other.
            assert_eq!(host.retry_waits, 1);
            assert!(host.held_during_backoff.is_empty());
            assert_eq!(host.failures, ["owned authorization write failed"]);
        }

        #[test]
        fn a_choice_made_while_the_answer_is_retried_ends_the_wait_and_gets_the_claim() {
            // The answer's write failed and waits to retry; a choice made meanwhile ends the
            // wait and holds the claim until the answer takes it again for its next try.
            let mut host = Host {
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                authorizations: [Err("owned authorization write failed"), Ok(())].into(),
                choice_awaits: [false, true].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(result, PreparedSetup::Authorized { .. }));
            assert_eq!(host.retry_waits, 1);
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.authorized.len(), 2);
            assert_eq!(
                host.active.get(),
                1,
                "the run inherits the claim taken again"
            );
            drop(result);
            // The choice's run holds the claim and finishes setup: the
            // answer is not written again.
            let mut host = Host {
                claims: [Ok(true), Ok(false)].into(),
                finished: [Ok(false), Ok(true)].into(),
                offers: [Begin(Plan::default())].into(),
                authorizations: [Err("owned authorization write failed")].into(),
                choice_awaits: [false, true].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Finished
            ));
            assert_eq!(host.authorized.len(), 1);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn an_unbound_authorization_failure_releases_the_choice_without_retry() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Begin(Plan::default())].into(),
                authorizations: [Err("the failed publication could not be pinned")].into(),
                terminal_authorization_error: true,
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Unavailable
            ));
            assert_eq!(host.claim_calls, 1);
            assert_eq!(host.offer_calls, 1);
            assert_eq!(host.authorized, [Plan::default()]);
            assert_eq!(
                host.failures,
                ["the failed publication could not be pinned"]
            );
            assert!(host.held_during_backoff.is_empty());
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn an_unavailable_window_releases_its_claim_before_waiting_and_reoffering() {
            let mut host = Host {
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false), Ok(false), Ok(false)].into(),
                offers: [Unavailable, Begin(Plan::default())].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Authorized { .. }
            ));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 2);
            assert_eq!(host.held_during_backoff, [0]);
            assert!(host.failures.is_empty());
        }

        #[test]
        fn a_finished_setup_stops_later_offers() {
            for mut host in [
                Host {
                    claims: [Ok(false)].into(),
                    finished: [Ok(true)].into(),
                    ..Host::default()
                },
                Host {
                    claims: [Ok(true)].into(),
                    finished: [Ok(true)].into(),
                    ..Host::default()
                },
                Host {
                    claims: [Ok(true), Ok(false)].into(),
                    finished: [Ok(false), Ok(true)].into(),
                    offers: [Unavailable].into(),
                    ..Host::default()
                },
            ] {
                assert!(matches!(
                    prepare_setup(&mut host).unwrap(),
                    PreparedSetup::Finished
                ));
                assert!(
                    host.offers.is_empty(),
                    "no duplicate offer after completion"
                );
                assert_eq!(host.active.get(), 0);
            }
        }

        #[test]
        fn checklist_choice_is_loaded_once_and_persisted_before_engine_authorization() {
            let saved = plan(Selection::from_bits(1).unwrap());
            let selected = plan(Selection::from_bits(2).unwrap());
            let mut host = Host {
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false), Ok(false)].into(),
                initial: [Err("selection read failed"), Ok(saved.clone())].into(),
                offers: [Begin(selected.clone())].into(),
                authorizations: [Err("authorization write failed"), Ok(())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            let PreparedSetup::Authorized { claim, .. } = &result else {
                panic!("the worker must inherit the authorized run")
            };
            assert!(
                Rc::ptr_eq(&claim.0, &host.active),
                "the worker inherits this setup's live lease"
            );
            assert_eq!(host.offered, [saved]);
            assert_eq!(host.authorized, [selected.clone(), selected]);
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.retry_waits, 1);
            drop(result);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn an_empty_checklist_finishes_without_entering_the_engine() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(true)].into(),
                offers: [Begin(plan(Selection::none()))].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Finished
            ));
            assert_eq!(host.authorized, [plan(Selection::none())]);
            assert_eq!(host.active.get(), 0);
        }
    }

    #[test]
    fn setup_windows_keep_scope_ssh_and_command_guards() {
        let window = [OsString::from("--window")];
        assert!(first_run_window_eligible(
            &window,
            DoctorStartupEnv::default()
        ));
        assert!(
            first_run_window_eligible(&[], DoctorStartupEnv::default()),
            "Dock and updater launches have a GUI offer surface"
        );
        for env in [
            DoctorStartupEnv {
                isolated: true,
                ..DoctorStartupEnv::default()
            },
            DoctorStartupEnv {
                outside_person_scope: true,
                ..DoctorStartupEnv::default()
            },
            // A window started over ssh leaves setup to one started at this Mac.
            DoctorStartupEnv {
                ssh: true,
                ..DoctorStartupEnv::default()
            },
        ] {
            assert!(!first_run_window_eligible(&window, env));
        }
        for args in [
            vec![
                OsString::from("--control-sock"),
                OsString::from("/tmp/private"),
            ],
            vec![OsString::from("-e"), OsString::from("zsh")],
            vec![OsString::from("--headless")],
        ] {
            assert!(!first_run_window_eligible(
                &args,
                DoctorStartupEnv::default()
            ));
        }
    }

    #[test]
    fn setup_relaunch_hint_preserves_private_flags_before_its_carried_command() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        for args in [
            osv(&["--control-sock", "/tmp/test.sock"]),
            osv(&["--control-sock", "/tmp/test.sock", "-e", "zsh"]),
            osv(&["--headless", "-e", "zsh"]),
        ] {
            assert!(
                !first_run_window_eligible(
                    first_run_window_args(&args, true),
                    DoctorStartupEnv::default(),
                ),
                "a relaunch hint must preserve {args:?}",
            );
        }
        let ordinary = osv(&["--window", "-d", "/tmp"]);
        assert!(first_run_window_eligible(
            first_run_window_args(&ordinary, true),
            DoctorStartupEnv::default(),
        ));
        let carried = osv(&["--window", "-d", "/tmp", "-e", "zsh", "--control-sock"]);
        assert_eq!(first_run_window_args(&carried, true), ordinary.as_slice());
        assert!(!first_run_window_eligible(
            first_run_window_args(&carried, false),
            DoctorStartupEnv::default(),
        ));
    }

    #[test]
    fn ordinary_window_options_keep_the_unified_setup_offer() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        for args in [
            osv(&["--gpu"]),
            osv(&["--cpu"]),
            osv(&["--font-px", "24", "--font", "Menlo", "--scale", "2"]),
            osv(&["--columns", "120", "--lines", "40", "-d", "/tmp"]),
            osv(&[
                "--shell",
                "zsh",
                "--shell-integration",
                "--no-shell-integration",
            ]),
        ] {
            assert!(
                first_run_window_eligible(&args, DoctorStartupEnv::default()),
                "ordinary window options must retain the setup surface: {args:?}",
            );
        }
        for args in [
            osv(&["--gpu", "-e", "zsh"]),
            osv(&["--cpu", "--headless"]),
            osv(&["--font", "Menlo", "--control-sock", "/tmp/private"]),
            osv(&["--columns", "120", "--diagnose"]),
            osv(&["--gpu", "--containment", "user"]),
            osv(&["--gpu", "--bogus"]),
            osv(&["--font-px"]),
        ] {
            assert!(
                !first_run_window_eligible(&args, DoctorStartupEnv::default()),
                "ordinary options cannot erase a scope/command refusal: {args:?}",
            );
        }
    }

    #[test]
    fn an_update_successor_of_a_shell_started_window_is_still_a_window() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        for argv in [
            osv(&["--window"]),
            osv(&[]),
            osv(&["--window", "--window", "-d", "/tmp"]),
        ] {
            let successor = aterm_update::window_successor_args(argv.clone().into_iter());
            assert!(
                is_windowish(&successor, false, true),
                "{argv:?} -> {successor:?} must route to the window with a TTY on stdin"
            );
        }
        assert!(!is_windowish(
            &aterm_update::reexec_forwarded_args(osv(&["--window"]).into_iter()),
            false,
            true
        ));
        for argv in [
            osv(&["new-window"]),
            osv(&["new-window", "-d", "/tmp"]),
            osv(&["new-tab"]),
            osv(&["split-pane", "-H"]),
        ] {
            let running =
                aterm_update::running_window_successor_args(argv.clone().into_iter(), None);
            let mode_args = strip_mode_flags(&running);
            assert!(
                is_windowish(&running, false, true)
                    && mode_args.first().is_none_or(|first| {
                        aterm_cli::Verb::from_operand(&first.to_string_lossy()).is_none()
                    }),
                "{argv:?} -> {running:?}: the window parser gets {mode_args:?}"
            );
            let boot = aterm_update::window_successor_args(argv.clone().into_iter());
            assert!(
                boot.first()
                    .is_some_and(
                        |first| aterm_cli::Verb::from_operand(&first.to_string_lossy()).is_some()
                    ),
                "{argv:?} -> {boot:?}: the boot swap's re-exec is routed as the verb"
            );
        }
        assert!(is_windowish(&osv(&[]), false, false), "no TTY: the window");
        assert!(!is_windowish(&osv(&[]), false, true), "a TTY: the session");
        assert!(
            !is_windowish(&osv(&["--window"]), true, false),
            "--session wins"
        );
    }

    use super::*;

    /// Windows has no updater, so `aterm update status` and `check` land on this line: the
    /// state, then the one lane that updates a Windows copy (the words `aterm help update`
    /// prints), never an app bundle.
    #[cfg(windows)]
    #[test]
    fn windows_update_line_names_the_lane_that_updates_it() {
        let line = update_unreadable_line("0.94.0");
        assert!(line.starts_with("aterm 0.94.0 "), "{line}");
        assert!(line.contains(aterm_cli::WINDOWS_UPDATE_LANE), "{line}");
        assert!(!line.contains("aterm.app"), "{line}");
        assert!(!line.contains("macOS"), "{line}");
    }

    fn flapped_cast() -> String {
        let mut frame = String::from("\\u001b[?1049h\\u001b[2J");
        for r in 0..8 {
            frame.push_str(&format!("\\u001b[{};1HA{r}", r + 1));
        }
        frame.push_str("\\u001b[6;3H");
        format!(
            "{{\"version\": 2, \"width\": 20, \"height\": 8}}\n\
             [0.100000, \"o\", \"{frame}\"]\n\
             [1.000000, \"r\", \"20x7\"]\n\
             [1.000300, \"o\", \"\\u001b[?1000h\"]\n\
             [1.000700, \"r\", \"20x8\"]\n\
             [1.005000, \"o\", \"\\u001b[3;1H\\u001b[KB2\\u001b[6;3H\"]\n"
        )
    }

    #[test]
    fn ctl_cast_drift_computes_the_report_client_side() {
        let cast = flapped_cast();
        let screen: Vec<String> = ["A1", "A2", "B2", "A4", "A5", "A6", "A7", ""]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let job = aterm_ctl::CastDriftJob {
            cast: &cast,
            screen: Some(&screen),
            cursor: Some((5, 2)),
            alt: Some(true),
            args: "",
            cut: aterm_ctl::DriftCut::Quiescent,
            until: None,
            peer: aterm_ctl::DriftPeer::PreUndo,
        };
        let report = ctl_cast_drift(&job).expect("a report");
        let head = report.lines().next().unwrap();
        for key in [
            "verdict=desync",
            "fidelity=pass",
            "culprit=1.000000",
            "cut=quiescent",
            "dropped=-",
            "undo=off",
            "computed=client",
        ] {
            assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
        }
        assert!(report.contains("\nrow 7 live= expected=A7\n"), "{report}");
        let offline = ctl_cast_drift(&aterm_ctl::CastDriftJob {
            screen: None,
            cursor: None,
            alt: None,
            cut: aterm_ctl::DriftCut::File,
            peer: aterm_ctl::DriftPeer::Unknown,
            ..job
        })
        .expect("a report");
        assert!(
            offline.starts_with("OK 9 verdict=desync fidelity=- against=replay"),
            "{offline}"
        );
        let early = ctl_cast_drift(&aterm_ctl::CastDriftJob {
            screen: None,
            cut: aterm_ctl::DriftCut::File,
            until: Some(0.5),
            peer: aterm_ctl::DriftPeer::Unknown,
            ..job
        })
        .expect("a report");
        assert!(early.starts_with("OK 0 verdict=clean"), "{early}");
        let bad = ctl_cast_drift(&aterm_ctl::CastDriftJob {
            args: "count=2",
            ..job
        })
        .expect_err("usage");
        assert!(bad.starts_with("ERR usage: cast drift"), "{bad}");
        let broken = ctl_cast_drift(&aterm_ctl::CastDriftJob {
            cast: "not a cast",
            ..job
        })
        .expect_err("unparseable");
        assert!(broken.contains("line 1"), "{broken}");
    }

    #[test]
    fn a_quiet_flap_on_an_aterm_before_the_undo_is_named_client_side() {
        let mut frame = String::from("\\u001b[?1049h\\u001b[2J");
        for r in 0..8 {
            frame.push_str(&format!("\\u001b[{};1HL{r}", r + 1));
        }
        frame.push_str("\\u001b[6;3H");
        let cast = format!(
            "{{\"version\": 2, \"width\": 20, \"height\": 8}}\n\
             [0.100000, \"o\", \"{frame}\"]\n\
             [1.000000, \"r\", \"20x7\"]\n\
             [1.000700, \"r\", \"20x8\"]\n\
             [1.005000, \"o\", \"\\u001b[3;1H\\u001b[KB2\\u001b[6;3H\"]\n"
        );
        let old: Vec<String> = ["L1", "L2", "B2", "L4", "L5", "L6", "L7", ""]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let job = aterm_ctl::CastDriftJob {
            cast: &cast,
            screen: Some(&old),
            cursor: Some((5, 2)),
            alt: Some(true),
            args: "",
            cut: aterm_ctl::DriftCut::Quiescent,
            until: None,
            peer: aterm_ctl::DriftPeer::PreUndo,
        };
        for peer in [aterm_ctl::DriftPeer::PreUndo, aterm_ctl::DriftPeer::Unknown] {
            let report =
                ctl_cast_drift(&aterm_ctl::CastDriftJob { peer, ..job }).expect("a report");
            let head = report.lines().next().unwrap();
            for key in [
                "verdict=desync",
                "fidelity=pass",
                "culprit=1.000000",
                "undo=off",
                "computed=client",
            ] {
                assert!(
                    head.split_whitespace().any(|t| t == key),
                    "{peer:?}: {key} in {head}"
                );
            }
            assert!(report.contains("\nrow 0 live=L1 expected=L0\n"), "{report}");
        }
        let new: Vec<String> = ["L0", "L1", "B2", "L3", "L4", "L5", "L6", "L7"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let report = ctl_cast_drift(&aterm_ctl::CastDriftJob {
            screen: Some(&new),
            cut: aterm_ctl::DriftCut::File,
            peer: aterm_ctl::DriftPeer::Unknown,
            ..job
        })
        .expect("a report");
        let head = report.lines().next().unwrap();
        assert!(head.contains(" verdict=clean fidelity=pass "), "{head}");
        assert!(!head.contains(" undo=off"), "{head}");
    }

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

    #[test]
    fn a_missing_ledger_fails_check_everywhere_and_status_only_on_a_mac() {
        assert!(
            no_ledger_fails(true),
            "check cannot do its job without a ledger"
        );
        assert_eq!(no_ledger_fails(false), cfg!(target_os = "macos"));
    }

    #[test]
    fn the_update_line_claims_up_to_date_only_over_a_completed_check() {
        let checked =
            aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
        let now = checked + 12 * 60;
        let say = |st: &aterm_update::UpdateStatus,
                   lane: Option<&aterm_update::ApplyLaneReport>| {
            update_summary("0.91.0", 100, st, lane, now, true, true, &DevCopy::No, "").0
        };
        let up_to_date = |line: &str| line.contains("is up to date");
        assert!(up_to_date(&say(&update_status(), None)));
        let mut never = update_status();
        never.updated_at = String::new();
        assert!(!up_to_date(&say(&never, None)), "never checked");

        let mut waiting = update_status();
        waiting.linux = Some(aterm_update::LinuxUpdateStatus {
            installed_build: 101,
            installed_version: "0.92.0".into(),
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            trial_phase: Some("Installed".into()),
            trial_starts: 0,
            trial_healthy: false,
            refused_newer: false,
        });
        assert!(!up_to_date(&say(&waiting, None)), "an unfinished install");
        waiting.linux.as_mut().expect("linux").trial_healthy = true;
        assert!(up_to_date(&say(&waiting, None)), "a confirmed install");
        let native = waiting.linux.as_mut().expect("linux");
        native.trial_phase = None;
        native.refused_newer = true;
        assert!(
            !up_to_date(&say(&waiting, None)),
            "a rolled-back newer build"
        );

        let mut staged = update_status();
        staged.staged_build = Some(101);
        staged.staged_version = Some("0.92.0".into());
        staged.failing_applies = 3;
        let tried = |target: u64, failures: u32| aterm_update::ApplyLaneReport {
            last_failure_target_build: target,
            failures_for_target: failures,
            ..Default::default()
        };
        let failed = |line: &str| line.contains("didn\u{2019}t install");
        assert!(
            failed(&say(&staged, Some(&tried(101, 2)))),
            "this build's failures"
        );
        for lane in [tried(99, 3), tried(0, 0)] {
            assert!(!failed(&say(&staged, Some(&lane))), "{lane:?}");
        }
    }

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
        atpkg::platform::spawn_detached(
            std::ffi::OsStr::new("/bin/sh"),
            &args,
            Some(("ATPKG_SPAWNER_PID", atpkg::cli::SPAWNER_DETACHED)),
        )
        .expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
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

    static NO_REROUTE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let dash_e = osv(&["-e", "vim"]);
        assert_eq!(payload_boundary(&dash_e), 0);
        assert!(
            plain_launch_request(&dash_e[..0], env).is_some(),
            "the pre-boundary scan of `-e vim` is an empty, maximally eligible argv — \
             the gate must never be handed it"
        );
        assert!(plain_launch_request(&osv(&[]), env).is_some());
        assert!(plain_launch_request(&osv(&["--window"]), env).is_some());
    }

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
        for operand in ["", "--window", "-d", "--headless", "--diagnose"] {
            assert_eq!(
                alias_route("aterm-gui", operand),
                AliasRoute::AliasWindow,
                "aterm-gui {operand:?}"
            );
        }
        assert_eq!(alias_route("aterm-ctl", "new-tab"), AliasRoute::Ctl);
        assert_eq!(alias_route("atpkg", "new-tab"), AliasRoute::Pkg);
        assert_eq!(alias_route("agh", "release"), AliasRoute::Agh);
        assert_eq!(alias_route("aterm-fleet", ""), AliasRoute::Fleet);
        assert_eq!(alias_route("aterm-drive", ""), AliasRoute::Drive);
        assert_eq!(
            alias_route("aterm-askpass", "Password:"),
            AliasRoute::Askpass
        );
        assert_eq!(
            alias_route("aterm-machines-askpass", "admin@lab-07.local's password: "),
            AliasRoute::MachinesAskpass
        );
        assert_eq!(alias_route("aterm-link", ""), AliasRoute::Link);
        assert_eq!(alias_route("aterm-link", "serve"), AliasRoute::Link);
        assert_eq!(
            alias_route("aterm-open-url", "https://claude.ai/x"),
            AliasRoute::OpenUrl
        );
        for operand in ["", ".git/COMMIT_EDITMSG", "+3", "new-window", "edit"] {
            assert_eq!(
                alias_route("amacs", operand),
                AliasRoute::Amacs,
                "{operand:?}"
            );
        }
        for name in ["aterm", "aterm-cli", "my-renamed-aterm"] {
            assert_eq!(
                alias_route(name, "new-window"),
                AliasRoute::FrontDoor,
                "{name}"
            );
            assert_eq!(alias_route(name, ""), AliasRoute::FrontDoor, "{name}");
        }
    }

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

    /// Which windowed image the console image hands a window to, against a real folder: the
    /// shipped name in an install folder, only as a file; in a build tree (cargo's `.fingerprint`
    /// beside the exe) the target name, never the `aterm-gui.exe` there (crate aterm-gui's thin
    /// dev bin).
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A build tree's windowed image stands for this build only when it is as new as this one,
    /// give or take the two bins' link spread (measured 6 ms, either order): `cargo run -p aterm`
    /// relinks the console image alone.
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

    /// After the call no std handle is inheritable, or a capturing shell's
    /// `$o = & aterm --window` would hang until the window closed.
    #[cfg(windows)]
    #[test]
    fn the_std_handles_are_not_inheritable_once_the_sibling_is_about_to_start() {
        windowed_sibling::stop_inheriting_std_handles();
        assert!(
            !windowed_sibling::a_std_handle_is_inheritable(),
            "a std handle would still be carried into the detached window"
        );
    }

    /// `RegisterApplicationRestart` fires after a Restart-Manager reboot and (`dwFlags = 0`)
    /// after a crash or hang, and its command line comes back through this router.
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
        assert_eq!(
            alias_route("aterm-gui", line),
            AliasRoute::AliasWindowVerb,
            "the relaunched image is current_exe(), normally aterm-gui.exe"
        );
    }

    #[test]
    fn the_mode_flags_are_stripped_before_the_window_but_never_inside_a_payload() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        assert_eq!(strip_mode_flags(&osv(&["--window"])), osv(&[]));
        assert_eq!(
            strip_mode_flags(&osv(&["--window", "-d", "/tmp"])),
            osv(&["-d", "/tmp"])
        );
        assert_eq!(strip_mode_flags(&osv(&["--session"])), osv(&[]));
        assert_eq!(
            strip_mode_flags(&osv(&["--window", "-e", "sh", "--window"])),
            osv(&["-e", "sh", "--window"])
        );
        assert_eq!(payload_boundary(&osv(&["-d", "/tmp"])), 2);
        assert_eq!(payload_boundary(&osv(&["--window", "--", "x"])), 1);
        assert_eq!(
            strip_flags(&osv(&["--window", "--session"]), &["--window"]),
            osv(&["--session"])
        );
    }

    #[test]
    fn no_reroute_is_stripped_before_dispatch_and_sets_the_marker() {
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let env = atpkg::reroute::PASSTHROUGH_ENV;
        let engaged = || atpkg::reroute::engaged(std::env::var(env).ok().as_deref());
        aterm_log::env::unset(env);
        aterm_log::env::set(env, "1");
        assert_eq!(take_no_reroute(&osv(&["--window"])), osv(&["--window"]));
        assert!(
            !engaged(),
            "no flag, no escape — whatever the environment carried"
        );
        assert_eq!(
            take_no_reroute(&osv(&["-e", "sh", "--no-reroute"])),
            osv(&["-e", "sh", "--no-reroute"])
        );
        assert!(!engaged(), "a payload token must not engage the escape");
        assert_eq!(
            take_no_reroute(&osv(&["--window", "--no-reroute", "-d", "/tmp"])),
            osv(&["--window", "-d", "/tmp"])
        );
        assert!(engaged(), "the flag must establish {env}=1");
        assert_eq!(
            plain_launch_request(
                &osv(&["--window", "--no-reroute"]),
                aterm_cli::LaunchEnv::default()
            ),
            None,
            "a --no-reroute launch fails closed to a local spawn"
        );
        let announced = atpkg::reroute::ANNOUNCED_ENV;
        for flags in [&["--window"][..], &["--window", "--no-reroute"][..]] {
            aterm_log::env::set(announced, "1");
            let _ = take_no_reroute(&osv(flags));
            assert!(
                std::env::var_os(announced).is_none(),
                "{flags:?}: an inherited {announced} would silence the session's signposts"
            );
        }
        aterm_log::env::unset(env);
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-front-door-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

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

    /// A prefix that refuses the `mkdir`, or a symlink at `agents/`, hands nothing (said on
    /// stderr), creates nothing, and `hand_agents_dir` removes an inherited value.
    #[cfg(unix)]
    #[test]
    fn a_refused_agents_dir_hands_nothing_and_clears_an_inherited_stray() {
        use std::os::unix::fs::PermissionsExt as _;
        let _env_lock = NO_REROUTE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scratch = scratch_dir("agents-refused");
        let var = atpkg::reroute::AGENTS_DIR_ENV;
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
        let filed = atpkg::store::Layout {
            prefix: scratch.join("filed"),
        };
        std::fs::create_dir_all(&filed.prefix).unwrap();
        std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
        assert_eq!(agents_dir_handoff(Some(&filed)), None);
        aterm_log::env::set(var, real.to_str().unwrap());
        hand_agents_dir(Some(&linked));
        assert_eq!(
            std::env::var_os(var),
            None,
            "an inherited stray must not survive a refusal"
        );
        // SAFETY: getuid() takes no arguments and cannot fail.
        if unsafe { libc::getuid() } == 0 {
            let _ = std::fs::remove_dir_all(&scratch);
            aterm_spec::host::could_not_run(
                "running as root, where a read-only prefix refuses nothing: the refusal leg cannot run",
            );
        }
        let layout = atpkg::store::Layout {
            prefix: scratch.join("pkg"),
        };
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(agents_dir_handoff(Some(&layout)), None);
        assert!(!layout.agents_dir().exists(), "nothing created");
        aterm_log::env::set(var, real.to_str().unwrap());
        hand_agents_dir(Some(&layout));
        assert_eq!(std::env::var_os(var), None);
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
}

#[cfg(test)]
mod open_probe_alias_boundary_tests {
    use super::{AliasRoute, alias_route};

    #[test]
    fn a_window_alias_serves_its_open_probe_child_at_the_front_door() {
        let verb = aterm_containment::consent::OPEN_PROBE_VERB;
        assert_eq!(alias_route("aterm-gui", ""), AliasRoute::AliasWindow);
        assert_eq!(
            alias_route("aterm-windowed", "new-window"),
            AliasRoute::AliasWindowVerb
        );
        assert_eq!(alias_route("aterm-ctl", "status"), AliasRoute::Ctl);
        assert_eq!(alias_route("aterm-askpass", verb), AliasRoute::Askpass);
        let observed: Vec<_> = [
            "aterm",
            "aterm-cli",
            "aterm-gui",
            "aterm-windowed",
            "renamed-copy",
        ]
        .into_iter()
        .map(|name| (name, alias_route(name, verb)))
        .collect();
        assert!(
            observed
                .iter()
                .all(|(_, route)| *route == AliasRoute::FrontDoor),
            "a window alias must route its probe child before window mode: {observed:?}"
        );
    }
}
