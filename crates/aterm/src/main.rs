// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: the one binary.

#![cfg_attr(all(unix, trust_verify), feature(register_tool))]
#![cfg_attr(all(unix, trust_verify), register_tool(trust))]

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::ExitCode;

#[cfg(target_os = "macos")]
mod compact_zone;
#[cfg(test)]
mod macho_order;

use aterm_cli::{AliasRoute, alias_route};

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();
#[cfg(all(test, unix))]
aterm_secrets::memoized_x25519_in_tests!();

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

/// `aterm machines` where it cannot run: the controller and its agent drive macOS's own tools,
/// so off macOS (where `aterm-machines` is linked for `aterm init` alone) the verb stays on the
/// roster and refuses by name.
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

/// `aterm node` where `systems.alab.aterm-node` cannot run (Unix only).
#[cfg(not(unix))]
fn node_unavailable() -> ExitCode {
    eprintln!("aterm node: not available on this platform (Unix only)");
    ExitCode::FAILURE
}

/// Background Commons runtime and steward enroller held by `aterm node serve` (C22), yielding
/// local observation and transport whenever an interactive window runs.
#[cfg(unix)]
fn start_node_commons_runtime() -> Option<aterm_db::runtime::Runtime> {
    if std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some() {
        return None;
    }
    let paths = aterm_secrets::client::Paths::live().ok()?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let lan = commons_lan::gate(&config, commons_lan::Host::NodeServe);
    match aterm_db::runtime::Runtime::start_background(paths, config, lan, |status| match status {
        aterm_db::runtime::Status::Refused => aterm_log::warn!(
            "Commons node background repair refused local configuration or trust state"
        ),
        aterm_db::runtime::Status::LanHeld => {
            aterm_log::info!("Commons node background repair: {status:?}");
        }
        _ => aterm_log::debug!("Commons node background repair: {status:?}"),
    }) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            aterm_log::warn!("Commons node background runtime could not start: {error}");
            None
        }
    }
}

#[cfg(unix)]
fn dispatch_node(forwarded: &[OsString]) -> ExitCode {
    let args: Vec<String> = forwarded
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let subcmd = args.first().map_or("status", String::as_str);
    if matches!(subcmd, "-h" | "--help" | "help") {
        let _ = aterm_uds::stdout::write_all(aterm_agent::node_service::NODE_USAGE.as_bytes());
        return ExitCode::SUCCESS;
    }
    let rest = args.get(1..).unwrap_or(&[]);
    if subcmd == "serve" {
        if rest.len() == 1 && matches!(rest[0].as_str(), "-h" | "--help" | "help") {
            let _ = aterm_uds::stdout::write_all(aterm_agent::node_service::NODE_USAGE.as_bytes());
            return ExitCode::SUCCESS;
        }
        if !rest.is_empty() {
            eprint!(
                "aterm node serve: unexpected argument {:?}\n{}",
                rest[0],
                aterm_agent::node_service::NODE_USAGE
            );
            return ExitCode::from(2);
        }
        aterm_gui::install_session_log();
        let commons = start_node_commons_runtime();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(runtime) = commons.as_ref() {
            let _ = NODE_OBSERVATION.set(runtime.network_observation());
        }
        // A Commons peer heard again after a silence is the node job's "network moved" too.
        if let Some(observation) = commons
            .as_ref()
            .map(aterm_db::runtime::Runtime::network_observation)
        {
            aterm_gui::hear_commons(move || observation.snapshot().peers);
        }
        return aterm_gui::run_node_serve();
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        eprintln!("aterm node: HOME is not set");
        return ExitCode::FAILURE;
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm node: state directory is unavailable");
        return ExitCode::FAILURE;
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("aterm"));
    let isolated =
        std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some();
    let supervisor: &dyn aterm_agent::node_service::ServiceSupervisor = if isolated {
        &aterm_agent::node_service::FileOnlySupervisor
    } else {
        &aterm_agent::node_service::LiveSupervisor
    };
    match aterm_agent::node_service::run_admin_subcommand(
        subcmd, rest, &home, &state, &exe, supervisor,
    ) {
        Ok(out) => {
            let _ = aterm_uds::stdout::write_all(out.as_bytes());
            ExitCode::SUCCESS
        }
        Err((msg, code)) => {
            eprint!("{msg}");
            ExitCode::from(code)
        }
    }
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
    // First, while this process has one thread: a window's tethered helper starts as this verb.
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(target_os = "macos")]
    aterm_uds::process::register_allocator_relief(compact_zone::release_free_memory);
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    aterm_types::version::install(BUILD_STAMP);
    // Before any vault path runs: the vault refuses every AI CLI's login file as an entry, as the
    // connector rows say where each keeps it (aterm-secrets cannot read them itself).
    #[cfg(unix)]
    aterm_secrets::text::on_cli_login(aterm_agent::ai::connector::login_file);
    // A Mac waiting to be let into ALab asks ALab's members on the LAN to show its request from
    // its vault subscriber, every minute, over the fleet port's `ask` (aterm-secrets cannot dial
    // it itself), with the ask the Secrets stage registers below.
    #[cfg(unix)]
    aterm_secrets::client::on_ask_members(|paths| {
        aterm_agent::members_cli::ask_members(&paths.home).map_err(aterm_secrets::Error::new)
    });
    // Every `aterm pkg` download races this Mac's fleet members unless `[packages] peers` says
    // otherwise, and its doctor counts them; the read runs only when one of those asks for it.
    #[cfg(unix)]
    atpkg::peers::set_fleet(unit_peers::fleet);
    // Once a look heard the Local Network switch on, no route to a LAN vault host is a host that
    // did not answer (a LAN this Mac left), never the permission.
    #[cfg(target_os = "macos")]
    aterm_secrets::transport::set_local_network_granted(|| {
        matches!(
            aterm_cli::macos_doctor::local_network_heard(),
            aterm_cli::macos_doctor::LocalNetworkHeard::On
        )
    });
    // The Secrets stage of System State and `aterm init` asks ALab's members for an Allow once
    // its join waits on the ALab vault, as `aterm members join --no-wait` does.
    #[cfg(unix)]
    aterm_machines::init_secrets::on_ask_members(aterm_agent::members_cli::ask_members);
    // The fixed collector runs before GUI initialization or any per-user state is read.
    #[cfg(target_os = "macos")]
    let early_args: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(target_os = "macos")]
    if early_args
        .first()
        .is_some_and(|a| a == aterm_sysprobe::cpu_policy::COLLECTOR_VERB)
    {
        if aterm_uds::sudo::stdout_is_anon_pipe().is_err() {
            eprintln!("CPU policy collection requires its owned anonymous output pipe");
            return ExitCode::from(2);
        }
        return ExitCode::from(aterm_sysprobe::cpu_policy::collector_entry(
            &early_args[1..],
        ));
    }
    aterm_gui::mark_rust_main_start();
    #[cfg(unix)]
    shared_login::install_automatic();
    #[cfg(unix)]
    aterm_agent::harness::account_catalog::install_reader(shared_accounts::read);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    account_claims::install(claims_observation);
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
        AliasRoute::CommandLineToolsShim => return aterm_cli::clt_shim::run(argv0, rest),
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

    if first == aterm_gui::REOPEN_VERB {
        return aterm_gui::run_reopen_after(rest.get(1..).unwrap_or(&[]));
    }

    // aCaffiene's name before 0.145.0: an old login job runs `caffeine serve` at every login.
    if first == aterm_acaffiene::legacy::VERB {
        return aterm_acaffiene::legacy::main_entry(rest.get(1..).unwrap_or(&[]));
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
            #[cfg(unix)]
            aterm_cli::Verb::Node => dispatch_node(&forwarded),
            #[cfg(not(unix))]
            aterm_cli::Verb::Node => node_unavailable(),
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
            #[cfg(unix)]
            aterm_cli::Verb::Db => aterm_db::cli::main_entry(forwarded),
            #[cfg(not(unix))]
            aterm_cli::Verb::Db => {
                eprintln!("aterm db: not available on this platform (Unix only)");
                ExitCode::from(2)
            }
            aterm_cli::Verb::Harness => {
                #[cfg(unix)]
                if let Some(result) = shared_login::try_run(&forwarded) {
                    return result;
                }
                #[cfg(not(unix))]
                if forwarded.first().is_some_and(|arg| arg == "accounts")
                    && forwarded.get(1).is_some_and(|arg| arg == "shared-renew")
                {
                    eprintln!("aterm shared-renew: not available on this platform (Unix only)");
                    return ExitCode::from(2);
                }
                aterm_agent::harness::cli::main_entry(forwarded)
            }
            aterm_cli::Verb::Ai => aterm_agent::ai::cli::main_entry(forwarded),
            #[cfg(target_os = "macos")]
            aterm_cli::Verb::Machines => aterm_machines::cli::main_entry(forwarded),
            #[cfg(not(target_os = "macos"))]
            aterm_cli::Verb::Machines => machines_unavailable(),
            #[cfg(target_os = "macos")]
            aterm_cli::Verb::Init => {
                // The run's phase totals go to aterm.log, never to this terminal.
                aterm_gui::install_session_log();
                aterm_machines::init::main_entry(
                    forwarded,
                    &aterm_machines::init::Host {
                        base: &doctor::init_base,
                        full_disk_access: &aterm_cli::macos_doctor::full_disk_access,
                        records: &aterm_cli::macos_doctor::InitRecords,
                        grants: aterm_cli::macos_doctor::init_grants,
                        walk: &aterm_cli::init_permissions_walk,
                        converge: &converge::Live,
                    },
                )
            }
            // Elsewhere (Linux): the base layer, the vault and the repositories; each
            // workstation row reads "macOS only".
            #[cfg(all(unix, not(target_os = "macos")))]
            aterm_cli::Verb::Init => {
                aterm_gui::install_session_log();
                aterm_machines::init::main_entry(
                    forwarded,
                    &aterm_machines::init::Host {
                        base: &doctor::init_base,
                        full_disk_access: &|| None,
                        records: &aterm_machines::init::NoRecords,
                        grants: aterm_machines::init::no_grants,
                        walk: &|_| None,
                        converge: &converge::Live,
                    },
                )
            }
            #[cfg(not(unix))]
            aterm_cli::Verb::Init => {
                eprintln!("aterm init: not available on this platform (Unix only)");
                ExitCode::from(2)
            }
            #[cfg(unix)]
            aterm_cli::Verb::Secrets => {
                let first = forwarded
                    .first()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned);
                if first.as_deref() == Some("need") {
                    // The need verbs read the lab's table, which aterm-db holds.
                    return aterm_db::credentials::cli::main_entry(
                        forwarded.into_iter().skip(1).collect(),
                    );
                }
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
            #[cfg(unix)]
            aterm_cli::Verb::Members => aterm_agent::members_cli::main_entry(forwarded),
            #[cfg(not(unix))]
            aterm_cli::Verb::Members => {
                eprintln!("aterm members: not available on this platform (Unix only)");
                ExitCode::from(2)
            }
            aterm_cli::Verb::Keeper => {
                aterm_keeper::cli::main_entry(forwarded, aterm_types::version::app_version())
            }
            aterm_cli::Verb::Acaffiene => {
                aterm_gui::install_session_log();
                aterm_acaffiene::cli::main_entry(forwarded, aterm_types::version::app_version())
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
            atpkg::store::prepend_bin_to_path(std::env::var_os("PATH").as_deref(), &bin),
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
        spawn_commons_runtime(commons_lan::Host::SessionLane);
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
    // The day's host settings are this Mac's: a remote shell (ssh) changes none of them and
    // leaves the day's slot to a session at its console.
    if cfg!(target_os = "macos")
        && session_lane_is_interactive()
        && !session_away_from_console()
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
    let now = std::time::Instant::now();
    let until = now
        .checked_add(silent)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(86_400 * 365));
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

/// Whether this terminal session runs outside the console owner's own graphic session: an ssh
/// login, a session without the window server, or another login at this Mac's screen
/// (`aterm_machines::console::remote`).
#[cfg(target_os = "macos")]
fn session_away_from_console() -> bool {
    aterm_machines::console::remote_here()
}

/// Off macOS no host settings are applied at launch.
#[cfg(not(target_os = "macos"))]
fn session_away_from_console() -> bool {
    true
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let ordinary = ordinary_window(&args);
    // A command line the window's parser answers (`--list-fonts`) or refuses (a word no verb or
    // other front-door route took, which a launch without a terminal brings here) ends in this
    // check, before the Commons runtime and, on macOS, aCaffiene's autostart, which keeps the
    // Mac awake for good.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if ordinary {
        aterm_gui::check_window_args(args.clone());
    }
    let first_run = wire_window(&args);
    // System State's Local Network step asks macOS first, in a window that runs first-run setup
    // only: there its Commons runtime, its Secrets stage, Tab Control's sweep and the network
    // signal's Bonjour browse wait for it; a window that runs none takes no step, so its Commons
    // gate asks by itself and nothing else waits. One decision feeds the gate and the reader, and
    // the reader is in place before the GUI starts any of the others.
    #[cfg(target_os = "macos")]
    let runs_step = first_lan_uses_wait_for_the_step(ordinary, first_run);
    #[cfg(target_os = "linux")]
    let runs_step = false;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if ordinary {
        spawn_commons_runtime(commons_lan::Host::Window {
            runs_setup: runs_step,
        });
    }
    #[cfg(target_os = "macos")]
    if ordinary {
        spawn_acaffiene_autostart(acaffiene_launch(std::env::var_os));
    }
    #[cfg(target_os = "macos")]
    if runs_step && let Some(config) = aterm_types::dirs::aterm_config_path() {
        commons_lan::hold_first_uses_for_the_step(&config);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let network_source = ordinary.then(network_state_source).flatten();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let network_source = None;
    #[expect(
        clippy::disallowed_methods,
        reason = "the one window door: every window route reaches the GUI here"
    )]
    aterm_gui::main_entry_with_network(args, first_run, network_source);
    ExitCode::SUCCESS
}

/// Whether no word of `args`, the command after `-e` included, is `--headless`, `--diagnose` or
/// a `--control-sock` with a socket of its own: with no value, or one other than `0` and `off`
/// (no socket, as `--no-control-sock`) and the empty one (the default socket)
/// ([`aterm_types::control_socket::socket_directive`]). A launch with one starts no Commons
/// runtime, aCaffiene or network source. [`run_window`] lets the window's parser end a one-shot
/// or refused one of the rest first.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ordinary_window(args: &[OsString]) -> bool {
    use aterm_types::control_socket::{SocketDirective, socket_directive};
    let mut words = args.iter().map(|a| a.to_string_lossy());
    while let Some(word) = words.next() {
        let private = match word.as_ref() {
            "--headless" | "--diagnose" => true,
            "--control-sock" => words.next().is_none_or(|value| {
                matches!(
                    socket_directive(Some(value.as_ref()), None),
                    SocketDirective::Explicit(_)
                )
            }),
            _ => false,
        };
        if private {
            return false;
        }
    }
    true
}

/// How this window came to start, as aCaffiene's autostart takes it: a running window's
/// successor when `var` (the environment's reader) finds
/// [`aterm_update::RELAUNCH_OF_RUNNING_ENV`], which an update and a launch-record renewal set,
/// else a launch.
#[cfg(target_os = "macos")]
fn acaffiene_launch(
    var: impl Fn(&'static str) -> Option<OsString>,
) -> aterm_acaffiene::cli::Launch {
    if var(aterm_update::RELAUNCH_OF_RUNNING_ENV).is_some() {
        aterm_acaffiene::cli::Launch::Successor
    } else {
        aterm_acaffiene::cli::Launch::Fresh
    }
}

/// Whether a window's first LAN uses (its Commons runtime's, [`commons_lan::Host::Window`]'s
/// `runs_setup`; its Secrets stage's vault probe and join, Tab Control's Bonjour sweep and the
/// network signal's Bonjour browse) wait for System State's Local Network step: only in an
/// `ordinary` window that runs first-run setup (`first_run`, [`wire_window`]'s), whose System
/// State runs the step. A window that runs none (started over ssh or away from the console, under
/// a private state root, outside the person's scope, or running a command) takes no step unless a
/// person presses Set up now, so none of its uses waits on one: its Commons gate asks by itself.
#[cfg(target_os = "macos")]
const fn first_lan_uses_wait_for_the_step(ordinary: bool, first_run: Option<fn() -> bool>) -> bool {
    ordinary && first_run.is_some()
}

/// How long a thread [`run_window`] starts ahead of the window's logger waits for that logger
/// ([`aterm_log::wait_for_logger`]) before it goes on without one. A process settles its
/// logging, with a logger or without one ([`aterm_log::settle_logger`]), which ends the wait:
/// a window early in its start, and a terminal session in
/// [`aterm_gui::install_session_log`] before it starts the Commons runtime, so this bounds only
/// a window that has not got that far.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const EARLY_LOG_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Log `line` as a warning: at once when a logger takes it (a terminal session's), and otherwise
/// from a thread of its own once the window's logger does ([`EARLY_LOG_WAIT`]), since
/// [`run_window`]'s own thread has it before [`aterm_gui::main_entry_with_network`] installs
/// the logger, which drops what comes first. To stderr when that thread cannot start.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn warn_when_logging(line: String) {
    if aterm_log::wait_for_logger(std::time::Duration::ZERO) {
        aterm_log::warn!("{line}");
        return;
    }
    let shown = line.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("aterm-early-warning".into())
        .spawn(move || {
            let _ = aterm_log::wait_for_logger(EARLY_LOG_WAIT);
            aterm_log::warn!("{line}");
        })
    {
        eprintln!("aterm: {shown} (no thread to log it: {error})");
    }
}

/// Start aCaffiene beside an ordinary window once the window has set up its logging
/// ([`EARLY_LOG_WAIT`]): set to keep the Mac awake for good on a launch, and as saved beside a
/// running window's successor ([`aterm_acaffiene::cli::Launch`]). Its login-item choice is left
/// alone.
#[cfg(target_os = "macos")]
fn spawn_acaffiene_autostart(launch: aterm_acaffiene::cli::Launch) {
    if let Err(error) = std::thread::Builder::new()
        .name("aterm-acaffiene-autostart".into())
        .spawn(move || {
            // The window installs its logger in main_entry_with_network, after this thread
            // starts; a line the autostart logs before that would be dropped.
            let _ = aterm_log::wait_for_logger(EARLY_LOG_WAIT);
            aterm_acaffiene::cli::autostart(launch);
        })
    {
        warn_when_logging(format!("acaffiene: could not start with aterm: {error}"));
    }
}

/// Only long-lived ordinary hosts contend for Commons transport ownership.
/// One-shot verbs return before these call sites; private labs are inert.
#[cfg(any(target_os = "macos", target_os = "linux"))]
static COMMONS_RUNTIME: std::sync::OnceLock<aterm_db::runtime::Runtime> =
    std::sync::OnceLock::new();

/// What the node job's background Commons runtime observes (`aterm node serve`, which keeps its
/// runtime to itself and serves the census): the presence its account claims lapse by.
#[cfg(any(target_os = "macos", target_os = "linux"))]
static NODE_OBSERVATION: std::sync::OnceLock<aterm_db::network_state::ObservationHandle> =
    std::sync::OnceLock::new();

/// What this process's Commons runtime last observed of the network, where one runs here (a
/// window's or session lane's, else the node job's), else what the local owner publishes over its
/// relay (a one-shot verb such as `aterm fleet census --refresh`): the presence account claims
/// lapse by. While a window owns Commons, the node job's runtime publishes that window's relayed
/// observation, or a waiting one that claims read no presence from.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn claims_observation() -> Option<aterm_types::network_state::NetworkObservation> {
    if let Some(runtime) = COMMONS_RUNTIME.get() {
        return runtime
            .network_observation()
            .try_publication()
            .map(account_claims::current_observation);
    }
    if let Some(node) = NODE_OBSERVATION.get() {
        return node
            .try_publication()
            .map(account_claims::current_observation);
    }
    if std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some() {
        return None;
    }
    aterm_db::network_state::read_live_observation(&aterm_types::dirs::aterm_config_path()?).ok()
}

/// Start this process's Commons runtime once, for `host` (its LAN gate, [`commons_lan::gate`]).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn spawn_commons_runtime(host: commons_lan::Host) {
    if std::env::var_os("ATERM_STATE_HOME").is_some()
        || std::env::var_os("CI").is_some()
        || COMMONS_RUNTIME.get().is_some()
    {
        return;
    }
    let Ok(paths) = aterm_secrets::client::Paths::live() else {
        return;
    };
    let Some(config) = aterm_types::dirs::aterm_config_path() else {
        return;
    };
    let lan = commons_lan::gate(&config, host);
    match aterm_db::runtime::Runtime::start(paths, config, lan, |status| {
        // In a window, the owner thread can report its first status before the window installs
        // its logger, and it reports a status only when it changes, so a dropped one stays
        // unsaid. A terminal session has settled its logging before this runtime starts.
        let _ = aterm_log::wait_for_logger(EARLY_LOG_WAIT);
        match status {
            aterm_db::runtime::Status::Refused => aterm_log::warn!(
                "Commons automatic control repair refused local configuration or trust state"
            ),
            aterm_db::runtime::Status::LanHeld => {
                aterm_log::info!("Commons control repair: {status:?}");
            }
            _ => aterm_log::debug!("Commons control repair: {status:?}"),
        }
    }) {
        Ok(runtime) => {
            let _ = COMMONS_RUNTIME.set(runtime);
        }
        Err(error) => warn_when_logging(format!("Commons runtime could not start: {error}")),
    }
}

/// Join the independent producers, loading fallback history once before the GUI starts.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn network_state_source() -> Option<aterm_gui::NetworkSnapshotSource> {
    use std::sync::OnceLock;

    if std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some() {
        return None;
    }

    static SERVICES: OnceLock<Option<aterm_db::service_state::Runtime>> = OnceLock::new();
    let network = COMMONS_RUNTIME
        .get()
        .map(aterm_db::runtime::Runtime::network_observation);
    let services = SERVICES
        .get_or_init(|| {
            let paths = aterm_secrets::client::Paths::live().ok()?;
            let config = aterm_types::dirs::aterm_config_path()?;
            aterm_db::service_state::Runtime::start(paths, config).ok()
        })
        .as_ref()
        .map(aterm_db::service_state::Runtime::observation);
    let retained = if network.is_none() {
        aterm_types::dirs::aterm_config_path()
            .and_then(|config| aterm_db::network_state::read_history(&config).ok())
    } else {
        None
    };
    Some(network_state_source_from(network, services, retained))
}

/// Compose independent observers, including when only one could start.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn network_state_source_from(
    network: Option<aterm_db::network_state::ObservationHandle>,
    services: Option<aterm_db::service_state::ObservationHandle>,
    retained: Option<aterm_types::network_state::NetworkObservation>,
) -> aterm_gui::NetworkSnapshotSource {
    network_state_source_with_readers(
        move || {
            network
                .as_ref()
                .and_then(|network| network.try_publication())
        },
        move || {
            services
                .as_ref()
                .and_then(|services| services.try_publication())
        },
        retained,
        || (std::time::Instant::now(), aterm_time::unix_millis()),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn network_state_source_with_readers(
    network: impl Fn() -> Option<aterm_db::network_state::Publication> + Send + Sync + 'static,
    services: impl Fn() -> Option<aterm_db::service_state::Publication> + Send + Sync + 'static,
    retained: Option<aterm_types::network_state::NetworkObservation>,
    clock: impl Fn() -> (std::time::Instant, Option<u64>) + Send + Sync + 'static,
) -> aterm_gui::NetworkSnapshotSource {
    use aterm_types::network_state::{
        HistoryState, NetworkObservation, NetworkStateSnapshot, Reachability, ServiceState,
        ServiceStates, Visibility,
    };
    use std::sync::{Arc, Mutex};

    let mut retained = retained.unwrap_or_else(|| NetworkObservation {
        history: HistoryState::Unavailable,
        detail: "Retained network history is unavailable".into(),
        ..NetworkObservation::default()
    });
    retained.visibility = Visibility::Unavailable;
    // A saved observer incarnation is not an attestation of this process.
    retained.observer = None;
    retained.unverified_endpoints = 0;
    if retained.history == HistoryState::Restoring {
        retained.history = HistoryState::Unavailable;
    }
    for peer in &mut retained.peers {
        peer.reachability = Reachability::Unknown;
        peer.last_checked_ms = None;
    }
    retained.detail = format!(
        "Network observation is unavailable; current reachability is unknown. {}",
        retained.detail
    );
    let unavailable_services = ServiceStates {
        accounts: ServiceState {
            detail: "Service observation is unavailable".into(),
            ..ServiceState::default()
        },
        secrets: ServiceState {
            detail: "Service observation is unavailable".into(),
            ..ServiceState::default()
        },
    };
    let fallback = Arc::new(NetworkStateSnapshot {
        network: retained.clone(),
        services: unavailable_services.clone(),
        ..NetworkStateSnapshot::default()
    });
    let current = Mutex::new((
        fallback.clone(),
        None::<aterm_db::network_state::Publication>,
        None::<aterm_db::service_state::Publication>,
    ));
    Arc::new(move || {
        // This callback runs on the UI thread. A worker paused while publishing
        // must not pause drawing; retained timestamps still expire in the GUI.
        let mut current = match current.try_lock() {
            Ok(current) => current,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return fallback.clone(),
        };
        if let Some(publication) = network() {
            current.1 = Some(publication);
        }
        if let Some(publication) = services() {
            current.2 = Some(publication);
        }
        let (now, wall) = clock();
        let network = current.1.as_ref().map_or_else(
            || retained.clone(),
            |publication| publication.current_at(now, wall),
        );
        let services = current.2.as_ref().map_or_else(
            || unavailable_services.clone(),
            |publication| publication.current_at(now, wall),
        );
        if current.0.network != network || current.0.services != services {
            current.0 = Arc::new(NetworkStateSnapshot {
                revision: current.0.revision.saturating_add(1),
                network,
                services,
            });
        }
        current.0.clone()
    })
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
        // An update successor resumes an admitted run but never shows a review: the person
        // may be away while the update relaunched this window ([`Review::Withheld`]).
        let start: fn() -> bool = if restoring_window {
            apply_successor_first_run
        } else {
            apply_first_run_doctor
        };
        first_run_window_eligible(setup_args, doctor_startup_env()).then_some(start)
    };
    #[cfg(not(target_os = "macos"))]
    let first_run = {
        let _ = args;
        None
    };
    #[cfg(target_os = "macos")]
    watch_setup_signals();
    #[cfg(target_os = "macos")]
    aterm_gui::set_secrets_sync_gate(vault_sync_kept, while_vault_sync_kept);
    #[cfg(target_os = "macos")]
    aterm_gui::set_launch_repairs(doctor::launch_repairs);
    #[cfg(target_os = "macos")]
    aterm_gui::set_initial_setup_handlers(aterm_gui::InitialSetupHandlers {
        apply: restart_initial_setup,
        check: check_initial_setup,
        restore: restore_initial_setup,
        status: initial_setup_status,
        observe: observe_initial_setup,
        look_for_token: look_for_github_token,
        stop: || {
            stop_initial_setup();
        },
        settle: settle_initial_setup,
        sync_on: turn_sync_on,
        fda_granted: verified_full_disk_access_granted,
    });
    aterm_gui::register_resource_runner();
    first_run
}

/// What keeps a launch from first-run setup, which belongs to a window started by the person at
/// this Mac: a private state root (a laboratory's) or CI, a scope outside the person's, or a
/// remote session.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default)]
struct DoctorStartupEnv {
    isolated: bool,
    outside_person_scope: bool,
    /// Started outside the console owner's own graphic session: from an ssh login, in a session
    /// without the window server, or with another login (or nobody) at this Mac's screen
    /// (`aterm_machines::console::remote`). The person is not at this Mac, so setup waits for a
    /// window started at it.
    remote: bool,
}

#[cfg(target_os = "macos")]
fn doctor_startup_env() -> DoctorStartupEnv {
    doctor_startup_env_from(
        |key| std::env::var_os(key),
        aterm_cli::macos_doctor::lasting_scope_refusal(),
        aterm_machines::console::remote_here,
    )
}

/// [`doctor_startup_env`] from the environment variable reader `var`, the process's lasting
/// scope refusal (`aterm_cli::macos_doctor::lasting_scope_refusal`) and its console session
/// reading (`aterm_machines::console::remote_here`, asked only when no ssh marker answers),
/// which a test feeds from scratch account paths and a fixed answer.
#[cfg(target_os = "macos")]
fn doctor_startup_env_from(
    var: impl Fn(&str) -> Option<OsString>,
    lasting_scope_refusal: Option<String>,
    away_from_console: impl FnOnce() -> bool,
) -> DoctorStartupEnv {
    DoctorStartupEnv {
        isolated: var("ATERM_STATE_HOME").is_some() || var("CI").is_some(),
        outside_person_scope: lasting_scope_refusal.is_some(),
        remote: var("SSH_CONNECTION").is_some() || var("SSH_TTY").is_some() || away_from_console(),
    }
}

/// A new GUI window has its own setup surface even when it inherits markers from the shell that
/// opened it, or from its update predecessor.
#[cfg(any(target_os = "macos", test))]
fn first_run_window_eligible(args: &[OsString], env: DoctorStartupEnv) -> bool {
    if env.isolated || env.outside_person_scope || env.remote {
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
    start_first_run(Review::Offered)
}

/// An update successor's first-run setup ([`Review::Withheld`]).
#[cfg(target_os = "macos")]
fn apply_successor_first_run() -> bool {
    start_first_run(Review::Withheld)
}

#[cfg(target_os = "macos")]
fn start_first_run(review: Review) -> bool {
    match std::thread::Builder::new()
        .name("aterm-workstation-setup".into())
        .spawn(move || {
            // A plan aterm chose by itself on an earlier launch widens only through a review of
            // its own, which the watch offers as soon as the startup is done ([`AlabWatch`]):
            // nothing is widened unreviewed (System State design §3.5).
            run_startup_setup_with(None, review);
        }) {
        Ok(_) => true,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor worker could not start: {why}");
            false
        }
    }
}

/// A plan a watch widened ([`widen_on_alab_signal`]), as the run it admitted registers
/// ([`register_admitted_run`]).
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Widened {
    /// The admission number ([`SETUP_SENT`]) the look took before it read the Stop hold: a Stop
    /// pressed since covers the widened run.
    sent: u64,
    /// The revision it widened: a Stop asked for it holds the widened run too.
    from: u64,
    /// The exact revision this look admitted.
    to: u64,
    /// The look's shutdown ownership, retained until its exact run registers or is retired.
    under_way: Option<UnderWay>,
}

/// Offer or resume setup as a launch does, then run it. A plan aterm chose by itself before it
/// heard the ALab vault is watched meanwhile ([`maintain_setup_watch`]). `widened`: the plan a
/// watch widened, so a Stop pressed since its look, or asked for the plan it widened, stops the
/// run it admitted.
#[cfg(target_os = "macos")]
fn run_startup_setup(widened: Option<Widened>) {
    run_startup_setup_with(widened, Review::Offered);
}

/// Whether a startup may show its plan on System State for review ([`SetupStartup::may_offer`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Review {
    /// A launch the person started, or a startup a person's press began: the plan is offered.
    Offered,
    /// An update successor's: nobody may be at this Mac while the update relaunched the window,
    /// so a setup never reviewed waits for the person, who opens System State from a note
    /// (`aterm_gui::note_setup_unfinished`); a plan already admitted still resumes.
    Withheld,
}

/// [`run_startup_setup`] for a startup whose review is `review`.
#[cfg(target_os = "macos")]
fn run_startup_setup_with(mut widened: Option<Widened>, review: Review) {
    let running = StartupRunning::enter();
    run_repaired_startups(|reserved| {
        let mut password = None;
        let (prepared, mut sent, recovered) =
            prepare_first_run_doctor(reserved, review, |initial, look, on_begin| {
                let mut answer = aterm_gui::await_initial_setup_answer_while(
                    initial,
                    || {
                        startup_offer_withdrawn(|| {
                            aterm_cli::macos_doctor::initial_setup_finished().unwrap_or(false)
                        })
                    },
                    look,
                    on_begin,
                );
                password = answer.password.take();
                answer
            });
        if let Some(widened) = widened.take() {
            sent.inherit_widened(&prepared, widened);
        }
        let recovery = recovered.then(|| sent.take_recovery());
        run_prepared_startup_with(prepared, password, sent, maintain_setup_watch);
        recovery
    });
    drop(running);
    read_again_if_unread();
}

/// A cancelled or closed review ends its attempt; other outcomes retain their watch maintenance.
/// A Cancel, recorded for good, still starts the converge pass's window lane: its `sync.peers`
/// reads the Cancel and joins nothing, and the rest is the upkeep a vault's subscriber runs
/// anyway. A review closed unanswered is "not this launch": nothing of setup's runs, the lane
/// (which would join the vault the review offers) included, and the next launch offers again.
#[cfg(target_os = "macos")]
fn run_prepared_startup_with(
    prepared: PreparedSetup<StartupClaim>,
    password: Option<aterm_secrets::SecretBytes>,
    sent: StartupSent,
    maintain: impl FnMut(WatchMaintenance),
) {
    if matches!(prepared, PreparedSetup::Cancelled) {
        (setup_io().converge_lane)();
    }
    if matches!(prepared, PreparedSetup::Cancelled | PreparedSetup::Closed) {
        return;
    }
    with_setup_watch(
        false,
        || run_prepared_setup(prepared, password, sent),
        maintain,
    );
}

/// A repair ends its bound review, releasing all its leases before another
/// review starts on this same worker. Unchanged failures retain their own
/// backoff, and a Stop covering the retired review withholds the fresh offer.
#[cfg(target_os = "macos")]
fn run_repaired_startups(mut attempt: impl FnMut(Option<StartupSent>) -> Option<StartupSent>) {
    let mut reserved = None;
    while let Some(sent) = attempt(reserved.take()) {
        let Some(next) = StartupSent::continue_recovery(sent) else {
            return;
        };
        reserved = Some(next);
    }
}

/// A startup ended (its capture failed, say) on a saved setup it cannot read, which the page
/// says aterm reads again by itself ([`aterm_gui::STATE_UNREAD_NOTE`]): after the retry wait, or
/// a setup wake, the page reads it again, which starts a fresh attempt while it stays unreadable
/// ([`heal_unread_setup`]).
#[cfg(target_os = "macos")]
fn read_again_if_unread() {
    let unread = || {
        matches!(
            aterm_cli::macos_doctor::initial_setup_snapshot(),
            Err(aterm_cli::macos_doctor::InitialSetupSnapshotError::Read(_))
        )
    };
    if unread() {
        let mut seen = SETUP_WAKES.count();
        SETUP_WAKES.back_off(&mut seen, SETUP_RETRY_WAIT);
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum WatchMaintenance {
    Spawn,
    Inline,
}

/// The common worker's reservation boundaries. Inline maintenance belongs after the
/// closure has returned, once all the run's leases and registration have been released.
#[cfg(target_os = "macos")]
fn with_setup_watch<T>(
    entering_worker: bool,
    run: impl FnOnce() -> T,
    mut maintain: impl FnMut(WatchMaintenance),
) -> T {
    if entering_worker {
        maintain(WatchMaintenance::Spawn);
    }
    let result = run();
    maintain(WatchMaintenance::Inline);
    result
}

#[cfg(target_os = "macos")]
fn read_setup_park_mark_with<M>(read: impl FnOnce() -> M, maintain: impl FnOnce()) -> M {
    maintain();
    read()
}

/// How soon a watch for the ALab vault looks again by itself after its first look that heard
/// none, doubling with each look to [`ALAB_LOOK_LONGEST`]. A wake looks sooner.
#[cfg(any(target_os = "macos", test))]
const ALAB_LOOK_FIRST: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest a watch for the ALab vault waits between two looks without a wake.
#[cfg(any(target_os = "macos", test))]
const ALAB_LOOK_LONGEST: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The least time between two looks for the ALab vault, however many wakes come in between.
#[cfg(any(target_os = "macos", test))]
const ALAB_LOOK_GAP: std::time::Duration = std::time::Duration::from_secs(10);

/// The longest a watch waits after `looks` looks that heard no ALab vault.
#[cfg(any(target_os = "macos", test))]
fn alab_look_after(looks: u32) -> std::time::Duration {
    ALAB_LOOK_FIRST
        .saturating_mul(1 << looks.min(16))
        .min(ALAB_LOOK_LONGEST)
}

/// How long after a watch for the ALab vault first heard Local Network's current answer a
/// window's focus gain stays a reason to look while that answer is neither on nor off
/// ([`AlabFocus`]): macOS tells no app that the person answered its question or turned the
/// switch on in System Settings, and a person who did comes back to aterm within moments.
#[cfg(any(target_os = "macos", test))]
const ALAB_FOCUS_FRESH: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What aterm last heard of macOS's Local Network switch for it, as a watch for the ALab vault
/// weighs a focus gain by it (`aterm_cli::macos_doctor::LocalNetworkHeard`).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LanHeard {
    /// Nothing says.
    Untold,
    /// The window raised macOS's question, and no look since heard how it went.
    Asked,
    /// A look reached the LAN.
    On,
    /// macOS refused a look the LAN.
    Off,
}

/// What aterm last heard of Local Network for it, from its own records.
#[cfg(target_os = "macos")]
fn lan_heard() -> LanHeard {
    use aterm_cli::macos_doctor::LocalNetworkHeard;
    match aterm_cli::macos_doctor::local_network_heard() {
        LocalNetworkHeard::Untold => LanHeard::Untold,
        LocalNetworkHeard::Asked => LanHeard::Asked,
        LocalNetworkHeard::On => LanHeard::On,
        LocalNetworkHeard::Off => LanHeard::Off,
    }
}

/// Whether a window's focus gain is a reason for a watch for the ALab vault to look again
/// ([`WakeMarks::ends_alab_wait`]): never once a look reached the LAN, since a focus gain changes
/// nothing a look reads; always while macOS refuses aterm the LAN, since only the person's switch
/// in System Settings moves that and a refused look asks nothing of the network; and otherwise
/// (macOS's question asked, or nothing heard) for [`ALAB_FOCUS_FRESH`] after the watch first
/// heard that answer. A Mac with no ALab vault on its network never hears the switch on (nothing
/// answers its looks), and a look at every focus gain asked its network every ten seconds for as
/// long as aterm ran.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug)]
struct AlabFocus {
    /// The answer last weighed, and when the watch first heard it.
    heard: Option<(LanHeard, std::time::Instant)>,
}

#[cfg(any(target_os = "macos", test))]
impl AlabFocus {
    const fn new() -> Self {
        Self { heard: None }
    }

    /// Whether a focus gain is a reason to look at `now`, aterm having heard `heard` last.
    fn counts(&mut self, heard: LanHeard, now: std::time::Instant) -> bool {
        let since = match self.heard {
            Some((last, since)) if last == heard => since,
            _ => {
                self.heard = Some((heard, now));
                now
            }
        };
        match heard {
            LanHeard::On => false,
            LanHeard::Off => true,
            LanHeard::Untold | LanHeard::Asked => {
                now.saturating_duration_since(since) < ALAB_FOCUS_FRESH
            }
        }
    }
}

/// The network a watch for the ALab vault waits on: the IPv4 subnets a look's record is kept
/// for (`aterm_secrets::heard::network`), read as the wait starts.
#[cfg(target_os = "macos")]
static ALAB_WAIT_NETWORK: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);

/// Whether this Mac's network as it is `now` moved off the one a watch for the ALab vault waits
/// on (`waited_on`, unset before any wait): only then is a change the network watch heard a
/// reason to look. An interface that comes and goes without moving the subnets (AirDrop's and
/// Continuity's peer-to-peer links, a tunnel with no IPv4 address) is no new network, and the
/// network watch would otherwise end every wait within a minute of the last on a Mac that has
/// them. A move between two networks with the same subnets is the same network to a look's
/// record too; System State coming to the front, Check again and the timer look there.
#[cfg(any(target_os = "macos", test))]
fn alab_network_moved(
    waited_on: &std::sync::Mutex<Option<Option<String>>>,
    now: Option<String>,
) -> bool {
    waited_on
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_none_or(|waited_on| *waited_on != now)
}

/// What [`widen_on_alab_signal`] does on this Mac, or in a test.
#[cfg(any(target_os = "macos", test))]
trait AlabWatch {
    /// The saved plan is aterm's own provisional one, which the ALab signal widens, or an
    /// upgrade of it this watch holds; an error when aterm's records cannot be read just now.
    fn widens(&mut self) -> Result<bool, String>;
    /// Wait out `gap`, then until a wake since the last look, or until `limit` has passed.
    fn wait(&mut self, gap: std::time::Duration, limit: std::time::Duration);
    /// Look for the ALab signal and, once it is heard, offer the groups it adds as a review of
    /// their own (design §3.5): the person's answer. An upgrade this watch holds was answered.
    fn review(&mut self) -> Result<WidenReview, String>;
    /// Widen the plan as its review admitted it: the widened plan's revision.
    fn look(&mut self) -> Result<Option<u64>, String>;
    /// Run the widened plan as a launch runs it.
    fn run(&mut self);
}

/// The answer to a review of the groups this Mac's ALab signal adds to aterm's provisional plan
/// ([`AlabWatch::review`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidenReview {
    /// No signal heard yet, or no window could show the review: the watch looks again.
    NotYet,
    /// The review's plan was applied as it showed it: the watch widens the plan and runs it.
    Applied,
    /// The person answered it otherwise (a group choice, which runs as their choice does; a
    /// Cancel, which keeps the plan in force as theirs; or closing it, not this launch): the
    /// watch ends, and nothing offers this review again in this launch.
    Answered,
}

/// While the plan aterm chose by itself is provisional, look for the ALab vault on each wake
/// that is a reason to (the window's Local Network step, System State coming to the front, Check
/// again, this Mac's network changing: [`WakeMarks::ends_alab_wait`]) and on a slow timer, first
/// as the watch starts: on a first launch the scoping probe is aterm's first request to the LAN,
/// which macOS holds until the person answers its Local Network question, so it hears nothing.
/// Once one hears it, System State offers the groups it adds as a review of their own, and the
/// plan widens and runs in this launch only as that review admits it.
#[cfg(any(target_os = "macos", test))]
fn widen_on_alab_signal<W: AlabWatch>(watch: &mut W) {
    let mut looks = 0;
    // Only a definite no ends the watch: a read that fails (a pass saving its records) waits
    // for the next look and probes nothing.
    loop {
        let eligible = match watch.widens() {
            Ok(false) => return,
            Ok(true) => true,
            Err(why) => {
                aterm_log::warn!(
                    "setup: reading whether the automatic plan may widen failed: {why}"
                );
                false
            }
        };
        // The first look goes at once: a review a person left unanswered on an earlier launch
        // is offered again as this one starts.
        if looks > 0 {
            watch.wait(ALAB_LOOK_GAP, alab_look_after(looks - 1));
        }
        looks = looks.saturating_add(1);
        if !eligible {
            continue;
        }
        match watch.review() {
            Ok(WidenReview::NotYet) => continue,
            Ok(WidenReview::Answered) => return,
            Ok(WidenReview::Applied) => {}
            Err(why) => {
                aterm_log::warn!("setup: looking for the ALab vault did not finish: {why}");
                continue;
            }
        }
        match watch.look() {
            Ok(Some(_)) => {
                watch.run();
                return;
            }
            Ok(None) => {}
            Err(why) => {
                aterm_log::warn!("setup: widening the automatic plan did not finish: {why}")
            }
        }
    }
}

/// A review of the groups the ALab signal adds was answered in this launch other than by
/// applying it ([`WidenReview::Answered`]): no watch offers it again until the next launch.
#[cfg(target_os = "macos")]
static WIDEN_REVIEW_ANSWERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether a watch of aterm's provisional plan has work in this launch
/// ([`aterm_cli::macos_doctor::provisional_setup_widens`], [`WIDEN_REVIEW_ANSWERED`],
/// [`widens_here_with`]).
#[cfg(target_os = "macos")]
fn provisional_setup_widens_here() -> Result<bool, String> {
    widens_here_with(
        WIDEN_REVIEW_ANSWERED.load(std::sync::atomic::Ordering::Acquire),
        aterm_cli::macos_doctor::provisional_setup_widens,
        aterm_cli::macos_doctor::initial_setup_revision,
    )
}

/// Whether a watch of aterm's provisional plan has work in this launch: no review of the
/// widen was `answered` in it, the records still widen the plan in force (`widens`), and no
/// Stop was asked in it for the run in force (`revision`, [`setup_stop_requested`]). A person
/// who pressed Stop gets no more automatic setup in this launch, so no review offers to widen
/// the run they stopped, even one the Stop came too late for (it finished, and its records keep
/// no Stop). The next launch may offer it, as may a later run in this one (Set up now, a
/// choice), which the Stop did not stop.
#[cfg(target_os = "macos")]
fn widens_here_with(
    answered: bool,
    widens: impl FnOnce() -> Result<bool, String>,
    revision: impl FnOnce() -> Result<u64, String>,
) -> Result<bool, String> {
    if answered || !widens()? {
        return Ok(false);
    }
    Ok(!setup_stop_requested(revision()?))
}

/// A person's Cancel of the review of the groups the ALab signal adds: the plan in force becomes
/// theirs for good ([`aterm_cli::macos_doctor::decline_provisional_setup_widen`]), a write that
/// fails tried again by itself after the retry wait.
#[cfg(target_os = "macos")]
fn decline_widen() {
    let mut seen = SETUP_WAKES.count();
    loop {
        match aterm_cli::macos_doctor::decline_provisional_setup_widen() {
            Ok(_) => break,
            Err(why) if setup_is_newer(&why) => break,
            Err(why) => {
                aterm_log::warn!("setup: keeping the plan as it is did not finish: {why}");
                SETUP_WAKES.back_off(&mut seen, SETUP_RETRY_WAIT);
            }
        }
    }
    aterm_gui::publish_initial_setup_status(initial_setup_status());
}

#[cfg(target_os = "macos")]
struct LiveAlabWatch {
    wakes_seen: WakeMarks,
    /// When a focus gain is a reason to look ([`AlabFocus`]).
    focus: AlabFocus,
    /// The upgrade this watch began, held while a commit that failed can still finish.
    upgrade: aterm_cli::macos_doctor::InitialSetupUpgrade,
    /// The plan the last look widens, or widened.
    widened: Widened,
    /// The page's receipt for the review this watch's upgrade answers: accepted once the
    /// upgrade is saved, so the page takes the widened plan as the one in force; kept only
    /// while the upgrade is pending, and refused otherwise ([`look_settling_with`]).
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    /// The number the review's Apply took as the latest plan change
    /// ([`SETUP_APPLY_GENERATION`]): a later choice overtakes it while it waits for another
    /// setup, as it overtakes a choice waiting. Zero before a review applied.
    generation: u64,
}

#[cfg(target_os = "macos")]
impl LiveAlabWatch {
    fn new() -> Self {
        Self {
            wakes_seen: SETUP_WAKES.marks(),
            focus: AlabFocus::new(),
            upgrade: aterm_cli::macos_doctor::InitialSetupUpgrade::default(),
            widened: Widened::default(),
            receipt: None,
            generation: 0,
        }
    }
}

#[cfg(target_os = "macos")]
impl AlabWatch for LiveAlabWatch {
    fn widens(&mut self) -> Result<bool, String> {
        if self.upgrade.pending() {
            Ok(true)
        } else {
            provisional_setup_widens_here()
        }
    }

    fn wait(&mut self, gap: std::time::Duration, limit: std::time::Duration) {
        // A held upgrade whose write failed is a review's Apply still owed: it is tried again
        // after the retry wait, never after the longer wait between looks for the signal.
        let limit = if self.upgrade.pending() {
            limit.min(SETUP_RETRY_WAIT)
        } else {
            limit
        };
        // This Mac's network changing is the one wake a provisional plan's Mac meets on the way
        // onto the ALab vault's LAN; the watch hears it while it waits, once the subnets move
        // off the ones it waits on ([`alab_network_moved`]). One that could not start leaves the
        // timer and the other wakes.
        *ALAB_WAIT_NETWORK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(aterm_secrets::heard::network());
        let _network = aterm_cli::macos_doctor::ParkWatch::start(
            aterm_cli::macos_doctor::Watches {
                network: true,
                ..aterm_cli::macos_doctor::Watches::default()
            },
            wake_setup_network,
        )
        .ok();
        std::thread::sleep(gap);
        let focus = self.focus.counts(lan_heard(), std::time::Instant::now());
        SETUP_WAKES.back_off_alab(&mut self.wakes_seen, focus, limit.saturating_sub(gap));
    }

    fn review(&mut self) -> Result<WidenReview, String> {
        use aterm_gui::WorkstationSetupDecision;
        if self.upgrade.pending() {
            return Ok(WidenReview::Applied);
        }
        // Read again at the moment of the look: a Stop pressed while the watch waited holds it.
        if !provisional_setup_widens_here()? {
            return Ok(WidenReview::NotYet);
        }
        let Some((in_force, widened)) = aterm_cli::macos_doctor::provisional_setup_widened()?
        else {
            return Ok(WidenReview::NotYet);
        };
        // Offered in System State's tab without taking the person's (design §6); withdrawn once
        // the plan is no longer aterm's to widen (a choice made meanwhile), or a Stop holds it.
        let mut answer = aterm_gui::await_widen_answer_while(
            widened.clone(),
            in_force,
            || !provisional_setup_widens_here().unwrap_or(true),
            || {},
        );
        let password = answer.password.take();
        let receipt = answer.receipt.take();
        let answered = || {
            WIDEN_REVIEW_ANSWERED.store(true, std::sync::atomic::Ordering::Release);
            Ok(WidenReview::Answered)
        };
        match answer.decision {
            WorkstationSetupDecision::Begin(plan) if !answer.chosen && plan == widened => {
                self.receipt = receipt;
                // The latest plan change from now: a later choice overtakes it.
                let generation =
                    SETUP_APPLY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                self.generation = generation;
                // Shown waiting the moment its write finds another setup holding the records
                // (design §3.3), never a run going.
                self.upgrade
                    .noting_wait(move || queue_setup_press(generation));
                Ok(WidenReview::Applied)
            }
            // A press on the review is the person's choice, which runs as their choices do.
            WorkstationSetupDecision::Begin(plan) => {
                if let Some(receipt) = receipt {
                    restart_initial_setup(plan, answer.vault_typed, password, receipt);
                }
                answered()
            }
            WorkstationSetupDecision::Cancelled => {
                WIDEN_REVIEW_ANSWERED.store(true, std::sync::atomic::Ordering::Release);
                decline_widen();
                answered()
            }
            WorkstationSetupDecision::Closed => answered(),
            WorkstationSetupDecision::Unavailable => Ok(WidenReview::NotYet),
        }
    }

    fn look(&mut self) -> Result<Option<u64>, String> {
        let generation = self.generation;
        let (looked, receipt) = look_settling_with(
            &mut self.widened,
            &mut self.upgrade,
            self.receipt.take(),
            |widened| {
                queue_setup_press(generation);
                wait_while_queued(|| {
                    setup_choice_overtaken(generation)
                        || stop_covers(widened.sent)
                        || setup_stop_requested(widened.from)
                })
            },
        );
        self.receipt = receipt;
        if generation != 0 {
            unqueue_setup_press(generation);
        }
        looked
    }

    fn run(&mut self) {
        // A worker parked on the narrower revision wakes, finds it gone, and ends.
        wake_setup();
        run_startup_setup(Some(std::mem::take(&mut self.widened)));
    }
}

/// One look of a watch whose widen review applied (`receipt`, the page's for that Apply), which
/// settles that Apply by every way the look ends but one: accepted once the widened plan is
/// saved (`Ok(Some)`); kept while the upgrade is still pending, whose next look finishes or
/// retires it; and otherwise refused, so the page shows the plan in force again, its controls
/// live and the permission gate released (System State design §3.5). The receipt that comes
/// back is the one kept.
///
/// An upgrade that finds another setup holding the records past its wait
/// ([`aterm_cli::macos_doctor::SETUP_BUSY`]) waits on, as a choice does (§3.3): `queue` shows it
/// waiting, waits a moment and says whether to ask again, `false` once a Stop covers it or a
/// later choice overtook it. An attempt that published nothing is then let go.
#[cfg(target_os = "macos")]
fn look_settling_with(
    widened: &mut Widened,
    upgrade: &mut impl WatchUpgrade,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    mut queue: impl FnMut(&Widened) -> bool,
) -> (
    Result<Option<u64>, String>,
    Option<aterm_gui::InitialSetupApplyReceipt>,
) {
    let looked = loop {
        let looked = look_setup_upgrade_with(widened, upgrade);
        let held_elsewhere = upgrade.pending()
            && matches!(&looked, Err(why) if why == aterm_cli::macos_doctor::SETUP_BUSY);
        if !held_elsewhere {
            break looked;
        }
        if queue(widened) {
            continue;
        }
        // Let go (a Stop, a later choice): an attempt that published nothing goes with it.
        if widened.under_way.is_none() {
            upgrade.forget();
            break Ok(None);
        }
        break looked;
    };
    let receipt = match (&looked, receipt) {
        (Ok(Some(_)), Some(receipt)) => {
            receipt.accept();
            None
        }
        (Ok(Some(_)), None) => None,
        (_, receipt) if upgrade.pending() => receipt,
        // Dropped: refused.
        (_, _) => None,
    };
    (looked, receipt)
}

/// The actual admission port of one look; the test arm uses the same scratch record writers.
#[cfg(target_os = "macos")]
trait WatchUpgrade {
    fn pending(&self) -> bool;
    fn widens_from(&self) -> Option<u64>;
    fn revision(&self) -> Result<u64, String>;
    fn persist(&mut self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>;
    fn published(&self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>;
    fn forget(&mut self);
}

#[cfg(target_os = "macos")]
impl WatchUpgrade for aterm_cli::macos_doctor::InitialSetupUpgrade {
    fn pending(&self) -> bool {
        Self::pending(self)
    }
    fn widens_from(&self) -> Option<u64> {
        Self::widens_from(self)
    }
    fn revision(&self) -> Result<u64, String> {
        aterm_cli::macos_doctor::initial_setup_revision()
    }
    fn persist(&mut self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        Self::persist(self)
    }
    fn published(&self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        Self::published(self)
    }
    fn forget(&mut self) {
        Self::forget(self);
    }
}

#[cfg(target_os = "macos")]
fn look_setup_upgrade_with(
    widened: &mut Widened,
    upgrade: &mut impl WatchUpgrade,
) -> Result<Option<u64>, String> {
    let pending = upgrade.pending();
    if let Some(from) = upgrade.widens_from() {
        widened.from = from;
    } else {
        widened.from = upgrade.revision()?;
        widened.sent = send_admission();
        widened.to = 0;
        widened.under_way = None;
    }
    let unpublished = !pending || widened.under_way.is_none();
    widened
        .under_way
        .get_or_insert_with(|| UnderWay::new(widened.sent));
    // No writes have been published when an idle, unpublished attempt had no guard.
    // A covered Stop can retire it, without naming a speculative next revision.
    if unpublished && (setup_stop_requested(widened.from) || stop_covers(widened.sent)) {
        upgrade.forget();
        widened.under_way = None;
        return Ok(None);
    }
    let result = upgrade.persist();
    let published = match &result {
        Ok(Some(target)) => Ok(Some(*target)),
        Ok(None) => Ok(None),
        Err(_) => upgrade.published(),
    };
    match published {
        Ok(Some(target)) => {
            widened.from = target.from;
            widened.to = target.to;
            widened
                .under_way
                .as_ref()
                .unwrap()
                .published(target.from, target.to);
        }
        Ok(None) => {
            let covered = stop_covers(widened.sent) || setup_stop_requested(widened.from);
            widened.under_way = None;
            if covered || !upgrade.pending() {
                upgrade.forget();
            }
        }
        Err(why) => {
            // A failed read cannot certify that the attempt published nothing.
            return Err(why);
        }
    }
    result.map(|target| target.map(|target| target.to))
}

/// Keep one watch of aterm's own provisional plan. Active runs try a worker; a finished
/// coordinator can use its now-free thread without needing another successful spawn.
#[cfg(target_os = "macos")]
fn maintain_setup_watch(how: WatchMaintenance) {
    maintain_setup_watch_with(
        &SETUP_WATCH,
        how,
        provisional_setup_widens_here,
        |reservation| {
            let mut watch = LiveAlabWatch::new();
            std::thread::Builder::new()
                .name("aterm-setup-alab-watch".into())
                .spawn(move || {
                    run_reserved_alab_watch(reservation, || widen_on_alab_signal(&mut watch));
                })
                .map(|_| ())
        },
        |reservation| {
            let mut watch = LiveAlabWatch::new();
            run_reserved_alab_watch(reservation, || widen_on_alab_signal(&mut watch));
        },
    );
}

/// The one watch of aterm's own provisional plan ([`maintain_setup_watch`]).
#[cfg(target_os = "macos")]
static SETUP_WATCH: SetupWatch = SetupWatch::new();

/// Whether the watch of aterm's own provisional plan is owed: its thread could not start, so a
/// parked run keeps a timer to try it again ([`park_wait_limit`]).
#[cfg(target_os = "macos")]
fn setup_watch_owed() -> bool {
    SETUP_WATCH.owed.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(target_os = "macos")]
fn maintain_setup_watch_with<'a>(
    watching: &'a SetupWatch,
    how: WatchMaintenance,
    mut eligible: impl FnMut() -> Result<bool, String>,
    spawn: impl FnOnce(WatchReservation<'a>) -> std::io::Result<()>,
    inline: impl FnOnce(WatchReservation<'a>),
) {
    use std::sync::atomic::Ordering;
    // Request before testing ownership. The retiring watch sees this even if this
    // run completes before it releases its reservation.
    watching.requests.fetch_add(1, Ordering::SeqCst);
    let Some(mut reservation) = watching.reserve() else {
        return;
    };
    loop {
        let seen = reservation.requests();
        if !matches!(eligible(), Ok(false)) {
            break;
        }
        if !reservation.recheck(seen) {
            watching.owed.store(false, Ordering::SeqCst);
            return;
        }
    }
    match how {
        WatchMaintenance::Spawn => {
            let started = spawn(reservation);
            watching.owed.store(started.is_err(), Ordering::SeqCst);
            if let Err(why) = started {
                aterm_log::warn!("setup: the watch for the ALab vault could not start: {why}");
            }
        }
        WatchMaintenance::Inline => {
            watching.owed.store(false, Ordering::SeqCst);
            inline(reservation);
        }
    }
}

#[cfg(target_os = "macos")]
struct SetupWatch {
    running: std::sync::atomic::AtomicBool,
    requests: std::sync::atomic::AtomicU64,
    /// The last try to start the watch's thread failed ([`setup_watch_owed`]).
    owed: std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "macos")]
impl SetupWatch {
    const fn new() -> Self {
        Self {
            running: std::sync::atomic::AtomicBool::new(false),
            requests: std::sync::atomic::AtomicU64::new(0),
            owed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn reserve(&self) -> Option<WatchReservation<'_>> {
        use std::sync::atomic::Ordering;
        self.running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| WatchReservation {
                watch: self,
                held: true,
            })
    }
}

#[cfg(target_os = "macos")]
struct WatchReservation<'a> {
    watch: &'a SetupWatch,
    held: bool,
}

#[cfg(target_os = "macos")]
impl WatchReservation<'_> {
    fn requests(&self) -> u64 {
        self.watch
            .requests
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn release(&mut self) {
        if self.held {
            self.held = false;
            self.watch
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn recheck(&mut self, seen: u64) -> bool {
        use std::sync::atomic::Ordering;
        self.release();
        // Sequential consistency orders request-before-reserve against release-before-read:
        // an entrant either acquires the free reservation or leaves a visible recheck here.
        if self.requests() != seen
            && self
                .watch
                .running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.held = true;
            true
        } else {
            false
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for WatchReservation<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(target_os = "macos")]
fn run_reserved_alab_watch(mut reservation: WatchReservation<'_>, mut run: impl FnMut()) {
    loop {
        let seen = reservation.requests();
        run();
        if !reservation.recheck(seen) {
            return;
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
    reserved: Option<StartupSent>,
    review: Review,
    offer: impl FnMut(
        aterm_types::macos_setup::Plan,
        StartupLook,
        StartupHandoff,
    ) -> aterm_gui::SetupAnswer,
) -> (PreparedSetup<StartupClaim>, StartupSent, bool) {
    let mut host = LiveSetupStartup {
        offer,
        review,
        receipt: None,
        authorization: None,
        last_failure: None,
        observation: None,
        offered: None,
        chosen: false,
        vault_typed: false,
        sent: reserved.unwrap_or_default(),
        retry_seen: SETUP_WAKES.count(),
        recovered: false,
        reviewed: None,
    };
    let prepared = match prepare_setup(&mut host) {
        Ok(prepared) => prepared,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor could not start: {why}");
            PreparedSetup::Unavailable
        }
    };
    startup_went_on();
    (prepared, host.sent, host.recovered)
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
    /// A repaired continuation keeps its reserved admission across send boundaries.
    recovery: bool,
    under_way: std::sync::Arc<std::sync::Mutex<Option<UnderWay>>>,
    /// A preparatory write keeps any earlier publication uncertainty through retries.
    preparing: Option<RepairPublication>,
    /// A repair's exact staged checkpoint, separate from the next Begin's first save.
    repair: Option<UnderWay>,
    /// The revision a watch widened into the plan this startup resumes ([`Widened`]).
    widened_from: Option<u64>,
    upgrade: Option<UnderWay>,
}

#[cfg(target_os = "macos")]
struct RepairPublication {
    temporary: bool,
    uncertain: bool,
}

#[cfg(target_os = "macos")]
type StartupHandoff = Box<dyn FnOnce() + Send>;

#[cfg(target_os = "macos")]
impl StartupSent {
    /// Reserve the next review in the same ordering as Stop. Its initial and
    /// pre-offer sends must never remint this continuation past a later Stop.
    fn reserve_recovery(from: u64) -> Option<Self> {
        let _run = SETUP_RUN.lock().ok()?;
        if stop_covers(from) {
            return None;
        }
        let mut sent = Self::default();
        sent.send();
        sent.recovery = true;
        Some(sent)
    }

    fn continue_recovery(mut retired: Self) -> Option<Self> {
        let mut next = Self::reserve_recovery(retired.sent)?;
        next.repair = retired.repair.take();
        Some(next)
    }

    /// The old review and its password retire; only its exact Stop evidence continues.
    fn take_recovery(&mut self) -> Self {
        Self {
            sent: self.sent,
            repair: self.repair.take(),
            ..Self::default()
        }
    }

    /// Own any publication before a repair can write. Even without a Begin, a
    /// Stop must wait for that attempt to identify or abandon its checkpoint.
    fn begin_repair(&mut self) {
        let mut guard = self
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let repair = self.repair.as_ref().filter(|guard| guard.0 == self.sent);
        let temporary = guard.is_none() && repair.is_none();
        if temporary {
            *guard = Some(UnderWay::new(self.sent));
        }
        let uncertain = guard.as_ref().or(repair).unwrap().begin_publication();
        if let Some(publication) = &mut self.preparing {
            // A newer admission may have retired the previous error. Every
            // actual attempt rearms debt using the live ledger as its fallback.
            publication.uncertain = uncertain;
        } else {
            self.preparing = Some(RepairPublication {
                temporary,
                uncertain,
            });
        }
    }

    fn no_repair(&mut self) {
        if let Some(mut publication) = self.preparing.take() {
            let mut guard = self
                .under_way
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            publication.uncertain = guard
                .as_ref()
                .or(self.repair.as_ref())
                .is_some_and(|guard| guard.restore_publication(publication.uncertain));
            if publication.temporary && !publication.uncertain {
                guard.take();
            }
            if publication.uncertain {
                self.preparing = Some(publication);
            }
        }
    }

    fn repair_failed(&mut self) {
        if let Some(publication) = &mut self.preparing {
            publication.uncertain = true;
        }
    }

    /// Collect exact checkpoints while the backend owns its profile lock, then
    /// update Stop ownership once that lock has been released.
    fn observe_repair_with<T>(
        &mut self,
        work: impl FnOnce(&mut dyn FnMut(u64)) -> Result<T, String>,
    ) -> Result<T, String> {
        self.begin_repair();
        let mut published = None;
        let result = work(&mut |revision| published = Some(revision));
        if let Some(revision) = published {
            self.record_repair(revision);
        }
        if result.is_ok() {
            self.no_repair();
        } else {
            self.repair_failed();
        }
        result
    }

    fn record_repair(&mut self, target: u64) {
        let guard = self
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(guard) = guard {
            guard.record_publication(Ok(Some(target)));
            self.repair = Some(guard);
        } else if let Some(guard) = &self.repair
            && guard.0 == self.sent
        {
            guard.record_publication(Ok(Some(target)));
        } else {
            let guard = UnderWay::new(self.sent);
            guard.record_publication(Ok(Some(target)));
            self.repair = Some(guard);
        }
        self.preparing = None;
        // Whatever number held the checkpoint, a future Begin must own another
        // unresolved debt before it can publish its own admitted revision.
        self.recovery = false;
    }

    /// Stop evidence is independent of whether this review may still admit its plan.
    fn record_publication(&mut self, publication: Result<Option<u64>, String>) {
        let confirmed = matches!(publication, Ok(Some(_)));
        let under_way = self
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(under_way) = under_way.as_ref() {
            under_way.record_publication(publication);
            if confirmed {
                self.preparing = None;
            }
        }
    }

    /// A new durable answer supersedes older abandoned publications, including when empty.
    fn admitted(&self) {
        retire_unconfirmed_admissions(self.sent);
    }

    fn inherit_widened<C>(&mut self, prepared: &PreparedSetup<C>, mut widened: Widened) {
        if matches!(prepared, PreparedSetup::Authorized { revision, .. } if *revision == widened.to)
        {
            self.sent = self.sent.min(widened.sent);
            self.widened_from = Some(widened.from);
            self.upgrade = widened.under_way.take();
        }
    }
    /// Number the admission the startup sends now: a resume, or an offer.
    fn send(&mut self) {
        if !self.recovery {
            // A repair can publish before any page answer. Keep its Stop guard,
            // and reserve the distinct future Begin only if Stop did not cover
            // this review. The two debts must never share an admission number.
            if self.repair.is_some() {
                let _run = SETUP_RUN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !stop_covers(self.sent) {
                    self.under_way = Default::default();
                    self.sent = send_admission();
                }
                self.recovery = true;
                return;
            }
            if self.preparing.is_some() {
                // An earlier write has not identified its publication yet. A
                // later answer may settle it, but numbering cannot escape Stop.
                self.recovery = true;
                return;
            }
            self.under_way = Default::default();
            self.sent = send_admission();
        }
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
    plan: aterm_types::macos_setup::Plan,
    revision: u64,
    completed: bool,
}

#[cfg(any(target_os = "macos", test))]
trait SetupStartup {
    type Claim;

    /// Whether this launch's person is resolved: a passing failure waits before any records
    /// bind the review; a lasting refusal has no startup work.
    fn scope_ready(&mut self) -> Result<bool, String> {
        Ok(true)
    }

    /// Bring forward a setup a Restore ended before any run was admitted here (an older
    /// aterm's), so this launch's offer gives it aterm's own permissions with its Mac preference
    /// groups left as is (System State design §3.4). It runs once the person is resolved and
    /// before the records are bound. `Ok(false)` while another pass holds the profile lock past
    /// the bring-forward's own wait: the startup waits quietly and asks again. Temporary
    /// errors use the ordinary startup retry note; a newer aterm's records end this attempt.
    fn bring_forward(&mut self) -> Result<bool, String> {
        Ok(true)
    }

    /// Set aside the saved setup records that are corrupt for good, so the restored setup
    /// brought forward and the records bound are fresh ones (System State design §3.5). A
    /// temporary error (a read that failed for another reason, the records another setup holds)
    /// is tried again. A newer aterm's records are left as they are.
    fn set_aside(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Repair records that became corrupt after this review bound. A repair
    /// retires this review; only a fresh coordinator may bind the new records.
    fn recover_after_read_error(&mut self) -> Result<bool, String> {
        Ok(false)
    }

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
    /// Whether this startup may show a plan nobody admitted yet for review: an update
    /// successor's may not ([`Review::Withheld`]), and leaves a note instead. Asked before the
    /// plan is read, so a withheld review reads and probes nothing for it.
    fn may_offer(&mut self) -> bool {
        true
    }
    /// Offer the plan of `admission`, the run this startup resumes, back to System State without
    /// waiting for its answer (design §3.3, §6): the page answers it by itself as it answers the
    /// first launch's offer, and that answer, taken as "go on with this revision" (nothing new
    /// admitted, the page's receipt accepted at once), is the window's own admission of the
    /// plan, which the debugger's step needs and an earlier process's yes never gives. Every
    /// startup offers it, a launch's, an update successor's, a heal's or a Restore's alike: the
    /// page answers only with a person at this Mac, and lets the offer go when the window holds
    /// its own admission of the plan in force already.
    fn offer_resume(&mut self, _admission: &SetupAdmission) {}
    fn initial(&mut self) -> Result<aterm_types::macos_setup::Plan, String>;
    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision;
    /// Record a person's Cancel of the review just answered, for good (design §3.3): every group
    /// at Leave as is, aterm's own permissions with them, until Set up now applies the plan the
    /// review showed or a group choice sets up its group. `Ok(false)` when the records changed
    /// since the review bound (a choice made meanwhile answers for it): nothing is written.
    fn cancel(&mut self) -> Result<bool, String> {
        Ok(true)
    }
    /// The person closed the review just shown, unanswered: nothing is recorded, nothing offers
    /// it again in this launch, and System State shows the plan it showed with Set up now; the
    /// next launch offers it once more (design §3.3).
    fn closed(&mut self) {}
    fn validate(&mut self, _: &aterm_types::macos_setup::Plan) -> Result<(), String> {
        Ok(())
    }
    fn authorize(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String>;
    fn authorization_pending(&self) -> bool;
    /// The last failed admission could not read its current record context.
    fn authorization_read_failed(&self) -> bool {
        false
    }
    fn stopped(&self) -> bool {
        false
    }
    /// Wait before writing the page's answer again, holding the claim. The
    /// wait ends the moment a person's choice made in this process waits for
    /// that claim (`run_setup_restart`), or already does: `true` then, and the
    /// claim goes to the choice. `false` once the wait ran out without one.
    fn wait_holding_claim(&mut self) -> bool;
    /// Wait before trying again, holding no claim.
    fn backoff(&mut self);
    /// A step failed that this startup tries again by itself.
    fn report_failure(&mut self, why: &str);
    /// A step failed that ends this attempt: nothing tries it again by itself.
    fn report_rejection(&mut self, why: &str) {
        self.report_failure(why);
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupStartup<F> {
    /// Shows the plan on System State and waits for the page's answer.
    offer: F,
    /// Whether this startup may show it ([`Review`]).
    review: Review,
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
    /// Setup changes heard before the operation whose failure may need a retry.
    retry_seen: u64,
    /// This bound review repaired corrupt records and retired without adopting them.
    recovered: bool,
    /// The plan the last review showed when it was answered: the one offered, or the plan this
    /// Mac's ALab signal widened it to while it was up. A Cancel names it.
    reviewed: Option<aterm_types::macos_setup::Plan>,
}

/// What the offer of a resumed run's plan reads and waits on ([`run_resume_offer`]): the live
/// records and System State's channel in aterm ([`LiveResumeOffer`]), a script in a test.
#[cfg(any(target_os = "macos", test))]
trait ResumeOfferReach {
    /// Whether the resumed run is still the one admitted, unfinished and unstopped; `None` while
    /// the records cannot be read (a write under way, say).
    fn current(&mut self) -> Option<bool>;
    /// Whether a choice of the page's waits for the setup claim
    /// ([`SetupStartup::choice_awaits_claim`]): the page's own choice goes first.
    fn choice_awaits_claim(&mut self) -> bool;
    /// Offer `plan` on System State and wait for the answer, which the offer's going
    /// ([`Self::current`] false, or a choice waiting for the claim) withdraws as `Unavailable`.
    fn answer(&mut self, plan: aterm_types::macos_setup::Plan) -> aterm_gui::SetupAnswer;
    /// Wait up to `limit`, or until a setup wake.
    fn back_off(&mut self, limit: std::time::Duration);
}

/// How the offer of a resumed run's plan ended ([`run_resume_offer`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeOffer {
    /// The page's automatic answer of the plan as offered, its receipt accepted: the window's own
    /// admission of the resumed revision.
    Admitted,
    /// The offer went unanswered, or its answer was let go.
    LetGo,
}

/// Offer `plan`, the plan of the run a startup resumes, back until the page answers it
/// ([`SetupStartup::offer_resume`]). The page's automatic answer of that same plan is taken as
/// "go on": nothing is written, and its receipt is accepted while the run is still the one
/// admitted, unfinished and unstopped, so the window's own admission names that revision. Any
/// other answer, or one that comes after the run went, is let go. An offer no window could show
/// is made again once one can, each time only while the run is still current and no choice of the
/// page's waits for the claim, so a stale offer never reaches a page whose person chose since.
#[cfg(any(target_os = "macos", test))]
fn run_resume_offer(
    reach: &mut impl ResumeOfferReach,
    plan: &aterm_types::macos_setup::Plan,
) -> ResumeOffer {
    use aterm_gui::WorkstationSetupDecision;
    loop {
        if reach.choice_awaits_claim() || reach.current() == Some(false) {
            return ResumeOffer::LetGo;
        }
        let mut answer = reach.answer(plan.clone());
        match answer.decision {
            WorkstationSetupDecision::Begin(ref answered) => {
                let Some(receipt) = answer.receipt.take() else {
                    return ResumeOffer::LetGo;
                };
                let as_offered = answered == plan
                    && receipt.admission() == aterm_gui::InitialSetupAdmission::Automatic
                    && !answer.chosen
                    && !answer.vault_typed
                    && answer.changed.is_none();
                if !as_offered {
                    return ResumeOffer::LetGo;
                }
                // A read that met a write is tried again for a moment; a receipt let go refuses
                // the answer, and the page reads the records again.
                let mut reads = 0;
                let still = loop {
                    match reach.current() {
                        Some(still) => break still,
                        None if reads < 8 => {
                            reads += 1;
                            reach.back_off(std::time::Duration::from_millis(250));
                        }
                        None => break false,
                    }
                };
                if !still {
                    return ResumeOffer::LetGo;
                }
                receipt.accept();
                return ResumeOffer::Admitted;
            }
            WorkstationSetupDecision::Unavailable => reach.back_off(SETUP_RETRY_WAIT),
            WorkstationSetupDecision::Cancelled | WorkstationSetupDecision::Closed => {
                return ResumeOffer::LetGo;
            }
        }
    }
}

/// The live [`ResumeOfferReach`] of the run of `revision`, whose startup's admission number is
/// `sent` (a Stop pressed since covers it).
#[cfg(target_os = "macos")]
struct LiveResumeOffer {
    revision: u64,
    sent: u64,
    seen: u64,
}

#[cfg(target_os = "macos")]
impl LiveResumeOffer {
    fn current_of(revision: u64, sent: u64) -> Option<bool> {
        if stop_covers(sent) {
            return Some(false);
        }
        aterm_cli::macos_doctor::initial_setup_admission()
            .ok()
            .map(|admission| {
                admission
                    .is_some_and(|admission| admission.revision == revision && !admission.completed)
            })
    }
}

#[cfg(target_os = "macos")]
impl ResumeOfferReach for LiveResumeOffer {
    fn current(&mut self) -> Option<bool> {
        Self::current_of(self.revision, self.sent)
    }

    fn choice_awaits_claim(&mut self) -> bool {
        choice_awaits_claim()
    }

    fn answer(&mut self, plan: aterm_types::macos_setup::Plan) -> aterm_gui::SetupAnswer {
        let (revision, sent) = (self.revision, self.sent);
        aterm_gui::await_resume_answer_while(plan, move || {
            choice_awaits_claim() || Self::current_of(revision, sent) == Some(false)
        })
    }

    fn back_off(&mut self, limit: std::time::Duration) {
        SETUP_WAKES.back_off(&mut self.seen, limit);
    }
}

/// Offer `plan`, the run of `revision` a startup resumes, back to System State on a thread of its
/// own ([`run_resume_offer`]), so the run never waits for the answer.
#[cfg(target_os = "macos")]
fn offer_resumed_plan(plan: aterm_types::macos_setup::Plan, revision: u64, sent: u64) {
    let spawned = std::thread::Builder::new()
        .name("aterm-setup-resume-offer".into())
        .spawn(move || {
            let mut reach = LiveResumeOffer {
                revision,
                sent,
                seen: SETUP_WAKES.count(),
            };
            if run_resume_offer(&mut reach, &plan) == ResumeOffer::Admitted {
                aterm_log::info!("initial setup: the window admitted resumed revision {revision}");
            }
        });
    if let Err(why) = spawned {
        aterm_log::warn!("initial setup: the resumed plan's offer could not start: {why}");
    }
}

/// What a startup's review asks each second while it is up: the plan the ALab signal widens its
/// draft to, once ([`aterm_gui::await_initial_setup_answer_while`]).
#[cfg(target_os = "macos")]
type StartupLook = Box<dyn FnMut() -> Option<aterm_types::macos_setup::Plan>>;

/// The plan a review of `offered` widens to in place once this Mac shows the ALab signal while
/// it is up (design §3.5): the whole profile, for aterm's own provisional offer that is less.
#[cfg(target_os = "macos")]
fn review_widens_to(
    offered: &aterm_types::macos_setup::Plan,
    provisional: bool,
) -> Option<aterm_types::macos_setup::Plan> {
    let all = aterm_types::macos_setup::Selection::all();
    (provisional && offered.selection != all).then(|| aterm_types::macos_setup::Plan {
        selection: all,
        vault_url: offered.vault_url.clone(),
    })
}

/// The review's look for the ALab signal, paced as the watch's ([`widen_on_alab_signal`]): on a
/// setup wake that is a reason to look ([`WakeMarks::ends_alab_wait`]: the Local Network answer,
/// the review coming to the front, Check again) and on a timer from [`ALAB_LOOK_FIRST`] doubling,
/// never twice within [`ALAB_LOOK_GAP`]; `widened` once this Mac shows it
/// ([`aterm_cli::macos_doctor::alab_mac`], which may probe the network for a few seconds on this
/// worker). The offer itself looked just before.
#[cfg(target_os = "macos")]
fn review_alab_look(widened: aterm_types::macos_setup::Plan) -> StartupLook {
    let mut seen = SETUP_WAKES.marks();
    let mut last = std::time::Instant::now();
    let mut looks = 0;
    let mut focus = AlabFocus::new();
    Box::new(move || {
        let wakes = SETUP_WAKES.marks();
        let since = last.elapsed();
        if since < ALAB_LOOK_GAP {
            return None;
        }
        if since < alab_look_after(looks) {
            if wakes == seen {
                return None;
            }
            if !wakes.ends_alab_wait(seen, focus.counts(lan_heard(), std::time::Instant::now())) {
                // A wake that is no reason to look (a focus gain [`AlabFocus`] does not count) is
                // taken as heard, so the next tick does not weigh it again.
                seen = wakes;
                return None;
            }
        }
        seen = wakes;
        last = std::time::Instant::now();
        looks = looks.saturating_add(1);
        aterm_cli::macos_doctor::alab_mac().then(|| widened.clone())
    })
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupAnswerKind {
    Automatic,
    /// The page's answer with no press, of a plan that is not aterm's provisional one.
    Offered,
    Person,
    Join,
}

#[cfg(target_os = "macos")]
impl<F> LiveSetupStartup<F> {
    fn backoff_for(&mut self, limit: std::time::Duration) -> bool {
        SETUP_WAKES.back_off(&mut self.retry_seen, limit)
    }

    fn wait_holding_claim_for(&mut self, limit: std::time::Duration) -> bool {
        wait_before_admission_retry(
            self.sent.sent,
            &mut self.retry_seen,
            limit,
            |seen, limit| {
                if !choice_awaits_claim() {
                    SETUP_WAKES.back_off(seen, limit);
                }
            },
        );
        choice_awaits_claim()
    }

    fn answer_kind(&self, plan: &aterm_types::macos_setup::Plan) -> StartupAnswerKind {
        if self.vault_typed {
            StartupAnswerKind::Join
        } else if self.chosen {
            StartupAnswerKind::Person
        } else if self
            .offered
            .as_ref()
            .is_some_and(|(offered, provisional)| *provisional && offered == plan)
        {
            StartupAnswerKind::Automatic
        } else {
            StartupAnswerKind::Offered
        }
    }
}

#[cfg(target_os = "macos")]
impl<
    F: FnMut(aterm_types::macos_setup::Plan, StartupLook, StartupHandoff) -> aterm_gui::SetupAnswer,
> SetupStartup for LiveSetupStartup<F>
{
    type Claim = StartupClaim;

    fn scope_ready(&mut self) -> Result<bool, String> {
        aterm_cli::macos_doctor::initial_setup_scope_ready()
    }

    fn set_aside(&mut self) -> Result<(), String> {
        let result = self.sent.observe_repair_with(|published| {
            aterm_cli::macos_doctor::set_aside_unreadable_setup_observing(published)
        });
        match result {
            Ok(Some(set)) => {
                self.sent.record_repair(set.stop_target);
                note_setup_set_aside(&set);
            }
            Ok(None) => {}
            Err(why) => return Err(why),
        }
        Ok(())
    }

    fn recover_after_read_error(&mut self) -> Result<bool, String> {
        let result = self.sent.observe_repair_with(|published| {
            aterm_cli::macos_doctor::set_aside_unreadable_setup_observing(published)
        });
        match result {
            Ok(Some(set)) => {
                self.sent.record_repair(set.stop_target);
                note_setup_set_aside(&set);
                self.recovered = true;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(why) => Err(why),
        }
    }

    fn bring_forward(&mut self) -> Result<bool, String> {
        self.sent
            .observe_repair_with(|published| {
                aterm_cli::macos_doctor::bring_forward_restored_setup_observing(published)
            })
            .map(|outcome| !matches!(outcome, aterm_cli::macos_doctor::BroughtForward::Busy))
    }

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
        // The revision the review bound, read the same way: a choice cut short after it saved
        // its plan, before its progress, still resumes at this launch.
        aterm_cli::macos_doctor::initial_setup_review_revision()
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
                plan: admission.plan,
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

    fn may_offer(&mut self) -> bool {
        if self.review == Review::Withheld {
            aterm_gui::note_setup_unfinished();
            return false;
        }
        true
    }

    fn offer_resume(&mut self, admission: &SetupAdmission) {
        offer_resumed_plan(admission.plan.clone(), admission.revision, self.sent.sent);
    }

    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision {
        startup_went_on();
        // The page shows the review itself: the note an update successor left goes.
        aterm_gui::clear_setup_unfinished();
        let widen_to = self
            .offered
            .as_ref()
            .and_then(|(offered, provisional)| review_widens_to(offered, *provisional));
        let look = match widen_to.clone() {
            Some(widened) => review_alab_look(widened),
            None => Box::new(|| None),
        };
        let mut answer = (self.offer)(initial.clone(), look, self.sent.handoff());
        // A retry's wait starts once the offer is answered: the review's own coming to the
        // front is a setup wake, which must not end the wait before the next offer at once.
        self.retry_seen = SETUP_WAKES.count();
        self.receipt = answer.receipt.take();
        self.chosen = answer.chosen;
        self.vault_typed = answer.vault_typed;
        self.reviewed = Some(initial);
        if answer.widened
            && let Some(widened) = widen_to
        {
            // The widened plan is aterm's own provisional offer now, as the next launch on this
            // ALab Mac offers it: admitted untouched, it is the automatic admission.
            self.reviewed = Some(widened.clone());
            self.offered = Some((widened, true));
        }
        answer.decision
    }

    fn cancel(&mut self) -> Result<bool, String> {
        let reviewed = self
            .reviewed
            .clone()
            .ok_or("the startup review showed no plan")?;
        let authorization = self
            .authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?;
        let observed = authorization.cancel_observed(&reviewed);
        self.observation = observed.observation;
        let cancelled = observed.result?;
        clear_initial_setup_rows();
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        Ok(cancelled)
    }

    fn closed(&mut self) {
        note_review_closed(self.reviewed.clone());
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }

    fn validate(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<(), String> {
        self.observation = aterm_cli::macos_doctor::initial_setup_observation().ok();
        aterm_cli::macos_doctor::validate_initial_setup_plan(plan)
    }

    fn authorize(&mut self, plan: &aterm_types::macos_setup::Plan) -> Result<Option<u64>, String> {
        self.retry_seen = SETUP_WAKES.count();
        let kind = self.answer_kind(plan);
        let authorization = self
            .authorization
            .as_mut()
            .ok_or("the startup review has no bound authorization")?;
        let observed = match kind {
            StartupAnswerKind::Automatic => authorization.persist_automatic_observed(plan),
            StartupAnswerKind::Offered => authorization.persist_offered_observed(plan),
            StartupAnswerKind::Person => authorization.persist_observed(plan),
            StartupAnswerKind::Join => authorization.persist_join_observed(plan),
        };
        self.observation = observed.observation;
        self.sent.record_publication(authorization.stop_target());
        let not_joined = authorization.join_unread();
        let revision = observed.result?;
        if revision.is_some() {
            self.sent.admitted();
            note_vault_not_joined(not_joined);
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

    fn authorization_read_failed(&self) -> bool {
        self.observation.is_none()
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }

    fn wait_holding_claim(&mut self) -> bool {
        self.wait_holding_claim_for(SETUP_RETRY_WAIT)
    }

    fn backoff(&mut self) {
        // Focus returning, Check again and other setup recovery signals all resume
        // the same bound review. An unchanged failure still waits for its timer.
        self.backoff_for(SETUP_RETRY_WAIT);
    }

    fn report_failure(&mut self, why: &str) {
        // Every startup step is tried again by itself after a wait: no failure for the page to
        // signal, one calm line (System State design §3.5, §3.6).
        startup_step_failed(why);
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
    ) -> LiveSetupStartup<impl FnMut(Plan, StartupLook, StartupHandoff) -> aterm_gui::SetupAnswer>
    {
        let plan = Plan::default();
        let mut host = LiveSetupStartup {
            offer: move |plan, _, _| aterm_gui::SetupAnswer {
                chosen,
                vault_typed,
                ..aterm_gui::WorkstationSetupDecision::Begin(plan).into()
            },
            review: Review::Offered,
            receipt: None,
            authorization: None,
            last_failure: None,
            observation: None,
            offered: Some((plan.clone(), true)),
            chosen: false,
            vault_typed: false,
            sent: StartupSent::default(),
            retry_seen: SETUP_WAKES.count(),
            recovered: false,
            reviewed: None,
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

    /// The page's answer with no press, of a plan aterm did not choose by itself (the plan a
    /// set-aside or a Restore's bring-forward left for the offer), chooses nothing: it is the
    /// offered admission, never a person's, so a set-aside's note stays.
    #[test]
    fn an_unpressed_answer_of_a_persons_plan_is_the_offered_admission() {
        let mut host = answered(false, false);
        host.offered = Some((Plan::default(), false));
        assert_eq!(
            host.answer_kind(&Plan::default()),
            StartupAnswerKind::Offered
        );
    }

    #[test]
    fn a_startup_join_uses_the_explicit_admission_path() {
        assert_eq!(
            answered(true, true).answer_kind(&Plan::default()),
            StartupAnswerKind::Join
        );
    }

    /// The review showed the plan this Mac's ALab signal widened its untouched draft to while it
    /// was up (design §3.5): admitted untouched, it is aterm's own provisional admission, as the
    /// next launch on this ALab Mac offers it, and a Cancel names it as the plan the review
    /// showed. Only aterm's own provisional plan widens, and only to the whole profile.
    #[test]
    fn a_review_widened_in_place_answers_for_the_widened_plan() {
        use aterm_types::macos_setup::{Selection, Setting};
        let mut permissions = Selection::none();
        permissions.set(Setting::Permissions, true);
        let narrow = Plan::from(permissions);
        let widened = review_widens_to(&narrow, true).expect("the provisional plan widens");
        assert_eq!(widened.selection, Selection::all());
        assert_eq!(widened.vault_url, narrow.vault_url);
        assert_eq!(
            review_widens_to(&narrow, false),
            None,
            "a person's plan stays"
        );
        assert_eq!(
            review_widens_to(&widened, true),
            None,
            "the whole profile stays"
        );
        let answer = widened.clone();
        let mut host = LiveSetupStartup {
            offer: move |_, _, _| aterm_gui::SetupAnswer {
                widened: true,
                ..aterm_gui::WorkstationSetupDecision::Begin(answer.clone()).into()
            },
            review: Review::Offered,
            receipt: None,
            authorization: None,
            last_failure: None,
            observation: None,
            offered: Some((narrow.clone(), true)),
            chosen: false,
            vault_typed: false,
            sent: StartupSent::default(),
            retry_seen: SETUP_WAKES.count(),
            recovered: false,
            reviewed: None,
        };
        assert_eq!(
            host.offer(narrow),
            aterm_gui::WorkstationSetupDecision::Begin(widened.clone())
        );
        assert_eq!(host.answer_kind(&widened), StartupAnswerKind::Automatic);
        assert_eq!(host.reviewed, Some(widened));
    }
}

#[cfg(target_os = "macos")]
use aterm_cli::macos_doctor::setup_is_newer;

/// Off macOS no setup is saved, so none is a newer aterm's (the retry tests run everywhere).
#[cfg(all(test, not(target_os = "macos")))]
fn setup_is_newer(_why: &str) -> bool {
    false
}

#[cfg(any(target_os = "macos", test))]
enum PreparedSetup<C> {
    Authorized {
        claim: C,
        revision: u64,
    },
    Finished,
    /// A person's Cancel of the review, recorded for good (design §3.3).
    Cancelled,
    /// The person closed the review unanswered: not this launch (design §3.3).
    Closed,
    Unavailable,
}

/// A startup step finished, or Stop retired its review without another attempt.
#[cfg(any(target_os = "macos", test))]
enum SetupStep<T> {
    Ready(T),
    Stopped,
}

/// Retry state I/O on the window's background worker until it succeeds or Stop
/// retires the review. A newer aterm's records end the attempt without another read.
/// A mutator records its publication evidence before returning.
#[cfg(any(target_os = "macos", test))]
fn retry_setup_step<H: SetupStartup, T>(
    host: &mut H,
    mut step: impl FnMut(&mut H) -> Result<T, String>,
) -> Result<SetupStep<T>, String> {
    loop {
        if host.stopped() {
            return Ok(SetupStep::Stopped);
        }
        match step(host) {
            Ok(value) => {
                return Ok(if host.stopped() {
                    SetupStep::Stopped
                } else {
                    SetupStep::Ready(value)
                });
            }
            Err(why) => {
                if host.stopped() {
                    return Ok(SetupStep::Stopped);
                }
                if setup_is_newer(&why) {
                    host.report_rejection(&why);
                    return Err(why);
                }
                host.report_failure(&why);
                if host.stopped() {
                    return Ok(SetupStep::Stopped);
                }
                host.backoff();
            }
        }
    }
}

/// A bound review retries local read errors, but never adopts records repaired
/// since it bound. The caller releases its claim before asking for recovery. A newer aterm's
/// records end it: reading them again changes nothing, so nothing is tried again.
#[cfg(any(target_os = "macos", test))]
fn report_bound_setup_read_error<H: SetupStartup>(
    host: &mut H,
    why: &str,
) -> Result<SetupStep<()>, String> {
    if host.stopped() {
        return Ok(SetupStep::Stopped);
    }
    if setup_is_newer(why) {
        host.report_rejection(why);
        return Err(why.to_owned());
    }
    host.report_failure(why);
    if host.stopped() {
        return Ok(SetupStep::Stopped);
    }
    let repaired = host.recover_after_read_error().unwrap_or(false);
    if host.stopped() {
        Ok(SetupStep::Stopped)
    } else if repaired {
        Err(why.to_owned())
    } else {
        Ok(SetupStep::Ready(()))
    }
}

#[cfg(any(target_os = "macos", test))]
fn retry_bound_setup_read<H: SetupStartup, T>(
    host: &mut H,
    mut read: impl FnMut(&mut H) -> Result<T, String>,
) -> Result<SetupStep<T>, String> {
    loop {
        if host.stopped() {
            return Ok(SetupStep::Stopped);
        }
        match read(host) {
            Ok(value) => {
                return Ok(if host.stopped() {
                    SetupStep::Stopped
                } else {
                    SetupStep::Ready(value)
                });
            }
            Err(why) => {
                if matches!(
                    report_bound_setup_read_error(host, &why)?,
                    SetupStep::Stopped
                ) {
                    return Ok(SetupStep::Stopped);
                }
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
    if host.stopped() {
        return Ok(PreparedSetup::Unavailable);
    }
    // An unavailable home can look like empty setup records. Wait for the account to resolve
    // before binding its one review.
    if !matches!(
        retry_setup_step(host, H::scope_ready)?,
        SetupStep::Ready(true)
    ) {
        return Ok(PreparedSetup::Unavailable);
    }
    // A record corrupt for good is set aside first, so what is brought forward and bound is a
    // fresh record (§3.5); a set-aside that fails is tried again by itself.
    if matches!(retry_setup_step(host, H::set_aside)?, SetupStep::Stopped) {
        return Ok(PreparedSetup::Unavailable);
    }
    // Finish a bring-forward before binding: another pass's lock and transient
    // writes both wait here. Stop retires this review before another attempt or
    // before it can adopt records a later manual choice published.
    loop {
        match retry_setup_step(host, H::bring_forward)? {
            SetupStep::Ready(true) => break,
            SetupStep::Ready(false) => host.backoff(),
            SetupStep::Stopped => return Ok(PreparedSetup::Unavailable),
        }
    }
    if host.stopped() {
        return Ok(PreparedSetup::Unavailable);
    }
    // The review is bound once: a capture that failed ends this attempt, never to bind a later
    // run's revision (a manual repair's) as this launch's own. A saved setup that stays
    // unreadable starts a fresh attempt from the page's read (`heal_unread_setup`).
    let revision = host
        .bind_authorization()
        .inspect_err(|why| host.report_rejection(why))?;
    // Capture holds no automatic claim: a Stop and a later manual admission
    // may both have completed while it read. This older coordinator cannot
    // resume that newer run using the Stop-covered admission it sent.
    if host.stopped() {
        return Ok(PreparedSetup::Unavailable);
    }
    enum BeforeOffer {
        Initial(aterm_types::macos_setup::Plan),
        Resume(SetupAdmission),
        Finished,
        Superseded,
        Withheld,
    }
    loop {
        let claim = match retry_bound_setup_read(host, H::claim)? {
            SetupStep::Ready(Some(claim)) => claim,
            SetupStep::Ready(None) => {
                match retry_bound_setup_read(host, H::finished)? {
                    SetupStep::Ready(true) => return Ok(PreparedSetup::Finished),
                    SetupStep::Ready(false) => host.backoff(),
                    SetupStep::Stopped => return Ok(PreparedSetup::Unavailable),
                }
                continue;
            }
            SetupStep::Stopped => return Ok(PreparedSetup::Unavailable),
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
                    BeforeOffer::Resume(admission)
                });
            }
            if !host.may_offer() {
                return Ok(BeforeOffer::Withheld);
            }
            host.initial().map(BeforeOffer::Initial)
        })();
        if host.stopped() {
            return Ok(PreparedSetup::Unavailable);
        }
        let initial = match ready {
            Ok(BeforeOffer::Initial(initial)) => initial,
            Ok(BeforeOffer::Resume(admission)) => {
                // A startup that resumes a run an earlier process admitted (Quit & Reopen's or an
                // update's relaunch, say) may find its window with no admission of its own, so
                // the plan is offered back for the page's automatic answer, while the run goes
                // on; a window that holds its own admission lets the offer go. Only a plan with
                // aterm's own permissions needs that admission (§6).
                if admission
                    .plan
                    .selection
                    .contains(aterm_types::macos_setup::Setting::Permissions)
                {
                    host.offer_resume(&admission);
                }
                return Ok(PreparedSetup::Authorized {
                    claim,
                    revision: admission.revision,
                });
            }
            Ok(BeforeOffer::Finished) => return Ok(PreparedSetup::Finished),
            Ok(BeforeOffer::Superseded | BeforeOffer::Withheld) => {
                return Ok(PreparedSetup::Unavailable);
            }
            Err(why) => {
                drop(claim);
                if matches!(
                    report_bound_setup_read_error(host, &why)?,
                    SetupStep::Stopped
                ) {
                    return Ok(PreparedSetup::Unavailable);
                }
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
        if host.stopped() {
            return Ok(PreparedSetup::Unavailable);
        }
        match host.offer(initial) {
            WorkstationSetupDecision::Begin(plan) => {
                host.answered();
                return Ok(finish_setup_choice(host, claim, plan));
            }
            WorkstationSetupDecision::Cancelled => {
                // A person's Cancel holds until they ask again (design §3.3): it is recorded
                // before the startup ends, and a write that fails is tried again by itself. The
                // claim goes first: a choice made meanwhile answers for the Cancel.
                drop(claim);
                retry_setup_step(host, H::cancel)?;
                return Ok(PreparedSetup::Cancelled);
            }
            WorkstationSetupDecision::Closed => {
                drop(claim);
                host.closed();
                return Ok(PreparedSetup::Closed);
            }
            WorkstationSetupDecision::Unavailable => {
                drop(claim);
                match retry_bound_setup_read(host, H::finished)? {
                    SetupStep::Ready(true) => return Ok(PreparedSetup::Finished),
                    SetupStep::Ready(false) => host.backoff(),
                    SetupStep::Stopped => return Ok(PreparedSetup::Unavailable),
                }
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
    let mut attempted = false;
    loop {
        if attempted && host.stopped() {
            return PreparedSetup::Unavailable;
        }
        let Some(held) = claim.take() else {
            match host.claim() {
                Ok(Some(held)) => claim = Some(held),
                Ok(None) => match retry_bound_setup_read(host, H::finished) {
                    Ok(SetupStep::Ready(true)) => return PreparedSetup::Finished,
                    Ok(SetupStep::Ready(false)) => host.backoff(),
                    Ok(SetupStep::Stopped) | Err(_) => return PreparedSetup::Unavailable,
                },
                Err(why) => {
                    if !matches!(
                        report_bound_setup_read_error(host, &why),
                        Ok(SetupStep::Ready(()))
                    ) {
                        return PreparedSetup::Unavailable;
                    }
                    host.backoff();
                }
            }
            continue;
        };
        attempted = true;
        match host.authorize(&plan) {
            Ok(None) => {
                drop(held);
                // A changed publication already retired this ticket, but its
                // unreadable replacement may still need recovery. Readable
                // supersession never starts a new review from the old ticket.
                if host.authorization_read_failed() && !host.stopped() {
                    let _ = host.recover_after_read_error();
                }
                return PreparedSetup::Unavailable;
            }
            // Empty authorization already durably commits completion.
            Ok(Some(_)) if plan.selection.is_empty() => return PreparedSetup::Finished,
            Ok(Some(revision)) => {
                return PreparedSetup::Authorized {
                    claim: held,
                    revision,
                };
            }
            Err(why) => {
                // The first delivered Begin still saved its intent and Stop
                // evidence. A covered failure retires without another write,
                // recovery attempt, wait, or rejection note.
                if host.stopped() {
                    return PreparedSetup::Unavailable;
                }
                if host.authorization_read_failed() {
                    // Recovery may replace the bound records. No accepted review
                    // keeps the automatic claim while that replacement is made.
                    drop(held);
                    if !matches!(
                        report_bound_setup_read_error(host, &why),
                        Ok(SetupStep::Ready(()))
                    ) {
                        return PreparedSetup::Unavailable;
                    }
                    if !host.authorization_pending() || host.stopped() {
                        if !host.stopped() {
                            host.report_rejection(&why);
                        }
                        return PreparedSetup::Unavailable;
                    }
                    host.backoff();
                    continue;
                }
                if !host.authorization_pending() {
                    host.report_rejection(&why);
                    return PreparedSetup::Unavailable;
                }
                if host.stopped() {
                    return PreparedSetup::Unavailable;
                }
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
            let held_widen = register_admitted_run(revision, sent.sent, false, sent.widened_from);
            drop(sent);
            if held_widen {
                put_back_held_widen(revision);
            }
            // A run can wait on a person past its Secrets stage for as long as they take, and a
            // done Secrets stage is not run again: while the plan in force keeps the vault in
            // sync ([`vault_sync_kept`]), the mend keeps a vault subscriber whose plist runs a
            // copy that is gone (aterm.app moved since), or that this window owes the Mac, as
            // after a finished setup. Its effects take the profile lock a pass holds, so it
            // never acts beside the run's own Secrets stage.
            (setup_io().mend_subscriber)();
            // The converge pass's window lane, for the same reason: a run waiting on a person
            // ends no time soon, and one whose Secrets stage is done starts no pass of its own.
            // Its `sync.peers` leaves the join to a Secrets stage the run still owes.
            (setup_io().converge_lane)();
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
            // The run ended: the page's note that aterm widened it goes with it, before a later
            // worker the slot keeps busy (a put-back once Full Disk Access is granted) shows it.
            run_ended_widened(revision);
            if held_widen {
                put_back_held_widen(revision);
            }
        }
        PreparedSetup::Finished => {
            aterm_gui::publish_initial_setup_status(initial_setup_status());
            // A finished setup runs no Secrets stage: while the plan in force keeps the vault in
            // sync ([`vault_sync_kept`]), the mend keeps a vault subscriber whose plist runs a copy
            // that is gone, or that this window owes the Mac.
            (setup_io().mend_subscriber)();
            // The converge pass's window lane, as the mend: a finished setup runs no Secrets stage.
            (setup_io().converge_lane)();
            // On a thread of its own: its wait for the profile lock another pass holds must not
            // hold up the watch for the ALab vault that starts once this returns.
            (setup_io().start_follow_up)();
        }
        // No run here, now or later in this process (an update's successor that withholds the
        // review, a Stop, a newer choice): the window lane is this launch's converge pass beside
        // the subscriber's, each item keeping its own off switch.
        PreparedSetup::Unavailable => (setup_io().converge_lane)(),
        // The startup ends these before a run ([`run_prepared_startup_with`]).
        PreparedSetup::Cancelled | PreparedSetup::Closed => {}
    }
}

/// Whether System State's plan in force keeps this Mac's vault in sync now
/// (`aterm_cli::macos_doctor::initial_setup_keeps_vault_sync`), and no Stop pressed in this process
/// holds its run, even one too late for it (design §3.5): the vault subscriber's mend
/// (`aterm_gui::mend_secrets_subscriber`) repairs the sync process only then. A read that fails
/// keeps nothing in sync for now; the mend reads again at its next turn.
#[cfg(target_os = "macos")]
fn vault_sync_kept() -> bool {
    aterm_secrets::swarm::enabled_live().unwrap_or(false)
        && aterm_cli::macos_doctor::initial_setup_keeps_vault_sync().unwrap_or(false)
        && aterm_cli::macos_doctor::initial_setup_revision()
            .is_ok_and(|revision| !setup_stop_requested(revision))
}

/// The mend's effect boundary: the plan and its exact revision stay current
/// under the backend's profile lock, and a local late Stop still holds them.
#[cfg(target_os = "macos")]
fn while_vault_sync_kept(work: &mut dyn FnMut()) -> bool {
    while_vault_sync_kept_with(work, guard_current_vault_sync)
}

/// Adapt the generic backend guard to the mend's borrowed callback lifetime.
#[cfg(target_os = "macos")]
fn guard_current_vault_sync(work: &mut dyn FnMut(u64) -> bool) -> Result<Option<bool>, String> {
    aterm_cli::macos_doctor::with_initial_setup_vault_sync(work)
}

#[cfg(target_os = "macos")]
fn while_vault_sync_kept_with(
    work: &mut dyn FnMut(),
    guard: impl FnOnce(&mut dyn FnMut(u64) -> bool) -> Result<Option<bool>, String>,
) -> bool {
    guard(&mut |revision| {
        if setup_stop_requested(revision) {
            return false;
        }
        work();
        true
    })
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// At the launch of a window whose setup has finished, and as the window records Full Disk
/// Access granted, once this aterm holds that grant: put back what a Restore could not for want
/// of it, and clear what setup left alone in another app's container (Notification Center's
/// widgets). It logs, and a Restore it finished says so on the page. While it may hold the
/// profile lock it holds System State's busy slot, so the page offers no Apply, Check or Restore
/// that would fail on that lock; a launch with nothing owed reads only aterm's records and takes
/// no slot. Without the grant it changes and asks nothing; a try that did not verify is retried
/// at a later launch once it is due.
#[cfg(target_os = "macos")]
fn finish_withheld_preferences() {
    report_withheld_preferences(
        finish_withheld_preferences_with(
            aterm_cli::macos_doctor::finish_withheld_preferences_observed_unless_stopped,
        ),
        |report, observation| {
            remember_initial_setup_rows(report, None, observation);
            aterm_gui::publish_initial_setup_status(initial_setup_status());
        },
        restore_initial_setup_error,
    );
}

/// Keep the follow-up's setup slot until its result has been published: a later choice cannot
/// publish a newer result only to have this one replace it. The observation came from under
/// the profile lock, so an external choice since then filters this result out.
#[cfg(target_os = "macos")]
fn report_withheld_preferences<S>(
    finished: (
        aterm_cli::macos_doctor::InitialSetupObserved<Option<aterm_cli::macos_doctor::Report>>,
        Option<S>,
    ),
    restored: impl FnOnce(
        &aterm_cli::macos_doctor::Report,
        Option<aterm_cli::macos_doctor::InitialSetupObservation>,
    ),
    failed: impl FnOnce(String, Option<aterm_cli::macos_doctor::InitialSetupObservation>),
) {
    let (report, _reservation) = finished;
    // A Restore it finished says so on the page in place of what that Restore left waiting.
    match &report.result {
        Ok(Some(restored_report)) if restored_report.restore.is_some() => {
            restored(restored_report, report.observation);
        }
        Err(why) if why.starts_with(aterm_cli::macos_doctor::BACKUP_UNREAD) => {
            failed(why.clone(), report.observation);
        }
        _ => {}
    }
    match report.result {
        Ok(Some(report)) if report.needs_attention() && report.failed() => aterm_log::warn!(
            "setup follow-up with Full Disk Access: {}",
            report.attention_line()
        ),
        Ok(Some(report)) if report.needs_attention() => aterm_log::info!(
            "setup follow-up with Full Disk Access: {}",
            report.attention_line()
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
    follow_up: impl FnOnce(fn() -> WithheldReservation, fn(u64) -> bool) -> T,
) -> T {
    follow_up(
        || {
            let busy = wait_for_initial_setup_slot();
            // Tuple fields drop in order: retire and refresh the widget reading
            // before another operation can take the setup slot.
            (
                PreferenceReadings::begin(vec![aterm_types::macos_setup::Subject::Setting(
                    aterm_types::macos_setup::Setting::Widgets,
                )]),
                busy,
            )
        },
        setup_stop_requested,
    )
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
    with_setup_watch(
        true,
        || {
            with_setup_run(revision, || {
                run_setup_retry(
                    &mut LiveSetupRetry {
                        revision,
                        password,
                        last_failure: None,
                        success_reported: false,
                        wakes_seen: SETUP_WAKES.marks(),
                        park_due_seen: SetupRetryDeadlines::default(),
                        park_before_pass: None,
                        accounts_said: None,
                    },
                    claim,
                    busy,
                    revision,
                );
            })
        },
        maintain_setup_watch,
    );
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
    /// Wait a minute before the next pass, or less once a wake ends it (never a window's focus
    /// gain). `true` when a person pressed Check again meanwhile: the run's tries start afresh.
    fn backoff(&mut self) -> bool;
    /// Wait until something a parked run waits on may have moved: a person's
    /// grant, Check again, a new plan, Stop, System State coming to the front, a
    /// vault approval, a local fact the park is marked on, a try that came due, or,
    /// while an Accounts or Permissions stage is left, a window gaining focus. No pass
    /// runs meanwhile, however long that takes. `true` when what ended it starts the
    /// run's tries afresh: a fact the park is marked on changed, or a person pressed
    /// Check again. Any other end leaves spent tries spent ([`idle_after`]).
    fn park(&mut self) -> bool;
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
/// that only retried. Once that count is spent the loop parks and the count stays
/// spent, whatever wakes the park, so a pass a wake runs then says the run no
/// longer retries (System State design §3.6): only a pass that leaves nothing to
/// retry, a wait that starts the tries afresh ([`SetupRetryHost::park`],
/// [`SetupRetryHost::backoff`]), a new run or a new launch counts from nothing again.
#[cfg(any(target_os = "macos", test))]
fn idle_after(next: PassNext, retries: u32) -> (PassNext, u32) {
    match next {
        PassNext::Retry if retries + 1 < SETUP_RETRY_LIMIT => (PassNext::Retry, retries + 1),
        PassNext::Retry => (PassNext::Park, retries),
        PassNext::Park => (PassNext::Park, 0),
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
/// may last, and its failure must reach the menu line (System State design §3.6). A person's
/// Check again meanwhile starts the run's tries afresh (`retries`).
#[cfg(any(target_os = "macos", test))]
fn back_off_before_a_pass<H: SetupRetryHost>(
    host: &mut H,
    claim: &mut Option<H::Claim>,
    busy: &mut Option<H::Busy>,
    retries: &mut u32,
) {
    host.retrying(false);
    release_setup_pass(claim, busy);
    if host.backoff() {
        *retries = 0;
    }
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
                back_off_before_a_pass(host, &mut claim, &mut busy, &mut retries);
                continue;
            }
        }
        match host.revision() {
            Ok(current) if current != revision => return,
            Ok(_) => {}
            Err(why) => {
                host.failure(&why);
                back_off_before_a_pass(host, &mut claim, &mut busy, &mut retries);
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
                    back_off_before_a_pass(host, &mut claim, &mut busy, &mut retries);
                    continue;
                }
                Err(why) => {
                    host.failure(&why);
                    back_off_before_a_pass(host, &mut claim, &mut busy, &mut retries);
                    continue;
                }
            }
            match host.revision() {
                Ok(current) if current != revision => return,
                Ok(_) => {}
                Err(why) => {
                    host.failure(&why);
                    back_off_before_a_pass(host, &mut claim, &mut busy, &mut retries);
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
        let fresh = if idle == PassNext::Park {
            host.retrying(false);
            host.park()
        } else {
            host.backoff()
        };
        if fresh {
            retries = 0;
        }
    }
}

/// How long a parked run waits before it reads its mark again by itself: until its next retry
/// falls due (`next_due`, seconds since the epoch, against `now`), or with no timer at all when
/// none is ahead. A run whose facts are not all `watched` (a watch that could not start) reads
/// them at least once a minute, as before watches.
#[cfg(any(target_os = "macos", test))]
fn park_wait_limit(next_due: Option<u64>, now: u64, watched: bool) -> Option<std::time::Duration> {
    const UNWATCHED: std::time::Duration = std::time::Duration::from_secs(60);
    let due = next_due.map(|due| std::time::Duration::from_secs(due.saturating_sub(now).max(1)));
    if watched {
        due
    } else {
        Some(due.map_or(UNWATCHED, |due| due.min(UNWATCHED)))
    }
}

/// Seconds since the epoch on this Mac's clock.
#[cfg(target_os = "macos")]
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
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

/// Keep the local facts a pass began with, before it can discover that they changed.
#[cfg(any(target_os = "macos", test))]
fn with_setup_pass_mark<M, T>(
    before: &mut Option<M>,
    read: impl FnOnce() -> M,
    pass: impl FnOnce() -> T,
) -> T {
    *before = Some(read());
    pass()
}

/// A change during the last pass is ready at entry. Later comparisons use the
/// fresh parked baseline, so bookkeeping done by the pass cannot keep waking it.
#[cfg(any(target_os = "macos", test))]
fn wait_for_setup_park_change_since<M: PartialEq>(
    before_pass: Option<&M>,
    mark: impl FnMut() -> M,
    mut ready: impl FnMut(&M) -> bool,
    facts_changed: impl Fn(&M, &M) -> bool,
    still_waiting: impl FnMut() -> bool,
    wait: impl FnMut(),
) -> M {
    let mut entry = true;
    wait_for_setup_park_change(
        mark,
        |current| {
            let changed = entry && before_pass.is_some_and(|before| facts_changed(current, before));
            entry = false;
            ready(current) || changed
        },
        still_waiting,
        wait,
    )
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
    /// The last wakes this worker answered: set when a pass begins, so a wake
    /// during it is not lost, and by each backoff that a wake ended.
    wakes_seen: WakeMarks,
    /// Each source's last consumed deadline: an unchanged due attempt cannot
    /// wake every park, nor hide a newly due attempt from another source.
    park_due_seen: SetupRetryDeadlines,
    /// Local facts before the last pass: a change it refused may already be
    /// stable by the time its worker reaches the park's initial reading.
    park_before_pass: Option<aterm_cli::macos_doctor::InitialSetupParkMark>,
    /// The last accounts resync line this worker logged (`accounts_resynced`).
    accounts_said: Option<String>,
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
        self.wakes_seen = SETUP_WAKES.marks();
        let observed = with_setup_pass_mark(
            &mut self.park_before_pass,
            aterm_cli::macos_doctor::initial_setup_park_mark,
            || {
                with_setup_activity(|| {
                    aterm_cli::macos_doctor::run_initial_setup_pass(
                        self.revision,
                        &mut self.password,
                        SETUP_HOOKS,
                    )
                })
            },
        );
        let Some((report, next)) = observed
            .result
            .inspect_err(|why| initial_setup_error(why.clone(), observed.observation.clone()))?
        else {
            return Ok(PassNext::Done);
        };
        if let Ok(env) = aterm_agent::harness::cli::Env::from_process()
            && let (Some(home), Some(roster_path)) = (env.home.as_deref(), env.accounts_roster())
        {
            // A pass syncs only what changed (and writes only what differs), and hands the rows
            // it leaves with no login made here to the automatic sign-in ladder, never to a
            // person, where this instance keeps its account's own fleet accounts.
            aterm_agent::harness::accounts_sync::log_sign_ins_to(|line| {
                aterm_log::info!("harness: accounts: {line}");
            });
            let resync = aterm_agent::harness::accounts_sync::resync_for_setup(
                home,
                &env.state,
                &roster_path,
                env.now,
                aterm_types::dirs::not_own_account().is_none(),
            );
            self.accounts_resynced(&resync);
        }
        remember_initial_setup_rows(&report, None, observed.observation);
        if report.needs_attention() {
            report_first_run_doctor_attention(&mut self.last_failure, &report);
        } else if !self.success_reported {
            aterm_log::info!("first-run macOS doctor: {}", report.to_text());
            self.success_reported = true;
        }
        Ok(match next {
            aterm_cli::macos_doctor::SetupNext::Done => PassNext::Done,
            aterm_cli::macos_doctor::SetupNext::Retry => PassNext::Retry,
            aterm_cli::macos_doctor::SetupNext::Park => PassNext::Park,
        })
    }
    fn backoff(&mut self) -> bool {
        maintain_setup_watch(WatchMaintenance::Spawn);
        // A wake (Check again, Stop, a new plan, a person's grant) ends it early,
        // once: a read or a claim that keeps failing waits a minute each time. A
        // window gaining focus does not: it is no reason to try again sooner.
        let seen = self.wakes_seen;
        SETUP_WAKES.back_off_run(&mut self.wakes_seen, std::time::Duration::from_secs(60));
        self.wakes_seen.checked_since(seen)
    }
    fn park(&mut self) -> bool {
        // Local vault facts, interfaces for pending Secrets or Repositories, the
        // Command Line Tools and existing retry deadlines can change without the
        // window. Each is watched (`ParkWatch`), so the park reads its mark again
        // only when one may have moved, a retry falls due or a wake comes: a run
        // left waiting on a person alone has no timer. A window gaining focus ends
        // the park only while it waits on what a person does in System Settings or
        // macOS (an Accounts or Permissions stage left).
        use aterm_cli::macos_doctor::{ParkWatch, Watches};
        let seen = self.wakes_seen;
        let due_seen = self.park_due_seen;
        let before_pass = self.park_before_pass.take();
        let mut previous = None;
        let focus = std::cell::Cell::new(false);
        let facts = std::cell::Cell::new(SETUP_WAKES.marks().facts);
        let limit = std::cell::Cell::new(None);
        let mut watching: Option<(Watches, Option<ParkWatch>)> = None;
        let mark = wait_for_setup_park_change_since(
            before_pass.as_ref(),
            || {
                // Taken before the read: a fact that moves during it ends the next wait at once.
                facts.set(SETUP_WAKES.marks().facts);
                let mut mark = read_setup_park_mark_with(
                    aterm_cli::macos_doctor::initial_setup_park_mark,
                    || maintain_setup_watch(WatchMaintenance::Spawn),
                );
                if let Some(known) = &previous {
                    mark.retain_known_from(known);
                }
                focus.set(mark.waits_on_settings());
                let watches = mark.watches();
                if watching.as_ref().map(|(on, _)| *on) != Some(watches) {
                    // The old watch ends before the new one starts.
                    drop(watching.take());
                    let started = (!watches.none())
                        .then(|| {
                            ParkWatch::start(watches, wake_setup_facts)
                                .inspect_err(|why| {
                                    aterm_log::info!(
                                        "setup: a parked run reads its facts each minute: {why}"
                                    );
                                })
                                .ok()
                        })
                        .flatten();
                    // A fact that moved between the read and the watch starting: one more read.
                    if started.is_some() {
                        facts.set(facts.get().wrapping_sub(1));
                    }
                    watching = Some((watches, started));
                }
                // A watch that stopped hearing its fact gives the park its timer back.
                let watched = watching.as_ref().is_some_and(|(on, started)| {
                    on.none() || started.as_ref().is_some_and(ParkWatch::whole)
                }) && !setup_watch_owed();
                limit.set(park_wait_limit(mark.next_due(), unix_seconds(), watched));
                previous = Some(mark.clone());
                mark
            },
            |mark| due_seen.unseen(mark.retry_due(), mark.mac_settings_retry_due()),
            aterm_cli::macos_doctor::InitialSetupParkMark::facts_changed_since,
            || !SETUP_WAKES.marks().ends_run_wait(seen, focus.get()),
            || {
                SETUP_WAKES.wait_for_park(seen, facts.get(), focus.get(), limit.get());
            },
        );
        self.park_due_seen
            .consume(mark.retry_due(), mark.mac_settings_retry_due());
        // A fact the park is marked on that moved since the last pass began, or a person's
        // Check again, starts the run's tries afresh; a due try or any other wake does not.
        before_pass
            .as_ref()
            .is_some_and(|before| mark.facts_changed_since(before))
            || SETUP_WAKES.marks().checked_since(seen)
    }
    fn retrying(&mut self, retrying: bool) {
        set_setup_retrying(self.revision, retrying);
    }
    fn failure(&mut self, why: &str) {
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }
    fn completed(&mut self) {
        // While the plan in force keeps the vault in sync ([`vault_sync_kept`]), the mend keeps a
        // stale or owed subscriber (started once per process: a run admitted at launch started
        // it already), and System State's Secrets rows show the rest.
        (setup_io().mend_subscriber)();
        // The converge pass's window lane (started once per process: a run admitted at launch
        // started it already).
        (setup_io().converge_lane)();
    }
}

#[cfg(target_os = "macos")]
impl LiveSetupRetry {
    /// One `aterm.log` line for a pass's accounts resync that changed something or failed,
    /// unless it is the line this worker logged last; a pass that found nothing new says nothing.
    fn accounts_resynced(&mut self, resync: &aterm_agent::harness::accounts_sync::Resync) {
        let to_sign_in = if resync.to_sign_in.is_empty() {
            String::new()
        } else {
            format!(
                "; no login here yet: {}{}",
                resync.to_sign_in.join(", "),
                if resync.renewal {
                    " (a sign-in started)"
                } else {
                    ""
                }
            )
        };
        let line = match (&resync.failed, resync.changed) {
            (Some(why), _) => format!("first-run setup: accounts sync: {why}"),
            (None, true) => format!("first-run setup: accounts synced{to_sign_in}"),
            (None, false) => return,
        };
        if self.accounts_said.as_deref() != Some(line.as_str()) {
            aterm_log::info!("{line}");
            self.accounts_said = Some(line);
        }
    }
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

#[cfg(all(test, target_os = "macos"))]
mod watch_spawn_recovery_tests;

#[cfg(test)]
mod alab_watch_tests {
    use super::{
        ALAB_LOOK_FIRST, ALAB_LOOK_GAP, ALAB_LOOK_LONGEST, AlabWatch, WidenReview, alab_look_after,
        widen_on_alab_signal,
    };
    use std::collections::VecDeque;
    use std::time::Duration;

    /// A watch whose answers come from a script; the last `widens` answer sticks.
    #[derive(Default)]
    struct Scripted {
        widens: VecDeque<Option<bool>>,
        /// The review's answers; once they run out, every review is applied.
        reviews: VecDeque<WidenReview>,
        looks: VecDeque<Result<Option<u64>, String>>,
        waits: Vec<(Duration, Duration)>,
        runs: u32,
    }

    impl AlabWatch for Scripted {
        fn widens(&mut self) -> Result<bool, String> {
            if self.widens.len() > 1 {
                self.widens.pop_front().unwrap()
            } else {
                self.widens.front().copied().unwrap_or(Some(false))
            }
            .ok_or_else(|| "a pass is saving the records".to_string())
        }
        fn wait(&mut self, gap: Duration, limit: Duration) {
            self.waits.push((gap, limit));
        }
        fn review(&mut self) -> Result<WidenReview, String> {
            Ok(self.reviews.pop_front().unwrap_or(WidenReview::Applied))
        }
        fn look(&mut self) -> Result<Option<u64>, String> {
            self.looks.pop_front().expect("a look the script answers")
        }
        fn run(&mut self) {
            self.runs += 1;
        }
    }

    #[test]
    fn a_watch_looks_again_more_slowly_and_runs_the_widened_plan_once() {
        let mut watch = Scripted {
            widens: [Some(true)].into(),
            looks: [
                Ok(None),
                Err("a profile pass is running".into()),
                Ok(None),
                Ok(Some(9)),
            ]
            .into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut watch);
        let minute = Duration::from_secs(60);
        assert_eq!(
            watch.waits,
            [
                (ALAB_LOOK_GAP, ALAB_LOOK_FIRST),
                (ALAB_LOOK_GAP, minute),
                (ALAB_LOOK_GAP, 2 * minute),
            ],
            "the first look goes at once, then each waits longer"
        );
        assert_eq!(watch.runs, 1);
        assert!(watch.looks.is_empty());
    }

    /// The groups the ALab signal adds are offered as a review of their own, at once as the
    /// watch starts: the plan widens only once that review is applied. Any other answer (a
    /// group choice, a Cancel, closing it) ends the watch with nothing widened or run, and a
    /// review no window could show yet is offered again at the next look.
    #[test]
    fn a_watch_widens_only_as_its_review_admits() {
        let mut later = Scripted {
            widens: [Some(true)].into(),
            reviews: [WidenReview::NotYet].into(),
            looks: [Ok(Some(3))].into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut later);
        assert_eq!(later.waits, [(ALAB_LOOK_GAP, ALAB_LOOK_FIRST)]);
        assert_eq!(later.runs, 1);
        let mut answered = Scripted {
            widens: [Some(true)].into(),
            reviews: [WidenReview::Answered].into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut answered);
        assert!(answered.waits.is_empty(), "the review was offered at once");
        assert_eq!(answered.runs, 0, "nothing widened or run");
        assert!(answered.reviews.is_empty());
    }

    #[test]
    fn a_watch_ends_once_the_plan_is_no_longer_aterms_to_widen() {
        let mut watch = Scripted {
            widens: [Some(true), Some(true), Some(false)].into(),
            looks: [Ok(None), Ok(None)].into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut watch);
        assert_eq!(watch.waits.len(), 1);
        assert_eq!(watch.runs, 0);
        let mut unread = Scripted {
            widens: [None, Some(true), None, Some(false)].into(),
            looks: [Ok(None)].into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut unread);
        assert_eq!(
            unread.waits.len(),
            2,
            "records that cannot be read keep the watch"
        );
        assert!(
            unread.looks.is_empty(),
            "and take no look while they cannot"
        );
        let mut found_later = Scripted {
            widens: [None, None, Some(true)].into(),
            looks: [Ok(Some(4))].into(),
            ..Scripted::default()
        };
        widen_on_alab_signal(&mut found_later);
        assert_eq!(found_later.runs, 1);
        assert!(found_later.looks.is_empty());
        let mut idle = Scripted::default();
        widen_on_alab_signal(&mut idle);
        assert!(idle.waits.is_empty(), "nothing to widen, nothing to watch");
    }

    #[test]
    fn a_watch_waits_at_most_half_an_hour_between_looks() {
        assert_eq!(alab_look_after(0), ALAB_LOOK_FIRST);
        assert_eq!(alab_look_after(5), Duration::from_secs(16 * 60));
        assert_eq!(alab_look_after(6), ALAB_LOOK_LONGEST);
        assert_eq!(alab_look_after(u32::MAX), ALAB_LOOK_LONGEST);
        assert!(ALAB_LOOK_GAP < ALAB_LOOK_FIRST);
    }

    #[cfg(target_os = "macos")]
    mod recovery {
        use super::*;
        use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;
        use aterm_types::macos_setup::Selection;

        #[derive(Clone, Copy)]
        enum Replacement {
            Stop,
            OldStop(u64),
            Person,
        }

        struct RealWatch<'a> {
            setup: ProvisionalSetupWatchTest<'a>,
            faults: VecDeque<Option<(usize, bool)>>,
            replacement: Option<Replacement>,
            failed_read: bool,
            reads: usize,
            waits: usize,
            looks: usize,
            recovered: Vec<u64>,
        }

        impl<'a> RealWatch<'a> {
            fn new(root: &'a std::path::Path) -> Self {
                Self {
                    setup: ProvisionalSetupWatchTest::new(root).unwrap(),
                    faults: VecDeque::new(),
                    replacement: None,
                    failed_read: false,
                    reads: 0,
                    waits: 0,
                    looks: 0,
                    recovered: Vec::new(),
                }
            }
        }

        impl AlabWatch for RealWatch<'_> {
            fn widens(&mut self) -> Result<bool, String> {
                self.reads += 1;
                if std::mem::take(&mut self.failed_read) {
                    assert_eq!(self.looks, 0, "a failed read authorizes no probe or write");
                    let before = self.setup.snapshot().unwrap();
                    let eligible = self.setup.widens_after_failed_read();
                    let repaired = self.setup.snapshot().unwrap();
                    assert_eq!(repaired.observation, before.observation);
                    assert_eq!(repaired.permissions_allowed, before.permissions_allowed);
                    eligible
                } else {
                    self.setup.widens()
                }
            }

            fn wait(&mut self, gap: Duration, limit: Duration) {
                assert!(gap <= limit);
                self.waits += 1;
            }

            fn review(&mut self) -> Result<WidenReview, String> {
                Ok(WidenReview::Applied)
            }

            fn look(&mut self) -> Result<Option<u64>, String> {
                self.looks += 1;
                assert!(
                    self.looks <= 3,
                    "the watch must converge without a retry spin"
                );
                let upgraded = self.setup.upgrade(self.faults.pop_front().flatten());
                if let Some(replacement) = self.replacement.take() {
                    assert!(upgraded.is_err());
                    assert_eq!(
                        self.setup.snapshot().unwrap().plan.selection,
                        Selection::all()
                    );
                    match replacement {
                        Replacement::Stop => self.setup.stop().unwrap(),
                        Replacement::OldStop(revision) => {
                            self.setup.stop_revision(revision).unwrap()
                        }
                        Replacement::Person => self.setup.choose().unwrap(),
                    }
                }
                upgraded
            }

            fn run(&mut self) {
                self.recovered.push(self.setup.recover().unwrap());
                let resumed = self.setup.snapshot().unwrap();
                assert_eq!(resumed.plan.selection, Selection::all());
                assert!(!resumed.finished, "the widened stages are still owed");
                assert!(
                    resumed.permissions_allowed,
                    "recovery confirms real admission"
                );
                assert_eq!(resumed.revision, *self.recovered.last().unwrap());
            }
        }

        #[test]
        fn watch_recovery_starts_after_a_real_upgrade_publication_reports_failure() {
            for fault in [(1, false), (3, true)] {
                let root = aterm_tempfile::tempdir().unwrap();
                let mut watch = RealWatch::new(root.path());
                let original = watch.setup.snapshot().unwrap().revision;
                watch.faults.push_back(Some(fault));
                widen_on_alab_signal(&mut watch);
                assert_eq!(
                    watch.recovered.len(),
                    1,
                    "save {fault:?}: the published full run needs startup recovery in this process"
                );
                assert_eq!(
                    watch.looks, 2,
                    "retry only the owned admission after its failed publication"
                );
                assert_eq!(watch.waits, 1, "the first look goes at once");
                assert_eq!(watch.recovered[0], original + 1);
            }
        }

        #[test]
        fn watch_recovery_ignores_a_delayed_stop_of_its_completed_predecessor() {
            for fault in [(1, false), (3, false), (3, true)] {
                let root = aterm_tempfile::tempdir().unwrap();
                let mut watch = RealWatch::new(root.path());
                let original = watch.setup.snapshot().unwrap().revision;
                watch.faults.push_back(Some(fault));
                watch.replacement = Some(Replacement::OldStop(original));
                widen_on_alab_signal(&mut watch);
                assert_eq!(watch.recovered, vec![original + 1], "save {fault:?}");
                assert_eq!(watch.looks, 2, "the same owned admission must recover");
                assert_eq!(watch.waits, 1, "the first look goes at once");
            }
        }

        #[test]
        fn watch_recovery_retries_a_repaired_eligibility_read_without_granting_authority() {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut watch = RealWatch::new(root.path());
            watch.failed_read = true;
            widen_on_alab_signal(&mut watch);
            assert_eq!(
                watch.recovered.len(),
                1,
                "a transient record read must not end the only in-process ALab watch"
            );
            assert!(watch.reads >= 2);
            assert_eq!(watch.looks, 1, "probe only after eligibility is readable");
            assert!(watch.waits >= 1);
        }

        #[test]
        fn watch_recovery_keeps_prepublication_retries_and_authoritative_replacements() {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut watch = RealWatch::new(root.path());
            watch.faults.push_back(Some((0, false)));
            widen_on_alab_signal(&mut watch);
            assert_eq!(watch.recovered.len(), 1);
            assert_eq!(
                watch.looks, 2,
                "an unpublished failure may try the upgrade again"
            );

            for replacement in [Replacement::Stop, Replacement::Person] {
                let root = aterm_tempfile::tempdir().unwrap();
                let mut watch = RealWatch::new(root.path());
                watch.faults.push_back(Some((1, false)));
                watch.replacement = Some(replacement);
                widen_on_alab_signal(&mut watch);
                assert!(
                    watch.recovered.is_empty(),
                    "a later choice remains authoritative"
                );
                assert_eq!(
                    watch.looks, 2,
                    "the retained ticket confirms its retirement"
                );
                assert!(!watch.setup.widens().unwrap());
            }
        }
    }
}

#[cfg(test)]
mod setup_retry_tests {
    use super::{
        ALAB_FOCUS_FRESH, AlabFocus, LanHeard, PassNext, SETUP_RETRY_LIMIT, SetupRetryHost,
        SetupWakes, alab_network_moved, begin_check_again, idle_after, run_setup_retry,
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
        fn park(&mut self) -> bool {
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
            false
        }
        fn backoff(&mut self) -> bool {
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
            false
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
    fn retries_that_never_settle_end_in_a_park_that_keeps_them_spent() {
        let mut retries = 0;
        for pass in 1..SETUP_RETRY_LIMIT {
            let (idle, count) = idle_after(PassNext::Retry, retries);
            assert_eq!(idle, PassNext::Retry, "pass {pass} backs off");
            retries = count;
        }
        let spent = (PassNext::Park, SETUP_RETRY_LIMIT - 1);
        assert_eq!(idle_after(PassNext::Retry, retries), spent);
        assert_eq!(
            idle_after(PassNext::Retry, spent.1),
            spent,
            "a pass a wake runs once the tries are spent parks again at once"
        );
        // A pass that leaves nothing to retry ends the row of passes that only retried.
        assert_eq!(idle_after(PassNext::Park, 3), (PassNext::Park, 0));
        assert_eq!(idle_after(PassNext::Done, 3), (PassNext::Retry, 0));
    }

    /// A transient failure that never clears (GitHub not answering on this network): after ten
    /// passes the run parks and says it no longer retries. A wake that changed nothing (an
    /// access step, a try that came due, a focus gain while Permissions is left) runs one pass,
    /// which still says so, and parks again with no backoff: no wake buys ten more passes a
    /// minute apart, and what the run says never flaps back to retrying. A park that starts the
    /// tries afresh (a fact it is marked on changed, or Check again) gives the run its ten
    /// tries again, once, and so does Check again during a backoff, with nothing said twice.
    #[test]
    fn a_wake_that_changed_nothing_never_rearms_spent_tries() {
        struct Spent {
            revision: u64,
            passes: usize,
            parks: usize,
            backoffs: usize,
            /// The park, and the backoff, that start the tries afresh, if one does.
            fresh_at: (Option<usize>, Option<usize>),
            retrying: Vec<bool>,
        }
        impl SetupRetryHost for Spent {
            type Claim = ();
            type Busy = ();
            fn revision(&mut self) -> Result<u64, String> {
                Ok(self.revision)
            }
            fn finished(&mut self) -> Result<bool, String> {
                Ok(false)
            }
            fn claim(&mut self) -> Result<Option<()>, String> {
                Ok(Some(()))
            }
            fn busy(&mut self) {}
            fn register(&mut self) {}
            fn apply(&mut self) -> Result<PassNext, String> {
                self.passes += 1;
                Ok(PassNext::Retry)
            }
            fn backoff(&mut self) -> bool {
                self.backoffs += 1;
                self.fresh_at.1 == Some(self.backoffs)
            }
            fn park(&mut self) -> bool {
                // Two wakes end parks, then a new plan ends this run.
                self.parks += 1;
                if self.parks == 3 {
                    self.revision += 1;
                }
                self.fresh_at.0 == Some(self.parks)
            }
            fn retrying(&mut self, retrying: bool) {
                if self.retrying.last() != Some(&retrying) {
                    self.retrying.push(retrying);
                }
            }
            fn failure(&mut self, _: &str) {}
            fn completed(&mut self) {}
        }
        let limit = SETUP_RETRY_LIMIT as usize;
        let run = |fresh_at| {
            let mut host = Spent {
                revision: 1,
                passes: 0,
                parks: 0,
                backoffs: 0,
                fresh_at,
                retrying: Vec::new(),
            };
            run_setup_retry(&mut host, Some(()), (), 1);
            (host.passes, host.backoffs, host.retrying)
        };
        assert_eq!(
            run((None, None)),
            (limit + 2, limit - 1, vec![true, false]),
            "a wake that changed nothing re-armed the spent failure as retrying"
        );
        assert_eq!(
            run((Some(1), None)),
            (
                2 * limit + 1,
                2 * (limit - 1),
                vec![true, false, true, false]
            ),
            "a fresh park gives the run its ten tries again, once"
        );
        assert_eq!(
            run((None, Some(5))),
            (5 + limit + 2, 5 + limit - 1, vec![true, false]),
            "Check again during a backoff counts the tries from nothing"
        );
    }

    /// A window's focus gain ends a run's wait only where the run waits on what a person does in
    /// System Settings or macOS (an Accounts or Permissions stage left); the startup's waits hear
    /// every one. Any other wake ends a run's wait, and only Check again starts its tries afresh.
    #[test]
    fn a_focus_gain_ends_only_a_run_wait_that_listens_for_it() {
        use std::time::Duration;
        let wakes = SetupWakes::new();
        let seen = wakes.marks();
        wakes.raise_focus();
        assert!(
            !wakes.wait_for_run(seen, false, Duration::ZERO),
            "a park on a transient failure sleeps on"
        );
        assert!(
            wakes.wait_for_run(seen, true, Duration::ZERO),
            "a park on an Accounts or Permissions stage wakes"
        );
        let mut backoff = seen;
        assert!(
            !wakes.back_off_run(&mut backoff, Duration::ZERO),
            "a backoff between two tries waits its minute"
        );
        assert_eq!(wakes.count(), seen.all + 1, "the startup hears it");
        assert!(!wakes.marks().checked_since(seen));
        wakes.raise();
        assert!(wakes.wait_for_run(seen, false, Duration::ZERO));
        assert!(!wakes.marks().checked_since(seen), "no press");
        wakes.raise_check();
        assert!(wakes.marks().checked_since(seen));
    }

    /// One look per reason: a watch for the ALab vault looks again on this Mac's network moving
    /// and on any wake but a window's focus gain, and on a focus gain only where [`AlabFocus`]
    /// counts one; a network change wakes no run.
    #[test]
    fn a_focus_gain_wakes_the_alab_watch_only_where_it_counts() {
        use std::time::Duration;
        let wakes = SetupWakes::new();
        let mut seen = wakes.marks();
        wakes.raise_focus();
        assert!(
            !wakes.back_off_alab(&mut seen, false, Duration::ZERO),
            "a focus gain that does not count: no look"
        );
        assert_eq!(seen, wakes.marks(), "taken as heard");
        wakes.raise_focus();
        assert!(wakes.back_off_alab(&mut seen, true, Duration::ZERO));
        wakes.raise_network();
        assert!(wakes.back_off_alab(&mut seen, false, Duration::ZERO));
        wakes.raise();
        assert!(wakes.back_off_alab(&mut seen, false, Duration::ZERO));
        assert!(
            !wakes.back_off_alab(&mut seen, true, Duration::ZERO),
            "each wake once"
        );
        let mut run = wakes.marks();
        wakes.raise_network();
        assert!(
            !wakes.back_off_run(&mut run, Duration::ZERO),
            "a network change wakes only the watch"
        );
    }

    /// A focus gain is a reason to look for the ALab vault never once a look reached the LAN,
    /// always while macOS refuses aterm the LAN, and otherwise (the question asked, or nothing
    /// heard) for ten minutes after the watch first heard that answer, each new answer starting
    /// its ten minutes afresh. A Mac with no ALab vault on its network never hears the switch on,
    /// and looked at every focus gain for as long as aterm ran.
    #[test]
    fn a_focus_gain_counts_for_ten_minutes_after_an_answer_that_says_neither_on_nor_off() {
        let start = std::time::Instant::now();
        let at = |secs: u64| start + std::time::Duration::from_secs(secs);
        let fresh = ALAB_FOCUS_FRESH.as_secs();
        for heard in [LanHeard::Untold, LanHeard::Asked] {
            let mut focus = AlabFocus::new();
            assert!(focus.counts(heard, at(0)), "{heard:?}");
            assert!(focus.counts(heard, at(fresh - 1)), "{heard:?}");
            assert!(!focus.counts(heard, at(fresh)), "{heard:?}: spent");
            assert!(!focus.counts(heard, at(10 * fresh)), "{heard:?}: spent");
        }
        let mut focus = AlabFocus::new();
        assert!(focus.counts(LanHeard::Untold, at(0)));
        assert!(!focus.counts(LanHeard::Untold, at(fresh)));
        // macOS's question, asked later, starts its ten minutes from then.
        assert!(focus.counts(LanHeard::Asked, at(2 * fresh)));
        assert!(!focus.counts(LanHeard::Asked, at(3 * fresh)));
        assert!(
            !focus.counts(LanHeard::On, at(3 * fresh)),
            "the LAN reached"
        );
        assert!(focus.counts(LanHeard::Off, at(9 * fresh)), "refused");
        assert!(focus.counts(LanHeard::Off, at(90 * fresh)), "refused");
    }

    /// A change the network watch hears wakes the ALab watch only once this Mac's IPv4 subnets
    /// moved off the ones it waits on: an interface that comes and goes without moving them is
    /// no reason to ask the network.
    #[test]
    fn a_network_change_that_leaves_the_subnets_is_no_reason_to_look() {
        let waited_on = std::sync::Mutex::new(None);
        assert!(
            alab_network_moved(&waited_on, Some("192.168.4.0/24".into())),
            "no wait yet"
        );
        *waited_on.lock().unwrap() = Some(Some("192.168.4.0/24".to_owned()));
        assert!(!alab_network_moved(
            &waited_on,
            Some("192.168.4.0/24".into())
        ));
        assert!(alab_network_moved(&waited_on, Some("10.0.0.0/8".into())));
        assert!(alab_network_moved(&waited_on, None), "off every network");
        *waited_on.lock().unwrap() = Some(None);
        assert!(!alab_network_moved(&waited_on, None));
        assert!(alab_network_moved(&waited_on, Some("10.0.0.0/8".into())));
    }

    /// A park whose facts are all watched has a timer only for a retry ahead, never a zero one;
    /// a park waiting on a person alone has none. A watch that could not start leaves the park
    /// reading its facts once a minute.
    #[test]
    fn a_park_on_a_person_alone_has_no_timer() {
        use super::park_wait_limit;
        use std::time::Duration;
        assert_eq!(park_wait_limit(None, 100, true), None);
        assert_eq!(
            park_wait_limit(Some(130), 100, true),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            park_wait_limit(Some(100), 100, true),
            Some(Duration::from_secs(1)),
            "a retry the clock calls due now waits a second, never spins"
        );
        assert_eq!(
            park_wait_limit(None, 100, false),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            park_wait_limit(Some(110), 100, false),
            Some(Duration::from_secs(10))
        );
    }

    /// A watched fact moving ends a park's wait, with or without a timer, and no other wait: a
    /// run backing off a failure and the startup's waits sleep on.
    #[test]
    fn a_watched_fact_ends_only_a_parks_wait() {
        use std::time::Duration;
        static WAKES: SetupWakes = SetupWakes::new();
        let seen = WAKES.marks();
        WAKES.raise_facts();
        assert!(!WAKES.wait_for_run(seen, true, Duration::ZERO));
        let mut backoff = seen;
        assert!(!WAKES.back_off_run(&mut backoff, Duration::ZERO));
        assert!(
            !WAKES.wait(seen.all, Duration::ZERO),
            "the startup hears no fact"
        );
        assert!(WAKES.wait_for_park(seen, seen.facts, false, Some(Duration::ZERO)));
        let looked = WAKES.marks();
        assert!(
            !WAKES.wait_for_park(seen, looked.facts, false, Some(Duration::ZERO)),
            "a fact already looked at ends no later wait"
        );
        let (told, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            told.send(WAKES.wait_for_park(seen, looked.facts, false, None))
                .unwrap();
        });
        WAKES.raise_facts();
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(60)),
            Ok(true),
            "a park with no timer wakes on a watched fact"
        );
    }

    /// A wake lock a panicking holder poisoned still counts and still waits: a park with no
    /// timer sleeps until a fact moves, never returning at once to read its mark again in a spin.
    #[test]
    fn a_poisoned_wake_lock_still_waits_for_a_fact() {
        use std::time::Duration;
        static WAKES: SetupWakes = SetupWakes::new();
        let _ = std::thread::spawn(|| {
            let _held = WAKES.count.lock();
            panic!("a holder panics");
        })
        .join();
        assert!(WAKES.count.is_poisoned());
        let seen = WAKES.marks();
        let (told, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            told.send(WAKES.wait_for_park(seen, seen.facts, false, None))
                .unwrap();
        });
        WAKES.raise_facts();
        assert_eq!(heard.recv_timeout(Duration::from_secs(60)), Ok(true));
        assert_ne!(WAKES.marks().facts, seen.facts, "the fact was counted");
    }

    #[derive(Clone, Copy)]
    enum RouteChange {
        WhileParked,
        DuringLastPass,
        None,
    }

    fn retry_with_route_change(pending_secrets: bool, change: RouteChange) {
        use super::{wait_for_setup_park_change_since, with_setup_pass_mark};
        use std::cell::Cell;

        struct Routed {
            inner: Fake,
            pending_secrets: bool,
            change: RouteChange,
            network: Cell<u8>,
            before_pass: Option<(u64, Option<u8>)>,
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
                let result = with_setup_pass_mark(
                    &mut self.before_pass,
                    || (1, self.pending_secrets.then(|| self.network.get())),
                    || {
                        let result = self.inner.apply();
                        if matches!(self.change, RouteChange::DuringLastPass)
                            && self.inner.state.borrow().applied.len() == SETUP_RETRY_LIMIT as usize
                        {
                            self.network.set(2);
                        }
                        result
                    },
                );
                if self.inner.parks != 0 {
                    // The route or explicit wake let the next pass finish.
                    self.inner.state.borrow_mut().finished = true;
                }
                result
            }
            fn backoff(&mut self) -> bool {
                self.inner.backoff()
            }
            fn park(&mut self) -> bool {
                self.inner.park();
                let state = Rc::clone(&self.inner.state);
                let wakes = Cell::new(0);
                wait_for_setup_park_change_since(
                    self.before_pass.take().as_ref(),
                    // Membership remains Unconfigured. Pending network work
                    // adds a route to the CLI's opaque park mark.
                    || (1, self.pending_secrets.then(|| self.network.get())),
                    |_| false,
                    |current, before| current != before,
                    || wakes.get() == 0,
                    || {
                        self.waits += 1;
                        assert!(
                            !matches!(self.change, RouteChange::DuringLastPass),
                            "the fact changed inside the last pass; do not wait for another change"
                        );
                        assert!(self.waits <= 3, "the wake must end this wait");
                        let mut state = state.borrow_mut();
                        assert!(!state.claimed && !state.busy);
                        assert_eq!(state.applied.len(), SETUP_RETRY_LIMIT as usize);
                        if matches!(self.change, RouteChange::None) {
                            // An unchanged failure stays parked. Stop ends the
                            // inert run without another setup attempt.
                            state.finished = true;
                            wakes.set(1);
                        } else if self.waits == 2 {
                            self.network.set(2);
                        } else if self.waits == 3 {
                            // A permissions-only run ignores that route. Its
                            // unchanged mark waits until an explicit wake.
                            assert!(!self.pending_secrets);
                            wakes.set(1);
                        }
                    },
                );
                false
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

        let (mut inner, claim, busy) = fixture();
        inner.backoff_limit = SETUP_RETRY_LIMIT as usize;
        let mut host = Routed {
            inner,
            pending_secrets,
            change,
            network: Cell::new(1),
            before_pass: None,
            waits: 0,
        };
        run_setup_retry(&mut host, Some(claim), busy, 1);
        let (waits, extra_pass) = match change {
            RouteChange::WhileParked => (if pending_secrets { 2 } else { 3 }, 1),
            RouteChange::DuringLastPass => (0, 1),
            RouteChange::None => (1, 0),
        };
        assert_eq!(host.waits, waits);
        assert_eq!(host.inner.parks, 1);
        assert_eq!(host.inner.backoffs, SETUP_RETRY_LIMIT as usize - 1);
        assert_eq!(
            host.inner.state.borrow().applied.len(),
            SETUP_RETRY_LIMIT as usize + extra_pass
        );
        assert_eq!(host.inner.completions, 1);
    }

    #[test]
    fn a_route_change_wakes_a_bounded_retry_run_without_another_pass_while_parked() {
        for pending_secrets in [true, false] {
            retry_with_route_change(pending_secrets, RouteChange::WhileParked);
        }
    }

    #[test]
    fn late_setup_facts_changed_inside_the_last_pass_wake_before_waiting() {
        retry_with_route_change(true, RouteChange::DuringLastPass);
    }

    #[test]
    fn late_setup_facts_unchanged_still_park_at_the_ten_pass_limit() {
        retry_with_route_change(true, RouteChange::None);
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

/// A pass that left rows needing attention, said once until it changes: its verdict and those
/// rows alone ([`aterm_cli::macos_doctor::Report::attention_line`]), a warning only where
/// aterm's own step failed. A row that waits on a person, a vault or a service is a not-yet.
#[cfg(target_os = "macos")]
fn report_first_run_doctor_attention(
    last_failure: &mut Option<String>,
    report: &aterm_cli::macos_doctor::Report,
) {
    let line = report.attention_line();
    if report.failed() {
        report_first_run_doctor_failure(last_failure, &line);
    } else if last_failure.as_deref() != Some(line.as_str()) {
        aterm_log::info!("first-run macOS doctor: {line}");
        *last_failure = Some(line);
    }
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
#[cfg(any(target_os = "macos", test))]
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
        wake_setup();
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

/// The admission's retry wait, separate from the operation that failed.
#[cfg(target_os = "macos")]
fn wait_before_admission_retry(
    sent: u64,
    seen: &mut u64,
    limit: std::time::Duration,
    wait: impl FnOnce(&mut u64, std::time::Duration),
) {
    if !stop_covers(sent) {
        wait(seen, limit);
    }
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

/// The admitted revision a watch widened when it heard the ALab vault ([`Widened`]), or 0: the
/// last run to register says whether it is one ([`register_admitted_run`]), published as
/// `SetupStatus::widened`, so the page says why its groups changed while that run goes.
#[cfg(target_os = "macos")]
static SETUP_WIDENED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run `revision`'s worker ended: if aterm widened it, the page no longer says so
/// ([`SETUP_WIDENED`]). A later run's mark stays.
#[cfg(target_os = "macos")]
fn run_ended_widened(revision: u64) {
    if SETUP_WIDENED
        .compare_exchange(
            revision,
            0,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

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
    /// The newest exact run or staged checkpoint confirmed here. Retirement
    /// releases its guard, but a queued Stop still names its published target.
    /// This evidence alone owes no save and authorizes no work.
    last_registered: Option<u64>,
    /// The newest exact Stop handed here, retained across failed saves and owner retirement.
    requested: Option<u64>,
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
    under_way: Vec<AdmissionDebt>,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct AdmissionDebt {
    sent: u64,
    /// Only a target whose actual publication the owning ticket confirmed.
    target: Option<u64>,
    /// An unidentified mutation cannot be called unpublished when its worker retires.
    unconfirmed: bool,
    /// A live guard still owns the admission, even if a newer choice superseded its writes.
    owned: bool,
}

#[cfg(target_os = "macos")]
impl StopSaves {
    /// What a quit still waits for, a Stop covering admissions through
    /// `covers` ([`STOP_COVERS`]): a save going or handed to the worker, or an
    /// admission the Stop covers that has not registered its run yet.
    fn owed(&self, covers: u64) -> bool {
        self.saving.is_some()
            || self.wanted.is_some()
            || self
                .requested
                .is_some_and(|requested| self.saved.is_none_or(|saved| saved < requested))
            || self.under_way.iter().any(|debt| {
                debt.sent <= covers
                    && (debt.unconfirmed
                        || debt
                            .target
                            .is_none_or(|target| self.saved.is_none_or(|saved| saved < target)))
            })
    }

    fn covered_target(&self, covers: u64) -> Option<u64> {
        self.under_way
            .iter()
            .filter(|debt| debt.sent <= covers)
            .filter_map(|debt| debt.target)
            .max()
    }
}

#[cfg(target_os = "macos")]
static STOP_SAVES: std::sync::Mutex<StopSaves> = std::sync::Mutex::new(StopSaves {
    last_registered: None,
    requested: None,
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
        stop_saves().under_way.push(AdmissionDebt {
            sent,
            target: None,
            unconfirmed: false,
            owned: true,
        });
        Self(sent)
    }

    /// A new preparatory mutation is unresolved even when an older target's
    /// Stop is saved. Keep that target as fallback until this publication ends.
    fn begin_publication(&self) -> bool {
        let mut saves = stop_saves();
        let debt = saves
            .under_way
            .iter_mut()
            .find(|debt| debt.sent == self.0)
            .expect("the publication guard owns its admission");
        std::mem::replace(&mut debt.unconfirmed, true)
    }

    fn restore_publication(&self, uncertain: bool) -> bool {
        let retained = if let Some(debt) = stop_saves()
            .under_way
            .iter_mut()
            .find(|debt| debt.sent == self.0)
        {
            // Retirement during the attempt wins over its earlier fallback.
            debt.unconfirmed &= uncertain;
            debt.unconfirmed
        } else {
            false
        };
        STOP_SAVES_MOVED.notify_all();
        retained
    }

    /// A write attempt's evidence for Stop, never permission to execute or write again.
    fn record_publication(&self, publication: Result<Option<u64>, String>) {
        match publication {
            Ok(Some(target)) => self.confirm_target(target, None),
            result => {
                if let Some(debt) = stop_saves()
                    .under_way
                    .iter_mut()
                    .find(|debt| debt.sent == self.0)
                {
                    debt.unconfirmed = result.is_err();
                }
                STOP_SAVES_MOVED.notify_all();
            }
        }
    }

    fn published(&self, from: u64, target: u64) {
        self.confirm_target(target, Some(from));
    }

    fn confirm_target(&self, target: u64, widened_from: Option<u64>) {
        // Prepare the background saver before publishing a target the UI can stop.
        start_stop_worker();
        // The same lock order as a Stop: registration, in-memory Stop, then debt.
        let _run = SETUP_RUN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stopped = widened_from.is_some_and(setup_stop_requested);
        let covered = {
            let mut saves = stop_saves();
            let covered = stop_covers(self.0)
                || (widened_from.is_some()
                    && (stopped || saves.under_way.iter().any(|debt| stop_covers(debt.sent))));
            if let Some(debt) = saves.under_way.iter_mut().find(|debt| debt.sent == self.0) {
                debt.target = Some(target);
                debt.unconfirmed = false;
            }
            saves.last_registered = Some(
                saves
                    .last_registered
                    .map_or(target, |last| last.max(target)),
            );
            covered
        };
        if covered {
            save_setup_stop(target);
        }
        STOP_SAVES_MOVED.notify_all();
    }
}

#[cfg(target_os = "macos")]
impl Drop for UnderWay {
    fn drop(&mut self) {
        stop_saves().under_way.retain_mut(|debt| {
            if debt.sent != self.0 {
                return true;
            }
            debt.owned = false;
            debt.unconfirmed
        });
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

/// Wakes a parked or backing-off run: raised by Check again, Stop, a new plan, a
/// reading that finds a row a person fixed, and one of aterm's windows gaining focus.
#[cfg(target_os = "macos")]
static SETUP_WAKES: SetupWakes = SetupWakes::new();

/// The wakes so far: all of them, and among them the focus gains of aterm's windows and the
/// presses of Check again, which a run's wait tells apart ([`SetupWakes::wait_for_run`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WakeMarks {
    all: u64,
    focus: u64,
    checks: u64,
    /// A fact a parked run watches may have moved ([`SetupWakes::raise_facts`]): counted apart
    /// from `all`, so only a park's wait ends on it ([`SetupWakes::wait_for_park`]).
    facts: u64,
    /// This Mac's network changed while a watch for the ALab vault waited
    /// ([`SetupWakes::raise_network`]): counted apart from `all`, so only that watch's wait ends
    /// on it ([`WakeMarks::ends_alab_wait`]).
    network: u64,
}

#[cfg(any(target_os = "macos", test))]
impl WakeMarks {
    /// Whether a wake since `seen` ends a setup run's wait: any wake but a window's focus gain,
    /// and a focus gain only where the run waits on what a person does in System Settings or
    /// macOS (`focus`, an Accounts or Permissions stage left). A focus gain is no reason to try a
    /// transient failure again (System State design §3.5).
    fn ends_run_wait(self, seen: Self, focus: bool) -> bool {
        let others = |marks: Self| marks.all.wrapping_sub(marks.focus);
        others(self) != others(seen) || (focus && self.focus != seen.focus)
    }

    /// Whether a wake since `seen` is a reason to look for the ALab vault again: this Mac's
    /// network moving, any other wake but a window's focus gain, and a focus gain only where
    /// `focus` says one is ([`AlabFocus`]).
    fn ends_alab_wait(self, seen: Self, focus: bool) -> bool {
        self.network != seen.network || self.ends_run_wait(seen, focus)
    }

    /// Whether a person pressed Check again since `seen`: the run's tries start afresh.
    fn checked_since(self, seen: Self) -> bool {
        self.checks != seen.checks
    }
}

/// A count of wakes and the condition a parked or backing-off worker waits on.
#[cfg(any(target_os = "macos", test))]
struct SetupWakes {
    count: std::sync::Mutex<WakeMarks>,
    raised: std::sync::Condvar,
}

#[cfg(any(target_os = "macos", test))]
impl SetupWakes {
    const fn new() -> Self {
        Self {
            count: std::sync::Mutex::new(WakeMarks {
                all: 0,
                focus: 0,
                checks: 0,
                facts: 0,
                network: 0,
            }),
            raised: std::sync::Condvar::new(),
        }
    }

    fn count(&self) -> u64 {
        self.marks().all
    }

    fn marks(&self) -> WakeMarks {
        *self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn raise(&self) {
        self.raise_with(|_| {});
    }

    /// One of aterm's windows gained focus.
    fn raise_focus(&self) {
        self.raise_with(|marks| marks.focus = marks.focus.wrapping_add(1));
    }

    /// A person pressed Check again.
    fn raise_check(&self) {
        self.raise_with(|marks| marks.checks = marks.checks.wrapping_add(1));
    }

    /// A fact a parked run watches may have moved (`aterm_cli::macos_doctor::ParkWatch`): the
    /// park looks at its mark again and runs a pass only if a fact changed. No other wait ends.
    fn raise_facts(&self) {
        let mut marks = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks.facts = marks.facts.wrapping_add(1);
        drop(marks);
        self.raised.notify_all();
    }

    /// This Mac's network changed (`aterm_cli::macos_doctor::ParkWatch`'s network watch, while
    /// a watch for the ALab vault waits): only that watch's wait ends ([`WakeMarks::network`]).
    fn raise_network(&self) {
        let mut marks = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks.network = marks.network.wrapping_add(1);
        drop(marks);
        self.raised.notify_all();
    }

    fn raise_with(&self, also: impl FnOnce(&mut WakeMarks)) {
        let mut marks = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks.all = marks.all.wrapping_add(1);
        also(&mut marks);
        drop(marks);
        self.raised.notify_all();
    }

    /// Wait until a wake after `seen`, or `limit` passes. `true` when woken.
    fn wait(&self, seen: u64, limit: std::time::Duration) -> bool {
        self.wait_while(Some(limit), |marks| marks.all == seen)
    }

    /// A setup run's wait: until a wake since `seen` that ends it ([`WakeMarks::ends_run_wait`]),
    /// or `limit` passes. `true` when woken.
    fn wait_for_run(&self, seen: WakeMarks, focus: bool, limit: std::time::Duration) -> bool {
        self.wait_while(Some(limit), |marks| !marks.ends_run_wait(seen, focus))
    }

    /// A parked run's wait for its next look: [`Self::wait_for_run`]'s wakes, or a watched fact's
    /// since `facts` ([`Self::raise_facts`]), or `limit` (`None`: no timer at all).
    fn wait_for_park(
        &self,
        seen: WakeMarks,
        facts: u64,
        focus: bool,
        limit: Option<std::time::Duration>,
    ) -> bool {
        self.wait_while(limit, |marks| {
            !marks.ends_run_wait(seen, focus) && marks.facts == facts
        })
    }

    fn wait_while(
        &self,
        limit: Option<std::time::Duration>,
        mut waiting: impl FnMut(&WakeMarks) -> bool,
    ) -> bool {
        // The marks are plain counters, whole even if a holder panicked: a poisoned lock still
        // waits, or a wait with no timer would return at once and its park would spin.
        let marks = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match limit {
            Some(limit) => {
                let (marks, waited) = self
                    .raised
                    .wait_timeout_while(marks, limit, |marks| waiting(marks))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(marks);
                !waited.timed_out()
            }
            None => {
                drop(
                    self.raised
                        .wait_while(marks, |marks| waiting(marks))
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                true
            }
        }
    }

    /// A retry's backoff: wait for a wake after `seen`, or `limit`, then take
    /// every wake so far as answered, so the next backoff waits again rather
    /// than returning at once on the wake this one returned on. `true` when woken.
    fn back_off(&self, seen: &mut u64, limit: std::time::Duration) -> bool {
        let woken = self.wait(*seen, limit);
        *seen = self.count();
        woken
    }

    /// A setup run's backoff between two tries: as [`Self::back_off`], but a window's focus gain
    /// does not end it ([`WakeMarks::ends_run_wait`]): the next try comes a minute later.
    fn back_off_run(&self, seen: &mut WakeMarks, limit: std::time::Duration) -> bool {
        let woken = self.wait_for_run(*seen, false, limit);
        *seen = self.marks();
        woken
    }

    /// A watch for the ALab vault's wait between two looks: as [`Self::back_off`], but only on a
    /// wake that is a reason to look again ([`WakeMarks::ends_alab_wait`], `focus` whether a
    /// window's focus gain is one).
    fn back_off_alab(&self, seen: &mut WakeMarks, focus: bool, limit: std::time::Duration) -> bool {
        let from = *seen;
        let woken = self.wait_while(Some(limit), |marks| !marks.ends_alab_wait(from, focus));
        *seen = self.marks();
        woken
    }
}

/// The person's latest plan change: a queued one that a later change overtook is dropped.
#[cfg(target_os = "macos")]
static SETUP_APPLY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A person's press waiting for another setup that holds this Mac's setup records (another
/// aterm, `aterm doctor` in a terminal): the choice's number ([`SETUP_APPLY_GENERATION`]), or
/// [`RESTORE_QUEUED`] for Restore previous settings, or 0. Published as `SetupStatus::queued`:
/// the page keeps the press showing and says why in one calm line, and the press is applied
/// when that setup finishes (System State design §3.3, §3.4). A later choice is the person's
/// latest intent: it overtakes the one waiting, which is let go.
#[cfg(target_os = "macos")]
static SETUP_QUEUED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`SETUP_QUEUED`]'s mark for a Restore, which no choice's number reaches.
#[cfg(target_os = "macos")]
const RESTORE_QUEUED: u64 = u64::MAX;

/// How long a waiting press waits between two asks of the setup that holds the records.
#[cfg(target_os = "macos")]
const QUEUED_ASK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Mark `mark`'s press as waiting for another setup ([`SETUP_QUEUED`]), publishing the status
/// when that changed.
#[cfg(target_os = "macos")]
fn queue_setup_press(mark: u64) {
    if SETUP_QUEUED.swap(mark, std::sync::atomic::Ordering::AcqRel) != mark {
        aterm_log::info!("System State: a press waits for another setup to finish");
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

/// `mark`'s press waits no more (admitted, let go, or done); another press's mark stays.
#[cfg(target_os = "macos")]
fn unqueue_setup_press(mark: u64) {
    if SETUP_QUEUED
        .compare_exchange(
            mark,
            0,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

/// Wait [`QUEUED_ASK_EVERY`] before a waiting press asks again, looking every quarter of it
/// whether it was `let_go` (a later choice, a Stop): `false` then.
#[cfg(target_os = "macos")]
fn wait_while_queued(let_go: impl Fn() -> bool) -> bool {
    for _ in 0..4 {
        if let_go() {
            return false;
        }
        std::thread::sleep(QUEUED_ASK_EVERY / 4);
    }
    !let_go()
}

#[cfg(target_os = "macos")]
const SETUP_HOOKS: aterm_cli::macos_doctor::SetupHooks = aterm_cli::macos_doctor::SetupHooks {
    stop: setup_stop_requested,
    stage: setup_stage,
    on_screen: aterm_types::macos_setup::system_state_front,
    developer_tools: aterm_gui::offer_developer_tools_install,
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
    /// Watch this Mac's vault subscriber once setup is finished
    /// (`aterm_gui::mend_secrets_subscriber`).
    mend_subscriber: fn(),
    /// Start the converge pass's window lane: whatever the launch's setup does, and once a run
    /// a person's choice admitted completed (`aterm_gui::start_converge_lane`, once per process).
    converge_lane: fn(),
    /// Start one converge pass for the Secrets stage that just ended
    /// (`aterm_gui::converge_after_secrets_stage`).
    converge_setup: fn(),
    /// Put back the plan a watch widened when a Stop held its run
    /// (`aterm_cli::macos_doctor::put_back_held_initial_setup`).
    put_back: fn(u64) -> Result<bool, String>,
    /// Start the follow-up with Full Disk Access on a thread of its own, as the window records
    /// the grant ([`start_withheld_follow_up`]).
    start_follow_up: fn(),
    /// Bring forward a setup a Restore left with no admitted run to carry aterm's own
    /// permissions (`aterm_cli::macos_doctor::bring_forward_restored_setup`): a run that waits
    /// for the startup's offer, nothing, or a profile lock another pass holds.
    bring_forward: fn() -> Result<aterm_cli::macos_doctor::BroughtForward, String>,
    /// Check again's look at the LAN, on the reading's worker
    /// (`aterm_cli::macos_doctor::probe_local_network_again`).
    probe_lan: fn(),
    /// Run the startup again on a thread of its own, as a launch runs it ([`start_startup_again`]).
    start_startup: fn() -> bool,
    /// Continue startup on a worker that has released its previous operation's leases.
    start_startup_inline: fn(),
    /// Whether another setup holds this Mac's setup records now
    /// (`aterm_cli::macos_doctor::setup_records_held`).
    records_held: fn() -> bool,
    /// The vault's materialization mark, a local reading that asks nothing of the network
    /// (`aterm_secrets::client::materialization_mark`): it moves when a token renewed in the
    /// vault reaches this Mac. `None` when there is no reading.
    vault_mark: fn() -> Option<aterm_secrets::client::MaterializationMark>,
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
        mend_subscriber: aterm_gui::mend_secrets_subscriber,
        converge_lane: aterm_gui::start_converge_lane,
        converge_setup: aterm_gui::converge_after_secrets_stage,
        put_back: aterm_cli::macos_doctor::put_back_held_initial_setup,
        start_follow_up: start_withheld_follow_up,
        bring_forward: aterm_cli::macos_doctor::bring_forward_restored_setup,
        probe_lan: aterm_cli::macos_doctor::probe_local_network_again,
        start_startup: start_startup_again,
        start_startup_inline: || run_startup_setup(None),
        records_held: aterm_cli::macos_doctor::setup_records_held,
        vault_mark: || {
            aterm_secrets::client::Paths::live()
                .ok()
                .and_then(|paths| aterm_secrets::client::materialization_mark(&paths).ok())
                .flatten()
        },
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

/// One of aterm's windows gained focus: a wake that ends a run's wait only where the run waits
/// on what a person does in System Settings or macOS ([`WakeMarks::ends_run_wait`]).
#[cfg(target_os = "macos")]
fn wake_setup_on_focus() {
    SETUP_WAKES.raise_focus();
}

/// A person pressed Check again: a wake that starts a parked run's tries afresh, and lifts
/// every stage the run's ledger parked (aterm stopped twice in it, or it ran past the hang
/// limit) before the wake, so the pass it wakes runs them (design §6.5).
#[cfg(target_os = "macos")]
fn wake_setup_for_check() {
    if let Err(why) = aterm_cli::macos_doctor::retry_parked_setup_stages() {
        aterm_log::warn!("Check again could not lift the parked setup stages: {why}");
    }
    SETUP_WAKES.raise_check();
}

/// A fact a parked run watches may have moved: the secrets folder, the network, the Command Line
/// Tools (`aterm_cli::macos_doctor::ParkWatch`).
#[cfg(target_os = "macos")]
fn wake_setup_facts() {
    SETUP_WAKES.raise_facts();
}

/// This Mac's network changed while a watch for the ALab vault waited ([`LiveAlabWatch`]): a
/// wake once the subnets moved off the ones it waits on ([`alab_network_moved`]).
#[cfg(target_os = "macos")]
fn wake_setup_network() {
    if alab_network_moved(&ALAB_WAIT_NETWORK, aterm_secrets::heard::network()) {
        SETUP_WAKES.raise_network();
    }
}

/// What the setup worker hears from the window, registered once before the GUI
/// starts: a run parked on System State (the Accounts stage) resumes when the
/// page comes to the front, and an access step the window recorded (handed,
/// declined, triggered, granted) moves the Access rows and a run parked on the
/// Permissions stage at once (System State design §6). A caller that timed out
/// behind another dialog also retries once its last guard ends. A window gaining
/// focus wakes the startup's waits, and a run's park only while an Accounts or
/// Permissions stage is left.
#[cfg(target_os = "macos")]
fn watch_setup_signals() {
    aterm_gui::on_window_focus_gained(wake_setup_on_focus);
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
    access: aterm_types::macos_setup::Access,
    outcome: aterm_types::macos_setup::AccessOutcome,
) {
    // Local Network's step is kept with its time, so it outranks an older answer a look kept.
    aterm_cli::macos_doctor::initial_setup_access_step(access, outcome);
    access_fact_observed(access, outcome);
}

/// The window's own grant is a completed fact even while the setup permission gate is held:
/// it admits no prompt and creates no access record, but a waiting Restore can continue.
#[cfg(target_os = "macos")]
fn verified_full_disk_access_granted() {
    access_fact_observed(
        aterm_types::macos_setup::Access::FullDiskAccess,
        aterm_types::macos_setup::AccessOutcome::Granted,
    );
}

#[cfg(target_os = "macos")]
fn access_fact_observed(
    access: aterm_types::macos_setup::Access,
    outcome: aterm_types::macos_setup::AccessOutcome,
) {
    use aterm_types::macos_setup::{Access, AccessOutcome, Subject};
    request_observation(None, Access::ALL.map(Subject::Access).to_vec(), false);
    wake_setup();
    // Full Disk Access granted: what a Restore or setup left in another app's container for
    // want of it goes in now (design §3.4), not at the next launch. A run going does it at its
    // pass's end; this worker then finds nothing owed, or waits for the profile lock that pass,
    // or another process's, holds, and goes on once it is let go.
    if access == Access::FullDiskAccess && outcome == AccessOutcome::Granted {
        (setup_io().start_follow_up)();
    }
}

/// The live [`SetupIo::start_follow_up`]: [`finish_withheld_preferences`] on a thread of its
/// own, which reads aterm's records and, with nothing owed, takes no slot and changes nothing.
#[cfg(target_os = "macos")]
fn start_withheld_follow_up() {
    if let Err(why) = std::thread::Builder::new()
        .name("aterm-setup-follow-up".into())
        .spawn(finish_withheld_preferences)
    {
        aterm_log::warn!("setup follow-up with Full Disk Access did not start: {why}");
    }
}

#[cfg(target_os = "macos")]
fn setup_stop_requested(revision: u64) -> bool {
    SETUP_STOP.lock().is_ok_and(|stop| *stop == Some(revision))
        || stop_saves().requested == Some(revision)
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
    if ends_secrets_stage(&event) {
        (setup_io().converge_setup)();
    }
}

/// Whether `event` is the end of the Secrets stage, the one stage whose settings name
/// [`Setting::Secrets`]: the converge pass runs as it ends (design §8.5's first-launch Secrets
/// stage), not as it starts and not at another stage's end.
#[cfg(target_os = "macos")]
fn ends_secrets_stage(event: &aterm_cli::macos_doctor::StageEvent<'_>) -> bool {
    !event.running
        && event
            .settings
            .contains(&aterm_types::macos_setup::Setting::Secrets)
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
impl ObserveRequest {
    fn include_subjects(&mut self, subjects: Vec<aterm_types::macos_setup::Subject>) {
        for subject in subjects {
            if !self.subjects.contains(&subject) {
                self.subjects.push(subject);
            }
        }
    }

    fn compatible_plan(&self, other: &Self) -> bool {
        self.plan.is_none() || other.plan.is_none() || self.plan == other.plan
    }
}

#[cfg(target_os = "macos")]
struct ObserveQueue {
    running: bool,
    /// Latest ordinary refresh, kept apart from a Check of another source.
    next: Option<ObserveRequest>,
    /// Latest explicit Check. Its scope never transfers to another plan.
    check: Option<ObserveRequest>,
}

#[cfg(target_os = "macos")]
impl ObserveQueue {
    fn enqueue(&mut self, mut request: ObserveRequest) {
        if let Some(pending) = self.next.as_mut().or(self.check.as_mut()) {
            if request.choices < pending.choices {
                // An old request can add rows to read, but cannot restore its
                // plan or its broader personal/network scope.
                pending.include_subjects(request.subjects);
                return;
            }
            if request.choices > pending.choices {
                // A person's newer choice retires both pending intents.
                for previous in self.take_pending().into_iter().flatten() {
                    request.include_subjects(previous.subjects);
                }
            }
        }
        let slot = if request.forced {
            &mut self.check
        } else {
            &mut self.next
        };
        if let Some(previous) = slot.take() {
            if request.plan.is_none() {
                request.plan = previous.plan;
            }
            request.include_subjects(previous.subjects);
        }
        if request.forced {
            if self
                .next
                .as_ref()
                .is_some_and(|next| request.compatible_plan(next))
            {
                let next = self.next.take().unwrap();
                if request.plan.is_none() {
                    request.plan = next.plan;
                }
                request.include_subjects(next.subjects);
            }
            self.check = Some(request);
        } else if let Some(check) = self
            .check
            .as_mut()
            .filter(|check| request.compatible_plan(check))
        {
            if check.plan.is_none() {
                check.plan = request.plan;
            }
            check.include_subjects(request.subjects);
        } else {
            self.next = Some(request);
        }
    }

    /// Dispatch one finite batch: the current-source refresh first, then the
    /// explicit Check. New ordinary arrivals cannot postpone that Check.
    fn take_pending(&mut self) -> [Option<ObserveRequest>; 2] {
        [self.next.take(), self.check.take()]
    }
}

#[cfg(target_os = "macos")]
static OBSERVE_QUEUE: std::sync::Mutex<ObserveQueue> = std::sync::Mutex::new(ObserveQueue {
    running: false,
    next: None,
    check: None,
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
/// slot. Requests made while one runs coalesce by ordinary refresh or explicit Check.
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
    queue_observation_with(request, |job| {
        std::thread::Builder::new()
            .name("aterm-setup-observe".into())
            .spawn(job)
            .map(|_| ())
    })
}

#[cfg(target_os = "macos")]
type ObservationJob = Box<dyn FnOnce() + Send>;

#[cfg(target_os = "macos")]
fn queue_observation_with(
    request: ObserveRequest,
    spawn: impl FnOnce(ObservationJob) -> std::io::Result<()>,
) -> bool {
    let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
        return false;
    };
    if queue.running {
        queue.enqueue(request);
        return true;
    }
    queue.running = true;
    // Admit another caller only after this worker exists. A failed spawn
    // cannot leave a concurrently acknowledged request without a worker.
    let spawned = spawn(Box::new(move || {
        let mut batch = [Some(request), None];
        loop {
            for request in batch.into_iter().flatten() {
                observe_rows(request);
            }
            let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
                return;
            };
            batch = queue.take_pending();
            if batch.iter().all(Option::is_none) {
                queue.running = false;
                return;
            }
        }
    }));
    queue.running = spawned.is_ok();
    drop(queue);
    if let Err(why) = spawned {
        aterm_log::warn!("System State could not read its rows: {why}");
        return false;
    }
    true
}

/// Read, keep what no stage changed meanwhile, and publish the new readings.
#[cfg(target_os = "macos")]
fn observe_rows(request: ObserveRequest) {
    use aterm_types::macos_setup::{Observation, Setting};
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
    // Check again reads Local Network as macOS answers now: a look at the LAN first, which
    // never puts macOS's question on screen.
    if forced
        && subjects.contains(&aterm_types::macos_setup::Subject::Access(
            aterm_types::macos_setup::Access::LocalNetwork,
        ))
    {
        (setup_io().probe_lan)();
    }
    if network
        && subjects.contains(&aterm_types::macos_setup::Subject::Setting(
            Setting::Repositories,
        ))
    {
        // Before the read: a token that arrives during it moves the mark past this one.
        token_mark_moved(|| true);
    }
    let started = SETUP_ACTIVITY.lock().map_or(0, |activity| activity.epoch);
    let read = std::panic::catch_unwind(|| (setup_io().read)(&plan, &subjects, network))
        .unwrap_or_else(|_| {
            // Do not keep a previous success as this reading's result, nor
            // retry the failed batch. The worker still drains accepted work;
            // publication below retains its choice, source and stage fences.
            aterm_log::warn!("System State's row reading unwound; the requested rows are unknown");
            subjects
                .iter()
                .map(|subject| Observation {
                    note: Some("This reading did not finish. Check again.".into()),
                    ..Observation::unknown(*subject)
                })
                .collect()
        });
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

/// The vault's materialization mark as of the last reading of the GitHub repositories row, or
/// the last look whose reading was accepted ([`token_mark_moved`]).
#[cfg(target_os = "macos")]
static GITHUB_TOKEN_MARK: std::sync::Mutex<Option<aterm_secrets::client::MaterializationMark>> =
    std::sync::Mutex::new(None);

/// Whether the vault's mark moved since [`GITHUB_TOKEN_MARK`] and `accept` accepted it. Keep
/// the new mark only then, so a refused observation start still owes its reading. Admission
/// only queues an asynchronous worker: it must not run the reading under this mark's lock.
/// A mark that cannot be read (the vault's lock busy, no vault) moves nothing.
#[cfg(target_os = "macos")]
fn token_mark_moved(accept: impl FnOnce() -> bool) -> bool {
    let Some(current) = (setup_io().vault_mark)() else {
        return false;
    };
    let Ok(mut seen) = GITHUB_TOKEN_MARK.lock() else {
        return false;
    };
    if *seen == Some(current) || !accept() {
        return false;
    }
    *seen = Some(current);
    true
}

/// System State's look for a renewed GitHub token ([`aterm_gui::InitialSetupHandlers::look_for_token`]),
/// on a thread of its own: the vault's mark is a few local files.
#[cfg(target_os = "macos")]
fn look_for_github_token(plan: aterm_types::macos_setup::Plan) -> bool {
    std::thread::Builder::new()
        .name("aterm-setup-token-look".into())
        .spawn(move || {
            read_github_again_if_its_token_moved(plan);
        })
        .is_ok()
}

/// Read the GitHub repositories row again for `plan`, that row alone, when the vault's mark
/// moved since it was last read: a token renewed in the vault reached this Mac (System State
/// design §3.6). An unchanged mark reads nothing and asks nothing of GitHub. Whether a reading
/// was queued.
#[cfg(target_os = "macos")]
fn read_github_again_if_its_token_moved(plan: aterm_types::macos_setup::Plan) -> bool {
    read_github_again_if_its_token_moved_with(plan, request_observation)
}

#[cfg(target_os = "macos")]
fn read_github_again_if_its_token_moved_with(
    plan: aterm_types::macos_setup::Plan,
    observe: impl FnOnce(
        Option<aterm_types::macos_setup::Plan>,
        Vec<aterm_types::macos_setup::Subject>,
        bool,
    ) -> bool,
) -> bool {
    use aterm_types::macos_setup::{Setting, Subject};
    token_mark_moved(|| {
        observe(
            Some(plan),
            vec![Subject::Setting(Setting::Repositories)],
            false,
        )
    })
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
/// registers; their plans stay ([`stop_runs_sent_through`]). A queued press
/// after completion still stops its last confirmed revision. On the UI thread this takes
/// uncontended locks and wakes the Stop's worker: no file is touched, and no
/// thread is started unless the worker could not be when the run registered.
#[cfg(target_os = "macos")]
fn stop_initial_setup() -> bool {
    stop_initial_setup_with(|| {})
}

#[cfg(target_os = "macos")]
fn stop_initial_setup_with(before_lock: impl FnOnce()) -> bool {
    before_lock();
    stop_runs_with_cutoff(|| SETUP_SENT.load(std::sync::atomic::Ordering::Acquire))
}

/// Stop the run registered now ([`stop_setup_run`]), and cover every
/// admission sent through `through` ([`STOP_COVERS`]): a run one of them admits
/// later is stopped as it registers. With no active registration, the last
/// confirmed revision is still the target of a press queued before it finished.
#[cfg(target_os = "macos")]
fn stop_runs_sent_through(through: u64) -> bool {
    stop_runs_with_cutoff(|| through)
}

#[cfg(target_os = "macos")]
fn stop_runs_with_cutoff(through: impl FnOnce() -> u64) -> bool {
    let Ok(run) = SETUP_RUN.lock() else {
        return false;
    };
    let through = through();
    STOP_COVERS.fetch_max(through, std::sync::atomic::Ordering::AcqRel);
    let revision = (*run).or_else(|| stop_saves().last_registered);
    if let Some(revision) = revision {
        stop_setup_run(revision);
    }
    let target = stop_saves().covered_target(through);
    if let Some(target) = target {
        save_setup_stop(target);
    }
    drop(run);
    wake_setup();
    revision.is_some() || target.is_some()
}

/// A newly saved choice supersedes earlier unidentified publications. Live guards still
/// count until their workers retire; abandoned ones no longer owe an unidentifiable Stop.
/// Call only for a new successful admission, never a resume of a readable run.
#[cfg(target_os = "macos")]
fn retire_unconfirmed_admissions(sent: u64) {
    stop_saves().under_way.retain_mut(|debt| {
        if debt.sent >= sent || !debt.unconfirmed {
            return true;
        }
        debt.unconfirmed = false;
        debt.owned
    });
    STOP_SAVES_MOVED.notify_all();
}

/// Register admitted run `revision` as the one a Stop stops ([`SETUP_RUN`]),
/// the moment its revision is durable, before its worker takes a step: a Stop
/// that covers the admission it came from (`sent`, [`stop_covers`]),
/// `overtaken` (a later choice made while it was being admitted), or a Stop
/// asked for the revision a watch widened into it (`widened_from`), or one
/// covering an admission still under way when a watch widened it, stops it
/// before its first stage. Those are read under [`SETUP_RUN`], which every
/// Stop takes, so one pressed while the watch looked is never missed. Callers
/// hold the automatic claim, so no newer run can have registered first.
/// `true` when a Stop so held a widened run no later choice overtook: it
/// started nothing, so the plan it widened goes back in force
/// ([`put_back_held_widen`]).
#[cfg(target_os = "macos")]
fn register_admitted_run(
    revision: u64,
    sent: u64,
    overtaken: bool,
    widened_from: Option<u64>,
) -> bool {
    start_stop_worker();
    // A run admitted: setup is no longer waiting for its review.
    aterm_gui::clear_setup_unfinished();
    SETUP_WIDENED.store(
        if widened_from.is_some() { revision } else { 0 },
        std::sync::atomic::Ordering::Release,
    );
    let Ok(mut run) = SETUP_RUN.lock() else {
        return false;
    };
    *run = Some(revision);
    stop_saves().last_registered = Some(revision);
    let held = widened_from.is_some_and(|from| {
        setup_stop_requested(from)
            || stop_saves()
                .under_way
                .iter()
                .any(|debt| stop_covers(debt.sent))
    });
    let stopped = overtaken || stop_covers(sent) || held;
    if stopped {
        stop_setup_run(revision);
    }
    stopped && !overtaken && widened_from.is_some()
}

/// A Stop held widened run `revision` as it registered ([`register_admitted_run`]): put back
/// the plan in force before the widen (System State design §3.5), and show it. The widening
/// started nothing the person stopped, so the page shows what was in force before. Asked as
/// the run registers, and again once its worker ends, should another pass have held the
/// profile lock the first time; once it is put back, nothing is left to put back.
#[cfg(target_os = "macos")]
fn put_back_held_widen(revision: u64) {
    match (setup_io().put_back)(revision) {
        Ok(true) => aterm_gui::publish_initial_setup_status(initial_setup_status()),
        Ok(false) => {}
        Err(why) => aterm_log::warn!(
            "System State could not yet put back the plan a Stop held from widening: {why}"
        ),
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
        saves.requested = Some(
            saves
                .requested
                .map_or(revision, |requested| requested.max(revision)),
        );
        if saves.saved.is_some_and(|saved| saved >= revision)
            || saves.saving.is_some_and(|saving| saving >= revision)
        {
            return;
        }
        saves.wanted = Some(saves.wanted.map_or(revision, |wanted| wanted.max(revision)));
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
                saves.saved = Some(saves.saved.map_or(revision, |saved| saved.max(revision)));
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
    let stopped = SETUP_STOP.lock().ok().and_then(|stop| *stop);
    let covers = STOP_COVERS.load(std::sync::atomic::Ordering::Acquire);
    let target = {
        let saves = stop_saves();
        [stopped, saves.requested].into_iter().flatten().max()
    };
    if let Some(target) = target {
        save_setup_stop(target);
    }
    let queued = stop_saves().wanted.is_some();
    if queued {
        start_stop_worker();
    }
    let waited = STOP_SAVES_MOVED
        .wait_timeout_while(stop_saves(), limit, |saves| saves.owed(covers))
        .is_ok_and(|(saves, _)| !saves.owed(covers));
    // A run that registered meanwhile was stopped: the Stop to read is the
    // one asked for last, read without holding the worker's lock (the order
    // is [`SETUP_STOP`], then [`STOP_SAVES`]).
    let stopped = SETUP_STOP.lock().ok().and_then(|stop| *stop);
    waited && stopped.is_none_or(|stopped| stop_saves().saved.is_some_and(|saved| saved >= stopped))
}

/// [`aterm_gui::InitialSetupHandlers::settle`]: the app is quitting, or handing off to an
/// update. A stage running now is cut off by no crash: the next launch resumes it without
/// counting toward the park two crashes in one stage earn (design §6.5).
#[cfg(target_os = "macos")]
fn settle_initial_setup() {
    if !settle_setup_stop(STOP_SETTLE_LIMIT) {
        aterm_log::warn!("System State quit before what its Stop owes the records was saved");
    }
    if let Err(why) = aterm_cli::macos_doctor::note_setup_interrupted() {
        aterm_log::warn!("System State could not note the setup stage a quit cuts off: {why}");
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
        }
    }
    if let Ok(mut rows) = OBSERVED_ROWS.lock()
        && rows.choices == choice.choices
    {
        rows.choices += 1;
        rows.plan = None;
    }
    // Startup may already have answered the choice's earlier claim-wait wake.
    // A terminal refusal releases it only after both rollback locks are gone.
    wake_setup();
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
    // A review closed unanswered in this launch: while nothing is admitted, the page shows the
    // plan it showed with Set up now, and nothing needs anyone (design §3.3).
    let closed = closed_review();
    let snapshot = aterm_cli::macos_doctor::initial_setup_snapshot_offering(closed.as_ref());
    if matches!(
        snapshot,
        Err(aterm_cli::macos_doctor::InitialSetupSnapshotError::Read(_))
    ) {
        heal_unread_setup();
    }
    set_aside_as_read(
        snapshot.as_ref().ok().map(|snapshot| snapshot.recovered),
        aterm_gui::clear_setup_set_aside,
    );
    let busy = INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire);
    let mut status = match INITIAL_SETUP_ROWS.lock() {
        Ok(last) => setup_status_after_review(snapshot, busy, &last, closed.as_ref()),
        Err(_) => setup_status_after_review(
            snapshot,
            busy,
            &InitialSetupRows::default(),
            closed.as_ref(),
        ),
    };
    status.retrying = SETUP_RETRYING.load(std::sync::atomic::Ordering::Acquire) != 0;
    status.widened = SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire) != 0;
    status.queued = SETUP_QUEUED.load(std::sync::atomic::Ordering::Acquire) != 0;
    let startup_retrying = STARTUP_RETRY.lock().is_ok_and(|retry| retry.retrying());
    add_startup_retry(&mut status, startup_retrying);
    add_setup_readings(&mut status);
    add_vault_not_joined(
        &mut status,
        SETUP_VAULT_NOT_JOINED.load(std::sync::atomic::Ordering::Acquire),
    );
    status
}

/// The plan this launch's review showed when the person closed it unanswered (design §3.3):
/// nothing offers it again in this launch, and System State shows it with Set up now until
/// something is admitted. The next launch, which starts without it, offers the review again.
#[cfg(target_os = "macos")]
static CLOSED_REVIEW: std::sync::Mutex<Option<aterm_types::macos_setup::Plan>> =
    std::sync::Mutex::new(None);

#[cfg(target_os = "macos")]
fn note_review_closed(reviewed: Option<aterm_types::macos_setup::Plan>) {
    *CLOSED_REVIEW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = reviewed;
}

#[cfg(target_os = "macos")]
fn closed_review() -> Option<aterm_types::macos_setup::Plan> {
    CLOSED_REVIEW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Project the saved setup after a review this launch closed, retaining its plan without
/// treating permission rows as approved while no setup has been admitted.
#[cfg(target_os = "macos")]
fn setup_status_after_review(
    snapshot: Result<
        aterm_cli::macos_doctor::InitialSetupSnapshot,
        aterm_cli::macos_doctor::InitialSetupSnapshotError,
    >,
    busy: bool,
    last: &InitialSetupRows,
    closed: Option<&aterm_types::macos_setup::Plan>,
) -> aterm_gui::SetupStatus {
    let unanswered = closed.is_some()
        && snapshot.as_ref().is_ok_and(|snapshot| {
            !snapshot.run_allowed && !snapshot.cancelled && !snapshot.finished
        });
    let mut status = initial_setup_status_with(snapshot, busy, last);
    status.unanswered = unanswered;
    status
}

/// The launch's answer to its offer was a Join that aterm admitted without the Join, since it
/// cannot read this Mac's vault settings
/// ([`aterm_cli::macos_doctor::InitialSetupAuthorization::join_unread`]): set as that answer is
/// admitted, cleared as the next one, or a person's next choice, is, or as a Join pressed since
/// is refused for the same settings, whose own line says so ([`setup_error_row`]).
#[cfg(target_os = "macos")]
static SETUP_VAULT_NOT_JOINED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Note whether the answer or choice admitted now left its Join undone
/// ([`SETUP_VAULT_NOT_JOINED`]).
#[cfg(target_os = "macos")]
fn note_vault_not_joined(not_joined: bool) {
    SETUP_VAULT_NOT_JOINED.store(not_joined, std::sync::atomic::Ordering::Release);
}

/// While the launch's Join waits unjoined (`not_joined`) and the Secrets row still reads that
/// aterm cannot read this Mac's vault settings, the page says in one calm line that the vault
/// was not joined and why ([`aterm_gui::VAULT_NOT_JOINED_NOTE`]): no failure, since a press
/// changes nothing until those settings read, when the typed vault is the person's to Join.
#[cfg(target_os = "macos")]
fn add_vault_not_joined(status: &mut aterm_gui::SetupStatus, not_joined: bool) {
    use aterm_types::macos_setup::{Setting, Subject};
    if not_joined
        && status.plan.is_some()
        && status.observed.iter().any(|observation| {
            observation.subject == Subject::Setting(Setting::Secrets)
                && observation.note.as_deref()
                    == Some(aterm_cli::macos_doctor::VAULT_SETTINGS_UNREAD)
        })
    {
        status
            .rows
            .push(aterm_gui::calm_setup_row(aterm_gui::VAULT_NOT_JOINED_NOTE));
    }
}

/// While the launch's setup tries a step again by itself (`startup_retrying`, System State
/// design §3.5), say so in one calm line and keep every failure off the menu line and the tab's
/// attention: nothing a person presses changes what comes next. A saved setup aterm could not
/// read says so in its own line ([`aterm_gui::STATE_UNREAD_NOTE`]).
#[cfg(target_os = "macos")]
fn add_startup_retry(status: &mut aterm_gui::SetupStatus, startup_retrying: bool) {
    if !startup_retrying {
        return;
    }
    status.retrying = true;
    if status.plan.is_some() {
        status
            .rows
            .push(aterm_gui::calm_setup_row(aterm_gui::STARTUP_RETRY_NOTE));
    }
}

/// When the launch's setup has not started for this long, trying a step again by itself, the
/// exceptions digest says so: a fact for later, never an alert (System State design §3.6).
#[cfg(target_os = "macos")]
const STARTUP_STUCK_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The launch's setup could not take a step and tries it again by itself (System State design
/// §3.5): since when, and whether the exceptions digest has said it has not started.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupRetry {
    since: Option<std::time::Instant>,
    said: bool,
}

#[cfg(target_os = "macos")]
impl StartupRetry {
    const NONE: Self = Self {
        since: None,
        said: false,
    };

    const fn retrying(&self) -> bool {
        self.since.is_some()
    }

    /// A step failed at `now`: whether the exceptions digest says so now, once
    /// [`STARTUP_STUCK_AFTER`] has passed since the first failure, and only once.
    fn failed(&mut self, now: std::time::Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        let tell = !self.said && now.saturating_duration_since(since) >= STARTUP_STUCK_AFTER;
        self.said |= tell;
        tell
    }

    /// The startup went on, or ended: whether the digest's entry goes now.
    fn went_on(&mut self) -> bool {
        let said = self.said;
        *self = Self::NONE;
        said
    }
}

#[cfg(target_os = "macos")]
static STARTUP_RETRY: std::sync::Mutex<StartupRetry> = std::sync::Mutex::new(StartupRetry::NONE);

/// A step of the launch's setup failed, which it tries again by itself after a wait: one calm
/// line on the page ([`add_startup_retry`]), and ten minutes on, the exceptions digest's entry;
/// the log keeps `why`.
#[cfg(target_os = "macos")]
fn startup_step_failed(why: &str) {
    let tell = STARTUP_RETRY
        .lock()
        .is_ok_and(|mut retry| retry.failed(std::time::Instant::now()));
    if tell {
        aterm_gui::note_setup_not_started(why);
    }
    aterm_gui::publish_initial_setup_status(initial_setup_status());
}

/// The launch's setup went past the step it was trying again, or ended.
#[cfg(target_os = "macos")]
fn startup_went_on() {
    let (was, said) = STARTUP_RETRY.lock().map_or((false, false), |mut retry| {
        let was = retry.retrying();
        (was, retry.went_on())
    });
    if said {
        aterm_gui::clear_setup_not_started();
    }
    if was {
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
}

/// Startups running in this process ([`run_startup_setup`]).
#[cfg(target_os = "macos")]
static STARTUP_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A startup [`heal_unread_setup`] started has not begun yet.
#[cfg(target_os = "macos")]
static HEAL_STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A startup running, counted in [`STARTUP_RUNS`] while it lives.
#[cfg(target_os = "macos")]
struct StartupRunning;

#[cfg(target_os = "macos")]
impl StartupRunning {
    fn enter() -> Self {
        STARTUP_RUNS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        HEAL_STARTING.store(false, std::sync::atomic::Ordering::Release);
        Self
    }
}

#[cfg(target_os = "macos")]
impl Drop for StartupRunning {
    fn drop(&mut self) {
        STARTUP_RUNS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// A read of the saved setup failed. A startup sets a corrupt record aside before it binds its
/// records, and tries a failed read again by itself; with none running in this process (setup
/// finished before, or the launch's startup long done), one starts now to do so (System State
/// design §3.5). For a record that reads again it finds the setup finished and does nothing.
#[cfg(target_os = "macos")]
fn heal_unread_setup() {
    if STARTUP_RUNS.load(std::sync::atomic::Ordering::Acquire) == 0
        && !HEAL_STARTING.swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        if !(setup_io().scope_refused)() {
            aterm_log::info!(
                "System State: the saved setup could not be read; setup reads it again"
            );
            if !(setup_io().start_startup)() {
                // No child can take over this reservation after a refused spawn.
                HEAL_STARTING.store(false, std::sync::atomic::Ordering::Release);
            }
        } else {
            HEAL_STARTING.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// aterm set aside saved setup records it could not parse, and goes on as from a fresh record
/// (System State design §3.5): the log names them, and the exceptions digest keeps the fact.
#[cfg(target_os = "macos")]
fn note_setup_set_aside(set: &aterm_cli::macos_doctor::SetAside) {
    let kept = set.kept.join(", ");
    aterm_log::warn!(
        "System State: saved setup records could not be read and were set aside as {kept}"
    );
    SET_ASIDE_SAID.store(true, std::sync::atomic::Ordering::Release);
    aterm_gui::note_setup_set_aside(&kept);
}

/// The exceptions digest may hold the set-aside fact ([`note_setup_set_aside`]): at launch, one
/// an earlier launch raised, and since, one this launch raised.
#[cfg(target_os = "macos")]
static SET_ASIDE_SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The saved setup read just now, `recovered` as its snapshot says (`None`: it could not be
/// read). Once it reads with no set-aside to say (a person chose, or Restore ran, since aterm set
/// it aside, or it never did), the digest's entry goes with the page's note (System State design
/// §3.5): `clear` runs once, until aterm sets a saved setup aside again.
#[cfg(target_os = "macos")]
fn set_aside_as_read(recovered: Option<bool>, clear: impl FnOnce()) {
    if recovered == Some(false) && SET_ASIDE_SAID.swap(false, std::sync::atomic::Ordering::AcqRel) {
        clear();
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
    let changing = matches!(
        snapshot,
        Err(aterm_cli::macos_doctor::InitialSetupSnapshotError::Updating)
    );
    let recovered = snapshot.as_ref().is_ok_and(|snapshot| snapshot.recovered);
    let cancelled = snapshot.as_ref().is_ok_and(|snapshot| snapshot.cancelled);
    let run = snapshot
        .as_ref()
        .ok()
        .map(|snapshot| aterm_gui::InitialSetupRunPolicy {
            revision: snapshot.revision,
            allowed: snapshot.run_allowed,
        });
    let (plan, finished, provisional, permissions, mut rows, observation) = match snapshot {
        Ok(snapshot) => (
            Some(snapshot.plan),
            snapshot.finished,
            snapshot.provisional,
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
            false,
            None,
            // The page says it in its own words, never the file or the error: `aterm doctor`
            // reads the same state and says why. A read that failed is no failure (System State
            // design §3.6): aterm reads it again by itself, and sets aside a record that stays
            // unreadable ([`heal_unread_setup`]), so it is one calm line.
            vec![match error {
                aterm_cli::macos_doctor::InitialSetupSnapshotError::Updating => {
                    aterm_gui::SetupStatusRow {
                        name: aterm_gui::SETUP_ROW.into(),
                        status: "pending".into(),
                        detail: String::new(),
                    }
                }
                aterm_cli::macos_doctor::InitialSetupSnapshotError::Read(_) => {
                    aterm_gui::calm_setup_row(aterm_gui::STATE_UNREAD_NOTE)
                }
                // A newer aterm's setup: this one leaves it as it is, and reads nothing again.
                aterm_cli::macos_doctor::InitialSetupSnapshotError::Newer => {
                    aterm_gui::calm_setup_row(aterm_gui::NEWER_SETUP_NOTE)
                }
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
        run,
        busy,
        finished,
        provisional,
        rows,
        check,
        changing,
        recovered,
        cancelled,
        ..aterm_gui::SetupStatus::default()
    }
}

#[cfg(target_os = "macos")]
fn remember_initial_setup_rows(
    report: &aterm_cli::macos_doctor::Report,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    // A change aterm made and could not log is a fact for later (System State design §3.6):
    // the exceptions digest's, never the menu line or the tab's attention; the page says it in
    // one calm line.
    if let Some(refused) = report.journal_refused() {
        aterm_gui::note_journal_refused(refused);
    }
    // A row a person changed after setup had it as wanted is theirs: no automatic pass rewrites
    // it, and the exceptions digest keeps the fact until a check reads nothing out of place or a
    // Restore puts the originals back.
    let changed = report.changed_since();
    if !changed.is_empty() {
        aterm_gui::note_setup_changed_since(&changed.join(", "));
    } else if report.action == aterm_cli::macos_doctor::Action::Restore
        || (report.action == aterm_cli::macos_doctor::Action::Check && !report.needs_attention())
    {
        aterm_gui::clear_setup_changed_since();
    }
    publish_initial_setup_rows(initial_setup_report_rows(report), checked_plan, observation);
}

#[cfg(target_os = "macos")]
fn initial_setup_report_rows(
    report: &aterm_cli::macos_doctor::Report,
) -> Vec<aterm_gui::SetupStatusRow> {
    // A Restore reaches the page as what it did to each System State row, in the page's
    // own words (one calm line, and a row for each setting it could not put back), never
    // as the preference keys `aterm doctor` lists: those stay in the CLI's text.
    // An explicit page can also name a separate setup failure after that Restore.
    let mut page = report.restore.as_ref().map_or_else(Vec::new, |restore| {
        aterm_gui::restore_rows(&restore.outcome)
    });
    let checked = report.action == aterm_cli::macos_doctor::Action::Check;
    // A pass, and a Check, reach the page the same way (design §3.6): a change that failed, or
    // could not be confirmed, by its row, a step that failed as a whole under the rows it
    // covers, and what a rollback put back in a Restore's words, never a stage's own name or an
    // item's own title (a preference key, a keyboard's ID); its other rows as they are.
    let rows = match &report.page {
        Some(part) => {
            page.extend(aterm_gui::pass_rows(&part.outcome));
            part.rows.as_slice()
        }
        None if report.restore.is_some() => &[],
        None => report.rows.as_slice(),
    };
    if checked {
        for row in &mut page {
            row.name = format!("Check: {}", row.name);
        }
    }
    // The change journal's refusal is the exceptions digest's ([`remember_initial_setup_rows`]).
    let rows = rows
        .iter()
        .filter(|row| row.name != aterm_cli::macos_doctor::JOURNAL_ROW);
    page.extend(rows.map(|row| {
        // Setup's blocked steps are explained by their observed row, but
        // Restore's are operation results: even an in-place preference
        // can have an unknown or inaccessible original. Keep that reason
        // visible independently of the current plan's selected groups. A
        // setting back that loads at the next login (`LOGIN`) is no
        // failure and passes as itself.
        let not_restored = report.restore.is_none()
            && report.action == aterm_cli::macos_doctor::Action::Restore
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
    }));
    page
}

#[cfg(target_os = "macos")]
fn initial_setup_error(
    detail: String,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    publish_initial_setup_error(detail, None, observation);
}

/// Whether `detail` names a newer aterm's saved setup
/// ([`aterm_cli::macos_doctor::setup_is_newer`]). An update is needed to read those records,
/// so the page uses its calm line ([`aterm_gui::NEWER_SETUP_NOTE`]); the log keeps the detail.
#[cfg(target_os = "macos")]
fn refused_for_newer_setup(detail: &str) -> bool {
    if !aterm_cli::macos_doctor::setup_is_newer(detail) {
        return false;
    }
    aterm_log::info!("System State: {detail}");
    aterm_gui::publish_initial_setup_status(initial_setup_status());
    true
}

/// Publish a setup run, or a Check of `checked_plan`, that failed as a whole: the page says so
/// in its own words (System State design §3.6), and the log keeps `detail`, which can name a
/// file, a process or an error the page never shows.
#[cfg(target_os = "macos")]
fn publish_initial_setup_error(
    detail: String,
    checked_plan: Option<&aterm_types::macos_setup::Plan>,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    if refused_for_newer_setup(&detail) {
        return;
    }
    let row = setup_error_row(&detail, checked_plan.is_some());
    if row.status == "failed" {
        aterm_log::warn!("System State: {} failed: {detail}", row.name);
    } else {
        aterm_log::info!("System State: Join refused: {detail}");
        // This press's line says it: the launch's word that its Join went unjoined goes.
        note_vault_not_joined(false);
    }
    publish_initial_setup_rows(vec![row], checked_plan, observation);
}

/// The page's row for a setup run, or a Check (`check`), that failed as a whole with `detail`
/// ([`publish_initial_setup_error`]). A Join refused because aterm cannot read this Mac's vault
/// settings changed nothing, and pressing anything again meets the same settings: one calm line
/// says so ([`aterm_gui::VAULT_UNREAD_NOTE`]), never a failure, with no Set up now, menu line
/// or tab attention, and the page keeps the typed vault for a Join once they read.
#[cfg(target_os = "macos")]
fn setup_error_row(detail: &str, check: bool) -> aterm_gui::SetupStatusRow {
    if !check && aterm_cli::macos_doctor::vault_config_unread(detail) {
        return aterm_gui::calm_setup_row(aterm_gui::VAULT_UNREAD_NOTE);
    }
    let (name, said) = if check {
        ("Check", aterm_gui::CHECK_FAILED_NOTE)
    } else {
        // Set up now starts it again, as the page's note says; while the run retries by
        // itself, the note says that instead.
        (aterm_gui::SETUP_ROW, "")
    };
    aterm_gui::SetupStatusRow {
        name: name.into(),
        status: "failed".into(),
        detail: said.into(),
    }
}

/// Publish a Restore that failed as a whole, as [`publish_initial_setup_error`] does a run: the
/// page headlines it as the Restore it is, with its reason in the page's words (System State
/// design §3.4); the log keeps `detail`.
#[cfg(target_os = "macos")]
fn restore_initial_setup_error(
    detail: String,
    observation: Option<aterm_cli::macos_doctor::InitialSetupObservation>,
) {
    if refused_for_newer_setup(&detail) {
        return;
    }
    aterm_log::warn!("System State: Restore failed: {detail}");
    publish_initial_setup_rows(vec![restore_failed_row(&detail)], None, observation);
}

/// The page's row for a Restore that failed as a whole with `detail`: one that cannot read the
/// settings aterm saved before setup changed them names where to learn why, since pressing it
/// again reads the same record; any other is pressed again.
#[cfg(target_os = "macos")]
fn restore_failed_row(detail: &str) -> aterm_gui::SetupStatusRow {
    aterm_gui::SetupStatusRow {
        name: "Restore previous settings".into(),
        status: "failed".into(),
        detail: if detail.starts_with(aterm_cli::macos_doctor::BACKUP_UNREAD) {
            aterm_gui::RESTORE_BACKUP_UNREAD_NOTE
        } else {
            aterm_gui::RESTORE_FAILED_NOTE
        }
        .into(),
    }
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
    // provisional plan provisional (design §3.5). Either takes only what the page
    // changed onto the plan in force as it is admitted (§3.3).
    let restart = if receipt.chosen() {
        aterm_cli::macos_doctor::InitialSetupRestart::chosen()
    } else {
        aterm_cli::macos_doctor::InitialSetupRestart::default()
    }
    .changing(receipt.changed());
    // A choice that finds this Mac's setup records held by another setup waits from that moment
    // (System State design §3.3): the page says so at once, never a run going for the ten
    // seconds that wait can last.
    let generation = choice.generation;
    let restart = restart.noting_wait(move || queue_setup_press(generation));
    let mut host = LiveSetupRestart {
        restart,
        vault_typed,
        observation: busy.observation.clone(),
        generation: choice.generation,
        sent: choice.sent,
        awaiting,
        under_way: Some(under_way),
        startup_held: false,
        retry_seen: SETUP_WAKES.count(),
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
    fn stopped(&self) -> bool {
        false
    }
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
    /// `why`, a write the profile lock refused, says another setup held this Mac's setup
    /// records past the choice's short wait (`aterm_cli::macos_doctor::SETUP_BUSY`).
    fn held_elsewhere(&self, why: &str) -> bool;
    /// Another setup holds this Mac's setup records (another aterm, `aterm doctor` in a
    /// terminal): the page says the choice waits for it, and this waits a moment before the
    /// next ask. `false` once a later choice overtook this one or a Stop covers it: it is let
    /// go, as the person's latest press says (System State design §3.3).
    fn queue(&mut self) -> bool;
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
            // Another process's setup, or `aterm doctor`, holds it: the choice waits for it
            // and is applied when it finishes, never dropped (§3.3).
            Ok(None) => {
                if !host.queue() {
                    return Some(busy);
                }
            }
            Err(why) => {
                host.failure(&why);
                return Some(busy);
            }
        }
    };
    let mut attempted = false;
    loop {
        if attempted && host.stopped() {
            return Some(busy);
        }
        attempted = true;
        match host.persist(plan) {
            Ok(Some(revision)) => {
                host.admitted(revision, claim, busy, password, receipt);
                return None;
            }
            Ok(None) => return Some(busy),
            // Another setup held the records past the choice's wait: it waits on, as for the
            // claim, and asks again.
            Err(why) if host.held_elsewhere(&why) => {
                if !host.queue() {
                    return Some(busy);
                }
            }
            Err(why) => {
                host.failure(&why);
                if host.stopped() || !host.pending() {
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
    /// Setup changes heard before this attempt began its write.
    retry_seen: u64,
}

#[cfg(target_os = "macos")]
impl LiveSetupRestart {
    fn backoff_for(&mut self, limit: std::time::Duration) {
        wait_before_admission_retry(self.sent, &mut self.retry_seen, limit, |seen, limit| {
            SETUP_WAKES.back_off(seen, limit);
        });
    }
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
        self.retry_seen = SETUP_WAKES.count();
        let observed = (setup_io().persist)(&mut self.restart, plan, self.vault_typed);
        self.observation = observed.observation;
        if let Some(under_way) = &self.under_way {
            under_way.record_publication(self.restart.stop_target());
        }
        observed.result
    }

    fn pending(&self) -> bool {
        self.restart.pending()
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent)
    }

    fn backoff(&mut self) {
        self.backoff_for(SETUP_RETRY_WAIT);
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
        retire_unconfirmed_admissions(self.sent);
        // A choice's run widened nothing, so nothing is put back.
        register_admitted_run(
            revision,
            self.sent,
            setup_choice_overtaken(self.generation),
            None,
        );
        drop(self.under_way.take());
        unqueue_setup_press(self.generation);
        note_vault_not_joined(false);
        clear_initial_setup_rows();
        receipt.accept();
        aterm_gui::publish_initial_setup_status(initial_setup_status());
        // A worker parked on the replaced revision wakes, finds it gone, and ends.
        wake_setup();
        (setup_io().run)(Some(claim), revision, password, busy);
    }

    fn failure(&mut self, why: &str) {
        // A press that failed waits for no other setup now.
        unqueue_setup_press(self.generation);
        initial_setup_error(why.into(), self.observation.clone());
    }

    fn held_elsewhere(&self, why: &str) -> bool {
        why == aterm_cli::macos_doctor::SETUP_BUSY
    }

    fn queue(&mut self) -> bool {
        queue_setup_press(self.generation);
        wait_while_queued(|| setup_choice_overtaken(self.generation) || stop_covers(self.sent))
    }
}

#[cfg(target_os = "macos")]
impl Drop for LiveSetupRestart {
    fn drop(&mut self) {
        // Admitted, let go or overtaken: the choice waits for nothing now.
        unqueue_setup_press(self.generation);
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
        /// The waits for another setup begun, and how many go by before a later choice
        /// overtakes this one (`None`: none does).
        queues: usize,
        overtaken_after: Option<usize>,
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
        /// Another setup held the records past the choice's wait.
        Busy,
    }

    const BUSY: &str = "another setup holds the records";

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
                Admission::Busy => Err(BUSY.into()),
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

        fn held_elsewhere(&self, why: &str) -> bool {
            why == BUSY
        }

        fn queue(&mut self) -> bool {
            // The choice keeps its slot, receipt and password while it waits.
            assert!(self.state.borrow().alive.len() >= 3);
            assert_eq!(self.state.borrow().refused, 0);
            self.queues += 1;
            self.overtaken_after
                .is_none_or(|after| self.queues <= after)
        }
    }

    fn run(plan: &Plan, outcomes: Vec<Admission>) -> Host {
        let host = run_held(plan, Vec::new(), outcomes);
        assert_eq!(host.state.borrow().drops[0], "claim");
        host
    }

    /// [`run`] where the claim is held by `held`, look by look, before it is free.
    fn run_held(plan: &Plan, held: Vec<Holder>, outcomes: Vec<Admission>) -> Host {
        run_overtaken(plan, held, outcomes, None)
    }

    /// [`run_held`], where a later choice overtakes this one after `overtaken_after` waits for
    /// another setup.
    fn run_overtaken(
        plan: &Plan,
        held: Vec<Holder>,
        outcomes: Vec<Admission>,
        overtaken_after: Option<usize>,
    ) -> Host {
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
            queues: 0,
            overtaken_after,
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

    /// A choice that meets another setup is never dropped (System State design §3.3). This
    /// process's startup coordinator, holding the claim while it reads the plan in force, lets
    /// go by itself: the choice waits for it. Another process's setup, or `aterm doctor`,
    /// holding the claim or the records past the choice's wait: the choice waits for it too,
    /// keeping its slot, receipt and password, says so, and is applied once it finishes, with
    /// no failure. A later choice overtaking it lets it go, with no failure either.
    #[test]
    fn a_choice_waits_for_another_setup_and_is_applied_when_it_finishes() {
        let host = run_held(
            &Plan::default(),
            vec![Holder::Startup, Holder::Startup],
            vec![Admission::Admitted(5)],
        );
        assert_eq!(host.claims, 3);
        assert_eq!((host.awaits, host.queues), (2, 0));
        assert_eq!(host.dispatches, [5]);
        assert!(host.failures.is_empty());
        assert_eq!(host.state.borrow().accepted, 1);
        assert!(!host.slot_back);

        let host = run_held(
            &Plan::default(),
            vec![Holder::Elsewhere, Holder::Elsewhere],
            vec![Admission::Admitted(6)],
        );
        assert_eq!((host.claims, host.awaits, host.queues), (3, 0, 2));
        assert_eq!(host.dispatches, [6]);
        assert!(host.failures.is_empty());
        assert_eq!(host.state.borrow().accepted, 1);
        assert!(!host.slot_back);

        let plan = Plan::default();
        let host = run(&plan, vec![Admission::Busy, Admission::Admitted(7)]);
        assert_eq!((host.claims, host.queues, host.backoffs), (1, 1, 0));
        assert_eq!(host.plans, vec![plan.clone(); 2]);
        assert_eq!(host.dispatches, [7]);
        assert!(host.failures.is_empty());
        assert_eq!(host.state.borrow().accepted, 1);

        for (held, outcomes) in [
            (vec![Holder::Elsewhere, Holder::Elsewhere], Vec::new()),
            (Vec::new(), vec![Admission::Busy, Admission::Busy]),
        ] {
            let host = run_overtaken(&plan, held, outcomes, Some(1));
            assert_eq!(host.queues, 2);
            assert!(host.dispatches.is_empty());
            assert!(host.failures.is_empty(), "a choice let go fails nothing");
            assert_eq!(host.state.borrow().refused, 1);
            assert!(host.slot_back, "a choice let go gives its slot back");
        }
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
    // parked on a person look again, its tries afresh (System State design §3.5).
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
        wake_setup_for_check,
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
    spawn_initial_setup_operation("aterm-setup-restore", |busy| {
        run_setup_restore(busy, |waits, holding| {
            aterm_cli::macos_doctor::restore_observed(waits, holding)
        });
    })
}

/// Run System State's Restore previous settings holding the setup slot (`busy`). `restore`
/// takes the Restore's two hooks: the first says its own attempt found the records held by
/// another setup, the second runs once it holds them, before its first write, and says whether
/// it goes on (`aterm_cli::macos_doctor::restore_observed`).
#[cfg(target_os = "macos")]
fn run_setup_restore(
    busy: InitialSetupBusy,
    mut restore: impl FnMut(
        &mut dyn FnMut(),
        &mut dyn FnMut() -> bool,
    ) -> aterm_cli::macos_doctor::InitialSetupObserved<
        Option<aterm_cli::macos_doctor::Report>,
    >,
) {
    // A choice made after this Restore was pressed is the person's latest intent.
    let pressed = SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    let observed = loop {
        if !(setup_io().records_held)() {
            // Each attempt is the Restore going, not one waiting: the page says it waits again
            // only once this attempt finds the records held. A group pressed while it waited
            // has replaced it.
            unqueue_setup_press(RESTORE_QUEUED);
            if setup_choice_overtaken(pressed) {
                break None;
            }
            let observed = with_restore_readings(|| {
                restore(&mut || queue_setup_press(RESTORE_QUEUED), &mut || {
                    // It holds the records and has written nothing: the page shows it
                    // restoring from now, its groups and Check again as during a run, unless a
                    // group press overtook it, which it gives way to having changed nothing.
                    unqueue_setup_press(RESTORE_QUEUED);
                    !setup_choice_overtaken(pressed)
                })
            });
            // One that took the records past the Restore's own wait for them, since it asked,
            // is waited for as one that held them.
            match observed.result {
                Ok(None) => break None,
                Err(why) if why == aterm_cli::macos_doctor::SETUP_BUSY => {}
                Ok(Some(report)) => break Some((Ok(report), observed.observation)),
                Err(why) => break Some((Err(why), observed.observation)),
            }
        }
        // Another setup holds this Mac's setup records (another aterm, `aterm doctor` in a
        // terminal): the Restore waits on quietly, holding the slot, and reads no row until it
        // runs, when that setup finishes (System State design §3.4), unless a choice made
        // meanwhile replaces it, as a later choice replaces one waiting: it is let go, having
        // changed nothing, and the choice takes the slot.
        queue_setup_press(RESTORE_QUEUED);
        if !wait_while_queued(|| setup_choice_overtaken(pressed)) {
            break None;
        }
    };
    unqueue_setup_press(RESTORE_QUEUED);
    let Some((result, observation)) = observed else {
        aterm_log::info!("System State: a later choice replaced the waiting Restore");
        drop(busy);
        return;
    };
    match result {
        Ok(report) => remember_initial_setup_rows(&report, None, observation),
        Err(why) => restore_initial_setup_error(why, observation),
    }
    drop(busy);
    offer_again_after_restore();
}

/// After a Restore, with the setup slot let go: one that ended setup before any run of it was
/// admitted (a Restore pressed while the launch's offer waited for its answer, which it lets go,
/// or one before any setup ran here) left aterm's own permissions with no run to carry them. The
/// Restore brought it forward under its own lock, and the startup offers it again now, as the
/// next launch would (System State design §3.4), so Full Disk Access, Developer Tools, the
/// debugger and Local Network go on in this launch with no press. The startup also runs when this
/// finished a Restore cut short, or met the profile lock busy (the startup waits for it). A
/// Restore whose admitted run carries the permissions leaves nothing for the offer and starts
/// nothing.
#[cfg(target_os = "macos")]
fn offer_again_after_restore() {
    use aterm_cli::macos_doctor::BroughtForward;
    let io = setup_io();
    match (io.bring_forward)() {
        Ok(BroughtForward::Run(_) | BroughtForward::Busy) => {
            if !(io.start_startup)() {
                (io.start_startup_inline)();
            }
        }
        Ok(BroughtForward::Nothing) => {}
        Err(why) => aterm_log::warn!(
            "System State: bringing the restored setup forward did not finish: {why}"
        ),
    }
}

/// The live [`SetupIo::start_startup`]: [`run_startup_setup`] on a thread of its own, as a
/// launch starts it.
#[cfg(target_os = "macos")]
fn start_startup_again() -> bool {
    match std::thread::Builder::new()
        .name("aterm-workstation-setup".into())
        .spawn(|| run_startup_setup(None))
    {
        Ok(_) => true,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor worker could not start: {why}");
            false
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod initial_setup_result_tests {
    #[path = "withheld_restore_result_tests.rs"]
    mod withheld_restore_result_tests;

    #[path = "withheld_stop_tests.rs"]
    mod withheld_stop_tests;

    #[path = "startup_retry_wake_tests.rs"]
    mod startup_retry_wake_tests;

    #[path = "admission_stop_handoff_tests.rs"]
    mod admission_stop_handoff_tests;

    #[path = "watch_stop_handoff_tests.rs"]
    mod watch_stop_handoff_tests;

    #[path = "startup_recovery_tests.rs"]
    mod startup_recovery_tests;

    #[path = "startup_cancel_watch_tests.rs"]
    mod startup_cancel_watch_tests;

    #[path = "startup_repair_debt_tests.rs"]
    mod startup_repair_debt_tests;

    #[path = "queued_press_tests.rs"]
    mod queued_press_tests;

    #[path = "progress_publication_tests.rs"]
    mod progress_publication_tests;

    #[path = "restore_retry_tests.rs"]
    mod restore_retry_tests;

    #[path = "bind_stop_tests.rs"]
    mod bind_stop_tests;

    #[path = "startup_stop_retry_tests.rs"]
    mod startup_stop_retry_tests;

    #[path = "startup_newer_tests.rs"]
    mod startup_newer_tests;

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

    /// A Stop pressed in this launch for the run in force holds every widen review in it, even
    /// one the Stop came too late for (it came during the run's last stage, which finished, so
    /// the records keep no Stop and still widen): a person who pressed Stop gets no more
    /// automatic setup this launch (design §3.5). A later run, which the Stop did not stop,
    /// may be widened.
    #[test]
    fn a_stop_in_this_launch_offers_no_widen_review_of_the_run_it_stopped() {
        let _globals = globals();
        reset_worker_state();
        assert_eq!(
            widens_here_with(false, || Ok(true), || Ok(7)),
            Ok(true),
            "control: no Stop"
        );
        // What `stop_setup_run(7)` leaves in memory; the durable request it saves is spent.
        *SETUP_STOP.lock().unwrap() = Some(7);
        let held = widens_here_with(false, || Ok(true), || Ok(7));
        let later = widens_here_with(false, || Ok(true), || Ok(8));
        let answered = widens_here_with(true, || panic!("not read"), || panic!("not read"));
        reset_worker_state();
        assert_eq!(held, Ok(false), "no review of the run the person stopped");
        assert_eq!(later, Ok(true), "a later run is not the one stopped");
        assert_eq!(answered, Ok(false));
    }

    /// A stand-in for the watch's upgrade: each persist answers as scripted, with whether the
    /// attempt is still held after it.
    struct ScriptedUpgrade {
        pending: bool,
        answers: VecDeque<(
            Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>,
            bool,
        )>,
        persisted: u32,
        forgot: u32,
    }

    impl ScriptedUpgrade {
        fn new(
            answers: impl IntoIterator<
                Item = (
                    Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>,
                    bool,
                ),
            >,
        ) -> Self {
            Self {
                pending: false,
                answers: answers.into_iter().collect(),
                persisted: 0,
                forgot: 0,
            }
        }
    }

    impl WatchUpgrade for ScriptedUpgrade {
        fn pending(&self) -> bool {
            self.pending
        }
        fn widens_from(&self) -> Option<u64> {
            self.pending.then_some(7)
        }
        fn revision(&self) -> Result<u64, String> {
            Ok(7)
        }
        fn persist(&mut self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
            self.persisted += 1;
            let (answer, pending) = self.answers.pop_front().expect("a scripted answer");
            self.pending = pending;
            answer
        }
        fn published(&self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
            Ok(None)
        }
        fn forget(&mut self) {
            self.forgot += 1;
            self.pending = false;
        }
    }

    const WIDENED: aterm_cli::macos_doctor::SetupUpgrade =
        aterm_cli::macos_doctor::SetupUpgrade { from: 7, to: 8 };

    /// One look of a watch whose widen review applied, with the page's receipt for that Apply:
    /// the look's answer and the receipt it kept. The watch keeps `widened` from look to look.
    fn look_once(
        widened: &mut Widened,
        upgrade: &mut ScriptedUpgrade,
        receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
        queue: impl FnMut(&Widened) -> bool,
    ) -> (
        Result<Option<u64>, String>,
        Option<aterm_gui::InitialSetupApplyReceipt>,
    ) {
        look_settling_with(widened, upgrade, receipt, queue)
    }

    /// Every look of a widen review's Apply settles it but one whose upgrade is still pending:
    /// accepted once the widened plan is saved, refused when the look ends with nothing
    /// written (a Stop retired it, the records changed, a write failed for good), so the page
    /// never shows a run that nothing runs and no later offer is refused for it.
    #[test]
    fn every_look_of_a_widen_apply_accepts_keeps_or_refuses_its_receipt() {
        let _globals = globals();
        reset_worker_state();
        let no_wait = |_: &Widened| -> bool { panic!("nothing waits for another setup") };
        let heard = |answer: &std::sync::mpsc::Receiver<bool>| answer.try_recv().ok();

        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut upgrade = ScriptedUpgrade::new([(Ok(Some(WIDENED)), false)]);
        let (looked, kept) = look_once(
            &mut Widened::default(),
            &mut upgrade,
            Some(receipt),
            no_wait,
        );
        assert_eq!(
            (looked, kept.is_none(), heard(&answer)),
            (Ok(Some(8)), true, Some(true))
        );

        for (why, answer_now) in [
            ("nothing changed", (Ok(None), false)),
            (
                "a write that failed for good",
                (Err("disk".to_string()), false),
            ),
        ] {
            let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
            let mut upgrade = ScriptedUpgrade::new([answer_now]);
            let (_, kept) = look_once(
                &mut Widened::default(),
                &mut upgrade,
                Some(receipt),
                no_wait,
            );
            assert!(kept.is_none(), "{why}");
            assert_eq!(
                heard(&answer),
                Some(false),
                "{why}: refused, the page shows the plan in force"
            );
        }

        // A write that failed, its attempt held: the next look finishes it.
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut upgrade =
            ScriptedUpgrade::new([(Err("disk".to_string()), true), (Ok(Some(WIDENED)), false)]);
        let mut widened = Widened::default();
        let (looked, kept) = look_once(&mut widened, &mut upgrade, Some(receipt), no_wait);
        assert_eq!(looked, Err("disk".to_string()));
        assert!(
            kept.is_some() && heard(&answer).is_none(),
            "kept while pending"
        );
        let (looked, kept) = look_once(&mut widened, &mut upgrade, kept, no_wait);
        assert_eq!(
            (looked, kept.is_none(), heard(&answer)),
            (Ok(Some(8)), true, Some(true))
        );

        // This launch's Stop of the run the widen would replace (one spent on a finished run
        // included): the look retires the upgrade unwritten, and the Apply is refused.
        *SETUP_STOP.lock().unwrap() = Some(7);
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut upgrade = ScriptedUpgrade::new([(Ok(Some(WIDENED)), false)]);
        let (looked, kept) = look_once(
            &mut Widened::default(),
            &mut upgrade,
            Some(receipt),
            no_wait,
        );
        reset_worker_state();
        assert_eq!(looked, Ok(None));
        assert_eq!(upgrade.persisted, 0, "nothing reached the records");
        assert!(kept.is_none());
        assert_eq!(
            heard(&answer),
            Some(false),
            "refused, never left unanswered"
        );
    }

    /// The upgrade finds another setup holding the records past its wait: it waits on, shown
    /// waiting, as a choice does, and is admitted once that setup lets go; one a Stop or a later
    /// choice lets go meanwhile is retired unwritten and its Apply refused.
    #[test]
    fn a_widen_apply_another_setup_holds_waits_until_admitted_or_let_go() {
        let _globals = globals();
        reset_worker_state();
        let busy = || (Err(aterm_cli::macos_doctor::SETUP_BUSY.to_string()), true);
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut upgrade = ScriptedUpgrade::new([busy(), busy(), (Ok(Some(WIDENED)), false)]);
        let mut waits = 0;
        let (looked, kept) =
            look_once(&mut Widened::default(), &mut upgrade, Some(receipt), |_| {
                waits += 1;
                true
            });
        assert_eq!((looked, waits), (Ok(Some(8)), 2));
        assert!(kept.is_none());
        assert_eq!(answer.try_recv(), Ok(true));

        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut upgrade = ScriptedUpgrade::new([busy()]);
        let (looked, kept) =
            look_once(&mut Widened::default(), &mut upgrade, Some(receipt), |_| {
                false
            });
        reset_worker_state();
        assert_eq!(looked, Ok(None));
        assert_eq!(
            (upgrade.forgot, upgrade.pending),
            (1, false),
            "retired unwritten"
        );
        assert!(kept.is_none());
        assert_eq!(answer.try_recv(), Ok(false));
    }

    /// The person cancels the review that offered what a Restore brought forward: the status
    /// the page reads says so as for any Cancel, its Cancel line beside Set up now, every group
    /// at Leave as is, aterm's own permissions held; Set up now then applies the plan the review
    /// showed (design §3.3).
    #[test]
    fn a_cancelled_review_of_a_restore_brought_forward_reads_as_any_cancel() {
        use aterm_cli::macos_doctor::InitialSetupAdmissionStopTest;
        let root = aterm_tempfile::tempdir().unwrap();
        let restored = InitialSetupAdmissionStopTest::restored_before_run(root.path()).unwrap();
        assert!(
            restored.bring_forward().unwrap().is_some(),
            "brought forward"
        );
        let mut review = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        let reviewed = review.plan();
        assert!(review.cancel().unwrap(), "the Cancel is recorded");
        let status = initial_setup_status_with(
            Ok(review.snapshot().unwrap()),
            false,
            &InitialSetupRows::default(),
        );
        assert!(
            status.cancelled,
            "the page's Cancel line, beside Set up now"
        );
        assert!(status.finished && !status.provisional);
        assert!(
            status.plan.unwrap().selection.is_empty(),
            "every group at Leave as is"
        );
        assert!(
            !status.permissions.unwrap().allowed,
            "aterm's permission steps held"
        );

        assert!(
            review.set_up_now().unwrap().is_some(),
            "Set up now is admitted"
        );
        assert_eq!(review.saved_plan().unwrap(), Some(reviewed.clone()));
        let status = initial_setup_status_with(
            Ok(review.snapshot().unwrap()),
            false,
            &InitialSetupRows::default(),
        );
        assert!(!status.cancelled);
        assert_eq!(status.plan, Some(reviewed));
        assert!(status.permissions.unwrap().allowed);
    }

    #[test]
    fn clt_install_status_distinguishes_the_run_from_carried_permission_approval_and_stop() {
        use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;

        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let before = setup.snapshot().unwrap();
        let approval = before.access_revision;
        let run = setup.upgrade(None).unwrap().unwrap();
        let current = setup.snapshot().unwrap();
        assert_eq!(current.revision, run);
        assert!(run > before.revision);
        assert_eq!(current.access_revision, approval);
        assert!(
            current
                .plan
                .selection
                .contains(aterm_types::macos_setup::Setting::Repositories)
        );
        assert!(current.permissions_allowed);
        assert!(current.run_allowed);
        let status = initial_setup_status_with(Ok(current), false, &InitialSetupRows::default());
        assert_eq!(status.permissions.unwrap().revision, approval);
        assert_eq!(
            status.run,
            Some(aterm_gui::InitialSetupRunPolicy {
                revision: run,
                allowed: true
            })
        );
        assert!(!status.finished);

        setup.stop().unwrap();
        let stopped = setup.snapshot().unwrap();
        assert_eq!((stopped.revision, stopped.access_revision), (run, approval));
        assert!(!stopped.permissions_allowed);
        assert!(!stopped.run_allowed);
        let status = initial_setup_status_with(Ok(stopped), false, &InitialSetupRows::default());
        assert_eq!(status.permissions.unwrap().revision, approval);
        assert!(!status.permissions.unwrap().allowed);
        assert_eq!(
            status.run,
            Some(aterm_gui::InitialSetupRunPolicy {
                revision: run,
                allowed: false
            })
        );
        assert!(status.finished);
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
            let changing = matches!(error, InitialSetupSnapshotError::Updating);
            let status = initial_setup_status_with(Err(error), true, &last);
            assert_eq!(status.changing, changing, "only a read that met a write");
            assert!(!has_old_result(&status));
            assert!(status.permissions.is_none());
            assert!(status.busy);
            assert_eq!(status.rows.len(), 1);
        }
        let unbound = cached(None, false);
        let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &unbound);
        assert!(!has_old_result(&status));
        assert!(!status.changing, "a read that met no write");
    }

    /// A Restore reaches the page as what it did to each System State row: one calm line in
    /// the page's own row names, never a preference key, a process or a row per login reload.
    /// A reader that did not relaunch leaves the rows it reads to the next login, which the
    /// login sentence says (`aterm doctor --restore` keeps its `Reload` line); a setting it
    /// could not put back stays a failure, named as Restore's and its row's.
    #[test]
    fn a_restore_reaches_the_page_in_row_names_not_preference_keys() {
        use aterm_cli::macos_doctor::{Action, LOGIN, Report, RestoreRows, Row};
        use aterm_types::macos_setup::{PutBack, RestoreOutcome};
        const KEY: &str = "-g com.apple.keyboard.modifiermapping.1452-834-0";
        let report = |outcome: RestoreOutcome| Report {
            action: Action::Restore,
            rows: vec![
                Row {
                    name: KEY.into(),
                    status: "restored".into(),
                    detail: "original value restored".into(),
                    elapsed_ms: None,
                },
                Row {
                    name: "Caps Lock key".into(),
                    status: LOGIN.into(),
                    detail: "log out and back in to load the saved mappings".into(),
                    elapsed_ms: None,
                },
                Row {
                    name: "Reload Dock".into(),
                    status: "error".into(),
                    detail: "Dock did not come back".into(),
                    elapsed_ms: None,
                },
            ],
            manual_steps: Vec::new(),
            backup: std::path::PathBuf::from("original-preferences.json"),
            restore: Some(RestoreRows { outcome }),
            page: None,
        };
        let back = initial_setup_report_rows(&report(RestoreOutcome::of([
            (Some(Setting::CapsLock), PutBack::AtLogin),
            (Some(Setting::Dock), PutBack::AtLogin),
        ])));
        assert_eq!(
            back.iter()
                .map(|row| (row.name.as_str(), row.status.as_str()))
                .collect::<Vec<_>>(),
            [("Restore previous settings", "restored")]
        );
        assert_eq!(
            back[0].detail,
            "Restore previous settings finished. Caps Lock key and Dock come back at your next \
             login."
        );
        let failed = initial_setup_report_rows(&report(RestoreOutcome::of([(
            Some(Setting::CapsLock),
            PutBack::Failed,
        )])));
        assert_eq!(
            failed
                .iter()
                .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
                .collect::<Vec<_>>(),
            [(
                "Restore: Caps Lock key",
                "failed",
                "Caps Lock key could not be put back as it was."
            )]
        );
        for row in back.iter().chain(&failed) {
            for engine in ["com.apple", "Reload", "did not come back"] {
                assert!(
                    !row.name.contains(engine) && !row.detail.contains(engine),
                    "{row:?}"
                );
            }
        }
    }

    /// A Check reaches the page through its page as a pass does (design §3.6): a stage it could
    /// not confirm under its rows, with none of the stage's words, which `aterm doctor` keeps;
    /// and a change the journal could not log is no row of the page's at all.
    #[test]
    fn a_check_reaches_the_page_in_row_names_and_the_journal_row_never_does() {
        use aterm_cli::macos_doctor::{Action, JOURNAL_ROW, PassPage, Report, Row};
        let row = |name: &str, status: &str, detail: &str| Row {
            name: name.into(),
            status: status.into(),
            detail: detail.into(),
            elapsed_ms: None,
        };
        let report = Report {
            action: Action::Check,
            rows: vec![row(
                "Spotlight and Rust documentation",
                "unverified",
                "rustup: /usr/bin/env exited 1",
            )],
            manual_steps: Vec::new(),
            backup: std::path::PathBuf::from("original-preferences.json"),
            restore: None,
            page: Some(PassPage {
                outcome: aterm_types::macos_setup::PassOutcome::default(),
                rows: vec![row("Rust in Spotlight", "unverified", "")],
            }),
        };
        let said: Vec<(String, String, String)> = initial_setup_report_rows(&report)
            .into_iter()
            .map(|row| (row.name, row.status, row.detail))
            .collect();
        assert_eq!(
            said,
            [(
                "Check: Rust in Spotlight".to_owned(),
                "unverified".to_owned(),
                String::new()
            )]
        );
        let journal = Report {
            action: Action::Apply,
            rows: vec![
                row("Dock", "ok", "set"),
                row(JOURNAL_ROW, "error", "doctor.dock: disk full"),
            ],
            page: None,
            ..report
        };
        assert_eq!(journal.journal_refused(), Some("doctor.dock: disk full"));
        assert!(
            initial_setup_report_rows(&journal)
                .iter()
                .all(|row| row.name != JOURNAL_ROW),
            "the exceptions digest says it"
        );
    }

    /// A setup pass reaches the page in row names too (design §3.6): a change that did not take
    /// is a failure of its row, and what its rollback put back is one calm line in a Restore's
    /// words. `aterm doctor`'s rows, which name each key, stay in the terminal; the pass's
    /// other rows reach the page as they are.
    #[test]
    fn a_pass_reaches_the_page_in_row_names_not_preference_keys() {
        use aterm_cli::macos_doctor::{Action, PassPage, Report, Row};
        use aterm_types::macos_setup::{PassOutcome, PutBack, RestoreOutcome, Unfinished};
        let row = |name: &str, status: &str, detail: &str| Row {
            name: name.into(),
            status: status.into(),
            detail: detail.into(),
            elapsed_ms: None,
        };
        let repositories = row("GitHub repositories", "ok", "all 3 cloned");
        let report = Report {
            action: Action::Apply,
            rows: vec![
                row(
                    "Dock: com.apple.dock show-recents",
                    "error",
                    "absent → false",
                ),
                row(
                    "Preference rollback",
                    "restored",
                    "rolled back com.apple.dock show-recents",
                ),
                repositories.clone(),
            ],
            manual_steps: Vec::new(),
            backup: std::path::PathBuf::from("original-preferences.json"),
            restore: None,
            page: Some(PassPage {
                outcome: PassOutcome {
                    rolled_back: Some(RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)])),
                    unfinished: vec![(Setting::Dock, Unfinished::Failed)],
                },
                rows: vec![repositories],
            }),
        };
        let rows = initial_setup_report_rows(&report);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Dock", "error", "The change did not take."),
                (
                    "Undo",
                    "restored",
                    "aterm put back what it could not finish. Dock is as it was."
                ),
                ("GitHub repositories", "ok", "all 3 cloned"),
            ]
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
                    elapsed_ms: None,
                }],
                manual_steps: Vec::new(),
                backup: root.path().join("original-preferences.json"),
                restore: None,
                page: None,
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
        /// The next held reading unwinds after releasing the script lock.
        panic_read: bool,
        /// Stop saves begun: the run each names and the thread it ran on.
        stop_attempts: Vec<(u64, std::thread::ThreadId)>,
        /// Stop saves that landed.
        stops: Vec<u64>,
        /// Stop saves wait while this is set, as a loaded disk holds them.
        stop_hold: bool,
        /// Stop saves that fail before one lands.
        stop_failures: usize,
        /// An optional actual scratch Stop writer for admission handoff tests.
        stop_root: Option<std::path::PathBuf>,
        /// A person's choice asks for the claim only once this is clear.
        claim_hold: bool,
        /// Admissions of a person's choice begun.
        persists: usize,
        /// Admissions wait while this is set.
        persist_hold: bool,
        /// Each admission first finds this Mac's setup records held by another setup, as the
        /// profile lock's wait does ([`aterm_cli::macos_doctor::InitialSetupRestart::noting_wait`]).
        persist_finds_lock_held: bool,
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
        /// Follow-ups with Full Disk Access a recorded grant started.
        follow_ups: usize,
        /// Starts of the vault subscriber's mend asked for.
        mends: usize,
        /// Starts of the converge pass's window lane asked for.
        converge_lanes: usize,
        /// Converge passes the Secrets stage's end started.
        setup_converges: usize,
        /// What bringing a restored setup forward found.
        brought_forward: aterm_cli::macos_doctor::BroughtForward,
        /// Check again's looks at the LAN.
        lan_probes: usize,
        /// Startups run again in this process.
        startups: usize,
        /// Whether each requested startup worker was accepted by the spawner.
        startup_spawns: VecDeque<bool>,
        /// Startup continuations on a released Restore worker.
        startups_inline: usize,
        /// The answers, one an ask, to whether another setup holds the records (none: no).
        records_held: VecDeque<bool>,
        /// The widened mark ([`SETUP_WIDENED`]) as each run started.
        widened: Vec<u64>,
        /// The vault's materialization mark ([`SetupIo::vault_mark`]).
        vault_mark: Option<aterm_secrets::client::MaterializationMark>,
    }

    impl Script {
        const NEW: Self = Self {
            reads: Vec::new(),
            readings: VecDeque::new(),
            hold: false,
            panic_read: false,
            stop_attempts: Vec::new(),
            stops: Vec::new(),
            stop_hold: false,
            stop_failures: 0,
            stop_root: None,
            claim_hold: false,
            persists: 0,
            persist_hold: false,
            persist_finds_lock_held: false,
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
            follow_ups: 0,
            mends: 0,
            converge_lanes: 0,
            setup_converges: 0,
            brought_forward: aterm_cli::macos_doctor::BroughtForward::Nothing,
            lan_probes: 0,
            startups: 0,
            startup_spawns: VecDeque::new(),
            startups_inline: 0,
            records_held: VecDeque::new(),
            widened: Vec::new(),
            vault_mark: None,
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
        note_review_closed(None);
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
        *GITHUB_TOKEN_MARK
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
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
                mend_subscriber: || change(|script| script.mends += 1),
                converge_lane: || change(|script| script.converge_lanes += 1),
                converge_setup: || change(|script| script.setup_converges += 1),
                put_back: |_| Ok(false),
                start_follow_up: || change(|script| script.follow_ups += 1),
                bring_forward: || Ok(script().brought_forward),
                probe_lan: || change(|script| script.lan_probes += 1),
                start_startup: || {
                    let mut script = script();
                    script.startups += 1;
                    script.startup_spawns.pop_front().unwrap_or(true)
                },
                start_startup_inline: || change(|script| script.startups_inline += 1),
                records_held: || script().records_held.pop_front().unwrap_or(false),
                vault_mark: || script().vault_mark,
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
            let panics = std::mem::take(&mut script.panic_read);
            SCRIPT_MOVED.notify_all();
            drop(wait_while(script, |script| script.hold));
            assert!(!panics, "injected reader unwind");
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
            let root = script.stop_root.clone();
            drop(script);
            if let Some(root) = root {
                aterm_cli::macos_doctor::ProvisionalSetupWatchTest::at(&root)
                    .stop_revision(revision)?;
            }
            let mut script = self::script();
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
            restart: &mut aterm_cli::macos_doctor::InitialSetupRestart,
            _: &Plan,
            _: bool,
        ) -> aterm_cli::macos_doctor::InitialSetupObserved<Option<u64>> {
            if script().persist_finds_lock_held {
                restart.finds_lock_held_for_test();
            }
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
            change(|script| {
                script.runs.push(revision);
                script
                    .widened
                    .push(SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire));
            });
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

    /// The window recording Full Disk Access granted starts the follow-up that puts back what
    /// waited for it, once per grant; no other step the window records (handed, declined,
    /// triggered, nor any other access granted) starts one.
    #[test]
    fn recording_full_disk_access_granted_starts_the_follow_up_once() {
        use aterm_types::macos_setup::{Access, AccessOutcome};
        let _globals = globals();
        let _io = TestIo::install();
        // Keep a known plan whose personal/network groups are left as is: the
        // recovery fact requests Access rows alone and grants no broader read.
        OBSERVED_ROWS.lock().unwrap().plan = Some(left_as_is());
        for access in Access::ALL {
            for outcome in [
                AccessOutcome::Handed,
                AccessOutcome::Declined,
                AccessOutcome::Triggered,
                AccessOutcome::Granted,
            ] {
                if (access, outcome) != (Access::FullDiskAccess, AccessOutcome::Granted) {
                    access_step_recorded(1, access, outcome);
                }
            }
        }
        assert_eq!(script().follow_ups, 0);
        access_step_recorded(1, Access::FullDiskAccess, AccessOutcome::Granted);
        assert_eq!(script().follow_ups, 1);
        access_step_recorded(1, Access::FullDiskAccess, AccessOutcome::Declined);
        assert_eq!(script().follow_ups, 1);
        // The same verified fact without an access revision has the same
        // read/wake/recovery effects, without inventing a recorded revision.
        let before = SETUP_WAKES.count();
        verified_full_disk_access_granted();
        assert_eq!(script().follow_ups, 2);
        assert!(SETUP_WAKES.count() > before);
        wait_until("the Access rows' readings", observe_idle);
        let reads = script().reads.clone();
        assert!(!reads.is_empty());
        assert!(reads.iter().all(|(_, subjects, network)| {
            *subjects == Access::ALL.map(Subject::Access) && !network
        }));
    }

    /// A Restore that left aterm's own permissions with no admitted run to carry them (one that
    /// let the launch's offer go unanswered) is offered again in this launch; so is one whose
    /// bring-forward met the profile lock busy, which the startup waits for. A Restore whose run
    /// carries them starts nothing.
    #[test]
    fn a_restore_that_left_no_run_for_the_permissions_is_offered_again_in_this_launch() {
        use aterm_cli::macos_doctor::BroughtForward;
        let _globals = globals();
        let _io = TestIo::install();
        offer_again_after_restore();
        assert_eq!(script().startups, 0, "nothing brought forward");
        change(|script| script.brought_forward = BroughtForward::Run(3));
        offer_again_after_restore();
        assert_eq!(script().startups, 1);
        change(|script| script.brought_forward = BroughtForward::Busy);
        offer_again_after_restore();
        assert_eq!(script().startups, 2, "the startup waits for the lock");
    }

    /// A launch whose setup has finished starts the follow-up with Full Disk Access on a thread
    /// of its own: its wait for a profile lock another pass holds never holds up what the
    /// startup does next (the watch for the ALab vault).
    #[test]
    fn a_finished_launch_starts_its_follow_up_beside_the_startup() {
        let _globals = globals();
        let _io = TestIo::install();
        run_prepared_setup(PreparedSetup::Finished, None, StartupSent::default());
        assert_eq!(script().follow_ups, 1);
        assert_eq!(script().mends, 1, "the vault subscriber's mend");
        assert_eq!(
            script().converge_lanes,
            1,
            "the converge pass's window lane"
        );
    }

    /// A launch that resumes an admitted run starts the vault subscriber's mend and the converge
    /// pass's window lane before the run: the run may wait on a person past its done Secrets
    /// stage for as long as they take, which would leave a subscriber whose program is gone dead
    /// meanwhile, and the Mac with no window pass for the process's life.
    /// NEGATIVE CONTROL: a resumed run that starts no lane fails here.
    #[test]
    fn a_launch_resuming_a_run_starts_the_vault_mend_and_the_converge_lane() {
        let _globals = globals();
        let _io = TestIo::install();
        let claim = StartupClaim::claim().unwrap().unwrap();
        run_prepared_setup(
            PreparedSetup::Authorized { claim, revision: 8 },
            None,
            StartupSent::default(),
        );
        assert_eq!(script().runs, [8]);
        assert_eq!(script().mends, 1);
        assert_eq!(
            script().converge_lanes,
            1,
            "the converge pass's window lane"
        );
        clear_setup_run(8);
    }

    /// A launch whose setup runs nothing still starts the converge pass's window lane, since no
    /// run's end starts it later in this process: one unavailable (an update's successor that
    /// withholds the review), or a Cancel recorded for good. A review closed unanswered ("not
    /// this launch") starts nothing.
    /// NEGATIVE CONTROL: an unavailable or cancelled launch that starts no lane fails here.
    #[test]
    fn a_launch_with_no_run_still_starts_the_converge_lane() {
        let _globals = globals();
        let _io = TestIo::install();
        run_prepared_setup(PreparedSetup::Unavailable, None, StartupSent::default());
        assert_eq!(script().converge_lanes, 1, "unavailable");
        for (prepared, lanes) in [(PreparedSetup::Cancelled, 1), (PreparedSetup::Closed, 0)] {
            let _io = TestIo::install();
            run_prepared_startup_with(prepared, None, StartupSent::default(), |_| {
                panic!("no watch for a review that ended")
            });
            assert_eq!(script().converge_lanes, lanes, "lanes {lanes}");
            assert!(script().runs.is_empty());
        }
    }

    /// The converge pass runs as the Secrets stage ends: not as it starts, and not at another
    /// stage's end.
    /// NEGATIVE CONTROL: a check that ignores `running` starts a pass as the stage starts too.
    #[test]
    fn the_secrets_stage_starts_a_converge_pass_as_it_ends() {
        use aterm_cli::macos_doctor::StageEvent;
        use aterm_types::macos_setup::Setting;
        let event = |settings: &[Setting], running| {
            ends_secrets_stage(&StageEvent {
                settings,
                subjects: &[],
                running,
            })
        };
        assert!(event(&[Setting::Secrets], false), "the Secrets stage ended");
        assert!(!event(&[Setting::Secrets], true), "it started");
        assert!(
            !event(&[Setting::Repositories], false),
            "another stage ended"
        );
        assert!(!event(&[], false), "a stage with nothing selected");
    }

    /// Check again looks at the LAN before it reads the Local Network row, so the row reads what
    /// macOS answers now; a reading nobody forced, or a Check of other rows, never looks.
    #[test]
    fn check_again_looks_at_the_lan_before_it_reads_local_network() {
        use aterm_types::macos_setup::Access;
        let _globals = globals();
        let _io = TestIo::install();
        let lan = vec![Subject::Access(Access::LocalNetwork)];
        for (forced, subjects, looks) in [
            (false, lan.clone(), 0),
            (true, vec![Subject::Setting(Setting::Dock)], 0),
            (true, lan, 1),
        ] {
            let choices = OBSERVED_ROWS.lock().unwrap().choices;
            observe_rows(ObserveRequest {
                plan: Some(left_as_is()),
                choices,
                subjects,
                forced,
            });
            assert_eq!(std::mem::take(&mut script().lan_probes), looks, "{forced}");
        }
    }

    #[test]
    fn vault_sync_guard_preserves_a_local_late_stop_and_refuses_unreadable_state() {
        use aterm_cli::macos_doctor::{
            InitialSetupAdmissionStopTest, with_initial_setup_vault_sync_at,
        };
        use std::cell::Cell;

        let _globals = globals();
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        let revision = setup.complete_vault_setup().unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        register_admitted_run(revision, send_admission(), false, None);
        with_setup_run(revision, || {});
        let effects = Cell::new(0);
        let mend = || {
            while_vault_sync_kept_with(&mut || effects.set(effects.get() + 1), |work| {
                with_initial_setup_vault_sync_at(root.path(), work)
            })
        };
        assert!(mend());
        assert_eq!(effects.get(), 1);

        assert!(stop_initial_setup());
        wait_until("the completed run's late Stop is saved", stop_worker_idle);
        assert!(setup.snapshot().unwrap().finished);
        assert_eq!(
            with_initial_setup_vault_sync_at(root.path(), |_| true).unwrap(),
            Some(true),
            "the completed backend run spends the Stop; this process still holds it locally"
        );
        assert!(!mend());
        assert_eq!(effects.get(), 1, "no mend effect crosses the local Stop");

        let sent = send_admission();
        let next = setup.choose().unwrap();
        assert!(next > revision);
        register_admitted_run(next, sent, false, None);
        with_setup_run(next, || {});
        assert!(mend(), "a new accepted choice admits its own effects");
        assert_eq!(effects.get(), 2);

        let path = root.path().join("initial-setup.json");
        let saved = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"{\"profile\":").unwrap();
        assert!(!mend(), "an unreadable run certifies no mend authority");
        assert_eq!(effects.get(), 2);
        std::fs::write(&path, saved).unwrap();
        assert!(mend(), "a repaired record is read again on the next turn");
        assert_eq!(effects.get(), 3);
        assert!(script().runs.is_empty(), "the guard starts no setup stage");
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
        fn stopped(&self) -> bool {
            stop_covers(self.sent.sent)
        }
        fn wait_holding_claim(&mut self) -> bool {
            change(|script| script.retry_waits += 1);
            // Twice the hang detector: a choice that had to wait it out
            // fails the test before the wait ends.
            let mut seen = SETUP_WAKES.count();
            wait_before_admission_retry(self.sent.sent, &mut seen, HANG * 2, |seen, limit| {
                if !choice_awaits_claim() {
                    SETUP_WAKES.back_off(seen, limit);
                }
            });
            choice_awaits_claim()
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
                Ok(PreparedSetup::Cancelled) => "cancelled",
                Ok(PreparedSetup::Closed) => "closed",
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

    /// A Stop that holds the run a watch widened, asked for the plan it widened or pressed since
    /// the watch looked, marks that run for its pass to put back the plan it widened (System
    /// State design §3.5). A run a later choice overtook, a choice's own run, and a widened run
    /// nobody stopped are not marked.
    #[test]
    fn only_a_widened_run_a_stop_holds_puts_back_its_plan() {
        let _globals = globals();
        let _io = TestIo::install();
        let held = |stop_before: bool, stop_after: bool, overtaken: bool, widened: bool| {
            wait_until("no Stop is owed", stop_worker_idle);
            reset_worker_state();
            set_setup_run(Some(7));
            if stop_before {
                assert!(stop_initial_setup());
            }
            let look = send_admission();
            if stop_after {
                assert!(stop_initial_setup());
            }
            let held = register_admitted_run(8, look, overtaken, widened.then_some(7));
            assert_eq!(
                setup_stop_requested(8),
                stop_before && widened || stop_after || overtaken,
                "the run is stopped as it registers"
            );
            held
        };
        assert!(
            held(true, false, false, true),
            "a Stop of the plan it widened"
        );
        assert!(
            held(false, true, false, true),
            "a Stop since the watch looked"
        );
        assert!(
            !held(false, true, true, true),
            "a later choice replaces the plan"
        );
        assert!(!held(false, true, false, false), "a choice's own run");
        assert!(!held(false, false, false, true), "nobody stopped it");
        wait_until("no Stop is owed", stop_worker_idle);
        reset_worker_state();
    }

    /// A run aterm widened says so on the page while it goes (System State design §3.5), and no
    /// longer once its worker has ended: a later worker the slot keeps busy, which the page
    /// counts as a run (a put-back once Full Disk Access is granted), never shows the note
    /// again.
    #[test]
    fn the_widened_note_goes_when_its_run_ends() {
        let _globals = globals();
        let _io = TestIo::install();
        let claim = StartupClaim::claim().unwrap().unwrap();
        let sent = StartupSent {
            widened_from: Some(7),
            ..StartupSent::default()
        };
        run_prepared_setup(PreparedSetup::Authorized { claim, revision: 8 }, None, sent);
        assert_eq!(script().runs, [8]);
        assert_eq!(script().widened, [8], "said while the run goes");
        assert_eq!(
            SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire),
            0,
            "the run ended"
        );
        assert!(!initial_setup_status().widened);
        clear_setup_run(8);
    }

    /// A saved setup aterm could not read just now is one calm line, no failure (System State
    /// design §3.6): aterm reads it again by itself.
    #[test]
    fn a_saved_setup_unread_just_now_is_one_calm_line() {
        let status = initial_setup_status_with(
            Err(InitialSetupSnapshotError::Read(
                "automatic-plan.json: Permission denied".into(),
            )),
            false,
            &InitialSetupRows::default(),
        );
        assert!(status.plan.is_none());
        assert_eq!(
            status.rows,
            [aterm_gui::calm_setup_row(aterm_gui::STATE_UNREAD_NOTE)]
        );
        assert!(status.rows.iter().all(|row| row.status != "failed"));
    }

    /// A newer aterm's saved setup is one calm line that says so, no failure, and no read again
    /// by itself: this aterm leaves that setup as it is.
    #[test]
    fn a_newer_aterms_saved_setup_is_one_calm_line_read_no_more() {
        let status = initial_setup_status_with(
            Err(InitialSetupSnapshotError::Newer),
            false,
            &InitialSetupRows::default(),
        );
        assert!(status.plan.is_none() && status.permissions.is_none());
        assert_eq!(
            status.rows,
            [aterm_gui::calm_setup_row(aterm_gui::NEWER_SETUP_NOTE)]
        );
        assert!(aterm_cli::macos_doctor::setup_is_newer(
            &InitialSetupSnapshotError::Newer.to_string()
        ));
    }

    /// A Join refused because aterm cannot read this Mac's vault settings is one calm line that
    /// says so, never a failure with Set up now: pressing it again meets the same settings. Any
    /// other setup error, and a Check's, is still the failure it was.
    #[test]
    fn a_join_refused_for_unread_vault_settings_is_one_calm_line() {
        let refused = "cannot read which vault this Mac is in: source.toml: Permission denied";
        assert_eq!(
            setup_error_row(refused, false),
            aterm_gui::calm_setup_row(aterm_gui::VAULT_UNREAD_NOTE)
        );
        assert_eq!(setup_error_row(refused, true).status, "failed");
        let other = setup_error_row("the plan could not be saved", false);
        assert_eq!(
            (other.name.as_str(), other.status.as_str()),
            (aterm_gui::SETUP_ROW, "failed")
        );
    }

    /// The launch's Join that aterm admitted without the Join is said in one calm line while
    /// the Secrets row still reads that aterm cannot read this Mac's vault settings, and only
    /// then: once they read, the row says how the vault stands, and the typed vault is the
    /// person's to Join.
    #[test]
    fn a_startup_join_left_unjoined_is_said_while_the_vault_settings_stay_unread() {
        use aterm_types::macos_setup::{Observation, Setting, Subject};
        let secrets = |note: Option<&str>| Observation {
            note: note.map(str::to_owned),
            ..Observation::unknown(Subject::Setting(Setting::Secrets))
        };
        let status = |observed: Vec<Observation>| aterm_gui::SetupStatus {
            plan: Some(Plan::default()),
            observed,
            ..aterm_gui::SetupStatus::default()
        };
        let unread = aterm_cli::macos_doctor::VAULT_SETTINGS_UNREAD;
        let mut said = status(vec![secrets(Some(unread))]);
        add_vault_not_joined(&mut said, true);
        assert_eq!(
            said.rows,
            [aterm_gui::calm_setup_row(aterm_gui::VAULT_NOT_JOINED_NOTE)]
        );
        for (not_joined, observed) in [
            (false, vec![secrets(Some(unread))]),
            (true, vec![secrets(None)]),
            (true, Vec::new()),
        ] {
            let mut quiet = status(observed);
            add_vault_not_joined(&mut quiet, not_joined);
            assert!(quiet.rows.is_empty());
        }
    }

    /// A step of the launch's setup that failed is tried again by itself: the page says so in
    /// one calm line with every failure kept off the menu line and the tab (System State design
    /// §3.5, §3.6); ten minutes on, once, the exceptions digest says setup has not started, and
    /// the entry goes once the startup goes on.
    #[test]
    fn a_startup_that_tries_again_is_calm_and_told_to_the_digest_ten_minutes_on() {
        let start = Instant::now();
        let mut retry = StartupRetry::NONE;
        assert!(!retry.failed(start));
        assert!(retry.retrying());
        assert!(!retry.failed(start + Duration::from_secs(9 * 60)));
        assert!(retry.failed(start + STARTUP_STUCK_AFTER));
        assert!(!retry.failed(start + STARTUP_STUCK_AFTER * 2), "once");
        assert!(retry.went_on(), "its entry goes");
        assert_eq!(retry, StartupRetry::NONE);
        assert!(!retry.went_on());
        assert!(
            !retry.failed(start + STARTUP_STUCK_AFTER * 3),
            "counted anew"
        );

        let mut status = aterm_gui::SetupStatus {
            plan: Some(Plan::default()),
            ..aterm_gui::SetupStatus::default()
        };
        add_startup_retry(&mut status, false);
        assert!(!status.retrying && status.rows.is_empty());
        add_startup_retry(&mut status, true);
        assert!(status.retrying);
        assert_eq!(
            status.rows,
            [aterm_gui::calm_setup_row(aterm_gui::STARTUP_RETRY_NOTE)]
        );
        // A saved setup it cannot read says so in its own line alone.
        let mut unread = initial_setup_status_with(
            Err(InitialSetupSnapshotError::Read("unread".into())),
            false,
            &InitialSetupRows::default(),
        );
        add_startup_retry(&mut unread, true);
        assert_eq!(
            unread.rows,
            [aterm_gui::calm_setup_row(aterm_gui::STATE_UNREAD_NOTE)]
        );
    }

    /// A Restore that failed as a whole says why in the page's words: one that cannot read the
    /// settings aterm saved before setup changed them names where to learn why, since pressing
    /// it again reads the same record.
    #[test]
    fn a_whole_restore_failure_says_why_in_the_pages_words() {
        let unread = restore_failed_row(&format!(
            "{}: backup: expected value",
            aterm_cli::macos_doctor::BACKUP_UNREAD
        ));
        assert_eq!(unread.detail, aterm_gui::RESTORE_BACKUP_UNREAD_NOTE);
        assert_eq!(unread.status, "failed");
        let other = restore_failed_row("state.json: No space left on device");
        assert_eq!(other.detail, aterm_gui::RESTORE_FAILED_NOTE);
        assert_eq!(unread.name, other.name);
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
        assert!(stop_initial_setup());
        wait_until("completed run 8's Stop is saved", stop_worker_idle);
        assert_eq!(script().stops, [8]);
        change(|script| script.admits = [Some(9)].into());
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        wait_until("run 9 starts", || script().runs.len() == 2);
        assert!(!setup_stop_requested(9));
        assert!(settle_setup_stop(Duration::ZERO), "nothing is owed");
        assert_eq!(script().stops, [8], "the earlier Stop never names run 9");
    }

    /// While the GitHub repositories need a person, System State's look for a renewed token
    /// reads that row alone, and only once the vault's mark moved since the row was last read:
    /// an unchanged mark, or one that cannot be read, reads nothing and asks nothing of GitHub
    /// (design §3.6).
    #[test]
    fn a_look_for_a_renewed_token_reads_github_alone_once_its_mark_moved() {
        use aterm_secrets::client::MaterializationMark;
        let _globals = globals();
        let _io = TestIo::install();
        let mark =
            |byte: u8| MaterializationMark::from_cache_key(&format!("{byte:02x}").repeat(32));
        script().vault_mark = mark(1);
        let plan = Plan::default();
        // The page's reading of every row reads GitHub too, and keeps the vault's mark.
        assert!(observe_initial_setup(plan.clone()));
        wait_until("the page's reading ends", || {
            script().reads.len() == 1 && observe_idle()
        });
        assert!(
            !read_github_again_if_its_token_moved(plan.clone()),
            "an unchanged mark"
        );
        script().vault_mark = None;
        assert!(
            !read_github_again_if_its_token_moved(plan.clone()),
            "a mark that cannot be read"
        );
        // A token renewed in the vault reached this Mac.
        script().vault_mark = mark(2);
        assert!(read_github_again_if_its_token_moved(plan.clone()));
        wait_until("the row's reading ends", || {
            script().reads.len() == 2 && observe_idle()
        });
        assert!(
            !read_github_again_if_its_token_moved(plan.clone()),
            "once per move"
        );
        let reads = std::mem::take(&mut script().reads);
        assert_eq!(reads.len(), 2, "{reads:?}");
        assert_eq!(
            reads[1],
            (plan, vec![Subject::Setting(Setting::Repositories)], true),
            "GitHub repositories alone, over the network"
        );
    }

    #[test]
    fn a_token_look_retries_a_refused_observation_start() {
        use aterm_secrets::client::MaterializationMark;
        let _globals = globals();
        let _io = TestIo::install();
        let mark =
            |byte: u8| MaterializationMark::from_cache_key(&format!("{byte:02x}").repeat(32));
        script().vault_mark = mark(1);
        let plan = Plan::default();
        assert!(observe_initial_setup(plan.clone()));
        wait_until("the original token's reading ends", || {
            script().reads.len() == 1 && observe_idle()
        });

        script().vault_mark = mark(2);
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        let refused =
            read_github_again_if_its_token_moved_with(plan.clone(), |plan, subjects, forced| {
                queue_observation_with(
                    ObserveRequest {
                        plan,
                        choices,
                        subjects,
                        forced,
                    },
                    |_| {
                        Err(std::io::Error::other(
                            "injected token observation spawn refusal",
                        ))
                    },
                )
            });
        assert!(!refused);
        assert!(observe_idle());
        assert_eq!(script().reads.len(), 1, "no worker read the renewed token");

        let mut worker = None;
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let accepted =
            read_github_again_if_its_token_moved_with(plan.clone(), |plan, subjects, forced| {
                queue_observation_with(
                    ObserveRequest {
                        plan,
                        choices,
                        subjects,
                        forced,
                    },
                    |job| {
                        worker = Some(std::thread::spawn(move || {
                            held.recv_timeout(HANG).unwrap();
                            job();
                        }));
                        Ok(())
                    },
                )
            });
        // An accepted reading still waiting to run owns this mark. Another
        // look must not enqueue a duplicate before the worker samples it.
        let repeated = read_github_again_if_its_token_moved(plan.clone());
        if let Some(worker) = worker {
            release.send(()).unwrap();
            worker.join().unwrap();
        }
        assert!(
            accepted,
            "the same renewed token still needs its refused reading"
        );
        assert!(
            !repeated,
            "the accepted observation already owns this token move"
        );
        assert!(observe_idle());
        assert_eq!(
            script().reads,
            [
                (plan.clone(), Subject::all(), true),
                (
                    plan.clone(),
                    vec![Subject::Setting(Setting::Repositories)],
                    true
                ),
            ],
            "the recovered observation reads only the requested source and row"
        );
        assert!(!read_github_again_if_its_token_moved(plan));
        assert_eq!(
            script().reads.len(),
            2,
            "one accepted reading per token move"
        );
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

    /// Restore the queue even when an original-behavior assertion fails, so
    /// TestIo's teardown never waits for a worker that failed to start or died.
    struct ObserverDispatchReset;

    impl Drop for ObserverDispatchReset {
        fn drop(&mut self) {
            let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running = false;
            queue.next = None;
            queue.check = None;
        }
    }

    #[test]
    fn observer_dispatch_does_not_acknowledge_work_to_a_failed_start() {
        let _globals = globals();
        let _io = TestIo::install();
        let _queue = ObserverDispatchReset;
        let mut plan = left_as_is();
        plan.vault_url = "https://example.invalid/forced-current".into();
        let expected = plan.clone();
        let request = ObserveRequest {
            plan: Some(plan),
            choices: OBSERVED_ROWS.lock().unwrap().choices,
            subjects: vec![Subject::Setting(Setting::Repositories)],
            forced: true,
        };
        let mut second = None;
        let started = queue_observation_with(
            ObserveRequest {
                plan: Some(left_as_is()),
                choices: request.choices,
                subjects: vec![Subject::Setting(Setting::Dock)],
                forced: false,
            },
            |_| {
                let (reached, wait) = std::sync::mpsc::sync_channel(1);
                second = Some(std::thread::spawn(move || {
                    // Coordinate without a timing assertion: if admission is
                    // unlocked, force the second call to finish before the
                    // failed spawn returns. If locked, signal before waiting
                    // for the first caller to release its failed admission.
                    let unlocked = match OBSERVE_QUEUE.try_lock() {
                        Ok(queue) => {
                            drop(queue);
                            true
                        }
                        Err(std::sync::TryLockError::WouldBlock) => false,
                        Err(std::sync::TryLockError::Poisoned(_)) => panic!("queue poisoned"),
                    };
                    if !unlocked {
                        reached.send(()).unwrap();
                    }
                    let mut worker = None;
                    let accepted = queue_observation_with(request, |job| {
                        worker = Some(std::thread::spawn(job));
                        Ok(())
                    });
                    if unlocked {
                        reached.send(()).unwrap();
                    }
                    (accepted, worker)
                }));
                wait.recv_timeout(HANG).unwrap();
                Err(std::io::Error::other("injected worker spawn refusal"))
            },
        );
        assert!(!started, "the failed caller reports refusal");
        let (accepted, worker) = second.take().unwrap().join().unwrap();
        assert!(accepted);
        let ran = worker.is_some();
        if let Some(worker) = worker {
            worker.join().unwrap();
        }
        assert!(
            ran,
            "accepted work must have a worker after the failed start"
        );
        let script = script();
        assert_eq!(script.reads.len(), 1);
        assert_eq!(script.reads[0].0, expected);
        assert_eq!(script.reads[0].1, [Subject::Setting(Setting::Repositories)]);
        assert!(
            script.reads[0].2,
            "the accepted Check retains its forced scope"
        );
        assert!(observe_idle());
    }

    #[test]
    fn observer_dispatch_retires_an_unwound_read_and_drains_the_next_check_once() {
        use aterm_types::macos_setup::Standing;

        let _globals = globals();
        for panics in [false, true] {
            let _io = TestIo::install();
            let _queue = ObserverDispatchReset;
            let preference = Subject::Setting(Setting::Dock);
            let personal = Subject::Setting(Setting::Messages);
            let independent = Subject::Setting(Setting::Spotlight);
            let repositories = Subject::Setting(Setting::Repositories);
            let seen = |subject, standing| Observation {
                standing,
                ..Observation::unknown(subject)
            };
            let old = seen(preference, Standing::InPlace);
            let private = seen(personal, Standing::InPlace);
            let kept = seen(independent, Standing::NeedsYou);
            let fresh = seen(repositories, Standing::InPlace);
            let choices = {
                let mut rows = OBSERVED_ROWS.lock().unwrap();
                rows.rows = vec![old.clone(), private.clone(), kept.clone()];
                rows.choices
            };
            change(|script| {
                script.hold = true;
                script.panic_read = panics;
                script.readings = [vec![old], vec![fresh.clone()]].into();
            });
            let mut worker = None;
            assert!(queue_observation_with(
                ObserveRequest {
                    plan: Some(left_as_is()),
                    choices,
                    subjects: vec![preference, personal],
                    forced: false,
                },
                |job| {
                    worker = Some(std::thread::spawn(job));
                    Ok(())
                },
            ));
            wait_until("the observer reads its first batch", || {
                script().reads.len() == 1
            });
            let mut next = left_as_is();
            next.vault_url = "https://example.invalid/next-check".into();
            assert!(queue_observation_with(
                ObserveRequest {
                    plan: Some(next.clone()),
                    choices,
                    subjects: vec![repositories],
                    forced: true,
                },
                |_| panic!("the current worker owns the queued Check"),
            ));
            release_readings();
            let finished = worker.take().unwrap().join();
            assert!(
                finished.is_ok(),
                "one read cannot strand the accepted Check"
            );
            assert!(observe_idle());
            let rows = OBSERVED_ROWS.lock().unwrap();
            let current = rows
                .rows
                .iter()
                .find(|row| row.subject == preference)
                .unwrap();
            assert_eq!(
                current.standing,
                if panics {
                    Standing::Unknown
                } else {
                    Standing::InPlace
                }
            );
            if panics {
                // The words the page shows under the row, which say to Check again.
                assert!(
                    current
                        .note
                        .as_ref()
                        .is_some_and(|note| note.contains("Check again"))
                );
            }
            assert!(
                rows.rows.contains(&private),
                "filtered-out personal rows stay untouched"
            );
            assert!(rows.rows.contains(&kept));
            assert!(rows.rows.contains(&fresh));
            assert_eq!(rows.plan, Some(next.clone()));
            drop(rows);
            let script = script();
            assert_eq!(script.reads.len(), 2, "the unwound batch is never retried");
            assert_eq!(script.reads[0].1, [preference]);
            assert_eq!(script.reads[1].0, next);
            assert_eq!(script.reads[1].1, [repositories]);
            assert!(script.reads[1].2, "the queued Check keeps its own scope");
        }
    }

    #[test]
    fn queued_observation_intents_keep_a_draft_check_across_a_focus_refresh() {
        use aterm_types::macos_setup::Standing;

        let _globals = globals();
        let _io = TestIo::install();
        let _busy = take_initial_setup_slot().unwrap();
        let mut shown = left_as_is();
        shown.vault_url = "https://example.invalid/accepted-a".into();
        let mut checked = shown.clone();
        checked.vault_url = "https://example.invalid/draft-b".into();
        let vault = Subject::Setting(Setting::Secrets);
        let accepted = Observation {
            standing: Standing::InPlace,
            ..Observation::unknown(vault)
        };
        let draft = Observation {
            standing: Standing::NeedsYou,
            ..Observation::unknown(vault)
        };
        change(|script| {
            script.hold = true;
            script.readings = [vec![accepted.clone()], vec![accepted], vec![draft.clone()]].into();
        });
        assert!(observe_initial_setup(shown.clone()));
        wait_until("the accepted source's first reading starts", || {
            script().reads.len() == 1
        });

        // The actual Check producer acknowledges its forced reading even
        // while another pass owns the separate check worker's slot.
        let started = check_initial_setup(checked.clone());
        assert!(started.reading);
        assert!(!started.check);
        // The Host also asks for the reviewed plan's ordinary fallback, then
        // a genuine focus return asks for the accepted source once more.
        assert!(observe_initial_setup(checked.clone()));
        assert!(observe_initial_setup(shown.clone()));
        release_readings();
        wait_until("the accepted observation work finishes", observe_idle);

        let reads = std::mem::take(&mut script().reads);
        assert_eq!(reads.len(), 3, "both pending intents must be read");
        assert_eq!((&reads[0].0, reads[0].2), (&shown, false));
        assert_eq!((&reads[1].0, reads[1].2), (&shown, false));
        assert_eq!((&reads[2].0, reads[2].2), (&checked, true));
        for personal in [Setting::Messages, Setting::FaceTime] {
            assert!(!reads[1].1.contains(&Subject::Setting(personal)));
            assert!(reads[2].1.contains(&Subject::Setting(personal)));
        }
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert_eq!(rows.plan.as_ref(), Some(&checked));
        assert_eq!(rows.vault_url.as_deref(), Some(checked.vault_url.as_str()));
        assert!(rows.rows.contains(&draft));
    }

    #[test]
    fn queued_observation_intents_keep_only_the_latest_check_until_a_new_choice() {
        let _globals = globals();
        for new_choice in [false, true] {
            let _io = TestIo::install();
            let busy = take_initial_setup_slot().unwrap();
            let mut shown = left_as_is();
            shown.vault_url = "https://example.invalid/accepted-a".into();
            let mut checked = shown.clone();
            checked.vault_url = "https://example.invalid/draft-b".into();
            change(|script| script.hold = true);
            assert!(observe_initial_setup(shown.clone()));
            wait_until("the old source reading starts", || {
                script().reads.len() == 1
            });
            for source in ["draft-b", "draft-c"] {
                checked.vault_url = format!("https://example.invalid/{source}");
                assert_eq!(
                    check_initial_setup(checked.clone()),
                    aterm_gui::CheckStarted {
                        reading: true,
                        check: false,
                    }
                );
                assert!(observe_initial_setup(checked.clone()));
                assert!(observe_initial_setup(shown.clone()));
            }
            let mut chosen = shown.clone();
            chosen.vault_url = "https://example.invalid/new-choice".into();
            drop(busy);
            if new_choice {
                script().admits = [Some(9)].into();
                let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
                assert!(restart_initial_setup(chosen.clone(), false, None, receipt));
                assert_eq!(answer.recv_timeout(HANG), Ok(true));
                wait_until("the new choice's inert run finishes", || {
                    !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
                });
                assert!(observe_initial_setup(chosen.clone()));
            }
            release_readings();
            wait_until("the latest pending reads finish", observe_idle);
            let reads = std::mem::take(&mut script().reads);
            if new_choice {
                assert_eq!(reads.len(), 2);
                assert_eq!((&reads[1].0, reads[1].2), (&chosen, false));
                assert!(!reads[1].1.contains(&Subject::Setting(Setting::Messages)));
                assert!(!reads[1].1.contains(&Subject::Setting(Setting::FaceTime)));
                assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(chosen));
            } else {
                assert_eq!(reads.len(), 3);
                assert_eq!((&reads[1].0, reads[1].2), (&shown, false));
                assert_eq!((&reads[2].0, reads[2].2), (&checked, true));
                assert!(reads[2].1.contains(&Subject::Setting(Setting::Messages)));
                assert!(reads[2].1.contains(&Subject::Setting(Setting::FaceTime)));
                assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(checked));
            }
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
                queue.check = None;
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
            assert!(request_observation(
                Some(checked.clone()),
                Subject::all(),
                true
            ));
            assert!(observe_initial_setup(reviewed.clone()));
            let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
            for request in pending.into_iter().flatten() {
                observe_rows(request);
            }
            let reads = std::mem::take(&mut script().reads);
            assert_eq!(reads.len(), 1 + usize::from(edited));
            if edited {
                assert_eq!((&reads[0].0, reads[0].2), (&reviewed, false));
                assert!(
                    reads[0].1.iter().all(|subject| !matches!(
                        subject,
                        Subject::Setting(setting) if setting.group() == Group::Accounts
                    )),
                    "account reads cannot transfer to the ordinary plan"
                );
            }
            let forced = reads.last().unwrap();
            assert_eq!((&forced.0, forced.2), (&checked, true));
            assert!(forced.1.contains(&Subject::Setting(Setting::Messages)));
            assert!(forced.1.contains(&Subject::Setting(Setting::FaceTime)));
            // A fresh Check of that exact edited plan still reads all rows,
            // even when an ordinary stage refresh coalesces behind it.
            assert!(request_observation(
                Some(reviewed.clone()),
                Subject::all(),
                true
            ));
            assert!(request_observation(None, Subject::all(), false));
            let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
            for request in pending.into_iter().flatten() {
                observe_rows(request);
            }
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
                queue.check = None;
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
            let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
            for request in pending.into_iter().flatten() {
                observe_rows(request);
            }
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
                queue.check = None;
            }
        }
        {
            let mut queue = OBSERVE_QUEUE.lock().unwrap();
            assert!(!queue.running);
            assert!(queue.next.is_none() && queue.check.is_none());
            queue.running = true;
        }
        let _queue = QueueLease;
        let secrets = Subject::Setting(Setting::Secrets);
        let repositories = Subject::Setting(Setting::Repositories);
        let reading = |subject, standing, note: &str| Observation {
            subject,
            standing,
            now: None,
            after_login: None,
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
                    queue.check = None;
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
                queue.check = None;
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
                finish_withheld_preferences_with(|reserve, _stopped| {
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
                    finish_withheld_preferences_with(|reserve, _stopped| {
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
        assert_eq!(finish_withheld_preferences_with(|_, _| None::<()>), None);
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(SETUP_ACTIVITY.lock().unwrap().epoch, epoch);
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert_eq!(rows.rows, [widgets]);
        assert_eq!(rows.vault_url, Some(source));
        assert_eq!(rows.generation, generation);
        assert!(script().reads.is_empty());
        let queue = OBSERVE_QUEUE.lock().unwrap();
        assert!(!queue.running && queue.next.is_none() && queue.check.is_none());
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
            after_login: None,
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
            queue.check = None;
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

#[cfg(unix)]
mod shared_accounts;
#[cfg(unix)]
mod shared_login;

/// The Commons table account claims ride.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod account_claims;

/// `aterm init --converge`'s live ports.
#[cfg(unix)]
mod converge;

/// This Mac's fleet as LAN toolset sharing reads it: the members a download races by default.
#[cfg(unix)]
mod unit_peers;

/// When the Commons runtime may make its first LAN use: after System State's Local Network
/// question, on a Mac.
#[cfg(unix)]
mod commons_lan;

#[path = "doctor.rs"]
mod doctor;

/// Compiler-provenance probe: `build.rs` `include!`s it to stamp [`BUILD_STAMP`]'s values; this
/// test-only mount runs the same code against real `-vV` fixtures.
#[cfg(test)]
mod compiler_probe;

#[cfg(all(test, target_os = "macos"))]
mod startup_scope_tests;

#[cfg(test)]
mod tests {

    /// The offer of a resumed run's plan ([`super::run_resume_offer`]) on a scripted reach.
    mod resume_offer {
        use super::super::{ResumeOffer, ResumeOfferReach, run_resume_offer};
        use aterm_gui::{
            InitialSetupAdmission, InitialSetupApplyReceipt, SetupAnswer, WorkstationSetupDecision,
        };
        use aterm_types::macos_setup::{Plan, Selection};
        use std::collections::VecDeque;
        use std::sync::mpsc::Receiver;

        #[derive(Default)]
        struct Script {
            /// What each read of the run says, in order; once spent, the run is current.
            currents: VecDeque<Option<bool>>,
            /// The answers the page gives, one per offer.
            answers: VecDeque<SetupAnswer>,
            offers: usize,
            back_offs: usize,
        }

        impl ResumeOfferReach for Script {
            fn current(&mut self) -> Option<bool> {
                self.currents.pop_front().unwrap_or(Some(true))
            }

            fn choice_awaits_claim(&mut self) -> bool {
                false
            }

            fn answer(&mut self, _plan: Plan) -> SetupAnswer {
                self.offers += 1;
                self.answers
                    .pop_front()
                    .expect("an offer the script has no answer for")
            }

            fn back_off(&mut self, _limit: std::time::Duration) {
                self.back_offs += 1;
            }
        }

        fn resumed() -> Plan {
            Plan::from(Selection::all())
        }

        /// The page's automatic answer of `plan` as offered, and its receipt's answer.
        fn automatic(plan: Plan) -> (SetupAnswer, Receiver<bool>) {
            let (receipt, accepted) =
                InitialSetupApplyReceipt::detached_as(InitialSetupAdmission::Automatic, false);
            let mut answer = SetupAnswer::from(WorkstationSetupDecision::Begin(plan));
            answer.receipt = Some(receipt);
            (answer, accepted)
        }

        /// The page's automatic answer of the plan as offered, while the run is current, is the
        /// window's own admission: its receipt is accepted. Negative controls: the run gone by
        /// the answer, an answer that changed the plan, and a person's press each let it go.
        #[test]
        fn the_pages_automatic_answer_of_the_resumed_plan_is_accepted() {
            let (answer, accepted) = automatic(resumed());
            let mut reach = Script {
                answers: [answer].into(),
                ..Script::default()
            };
            assert_eq!(
                run_resume_offer(&mut reach, &resumed()),
                ResumeOffer::Admitted
            );
            assert_eq!(accepted.try_recv(), Ok(true), "the receipt accepted");

            let (answer, accepted) = automatic(resumed());
            let mut reach = Script {
                currents: [Some(true), None, Some(false)].into(),
                answers: [answer].into(),
                ..Script::default()
            };
            assert_eq!(run_resume_offer(&mut reach, &resumed()), ResumeOffer::LetGo);
            assert_eq!(accepted.try_recv(), Ok(false), "the run went first");
            assert_eq!(reach.back_offs, 1, "an unread record is read again");

            let (answer, accepted) = automatic(Plan::from(Selection::none()));
            let mut reach = Script {
                answers: [answer].into(),
                ..Script::default()
            };
            assert_eq!(run_resume_offer(&mut reach, &resumed()), ResumeOffer::LetGo);
            assert_eq!(accepted.try_recv(), Ok(false), "another plan");

            let (receipt, accepted) = InitialSetupApplyReceipt::detached(false);
            let mut answer = SetupAnswer::from(WorkstationSetupDecision::Begin(resumed()));
            answer.receipt = Some(receipt);
            let mut reach = Script {
                answers: [answer].into(),
                ..Script::default()
            };
            assert_eq!(run_resume_offer(&mut reach, &resumed()), ResumeOffer::LetGo);
            assert_eq!(accepted.try_recv(), Ok(false), "a press answers for itself");
        }

        /// An offer no window could show is made again after a back-off, but only while the run
        /// is still current: a run that went meanwhile (a person's Set up now admitted another
        /// revision) gets no stale offer, whose answer would wipe that person's admission.
        #[test]
        fn an_offer_no_window_showed_is_made_again_only_for_a_current_run() {
            let (answer, accepted) = automatic(resumed());
            let mut reach = Script {
                answers: [WorkstationSetupDecision::Unavailable.into(), answer].into(),
                ..Script::default()
            };
            assert_eq!(
                run_resume_offer(&mut reach, &resumed()),
                ResumeOffer::Admitted
            );
            assert_eq!((reach.offers, reach.back_offs), (2, 1));
            assert_eq!(accepted.try_recv(), Ok(true));

            // Current at the first offer, gone by the time the back-off ends.
            let mut reach = Script {
                currents: [Some(true), Some(false)].into(),
                answers: [WorkstationSetupDecision::Unavailable.into()].into(),
                ..Script::default()
            };
            assert_eq!(run_resume_offer(&mut reach, &resumed()), ResumeOffer::LetGo);
            assert_eq!((reach.offers, reach.back_offs), (1, 1), "no second offer");

            // A closed page ends the offer for this launch.
            let mut reach = Script {
                answers: [WorkstationSetupDecision::Closed.into()].into(),
                ..Script::default()
            };
            assert_eq!(run_resume_offer(&mut reach, &resumed()), ResumeOffer::LetGo);
            assert_eq!(reach.offers, 1);
        }
    }

    mod setup_startup {
        use super::super::{PreparedSetup, SetupAdmission, SetupStartup, prepare_setup};
        use aterm_gui::WorkstationSetupDecision::{self, Begin, Cancelled, Closed, Unavailable};
        use aterm_types::macos_setup::{Plan, Selection, Setting};
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
            /// The writes of a person's Cancel, write by write; once they run out, each one is
            /// recorded.
            cancels: VecDeque<Result<bool, &'static str>>,
            cancel_calls: usize,
            closed_calls: usize,
            /// The admitted runs whose plan the startup offered back as it resumed them.
            resumed: Vec<SetupAdmission>,
        }

        impl SetupStartup for Host {
            type Claim = Claim;

            fn offer_resume(&mut self, admission: &SetupAdmission) {
                assert_eq!(self.active.get(), 1, "the resumed run keeps its claim");
                self.resumed.push(admission.clone());
            }

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

            fn cancel(&mut self) -> Result<bool, String> {
                assert_eq!(self.active.get(), 0, "a Cancel's write holds no claim");
                self.cancel_calls += 1;
                self.cancels
                    .pop_front()
                    .unwrap_or(Ok(true))
                    .map_err(str::to_owned)
            }

            fn closed(&mut self) {
                assert_eq!(self.active.get(), 0, "a closed review holds no claim");
                self.closed_calls += 1;
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
        fn cancelling_a_startup_review_records_it_once_and_releases_its_claim_without_applying() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Cancelled].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Cancelled
            ));
            assert_eq!(
                (host.bind_calls, host.claim_calls, host.offer_calls),
                (1, 1, 1)
            );
            assert_eq!(host.cancel_calls, 1, "the Cancel is recorded for good");
            assert_eq!(host.active.get(), 0);
            assert!(host.authorized.is_empty());
            assert!(host.held_during_backoff.is_empty());
            assert!(host.failures.is_empty());
        }

        /// A Cancel whose write fails is tried again by itself, holding no claim, and never
        /// offers the review again: the person answered it.
        #[test]
        fn a_cancel_whose_write_fails_is_written_again_without_another_offer() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Cancelled].into(),
                cancels: [Err("a pass holds the profile lock"), Ok(true)].into(),
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Cancelled
            ));
            assert_eq!(
                (host.offer_calls, host.cancel_calls),
                (1, 2),
                "one offer, its Cancel written twice"
            );
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.failures, ["a pass holds the profile lock"]);
            assert!(host.authorized.is_empty());
        }

        /// The person closes the review that was shown (Cmd-W): that is not this launch. The
        /// startup ends at once, holding nothing, records nothing, and never offers it again in
        /// this launch, so it cannot come back over the person's terminal or admit anything.
        #[test]
        fn closing_the_review_ends_the_startup_without_another_offer() {
            let mut host = Host {
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                offers: [Closed].into(),
                ..Host::default()
            };
            let prepared = prepare_setup(&mut host).unwrap();
            let closed = matches!(prepared, PreparedSetup::Closed);
            drop(prepared);
            assert_eq!(
                (
                    closed,
                    host.offer_calls,
                    host.closed_calls,
                    host.held_during_backoff.len()
                ),
                (true, 1, 1, 0),
                "a closed review ends this launch's startup after its one offer"
            );
            assert_eq!(host.cancel_calls, 0, "closing records no Cancel");
            assert!(host.authorized.is_empty());
            assert_eq!(host.active.get(), 0);
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

        /// A startup that resumes a run an earlier process admitted (Quit & Reopen's relaunch
        /// mid-setup) offers nothing for review and writes no admission: it offers the admitted
        /// plan back for the window's own answer, without waiting for it, so the page's automatic
        /// answer gives this window the admission the debugger's step needs (design §3.3, §6).
        /// A plan without aterm's own permissions resumes with no such offer.
        #[test]
        fn an_admitted_pending_run_resumes_without_another_offer() {
            let mut selected = Plan {
                selection: Selection::from_bits(1 << 10).unwrap(),
                vault_url: "http://saved.local/v1/team".into(),
            };
            selected.selection.set(Setting::Permissions, true);
            let mut unpermitted = selected.clone();
            unpermitted.selection.set(Setting::Permissions, false);
            for (plan, offered_back) in [(selected.clone(), true), (unpermitted, false)] {
                let admitted = SetupAdmission {
                    plan: plan.clone(),
                    revision: 7,
                    completed: false,
                };
                let mut host = Host {
                    revisions: [Ok(7), Ok(7)].into(),
                    claims: [Ok(true)].into(),
                    finished: [Ok(false), Ok(false)].into(),
                    known_admissions: [Ok(Some(admitted.clone()))].into(),
                    initial: [Ok(plan.clone())].into(),
                    offers: [Begin(plan.clone())].into(),
                    ..Host::default()
                };
                let result = prepare_setup(&mut host).unwrap();
                assert_eq!(
                    host.offer_calls, 0,
                    "an admitted choice needs no second review"
                );
                assert!(
                    host.authorized.is_empty(),
                    "keep the live worker's admitted revision"
                );
                assert!(matches!(
                    result,
                    PreparedSetup::Authorized { revision: 7, .. }
                ));
                assert_eq!(
                    host.resumed,
                    if offered_back {
                        vec![admitted]
                    } else {
                        Vec::new()
                    },
                    "plan {:?}",
                    plan.selection
                );
                assert_eq!(host.initial, [Ok(plan)]);
                assert_eq!(host.active.get(), 1);
                drop(result);
                assert_eq!(host.active.get(), 0);
            }
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
                        plan: Plan::default(),
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
                        plan: Plan::default(),
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
            // A window started over ssh, or outside the console's graphic session, leaves
            // setup to one started at this Mac.
            DoctorStartupEnv {
                remote: true,
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

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn network_publication_cache_never_renews_stalled_or_busy_evidence() {
        use aterm_db::network_state::Publication;
        use aterm_types::network_state::{NetworkObservation, Visibility};
        use std::{
            sync::{Arc, Mutex},
            time::{Duration, Instant},
        };

        for prefetch in [false, true] {
            let at = Instant::now();
            let publisher = Arc::new(Mutex::new(Some(Publication::new(
                NetworkObservation {
                    observed_at_ms: 100_000,
                    visibility: Visibility::Active,
                    ..NetworkObservation::default()
                },
                at,
            ))));
            let clock = Arc::new(Mutex::new((at, Some(100_000))));
            let reader = publisher.clone();
            let timer = clock.clone();
            let read = super::network_state_source_with_readers(
                move || reader.try_lock().ok().and_then(|state| state.clone()),
                || None,
                None,
                move || *timer.lock().unwrap(),
            );
            if prefetch {
                assert_eq!(read().network.visibility, Visibility::Active);
            }
            *clock.lock().unwrap() = (at + Duration::from_secs(20), Some(120_000));
            assert_eq!(read().network.visibility, Visibility::Unavailable);
            let mut held = publisher.lock().unwrap();
            *clock.lock().unwrap() = (at + Duration::from_secs(21), Some(113_000));
            let during = read();
            assert_eq!(during.network.visibility, Visibility::Unavailable);
            assert_eq!(during.network.observed_at_ms, 100_000);
            *held = Some(Publication::new(
                NetworkObservation {
                    observed_at_ms: 113_000,
                    visibility: Visibility::Active,
                    ..NetworkObservation::default()
                },
                at + Duration::from_secs(21),
            ));
            drop(held);
            assert_eq!(read().network.visibility, Visibility::Active);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_observations_survive_an_absent_network_runtime() {
        use aterm_types::network_state::{
            HistoryState, ServiceAvailability, ServiceStates, Visibility,
        };
        use std::sync::{Arc, Mutex};

        let mut services = ServiceStates::default();
        services.secrets.availability = ServiceAvailability::Cached;
        services.secrets.observed_at_ms = 1000;
        let at = std::time::Instant::now();
        let services = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
            services, at,
        )));
        let publisher = services.clone();
        let read = super::network_state_source_with_readers(
            || None,
            move || Some(publisher.lock().unwrap().clone()),
            None,
            move || (at, Some(2000)),
        );
        let first = read();
        assert_eq!(first.network.visibility, Visibility::Unavailable);
        assert_eq!(first.network.history, HistoryState::Unavailable);
        assert_eq!(
            first.services.secrets.availability,
            ServiceAvailability::Cached
        );
        assert!(Arc::ptr_eq(&first, &read()));

        let mut changed = first.services.clone();
        changed.secrets.availability = ServiceAvailability::Unavailable;
        *services.lock().unwrap() = aterm_db::service_state::Publication::new(changed, at);
        let second = read();
        assert!(second.revision > first.revision);
        assert_eq!(
            second.services.secrets.availability,
            ServiceAvailability::Unavailable
        );
        assert_eq!(
            first.services.secrets.availability,
            ServiceAvailability::Cached
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_busy_service_publisher_cannot_stall_the_network_ui_reader() {
        use aterm_types::network_state::{ServiceAvailability, ServiceStates};
        use std::sync::{Arc, Mutex, mpsc};

        for prefetch in [false, true] {
            let mut initial = ServiceStates::default();
            initial.secrets.availability = ServiceAvailability::Cached;
            initial.secrets.observed_at_ms = 1000;
            let at = std::time::Instant::now();
            let services = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
                initial, at,
            )));
            let reader = services.clone();
            let read = super::network_state_source_with_readers(
                || None,
                move || reader.try_lock().ok().map(|state| state.clone()),
                None,
                move || (at, Some(2000)),
            );
            let first = prefetch.then(|| read());
            let mut publisher = services.lock().unwrap();
            let mut changed = ServiceStates::default();
            changed.secrets.availability = ServiceAvailability::Unavailable;
            changed.secrets.observed_at_ms = 2000;
            *publisher = aterm_db::service_state::Publication::new(changed, at);
            let concurrent = read.clone();
            let (send, receive) = mpsc::channel();
            let task = std::thread::spawn(move || send.send(concurrent()).unwrap());
            let result = receive.recv_timeout(std::time::Duration::from_secs(60));
            drop(publisher);
            task.join().unwrap();
            let during = result.expect("a publishing worker must not stall the UI reader");
            if let Some(first) = first {
                assert!(Arc::ptr_eq(&first, &during));
                assert_eq!(during.services.secrets.observed_at_ms, 1000);
            } else {
                assert_eq!(during.services.secrets.observed_at_ms, 0);
                assert_eq!(
                    during.services.secrets.availability,
                    ServiceAvailability::Unknown
                );
            }
            let after = read();
            assert_eq!(after.services.secrets.observed_at_ms, 2000);
            assert_eq!(
                after.services.secrets.availability,
                ServiceAvailability::Unavailable
            );
            assert!(after.revision > during.revision);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_publication_cache_expires_without_network_and_never_renews_on_rollback() {
        use aterm_types::network_state::{ServiceAvailability, ServiceStates, Visibility};
        use std::{
            sync::{Arc, Mutex},
            time::{Duration, Instant},
        };
        let at = Instant::now();
        let mut states = ServiceStates::default();
        states.secrets.availability = ServiceAvailability::Cached;
        states.secrets.observed_at_ms = 100_000;
        states.secrets.revision = Some("retained revision".into());
        let publisher = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
            states.clone(),
            at,
        )));
        let clock = Arc::new(Mutex::new((at, Some(100_000))));
        let source = publisher.clone();
        let timer = clock.clone();
        let read = super::network_state_source_with_readers(
            || None,
            move || source.try_lock().ok().map(|state| state.clone()),
            None,
            move || *timer.lock().unwrap(),
        );
        assert_eq!(
            read().services.secrets.availability,
            ServiceAvailability::Cached
        );
        *clock.lock().unwrap() = (at + Duration::from_secs(61), Some(161_001));
        assert_eq!(
            read().services.secrets.availability,
            ServiceAvailability::Unknown
        );
        let mut held = publisher.lock().unwrap();
        *clock.lock().unwrap() = (at + Duration::from_secs(62), Some(130_000));
        let stale = read();
        assert_eq!(stale.network.visibility, Visibility::Unavailable);
        assert_eq!(
            stale.services.secrets.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(stale.services.secrets.observed_at_ms, 100_000);
        assert_eq!(stale.services.secrets.revision, states.secrets.revision);
        drop(held);
        assert_eq!(
            read().services.secrets.availability,
            ServiceAvailability::Unknown
        );
        held = publisher.lock().unwrap();
        states.secrets.observed_at_ms = 130_000;
        *held = aterm_db::service_state::Publication::new(states, at + Duration::from_secs(62));
        drop(held);
        let restored = read();
        assert_eq!(
            restored.services.secrets.availability,
            ServiceAvailability::Cached
        );
        assert!(restored.revision > stale.revision);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn first_service_read_after_a_stall_and_rollback_is_not_fresh() {
        use aterm_types::network_state::{ServiceAvailability, ServiceStates};
        use std::time::{Duration, Instant};
        let at = Instant::now();
        let mut states = ServiceStates::default();
        states.accounts.availability = ServiceAvailability::Current;
        states.accounts.observed_at_ms = 100_000;
        let publication = aterm_db::service_state::Publication::new(states, at);
        let read = super::network_state_source_with_readers(
            || None,
            move || Some(publication.clone()),
            None,
            move || (at + Duration::from_secs(120), Some(110_000)),
        );
        assert_eq!(
            read().services.accounts.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(read().services.accounts.observed_at_ms, 100_000);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_missing_service_observer_does_not_hide_network_observations() {
        use aterm_types::network_state::{ServiceAvailability, Visibility};

        let network = aterm_db::network_state::ObservationHandle::default();
        let read = super::network_state_source_from(Some(network), None, None);
        let snapshot = read();
        assert_eq!(snapshot.network.visibility, Visibility::Checking);
        assert_eq!(
            snapshot.services.accounts.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(
            snapshot.services.secrets.availability,
            ServiceAvailability::Unknown
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn an_absent_network_runtime_retains_history_but_never_current_reachability() {
        use aterm_types::network_state::{
            HistoryState, MachineIdentity, NetworkObservation, PeerObservation, Reachability,
            Visibility,
        };
        use std::sync::Arc;

        let retained = NetworkObservation {
            history: HistoryState::Ready,
            visibility: Visibility::Active,
            observer: Some(MachineIdentity {
                machine_id: [3; 16],
                incarnation: [4; 16],
                label: None,
            }),
            observed_at_ms: 2000,
            unverified_endpoints: 3,
            peers: vec![PeerObservation {
                slot: 7,
                identity: MachineIdentity {
                    machine_id: [1; 16],
                    incarnation: [2; 16],
                    label: Some("saved machine".into()),
                },
                first_seen_ms: 1000,
                last_seen_ms: 2000,
                last_checked_ms: Some(2000),
                reachability: Reachability::Reachable,
                instances: Some(2),
                samples: Vec::new(),
            }],
            ..Default::default()
        };
        let read = super::network_state_source_from(None, None, Some(retained.clone()));
        let snapshot = read();
        assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
        assert!(snapshot.network.observer.is_none());
        assert_eq!(snapshot.network.history, HistoryState::Ready);
        assert_eq!(snapshot.network.observed_at_ms, 2000);
        assert_eq!(snapshot.network.unverified_endpoints, 0);
        let peer = &snapshot.network.peers[0];
        assert_eq!(peer.slot, 7);
        assert_eq!(peer.identity, retained.peers[0].identity);
        assert_eq!((peer.first_seen_ms, peer.last_seen_ms), (1000, 2000));
        assert_eq!(peer.reachability, Reachability::Unknown);
        assert_eq!(peer.last_checked_ms, None);
        assert!(Arc::ptr_eq(&snapshot, &read()));
        assert_eq!(retained.peers[0].reachability, Reachability::Reachable);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn only_an_ordinary_window_autostarts_acaffiene() {
        let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        for args in [
            osv(&[]),
            osv(&["--window"]),
            osv(&["-d", "."]),
            osv(&["--no-control-sock"]),
            osv(&["--window", "--control-sock", "off"]),
            osv(&["--control-sock", "OFF"]),
            osv(&["--control-sock", "0"]),
            osv(&["--control-sock", ""]),
        ] {
            assert!(ordinary_window(&args), "{args:?}");
        }
        for args in [
            osv(&["--headless"]),
            osv(&["--diagnose"]),
            osv(&["--control-sock", "/tmp/private.sock"]),
            osv(&["--control-sock"]),
            osv(&["--control-sock", "off", "--headless"]),
            osv(&["--control-sock", "--headless"]),
            osv(&["-e", "sh", "--control-sock", "/tmp/private.sock"]),
        ] {
            assert!(!ordinary_window(&args), "{args:?}");
        }
    }

    /// A running window's successor, an update's or a launch-record renewal's, which can come
    /// while the person is away, starts aCaffiene as saved; any other window launch, forever.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_windows_successor_starts_acaffiene_as_saved() {
        use aterm_acaffiene::cli::Launch;
        let only = |set: &'static str| {
            move |name: &'static str| (name == set).then(|| OsString::from("1"))
        };
        assert_eq!(
            acaffiene_launch(only(aterm_update::RELAUNCH_OF_RUNNING_ENV)),
            Launch::Successor
        );
        assert_eq!(acaffiene_launch(|_| None), Launch::Fresh);
        assert_eq!(acaffiene_launch(only("ATERM_UPDATED_FROM")), Launch::Fresh);
    }

    #[test]
    fn a_spawn_is_handed_to_the_sibling_as_new_window_never_as_a_plain_launch() {
        assert_eq!(
            alias_route("aterm-gui", "new-window"),
            AliasRoute::AliasWindowVerb
        );
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
                let verb_str = verb.to_string_lossy();
                assert_eq!(verb_str, "new-window", "{intent:?}");
                assert_eq!(
                    alias_route("aterm-gui", &verb_str),
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

    #[test]
    fn front_door_verbs_and_completion_flags_have_no_duplicates() {
        let verbs = super::front_door_verbs();
        let mut seen_verbs = std::collections::BTreeSet::new();
        for &verb in &verbs {
            assert!(
                seen_verbs.insert(verb),
                "duplicate front-door completion verb: {verb:?}"
            );
        }
        assert!(seen_verbs.contains("help"));
        for verb in aterm_cli::Verb::ALL {
            assert!(seen_verbs.contains(verb.name()), "missing {}", verb.name());
        }
        for &(name, _) in aterm_cli::DIAG_COMMANDS {
            assert!(seen_verbs.contains(name), "missing diag {name}");
        }

        let mut seen_flags = std::collections::BTreeSet::new();
        for &(flag, desc) in super::COMPLETION_FLAGS {
            assert!(flag.starts_with("--"), "flag {flag:?} must start with --");
            assert!(!desc.is_empty(), "flag {flag:?} must carry a description");
            assert!(
                seen_flags.insert(flag),
                "duplicate front-door completion flag: {flag:?}"
            );
        }

        for (shell, wiring) in [
            ("bash", "complete -F _aterm aterm\n"),
            ("zsh", "#compdef aterm\n"),
            ("fish", "complete -c aterm -f\n"),
        ] {
            let script =
                aterm_ctl::front_door_completion_script(shell, &verbs, super::COMPLETION_FLAGS)
                    .unwrap_or_else(|| panic!("{shell} script"));
            if shell == "zsh" {
                assert!(script.starts_with(wiring), "zsh must start with {wiring:?}");
            } else {
                assert!(script.contains(wiring), "{shell} missing {wiring:?}");
            }
            for &verb in &verbs {
                assert!(script.contains(verb), "{shell} missing verb {verb:?}");
            }
            for &(flag, _) in super::COMPLETION_FLAGS {
                let want = if shell == "fish" {
                    flag.trim_start_matches('-')
                } else {
                    flag
                };
                assert!(script.contains(want), "{shell} missing flag {want:?}");
            }
        }
        assert!(
            aterm_ctl::front_door_completion_script("tcsh", &verbs, super::COMPLETION_FLAGS)
                .is_none()
        );
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
