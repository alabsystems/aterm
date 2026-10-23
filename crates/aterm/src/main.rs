// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: the one binary.

#![cfg_attr(all(unix, trust_verify), feature(register_tool))]
#![cfg_attr(all(unix, trust_verify), register_tool(trust))]

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::ExitCode;

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
#[cfg(not(unix))]
fn link_unavailable(verb: &str) -> ExitCode {
    eprintln!("aterm {verb}: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

/// `aterm machines` where it cannot exist: `aterm-machines` drives macOS's own tools and is a
#[cfg(not(target_os = "macos"))]
fn machines_unavailable() -> ExitCode {
    eprintln!("aterm machines: not available on this platform (macOS only)");
    ExitCode::FAILURE
}

/// `aterm secrets` where it cannot exist: the vault keeps its secrets private with POSIX file
#[cfg(not(unix))]
fn secrets_unavailable() -> ExitCode {
    eprintln!("aterm secrets: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

/// The `aterm ctl` verbs this binary answers in-process because it links the terminal engine
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

/// `cast drift` computed by the client: against a server that predates the verb (from the
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

/// The build facts of this binary, the only place any of them is compiled in. The version is
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

/// The executable's inherited-lane ownership admission. The Link library
#[cfg(unix)]
fn dispatch_link(args: &[String]) -> ExitCode {
    /// The startup take of the launcher's two bridge lanes: the one unsafe call this
    #[cfg_attr(trust_verify, trust::skip)]
    fn take_startup_lanes() -> std::io::Result<aterm_link::bridge::InheritedLanes> {
        // SAFETY: the launcher transfers eligible lanes exclusively to this startup route, and `dispatch_link` is the only route that reaches this function.
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

    if routes_to_doctor(&first, rest.get(1..).unwrap_or(&[])) {
        return doctor::main_entry(rest.get(1..).unwrap_or(&[]));
    }

    let mode_free = matches!(
        first.as_str(),
        "help" | "-h" | "--help" | "-V" | "--version"
    ) || aterm_cli::DIAG_COMMANDS
        .iter()
        .any(|(name, _)| *name == first);
    if mode_free {
        let _ = aterm_cli::parse_args(rest);
        return ExitCode::SUCCESS;
    }

    if first == "amacs" {
        return aterm_cli::amacs_launch::run(rest.get(1..).unwrap_or(&[]).to_vec());
    }

    if aterm_cli::is_tool_candidate(Some(first.as_str())) && store_resolves(&first) {
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
                "aterm: reroute dir not created ({error}); this session does not reroute the upstream Rust names — `aterm pkg doctor` explains (aterm help reroute)"
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
                        "aterm: reroute stubs not laid ({error}); this session does not reroute the upstream Rust names — `aterm pkg doctor` explains (aterm help reroute)"
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
#[cfg(target_os = "linux")]
fn park_until_the_shell_spoke(spoke: &std::sync::atomic::AtomicBool) {
    while !spoke.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::park();
    }
}

/// A silent start still up defers the trial's verdict, once: with no shell output within
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

/// The session's head watch ([`atpkg::vendor_direct::watch::run_host`]): the window's cadence,
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

/// Who a refused `agents/` leaves without the managed agents, in the session's words.
const SESSION_REACH: &str = "this session";

/// The `$ATERM_AGENTS_DIR` decision: `Some(dir)`, the absolute managed `<prefix>/agents/`
fn agents_dir_handoff(layout: Option<&atpkg::store::Layout>) -> Option<String> {
    let layout = layout?;
    if !layout.prefix.is_absolute() {
        eprintln!(
            "aterm: managed `claude`/`codex` not first on PATH: package prefix {} is not absolute",
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

/// Set [`agents_dir_handoff`]'s answer in this process's environment as
fn hand_agents_dir(layout: Option<&atpkg::store::Layout>) {
    match agents_dir_handoff(layout) {
        Some(dir) => aterm_log::env::set(atpkg::reroute::AGENTS_DIR_ENV, dir),
        None => aterm_log::env::unset(atpkg::reroute::AGENTS_DIR_ENV),
    }
}

/// Whether this `--session` launch is a person's terminal (stdin is a terminal) rather than a
fn session_lane_is_interactive() -> bool {
    stdin_is_terminal()
}

/// Whether this launch spawns the detached pass: the machine-wide rule of the window's six-hour
fn session_pass_due(layout: &atpkg::store::Layout, now_unix: i64) -> bool {
    use aterm_update_core::pkg_check;
    pkg_check::full_pass_owed(&atpkg::status::pass_stamps(layout, now_unix), now_unix).is_some()
        && pkg_check::claim(
            &layout.session_pass_stamp(),
            now_unix,
            pkg_check::PASS_SPACING_SECS,
        )
}

/// Why the launch's `pkg machine apply` claim stood aside ([`atpkg::machine::LaunchApply::runs`]),
fn log_launch_apply_stood_aside(why: &'static str) {
    aterm_log::info!("{why}");
}

/// One detached `aterm pkg machine apply`: the lock-free host settings, once a day
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

/// One detached `aterm pkg update`: this binary's `pkg` verb on the window's argv
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

/// The detached pass's argv: `pkg update`, the window's scheduled-pass flags
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

/// How an invocation under an argv0 alias name is served; pure, for the test. On Windows the
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum AliasRoute {
    /// `aterm-ctl` — the control client.
    Ctl,
    /// `atpkg` — the package manager.
    Pkg,
    /// `abrew` / `atpkg-abrew` — aterm's Homebrew (docs/DESIGN-abrew-2026-10-19.md):
    Abrew,
    /// `agh` — the GitHub client (docs/DESIGN-agh-2026-10-19.md): an alias so agents and
    Agh,
    /// `aterm-fleet` — this machine's fleet membership.
    Fleet,
    /// `aterm-drive` — the agent drive CLI.
    Drive,
    /// `aterm-open-url`, the `BROWSER` a Claude Code login renewal runs
    OpenUrl,
    /// `aterm-link`, the fabric bridge: an alias as well as a verb because `[fabric] command`
    Link,
    /// `aterm-gui <new-tab|new-window|split-pane> …`: a windowing verb typed at, or committed
    AliasWindowVerb,
    /// `aterm-gui …`: the window, with the plain-launch routing policy ([`gui_alias_entry`]).
    AliasWindow,
    /// `aterm-askpass …`, the `SUDO_ASKPASS` helper (`docs/DESIGN-sudo-askpass-2026-10-15.md`):
    Askpass,
    /// `aterm-machines-askpass <prompt>`, the `SSH_ASKPASS` program `aterm machines enroll`
    MachinesAskpass,
    /// `amacs …`, aterm's editor (`aterm edit`, docs/DESIGN-amacs-2026-10-19.md §8.1): an alias
    Amacs,
    /// Not an alias: `aterm`, `aterm-cli` (the front door forced to a session), a renamed copy.
    FrontDoor,
}

/// The alias decision: argv0's file stem (already `EXE_SUFFIX`-trimmed) plus the
fn alias_route(argv0: &str, first: &str) -> AliasRoute {
    match argv0 {
        "aterm-gui" | "aterm-windowed" if first == aterm_containment::consent::OPEN_PROBE_VERB => {
            AliasRoute::FrontDoor
        }
        "aterm-ctl" => AliasRoute::Ctl,
        "atpkg" => AliasRoute::Pkg,
        "abrew" | "atpkg-abrew" => AliasRoute::Abrew,
        "agh" => AliasRoute::Agh,
        "aterm-fleet" => AliasRoute::Fleet,
        "aterm-drive" => AliasRoute::Drive,
        "aterm-askpass" => AliasRoute::Askpass,
        "aterm-machines-askpass" => AliasRoute::MachinesAskpass,
        "aterm-link" => AliasRoute::Link,
        "aterm-open-url" => AliasRoute::OpenUrl,
        "amacs" => AliasRoute::Amacs,
        "aterm-gui" | "aterm-windowed" => match aterm_cli::Verb::from_operand(first) {
            Some(verb) if verb.is_windowing() => AliasRoute::AliasWindowVerb,
            _ => AliasRoute::AliasWindow,
        },
        _ => AliasRoute::FrontDoor,
    }
}

/// The `aterm-gui` argv0 alias: the window mode, through the plain-launch policy and the
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
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum WindowHost {
    /// Run the window here: in the windowed image (the `aterm-gui`/`aterm-windowed` aliases),
    ThisProcess,
    /// The console image on Windows, asked for a real window: check the window's arguments
    WindowedSibling,
}

/// The host for a window-shaped front-door launch (`aterm --window`, the no-TTY launch), from
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

/// Run the window for `args` where `host` says, and return the route's exit code.
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
        let setup_args = first_run_window_args(&args, restoring_window);
        first_run_window_eligible(setup_args, doctor_startup_env())
            .then_some(apply_first_run_doctor as fn() -> bool)
    };
    #[cfg(not(target_os = "macos"))]
    let first_run = None;
    #[cfg(target_os = "macos")]
    aterm_gui::set_initial_setup_handlers(aterm_gui::InitialSetupHandlers {
        apply: restart_initial_setup,
        check: check_initial_setup,
        restore: restore_initial_setup,
        cancel: cancel_initial_setup,
        status: initial_setup_status,
    });
    aterm_gui::register_resource_runner();
    #[expect(
        clippy::disallowed_methods,
        reason = "the one window door: every window route reaches the GUI here"
    )]
    aterm_gui::main_entry_with_first_run(args, first_run);
    ExitCode::SUCCESS
}

/// First-run setup belongs to an interactive window launch, never a nested
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default)]
struct DoctorStartupEnv {
    isolated: bool,
    outside_person_scope: bool,
}

#[cfg(target_os = "macos")]
fn doctor_startup_env() -> DoctorStartupEnv {
    DoctorStartupEnv {
        isolated: std::env::var_os("ATERM_STATE_HOME").is_some()
            || std::env::var_os("CI").is_some(),
        outside_person_scope: aterm_cli::macos_doctor::lasting_scope_refusal().is_some(),
    }
}

/// A new GUI window has its own setup surface even when it inherits markers
#[cfg(any(target_os = "macos", test))]
fn first_run_window_eligible(args: &[OsString], env: DoctorStartupEnv) -> bool {
    if env.isolated || env.outside_person_scope {
        return false;
    }
    let args = strip_mode_flags(args);
    let args = strip_flags(&args, &[NO_REROUTE_FLAG]);
    aterm_gui::initial_setup_window_args(&args)
}

/// A running-window successor does not execute its carried command. Its launch
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
            let mut password = None;
            let prepared = prepare_first_run_doctor(
                |initial| {
                    let mut answer =
                        aterm_gui::await_initial_setup_countdown_while(initial, || {
                            aterm_cli::macos_doctor::initial_setup_finished().unwrap_or(false)
                        });
                    password = answer.password.take();
                    answer
                },
                true,
            );
            run_prepared_setup(prepared, password);
        }) {
        Ok(_) => true,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor worker could not start: {why}");
            aterm_gui::report_workstation_setup_failure();
            false
        }
    }
}

/// Reserve one offer across app processes before showing it. Claiming reads only
#[cfg(target_os = "macos")]
fn prepare_first_run_doctor(
    countdown: impl FnMut(aterm_types::macos_setup::Plan) -> aterm_gui::SetupAnswer,
    retry: bool,
) -> PreparedFirstRun<
    aterm_cli::macos_doctor::AutomaticSetupClaim,
    SetupDecisions<
        Option<aterm_cli::macos_doctor::InitialSetupAuthorization>,
        aterm_cli::macos_doctor::InitialSetupCancellation,
    >,
> {
    let mut host = LiveSetupStartup {
        countdown,
        receipt: None,
        cancel_receipt: None,
        authorization: None,
        cancellation: aterm_cli::macos_doctor::InitialSetupCancellation::default(),
        last_failure: None,
        observation: None,
    };
    let setup = match prepare_setup(&mut host, retry) {
        Ok(prepared) => prepared,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor could not start: {why}");
            aterm_gui::report_workstation_setup_failure();
            PreparedSetup::Unavailable
        }
    };
    PreparedFirstRun {
        setup,
        decisions: SetupDecisions {
            authorization: host.authorization,
            cancellation: host.cancellation,
        },
    }
}

/// The startup coordinator reads only aterm's state until an offer authorizes
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct SetupAdmission {
    revision: u64,
    completed: bool,
}

#[cfg(any(target_os = "macos", test))]
trait SetupStartup {
    type Claim;

    fn bind_authorization(&mut self) -> Result<u64, String>;
    fn revision(&mut self) -> Result<u64, String>;
    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    fn finished(&mut self) -> Result<bool, String>;
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
    fn cancel(&mut self) -> Result<(), String>;
    fn backoff(&mut self);
    fn report_failure(&mut self, why: &str);
    fn report_rejection(&mut self, why: &str) {
        self.report_failure(why);
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupStartup<F> {
    countdown: F,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    cancel_receipt: Option<aterm_gui::InitialSetupCancelReceipt>,
    authorization: Option<aterm_cli::macos_doctor::InitialSetupAuthorization>,
    cancellation: aterm_cli::macos_doctor::InitialSetupCancellation,
    last_failure: Option<String>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
}

#[cfg(target_os = "macos")]
impl<F: FnMut(aterm_types::macos_setup::Plan) -> aterm_gui::SetupAnswer> SetupStartup
    for LiveSetupStartup<F>
{
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;

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
        aterm_cli::macos_doctor::claim_automatic_setup()
    }

    fn finished(&mut self) -> Result<bool, String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::initial_setup_finished()
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
        // The ticket keeps the vault this offer is loaded in, for the run it admits.
        self.authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?
            .offer()
    }

    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision {
        let mut answer = (self.countdown)(initial);
        self.receipt = answer.receipt.take();
        self.cancel_receipt = answer.cancel_receipt.take();
        answer.decision
    }

    fn validate(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<(), String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::validate_initial_setup_plan(plan)
    }

    fn authorize(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        let authorization = self
            .authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?;
        let observed = authorization.persist_observed(plan);
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

    fn cancel(&mut self) -> Result<(), String> {
        persist_setup_cancellation(&mut self.cancellation, &mut self.observation)?;
        if let Some(receipt) = self.cancel_receipt.take() {
            receipt.complete();
        }
        Ok(())
    }

    fn backoff(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    fn report_failure(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }

    fn report_rejection(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
        aterm_log::warn!("initial setup review was not accepted: {why}");
        aterm_gui::report_workstation_setup_failure();
    }
}

/// Manual Cancel owns the local busy slot, without waiting for automatic.lock:
#[cfg(target_os = "macos")]
struct LiveSetupCancellation {
    cancellation: aterm_cli::macos_doctor::InitialSetupCancellation,
    last_failure: Option<String>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
}

#[cfg(target_os = "macos")]
impl SetupStartup for LiveSetupCancellation {
    type Claim = ();

    fn bind_authorization(&mut self) -> Result<u64, String> {
        Err("cancellation has no startup review".into())
    }

    fn revision(&mut self) -> Result<u64, String> {
        Err("cancellation has its own retained identity".into())
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        Ok(Some(()))
    }

    fn finished(&mut self) -> Result<bool, String> {
        aterm_cli::macos_doctor::initial_setup_finished()
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        Err("cancellation never resumes an admitted plan".into())
    }

    fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String> {
        Err("cancellation has no checklist to offer".into())
    }

    fn offer(&mut self, _: aterm_types::macos_setup::Plan) -> aterm_gui::WorkstationSetupDecision {
        aterm_gui::WorkstationSetupDecision::Unavailable
    }

    fn authorize(&mut self, _: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        Err("cancellation cannot authorize setup".into())
    }

    fn authorization_pending(&self) -> bool {
        false
    }

    fn cancel(&mut self) -> Result<(), String> {
        persist_setup_cancellation(&mut self.cancellation, &mut self.observation)
    }

    fn backoff(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    fn report_failure(&mut self, why: &str) {
        initial_setup_error(why.into(), self.observation.clone());
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }
}

#[cfg(any(target_os = "macos", test))]
enum PreparedSetup<C> {
    Authorized {
        claim: C,
        revision: u64,
    },
    Finished,
    Unavailable,
    Pending {
        claim: C,
        choice: SetupChoice,
        why: String,
    },
}

/// A tty decision can outlive its input surface. Move both operation identities
#[cfg(any(target_os = "macos", test))]
struct PreparedFirstRun<C, D> {
    setup: PreparedSetup<C>,
    decisions: D,
}

#[cfg(any(target_os = "macos", test))]
struct SetupDecisions<A, C> {
    authorization: A,
    cancellation: C,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
enum SetupChoice {
    Begin(aterm_types::macos_setup::Plan),
    Cancel,
}

/// Retry state I/O only on a window's background worker. A transparent terminal
#[cfg(any(target_os = "macos", test))]
fn retry_setup_step<H: SetupStartup, T>(
    host: &mut H,
    retry: bool,
    mut step: impl FnMut(&mut H) -> Result<T, String>,
) -> Result<T, String> {
    loop {
        match step(host) {
            Ok(value) => return Ok(value),
            Err(why) if retry => {
                host.report_failure(&why);
                host.backoff();
            }
            Err(why) => return Err(why),
        }
    }
}

/// Keep a decision across transient failures. Cancel is persisted without
#[cfg(any(target_os = "macos", test))]
fn prepare_setup<H: SetupStartup>(
    host: &mut H,
    retry: bool,
) -> Result<PreparedSetup<H::Claim>, String> {
    use aterm_gui::WorkstationSetupDecision;
    let revision = host.bind_authorization().inspect_err(|why| {
        if retry {
            host.report_failure(why);
        }
    })?;
    enum BeforeOffer {
        Initial(aterm_types::macos_setup::Plan),
        Resume(u64),
        Finished,
        Superseded,
    }
    loop {
        let Some(claim) = retry_setup_step(host, retry, H::claim)? else {
            if !retry {
                return Ok(PreparedSetup::Unavailable);
            }
            if retry_setup_step(host, retry, H::finished)? {
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
                if !retry {
                    return Err(why);
                }
                host.report_failure(&why);
                host.backoff();
                continue;
            }
        };
        match host.offer(initial) {
            WorkstationSetupDecision::Begin(selection) => {
                return Ok(finish_setup_choice(
                    host,
                    claim,
                    SetupChoice::Begin(selection),
                    retry,
                ));
            }
            WorkstationSetupDecision::Cancel => {
                return Ok(finish_setup_choice(host, claim, SetupChoice::Cancel, retry));
            }
            WorkstationSetupDecision::Unavailable => {
                drop(claim);
                if !retry {
                    return Ok(PreparedSetup::Unavailable);
                }
                if retry_setup_step(host, retry, H::finished)? {
                    return Ok(PreparedSetup::Finished);
                }
                host.backoff();
            }
        }
    }
}

/// Once the person has decided, retain that exact choice and its claim through
#[cfg(any(target_os = "macos", test))]
fn finish_setup_choice<H: SetupStartup>(
    host: &mut H,
    claim: H::Claim,
    choice: SetupChoice,
    retry: bool,
) -> PreparedSetup<H::Claim> {
    if let SetupChoice::Begin(plan) = &choice
        && let Err(why) = host.validate(plan)
    {
        host.report_rejection(&why);
        return PreparedSetup::Unavailable;
    }
    loop {
        let result = (|| {
            match &choice {
                SetupChoice::Begin(plan) => host.authorize(plan),
                SetupChoice::Cancel => {
                    // The idempotent backend preserves an already completed
                    // decision; explicit Cancel must still clear stale errors.
                    host.cancel()?;
                    Ok(None)
                }
            }
        })();
        match result {
            Ok(None) => {
                return match choice {
                    SetupChoice::Begin(_) => PreparedSetup::Unavailable,
                    SetupChoice::Cancel => PreparedSetup::Finished,
                };
            }
            Ok(Some(revision)) => {
                let SetupChoice::Begin(plan) = choice else {
                    unreachable!("cancellation finishes without authorizing setup")
                };
                return if plan.selection.is_empty() {
                    PreparedSetup::Finished
                } else {
                    PreparedSetup::Authorized { claim, revision }
                };
            }
            Err(why)
                if matches!(choice, SetupChoice::Begin(_)) && !host.authorization_pending() =>
            {
                host.report_rejection(&why);
                return PreparedSetup::Unavailable;
            }
            Err(why) if retry => {
                host.report_failure(&why);
                host.backoff();
            }
            Err(why) => return PreparedSetup::Pending { claim, choice, why },
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn resume_prepared_setup<H: SetupStartup, D>(
    prepared: PreparedFirstRun<H::Claim, D>,
    make_host: impl FnOnce(D) -> H,
) -> PreparedSetup<H::Claim> {
    match prepared.setup {
        PreparedSetup::Pending { claim, choice, why } => {
            let mut host = make_host(prepared.decisions);
            host.report_failure(&why);
            finish_setup_choice(&mut host, claim, choice, true)
        }
        other => other,
    }
}

/// Return the busy reservation only after Cancel has durably finished. Retrying
#[cfg(any(target_os = "macos", test))]
fn retain_setup_cancellation<H: SetupStartup<Claim = ()>, B>(host: &mut H, busy: B) -> B {
    match finish_setup_choice(host, (), SetupChoice::Cancel, true) {
        PreparedSetup::Finished => busy,
        _ => unreachable!("a retained cancellation retries until it finishes"),
    }
}

#[cfg(target_os = "macos")]
fn run_prepared_setup(
    prepared: PreparedFirstRun<
        aterm_cli::macos_doctor::AutomaticSetupClaim,
        SetupDecisions<
            Option<aterm_cli::macos_doctor::InitialSetupAuthorization>,
            aterm_cli::macos_doctor::InitialSetupCancellation,
        >,
    >,
    password: Option<aterm_secrets::SecretBytes>,
) {
    let prepared = resume_prepared_setup(prepared, |decisions| LiveSetupStartup {
        countdown: |_| aterm_gui::WorkstationSetupDecision::Unavailable.into(),
        receipt: None,
        cancel_receipt: None,
        authorization: decisions.authorization,
        cancellation: decisions.cancellation,
        last_failure: None,
        observation: None,
    });
    match prepared {
        PreparedSetup::Authorized { claim, revision } => {
            let busy = wait_for_initial_setup_slot();
            aterm_gui::publish_initial_setup_status(initial_setup_status());
            run_first_run_doctor(claim, revision, password, busy);
        }
        PreparedSetup::Finished => {
            aterm_gui::publish_initial_setup_status(initial_setup_status());
            aterm_gui::report_workstation_setup_finished();
            finish_withheld_preferences();
        }
        PreparedSetup::Unavailable => {}
        PreparedSetup::Pending { .. } => {
            unreachable!("background setup retains and retries its choice")
        }
    }
}

/// Clear what setup left alone in another app's container for want of Full Disk Access
/// (Notification Center's widgets), once this aterm holds the grant: at the launch of a
/// window whose setup has finished, silently, to the log alone. While it may hold the
/// profile lock it holds System State's busy slot too, so System State shows it busy and
/// offers no Apply, Check or Restore until it ends, instead of letting one fail on a lock
/// nobody it can see holds; a launch with nothing owed reads only aterm's record and takes
/// no slot. Without the grant it changes nothing and asks nothing; a try that did not
/// verify gets its second at a later launch once it is due.
#[cfg(target_os = "macos")]
fn finish_withheld_preferences() {
    match aterm_cli::macos_doctor::finish_withheld_preferences(wait_for_initial_setup_slot) {
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

/// The background setup worker of a window or a session. It reports to the log
#[cfg(target_os = "macos")]
fn run_first_run_doctor(
    claim: aterm_cli::macos_doctor::AutomaticSetupClaim,
    revision: u64,
    password: Option<aterm_secrets::SecretBytes>,
    busy: InitialSetupBusy,
) {
    // session must not wake each minute to learn that again.
    if let Some(why) = aterm_cli::macos_doctor::lasting_scope_refusal() {
        aterm_log::info!("first-run macOS doctor: skipped: {why}");
        drop(claim);
        drop(busy);
        return;
    }
    run_setup_retry(
        &mut LiveSetupRetry {
            revision,
            password,
            last_failure: None,
            success_reported: false,
        },
        claim,
        busy,
        revision,
    );
}

/// The scheduler owns the claim and UI reservation; the live host alone holds
#[cfg(any(target_os = "macos", test))]
trait SetupRetryHost {
    type Claim;
    type Busy;

    fn revision(&mut self) -> Result<u64, String>;
    fn finished(&mut self) -> Result<bool, String>;
    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    fn busy(&mut self) -> Self::Busy;
    fn apply(&mut self) -> Result<(), String>;
    fn backoff(&mut self);
    fn failure(&mut self, why: &str);
    fn completed(&mut self);
}

#[cfg(any(target_os = "macos", test))]
fn release_setup_pass<C, B>(claim: &mut Option<C>, busy: &mut Option<B>) {
    drop(claim.take());
    drop(busy.take());
}

#[cfg(any(target_os = "macos", test))]
fn run_setup_retry<H: SetupRetryHost>(host: &mut H, claim: H::Claim, busy: H::Busy, revision: u64) {
    let mut busy = Some(busy);
    let mut claim = Some(claim);
    loop {
        match host.finished() {
            Ok(true) => {
                host.completed();
                return;
            }
            Ok(false) => {}
            Err(why) => {
                host.failure(&why);
                release_setup_pass(&mut claim, &mut busy);
                host.backoff();
                continue;
            }
        }
        match host.revision() {
            Ok(current) if current != revision => return,
            Ok(_) => {}
            Err(why) => {
                host.failure(&why);
                release_setup_pass(&mut claim, &mut busy);
                host.backoff();
                continue;
            }
        }
        if claim.is_none() {
            match host.claim() {
                Ok(Some(acquired)) => claim = Some(acquired),
                Ok(None) => {
                    host.backoff();
                    continue;
                }
                Err(why) => {
                    host.failure(&why);
                    host.backoff();
                    continue;
                }
            }
            match host.revision() {
                Ok(current) if current != revision => return,
                Ok(_) => {}
                Err(why) => {
                    host.failure(&why);
                    release_setup_pass(&mut claim, &mut busy);
                    host.backoff();
                    continue;
                }
            }
        }
        if busy.is_none() {
            busy = Some(host.busy());
            continue;
        }
        if let Err(why) = host.apply() {
            host.failure(&why);
        }
        match host.finished() {
            Ok(true) => {
                host.completed();
                return;
            }
            Ok(false) => {}
            Err(why) => host.failure(&why),
        }
        release_setup_pass(&mut claim, &mut busy);
        host.backoff();
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupRetry {
    revision: u64,
    password: Option<aterm_secrets::SecretBytes>,
    last_failure: Option<String>,
    success_reported: bool,
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
        aterm_cli::macos_doctor::claim_automatic_setup()
    }
    fn busy(&mut self) -> Self::Busy {
        wait_for_initial_setup_slot()
    }
    fn apply(&mut self) -> Result<(), String> {
        clear_initial_setup_rows();
        let observed = aterm_cli::macos_doctor::apply_initial_setup_observed(
            self.revision,
            &mut self.password,
        );
        if let Some(report) = observed
            .result
            .inspect_err(|why| initial_setup_error(why.clone(), observed.observation.clone()))?
        {
            let text = report.to_text();
            remember_initial_setup_rows(&report, None, observed.observation);
            if report.needs_attention() {
                self.failure(&text);
            } else if !self.success_reported {
                aterm_log::info!("first-run macOS doctor: {text}");
                self.success_reported = true;
            }
        }
        Ok(())
    }
    fn backoff(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    fn failure(&mut self, why: &str) {
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }
    fn completed(&mut self) {
        aterm_gui::report_workstation_setup_finished();
    }
}

#[cfg(test)]
mod setup_retry_tests {
    use super::{
        PreparedFirstRun, PreparedSetup, SetupChoice, SetupDecisions, SetupRetryHost, SetupStartup,
        finish_setup_choice, resume_prepared_setup, retain_setup_cancellation, run_setup_retry,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn manual_cancel_retains_busy_through_write_failure_with_an_unreserved_negative_control() {
        #[derive(Default)]
        struct CancelState {
            busy: bool,
            cancelled: bool,
            writes: usize,
            retries: usize,
            automatic_applies: usize,
        }
        struct Busy(Rc<RefCell<CancelState>>);
        impl Drop for Busy {
            fn drop(&mut self) {
                self.0.borrow_mut().busy = false;
            }
        }
        struct CancelHost(Rc<RefCell<CancelState>>);
        impl SetupStartup for CancelHost {
            type Claim = ();
            fn bind_authorization(&mut self) -> Result<u64, String> {
                panic!("Cancel has no authorization to bind")
            }
            fn revision(&mut self) -> Result<u64, String> {
                panic!("Cancel owns a separate retained identity")
            }
            fn claim(&mut self) -> Result<Option<()>, String> {
                panic!("Cancel does not take automatic.lock")
            }
            fn finished(&mut self) -> Result<bool, String> {
                Ok(self.0.borrow().cancelled)
            }
            fn admission(&mut self) -> Result<Option<super::SetupAdmission>, String> {
                panic!("Cancel cannot resume setup")
            }
            fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String> {
                panic!("Cancel does not read a checklist")
            }
            fn offer(
                &mut self,
                _: aterm_types::macos_setup::Plan,
            ) -> aterm_gui::WorkstationSetupDecision {
                panic!("never repeat an accepted Cancel")
            }
            fn authorize(
                &mut self,
                _: &aterm_types::macos_setup::Plan,
            ) -> Result<Option<u64>, String> {
                panic!("Cancel cannot authorize setup")
            }
            fn authorization_pending(&self) -> bool {
                false
            }
            fn cancel(&mut self) -> Result<(), String> {
                let mut state = self.0.borrow_mut();
                state.writes += 1;
                if state.writes == 1 {
                    return Err("injected marker write failure".into());
                }
                state.cancelled = true;
                Ok(())
            }
            fn backoff(&mut self) {
                let mut state = self.0.borrow_mut();
                state.retries += 1;
                if !state.busy {
                    state.automatic_applies += 1;
                }
            }
            fn report_failure(&mut self, _: &str) {}
        }
        for reserved in [true, false] {
            let state = Rc::new(RefCell::new(CancelState {
                busy: reserved,
                ..CancelState::default()
            }));
            let busy = reserved.then(|| Busy(Rc::clone(&state)));
            let mut host = CancelHost(Rc::clone(&state));
            let busy = retain_setup_cancellation(&mut host, busy);
            {
                let observed = state.borrow();
                assert!(observed.cancelled);
                assert_eq!(observed.writes, 2);
                assert_eq!(observed.retries, 1);
                assert_eq!(
                    observed.busy, reserved,
                    "retain the reservation until final status cleanup"
                );
                assert_eq!(observed.automatic_applies, usize::from(!reserved));
            }
            drop(busy);
            assert!(!state.borrow().busy);
        }
    }

    #[test]
    fn authorization_fence_retains_the_cancel_ticket_across_a_tty_handoff() {
        #[derive(Default)]
        struct Ticket(Option<u64>);
        struct State {
            revision: u64,
            fail_write: bool,
            stopped: Vec<u64>,
        }
        struct CancelHost {
            state: Rc<RefCell<State>>,
            ticket: Ticket,
        }
        impl SetupStartup for CancelHost {
            type Claim = ();
            fn bind_authorization(&mut self) -> Result<u64, String> {
                panic!("a retained Cancel never binds an authorization")
            }
            fn revision(&mut self) -> Result<u64, String> {
                panic!("a retained Cancel never recaptures its identity")
            }
            fn claim(&mut self) -> Result<Option<()>, String> {
                panic!("a retained Cancel already owns its claim")
            }
            fn finished(&mut self) -> Result<bool, String> {
                panic!("Cancel must persist its own bound decision")
            }
            fn admission(&mut self) -> Result<Option<super::SetupAdmission>, String> {
                panic!("Cancel cannot resume setup")
            }
            fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String> {
                panic!("the tty is no longer an input surface")
            }
            fn offer(
                &mut self,
                _: aterm_types::macos_setup::Plan,
            ) -> aterm_gui::WorkstationSetupDecision {
                panic!("never offer again after tty input has ended")
            }
            fn authorize(
                &mut self,
                _: &aterm_types::macos_setup::Plan,
            ) -> Result<Option<u64>, String> {
                panic!("Cancel cannot authorize setup")
            }
            fn authorization_pending(&self) -> bool {
                false
            }
            fn cancel(&mut self) -> Result<(), String> {
                let mut state = self.state.borrow_mut();
                let revision = *self.ticket.0.get_or_insert(state.revision);
                if std::mem::take(&mut state.fail_write) {
                    return Err("injected cancellation write failure".into());
                }
                if revision == state.revision {
                    state.stopped.push(revision);
                }
                Ok(())
            }
            fn backoff(&mut self) {
                panic!("the retained ticket supersedes without another write failure")
            }
            fn report_failure(&mut self, _: &str) {}
        }
        for replace_ticket in [false, true] {
            let state = Rc::new(RefCell::new(State {
                revision: 1,
                fail_write: true,
                stopped: Vec::new(),
            }));
            let mut host = CancelHost {
                state: Rc::clone(&state),
                ticket: Ticket::default(),
            };
            let setup = finish_setup_choice(&mut host, (), SetupChoice::Cancel, false);
            assert!(matches!(setup, PreparedSetup::Pending { .. }));
            let mut prepared = PreparedFirstRun {
                setup,
                decisions: SetupDecisions {
                    authorization: (),
                    cancellation: host.ticket,
                },
            };
            state.borrow_mut().revision = 2;
            if replace_ticket {
                // would redirect the old keystroke to the replacement run.
                prepared.decisions.cancellation = Ticket::default();
            }
            let prepared = resume_prepared_setup(prepared, |decisions| CancelHost {
                state: Rc::clone(&state),
                ticket: decisions.cancellation,
            });
            assert!(matches!(prepared, PreparedSetup::Finished));
            assert_eq!(
                state.borrow().stopped,
                if replace_ticket { vec![2] } else { vec![] }
            );
        }
    }

    struct State {
        claimed: bool,
        busy: bool,
        finished: bool,
        revision: u64,
        applied: Vec<u64>,
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
        cancel_during_busy: bool,
        completions: usize,
        backoffs: usize,
        failures: usize,
    }

    fn fixture() -> (Fake, Lease, Lease) {
        let state = Rc::new(RefCell::new(State {
            claimed: true,
            busy: true,
            finished: false,
            revision: 1,
            applied: Vec::new(),
            drops: Vec::new(),
        }));
        (
            Fake {
                state: Rc::clone(&state),
                initial_error: false,
                finished_error: false,
                restart_during_backoff: false,
                restart_during_claim: false,
                cancel_during_busy: false,
                completions: 0,
                backoffs: 0,
                failures: 0,
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
            assert!(!state.claimed);
            state.claimed = true;
            if self.restart_during_claim {
                // A manual rerun won after the pre-claim revision read.
                state.revision += 1;
            }
            Ok(Some(Lease {
                kind: "claim",
                state: Rc::clone(&self.state),
            }))
        }
        fn busy(&mut self) -> Self::Busy {
            let mut state = self.state.borrow_mut();
            assert!(state.claimed && !state.busy);
            state.busy = true;
            if self.cancel_during_busy {
                state.finished = true;
            }
            Lease {
                kind: "busy",
                state: Rc::clone(&self.state),
            }
        }
        fn apply(&mut self) -> Result<(), String> {
            let mut state = self.state.borrow_mut();
            assert!(state.claimed && state.busy);
            let revision = state.revision;
            state.applied.push(revision);
            Ok(())
        }
        fn backoff(&mut self) {
            let mut state = self.state.borrow_mut();
            assert!(
                !state.claimed && !state.busy,
                "idle retries must leave the form usable"
            );
            self.backoffs += 1;
            assert_eq!(
                self.backoffs, 1,
                "a stale worker must end without another retry"
            );
            if self.restart_during_backoff {
                // Same plan, corrected password: the backend advances its revision.
                state.revision += 1;
            }
        }
        fn failure(&mut self, _: &str) {
            self.failures += 1;
        }
        fn completed(&mut self) {
            assert!(
                self.cancel_during_busy,
                "other scripted runs remain pending until replaced"
            );
            self.completions += 1;
        }
    }

    #[test]
    fn cancellation_committed_while_acquiring_busy_prevents_a_stale_retry() {
        let (mut host, claim, busy) = fixture();
        host.finished_error = true;
        host.cancel_during_busy = true;
        run_setup_retry(&mut host, claim, busy, 1);
        let state = host.state.borrow();
        assert!(
            state.applied.is_empty(),
            "recheck Cancel after acquiring busy"
        );
        assert_eq!(host.completions, 1);
        assert_eq!(host.backoffs, 1);
        assert_eq!(state.drops, ["claim", "busy", "claim", "busy"]);
        assert!(!state.claimed && !state.busy);
    }

    #[test]
    fn pending_setup_releases_claim_and_busy_before_a_same_plan_rerun_retires_it() {
        let (mut host, claim, busy) = fixture();
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, claim, busy, 1);
        let state = host.state.borrow();
        assert_eq!(state.applied, [1]);
        assert_eq!(state.drops, ["claim", "busy"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.backoffs, 1);
    }

    #[test]
    fn a_revision_changed_during_reacquisition_prevents_any_apply() {
        let (mut host, claim, busy) = fixture();
        host.finished_error = true;
        host.restart_during_claim = true;
        run_setup_retry(&mut host, claim, busy, 1);
        let state = host.state.borrow();
        assert!(state.applied.is_empty());
        assert_eq!(state.drops, ["claim", "busy", "claim"]);
        assert!(!state.claimed && !state.busy);
        assert_eq!(host.failures, 1);
    }

    #[test]
    fn a_revision_read_error_releases_resources_before_retrying_the_admitted_run() {
        let (mut host, claim, busy) = fixture();
        host.initial_error = true;
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, claim, busy, 1);
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
        run_setup_retry(&mut host, claim, busy, 1);
        assert!(
            host.state.borrow().applied.is_empty(),
            "a late baseline read must not adopt revision 2 with revision 1's password"
        );
    }
}

#[cfg(target_os = "macos")]
fn report_first_run_doctor_failure(last_failure: &mut Option<String>, text: &str) {
    if last_failure.as_deref() == Some(text) {
        return;
    }
    aterm_log::warn!("first-run macOS doctor: {text}; automatic setup retries while aterm is open");
    aterm_gui::report_workstation_setup_failure();
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
fn persist_setup_cancellation(
    cancellation: &mut aterm_cli::macos_doctor::InitialSetupCancellation,
    observation: &mut Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) -> Result<(), String> {
    let observed = cancellation.persist_observed();
    *observation = observed.observation;
    if observed.result? {
        clear_initial_setup_rows();
        aterm_log::info!("first-run macOS doctor: Cancel choice saved");
    } else {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
    Ok(())
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

#[cfg(target_os = "macos")]
fn initial_setup_status() -> aterm_gui::SetupStatus {
    let snapshot = aterm_cli::macos_doctor::initial_setup_snapshot();
    let busy = INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire);
    match INITIAL_SETUP_ROWS.lock() {
        Ok(last) => initial_setup_status_with(snapshot, busy, &last),
        Err(_) => initial_setup_status_with(snapshot, busy, &InitialSetupRows::default()),
    }
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
            Some(aterm_gui::InitialSetupPermissionPolicy {
                revision: snapshot.revision,
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
        // already published in the cache. Its own next snapshot can show it.
        None
    };
    aterm_gui::SetupStatus {
        plan,
        permissions,
        busy,
        finished,
        rows,
        check,
    }
}

#[cfg(target_os = "macos")]
fn remember_initial_setup_rows(
    report: &aterm_cli::macos_doctor::Report,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    let checked = report.action == aterm_cli::macos_doctor::Action::Check;
    let rows = report
        .rows
        .iter()
        .map(|row| aterm_gui::SetupStatusRow {
            name: if checked {
                format!("Check: {}", row.name)
            } else {
                row.name.clone()
            },
            status: row.status.clone(),
            detail: row.detail.clone(),
        })
        .collect();
    publish_initial_setup_rows(rows, checked_plan, observation);
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

#[cfg(target_os = "macos")]
fn spawn_initial_setup_operation(
    name: &str,
    run: impl FnOnce(InitialSetupBusy) + Send + 'static,
) -> bool {
    if aterm_cli::macos_doctor::lasting_scope_refusal().is_some()
        || INITIAL_SETUP_BUSY
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
    {
        return false;
    }
    if let Ok(mut last) = INITIAL_SETUP_ROWS.lock() {
        *last = InitialSetupRows::default();
    }
    match std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // File observation belongs on the worker, never the UI thread.
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
    spawn_initial_setup_operation("aterm-setup-apply", move |busy| {
        let mut host = LiveSetupRestart {
            restart: aterm_cli::macos_doctor::InitialSetupRestart::default(),
            vault_typed,
            observation: busy.observation.clone(),
        };
        run_setup_restart(&mut host, &plan, password, receipt, busy);
    })
}

#[cfg(any(target_os = "macos", all(test, unix)))]
trait SetupRestartHost {
    type Claim;
    type Busy;
    type Password;
    type Receipt;

    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
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

#[cfg(any(target_os = "macos", all(test, unix)))]
fn run_setup_restart<H: SetupRestartHost>(
    host: &mut H,
    plan: &aterm_types::macos_setup::Plan,
    password: H::Password,
    receipt: H::Receipt,
    busy: H::Busy,
) {
    let claim = match host.claim() {
        Ok(Some(claim)) => claim,
        Ok(None) => {
            host.failure("Another setup is running. Try again when it finishes.");
            return;
        }
        Err(why) => {
            host.failure(&why);
            return;
        }
    };
    loop {
        match host.persist(plan) {
            Ok(Some(revision)) => {
                host.admitted(revision, claim, busy, password, receipt);
                return;
            }
            Ok(None) => return,
            Err(why) => {
                host.failure(&why);
                if !host.pending() {
                    return;
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
}

#[cfg(target_os = "macos")]
impl SetupRestartHost for LiveSetupRestart {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;
    type Password = Option<aterm_secrets::SecretBytes>;
    type Receipt = aterm_gui::InitialSetupApplyReceipt;

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        aterm_cli::macos_doctor::claim_initial_setup_rerun()
    }

    fn persist(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        let observed = self.restart.persist_observed(plan, self.vault_typed);
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
        clear_initial_setup_rows();
        receipt.accept();
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        run_first_run_doctor(claim, revision, password, busy);
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
        backoffs: usize,
        dispatches: Vec<u64>,
        failures: Vec<String>,
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
            assert_eq!(self.claims, 1, "retain the original automatic claim");
            Ok(Some(Lease::new("claim", &self.state)))
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
            backoffs: 0,
            dispatches: Vec::new(),
            failures: Vec::new(),
        };
        run_setup_restart(&mut host, plan, password, receipt, busy);
        assert!(host.state.borrow().alive.is_empty());
        assert_eq!(host.state.borrow().drops[0], "claim");
        host
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
        assert_eq!(host.state.borrow().refused, 0);
    }
}

#[cfg(target_os = "macos")]
fn check_initial_setup(plan: aterm_types::macos_setup::Plan) -> bool {
    spawn_initial_setup_operation("aterm-setup-check", move |_busy| {
        let observed = aterm_cli::macos_doctor::check_initial_setup_observed(&plan);
        match observed.result {
            Ok(report) => remember_initial_setup_rows(&report, Some(&plan), observed.observation),
            Err(why) => publish_initial_setup_error(why, Some(&plan), observed.observation),
        }
    })
}

#[cfg(target_os = "macos")]
fn restore_initial_setup() -> bool {
    spawn_initial_setup_operation("aterm-setup-restore", |_busy| {
        let observed =
            aterm_cli::macos_doctor::run_observed(aterm_cli::macos_doctor::Action::Restore);
        match observed.result {
            Ok(report) => remember_initial_setup_rows(&report, None, observed.observation),
            Err(why) => initial_setup_error(why, observed.observation),
        }
    })
}

#[cfg(target_os = "macos")]
fn cancel_initial_setup(receipt: aterm_gui::InitialSetupCancelReceipt) -> bool {
    spawn_initial_setup_operation("aterm-setup-cancel", move |busy| {
        let mut host = LiveSetupCancellation {
            cancellation: aterm_cli::macos_doctor::InitialSetupCancellation::default(),
            last_failure: None,
            observation: busy.observation.clone(),
        };
        let busy = retain_setup_cancellation(&mut host, busy);
        receipt.complete();
        aterm_gui::report_workstation_setup_finished();
        drop(busy);
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
}

/// The windowed sibling's argv for a window the policy decided to spawn:
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
    /// `STD_INPUT_HANDLE` / `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE`.
    const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];
    /// `HANDLE_FLAG_INHERIT`.
    const HANDLE_FLAG_INHERIT: u32 = 0x1;

    /// How much older than this image a build tree's windowed image may be and still count as
    const SAME_BUILD_SPREAD: Duration = Duration::from_secs(60);

    /// Whether a build tree's windowed image, last written at `sibling`, is too old to open a window for this image, last written at `this`.
    pub(super) fn is_stale(sibling: SystemTime, this: SystemTime) -> bool {
        this.duration_since(sibling)
            .is_ok_and(|older_by| older_by > SAME_BUILD_SPREAD)
    }

    /// Whether `image` carries the windowed image's cargo target name: only a build tree has
    fn is_build_tree_image(image: &Path) -> bool {
        image
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("aterm-windowed.exe"))
    }

    /// Stop this process's std handles from being inherited by the sibling.
    pub(super) fn stop_inheriting_std_handles() {
        // SAFETY: handle queries and a flag write on handles this process owns; no pointers cross the boundary.
        unsafe {
            for kind in STD_HANDLES {
                let handle = GetStdHandle(kind);
                if handle != 0 && handle != -1 {
                    let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }

    /// Whether any of this process's std handles is still inheritable — the observation behind [`stop_inheriting_std_handles`]'s test.
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

    /// Start the windowed sibling with `args`, detached, stdio on NUL, and return once it is running.
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
#[cfg(any(windows, test))]
fn nests_inside_aterm(bare: bool, force_session: bool, aterm_child: bool, stdin_tty: bool) -> bool {
    bare && !force_session && aterm_child && stdin_tty
}

/// What the guarded launch does instead of nesting: `aterm new-tab -d <cwd>`, forced to the
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

/// The index of the `-e`/`--command`/`--` payload boundary, or `rest.len()`. Past it every
fn payload_boundary(rest: &[OsString]) -> usize {
    rest.iter()
        .position(|a| matches!(a.to_string_lossy().as_ref(), "-e" | "--command" | "--"))
        .unwrap_or(rest.len())
}

/// The two mode flags: they select which mode runs, and the mode libraries do not know them.
const MODE_FLAGS: &[&str] = &["--window", "--session"];

/// `rest` with `flags` removed, only before the payload boundary: a `--` or `-e` payload is a
fn strip_flags(rest: &[OsString], flags: &[&str]) -> Vec<OsString> {
    let boundary = payload_boundary(rest);
    rest.iter()
        .enumerate()
        .filter(|(i, a)| *i >= boundary || !flags.contains(&a.to_string_lossy().as_ref()))
        .map(|(_, a)| a.clone())
        .collect()
}

/// `rest` with both [`MODE_FLAGS`] removed — the front door's own strip, applied
fn strip_mode_flags(rest: &[OsString]) -> Vec<OsString> {
    strip_flags(rest, MODE_FLAGS)
}

/// `--no-reroute`: restore the upstream Rust names in this session (`aterm help reroute`;
const NO_REROUTE_FLAG: &str = "--no-reroute";

/// `rest` with [`NO_REROUTE_FLAG`] stripped, and the marker set to match:
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

/// The request a plain window launch would forward, or `None` when it must not be routed by
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

/// Single-instance routing for a plain window launch (Explorer, the Start menu, a pinned tile,
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

/// `aterm new-tab | new-window | split-pane [-H|-V] [-d <dir>]`, the wt-shaped front door. The
fn window_verb(verb: &str, args: &[OsString], host: WindowHost) -> ExitCode {
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
    Policy,
    /// `attach`, and no running aterm answered the probe.
    NoInstance,
    /// `attach`, one answered the probe, and the forward's dial then failed —
    Unreachable,
}

/// The line a `split-pane` that opens a new window prints, or `None` when
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

/// Decide the route for `request` and, when it is `Forward`, perform it. `Break(code)`: the
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

/// What a forwarded `intent` opens, for [`forward_error_line`].
fn intent_noun(intent: aterm_cli::LaunchIntent) -> &'static str {
    match intent {
        aterm_cli::LaunchIntent::SplitPane => "pane",
        aterm_cli::LaunchIntent::NewWindow => "window",
        aterm_cli::LaunchIntent::NewTab | aterm_cli::LaunchIntent::Plain => "tab",
    }
}

/// The line for a forward that got no reply. `delivered`: the request reached
fn forward_error_line(who: &str, delivered: bool, error: &str, noun: &str) -> String {
    if delivered {
        format!("aterm: {who} {error}; the {noun} may still open there")
    } else {
        format!("aterm: {who} is not answering; opening a new window")
    }
}

/// The line for a `spawn` that `who` answered with `ERR`. The two refusals a person can act on
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

/// The effective `windowing_behavior`: the `aterm.toml` key, else the default. An unrecognized
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

/// Resolve one `-d <dir>` operand to the absolute native path a forwarded request carries: the
fn resolve_dir_absolute(raw: &str) -> Result<String, String> {
    let path = std::path::Path::new(raw);
    let absolute = std::path::absolute(path).map_err(|e| format!("cannot resolve {raw}: {e}"))?;
    if !absolute.is_dir() {
        return Err(format!("not a directory: {raw}"));
    }
    Ok(absolute.to_string_lossy().into_owned())
}

/// The `-d <dir>` of a plain window launch, resolved to an absolute path. The scan
fn plain_launch_dir(scan: &[OsString]) -> Result<Option<String>, ()> {
    match aterm_cli::plain_launch_dir_operand(scan) {
        Some(value) => resolve_dir_absolute(&value).map(Some).map_err(|_| ()),
        None => Ok(None),
    }
}

/// `aterm update [status|check] [-v]`: the headless update lane, served in-process from the
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
                // says what it is, whatever flag follows it.
                if let Some(word) = sub.as_deref().filter(|w| !matches!(*w, "status" | "check")) {
                    eprintln!("{}", update_unknown_line(word, store_resolves));
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
            // and a Linux install waiting for its first start skips the check.
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
            eprintln!("{}", update_unknown_line(other, store_resolves));
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

/// Whether `aterm update` ends in FAILURE when there is no ledger to read.
fn no_ledger_fails(checking: bool) -> bool {
    checking || cfg!(target_os = "macos")
}

/// The verbs every platform's `aterm update` answers; the Linux-only ones are
const UPDATE_USAGE: &str = "usage: aterm update [status|check] [-v] | identity";

/// `aterm update --help` under [`UPDATE_USAGE`]: one line per word.
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

/// What `aterm update <word>` says for a word it does not take. The Linux delivery's
fn update_unknown_line(word: &str, is_package: impl FnOnce(&str) -> bool) -> String {
    if !cfg!(target_os = "linux") && matches!(word, "enable" | "apply" | "rollback") {
        let mac = if cfg!(target_os = "macos") && word == "apply" {
            " \u{2014} on a Mac a downloaded update installs from Settings \u{25b8} Software \
             Update"
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
#[derive(Clone, Debug, PartialEq, Eq)]
enum DevCopy {
    /// Not dev-marked: the updater's own ledger speaks for it.
    No,
    /// Dev-marked, so the updater leaves it alone. `lag`: its standing against the public
    Marked {
        lag: Option<aterm_update::dev_channel::DevLag>,
        shared_writes: Option<aterm_gui::DevSharedWrites>,
    },
    /// Not dev-marked, and running from where the updater never replaces a copy: the disk
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
    /// This copy, read now: the dev mark and, for a dev build, its standing, asked of the
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

/// What a dev build named `aterm.app` writes of the release's state and the release verbs that
fn dev_shared_writes_line(writes: aterm_gui::DevSharedWrites) -> String {
    let aterm_gui::DevSharedWrites { what, repair } = writes;
    format!(
        "This dev build writes {what}, which only the release writes by itself, because its \
         bundle is named aterm.app \u{2014} rebuild it with `targo --unverified ship dev-app` (it \
         installs as aterm (dev).app) and remove this copy, then run {repair} from the release."
    )
}

/// What `aterm update` says when there is no ledger to read: off macOS and Linux that is the
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

/// Where a trouble line sends the reader for the rest: the aterm.log this process
fn update_log_hint() -> String {
    let log = aterm_types::dirs::logs_dir().map(|dir| dir.join("aterm.log"));
    update_log_hint_for(log.as_deref(), aterm_types::dirs::home_dir().as_deref())
}

/// The pure half of [`update_log_hint`].
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

/// How many times the window failed to install the staged build: the apply lane's count for the
fn staged_tries(staged: u64, lane: Option<&aterm_update::ApplyLaneReport>) -> u32 {
    lane.filter(|lane| staged > 0 && lane.last_failure_target_build == staged)
        .map_or(0, |lane| lane.failures_for_target)
}

/// What `aterm update status|check` says: one plain line for stdout and, when something is
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
            Some(log_hint.to_string()),
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
        // No completed check yet: "up to date" would be a claim nothing has tested.
        format!("aterm {version} hasn\u{2019}t checked for updates yet")
    };
    if !automatic_checks {
        line.push_str(" \u{b7} automatic checks are off (Settings \u{25b8} Software Update)");
    }
    (line, None)
}

/// A failing check class (the updater's health-ledger names) in a person's words; `None` for a
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

/// Whether a replaced Linux executable still waits for its first start — a window's or
fn linux_install_waits_for_a_start(st: &aterm_update::UpdateStatus) -> bool {
    st.linux.as_ref().is_some_and(|native| {
        native.trial_phase.as_deref() == Some("Installed") && !native.trial_healthy
    })
}

/// How long before `now` the instant `at` was: `just now`, `12 min ago`, `3 h ago`,
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

/// Whether the managed store resolves `tool`: in-process `atpkg::which` against `pkg which`'s
fn store_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured().is_some_and(|layout| {
        atpkg::which(&layout, tool).is_some()
            || atpkg::cli::live_without_shim(&layout, tool).is_some()
    })
}

/// Whether the managed store holds a pending stub for `tool`, checked only after
fn pending_stub_resolves(tool: &str) -> bool {
    atpkg::store::resolve_configured()
        .is_some_and(|layout| atpkg::stub::pending_stub_exists(&layout, tool))
}

/// The mode fork's rule, pure: the window when a flag asks for it (`--window`,
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

/// The first-position verbs `--completions` offers, built from the tables the routing consults
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

/// What `aterm theme` reads from the window library, which holds the theme catalog: the
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

/// The user-typed front-door flags `--completions` offers, with the zsh/fish descriptions: the
const COMPLETION_FLAGS: &[(&str, &str)] = &[
    ("--window", "open the GPU window explicitly"),
    ("--session", "force the transparent shell session"),
    ("--headless", "engine + control socket, no window"),
    (
        "--containment",
        "containment mode (user|safety|containment)",
    ),
    ("--sandbox", "shorthand for --containment containment"),
    ("--no-sandbox", "shorthand for --containment user"),
    (
        "--no-reroute",
        "restore the upstream Rust names in this session (see aterm help reroute)",
    ),
    ("--quiet", "hide the session startup line"),
    (
        "--verbose",
        "print a one-line session summary to stderr on exit",
    ),
    ("--help", "print help and exit"),
    ("--version", "print the version and exit"),
];

/// Whether `aterm <first> <rest>` is the composed doctor ([`doctor::main_entry`]): `doctor`
fn routes_to_doctor(first: &str, rest: &[OsString]) -> bool {
    first == "doctor" && !rest.iter().any(|arg| arg == "-h" || arg == "--help")
}

#[path = "doctor.rs"]
mod doctor;

/// Compiler-provenance probe: `build.rs` `include!`s it to stamp [`BUILD_STAMP`]'s values; this
#[cfg(test)]
mod compiler_probe;

#[cfg(test)]
mod tests {

    mod setup_startup {
        use super::super::{
            PreparedFirstRun, PreparedSetup, SetupAdmission, SetupChoice, SetupDecisions,
            SetupStartup, finish_setup_choice, prepare_setup, resume_prepared_setup,
        };
        use aterm_gui::WorkstationSetupDecision::{self, Begin, Cancel, Unavailable};
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
            cancellations: VecDeque<Result<(), &'static str>>,
            offered: Vec<Plan>,
            authorized: Vec<Plan>,
            active: Rc<Cell<usize>>,
            held_during_backoff: Vec<usize>,
            failures: Vec<String>,
            bind_calls: usize,
            claim_calls: usize,
            offer_calls: usize,
            cancel_calls: usize,
            invalid_plan: bool,
            pending_authorization: bool,
            terminal_authorization_error: bool,
        }

        impl SetupStartup for Host {
            type Claim = Claim;

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

            fn cancel(&mut self) -> Result<(), String> {
                self.cancel_calls += 1;
                assert_eq!(self.active.get(), 1, "keep cancellation exclusively owned");
                self.cancellations
                    .pop_front()
                    .expect("unexpected cancellation")
                    .map_err(str::to_owned)
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
            let result = prepare_setup(&mut host, true).unwrap();
            assert!(matches!(result, PreparedSetup::Authorized { .. }));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 1);
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.failures, ["temporary state I/O"]);
            assert_eq!(host.active.get(), 1, "worker inherits the live claim");
            drop(result);
            assert_eq!(host.active.get(), 0);
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
            let result = prepare_setup(&mut host, true).unwrap();
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
                    prepare_setup(&mut host, true).unwrap(),
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
            let result = prepare_setup(&mut host, true).unwrap();
            assert_eq!(
                host.offer_calls, 0,
                "an admitted choice needs no second countdown"
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
            for retry in [false, true] {
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
                match prepare_setup(&mut host, retry) {
                    Ok(result) => {
                        assert!(retry);
                        assert!(matches!(
                            result,
                            PreparedSetup::Authorized { revision: 7, .. }
                        ));
                        assert_eq!(host.claim_calls, 2);
                        assert_eq!(host.held_during_backoff, [0]);
                        drop(result);
                    }
                    Err(why) => {
                        assert!(!retry);
                        assert_eq!(why, "admission snapshot changed");
                        assert_eq!(host.claim_calls, 1);
                        assert!(host.held_during_backoff.is_empty());
                    }
                }
                assert_eq!(host.offer_calls, 0);
                assert!(host.authorized.is_empty());
                assert_eq!(host.active.get(), 0);
            }
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
                let result = prepare_setup(&mut host, true).unwrap();
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

            fn cancel(&mut self) -> Result<(), String> {
                panic!("this fixture has an accepted Apply")
            }

            fn backoff(&mut self) {
                assert_eq!(self.active.get(), 1);
                let mut state = self.state.borrow_mut();
                assert_eq!(state.accepted, 0);
                state.backoffs += 1;
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
                let prepared = prepare_setup(&mut host, true).unwrap();
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
                prepare_setup(&mut host, true).unwrap(),
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
        fn tty_handoff_preserves_own_authorization_when_a_same_plan_empty_run_replaces_it() {
            for (replacement, completed, fresh_ticket) in [
                (false, false, false),
                (true, false, false),
                (true, false, true),
                (true, true, false),
                (true, true, true),
            ] {
                let mut host = review_host(plan(Selection::none()));
                host.fail_publication = true;
                let setup = prepare_setup(&mut host, false).unwrap();
                assert!(matches!(setup, PreparedSetup::Pending { .. }));
                let mut decisions = SetupDecisions {
                    authorization: host.authorization.take(),
                    cancellation: (),
                };
                if replacement {
                    let mut state = host.state.borrow_mut();
                    state.revision += 1;
                    state.publication += 1;
                    state.finished = completed;
                    if fresh_ticket {
                        decisions.authorization =
                            Some(AuthorizationTicket::bind(state.publication));
                    }
                }
                let state = Rc::clone(&host.state);
                let active = Rc::clone(&host.active);
                let selected = host.selected.clone();
                let prepared =
                    resume_prepared_setup(PreparedFirstRun { setup, decisions }, |decisions| {
                        ReviewHost {
                            state: Rc::clone(&state),
                            active: Rc::clone(&active),
                            authorization: decisions.authorization,
                            selected,
                            replace_during_offer: false,
                            fail_publication: false,
                            fail_confirmations: 0,
                        }
                    });
                let rewritten = fresh_ticket && !completed;
                let accepted = !replacement || rewritten;
                assert_eq!(matches!(prepared, PreparedSetup::Finished), accepted);
                assert_eq!(matches!(prepared, PreparedSetup::Unavailable), !accepted);
                let state = state.borrow();
                assert_eq!(state.binds, 1);
                assert_eq!(state.claims, 1);
                assert_eq!(state.offers, 1);
                assert_eq!(state.writes, 1 + usize::from(rewritten));
                assert_eq!(state.accepted, usize::from(accepted));
                assert_eq!(
                    state.revision,
                    1 + u64::from(replacement) + u64::from(rewritten)
                );
                assert_eq!(active.get(), 0);
            }
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
                let result = prepare_setup(&mut host, true).unwrap();
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
            for retry in [false, true] {
                let mut host = Host {
                    revisions: [Err("startup identity unreadable"), Ok(8)].into(),
                    ..Host::default()
                };
                assert_eq!(
                    prepare_setup(&mut host, retry).err().as_deref(),
                    Some("startup identity unreadable")
                );
                assert_eq!(host.revisions, [Ok(8)]);
                assert_eq!(host.claim_calls, 0);
                assert_eq!(host.offer_calls, 0);
                assert!(host.held_during_backoff.is_empty());
                assert_eq!(host.failures.len(), usize::from(retry));
            }
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
                prepare_setup(&mut host, true).unwrap(),
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
            let prepared = prepare_setup(&mut host, false).unwrap();
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
                prepare_setup(&mut host, true).unwrap(),
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
                prepare_setup(&mut host, true).unwrap(),
                PreparedSetup::Authorized { .. }
            ));
            assert_eq!(host.offer_calls, 1, "never repeat an accepted offer");
            assert_eq!(host.claim_calls, 1);
            assert_eq!(host.held_during_backoff, [1]);
            assert_eq!(host.failures, ["owned authorization write failed"]);
        }

        #[test]
        fn an_unbound_authorization_failure_releases_the_choice_without_retry() {
            for retry in [false, true] {
                let mut host = Host {
                    claims: [Ok(true)].into(),
                    finished: [Ok(false)].into(),
                    offers: [Begin(Plan::default())].into(),
                    authorizations: [Err("the failed publication could not be pinned")].into(),
                    terminal_authorization_error: true,
                    ..Host::default()
                };
                assert!(matches!(
                    prepare_setup(&mut host, retry).unwrap(),
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
        }

        #[test]
        fn cancel_retries_persistence_without_another_offer_or_releasing_its_claim() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false), Ok(false)].into(),
                offers: [Cancel].into(),
                cancellations: [Err("profile is locked"), Err("state write failed"), Ok(())].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host, true).unwrap(),
                PreparedSetup::Finished
            ));
            assert_eq!(host.offer_calls, 1);
            assert_eq!(host.claim_calls, 1);
            assert_eq!(host.cancel_calls, 3);
            assert_eq!(host.held_during_backoff, [1, 1]);
            assert_eq!(host.failures, ["profile is locked", "state write failed"]);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn explicit_cancel_retries_cleanup_even_after_another_process_finished() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(true)].into(),
                offers: [Cancel].into(),
                cancellations: [Err("profile is locked"), Ok(())].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host, true).unwrap(),
                PreparedSetup::Finished
            ));
            assert_eq!(host.offer_calls, 1);
            assert_eq!(
                host.cancel_calls, 2,
                "retry idempotent cancellation cleanup"
            );
            assert_eq!(
                host.finished,
                [Ok(true)],
                "completion cannot bypass explicit cancellation cleanup"
            );
            assert_eq!(host.held_during_backoff, [1]);
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
                prepare_setup(&mut host, true).unwrap(),
                PreparedSetup::Authorized { .. }
            ));
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, 2);
            assert_eq!(host.held_during_backoff, [0]);
            assert!(host.failures.is_empty());
        }

        #[test]
        fn durable_completion_or_cancellation_stops_later_offers() {
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
                    prepare_setup(&mut host, true).unwrap(),
                    PreparedSetup::Finished
                ));
                assert!(
                    host.offers.is_empty(),
                    "no duplicate offer after completion"
                );
                assert_eq!(host.cancel_calls, 0);
                assert_eq!(host.active.get(), 0);
            }
        }

        #[test]
        fn a_terminal_never_retries_errors_or_waits_for_another_window() {
            let mut failed_claim = Host {
                claims: [Err("temporary claim error")].into(),
                ..Host::default()
            };
            assert!(prepare_setup(&mut failed_claim, false).is_err());
            assert!(failed_claim.held_during_backoff.is_empty());
            assert_eq!(failed_claim.offer_calls, 0);
            for mut host in [
                Host {
                    claims: [Ok(false)].into(),
                    ..Host::default()
                },
                Host {
                    claims: [Ok(true)].into(),
                    finished: [Ok(false)].into(),
                    offers: [Unavailable].into(),
                    ..Host::default()
                },
            ] {
                assert!(matches!(
                    prepare_setup(&mut host, false).unwrap(),
                    PreparedSetup::Unavailable
                ));
                assert!(host.held_during_backoff.is_empty());
                assert_eq!(host.active.get(), 0);
            }
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Err("temporary marker error")].into(),
                ..Host::default()
            };
            assert!(prepare_setup(&mut host, false).is_err());
            assert!(host.held_during_backoff.is_empty());
            assert!(
                host.failures.is_empty(),
                "caller reports a terminal error once"
            );
            assert_eq!(host.active.get(), 0);
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
            let result = prepare_setup(&mut host, true).unwrap();
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
            assert_eq!(host.held_during_backoff, [0, 1]);
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
                prepare_setup(&mut host, true).unwrap(),
                PreparedSetup::Finished
            ));
            assert_eq!(host.authorized, [plan(Selection::none())]);
            assert_eq!(host.active.get(), 0);
        }

        #[test]
        fn terminal_choice_and_claim_survive_persistence_failure_without_another_tty_read() {
            let selected = plan(Selection::from_bits(2).unwrap());
            for (offer, expected) in [
                (Cancel, SetupChoice::Cancel),
                (
                    Begin(selected.clone()),
                    SetupChoice::Begin(selected.clone()),
                ),
            ] {
                let mut host = Host {
                    claims: [Ok(true)].into(),
                    finished: [Ok(false), Ok(false), Ok(false), Ok(false), Ok(false)].into(),
                    offers: [offer].into(),
                    cancellations: [Err("state unavailable"), Err("still unavailable"), Ok(())]
                        .into(),
                    authorizations: [Err("state unavailable"), Err("still unavailable"), Ok(())]
                        .into(),
                    ..Host::default()
                };
                let PreparedSetup::Pending { claim, choice, why } =
                    prepare_setup(&mut host, false).unwrap()
                else {
                    panic!("the tty must pass its retained decision to the worker");
                };
                assert_eq!(choice, expected);
                assert_eq!(why, "state unavailable");
                assert_eq!(
                    host.active.get(),
                    1,
                    "no other process can replace this decision"
                );
                assert!(
                    host.held_during_backoff.is_empty(),
                    "start the shell without waiting"
                );
                let result = finish_setup_choice(&mut host, claim, choice, true);
                assert_eq!(host.claim_calls, 1);
                assert_eq!(host.offer_calls, 1, "the worker never reads the tty again");
                assert_eq!(host.held_during_backoff, [1]);
                match expected {
                    SetupChoice::Cancel => {
                        assert!(matches!(result, PreparedSetup::Finished));
                        assert!(host.authorized.is_empty());
                        assert_eq!(host.cancel_calls, 3);
                    }
                    SetupChoice::Begin(_) => {
                        let PreparedSetup::Authorized { claim, .. } = &result else {
                            panic!("the worker must inherit the authorized run")
                        };
                        assert!(Rc::ptr_eq(&claim.0, &host.active));
                        assert_eq!(
                            host.authorized,
                            [selected.clone(), selected.clone(), selected.clone()]
                        );
                        assert_eq!(host.cancel_calls, 0);
                    }
                }
                drop(result);
                assert_eq!(host.active.get(), 0);
            }
        }
    }

    #[test]
    fn setup_windows_keep_scope_and_command_guards() {
        let plain = DoctorStartupEnv::default();
        let window = [OsString::from("--window")];
        assert!(first_run_window_eligible(&window, plain));
        assert!(
            first_run_window_eligible(&[], plain),
            "Dock and updater launches have a GUI offer surface"
        );
        for env in [
            DoctorStartupEnv {
                isolated: true,
                ..plain
            },
            DoctorStartupEnv {
                outside_person_scope: true,
                ..plain
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
            assert!(!first_run_window_eligible(&args, plain));
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

    /// A window started as `aterm --window` from a shell keeps that shell's terminal on stdin,
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
    #[cfg(windows)]
    #[test]
    fn windows_update_line_names_the_lane_that_updates_it() {
        let line = update_unreadable_line("0.94.0");
        assert!(line.starts_with("aterm 0.94.0 "), "{line}");
        assert!(line.contains(aterm_cli::WINDOWS_UPDATE_LANE), "{line}");
        assert!(!line.contains("aterm.app"), "{line}");
        assert!(!line.contains("macOS"), "{line}");
    }

    /// A synthetic asciicast, as `cast` prints one: an alt-screen frame with the cursor above a
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

    /// `aterm ctl cast drift` computed by the client (an older server, or offline): the server's
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

    /// An aterm older than `cast drift` is older than the resize undo too, so its quiet flap
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

    /// With no ledger, `check` fails everywhere (it could not do its job), and `status` only
    #[test]
    fn a_missing_ledger_fails_check_everywhere_and_status_only_on_a_mac() {
        assert!(
            no_ledger_fails(true),
            "check cannot do its job without a ledger"
        );
        assert_eq!(no_ledger_fails(false), cfg!(target_os = "macos"));
    }

    /// `aterm update status|check` claims "up to date" only over a completed
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

    /// The session lane's due rule: the machine-wide rule (never checked or six hours since the
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

    /// A record whose passes keep failing (a proxy that refuses the download host) owes nothing
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

    /// The detached pass is re-parented away from this process (which goes on to run
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

    /// Serializes the tests that read or set the process-global reroute markers
    static NO_REROUTE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The plain-launch scan finds `-d` in the window's spellings, and an unusable one is `Err`
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

    /// A child command line is never routed by policy: `spawn` cannot carry one, so a forwarded
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

    /// The `aterm-gui` row matters: the Start-Menu shortcut targets `aterm-gui.exe` and the
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

    /// `aterm-windowed`, the windowed image's cargo target name in a dev tree, routes as
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

    /// On Windows the console image hands a window to the windowed sibling, except `--headless`
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

    /// A spawned window is handed to the sibling as `new-window [-d <dir>]`, the verb the policy
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

    /// A bare launch typed inside a tab opens a tab; the `aterm-cli` alias, a launch carrying
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

    /// The mode flags are stripped for the alias as for the front door (the window's parser
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

    /// `--no-reroute` is stripped like a mode flag (never inside a payload) and establishes the
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

    /// A fresh scratch directory for one test, named by pid and nanos.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-front-door-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A resolved layout yields the absolute managed `agents/`, created by the call (private in a
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
