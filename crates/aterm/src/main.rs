// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm`: the one binary.

#![cfg_attr(all(unix, trust_verify), feature(register_tool))]
#![cfg_attr(all(unix, trust_verify), register_tool(trust))]
#![cfg_attr(
    all(unix, anchor_rules, any(test, debug_assertions)),
    feature(macro_attr)
)]

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::ExitCode;

// The proofs of the mesh kernels this binary ships: a build of it checks them (Cargo.toml).
use aterm_mesh_core_islands as _;
use aterm_mesh_core_islands_no_lost_edit as _;
use aterm_mesh_core_islands_p1 as _;

mod lab_window;

#[cfg(all(debug_assertions, target_os = "macos"))]
mod source_image_fixture;

#[cfg(target_os = "macos")]
mod compact_zone;
#[cfg(target_os = "macos")]
mod input_attachments;
#[cfg(any(unix, windows))]
mod machine_status;
#[cfg(test)]
mod macho_order;
#[cfg(unix)]
mod release_probe;
#[cfg(unix)]
mod service_repair;
#[cfg(unix)]
mod service_repair_remote;
#[cfg(target_os = "macos")]
mod setup_resume;
#[cfg(target_os = "macos")]
mod setup_take_up;
#[cfg(target_os = "macos")]
mod subscriber_scheduling;
#[cfg(target_os = "macos")]
mod withheld_preferences;

use aterm_cli::{AliasRoute, alias_route};

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

/// `aterm pkg` and its `atpkg` alias: the package pass, then, after an install or update that
/// ended settled and that a person typed at a terminal ([`a_person_at_their_own_account`],
/// [`atpkg::cli::pass_finishes_setup`], [`atpkg::cli::pass_ended_settled`]), setup's follow-up
/// ([`InitEntry::PackageFollowUp`]) in this process. The pass's store lock is
/// [`atpkg::cli::main_entry`]'s own and is let go as it returns, before the follow-up starts.
/// A stop during the follow-up says [`FOLLOW_UP_STOPPED`] and ends the command as that stop
/// would have ([`aterm_machines::port::say_on_stop`]).
fn package_entry(args: Vec<OsString>) -> ExitCode {
    package_entry_with(
        args,
        a_person_at_their_own_account(),
        |forwarded| {
            #[cfg(target_os = "macos")]
            {
                match aterm_update::first_launch::native::prepare(
                    forwarded,
                    aterm_update::first_launch::native::Entry::Init,
                ) {
                    Ok(code) => code,
                    Err(reason) => {
                        eprintln!("aterm pkg: {reason}");
                        Some(ExitCode::FAILURE)
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = forwarded;
                None
            }
        },
        |args| {
            #[cfg(target_os = "macos")]
            if aterm_machines::cli::pkg_retires(&args) {
                aterm_machines::cli::retire_lan_agent();
            }
            let code = atpkg::cli::main_entry(args);
            (code, atpkg::cli::pass_ended_settled(code))
        },
        || {
            #[cfg(unix)]
            {
                let _says = aterm_machines::port::say_on_stop(FOLLOW_UP_STOPPED);
                run_init(InitEntry::PackageFollowUp)
            }
            #[cfg(not(unix))]
            {
                ExitCode::SUCCESS
            }
        },
    )
}

/// What the package follow-up says when a stop (a Ctrl-C, a closed tab, a `kill`) ends it.
#[cfg(unix)]
const FOLLOW_UP_STOPPED: &str =
    "aterm pkg: stopped; setup after this pass is not finished: `aterm init` finishes it\n";

/// What the package follow-up says when it ends on a panic: the pass's status stands.
const FOLLOW_UP_UNFINISHED: &str =
    "aterm pkg: setup after this pass did not finish; `aterm init` finishes it";

/// Whether a person runs this command at a terminal, as themselves in their own login's home,
/// outside CI ([`a_person_at_their_own_account_from`] of this process).
fn a_person_at_their_own_account() -> bool {
    #[cfg(unix)]
    {
        a_person_at_their_own_account_from(
            atpkg::cli::a_person_typed_this(),
            aterm_uds::peer::our_uid(),
            aterm_machines::sessions::is_live_account(),
            |name| std::env::var_os(name),
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether a person runs this command: at a terminal (`typed`, [`atpkg::cli::a_person_typed_this`]:
/// its standard input and standard error both are), as themselves (`uid` not root, which
/// `aterm init` refuses), in their own login's home (`live`,
/// [`aterm_machines::sessions::is_live_account`], which no private state root is), and not
/// under CI (`CI` set, as `var` reads it), where aterm makes no setup decision of its own, as
/// it makes no LAN look or fleet dial there. A typed `aterm init` under CI still runs: the CI
/// job asked for it. A pass a program runs with its input or its error elsewhere (the window's
/// package lanes and Settings ▸ Packages, a worker's admission, a detached pass, init's own
/// toolset pass, the machines items) is no person's. A pass a command runs on the person's
/// terminal as one step of its own work (`aterm ai doctor`'s and `aterm ai login`'s installs,
/// the AI screen's Install tab) reads as theirs here, and says itself that no setup follows it
/// ([`atpkg::cli::NO_INIT_FLAG`]).
#[cfg(any(unix, test))]
fn a_person_at_their_own_account_from(
    typed: bool,
    uid: u32,
    live: bool,
    var: impl Fn(&str) -> Option<OsString>,
) -> bool {
    typed && uid != 0 && live && var("CI").is_none()
}

/// [`package_entry`] with whether a person runs it (`person`), native launch preparation
/// (`prepare`, handed canonical `pkg` arguments), the package pass (`packages`,
/// which answers its status and whether it ended settled, [`atpkg::cli::pass_ended_settled`])
/// and the follow-up (`setup`). The follow-up runs only after a pass a person ran whose words
/// setup follows ([`atpkg::cli::pass_finishes_setup`]), and only once that pass ended settled.
/// The command's exit status is the pass's, unless a stop ends the follow-up: a follow-up that
/// panics says [`FOLLOW_UP_UNFINISHED`] and the pass's status stands. A native handoff returns
/// its installed command's status before this original process runs a package pass.
fn package_entry_with(
    args: Vec<OsString>,
    person: bool,
    prepare: impl FnOnce(Vec<OsString>) -> Option<ExitCode>,
    packages: impl FnOnce(Vec<OsString>) -> (ExitCode, bool),
    setup: impl FnOnce() -> ExitCode,
) -> ExitCode {
    let finish = person && atpkg::cli::pass_finishes_setup(&args);
    if finish {
        // argv0 aliases supply only package operands. The installed composed image
        // receives the canonical verb plus those exact bytes, and runs the pass once.
        let forwarded = std::iter::once(OsString::from("pkg"))
            .chain(args.iter().cloned())
            .collect();
        if let Some(code) = prepare(forwarded) {
            return code;
        }
    }
    let (code, settled) = packages(args);
    if finish && settled {
        // The follow-up's rows and summary on the person's terminal, and its line in aterm.log,
        // say what it left unfinished. Its status (75, 77, 69 or 1 for an unfinished run) would
        // read as the pass's own store contention, offline or failure.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(setup)).is_err() {
            let _ = aterm_uds::stdout::stderr_line(format_args!("{FOLLOW_UP_UNFINISHED}"));
        }
    }
    code
}

/// How `aterm init` is entered ([`run_init`]).
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum InitEntry {
    /// `aterm init` with its own words, and the running window's permission walk for the grants
    /// the run leaves to the person.
    Typed(Vec<OsString>),
    /// The follow-up of a person's own package install or update ([`package_entry`]). Running it
    /// is aterm's decision, not theirs ([`aterm_machines::init::Decider::Aterm`]), so it puts no
    /// question of macOS's on the screen: its words leave out the sign-outs and the workstation
    /// preferences ([`aterm_machines::init::PACKAGE_FOLLOW_UP`]), its look for the ALab vault
    /// waits, as every look does, until the LAN broker answers (macOS asks nothing of a look
    /// that goes through it), Rust in Spotlight searches no folder of the person's, and it hands
    /// no grant to the permission walk.
    PackageFollowUp,
}

#[cfg(unix)]
impl InitEntry {
    /// The words init parses for this entry.
    fn words(self) -> Vec<OsString> {
        match self {
            Self::Typed(words) => words,
            Self::PackageFollowUp => aterm_machines::init::PACKAGE_FOLLOW_UP
                .iter()
                .map(OsString::from)
                .collect(),
        }
    }

    /// Who decided that this run happens.
    const fn decided(&self) -> aterm_machines::init::Decider {
        match self {
            Self::Typed(_) => aterm_machines::init::Decider::Person,
            Self::PackageFollowUp => aterm_machines::init::Decider::Aterm,
        }
    }
}

/// `aterm init`, entered as `entry` says. Every entry composes the same base layer
/// ([`doctor::init_base`], the toolset row included) and the same host; the follow-up differs
/// in its words, in who decided it ([`InitEntry::decided`]) and in handing no grant to the
/// permission walk. The run's phase totals go to aterm.log, never to this terminal.
#[cfg(unix)]
fn run_init(entry: InitEntry) -> ExitCode {
    #[cfg(target_os = "macos")]
    if let InitEntry::Typed(words) = &entry
        && aterm_machines::init::launches_setup(words)
    {
        match aterm_update::first_launch::native::prepare(
            [vec![OsString::from("init")], words.clone()].concat(),
            aterm_update::first_launch::native::Entry::Init,
        ) {
            Ok(Some(code)) => return code,
            Ok(None) => {}
            Err(reason) => {
                eprintln!("aterm init: {reason}");
                return ExitCode::FAILURE;
            }
        }
    }
    aterm_gui::install_session_log();
    #[cfg(target_os = "macos")]
    let host = aterm_machines::init::Host {
        decided: entry.decided(),
        base: &doctor::init_base,
        full_disk_access: &aterm_cli::macos_doctor::full_disk_access,
        records: &aterm_cli::macos_doctor::InitRecords,
        grants: aterm_cli::macos_doctor::init_grants,
        workstation_check: &aterm_cli::macos_doctor::check_background_workstation,
        walk: match entry {
            InitEntry::Typed(_) => &aterm_cli::init_permissions_walk,
            InitEntry::PackageFollowUp => &|_| None,
        },
        converge: &converge::Live::here(),
        alab: &aterm_cli::macos_doctor::alab_mac,
        present: aterm_gui::person_at_this_screen,
    };
    // Elsewhere (Linux): the base layer, the vault and the repositories; each workstation row
    // reads "macOS only".
    #[cfg(not(target_os = "macos"))]
    let host = aterm_machines::init::Host {
        decided: entry.decided(),
        base: &doctor::init_base,
        full_disk_access: &|| None,
        records: &aterm_machines::init::NoRecords,
        grants: aterm_machines::init::no_grants,
        workstation_check: &|_| {
            aterm_machines::init::WorkstationObservation::Unavailable("macOS only".into())
        },
        walk: &|_| None,
        converge: &converge::Live::here(),
        alab: &aterm_machines::init_secrets::alab_member_here,
        present: || false,
    };
    aterm_machines::init::main_entry(entry.words(), &host)
}

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
/// so off macOS (where `aterm-machines`, linked on Unix, serves other verbs such as `aterm init`)
/// the verb stays on the roster and refuses by name.
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

/// The background Commons runtime `aterm node serve` holds, once it started
/// ([`node_commons_startup`]).
#[cfg(unix)]
static NODE_COMMONS: std::sync::OnceLock<aterm_db::runtime::Runtime> = std::sync::OnceLock::new();

/// A scratch host starts Commons only with an explicit guarded debug attachment.
/// Other automatic lanes still see its private-state or CI marker and stay off.
#[cfg(unix)]
fn commons_host_enabled() -> bool {
    let unmarked =
        std::env::var_os("ATERM_STATE_HOME").is_none() && std::env::var_os("CI").is_none();
    #[cfg(any(test, debug_assertions))]
    {
        unmarked || aterm_db::runtime::private_attachment_port().is_ok()
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        unmarked
    }
}

/// Start `aterm node serve`'s background Commons runtime (C22) while `[swarm] enabled` leaves
/// sharing on, retaining a failed start until its owned maintenance can retry, publishing its
/// observation to the account claims and its peers to the node job's "network moved" signal.
#[cfg(unix)]
fn node_commons_startup() -> Option<commons_lan::Startup> {
    if !commons_host_enabled() {
        return None;
    }
    let paths = aterm_secrets::client::Paths::live().ok()?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let home = paths.home.clone();
    let mut startup = commons_lan::Startup::new(&home, move || {
        start_node_commons_runtime(paths.clone(), config.clone())
    });
    let _ = startup.maintain();
    Some(startup)
}

/// Hand the node service's Commons `runtime` its mesh: which peers speak `Bus`
/// ([`aterm_mesh::Mesh::speaks_bus`]) and a push's relay ack ([`aterm_mesh::Mesh::push_table`]),
/// each read from the mesh open now, so a mesh that closes leaves the old path.
#[cfg(unix)]
fn relay_tables(runtime: &aterm_db::runtime::Runtime) {
    runtime.speaks_bus(std::sync::Arc::new(|key| {
        aterm_mesh::current().is_some_and(|mesh| mesh.speaks_bus(key))
    }));
    runtime.pusher().relay(std::sync::Arc::new(
        |table, deadline| match aterm_mesh::current()?.push_table(table, deadline)? {
            aterm_mesh::Pushed::Reached => Some(aterm_db::runtime::Pushed::Reached),
            aterm_mesh::Pushed::TimedOut => Some(aterm_db::runtime::Pushed::TimedOut),
        },
    ));
}

/// Hand a window's or a session lane's Commons `runtime` the node service's relay ack: its push
/// asks the node whose state dir is `state_dir` ([`aterm_mesh::push::push_by_node`]), and where
/// no node answers, the push takes the old path.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn relay_by_node(runtime: &aterm_db::runtime::Runtime, state_dir: std::path::PathBuf) {
    runtime
        .pusher()
        .relay(std::sync::Arc::new(
            move |table, deadline| match aterm_mesh::push::push_by_node(
                &state_dir, &table, deadline,
            )? {
                aterm_mesh::Pushed::Reached => Some(aterm_db::runtime::Pushed::Reached),
                aterm_mesh::Pushed::TimedOut => Some(aterm_db::runtime::Pushed::TimedOut),
            },
        ));
}

/// Background Commons runtime and steward enroller held by `aterm node serve` (C22). Before the
/// one-mesh transport it yields local observation and transport whenever an interactive window
/// runs; with `aterm_types::mesh_switch::MESH` on it yields to none (the runtime reads the
/// switch once: [`aterm_db::runtime::Runtime::start_background`]), and the node service is the
/// one owner.
#[cfg(unix)]
fn start_node_commons_runtime(
    paths: aterm_secrets::client::Paths,
    config: std::path::PathBuf,
) -> bool {
    if NODE_COMMONS.get().is_some() {
        return true;
    }
    let rounds = alab_register::reports(paths.clone(), config.clone());
    match aterm_db::runtime::Runtime::start_background_with_reports(
        paths,
        config,
        // A refusal warns once per cause (`Status::news`); a standing one logs at debug.
        |status| {
            if let Some(refusal) = status.news() {
                aterm_log::warn!(
                    "Commons node background repair refused local configuration or trust state: {refusal}"
                );
            } else if status == aterm_db::runtime::Status::LanHeld {
                aterm_log::info!("Commons node background repair: {status:?}");
            } else {
                aterm_log::debug!("Commons node background repair: {status:?}");
            }
        },
        rounds,
    ) {
        Ok(runtime) => {
            if NODE_COMMONS.set(runtime).is_ok() {
                let runtime = NODE_COMMONS.get().expect("published node runtime");
                // Bus peers use the node mesh's relay acknowledgment (§11 B7b).
                if aterm_types::mesh_switch::MESH {
                    relay_tables(runtime);
                }
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                {
                    let _ = NODE_OBSERVATION.set(runtime.network_observation());
                    let _ = NODE_PUSHER.set(runtime.pusher());
                }
                let observation = runtime.network_observation();
                aterm_gui::hear_commons(move || Some(observation.snapshot().peers));
                alab_journal::start_backfill();
                alab_observed::start_sweep();
            }
            true
        }
        Err(error) => {
            aterm_log::warn!("Commons node background runtime could not start: {error}");
            false
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
        // Every thread the job starts runs at utility, as the `Standard` launchd job it ran in
        // clamped it, but per thread, so the fleet book's lock lifts its holder to the default
        // class while it holds it (`node_service::node_plist_text`'s `Interactive` job caps no
        // thread). Unlike that clamp, the hold leaves a child started outside a role's
        // `qos::command` (its Bonjour advert) at the default class, and a dispatch worker's item
        // at its queue's class. First, as a thread started earlier keeps the default class.
        aterm_gui::hold_process_as_background_work();
        aterm_gui::install_session_log();
        // The job's lock first, then its boot: a build this job staged and ended its last run
        // for goes in now, with no window here to apply it, and that boot execs the build it
        // installed and does not return (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.13). Only
        // then this closure, before the job's listener or update checker starts. A start that
        // finds the lock held runs neither.
        let startup = || {
            // The job's automatic LAN uses go only while `[swarm] enabled` leaves sharing on.
            if let Some(home) = aterm_types::dirs::home_dir() {
                commons_lan::install_sharing(&home);
            }
            // Its own pass every minute, and the windows' needs pushes from the first one.
            metrics_publish::start();
            node_commons_startup().map(|startup| {
                let startup = std::sync::Mutex::new(startup);
                std::sync::Arc::new(move || {
                    if let Ok(mut startup) = startup.try_lock() {
                        let _ = startup.maintain();
                    }
                }) as aterm_gui::NodeMaintenance
            })
        };
        // The job's Commons runtime is a static nothing drops: it stops, and closes the
        // network history, in the job's quit step, which a terminating signal runs (launchd's
        // SIGTERM, the job's usual end) and so does the serve loop's return below.
        aterm_gui::install_host_quit(node_commons_stop_at_quit);
        #[cfg(target_os = "macos")]
        let code = input_attachments::node_serve(startup);
        #[cfg(not(target_os = "macos"))]
        let code = aterm_gui::run_node_serve_owning(startup);
        node_commons_stop_at_quit();
        return code;
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
    match aterm_agent::node_service::run_admin_subcommand(
        subcmd,
        rest,
        &home,
        &state,
        &exe,
        aterm_agent::node_service::supervisor_here(),
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
    fleet_drive: Some(aterm_agent::drive_fleet_from_shell),
    #[cfg(not(unix))]
    fleet_drive: None,
    #[cfg(unix)]
    fleet_node: Some(aterm_agent::fleet_node),
    #[cfg(not(unix))]
    fleet_node: None,
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

/// This binary's identity record (`aterm_stamp`): the version, commit, build number, build time,
/// dev-commit count, index hint and release mark `build.rs` wrote, in a section of its own so a
/// writer that patches the image after the link can rewrite it. No library compiles any of it in:
/// each reads [`aterm_stamp::current`] after [`main`] installs this.
#[used]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__DATA,__aterm_stamp"))]
#[cfg_attr(
    all(not(target_vendor = "apple"), not(windows)),
    unsafe(link_section = ".aterm_stamp")
)]
#[cfg_attr(windows, unsafe(link_section = ".astamp"))]
static STAMP: [u8; aterm_stamp::LEN] = *include_bytes!(concat!(env!("OUT_DIR"), "/stamp.bin"));

/// The facts of this binary's own compile, constant per toolchain and profile: the record's
/// fields are decoded over them.
const COMPILED: aterm_stamp::Stamp = aterm_stamp::Stamp {
    compiler_version_line: env!("ATERM_COMPILER_VERSION_LINE"),
    compiler_commit: env!("ATERM_COMPILER_COMMIT"),
    compiler_host: env!("ATERM_COMPILER_HOST"),
    compiler_trust_version: env!("ATERM_COMPILER_TRUST_VERSION"),
    compiler_flavor: env!("ATERM_COMPILER_FLAVOR"),
    build_profile: env!("ATERM_BUILD_PROFILE"),
    trust_verify: env!("ATERM_TRUST_VERIFY"),
    ..aterm_stamp::Stamp::UNSTAMPED
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

/// Name the Command Line Tools shim's developer directory for this process while the tools are
/// missing, or drop one inherited once they exist ([`aterm_types::clt_shim::developer_dir_move`]).
/// A stat and a readlink on a Mac with the tools: no child process, and no write.
#[cfg(target_os = "macos")]
fn settle_developer_dir() {
    use aterm_types::clt_shim::{DEVELOPER_DIR_ENV, DeveloperDirMove};
    // SAFETY: `geteuid` is a side-effect-free getter.
    let root = unsafe { libc::geteuid() } == 0;
    match aterm_types::clt_shim::developer_dir_move(
        std::env::var_os(DEVELOPER_DIR_ENV).as_deref(),
        aterm_types::clt_shim::developer_git(),
        aterm_types::clt_shim::shim_dir().as_deref(),
        root,
    ) {
        DeveloperDirMove::Keep => {}
        // SAFETY: `main` calls this first, before this process starts any thread.
        DeveloperDirMove::Set(dir) => unsafe { std::env::set_var(DEVELOPER_DIR_ENV, dir) },
        // SAFETY: as above.
        DeveloperDirMove::Clear => unsafe { std::env::remove_var(DEVELOPER_DIR_ENV) },
    }
    // The tools can arrive while this process runs (an `aterm init` that installed them, a
    // window that outlives the install). It cannot then take the shim's directory out of its own
    // environment, so each child it starts after that goes without it: an inherited shim
    // directory makes Apple's tools fail (`missing DEVELOPER_DIR path`), as `aterm init`'s
    // compiler read-back did on a guest that had just installed them (nux round 9).
    if std::env::var_os(DEVELOPER_DIR_ENV)
        .is_some_and(|dir| aterm_types::clt_shim::is_shim_developer_dir(dir.as_ref()))
    {
        aterm_uds::spawnfd::register_stale_inherited_var(stale_developer_dir);
    }
}

/// `DEVELOPER_DIR` while it names the shim's directory and the tools are there now; once they
/// are, they stay ([`settle_developer_dir`]).
#[cfg(target_os = "macos")]
fn stale_developer_dir() -> Option<&'static str> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ARRIVED: AtomicBool = AtomicBool::new(false);
    let dir = std::env::var_os(aterm_types::clt_shim::DEVELOPER_DIR_ENV)?;
    if !aterm_types::clt_shim::is_shim_developer_dir(dir.as_ref()) {
        return None;
    }
    if !ARRIVED.load(Ordering::Acquire) {
        if !aterm_types::clt_shim::developer_git() {
            return None;
        }
        ARRIVED.store(true, Ordering::Release);
    }
    Some(aterm_types::clt_shim::DEVELOPER_DIR_ENV)
}

/// The bound a front-door test sets, in milliseconds, for the CPU-policy collector's wait for its
/// ack (`ATERM_CPU_POLICY_HANG_MS`, a development seam): none in a shipped build.
#[cfg(target_os = "macos")]
fn cpu_policy_hang() -> Option<std::time::Duration> {
    aterm_types::dev_seam!("ATERM_CPU_POLICY_HANG_MS")
        .and_then(|ms| ms.to_str()?.parse().ok())
        .map(std::time::Duration::from_millis)
}

pub(crate) fn main() -> ExitCode {
    // First: every entry below, the early helper verbs included, reads the installed identity.
    aterm_stamp::install(&STAMP, COMPILED);
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    // Capture before ANY descriptor-opening subscriber/window/front-door setup.
    // SAFETY: stamp installation/args_os above only read in-memory startup data;
    // this executable has started no threads or descriptor-mutating callbacks.
    let original_root_store = match unsafe { aterm_ctl::capture_original_root_store(&argv[1..]) } {
        Ok(original) => original,
        Err(why) => {
            eprintln!("aterm resource-run: {why}");
            return ExitCode::from(75);
        }
    };
    #[cfg(target_os = "macos")]
    subscriber_scheduling::enter(&argv);
    // The watch-only thread policy above does no state, key, network or window work.
    // Window lifetime custody precedes the remaining helper entries that may fork or spawn.
    // This original opaque description remains live through the whole GUI run.
    let lab_window_guard = match lab_window::enter(&mut argv) {
        Ok(guard) => guard,
        Err(why) => {
            eprintln!("aterm: {why}");
            return ExitCode::from(2);
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(reason) = aterm_update::first_launch::native::acknowledge_relaunch(&argv[1..]) {
        eprintln!("aterm: {reason}");
        return ExitCode::FAILURE;
    }
    let code = front_door_main(argv, lab_window_guard.is_present(), original_root_store);
    // A pass the lab's mailbox discovery started on its own thread finishes before a
    // short-lived invocation returns (a bounded wait; nothing runs in most processes).
    #[cfg(unix)]
    alab_mailbox::settle();
    code
}

fn front_door_main(
    argv: Vec<OsString>,
    lab_window_guarded: bool,
    original_root_store: Option<aterm_ctl::OriginalRootStore>,
) -> ExitCode {
    #[cfg(unix)]
    if let Some(code) = argv.get(1..).and_then(aterm_resource_host::entry) {
        return code;
    }
    // First, while this process has one thread: a build's claim holder forks away from its
    // front's spawn (BUILD-ADMIT), and a window's tethered helper starts as its verb.
    if let Some(code) = atpkg::build_admit::hold::entry() {
        return code;
    }
    if let Err(error) = agh::git::HelperHost::Subcommand.record() {
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Some(code) = source_image_fixture::entry(&argv) {
        return code;
    }
    #[cfg(unix)]
    if let Some(code) = aterm_machines::port::claude_keychain::entry() {
        return code;
    }
    // While this process still has one thread: every program it starts, from `/usr/bin/git` by
    // absolute path to a tab's agent, reaches aterm's shim and not Apple's install dialog.
    #[cfg(target_os = "macos")]
    settle_developer_dir();
    #[cfg(target_os = "macos")]
    aterm_uds::process::register_allocator_relief(compact_zone::release_free_memory);
    // Every aterm process keeps its LAN while no broker is installed where macOS already allowed
    // this aterm, for the account it runs as and as the app it holds responsible
    // (docs/DESIGN-lan-broker-2026-11-05.md §5); it reads nothing until then.
    #[cfg(target_os = "macos")]
    aterm_lan::provide_direct();
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    // Before any vault path runs: the vault refuses every AI CLI's login file as an entry, as the
    // connector rows say where each keeps it (aterm-secrets cannot read them itself).
    #[cfg(unix)]
    aterm_secrets::text::on_cli_login(aterm_agent::ai::connector::login_file);
    // With the one mesh on, each durable table append and membership write of any aterm process
    // reaches the node service's egress (in-process there, by its wake socket elsewhere), which
    // publishes it on `tables.log` (docs/DESIGN-alab-one-mesh-2026-11-05.md §11 B7b).
    #[cfg(unix)]
    if aterm_types::mesh_switch::MESH
        && let Some(state_dir) = aterm_types::dirs::state_dir()
    {
        aterm_mesh::stores::vault::wake_sinks(&state_dir);
    }
    // A mailbox the harness opens to read a sign-in code tells the lab registry which addresses
    // it receives (a release build only; the hook is inert in tests and debug builds).
    #[cfg(unix)]
    alab_mailbox::install();
    // A login a tab's CLI was observed to run on reaches the registry's `account-obs` rows, and
    // an account no row names is born (a release build only; inert in tests and debug builds).
    #[cfg(unix)]
    alab_observed::install();
    // A Mac waiting to be let into ALab proves itself to ALab's members on the LAN from its vault
    // subscriber, then asks those that did not let it in to show its request: about half a
    // minute after its first ask, then after 1, 2, 4 and 8 minutes, then every 15, over the fleet
    // port's `ask` (aterm-secrets cannot dial it itself), with the ask the Secrets stage
    // registers below.
    #[cfg(unix)]
    aterm_secrets::client::on_ask_members(
        |paths| {
            aterm_agent::members_cli::ask_members(&paths.home).map_err(aterm_secrets::Error::new)
        },
        aterm_agent::members_cli::member_arrivals,
    );
    #[cfg(unix)]
    aterm_secrets::client::on_native_bootstrap(|paths, current| {
        aterm_agent::members_cli::bootstrap_from_members(paths, current)
    });
    #[cfg(all(unix, not(target_os = "macos")))]
    aterm_secrets::client::on_convergence_inputs(converge_inputs::observe);
    #[cfg(target_os = "macos")]
    aterm_secrets::client::on_convergence_inputs(input_attachments::convergence_inputs);
    #[cfg(target_os = "macos")]
    aterm_secrets::client::on_bootstrap_allowed(|_| {
        aterm_cli::macos_doctor::automatic_vault_bootstrap_allowed()
            .map_err(aterm_secrets::Error::new)
    });
    #[cfg(unix)]
    aterm_agent::service_repair_rpc::install_dispatcher(service_repair::rpc)
        .expect("service repair dispatcher is installed once before any listener");
    #[cfg(unix)]
    aterm_agent::service_repair_rpc::install_startup_recovery(service_repair::start_recovery)
        .expect("service recovery callback is installed once before any coordinator");
    // Every `aterm pkg` download races this Mac's fleet members unless `[packages] peers` says
    // otherwise, and its doctor counts them; the read runs only when one of those asks for it.
    #[cfg(unix)]
    atpkg::peers::set_fleet(unit_peers::fleet);
    // With the mesh switch on, a download takes a member's unit over a `Units` op of its node
    // service first, and dials its port 7487 where that gives none. Inert with it off.
    #[cfg(unix)]
    atpkg::peers::set_units_dialer(aterm_gui::dial_units);
    // With the mesh switch on, a fleet dial meets an `atm/1` listener by the verdict this Mac's
    // own listener judges by, and a listener that seats no `Node` record for this Mac's key is
    // offered it at its desk (aterm-net reads no membership log itself). Inert with it off.
    #[cfg(unix)]
    aterm_gui::judge_mesh_dials();
    #[cfg(unix)]
    aterm_net::fleet_member::on_unknown_to(aterm_agent::mesh_offer::offer_node);
    // The Secrets stage of System State and `aterm init` asks ALab's members for an Allow once
    // its join waits on the ALab vault, as `aterm members join --no-wait` does.
    #[cfg(unix)]
    aterm_machines::init_secrets::on_ask_members(aterm_agent::members_cli::ask_members_admitted);
    #[cfg(target_os = "macos")]
    aterm_gui::on_system_machine_name_apply(
        |state, config, names| {
            use aterm_machines::port::System as _;
            let sys = aterm_machines::port::Live::new();
            if sys.euid() == 0 || aterm_machines::console::remote(&sys) {
                return Err("macOS naming waits for this Mac's console account".into());
            }
            if aterm_types::dirs::state_dir().as_deref() != Some(state)
                || aterm_types::dirs::aterm_config_path().as_deref() != Some(config)
            {
                return Err(
                    "macOS naming requires this account's current name and settings".into(),
                );
            }
            let current = || {
                aterm_agent::machine_name::publish::check_system_current(state, config, names)
                    .map_err(|why| why.phrase())
            };
            aterm_machines::root_steps::name_by_rule(&sys, names, &current)
                .map(|_| ())
                .map_err(|why| {
                    aterm_log::info!(
                        "machine name: existing macOS authorization did not apply: {why}"
                    );
                    "macOS naming did not finish through setup's existing authorization".to_owned()
                })
        },
        |state, config| {
            use aterm_machines::port::System as _;
            let sys = aterm_machines::port::Live::new();
            if sys.euid() == 0
                || aterm_machines::console::remote(&sys)
                || aterm_types::dirs::state_dir().as_deref() != Some(state)
                || aterm_types::dirs::aterm_config_path().as_deref() != Some(config)
            {
                return Err(
                    "macOS naming authorization requires this Mac's console account".into(),
                );
            }
            aterm_machines::root_steps::name_rule_current(&sys)
        },
    );
    // Every removal path signs its table cutoffs before its `Remove` (one-mesh §4.10, B11):
    // aterm-db writes table operations and depends on aterm-secrets, which signs the `Remove`.
    #[cfg(unix)]
    aterm_secrets::client::on_table_cuts(aterm_db::table::store::cut_removed_writer);
    // The fixed collector runs before GUI initialization or any per-user state is read.
    #[cfg(target_os = "macos")]
    let early_args = argv.get(1..).unwrap_or(&[]);
    #[cfg(target_os = "macos")]
    if early_args
        .first()
        .is_some_and(|a| a == aterm_sysprobe::cpu_policy::COLLECTOR_VERB)
    {
        if aterm_uds::sudo::stdout_is_anon_pipe().is_err() {
            eprintln!("CPU policy collection requires its owned anonymous output pipe");
            return ExitCode::from(2);
        }
        return ExitCode::from(match cpu_policy_hang() {
            // A front-door test, this role's parent, acks and reads it: the role's deadline covers
            // the ack wait and the collection it wraps, and 5 s more, as the shipped pairing's
            // `COLLECT_FOR` + 5 s does for 5 s.
            Some(hang) => aterm_sysprobe::cpu_policy::collector_entry_within(
                &early_args[1..],
                hang.saturating_add(aterm_sysprobe::cpu_policy::COLLECT_FOR)
                    .saturating_add(std::time::Duration::from_secs(5)),
                hang,
            ),
            None => aterm_sysprobe::cpu_policy::collector_entry(&early_args[1..]),
        });
    }
    aterm_gui::mark_rust_main_start();
    #[cfg(unix)]
    shared_login::install_automatic();
    #[cfg(unix)]
    aterm_agent::harness::account_catalog::install_reader(shared_accounts::read);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    account_claims::install(claims_observation, claims_pusher);
    #[cfg(unix)]
    alab_journal::install();
    // The update record's publication as this Mac's `upd` row, and the lab's rows for the
    // updater (a release build only).
    #[cfg(unix)]
    alab_update::install();
    // The lab registry's preferences, as the harness's roster source (a release build only).
    #[cfg(unix)]
    alab_view::install();
    // The journal's last use, sign-ins and quota, for the account surfaces (a release build only).
    #[cfg(unix)]
    alab_seen::install();
    #[cfg(windows)]
    let _ = aterm_gui::attach_parent_console();
    let mut args_os = argv.into_iter();
    let argv0_os: Option<OsString> = args_os.next();
    let mut rest: Vec<OsString> = args_os.collect();

    let argv0 = argv0_os
        .as_ref()
        .and_then(|a| std::path::Path::new(a).file_name())
        .map(std::ffi::OsStr::to_string_lossy)
        .unwrap_or_default();
    let argv0 = argv0
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .unwrap_or(&argv0);
    let first = rest
        .first()
        .map(|a| a.to_string_lossy())
        .unwrap_or_default();
    match alias_route(argv0, &first) {
        AliasRoute::Ctl => return aterm_ctl::main_entry_with(rest, CTL_LOCAL),
        AliasRoute::Askpass => return aterm_ctl::askpass_main_entry(rest),
        #[cfg(target_os = "macos")]
        AliasRoute::MachinesAskpass => return aterm_machines::askpass::main(&rest),
        #[cfg(not(target_os = "macos"))]
        AliasRoute::MachinesAskpass => return machines_unavailable(),
        AliasRoute::Pkg => return package_entry(rest),
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

    // The window a sign-in on this Mac shows its provider's page in: hidden, off the roster,
    // started only by that sign-in's opener, which drives it over its pipes.
    if first == aterm_signin_window::VERB {
        return aterm_signin_window::main_entry(rest.get(1..).unwrap_or(&[]));
    }

    // aCaffeine's name before 0.145.0: an old login job runs `caffeine serve` at every login.
    if first == aterm_acaffeine::legacy::VERB {
        return aterm_acaffeine::legacy::main_entry(rest.get(1..).unwrap_or(&[]));
    }

    // The root LAN broker and its reader: hidden, off the roster.
    #[cfg(unix)]
    if first == lan_verb::VERB {
        return lan_verb::run(rest.get(1..).unwrap_or(&[]));
    }

    // The login job that resumes an unfinished setup run with no window: hidden, off the roster,
    // and read before anything answers its leading `--version` (`setup_resume::ENTRY`).
    #[cfg(target_os = "macos")]
    if setup_resume::is_entry(&rest) {
        return setup_resume::run();
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
        drop(first);
        if matches!(verb, aterm_cli::Verb::Agents) {
            let _ = aterm_cli::parse_args(rest);
            return ExitCode::SUCCESS;
        }
        if !rest.is_empty() {
            rest.remove(0);
        }
        let forwarded = rest;
        #[cfg(unix)]
        let mut forwarded = forwarded;
        return match verb {
            aterm_cli::Verb::Ctl => aterm_ctl::main_entry_with(forwarded, CTL_LOCAL),
            aterm_cli::Verb::Conn => aterm_ctl::conn_main_entry(forwarded),
            aterm_cli::Verb::ResourceRun => aterm_ctl::resource_run_main_entry_with_original_store(
                forwarded,
                original_root_store,
            ),
            aterm_cli::Verb::ResourceInspect => {
                #[cfg(unix)]
                if aterm_ctl::native_resource_inspection::register(
                    aterm_resource_host::inspect_native,
                )
                .is_err()
                {
                    return ExitCode::from(75);
                }
                aterm_ctl::resource_inspect_main_entry(forwarded)
            }
            aterm_cli::Verb::Pkg => package_entry(forwarded),
            aterm_cli::Verb::Agh => agh::cli::main_entry(forwarded),
            aterm_cli::Verb::Fleet => aterm_agent::fleet_cli::main_entry(forwarded),
            aterm_cli::Verb::Name => {
                #[cfg(unix)]
                let sync = converge::sync_saved_name;
                #[cfg(not(unix))]
                let sync =
                    || Err("saved system-name synchronization is supported on macOS only".into());
                aterm_agent::name_cli::main_entry(forwarded, sync)
            }
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
            #[cfg(unix)]
            aterm_cli::Verb::Release => release_probe::run(&forwarded),
            #[cfg(not(unix))]
            aterm_cli::Verb::Release => {
                eprintln!("aterm release probe: unavailable on this platform");
                ExitCode::from(2)
            }
            aterm_cli::Verb::Update => update_verb(&forwarded),
            #[cfg(unix)]
            aterm_cli::Verb::Db => alab_sheet::db_entry(forwarded),
            #[cfg(not(unix))]
            aterm_cli::Verb::Db => {
                eprintln!("aterm db: not available on this platform (Unix only)");
                ExitCode::from(2)
            }
            aterm_cli::Verb::Email => aterm_agent::harness::email_cli::main_entry(forwarded),
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
            #[cfg(unix)]
            aterm_cli::Verb::Init => run_init(InitEntry::Typed(forwarded)),
            #[cfg(not(unix))]
            aterm_cli::Verb::Init => {
                eprintln!("aterm init: not available on this platform (Unix only)");
                ExitCode::from(2)
            }
            #[cfg(any(unix, windows))]
            aterm_cli::Verb::Status => machine_status::main_entry(&forwarded),
            #[cfg(not(any(unix, windows)))]
            aterm_cli::Verb::Status => {
                eprintln!("aterm status: not available on this platform (Unix and Windows only)");
                ExitCode::from(2)
            }
            #[cfg(unix)]
            aterm_cli::Verb::Secrets => {
                let first = forwarded.first().and_then(|s| s.to_str());
                if first == Some("need") {
                    // The need verbs read the lab's table, which aterm-db holds.
                    forwarded.remove(0);
                    return aterm_db::credentials::cli::main_entry(forwarded);
                }
                if first == Some("service-repair") {
                    forwarded.remove(0);
                    return service_repair::run(forwarded);
                }
                let sync_after = matches!(
                    first,
                    Some("join" | "sync" | "set" | "init" | "create" | "edit")
                );
                let code = aterm_secrets::cli::main_entry(forwarded);
                if code == ExitCode::SUCCESS
                    && sync_after
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
                aterm_keeper::cli::main_entry(forwarded, aterm_stamp::app_version())
            }
            aterm_cli::Verb::Acaffeine => {
                aterm_gui::install_session_log();
                aterm_acaffeine::cli::main_entry(forwarded, aterm_stamp::app_version())
            }
            aterm_cli::Verb::Agents => ExitCode::SUCCESS,
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
        drop(first);
        if !rest.is_empty() {
            rest.remove(0);
        }
        return aterm_cli::amacs_launch::run(rest);
    }

    if aterm_cli::is_tool_candidate(Some(&first)) {
        if store_resolves(&first) {
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

        if pending_stub_resolves(&first) {
            let Some((tool, tool_args)) = rest.split_first() else {
                return ExitCode::from(2);
            };
            let mut pending_args: Vec<OsString> = vec![OsString::from("__pending"), tool.clone()];
            pending_args.extend(tool_args.iter().cloned());
            return atpkg::cli::main_entry(pending_args);
        }

        if atpkg::stub::describe(&first).is_some() || atpkg::stub::program_of(&first) != first {
            let Some((tool, tool_args)) = rest.split_first() else {
                return ExitCode::from(2);
            };
            let mut run_args: Vec<OsString> = vec![OsString::from("run"), tool.clone()];
            run_args.push(OsString::from("--"));
            run_args.extend(tool_args.iter().cloned());
            return atpkg::cli::main_entry(run_args);
        }
    }

    let scan = rest.get(..payload_boundary(&rest)).unwrap_or(&rest);
    let force_session = argv0 == "aterm-cli" || scan.iter().any(|a| a == "--session");
    let windowish = is_windowish(scan, force_session, stdin_is_terminal());
    let mode_args = take_no_reroute(&strip_mode_flags(&rest));
    if windowish {
        #[cfg(target_os = "macos")]
        if let Some(code) =
            aterm_gui::prepare_window_launch(&mode_args, std::env::args_os().skip(1).collect())
        {
            return code;
        }
        if lab_window_guarded {
            // The actual guarded image must retain its original description.
            // Forwarding a plain launch would leave that custody in this short-
            // lived sender instead of the window it was intended to protect.
            return run_window(mode_args, WindowHost::ThisProcess, &rest);
        }
        let spawn = match plain_launch_policy(&rest) {
            ControlFlow::Break(code) => return code,
            ControlFlow::Continue(spawn) => spawn,
        };
        let handoff: std::borrow::Cow<'_, [OsString]> = match &spawn {
            Some(request) => std::borrow::Cow::Owned(spawn_handoff_argv(request)),
            None => std::borrow::Cow::Borrowed(&rest),
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
        let ensured = match layout.ensure_dir(&dir) {
            Ok(()) => true,
            Err(error) => {
                aterm_log::warn!(
                    "aterm: reroute dir not created ({error}); this session does not reroute the upstream toolchain names (cargo, rustc, z3, …) — `aterm pkg doctor` explains (aterm help reroute)"
                );
                dir.is_dir()
            }
        };
        if ensured {
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
    let commons_maintenance = if session_lane_is_interactive() {
        let startup = commons_startup();
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
        startup.map(|mut startup| {
            Box::new(move || startup.maintain()) as aterm_cli::SessionMaintenance
        })
    } else {
        None
    };
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    let commons_maintenance = None;

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

    session_lane(
        quiet,
        #[cfg(unix)]
        commons_maintenance,
    )
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
    let start = std::time::Instant::now();
    loop {
        if spoke.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        let Some(left) = silent.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else {
            return false;
        };
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
fn session_lane(
    quiet: bool,
    #[cfg(unix)] maintenance: Option<aterm_cli::SessionMaintenance>,
) -> ExitCode {
    aterm_cli::session_main(
        quiet,
        #[cfg(unix)]
        maintenance,
    )
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
        .any(|a| matches!(a.to_str(), Some("--headless" | "--diagnose")));
    if in_process {
        WindowHost::ThisProcess
    } else {
        WindowHost::WindowedSibling
    }
}

fn run_window(args: Vec<OsString>, host: WindowHost, handoff: &[OsString]) -> ExitCode {
    #[cfg(target_os = "macos")]
    if let Some(code) =
        aterm_gui::prepare_window_launch(&args, std::env::args_os().skip(1).collect())
    {
        return code;
    }
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
    // check, before the Commons runtime and, on macOS, aCaffeine's watch, which starts the mug
    // that keeps the Mac awake unless the person turned it off.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if ordinary {
        aterm_gui::check_window_args(args.clone());
    }
    let first_run = wire_window(&args);
    // A window's automatic LAN uses go only while `[swarm] enabled` leaves sharing on.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if ordinary && let Some(home) = aterm_types::dirs::home_dir() {
        commons_lan::install_sharing(&home);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let mut commons = ordinary.then(commons_startup).flatten();
    #[cfg(target_os = "macos")]
    if ordinary {
        spawn_acaffeine_watch();
    }
    #[cfg(target_os = "macos")]
    let mut input_maintenance = ordinary.then(input_attachments::spawn).flatten();
    #[cfg(target_os = "linux")]
    let mut input_maintenance: Option<aterm_gui::HostMaintenance> = None;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let input_maintenance: Option<aterm_gui::HostMaintenance> = None;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let (network_source, mut services) = ordinary
        .then(network_state_source)
        .flatten()
        .map_or((None, None), |(source, startup)| (Some(source), startup));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let maintenance = if input_maintenance.is_some() || commons.is_some() || services.is_some() {
        Some(Box::new(move || {
            let inputs = input_maintenance.as_mut().and_then(|keep| keep());
            let commons = commons.as_mut().and_then(commons_lan::Startup::maintain);
            let services = services
                .as_mut()
                .and_then(host_startup::Constructor::maintain);
            [inputs, commons, services].into_iter().flatten().min()
        }) as aterm_gui::HostMaintenance)
    } else {
        None
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let maintenance = input_maintenance;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let network_source = None;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    aterm_gui::install_host_quit(commons_stop_at_quit);
    #[expect(
        clippy::disallowed_methods,
        reason = "the one window door: every window route reaches the GUI here"
    )]
    aterm_gui::main_entry_with_network(args, first_run, network_source, maintenance);
    ExitCode::SUCCESS
}

/// Whether no word of `args`, the command after `-e` included, is `--headless`, `--diagnose` or
/// a `--control-sock` with a socket of its own: with no value, or one other than `0` and `off`
/// (no socket, as `--no-control-sock`) and the empty one (the default socket)
/// ([`aterm_types::control_socket::socket_directive`]). A launch with one starts no Commons
/// runtime, aCaffeine or network source. [`run_window`] lets the window's parser end a one-shot
/// or refused one of the rest first.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ordinary_window(args: &[OsString]) -> bool {
    use aterm_types::control_socket::{SocketDirective, socket_directive};
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let private = match word.to_str() {
            Some("--headless" | "--diagnose") => true,
            Some("--control-sock") => words.next().is_none_or(|value| {
                value.to_str().is_none_or(|s| {
                    matches!(
                        socket_directive(Some(s), None),
                        SocketDirective::Explicit(_)
                    )
                })
            }),
            _ => false,
        };
        if private {
            return false;
        }
    }
    true
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

/// How often an ordinary window makes sure aCaffeine keeps this Mac awake.
#[cfg(target_os = "macos")]
const ACAFFEINE_WATCH: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Keep this Mac awake beside an ordinary window, at launch, once the window has set up its
/// logging ([`EARLY_LOG_WAIT`]), and every [`ACAFFEINE_WATCH`]
/// ([`aterm_acaffeine::cli::keep_awake_watch`]): at launch, what the old `aterm caffeine` name
/// left retired ([`aterm_acaffeine::legacy::tidy`]) and the temporaries cut-short writes left
/// removed ([`aterm_acaffeine::state::sweep_temps`],
/// [`aterm_acaffeine::launchd::sweep_plist_temps`]); each pass, the installed app's Enable at
/// Startup turned on and the mug started, each unless the person turned it off, whose off is
/// never overwritten. A trouble that lasts is an exceptions entry
/// ([`aterm_gui::note_keep_awake_failing`]), raised again only when its why changes, and
/// cleared on each pass with none ([`aterm_acaffeine::cli::EntryWatch`]), so one an earlier
/// window raised before a relaunch goes too.
#[cfg(target_os = "macos")]
fn spawn_acaffeine_watch() {
    let watch = || {
        use aterm_acaffeine::cli::{EntryWatch, EntryWord, Kept};
        // The window installs its logger in main_entry_with_network, after this thread starts;
        // a line the first pass logs before that would be dropped.
        let _ = aterm_log::wait_for_logger(EARLY_LOG_WAIT);
        let mut entry = EntryWatch::default();
        loop {
            let (kept, lasting) = aterm_acaffeine::cli::keep_awake_watch();
            match &kept {
                Kept::Failed(why) => {
                    aterm_log::warn!("acaffeine: could not keep this Mac awake: {why}");
                }
                kept if !entry.raised() => aterm_log::debug!("acaffeine: {}", kept.line()),
                _ => {}
            }
            match entry.step(&kept, lasting) {
                Some(EntryWord::Raise(why)) => aterm_gui::note_keep_awake_failing(&why),
                Some(EntryWord::Clear) => aterm_gui::clear_keep_awake_failing(),
                None => {}
            }
            std::thread::sleep(ACAFFEINE_WATCH);
        }
    };
    if let Err(error) = std::thread::Builder::new()
        .name("aterm-acaffeine-watch".into())
        .spawn(watch)
    {
        warn_when_logging(format!("acaffeine: could not start with aterm: {error}"));
    }
}

/// Only long-lived ordinary hosts contend for Commons transport ownership.
/// One-shot verbs return before these call sites; private labs need a guarded attachment.
#[cfg(any(target_os = "macos", target_os = "linux"))]
static COMMONS_RUNTIME: std::sync::OnceLock<aterm_db::runtime::Runtime> =
    std::sync::OnceLock::new();

/// How long an ending process waits for its Commons runtime's owner thread to stop
/// (`aterm_db::runtime::Runtime::shutdown`). Past it the process exits anyway and the network
/// history stays open, as after a kill.
#[cfg(unix)]
const COMMONS_STOP_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// The node job's quit step (`aterm_gui::install_host_quit`): its Commons runtime stops, which
/// writes the network history's pending transitions and closes it. Bounded by
/// [`COMMONS_STOP_WITHIN`]; a second call finds the runtime stopped.
#[cfg(unix)]
fn node_commons_stop_at_quit() {
    if let Some(runtime) = NODE_COMMONS.get()
        && !runtime.shutdown(COMMONS_STOP_WITHIN)
    {
        aterm_log::warn!(
            "Commons node runtime still stopping after {COMMONS_STOP_WITHIN:?}: the network history stays open"
        );
    }
}

/// The window's quit step (`aterm_gui::install_host_quit`), run by the window's quit and by a
/// terminating signal it armed for: its Commons runtime is a static nothing drops, so it stops
/// here, which writes the network history's pending transitions and closes it. Bounded by
/// [`COMMONS_STOP_WITHIN`]; a second call finds the runtime stopped.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn commons_stop_at_quit() {
    if let Some(runtime) = COMMONS_RUNTIME.get()
        && !runtime.shutdown(COMMONS_STOP_WITHIN)
    {
        aterm_log::warn!(
            "Commons runtime still stopping after {COMMONS_STOP_WITHIN:?}: the network history stays open"
        );
    }
}

/// What the node job's background Commons runtime observes (`aterm node serve`, which keeps its
/// runtime to itself and serves the census): the presence its account claims lapse by.
#[cfg(any(target_os = "macos", target_os = "linux"))]
static NODE_OBSERVATION: std::sync::OnceLock<aterm_db::network_state::ObservationHandle> =
    std::sync::OnceLock::new();

/// What hands the node job's account claims rows to its peers at once (its background Commons
/// runtime's push, `aterm_db::runtime::Pusher`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
static NODE_PUSHER: std::sync::OnceLock<aterm_db::runtime::Pusher> = std::sync::OnceLock::new();

/// This process's Commons runtime's push, where one runs here (a window's or session lane's, else
/// the node job's): how a quiet or off claims row reaches the peers before the Mac sleeps or
/// stops.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn claims_pusher() -> Option<aterm_db::runtime::Pusher> {
    COMMONS_RUNTIME
        .get()
        .map(aterm_db::runtime::Runtime::pusher)
        .or_else(|| NODE_PUSHER.get().cloned())
}

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
    if !commons_host_enabled() {
        return None;
    }
    aterm_db::runtime::read_live_publication(
        &aterm_secrets::client::Paths::live().ok()?,
        &aterm_types::dirs::aterm_config_path()?,
    )
    .ok()
    .map(account_claims::current_observation)
}

/// Keep this process's start intent in its host's maintenance until the canonical runtime is
/// published, respecting sharing on every attempt.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn commons_startup() -> Option<commons_lan::Startup> {
    if !commons_host_enabled() || COMMONS_RUNTIME.get().is_some() {
        return None;
    }
    let paths = aterm_secrets::client::Paths::live().ok()?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let home = paths.home.clone();
    let mut startup =
        commons_lan::Startup::new(&home, move || start_commons_runtime(paths.clone(), &config));
    let _ = startup.maintain();
    Some(startup)
}

/// Start this process's Commons runtime now, with `paths` and `config`. On a Mac its LAN
/// traffic goes through the LAN broker; its first LAN use waits for the broker's answer.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn start_commons_runtime(paths: aterm_secrets::client::Paths, config: &std::path::Path) -> bool {
    if COMMONS_RUNTIME.get().is_some() {
        return true;
    }
    let config = config.to_owned();
    let rounds = alab_register::reports(paths.clone(), config.clone());
    match aterm_db::runtime::Runtime::start_with_reports(
        paths,
        config,
        |status| {
            // In a window, the owner thread can report its first status before the window installs
            // its logger, and it reports a status only when it changes, so a dropped one stays
            // unsaid. A terminal session has settled its logging before this runtime starts.
            let _ = aterm_log::wait_for_logger(EARLY_LOG_WAIT);
            // A refusal warns once per cause (`Status::news`); a standing one logs at debug.
            if let Some(refusal) = status.news() {
                aterm_log::warn!(
                    "Commons automatic control repair refused local configuration or trust state: {refusal}"
                );
            } else if status == aterm_db::runtime::Status::LanHeld {
                aterm_log::info!("Commons control repair: {status:?}");
            } else {
                aterm_log::debug!("Commons control repair: {status:?}");
            }
        },
        rounds,
    ) {
        Ok(runtime) => {
            if COMMONS_RUNTIME.set(runtime).is_ok() {
                // With the one mesh on, this process's push asks the node service, whose mesh
                // relays the tables, to wait for a neighbour's ack (§11 B7b).
                if aterm_types::mesh_switch::MESH
                    && let Some(state_dir) = aterm_types::dirs::state_dir()
                    && let Some(runtime) = COMMONS_RUNTIME.get()
                {
                    relay_by_node(runtime, state_dir);
                }
                alab_journal::start_backfill();
                alab_observed::start_sweep();
            }
            true
        }
        Err(error) => {
            warn_when_logging(format!("Commons runtime could not start: {error}"));
            false
        }
    }
}

/// Join the independent producers, loading fallback history once before the GUI starts and
/// retaining a failed service-owner start for the host's maintenance.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn network_state_source() -> Option<(
    aterm_gui::NetworkSnapshotSource,
    Option<host_startup::Constructor>,
)> {
    use std::sync::OnceLock;

    if !commons_host_enabled() {
        return None;
    }

    static SERVICES: OnceLock<aterm_db::service_state::Runtime> = OnceLock::new();
    // Read at each look, not once: with `[swarm] enabled = false` the runtime starts only once a
    // person turns sharing on ([`commons_startup`]), and the page then shows what it hears.
    let started = COMMONS_RUNTIME.get().is_some();
    let network = || {
        COMMONS_RUNTIME
            .get()
            .map(aterm_db::runtime::Runtime::network_observation)
    };
    // This local metadata observer is independent of sharing. Its constructor does no
    // sampling; only WithServices reads ask the published worker for an observation.
    let startup = if SERVICES.get().is_none() {
        aterm_secrets::client::Paths::live()
            .ok()
            .zip(aterm_types::dirs::aterm_config_path())
            .map(|(paths, config)| {
                let mut startup = host_startup::Constructor::new(move || {
                    if SERVICES.get().is_some() {
                        return true;
                    }
                    match aterm_db::service_state::Runtime::start(paths.clone(), config.clone()) {
                        Ok(runtime) => {
                            let _ = SERVICES.set(runtime);
                            true
                        }
                        Err(error) => {
                            warn_when_logging(format!("Service observer could not start: {error}"));
                            false
                        }
                    }
                });
                let _ = startup.maintain();
                startup
            })
    } else {
        None
    };
    let retained = if started {
        None
    } else {
        aterm_types::dirs::aterm_config_path()
            .and_then(|config| aterm_db::network_state::read_history(&config).ok())
    };
    Some((
        network_state_source_from(
            network,
            || {
                SERVICES
                    .get()
                    .map(aterm_db::service_state::Runtime::observation)
            },
            retained,
        ),
        startup,
    ))
}

/// Compose independent observers, looking up either handle lazily because its owner may start
/// after this source does.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn network_state_source_from(
    network: impl Fn() -> Option<aterm_db::network_state::ObservationHandle> + Send + Sync + 'static,
    services: impl Fn() -> Option<aterm_db::service_state::ObservationHandle> + Send + Sync + 'static,
    retained: Option<aterm_types::network_state::NetworkObservation>,
) -> aterm_gui::NetworkSnapshotSource {
    offering_to_fleet(
        network_state_source_with_readers(
            move || network().and_then(|network| network.try_publication()),
            move || services().and_then(|services| services.try_publication()),
            retained,
            || (std::time::Instant::now(), aterm_time::unix_millis()),
        ),
        aterm_db::fleet::folder(),
    )
}

/// `source`, with each look's snapshot offered to the fleet tables `fleet` (`aterm_db::fleet`),
/// which fold the observer's machines and ask it nothing. The tables take it in the state it
/// was observed in: one no observer is making (none started, its publication expired) is held,
/// never live (`aterm_db::fleet::observe`). The offer never waits and a snapshot it already
/// holds changes nothing, so a look costs a pointer comparison.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn offering_to_fleet(
    source: aterm_gui::NetworkSnapshotSource,
    fleet: &'static aterm_db::fleet::Folder,
) -> aterm_gui::NetworkSnapshotSource {
    std::sync::Arc::new(move |read: aterm_gui::NetworkRead| {
        let snapshot = source(read)?;
        fleet.offer_network(&snapshot);
        Some(snapshot)
    })
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
    Arc::new(move |read: aterm_gui::NetworkRead| {
        // Both the UI and the Commons beacon read this callback. A paused reader
        // must not pause drawing or masquerade as a new startup observation.
        let mut current = match current.try_lock() {
            Ok(current) => current,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        if let Some(publication) = network() {
            current.1 = Some(publication);
        }
        // A services read asks their observer for its next sample, so only a reader that shows
        // their states reads them; any other gets their last sample, aged below as it ages.
        if read == aterm_gui::NetworkRead::WithServices
            && let Some(publication) = services()
        {
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
        Some(current.0.clone())
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
        .any(|a| matches!(a.to_str(), Some("--headless" | "--diagnose")))
    {
        let _ = std::thread::Builder::new()
            .name("aterm-machines-retire".into())
            .spawn(aterm_machines::cli::retire_lan_agent);
    }
    #[cfg(target_os = "macos")]
    let first_run = {
        let restoring_window = std::env::var_os(aterm_update::RELAUNCH_OF_RUNNING_ENV).is_some();
        first_run_start(args, restoring_window, doctor_startup_env())
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
        launch_history: initial_setup_launch_history,
        apply: restart_initial_setup,
        check: check_initial_setup,
        restore: restore_initial_setup,
        status: initial_setup_status,
        own_console: || !aterm_machines::console::remote_here(),
        observe: observe_initial_setup,
        look_for_token: look_for_github_token,
        stop: || {
            stop_initial_setup();
        },
        settle: settle_initial_setup,
        cut_off: cut_off_initial_setup,
        sync_on: turn_sync_on,
        ask_access: person_asked_for_access,
        tools_installed: command_line_tools_installed,
        fda_granted: verified_full_disk_access_granted,
        fda_off: full_disk_access_reads_off,
        join_phrase: join_alab_with_phrase,
    });
    aterm_gui::register_resource_runner();
    first_run
}

/// What keeps a launch from first-run setup, which belongs to a window started by the person at
/// this Mac: a private state root (a laboratory's) or CI, a scope outside the person's, or an
/// ssh login. A window whose screen is away as it starts (another login at it, or nobody) keeps
/// its setup, which waits for the screen ([`wait_for_the_screen`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default)]
struct DoctorStartupEnv {
    isolated: bool,
    outside_person_scope: bool,
    /// Started from an ssh login, whose markers it inherited: the person who started it is not
    /// at this Mac, so setup waits for a window started at it.
    remote: bool,
}

#[cfg(target_os = "macos")]
fn doctor_startup_env() -> DoctorStartupEnv {
    doctor_startup_env_from(
        |key| std::env::var_os(key),
        aterm_cli::macos_doctor::lasting_scope_refusal(),
    )
}

/// [`doctor_startup_env`] from the environment variable reader `var` and the process's lasting
/// scope refusal (`aterm_cli::macos_doctor::lasting_scope_refusal`), which a test feeds from
/// scratch account paths.
#[cfg(target_os = "macos")]
fn doctor_startup_env_from(
    var: impl Fn(&str) -> Option<OsString>,
    lasting_scope_refusal: Option<String>,
) -> DoctorStartupEnv {
    DoctorStartupEnv {
        isolated: var("ATERM_STATE_HOME").is_some() || var("CI").is_some(),
        outside_person_scope: lasting_scope_refusal.is_some(),
        remote: var("SSH_CONNECTION").is_some() || var("SSH_TTY").is_some(),
    }
}

/// How long a window whose screen is away waits before it reads the console again by itself.
#[cfg(target_os = "macos")]
const SCREEN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Wait until this login holds the Mac's screen (`away` reads `false`,
/// `aterm_machines::console::remote_here`): a window started while another login holds it, or
/// nobody does, starts its setup once the person is back, never not at all. Each look waits
/// [`SCREEN_WAIT`] by `wait`, which a wake (the console's return among them) ends early; one
/// journal line says it waits. The login job runs the stages that need nobody meanwhile.
#[cfg(target_os = "macos")]
fn wait_for_the_screen(mut away: impl FnMut() -> bool, mut wait: impl FnMut(std::time::Duration)) {
    let mut said = false;
    while away() {
        if !std::mem::replace(&mut said, true) {
            aterm_log::info!(
                "first-run setup: waits for this login at the Mac's screen before its startup"
            );
        }
        wait(SCREEN_WAIT);
    }
}

/// A window's first-run start: it waits for this login at the Mac's screen
/// ([`wait_for_the_screen`] by `away` and `wait`), and only then holds the window lock by `hold`
/// (from there this process runs setup and the login job stands down to it) and runs `start`,
/// whose return lets the hold go, so the job goes on with the stages that need nobody for as long
/// as the screen is away, and with a run reopened once this start's run returned.
#[cfg(target_os = "macos")]
fn start_at_the_screen<H>(
    away: impl FnMut() -> bool,
    wait: impl FnMut(std::time::Duration),
    hold: impl FnOnce() -> H,
    start: impl FnOnce(),
) {
    wait_for_the_screen(away, wait);
    let _held = hold();
    start();
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

/// The first-run setup a new window starts, or none: one start for a launch and an update
/// successor (`restoring_window`) alike, so a successor admits a pending plan and starts its
/// background stages at once as any launch does, and only a person stage waits for a person
/// (init-on-by-default design §6.1, §6.4).
#[cfg(target_os = "macos")]
fn first_run_start(
    args: &[OsString],
    restoring_window: bool,
    env: DoctorStartupEnv,
) -> Option<fn() -> bool> {
    first_run_window_eligible(first_run_window_args(args, restoring_window), env)
        .then_some(apply_first_run_doctor as fn() -> bool)
}

/// A window's first-run setup, a launch's and an update successor's alike. Once the launch's
/// startup is done, the same thread takes up, for the window's life, a run another process admits
/// or reopens while no setup runs here ([`setup_take_up`]), so its person stages never wait for a
/// launch the person need not make.
#[cfg(target_os = "macos")]
fn apply_first_run_doctor() -> bool {
    match std::thread::Builder::new()
        .name("aterm-workstation-setup".into())
        .spawn(|| {
            resume_at_the_screen();
            setup_take_up::take_up_with(&mut LiveTakeUp::default());
        }) {
        Ok(_) => true,
        Err(why) => {
            aterm_log::warn!("first-run macOS doctor worker could not start: {why}");
            false
        }
    }
}

/// Start or resume setup as a launch does, once the screen is this login's (holding the window
/// lock while it prepares, so the login job gives way), returning once that startup ended.
#[cfg(target_os = "macos")]
fn resume_at_the_screen() {
    start_at_the_screen(
        aterm_machines::console::remote_here,
        wait_for_setup_wake,
        hold_setup_window,
        // A plan aterm chose by itself on an earlier launch widens through the watch,
        // which looks as soon as the startup is done ([`AlabWatch`]).
        || run_startup_setup(None),
    );
}

/// The window's live [`setup_take_up::TakeUp`]: the setup records' watch (started again once
/// one goes, and none meanwhile but the take-up's timer, or a setup wake), this process's
/// startups and run workers, and a launch's own resume.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct LiveTakeUp {
    watch: Option<aterm_cli::macos_doctor::SetupRecordsWatch>,
    said: Option<String>,
}

#[cfg(target_os = "macos")]
impl setup_take_up::TakeUp for LiveTakeUp {
    fn arm(&mut self) -> bool {
        if self.watch.is_none() {
            match aterm_cli::macos_doctor::SetupRecordsWatch::start() {
                Ok(watch) => self.watch = Some(watch),
                Err(why) => self.say(format!(
                    "first-run setup: this window hears no change of the setup records ({why}); \
                     it looks on its timer"
                )),
            }
        }
        self.watch.is_some()
    }
    fn wait(&mut self, within: std::time::Duration) {
        let ended = match &self.watch {
            Some(watch) => watch.wait(within).err(),
            None => {
                wait_for_setup_wake(within);
                None
            }
        };
        if let Some(why) = ended {
            self.watch = None;
            self.say(format!(
                "first-run setup: the setup records' watch ended ({why}); a new one starts"
            ));
        }
    }
    fn setup_here(&mut self) -> bool {
        STARTUP_RUNS.load(std::sync::atomic::Ordering::Acquire) != 0
            || HEAL_STARTING.load(std::sync::atomic::Ordering::Acquire)
            || SETUP_RUN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    }
    fn resumable(&mut self) -> Result<Option<u64>, String> {
        aterm_cli::macos_doctor::initial_setup_resumable()
    }
    fn resume(&mut self) {
        resume_at_the_screen();
    }
    fn now(&mut self) -> std::time::Instant {
        std::time::Instant::now()
    }
    fn say(&mut self, line: String) {
        if self.said.as_deref() != Some(line.as_str()) {
            aterm_log::info!("{line}");
            self.said = Some(line);
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
fn run_startup_setup(mut widened: Option<Widened>) {
    let running = StartupRunning::enter();
    preload_setup_history();
    run_repaired_startups(|reserved| {
        let mut password = None;
        let (prepared, mut sent, recovered) =
            prepare_first_run_doctor(reserved, |initial, on_begin| {
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

/// Run what the startup prepared, keeping its watch maintenance.
#[cfg(target_os = "macos")]
fn run_prepared_startup_with(
    prepared: PreparedSetup<StartupClaim>,
    password: Option<aterm_secrets::SecretBytes>,
    sent: StartupSent,
    maintain: impl FnMut(WatchMaintenance),
) {
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

/// The longest a watch waits after `looks` looks that heard no ALab vault.
#[cfg(any(target_os = "macos", test))]
fn alab_look_after(looks: u32) -> std::time::Duration {
    ALAB_LOOK_FIRST
        .saturating_mul(1 << looks.min(16))
        .min(ALAB_LOOK_LONGEST)
}

/// What the automatic profile is waiting for: local authenticated membership, or a failed
/// read/publication whose retry must retain its backoff.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlabWait {
    Signal,
    Retry,
}

/// One durable membership listener across looks. Arm before reading the level so admission
/// between the previous look and this wait cannot be lost.
#[cfg(any(target_os = "macos", test))]
struct AlabMembershipWait<W> {
    seen: WakeMarks,
    watch: Option<W>,
}

#[cfg(any(target_os = "macos", test))]
impl<W> AlabMembershipWait<W> {
    fn new(seen: WakeMarks) -> Self {
        Self { seen, watch: None }
    }

    fn wait(
        &mut self,
        wakes: &SetupWakes,
        limit: std::time::Duration,
        member: impl FnOnce() -> bool,
        start: impl FnOnce() -> Option<W>,
        whole: impl Fn(&W) -> bool,
    ) -> bool {
        if !self.watch.as_ref().is_some_and(&whole) {
            self.watch = start();
        }
        if member() {
            return true;
        }
        if self.watch.as_ref().is_some_and(whole) {
            wakes.back_off_alab(&mut self.seen, limit)
        } else {
            // A failed listener must not spin on its own loss notification. Try again on the
            // ordinary retry deadline; unchanged healthy listeners are retained.
            wakes.back_off_run(&mut self.seen, limit.min(SETUP_RETRY_WAIT))
        }
    }
}

/// What [`widen_on_alab_signal`] does on this Mac, or in a test.
#[cfg(any(target_os = "macos", test))]
trait AlabWatch {
    /// The saved plan is aterm's own provisional one, which the ALab signal widens, or an
    /// upgrade of it this watch holds; an error when aterm's records cannot be read just now.
    fn widens(&mut self) -> Result<bool, String>;
    /// Wait for a local membership fact, or back off a failed read/publication.
    fn wait(&mut self, why: AlabWait, limit: std::time::Duration);
    /// Look for the ALab signal: heard, so the plan widens now with nothing asked (design §3.5).
    /// An upgrade this watch holds was heard.
    fn heard(&mut self) -> Result<bool, String>;
    /// Widen the plan: the widened plan's revision.
    fn look(&mut self) -> Result<Option<u64>, String>;
    /// Run the widened plan as a launch runs it.
    fn run(&mut self);
}

/// While aterm's own plan is provisional, follow the locally committed authenticated vault
/// membership. This reads no network; the subscriber owns admission. Once membership arrives,
/// widen and run at once, with no discovery cooldown or question (design §6.1).
#[cfg(any(target_os = "macos", test))]
fn widen_on_alab_signal<W: AlabWatch>(watch: &mut W) {
    let mut looks = 0;
    let mut why = AlabWait::Signal;
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
        // The first look goes at once: a signal heard on an earlier launch widens this one as it
        // starts.
        if looks > 0 {
            watch.wait(
                if eligible { why } else { AlabWait::Retry },
                alab_look_after(looks - 1),
            );
        }
        looks = looks.saturating_add(1);
        why = AlabWait::Retry;
        if !eligible {
            continue;
        }
        match watch.heard() {
            Ok(false) => {
                why = AlabWait::Signal;
                continue;
            }
            Ok(true) => {}
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

/// Whether a watch of aterm's provisional plan has work in this launch
/// ([`aterm_cli::macos_doctor::provisional_setup_widens`], [`widens_here_with`]).
#[cfg(target_os = "macos")]
fn provisional_setup_widens_here() -> Result<bool, String> {
    widens_here_with(
        aterm_cli::macos_doctor::provisional_setup_widens,
        aterm_cli::macos_doctor::initial_setup_revision,
    )
}

/// Whether a watch of aterm's provisional plan has work in this launch: the records still widen
/// the plan in force (`widens`), and no Stop was asked in it for the run in force (`revision`,
/// [`setup_stop_requested`]). A person who pressed Stop gets no more automatic setup in this
/// launch, so nothing widens the run they stopped, even one the Stop came too late for (it
/// finished, and its records keep no Stop). The next launch may widen it, as may a later run in
/// this one (Set up now, a choice), which the Stop did not stop.
#[cfg(target_os = "macos")]
fn widens_here_with(
    widens: impl FnOnce() -> Result<bool, String>,
    revision: impl FnOnce() -> Result<u64, String>,
) -> Result<bool, String> {
    if !widens()? {
        return Ok(false);
    }
    Ok(!setup_stop_requested(revision()?))
}

#[cfg(target_os = "macos")]
struct LiveAlabWatch {
    membership: AlabMembershipWait<aterm_cli::macos_doctor::ParkWatch>,
    /// The upgrade this watch began, held while a commit that failed can still finish.
    upgrade: aterm_cli::macos_doctor::InitialSetupUpgrade,
    /// The plan the last look widens, or widened.
    widened: Widened,
    /// The number the widen took as the latest plan change ([`SETUP_APPLY_GENERATION`]) once
    /// the signal was heard: a later choice overtakes it while it waits for another setup, as it
    /// overtakes a choice waiting. Zero before the signal was heard.
    generation: u64,
}

#[cfg(target_os = "macos")]
impl LiveAlabWatch {
    fn new() -> Self {
        Self {
            membership: AlabMembershipWait::new(SETUP_WAKES.marks()),
            upgrade: aterm_cli::macos_doctor::InitialSetupUpgrade::default(),
            widened: Widened::default(),
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

    fn wait(&mut self, why: AlabWait, limit: std::time::Duration) {
        if why == AlabWait::Retry || self.upgrade.pending() {
            // State-file churn is no reason to spin a failed read or repeat a publication.
            SETUP_WAKES.back_off_run(&mut self.membership.seen, limit.min(SETUP_RETRY_WAIT));
            return;
        }
        self.membership.wait(
            &SETUP_WAKES,
            limit,
            aterm_cli::macos_doctor::alab_mac,
            || {
                aterm_cli::macos_doctor::ParkWatch::start(
                    aterm_cli::macos_doctor::Watches {
                        secrets: true,
                        ..aterm_cli::macos_doctor::Watches::default()
                    },
                    wake_setup_facts,
                )
                .ok()
            },
            aterm_cli::macos_doctor::ParkWatch::whole,
        );
    }

    fn heard(&mut self) -> Result<bool, String> {
        if self.upgrade.pending() {
            return Ok(true);
        }
        // Read again at the moment of the look: a Stop pressed while the watch waited holds it.
        if !provisional_setup_widens_here()? {
            return Ok(false);
        }
        if aterm_cli::macos_doctor::provisional_setup_widened()?.is_none() {
            return Ok(false);
        }
        // The latest plan change from now: a later choice overtakes it.
        let generation =
            SETUP_APPLY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        self.generation = generation;
        // Nobody chose it: while it waits, the page says it adds the ALab profile.
        SETUP_QUEUED_WIDEN.store(generation, std::sync::atomic::Ordering::Release);
        // Shown waiting the moment its write finds another setup holding the records (design
        // §3.3), never a run going.
        self.upgrade
            .noting_wait(move || queue_setup_press(generation));
        Ok(true)
    }

    fn look(&mut self) -> Result<Option<u64>, String> {
        let generation = self.generation;
        let looked = look_settling_with(&mut self.widened, &mut self.upgrade, |widened| {
            queue_setup_press(generation);
            wait_while_queued(|| {
                setup_choice_overtaken(generation)
                    || stop_covers(widened.sent)
                    || setup_stop_requested(widened.from)
            })
        });
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

/// One look of a watch that heard the ALab signal: the widened plan's revision once it is saved
/// (`Ok(Some)`); an upgrade still pending (a write that failed part-way) is finished or retired
/// by its next look; a newer aterm's records end the upgrade whatever it says, and the page
/// shows the newer aterm's setup (System State design §3.5).
///
/// An upgrade that finds another setup holding the records past its wait
/// ([`aterm_cli::macos_doctor::SETUP_BUSY`]) waits on, as a choice does (§3.3): `queue` shows it
/// waiting, waits a moment and says whether to ask again, `false` once a Stop covers it or a
/// later choice overtook it. An attempt that published nothing is then let go.
#[cfg(target_os = "macos")]
fn look_settling_with(
    widened: &mut Widened,
    upgrade: &mut impl WatchUpgrade,
    mut queue: impl FnMut(&Widened) -> bool,
) -> Result<Option<u64>, String> {
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
    // A newer aterm's records are final for this binary, whatever the upgrade says of its
    // attempt: it ends, and the page shows the newer aterm's setup.
    if matches!(&looked, Err(why) if setup_is_newer(why)) {
        upgrade.forget();
    }
    looked
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
            // A failed read cannot certify that the attempt published nothing. One the upgrade
            // let go (a write it cannot identify) leaves its Stop debt behind as a startup
            // admission's does: unconfirmed, it outlives this look's guard until a later
            // admission retires it ([`retire_unconfirmed_admissions`]).
            if !upgrade.pending()
                && let Some(under_way) = widened.under_way.take()
            {
                under_way.record_publication(Err(why.clone()));
            }
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
    offer: impl FnMut(aterm_types::macos_setup::Plan, StartupHandoff) -> aterm_gui::SetupAnswer,
) -> (PreparedSetup<StartupClaim>, StartupSent, bool) {
    let mut host = LiveSetupStartup {
        offer,
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

    /// The capture that binds this review failed and the review ends unbound: a fresh one binds
    /// the records, at once (`run_repaired_startups`, which starts none once a Stop covered this
    /// one).
    fn bind_afresh(&mut self) {}

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
    /// This review retired without adopting the records (it repaired corrupt ones, or its capture
    /// failed): a fresh review binds them.
    recovered: bool,
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
impl<F: FnMut(aterm_types::macos_setup::Plan, StartupHandoff) -> aterm_gui::SetupAnswer>
    SetupStartup for LiveSetupStartup<F>
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

    fn bind_afresh(&mut self) {
        self.recovered = true;
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

    fn offer(
        &mut self,
        initial: aterm_types::macos_setup::Plan,
    ) -> aterm_gui::WorkstationSetupDecision {
        startup_went_on();
        // An older aterm's note that its update successor left setup unstarted goes.
        aterm_gui::clear_setup_unfinished();
        let mut answer = (self.offer)(initial, self.sent.handoff());
        // A retry's wait starts once the offer is answered: the review's own coming to the
        // front is a setup wake, which must not end the wait before the next offer at once.
        self.retry_seen = SETUP_WAKES.count();
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
#[path = "main_startup_answer_tests.rs"]
mod startup_answer_tests;

#[cfg(target_os = "macos")]
use aterm_cli::macos_doctor::setup_is_newer;

/// Off macOS no setup is saved, so none is a newer aterm's (the retry tests run everywhere).
#[cfg(all(test, not(target_os = "macos")))]
fn setup_is_newer(_why: &str) -> bool {
    false
}

#[cfg(any(target_os = "macos", test))]
enum PreparedSetup<C> {
    Authorized { claim: C, revision: u64 },
    Finished,
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
    // The review is bound once: a capture that failed ends this review, never to bind a later
    // run's revision (a manual repair's) as this one's own, and never this launch's setup: after
    // the startup's pause a fresh review binds what the records hold then (`bind_afresh`), unless
    // a Stop covered this one meanwhile. A newer aterm's records end it as they are.
    let revision = match host.bind_authorization() {
        Ok(revision) => revision,
        Err(why) => {
            if setup_is_newer(&why) {
                host.report_rejection(&why);
                return Err(why);
            }
            host.report_failure(&why);
            if !host.stopped() {
                host.backoff();
            }
            if !host.stopped() {
                host.bind_afresh();
            }
            return Ok(PreparedSetup::Unavailable);
        }
    };
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
            host.initial().map(BeforeOffer::Initial)
        })();
        if host.stopped() {
            return Ok(PreparedSetup::Unavailable);
        }
        let initial = match ready {
            Ok(BeforeOffer::Initial(initial)) => initial,
            Ok(BeforeOffer::Resume(admission)) => {
                // A startup that resumes a run an earlier process admitted (Quit & Reopen's or an
                // update's relaunch, say) goes on with it at once, with nothing offered again.
                return Ok(PreparedSetup::Authorized {
                    claim,
                    revision: admission.revision,
                });
            }
            Ok(BeforeOffer::Finished) => return Ok(PreparedSetup::Finished),
            Ok(BeforeOffer::Superseded) => return Ok(PreparedSetup::Unavailable),
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
            look_at_finished_setup();
            // A finished setup runs no Secrets stage: while the plan in force keeps the vault in
            // sync ([`vault_sync_kept`]), the mend keeps a vault subscriber whose plist runs a copy
            // that is gone, or that this window owes the Mac.
            (setup_io().mend_subscriber)();
            // The converge pass's window lane, as the mend: a finished setup runs no Secrets stage.
            (setup_io().converge_lane)();
            // The existing keeper owns this request; a busy profile cannot consume it or
            // hold up the watch for the ALab vault that starts once this returns.
            (setup_io().start_follow_up)();
        }
        // No run from this review (an update's successor that withholds it, a Stop, a newer
        // choice, a capture that met the records being written, which asks a fresh review of its
        // own): the window lane is this launch's converge pass beside the subscriber's, each item
        // keeping its own off switch.
        PreparedSetup::Unavailable => (setup_io().converge_lane)(),
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
/// no slot. Each finite attempt acquires both leases before capturing the grant and time;
/// the existing keeper retains contention and failed starts without another app launch.
#[cfg(target_os = "macos")]
fn finish_withheld_preferences() -> withheld_preferences::Attempted {
    publish_withheld_attempt(
        finish_withheld_preferences_with(
            aterm_cli::macos_doctor::try_finish_withheld_preferences_unless_stopped,
        ),
        |report, observation| {
            remember_initial_setup_rows(report, None, observation);
            aterm_gui::publish_initial_setup_status(initial_setup_status());
        },
        restore_initial_setup_error,
    )
}

#[cfg(target_os = "macos")]
fn publish_withheld_attempt<S>(
    finished: aterm_cli::macos_doctor::WithheldFollowUp<S>,
    restored: impl FnOnce(
        &aterm_cli::macos_doctor::Report,
        Option<aterm_cli::macos_doctor::InitialSetupObservation>,
    ),
    failed: impl FnOnce(String, Option<aterm_cli::macos_doctor::InitialSetupObservation>),
) -> withheld_preferences::Attempted {
    use aterm_cli::macos_doctor::{FollowedUp, WithheldFollowUp};
    use aterm_secrets::converge::Last;
    use withheld_preferences::Attempted;
    let WithheldFollowUp::Finished(finished) = finished else {
        return Attempted::plain(Last::Held);
    };
    let FollowedUp {
        observed,
        reservation,
        put_off_for,
    } = *finished;
    let attempted = match &observed.result {
        Ok(Some(report)) if report.needs_attention() => Attempted::plain(Last::Unfinished),
        // Only changes put off for their own pause are left (`DEFERRED`): the request stays
        // unfinished, and its keeper tries again once the first of them comes due.
        Ok(Some(report)) if !report.complete() => Attempted {
            last: Last::Unfinished,
            put_off_for,
        },
        Ok(_) => Attempted::plain(Last::Ended),
        Err(_) => Attempted::plain(Last::Failed),
    };
    report_withheld_preferences((observed, reservation), restored, failed);
    attempted
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
    follow_up: impl FnOnce(fn() -> Option<WithheldReservation>, fn(u64) -> bool) -> T,
) -> T {
    follow_up(
        || {
            let busy = take_initial_setup_slot()?;
            // Tuple fields drop in order: retire and refresh the widget reading
            // before another operation can take the setup slot.
            Some((
                PreferenceReadings::begin(vec![aterm_types::macos_setup::Subject::Setting(
                    aterm_types::macos_setup::Setting::Widgets,
                )]),
                busy,
            ))
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
    // This process runs the run's passes now: the login job stands down to it until the worker
    // returns, and takes up a run reopened after that.
    let _window = hold_setup_window();
    with_setup_watch(
        true,
        || {
            with_setup_run(revision, || {
                supervise_setup_retry(
                    &mut LiveSetupRetry::new(revision, password, SetupOwner::Window),
                    claim,
                    busy,
                    revision,
                    restart_setup_worker,
                );
            })
        },
        maintain_setup_watch,
    );
}

/// The login job's worker of admitted run `revision` ([`setup_resume`]): the run's own loop,
/// owned by the job ([`SetupOwner::Job`]), until the run finishes, a newer one replaces it, a
/// window that runs setup opens, or an update replaced the job's build. It holds no typed
/// password and keeps no ALab watch. Whether it ran a pass.
#[cfg(target_os = "macos")]
fn run_setup_job(revision: u64) -> bool {
    let busy = wait_for_initial_setup_slot();
    with_setup_run(revision, || {
        let mut host = LiveSetupRetry::new(revision, None, SetupOwner::Job);
        supervise_setup_retry(&mut host, None, busy, revision, restart_setup_worker);
        host.passes > 0
    })
}

/// Wait `pause`, or less on a setup wake, as the login job does between its looks.
#[cfg(target_os = "macos")]
fn wait_for_setup_wake(pause: std::time::Duration) {
    let mut seen = SETUP_WAKES.count();
    SETUP_WAKES.back_off(&mut seen, pause);
}

/// Note that this window runs setup while the returned hold lives
/// (`aterm_cli::macos_doctor::hold_initial_setup_window`), so the login job runs no pass
/// meanwhile. A hold that cannot be taken leaves the claim to keep their passes apart.
#[cfg(target_os = "macos")]
fn hold_setup_window() -> Option<aterm_cli::macos_doctor::SetupWindowHold> {
    aterm_cli::macos_doctor::hold_initial_setup_window()
        .inspect_err(|why| {
            aterm_log::info!("first-run setup: the login job cannot see this window: {why}");
        })
        .ok()
}

/// Keep the login job with the run before a window's pass
/// (`aterm_cli::macos_doctor::keep_initial_setup_resume`), so a quit, a crash or a logout of this
/// window leaves the run to the job: one journal line each time what launchd says of it changes.
#[cfg(target_os = "macos")]
fn keep_setup_resume(said: &mut Option<String>) {
    use aterm_cli::macos_doctor::SetupResume;
    let line = match aterm_cli::macos_doctor::keep_initial_setup_resume() {
        Ok(SetupResume::Ended) => "first-run setup: no run owes a pass; no login job".to_owned(),
        Ok(SetupResume::Running) => "first-run setup: launchd runs the login job".to_owned(),
        Ok(SetupResume::Started) => {
            "first-run setup: the login job was handed to launchd to start again".to_owned()
        }
        Ok(SetupResume::Loaded) => {
            "first-run setup: the login job was handed to launchd".to_owned()
        }
        Ok(SetupResume::Renewed) => "first-run setup: the login job ran a build an update \
                                      replaced; it was handed to launchd to start again on this one"
            .to_owned(),
        Err(why) => format!(
            "first-run setup: the login job is not kept ({why}); a run this window leaves waits \
             for the next window"
        ),
    };
    if said.as_deref() != Some(line.as_str()) {
        aterm_log::info!("{line}");
        *said = Some(line);
    }
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
    /// gain): after a pass that had nothing to do, or before one the slot, the claim or a read
    /// kept from running. `true` when a person pressed Check again or the pending preferences'
    /// console returned meanwhile: the run's pace starts afresh ([`Self::settled`]).
    fn backoff(&mut self) -> bool;
    /// A pass left a step to try again by itself ([`PassNext::Retry`]): one more such pass in a
    /// row of this run, kept on disk with its time (`aterm_cli::macos_doctor::PassRetry`), so the
    /// next [`Self::park`] ends by itself at the end of the pause that follows it, a minute after
    /// the first and longer after each pause that ran out, up to an hour (a pass a wake brought
    /// early starts the same pause again), and a relaunch or a reboot goes on from it.
    fn retried(&mut self);
    /// A pass left nothing to try again by itself, or a wait ended in a way that asks for a pass
    /// now (a person's Check again, a fact the run waits on, the console's return): the run's
    /// pace starts afresh.
    fn settled(&mut self);
    /// Wait until something the run waits on may have moved: a person's grant, Check again, a
    /// new plan, Stop, System State coming to the front, a vault approval, a local fact the
    /// park is marked on, a try that came due (a step's own pause, a parked stage's, or the
    /// run's own pause after a pass that left a step to retry, [`Self::retried`]), or, while an
    /// Accounts or Permissions stage is left, a window gaining focus. No pass runs meanwhile,
    /// however long that takes. `true` when what ended it asks for a pass now: a fact the park
    /// is marked on changed, or a person pressed Check again ([`Self::settled`]).
    fn park(&mut self) -> bool;
    /// Whether this run's worker goes on by itself: `true` from its first pass for as long as it
    /// runs, whatever a pass of it leaves, since every wait it makes ends by itself or on a
    /// wake; `false` once it leaves ([`StandDown`]). While it is `true` a failure is the page's
    /// alone, calm, never the menu line's or the tab's attention (System State design §3.5,
    /// §3.6).
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
    /// Something changes by itself (a service starting, the network): try again, after the
    /// run's own pause ([`SetupRetryHost::retried`]), however many passes in a row it takes.
    Retry,
    /// What is left waits on a person, or on a pause of its own: no pass changes it until one
    /// of them moves or comes due.
    Park,
}

#[cfg(any(target_os = "macos", test))]
fn release_setup_pass<C, B>(claim: &mut Option<C>, busy: &mut Option<B>) {
    drop(claim.take());
    drop(busy.take());
}

/// Give the slot and the claim back and wait before trying a pass again: what kept it from
/// running (a claim another process holds, a failing read) may last, and the run goes on by
/// itself meanwhile, so its failure stays the page's (System State design §3.6). A person's
/// Check again or a useful console return meanwhile starts the run's pace afresh.
#[cfg(any(target_os = "macos", test))]
fn back_off_before_a_pass<H: SetupRetryHost>(
    host: &mut H,
    claim: &mut Option<H::Claim>,
    busy: &mut Option<H::Busy>,
) {
    release_setup_pass(claim, busy);
    if host.backoff() {
        host.settled();
    }
}

/// The retry loop's host, which says the run no longer goes on by itself once
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
///
/// It never gives up on the run: a pass that leaves a step to retry is followed by a park that
/// ends by itself on the run's own growing pause, kept on disk ([`SetupRetryHost::retried`]),
/// however many passes in a row that takes; a pass that leaves only what a person or a step's
/// own pause moves parks on those. No count turns a retry into a hold only a press lifts.
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
    host.retrying(true);
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
        let fresh = match next {
            PassNext::Retry => {
                host.retried();
                host.park()
            }
            PassNext::Park => {
                host.settled();
                host.park()
            }
            PassNext::Done => {
                host.settled();
                host.backoff()
            }
        };
        if fresh {
            host.settled();
        }
    }
}

/// [`run_setup_retry`], started again whenever a pass panics: the unwind gives back the slot and
/// the claim and says the run no longer retries ([`StandDown`]), and the ledger counts the stage
/// it cut off as a crash (`aterm_cli::macos_doctor` `ledger::Begun`), so a stage that keeps
/// panicking parks on its own pause rather than running again at once. The panicked pass counts
/// on the run's durable pace as one more that left a step to retry ([`SetupRetryHost::retried`]),
/// so the pause grows with each panic in a row, starts afresh once a pass settles, and a relaunch
/// reads the same deadline; `restart` waits it out and takes the slot again, and the loop then
/// starts with no claim, as a worker that waited for one does. The worker ends only as the loop
/// does: the run finished, or a newer one replaced it.
#[cfg(any(target_os = "macos", test))]
fn supervise_setup_retry<H: SetupRetryHost>(
    host: &mut H,
    claim: Option<H::Claim>,
    busy: H::Busy,
    revision: u64,
    mut restart: impl FnMut(&mut H) -> H::Busy,
) {
    let mut claim = claim;
    let mut busy = busy;
    loop {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_setup_retry(&mut *host, claim, busy, revision);
        }));
        if ran.is_ok() {
            return;
        }
        host.retried();
        claim = None;
        busy = restart(host);
    }
}

/// The live [`supervise_setup_retry`] restart: one journal line, the run's own pause as its
/// durable pace now says (`aterm_cli::macos_doctor::PassRetry`), or less on a wake, then the
/// slot.
#[cfg(target_os = "macos")]
fn restart_setup_worker(host: &mut LiveSetupRetry) -> InitialSetupBusy {
    let now = unix_seconds();
    let pause = std::time::Duration::from_secs(
        host.pace
            .pending(now)
            .map_or(0, |due| due.saturating_sub(now)),
    );
    aterm_log::info!(
        "first-run setup: a pass of run {} panicked; its worker starts again in {}s",
        host.revision,
        pause.as_secs()
    );
    let mut seen = SETUP_WAKES.count();
    SETUP_WAKES.back_off(&mut seen, pause);
    host.busy()
}

/// How long a parked run waits before it reads its mark again by itself: until its next retry
/// falls due (`next_due`, seconds since the epoch, against `now`), or with no timer at all when
/// none is ahead. A run whose facts are not all `watched` (a watch that could not start) reads
/// them at least once a minute, as before watches. First-use door conditions are
/// process-local, so a waiting door is read each second even with healthy file watches.
#[cfg(any(target_os = "macos", test))]
fn park_wait_limit(
    next_due: Option<u64>,
    now: u64,
    watched: bool,
    door_waiting: bool,
) -> Option<std::time::Duration> {
    const UNWATCHED: std::time::Duration = std::time::Duration::from_secs(60);
    const DOOR_LOOK: std::time::Duration = std::time::Duration::from_secs(1);
    let due = next_due.map(|due| std::time::Duration::from_secs(due.saturating_sub(now).max(1)));
    let normal = if watched {
        due
    } else {
        Some(due.map_or(UNWATCHED, |due| due.min(UNWATCHED)))
    };
    if door_waiting {
        Some(normal.map_or(DOOR_LOOK, |due| due.min(DOOR_LOOK)))
    } else {
        normal
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

/// The last due deadline a parked worker woke for, per source of its tries: the Secrets
/// vault's, the Mac settings', and a parked stage's (`InitialSetupParkMark::retry_due`,
/// `mac_settings_retry_due`, `stage_retry_due`, in that order).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Default)]
struct SetupRetryDeadlines([Option<u64>; 3]);

#[cfg(any(target_os = "macos", test))]
impl SetupRetryDeadlines {
    /// Whether a source has a due deadline this worker has not woken for.
    fn unseen(self, dues: [Option<u64>; 3]) -> bool {
        dues.iter()
            .zip(self.0)
            .any(|(due, seen)| due.is_some_and(|due| Some(due) != seen))
    }

    /// The worker woke for `dues`: each is spent, even should it be absent for a while.
    fn consume(&mut self, dues: [Option<u64>; 3]) {
        for (seen, due) in self.0.iter_mut().zip(dues) {
            if due.is_some() {
                *seen = due;
            }
        }
    }
}

/// What times a parked worker by itself: each source's due deadline it last woke for
/// ([`SetupRetryDeadlines`]) and the end of the run's own pace after a pass that left a step to
/// retry (`pace_due`, seconds since the epoch; `aterm_cli::macos_doctor::PassRetry::pending`).
/// One rule for a window's worker and the login job's alike.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Default)]
struct ParkClock {
    due_seen: SetupRetryDeadlines,
    pace_due: Option<u64>,
}

#[cfg(any(target_os = "macos", test))]
impl ParkClock {
    /// The deadline the park's timer runs to: the mark's next one still ahead (`next_due`) or
    /// the pace's end, whichever comes first; none with neither, and the park then waits for a
    /// wake alone.
    fn next(self, next_due: Option<u64>) -> Option<u64> {
        next_due.into_iter().chain(self.pace_due).min()
    }

    /// Whether a reading of the mark at `now`, whose sources' due deadlines are `dues`, asks for
    /// a pass now: one of them is due and the worker has not woken for it, or the pace ended.
    fn ready(self, dues: [Option<u64>; 3], now: u64) -> bool {
        self.due_seen.unseen(dues) || self.pace_due.is_some_and(|due| now >= due)
    }
}

/// A park's due deadlines, one per source ([`SetupRetryDeadlines`]).
#[cfg(target_os = "macos")]
fn park_dues(mark: &aterm_cli::macos_doctor::InitialSetupParkMark) -> [Option<u64>; 3] {
    [
        mark.retry_due(),
        mark.mac_settings_retry_due(),
        mark.stage_retry_due(),
    ]
}

/// Whose worker a run's loop is.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupOwner {
    /// A window's, or a terminal session's: every stage of the plan, a person's among them.
    Window,
    /// The login job's ([`setup_resume`]), with no window and no person: the stages that run
    /// unattended alone (`aterm_cli::macos_doctor::run_background_setup_pass`); it stands down
    /// once a window runs setup.
    Job,
}

#[cfg(target_os = "macos")]
impl SetupOwner {
    /// Whether this worker gives way: the login job's, once a window runs setup or an update
    /// replaced the job's build (`setup_resume::replaced`), which launchd then starts again on
    /// the new one. A lock that cannot be read gives way to nothing, as the claim keeps their
    /// passes apart.
    fn gives_way(self) -> bool {
        self == Self::Job && job_gives_way()
    }
}

#[cfg(target_os = "macos")]
struct LiveSetupRetry {
    owner: SetupOwner,
    revision: u64,
    password: Option<aterm_secrets::SecretBytes>,
    last_failure: Option<String>,
    success_reported: bool,
    /// The last wakes this worker answered: set when a pass begins, so a wake
    /// during it is not lost, and by each backoff that a wake ended.
    wakes_seen: WakeMarks,
    /// The run's own pace between passes that left a step to retry, as on disk
    /// (`aterm_cli::macos_doctor::PassRetry`): read when the worker starts, so a relaunch
    /// goes on from it.
    pace: aterm_cli::macos_doctor::PassRetry,
    /// Each source's last consumed deadline: an unchanged due attempt cannot
    /// wake every park, nor hide a newly due attempt from another source.
    park_due_seen: SetupRetryDeadlines,
    /// Local facts before the last pass: a change it refused may already be
    /// stable by the time its worker reaches the park's initial reading.
    park_before_pass: Option<aterm_cli::macos_doctor::InitialSetupParkMark>,
    /// The last accounts resync line this worker logged (`accounts_resynced`).
    accounts_said: Option<String>,
    /// The last line a window's worker logged of the login job ([`keep_setup_resume`]).
    resume_said: Option<String>,
    /// The passes this worker ran ([`run_setup_job`] says whether it ran one).
    passes: u32,
}

#[cfg(target_os = "macos")]
impl SetupRetryHost for LiveSetupRetry {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;

    fn revision(&mut self) -> Result<u64, String> {
        aterm_cli::macos_doctor::initial_setup_revision()
    }
    /// The run finished, or (the login job's worker) a window runs setup now: either way this
    /// worker has nothing more to do.
    fn finished(&mut self) -> Result<bool, String> {
        Ok(aterm_cli::macos_doctor::initial_setup_finished()? || self.owner.gives_way())
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
        self.passes = self.passes.saturating_add(1);
        let pass = match self.owner {
            SetupOwner::Window => self.window_pass()?,
            SetupOwner::Job => self.job_pass()?,
        };
        let Some((report, next)) = pass else {
            return Ok(PassNext::Done);
        };
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
        if self.owner == SetupOwner::Window {
            maintain_setup_watch(WatchMaintenance::Spawn);
        }
        // A wake (Check again, Stop, a new plan, a person's grant) ends it early,
        // once: a read or a claim that keeps failing waits a minute each time. A
        // window gaining focus does not: it is no reason to try again sooner.
        let seen = self.wakes_seen;
        SETUP_WAKES.back_off_run(&mut self.wakes_seen, std::time::Duration::from_secs(60));
        self.wakes_seen.refreshed_since(seen)
    }
    fn retried(&mut self) {
        self.pace = self.pace.after_another(unix_seconds());
        self.keep_pace();
    }
    fn settled(&mut self) {
        if self.pace != aterm_cli::macos_doctor::PassRetry::default() {
            self.pace = aterm_cli::macos_doctor::PassRetry::default();
            self.keep_pace();
        }
    }
    fn park(&mut self) -> bool {
        // Local vault facts, interfaces for pending Secrets or Repositories, the
        // selected Git build and existing retry deadlines can change without the
        // window. Each is watched (`ParkWatch`), so the park reads its mark again
        // only when one may have moved, a retry falls due or a wake comes: a run
        // left waiting on a person alone has no timer. A window gaining focus ends
        // the park only while it waits on what a person does in System Settings or
        // macOS (an Accounts or Permissions stage left). After a pass that left a step to
        // retry, the run's own pause ends it too.
        use aterm_cli::macos_doctor::{ParkWatch, Watches};
        let owner = self.owner;
        let seen = self.wakes_seen;
        let clock = ParkClock {
            due_seen: self.park_due_seen,
            pace_due: self.pace.pending(unix_seconds()),
        };
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
                    || {
                        if owner == SetupOwner::Window {
                            maintain_setup_watch(WatchMaintenance::Spawn);
                        }
                    },
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
                // A watch that stopped hearing its fact gives the park its timer back. The login
                // job's park reads each minute whatever it watches, as a window that opens ends it.
                let watched = owner == SetupOwner::Window
                    && watching.as_ref().is_some_and(|(on, started)| {
                        on.none() || started.as_ref().is_some_and(ParkWatch::whole)
                    })
                    && !setup_watch_owed();
                limit.set(park_wait_limit(
                    clock.next(mark.next_due()),
                    unix_seconds(),
                    watched,
                    mark.waits_on_lan_broker(),
                ));
                previous = Some(mark.clone());
                mark
            },
            |mark| clock.ready(park_dues(mark), unix_seconds()),
            aterm_cli::macos_doctor::InitialSetupParkMark::facts_changed_since,
            || !SETUP_WAKES.marks().ends_run_wait(seen, focus.get()) && !owner.gives_way(),
            || {
                SETUP_WAKES.wait_for_park(seen, facts.get(), focus.get(), limit.get());
            },
        );
        self.park_due_seen.consume(park_dues(&mark));
        // A fact the park is marked on, a useful console return, or a person's Check again
        // starts the run's pace afresh; a due try or any other wake does not.
        before_pass
            .as_ref()
            .is_some_and(|before| mark.facts_changed_since(before))
            || SETUP_WAKES.marks().refreshed_since(seen)
    }
    fn retrying(&mut self, retrying: bool) {
        set_setup_retrying(self.revision, retrying);
    }
    fn failure(&mut self, why: &str) {
        report_first_run_doctor_failure(&mut self.last_failure, why);
    }
    fn completed(&mut self) {
        // The login job's worker leaves the vault's mend and the converge lane to the next
        // window, whose launch starts both.
        if self.owner == SetupOwner::Job {
            return;
        }
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
    /// `owner`'s worker of run `revision`, going on from the pace the run's last worker kept.
    fn new(revision: u64, password: Option<aterm_secrets::SecretBytes>, owner: SetupOwner) -> Self {
        Self {
            owner,
            revision,
            password,
            last_failure: None,
            success_reported: false,
            wakes_seen: SETUP_WAKES.marks(),
            pace: aterm_cli::macos_doctor::initial_setup_pass_retry(
                aterm_cli::macos_doctor::PassWorker::Run,
                revision,
            ),
            park_due_seen: SetupRetryDeadlines::default(),
            park_before_pass: None,
            accounts_said: None,
            resume_said: None,
            passes: 0,
        }
    }

    /// A window's pass: every stage, its rows and accounts for the page.
    fn window_pass(
        &mut self,
    ) -> Result<
        Option<(
            aterm_cli::macos_doctor::Report,
            aterm_cli::macos_doctor::SetupNext,
        )>,
        String,
    > {
        keep_setup_resume(&mut self.resume_said);
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
            return Ok(None);
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
        Ok(Some((report, next)))
    }

    /// The login job's pass: the stages that run unattended, with no page to read its rows and
    /// nobody to stop it but a Stop saved to the records, which the pass reads between stages.
    fn job_pass(
        &mut self,
    ) -> Result<
        Option<(
            aterm_cli::macos_doctor::Report,
            aterm_cli::macos_doctor::SetupNext,
        )>,
        String,
    > {
        self.wakes_seen = SETUP_WAKES.marks();
        let revision = self.revision;
        with_setup_pass_mark(
            &mut self.park_before_pass,
            aterm_cli::macos_doctor::initial_setup_park_mark,
            || aterm_cli::macos_doctor::run_background_setup_pass(revision, JOB_HOOKS),
        )
        .result
    }

    /// Keep the run's pace on disk, for the next worker of this run: a record that cannot be
    /// written leaves this worker's own pace as it is, and the next goes from the last it read.
    fn keep_pace(&self) {
        if let Err(why) = aterm_cli::macos_doctor::note_initial_setup_pass_retry(
            aterm_cli::macos_doctor::PassWorker::Run,
            self.revision,
            self.pace,
        ) {
            aterm_log::info!("setup: the retry pause is not kept on disk: {why}");
        }
    }

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
#[path = "main_alab_watch_tests.rs"]
mod alab_watch_tests;

#[cfg(test)]
#[path = "main_setup_retry_tests.rs"]
mod setup_retry_tests;

#[cfg(target_os = "macos")]
fn report_first_run_doctor_failure(last_failure: &mut Option<String>, text: &str) {
    if last_failure.as_deref() == Some(text) {
        return;
    }
    aterm_log::warn!("first-run macOS doctor: {text}; automatic setup tries it again by itself");
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
    wait_for_setup_slot(InitialSetupBusy::new)
}

#[cfg(target_os = "macos")]
fn wait_for_setup_slot<T>(acquired: impl FnOnce() -> T) -> T {
    while !take_initial_setup_slot_now() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    acquired()
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
    /// The settings whose stages run now: a set, since lanes run stages at once (the
    /// Secrets lane beside Workstation, design §6.2).
    settings: Vec<aterm_types::macos_setup::Setting>,
    /// The rows those stages can change.
    subjects: Vec<aterm_types::macos_setup::Subject>,
    /// The epoch of the last stage event that could change each row.
    touched: Vec<(aterm_types::macos_setup::Subject, u64)>,
    /// The workstation stage that owns the current native readback. This is a
    /// publication fence, not a saved readiness or permission receipt.
    readback: Option<SetupReadback>,
    /// Rows read by this owned pass, so its final refresh need not repeat them.
    readback_subjects: Vec<aterm_types::macos_setup::Subject>,
    /// Captured before this job reads its execution target, never rebased by START.
    readback_owner: Option<SetupReadbackScope>,
    /// The one keeper's actual activation/read-failure batch. It has separate
    /// custody from a setup stage that may still be finishing its own lease.
    input_readback: bool,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct SetupReadbackScope {
    revision: Option<u64>,
    choices: Option<u64>,
}

#[cfg(target_os = "macos")]
struct SetupReadback {
    revision: u64,
    choices: u64,
    started: u64,
    subjects: Vec<aterm_types::macos_setup::Subject>,
    history_ticket: Option<SetupHistoryTicket>,
}

#[cfg(target_os = "macos")]
impl SetupActivity {
    /// A stage started (`running`) or ended: its settings and rows join the running set, or
    /// leave it, and the others' stay.
    fn mark(
        &mut self,
        settings: &[aterm_types::macos_setup::Setting],
        subjects: &[aterm_types::macos_setup::Subject],
        running: bool,
    ) {
        self.touch(subjects);
        if running {
            for setting in settings {
                if !self.settings.contains(setting) {
                    self.settings.push(*setting);
                }
            }
            for subject in subjects {
                if !self.subjects.contains(subject) {
                    self.subjects.push(*subject);
                }
            }
        } else {
            self.settings.retain(|setting| !settings.contains(setting));
            self.subjects.retain(|subject| !subjects.contains(subject));
        }
    }

    /// Retire in-flight readings without changing which stages own their rows.
    fn touch(&mut self, subjects: &[aterm_types::macos_setup::Subject]) {
        self.epoch += 1;
        self.readback_subjects
            .retain(|subject| !subjects.contains(subject));
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
    readback: None,
    readback_subjects: Vec::new(),
    readback_owner: None,
    input_readback: false,
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
    /// from `all`, so a park or local membership wait ends on it, but a retry backoff does not.
    facts: u64,
    /// A useful native console return for a current pending settings run. It restarts only
    /// the retry budget; it is never a person's Check or a ledger-hold override.
    inputs: u64,
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

    /// Local membership inputs can change without a network or window event. They wake the
    /// profile's local reread as well as the ordinary parked setup, granting no authority.
    fn ends_alab_wait(self, seen: Self) -> bool {
        self.facts != seen.facts || self.ends_run_wait(seen, false)
    }

    /// Whether a person pressed Check again since `seen`: the run's tries start afresh.
    fn checked_since(self, seen: Self) -> bool {
        self.checks != seen.checks
    }

    fn refreshed_since(self, seen: Self) -> bool {
        self.checked_since(seen) || self.inputs != seen.inputs
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
                inputs: 0,
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

    fn raise_input(&self) {
        self.raise_with(|marks| marks.inputs = marks.inputs.wrapping_add(1));
    }

    /// A fact a parked run watches may have moved (`aterm_cli::macos_doctor::ParkWatch`): the
    /// park and the membership watcher reread their local facts. No retry backoff ends.
    fn raise_facts(&self) {
        let mut marks = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks.facts = marks.facts.wrapping_add(1);
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
    /// wake that is a reason to reread local membership ([`WakeMarks::ends_alab_wait`]).
    fn back_off_alab(&self, seen: &mut WakeMarks, limit: std::time::Duration) -> bool {
        let from = *seen;
        let woken = self.wait_while(Some(limit), |marks| !marks.ends_alab_wait(from));
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

/// The choice number of the automatic admission of the groups this Mac's ALab signal adds
/// (`LiveAlabWatch::review`, answered as offered with nobody's press), or 0: while
/// [`SETUP_QUEUED`] holds it, what waits is aterm's own, and the page says it waits to add this
/// Mac's ALab profile (`SetupStatus::queued_widen`), never the person's choice.
#[cfg(target_os = "macos")]
static SETUP_QUEUED_WIDEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    gives_way: || false,
};

/// The login job's pass hooks: no page reads its stages, and it gives way at each stage boundary
/// once a window runs setup or an update replaced its build ([`job_gives_way`]), ending the pass
/// with what it left to try again, so the window's pass goes on from there with no wait for the
/// job's whole pass.
#[cfg(target_os = "macos")]
const JOB_HOOKS: aterm_cli::macos_doctor::SetupHooks = aterm_cli::macos_doctor::SetupHooks {
    gives_way: job_gives_way,
    ..aterm_cli::macos_doctor::SetupHooks::NONE
};

/// Whether the login job gives way: a window runs setup now, or an update replaced the job's
/// build. A window lock that cannot be read gives way to nothing, as the claim keeps their passes
/// apart.
#[cfg(target_os = "macos")]
fn job_gives_way() -> bool {
    setup_resume::replaced()
        || aterm_cli::macos_doctor::initial_setup_window_open().unwrap_or(false)
}

/// A private row's reading kept for the next launch, with the journal as its read started
/// ([`SetupIo::remember`]).
#[cfg(target_os = "macos")]
type KeptReading = (
    aterm_types::macos_setup::Observation,
    aterm_cli::macos_doctor::Fence,
);

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
    /// Optional display-only storage, resolved by the reading worker or before a restored
    /// System State page is constructed at launch, never on the running GUI loop.
    history_store: fn() -> Option<aterm_cli::macos_doctor::SetupHistoryStore>,
    /// Read rows for a plan under their current observation admission, the network's too when
    /// asked (`aterm_cli::macos_doctor::observe_initial_setup`). Plan mode alone permits no
    /// hidden query of another app's data.
    read: fn(
        &aterm_types::macos_setup::Plan,
        &[aterm_types::macos_setup::Subject],
        bool,
        aterm_cli::macos_doctor::ObservationRead,
    ) -> Vec<aterm_types::macos_setup::Observation>,
    /// The wall clock a reading is stamped with as it starts, in Unix seconds
    /// (`aterm_time::unix_secs`; 0 where it cannot be read, which dates the reading as old as
    /// can be, never as new).
    now_secs: fn() -> u64,
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
    /// Retain a follow-up request for the existing keeper, as the window records the grant.
    start_follow_up: fn(),
    /// Bring forward a setup a Restore left with no admitted run to carry aterm's own
    /// permissions (`aterm_cli::macos_doctor::bring_forward_restored_setup`): a run that waits
    /// for the startup's offer, nothing, or a profile lock another pass holds.
    bring_forward: fn() -> Result<aterm_cli::macos_doctor::BroughtForward, String>,
    /// Check again's look for the ALab vault, on the reading's worker
    /// (`aterm_cli::macos_doctor::look_for_the_vault_again`).
    look_again: fn(),
    /// A person's Check again lifts every stage the run's ledger parked and starts the run's
    /// other tries afresh (`aterm_cli::macos_doctor::retry_parked_setup_stages`); nothing
    /// automatic calls it.
    lift_parks: fn() -> Result<(), String>,
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
    /// One coherent current, admitted and unheld unfinished Repositories scope.
    pending_repositories:
        fn() -> Result<Option<aterm_cli::macos_doctor::InitialSetupAdmission>, String>,
    /// Keep this process's latest readings of the rows read privately, those read back as
    /// wanted, each with the journal as its read started, for the next launch
    /// (`aterm_cli::macos_doctor::remember_private_read_back`, which keeps only those its fence
    /// still holds for).
    remember: fn(&[KeptReading]) -> Result<(), String>,
    /// What a previous launch kept of them, each with its fence, where it still holds: the same
    /// run revision, and no change journaled since its read started
    /// (`aterm_cli::macos_doctor::remembered_private_read_back`).
    remembered: fn() -> Vec<KeptReading>,
    /// The change journal as a read starts, which fences what it reads for the next launch
    /// (`aterm_cli::macos_doctor::journal_fence`); `None` where it cannot be read.
    fence: fn() -> Option<aterm_cli::macos_doctor::Fence>,
}

#[cfg(target_os = "macos")]
impl SetupIo {
    const LIVE: Self = Self {
        scope_refused: || aterm_cli::macos_doctor::lasting_scope_refusal().is_some(),
        history_store: aterm_cli::macos_doctor::SetupHistoryStore::for_person,
        read: aterm_cli::macos_doctor::observe_initial_setup,
        now_secs: || aterm_time::unix_secs().unwrap_or(0),
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
        look_again: aterm_cli::macos_doctor::look_for_the_vault_again,
        lift_parks: aterm_cli::macos_doctor::retry_parked_setup_stages,
        start_startup: start_startup_again,
        start_startup_inline: || run_startup_setup(None),
        records_held: aterm_cli::macos_doctor::setup_records_held,
        vault_mark: || {
            aterm_secrets::client::Paths::live()
                .ok()
                .and_then(|paths| aterm_secrets::client::materialization_mark(&paths).ok())
                .flatten()
        },
        pending_repositories: aterm_cli::macos_doctor::initial_setup_pending_repositories,
        remember: aterm_cli::macos_doctor::remember_private_read_back,
        remembered: aterm_cli::macos_doctor::remembered_private_read_back,
        fence: aterm_cli::macos_doctor::journal_fence,
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
    if let Err(why) = (setup_io().lift_parks)() {
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

/// A native console return can make an unheld Workstation stage runnable. Recheck its exact
/// current revision and the process's immediate Stop; the eventual pass owns every effect.
#[cfg(target_os = "macos")]
fn wake_setup_for_console() -> Result<(), String> {
    wake_setup_for_console_with(
        aterm_cli::macos_doctor::initial_setup_pending_workstation_revision,
        setup_stop_requested,
        || SETUP_WAKES.raise_input(),
    )
}

#[cfg(any(target_os = "macos", test))]
fn wake_setup_for_console_with(
    current: impl FnOnce() -> Result<Option<u64>, String>,
    stopped: impl FnOnce(u64) -> bool,
    wake: impl FnOnce(),
) -> Result<(), String> {
    if let Some(revision) = current()?
        && !stopped(revision)
    {
        wake();
    }
    Ok(())
}

/// What the setup worker hears from the window, registered once before the GUI
/// starts: a run parked on System State (the Accounts stage) resumes when the
/// page comes to the front, and an access step the window recorded (handed,
/// declined, triggered, granted) moves the Access rows and a run parked on the
/// Permissions stage at once (System State design §6). A caller that timed out
/// behind another dialog also retries once its last guard ends. A window gaining
/// focus wakes the startup's waits, and a run's park only while an Accounts or
/// Permissions stage is left. The LAN broker starting to answer (setup's elevation installed
/// it, or a repair or an upgrade restarted it) wakes them too
/// ([`aterm_machines::lan_reach::watch_arrivals`]): a vault look or join deferred until it
/// answers goes on then, waiting on the broker, never on a person.
#[cfg(target_os = "macos")]
fn watch_setup_signals() {
    aterm_gui::on_converge_pass_ended(observe_pending_repositories);
    aterm_gui::on_window_focus_gained(wake_setup_on_focus);
    aterm_types::macos_setup::on_system_state_front(wake_setup);
    aterm_types::macos_setup::access_record::on_append(access_step_recorded);
    aterm_types::macos_setup::on_macos_dialog_waiter_ready(wake_setup);
    if let Err(why) = std::thread::Builder::new()
        .name("lan-broker-arrivals".into())
        .spawn(|| aterm_machines::lan_reach::watch_arrivals(wake_setup))
    {
        aterm_log::info!("setup: no watch for the LAN broker's arrival: {why}");
    }
}

/// The window wrote a line to the access record (System State design §6): a step it handed to
/// macOS, a Not now, a grant it verified or the debugger's outcome.
/// The Access rows are read again so the page and the menu line follow it, and a run parked on
/// the Permissions stage looks at the record again.
#[cfg(target_os = "macos")]
fn access_step_recorded(
    _revision: u64,
    access: aterm_types::macos_setup::Access,
    outcome: aterm_types::macos_setup::AccessOutcome,
) {
    access_fact_observed(access, outcome);
}

/// Apple's Command Line Tools are installed, as System State's Mac Setup tab said or the
/// window found them ([`aterm_gui::InitialSetupHandlers::tools_installed`]): the GitHub
/// repositories row is read again at once, and a run parked on them wakes to go on (System State
/// design §3.3).
#[cfg(target_os = "macos")]
fn command_line_tools_installed() {
    use aterm_types::macos_setup::{Setting, Subject};
    request_observation(None, vec![Subject::Setting(Setting::Repositories)], false);
    wake_setup();
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

/// The window found a verified Full Disk Access grant reading off
/// ([`aterm_gui::InitialSetupHandlers::fda_off`]): a reading from before it says nothing now, so
/// the Access rows are read again at once, on the observer's worker (their reads need no page in
/// front), and `setup=` and the page say what they read rather than the grant the earlier reading
/// saw (System State design §3.5).
#[cfg(target_os = "macos")]
fn full_disk_access_reads_off() {
    use aterm_types::macos_setup::{Access, Subject};
    request_observation(None, Access::ALL.map(Subject::Access).to_vec(), false);
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

/// Retain the grant's follow-up intent before the existing keeper tries a finite worker.
#[cfg(target_os = "macos")]
fn start_withheld_follow_up() {
    withheld_preferences::request();
}

#[cfg(target_os = "macos")]
fn setup_stop_requested(revision: u64) -> bool {
    SETUP_STOP.lock().is_ok_and(|stop| *stop == Some(revision))
        || stop_saves().requested == Some(revision)
}

/// A stage of the running pass started or ended: its rows paint busy while it runs, what it
/// read of them is their reading as it ends ([`record_setup_stage`]), and the rows it read
/// nothing of are read again once it ends.
#[cfg(target_os = "macos")]
fn setup_stage(event: aterm_cli::macos_doctor::StageEvent<'_>) {
    let missing = record_setup_stage(event);
    aterm_gui::publish_initial_setup_status(initial_setup_status());
    if !event.running {
        remember_private_readings();
        request_observation(None, missing, false);
    }
    if ends_secrets_stage(&event) {
        (setup_io().converge_setup)();
    }
}

/// Record a stage's start or end. A start retires its rows' readings; an end takes each dated
/// reading the stage's own run made of one of its rows (`event.readback`) as that row's reading
/// now, while the run's revision is current and not stopped, the person's choice is the one its
/// run started under and nothing touched the row since the stage started, with the journal as
/// the stage ends ([`ObservedRows::fences`], what fences the reading for the next launch) and as
/// of its end ([`ObservedRows::read_epochs`]: a stage that could change the row starting after
/// it retires it). An undated one reads nothing. The rows of `event` it took no reading of.
#[cfg(target_os = "macos")]
fn record_setup_stage(
    event: aterm_cli::macos_doctor::StageEvent<'_>,
) -> Vec<aterm_types::macos_setup::Subject> {
    // A stage whose own run reads its rows natively (the Mac settings, the sign-outs, Spotlight).
    let native = event
        .settings
        .iter()
        .any(|setting| aterm_types::macos_setup::Subject::Setting(*setting).reads_privately());
    let current = !event.running
        && setup_run() == Some(event.revision)
        && !setup_stop_requested(event.revision);
    // The journal as the stage ends, after its own changes, read before either lock.
    let fence = (!event.running).then(|| (setup_io().fence)()).flatten();
    // The display history a native stage's read-back keeps ([`SetupReadback::history_ticket`]).
    let stage_history = if event.running && native {
        begin_setup_history(None, None)
    } else {
        None
    };
    let mut accepted = Vec::new();
    let mut history_ticket = None;
    let read_at = aterm_time::unix_millis().unwrap_or(0);
    if let Ok(mut activity) = SETUP_ACTIVITY.lock()
        && let Ok(mut rows) = OBSERVED_ROWS.lock()
    {
        if event.running {
            activity.mark(event.settings, event.subjects, true);
            retire_observed_rows(&mut rows, event.subjects);
            if native {
                activity.readback = activity.readback_owner.and_then(|owner| {
                    (owner.revision == Some(event.revision))
                        .then_some(owner.choices)
                        .flatten()
                        .map(|choices| SetupReadback {
                            revision: event.revision,
                            choices,
                            started: activity.epoch,
                            subjects: event.subjects.to_vec(),
                            history_ticket: stage_history.clone(),
                        })
                });
            }
        } else {
            let frame = native.then(|| activity.readback.take()).flatten();
            let readback: Vec<_> = event
                .readback
                .observations()
                .iter()
                .filter(|seen| {
                    current
                        && seen.read_at().is_some()
                        && event.subjects.contains(&seen.subject)
                        && !(activity.input_readback
                            && seen.subject
                                == aterm_types::macos_setup::Subject::Setting(
                                    aterm_types::macos_setup::Setting::CapsLock,
                                ))
                        && frame.as_ref().is_some_and(|frame| {
                            frame.revision == event.revision
                                && frame.choices == rows.choices
                                && frame.subjects.contains(&seen.subject)
                                && activity.touched.iter().all(|(subject, epoch)| {
                                    *subject != seen.subject || *epoch <= frame.started
                                })
                        })
                })
                .cloned()
                .collect();
            // Validate before our own end event advances the touched fence.
            activity.mark(event.settings, event.subjects, false);
            for seen in readback {
                let subject = seen.subject;
                accepted.push(subject);
                activity.readback_subjects.push(subject);
                if retain_completed_read(&mut rows, &seen, read_at) {
                    history_ticket = frame
                        .as_ref()
                        .and_then(|frame| frame.history_ticket.clone());
                }
                replace_observed_row(&mut rows, seen);
                rows.fence(subject, fence);
                match rows.read_epochs.iter_mut().find(|(row, _)| *row == subject) {
                    Some(kept) => kept.1 = activity.epoch,
                    None => rows.read_epochs.push((subject, activity.epoch)),
                }
            }
        }
    }
    persist_setup_history(history_ticket);
    event
        .subjects
        .iter()
        .copied()
        .filter(|subject| !accepted.contains(subject))
        .collect()
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

/// No stage runs now (a pass ended, possibly through a panic inside a stage).
#[cfg(target_os = "macos")]
fn end_setup_activity() {
    if let Ok(mut activity) = SETUP_ACTIVITY.lock()
        && (!activity.settings.is_empty() || !activity.subjects.is_empty())
    {
        // A panic missed the normal end hook: a stage that ends early with an
        // error still ends through it, but its fence calls no hook while its thread
        // unwinds. Retire readings taken during those stages just as the hook would,
        // before clearing their rows.
        let settings = std::mem::take(&mut activity.settings);
        let subjects = std::mem::take(&mut activity.subjects);
        activity.mark(&settings, &subjects, false);
        activity.readback = None;
    }
}

#[cfg(target_os = "macos")]
fn with_setup_activity<T>(pass: impl FnOnce() -> T) -> T {
    struct Activity;
    impl Drop for Activity {
        fn drop(&mut self) {
            // A panic may have missed the stage's normal end hook.
            end_setup_activity();
            let read = SETUP_ACTIVITY.lock().map_or_else(
                |_| Vec::new(),
                |mut activity| std::mem::take(&mut activity.readback_subjects),
            );
            // A complete owned readback already paid for these native reads. An
            // interrupted or invalidated stage contributes no such exemption.
            refresh_observation(
                aterm_types::macos_setup::Subject::all()
                    .into_iter()
                    .filter(|subject| !read.contains(subject))
                    .collect(),
            );
        }
    }
    let _readback = SetupReadbackOwner::begin(capture_setup_readback_scope(setup_run()));
    let _activity = Activity;
    pass()
}

#[cfg(target_os = "macos")]
fn capture_setup_readback_scope(revision: Option<u64>) -> SetupReadbackScope {
    SetupReadbackScope {
        revision,
        choices: OBSERVED_ROWS.lock().ok().map(|rows| rows.choices),
    }
}

#[cfg(target_os = "macos")]
struct SetupReadbackOwner(Option<SetupReadbackScope>);

#[cfg(target_os = "macos")]
impl SetupReadbackOwner {
    fn begin(scope: SetupReadbackScope) -> Self {
        Self(
            SETUP_ACTIVITY
                .lock()
                .ok()
                .and_then(|mut activity| activity.readback_owner.replace(scope)),
        )
    }
}

#[cfg(target_os = "macos")]
impl Drop for SetupReadbackOwner {
    fn drop(&mut self) {
        end_setup_activity();
        if let Ok(mut activity) = SETUP_ACTIVITY.lock() {
            activity.readback = None;
            activity.readback_owner = self.0;
            // What the pass read is spent with it: no later pass is spared a read by it.
            activity.readback_subjects.clear();
        }
        publish_setup_readings();
    }
}

/// One queued read of System State rows.
#[cfg(target_os = "macos")]
#[derive(Clone, PartialEq, Eq)]
struct ObserveRequest {
    /// The page's plan; `None` keeps the last one.
    plan: Option<aterm_types::macos_setup::Plan>,
    /// [`ObservedRows::choices`] when this request was made: a person's
    /// newer choice retires both the named plan and broader Check scope.
    choices: u64,
    subjects: Vec<aterm_types::macos_setup::Subject>,
    /// Check again: read GitHub repositories even while it is not managed.
    forced: bool,
    /// A child ending only requests a reading while its current Repo stage is pending.
    pending_repositories: bool,
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
    active: Vec<ObserveRequest>,
    current: Option<ObserveRequest>,
    /// Latest ordinary refresh, kept apart from a Check of another source.
    next: Option<ObserveRequest>,
    /// Latest explicit Check. Its scope never transfers to another plan.
    check: Option<ObserveRequest>,
    /// Workers whose thread could not start, in a row, and when the last of them failed
    /// (milliseconds on [`observation_millis`]'s clock): the next start waits on
    /// [`OBSERVE_AGAIN`] after them. Only a worker that never started owes a dispatch retry.
    misses: u32,
    missed_at: u64,
}

/// When a setup reader whose thread could not start is started again
/// ([`dispatch_observation_with`]): five seconds after the first failed start, doubling, never more
/// than five minutes apart. A launch reads afresh, so the schedule lives with the process.
#[cfg(target_os = "macos")]
const OBSERVE_AGAIN: aterm_types::retry_schedule::Backoff =
    aterm_types::retry_schedule::Backoff::new(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5 * 60),
    );

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
                for previous in [self.next.take(), self.check.take()].into_iter().flatten() {
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
            request.pending_repositories &= previous.pending_repositories;
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
        let batch = [self.next.take(), self.check.take()];
        self.active = batch.iter().flatten().cloned().collect();
        batch
    }

    fn progress(&self) -> Vec<(ObserveRequest, bool)> {
        self.current
            .iter()
            .map(|request| (request.clone(), false))
            .chain(
                self.active
                    .iter()
                    .chain(self.next.iter())
                    .chain(self.check.iter())
                    .map(|request| (request.clone(), true)),
            )
            .collect()
    }

    /// Whether a start that failed holds the next one at `now` (milliseconds on
    /// [`observation_millis`]'s clock), its pause on [`OBSERVE_AGAIN`] not yet passed.
    fn start_held(&self, now: u64) -> bool {
        let pause = u64::try_from(OBSERVE_AGAIN.after(self.misses).as_millis()).unwrap_or(u64::MAX);
        self.misses > 0 && now < self.missed_at.saturating_add(pause)
    }
}

#[cfg(target_os = "macos")]
static OBSERVE_QUEUE: std::sync::Mutex<ObserveQueue> = std::sync::Mutex::new(ObserveQueue {
    running: false,
    active: Vec::new(),
    current: None,
    next: None,
    check: None,
    misses: 0,
    missed_at: 0,
});

/// The latest reading of each row, keyed by row, with the generation the page
/// compares and the plan the readings were taken for.
#[cfg(target_os = "macos")]
struct ObservedRows {
    rows: Vec<aterm_types::macos_setup::Observation>,
    last_known: Vec<aterm_types::macos_setup::Observation>,
    retained_reads: Vec<aterm_cli::macos_doctor::LastRead>,
    history_restored: Vec<aterm_types::macos_setup::Subject>,
    history_epoch: u64,
    unconfirmed: Vec<aterm_types::macos_setup::Subject>,
    generation: u64,
    caps_live: Option<(u64, aterm_types::macos_setup::LiveKeyboardMapping)>,
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
    /// The [`SetupActivity`] epoch each kept row's read started in: what tells a reading from
    /// before a stage that could change its row from one after it.
    read_epochs: Vec<(aterm_types::macos_setup::Subject, u64)>,
    /// This process looked once for what a previous launch kept of the private rows
    /// ([`SetupIo::remembered`]): it seeds only rows this process has no reading of.
    remembered_looked: bool,
    /// The change journal as each kept row's read started ([`SetupIo::fence`]): what that
    /// reading is kept for the next launch under, and only while nothing was journaled since. A
    /// row with none (its journal unread) is not kept.
    fences: Vec<(
        aterm_types::macos_setup::Subject,
        aterm_cli::macos_doctor::Fence,
    )>,
}

#[cfg(target_os = "macos")]
impl ObservedRows {
    /// `subject`'s reading is fenced by `fence` from now, or by none.
    fn fence(
        &mut self,
        subject: aterm_types::macos_setup::Subject,
        fence: Option<aterm_cli::macos_doctor::Fence>,
    ) {
        self.fences.retain(|(row, _)| *row != subject);
        if let Some(fence) = fence {
            self.fences.push((subject, fence));
        }
    }
}

#[cfg(target_os = "macos")]
static OBSERVED_ROWS: std::sync::Mutex<ObservedRows> = std::sync::Mutex::new(ObservedRows {
    rows: Vec::new(),
    last_known: Vec::new(),
    retained_reads: Vec::new(),
    history_restored: Vec::new(),
    history_epoch: 0,
    unconfirmed: Vec::new(),
    generation: 0,
    caps_live: None,
    vault_url: None,
    plan: None,
    choices: 0,
    access_steps: 0,
    read_epochs: Vec::new(),
    remembered_looked: false,
    fences: Vec::new(),
});

/// Held while [`remember_private_readings`] takes its snapshot and hands it on, so writers take
/// turns and the last one writes the latest snapshot. Taken before [`SETUP_ACTIVITY`] and
/// [`OBSERVED_ROWS`], never under them.
#[cfg(target_os = "macos")]
static REMEMBER_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keep this process's latest readings of the rows read privately for the next launch
/// ([`SetupIo::remember`]): each kept, dated reading of such a row that reads it as wanted, no
/// stage running on its row and none run since its read started, with the journal as that read
/// started ([`ObservedRows::fences`]), which the record keeps it under only while no aterm process
/// journaled a change since. Run after a stage ends and whenever the readings change. A row that
/// reads otherwise drops out, so the next launch never carries it as wanted.
#[cfg(target_os = "macos")]
fn remember_private_readings() {
    let Ok(_turn) = REMEMBER_TURN.lock() else {
        return;
    };
    let snapshot: Vec<_> = {
        let Ok(activity) = SETUP_ACTIVITY.lock() else {
            return;
        };
        let Ok(rows) = OBSERVED_ROWS.lock() else {
            return;
        };
        // Before this process looked for what the last launch kept, a snapshot with no dated
        // private reading of its own would only erase that record before its look.
        if !rows.remembered_looked
            && !rows
                .rows
                .iter()
                .any(|seen| seen.subject.reads_privately() && seen.read_at().is_some())
        {
            return;
        }
        rows.rows
            .iter()
            .filter(|seen| seen.subject.reads_privately() && seen.read_back_settled())
            .filter(|seen| {
                let read_in = rows
                    .read_epochs
                    .iter()
                    .find(|(row, _)| *row == seen.subject)
                    .map_or(0, |(_, epoch)| *epoch);
                !activity.subjects.contains(&seen.subject)
                    && activity
                        .touched
                        .iter()
                        .all(|(row, epoch)| *row != seen.subject || *epoch <= read_in)
            })
            .filter_map(|seen| {
                rows.fences
                    .iter()
                    .find(|(row, _)| *row == seen.subject)
                    .map(|(_, fence)| (seen.clone(), *fence))
            })
            .collect()
    };
    if let Err(why) = (setup_io().remember)(&snapshot) {
        aterm_log::warn!("System State could not keep its last read-back: {why}");
    }
}

/// A generation captured before an actual native read, never before an effect gate.
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct SetupHistoryTicket {
    store: aterm_cli::macos_doctor::SetupHistoryStore,
    basis: u64,
}

#[cfg(target_os = "macos")]
fn begin_setup_history(
    plan: Option<&aterm_types::macos_setup::Plan>,
    choices: Option<u64>,
) -> Option<SetupHistoryTicket> {
    let store = (setup_io().history_store)()?;
    begin_setup_history_in(store, plan, choices)
}

/// Refresh saved display values before startup or a cold observer publishes checking.
/// These worker reads complement the GUI launch's display-only seed.
#[cfg(target_os = "macos")]
fn preload_setup_history() {
    let choices = {
        let Ok(rows) = OBSERVED_ROWS.lock() else {
            return;
        };
        if !rows.last_known.is_empty() {
            return;
        }
        rows.choices
    };
    let Some(store) = (setup_io().history_store)() else {
        return;
    };
    let plan = observed_plan(None, choices).map(|(plan, _)| plan);
    let _ = begin_setup_history_in(store, plan.as_ref(), Some(choices));
}

/// Any System State page gets its saved display values before the GUI loop starts.
/// This launch-only read uses aterm's records, never a native probe or a setup admission.
#[cfg(target_os = "macos")]
fn initial_setup_launch_history() -> Option<aterm_gui::SetupDisplayHistory> {
    let choices = OBSERVED_ROWS.lock().ok()?.choices;
    let store = (setup_io().history_store)()?;
    initial_setup_launch_history_in(store, choices)
}

#[cfg(target_os = "macos")]
fn initial_setup_launch_history_in(
    store: aterm_cli::macos_doctor::SetupHistoryStore,
    choices: u64,
) -> Option<aterm_gui::SetupDisplayHistory> {
    let (plan, history) = store.display()?;
    restore_setup_history(&history, Some(&plan), Some(choices))?;
    let rows = OBSERVED_ROWS.lock().ok()?;
    if rows.choices != choices {
        return None;
    }
    let source = plan.vault_url.trim();
    let source_matches = !source.is_empty()
        && history.vault_source.trim() == source
        && rows.vault_url.as_deref().map(str::trim) == Some(source);
    let last_known = rows
        .last_known
        .iter()
        .filter(|row| !row.subject.uses_vault_source() || source_matches)
        .cloned()
        .collect();
    Some(aterm_gui::SetupDisplayHistory {
        plan,
        last_known,
        vault_source: source_matches.then(|| rows.vault_url.clone()).flatten(),
    })
}

#[cfg(target_os = "macos")]
fn begin_setup_history_in(
    store: aterm_cli::macos_doctor::SetupHistoryStore,
    plan: Option<&aterm_types::macos_setup::Plan>,
    choices: Option<u64>,
) -> Option<SetupHistoryTicket> {
    let history = store.begin()?;
    restore_setup_history(&history, plan, choices)?;
    Some(SetupHistoryTicket {
        store,
        basis: history.generation,
    })
}

#[cfg(target_os = "macos")]
fn restore_setup_history(
    history: &aterm_cli::macos_doctor::DisplayHistory,
    plan: Option<&aterm_types::macos_setup::Plan>,
    choices: Option<u64>,
) -> Option<()> {
    let changed = if let Ok(mut rows) = OBSERVED_ROWS.lock() {
        if choices.is_some_and(|choices| choices != rows.choices) {
            return None;
        }
        let source = plan
            .map(|plan| plan.vault_url.trim())
            .or_else(|| rows.plan.as_ref().map(|plan| plan.vault_url.trim()))
            .map(str::to_owned);
        let mut changed = false;
        for read in &history.rows {
            if rows.history_restored.contains(&read.subject)
                || (read.subject.uses_vault_source()
                    && source.as_deref() != Some(history.vault_source.as_str()))
            {
                continue;
            }
            rows.history_restored.push(read.subject);
            if rows.rows.iter().any(|row| row.subject == read.subject) {
                continue;
            }
            if !rows
                .last_known
                .iter()
                .any(|row| row.subject == read.subject)
            {
                rows.last_known.push(read.observation());
                rows.retained_reads.push(read.clone());
                changed = true;
            }
            if read.subject.uses_vault_source() && rows.vault_url.is_none() {
                rows.vault_url = source.clone();
            }
        }
        if changed {
            rows.generation += 1;
        }
        changed
    } else {
        false
    };
    if changed {
        publish_setup_readings();
    }
    Some(())
}

#[cfg(target_os = "macos")]
fn retain_completed_read(
    rows: &mut ObservedRows,
    seen: &aterm_types::macos_setup::Observation,
    at: u64,
) -> bool {
    // A real accepted attempt (Unknown included) has already considered this subject in
    // this process. Disk history cannot later repopulate a deliberately retired context.
    if !rows.history_restored.contains(&seen.subject) {
        rows.history_restored.push(seen.subject);
    }
    let Some(read) = aterm_cli::macos_doctor::LastRead::capture(seen, at) else {
        return false;
    };
    if let Some(old) = rows
        .retained_reads
        .iter_mut()
        .find(|old| old.subject == read.subject)
    {
        *old = read;
    } else {
        rows.retained_reads.push(read);
    }
    rows.history_epoch += 1;
    true
}

/// A stale callback or another process's saved generation skips the optional write.
/// No cache mutex is held while acquiring the file lock or performing file I/O.
#[cfg(target_os = "macos")]
fn persist_setup_history(ticket: Option<SetupHistoryTicket>) {
    let Some(ticket) = ticket else {
        return;
    };
    let snapshot = OBSERVED_ROWS.lock().ok().map(|rows| {
        (
            rows.history_epoch,
            rows.generation,
            rows.choices,
            rows.vault_url.clone(),
            rows.retained_reads.clone(),
        )
    });
    let Some((epoch, generation, choices, source, reads)) = snapshot else {
        return;
    };
    if reads.is_empty() {
        return;
    }
    ticket.store.save(
        ticket.basis,
        source.as_deref().unwrap_or(""),
        &reads,
        || {
            OBSERVED_ROWS.lock().is_ok_and(|rows| {
                rows.history_epoch == epoch
                    && rows.generation == generation
                    && rows.choices == choices
                    && rows.vault_url == source
            })
        },
    );
}

#[cfg(target_os = "macos")]
fn retire_observed_rows(
    rows: &mut ObservedRows,
    subjects: &[aterm_types::macos_setup::Subject],
) -> bool {
    retire_observed_rows_with_history(rows, subjects, RetiredHistory::Discard)
}

#[cfg(target_os = "macos")]
enum RetiredHistory {
    Discard,
    Capture,
    Preserve,
}

#[cfg(target_os = "macos")]
fn remember_observed_values(
    rows: &mut ObservedRows,
    subjects: &[aterm_types::macos_setup::Subject],
) -> bool {
    use aterm_types::macos_setup::Standing;
    let mut changed = false;
    for row in rows.rows.iter().filter(|row| {
        subjects.contains(&row.subject)
            && !rows.unconfirmed.contains(&row.subject)
            && row.read_at().is_some_and(|at| at != 0)
            && !matches!(row.standing, Standing::Unknown | Standing::Failed)
    }) {
        match rows
            .last_known
            .iter_mut()
            .find(|old| old.subject == row.subject)
        {
            Some(old) if old == row => {}
            Some(old) => {
                *old = row.clone();
                changed = true;
            }
            None => {
                rows.last_known.push(row.clone());
                changed = true;
            }
        }
    }
    changed
}

#[cfg(target_os = "macos")]
fn retire_observed_rows_with_history(
    rows: &mut ObservedRows,
    subjects: &[aterm_types::macos_setup::Subject],
    history: RetiredHistory,
) -> bool {
    let mut history_changed = match history {
        RetiredHistory::Capture => remember_observed_values(rows, subjects),
        RetiredHistory::Preserve => false,
        RetiredHistory::Discard => {
            for subject in subjects {
                if !rows.history_restored.contains(subject) {
                    rows.history_restored.push(*subject);
                }
            }
            let before = rows.last_known.len();
            rows.last_known
                .retain(|row| !subjects.contains(&row.subject));
            before != rows.last_known.len()
        }
    };
    let unconfirmed = rows.unconfirmed.len();
    rows.unconfirmed
        .retain(|subject| !subjects.contains(subject));
    history_changed |= unconfirmed != rows.unconfirmed.len();
    let before = rows.rows.len();
    rows.rows.retain(|row| !subjects.contains(&row.subject));
    let live_retired = subjects.contains(&aterm_types::macos_setup::Subject::Setting(
        aterm_types::macos_setup::Setting::CapsLock,
    )) && rows.caps_live.take().is_some();
    let changed = rows.rows.len() != before || live_retired || history_changed;
    if changed {
        rows.generation += 1;
    }
    changed
}

#[cfg(target_os = "macos")]
fn replace_observed_row(rows: &mut ObservedRows, seen: aterm_types::macos_setup::Observation) {
    let was_unconfirmed = rows.unconfirmed.contains(&seen.subject);
    let unconfirmed = seen.standing == aterm_types::macos_setup::Standing::Unknown;
    if unconfirmed && !was_unconfirmed {
        rows.unconfirmed.push(seen.subject);
    } else if !unconfirmed {
        rows.unconfirmed.retain(|subject| *subject != seen.subject);
    }
    match rows.rows.iter_mut().find(|row| row.subject == seen.subject) {
        Some(row) if *row == seen && was_unconfirmed == unconfirmed => return,
        Some(row) => *row = seen,
        None => rows.rows.push(seen),
    }
    rows.generation += 1;
}

/// A source/device/choice transition invalidates before any activation, including
/// a failed activation. It also fences a read already in flight without ending
/// the stage that owns that read.
#[cfg(target_os = "macos")]
fn invalidate_setup_observations(subjects: &[aterm_types::macos_setup::Subject]) {
    let changed = if let Ok(mut activity) = SETUP_ACTIVITY.lock()
        && let Ok(mut rows) = OBSERVED_ROWS.lock()
    {
        activity.touch(subjects);
        retire_observed_rows(&mut rows, subjects)
    } else {
        false
    };
    if changed {
        publish_setup_readings();
        // A retired private reading leaves what the next launch keeps with it.
        remember_private_readings();
    }
}

#[cfg(target_os = "macos")]
fn invalidate_workstation_observations() {
    use aterm_types::macos_setup::{Setting, Subject};
    invalidate_setup_observations(
        &Setting::ALL
            .into_iter()
            .filter(|setting| setting.is_workstation())
            .map(Subject::Setting)
            .collect::<Vec<_>>(),
    );
}

#[cfg(target_os = "macos")]
fn invalidate_input_observations() {
    use aterm_types::macos_setup::{Setting, Subject};
    invalidate_setup_observations(&[Subject::Setting(Setting::CapsLock)]);
}

/// The observation fence captured before a Window's existing input sample. The accepted
/// revision is supplied by that sample's canonical scope, not by a finished setup marker.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct CapsLiveScope {
    choices: u64,
    started: u64,
}

#[cfg(target_os = "macos")]
impl CapsLiveScope {
    fn current(&self, activity: &SetupActivity, rows: &ObservedRows) -> bool {
        use aterm_types::macos_setup::{Setting, Subject};
        let caps = Subject::Setting(Setting::CapsLock);
        self.choices == rows.choices
            && !activity.subjects.contains(&caps)
            && !activity.input_readback
            && activity
                .touched
                .iter()
                .all(|(subject, epoch)| *subject != caps || *epoch <= self.started)
    }
}

#[cfg(target_os = "macos")]
fn capture_caps_live_scope() -> Option<CapsLiveScope> {
    let activity = SETUP_ACTIVITY.lock().ok()?;
    let rows = OBSERVED_ROWS.lock().ok()?;
    Some(CapsLiveScope {
        choices: rows.choices,
        started: activity.epoch,
    })
}

/// Publish only from the sampler, after releasing its actor lock. The fast path performs
/// no canonical/private read and no wake for an unchanged diagnostic. A new positive fact
/// uses the existing profile/current-revision guard; cache mutation takes only observation
/// locks, never filesystem, console or native readers.
#[cfg(target_os = "macos")]
fn publish_caps_live(
    scope: Option<CapsLiveScope>,
    live: Option<(u64, aterm_types::macos_setup::LiveKeyboardMapping)>,
) {
    let Some(scope) = scope else {
        return;
    };
    if let Ok(activity) = SETUP_ACTIVITY.lock()
        && let Ok(rows) = OBSERVED_ROWS.lock()
        && (!scope.current(&activity, &rows) || rows.caps_live == live)
    {
        return;
    }
    let changed = if let Some((revision, _)) = live {
        aterm_cli::macos_doctor::with_current_input_scope(revision, setup_stop_requested, || {
            replace_caps_live(scope, live)
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| replace_caps_live(scope, None))
    } else {
        replace_caps_live(scope, None)
    };
    if changed {
        publish_setup_readings();
    }
}

#[cfg(target_os = "macos")]
fn replace_caps_live(
    scope: CapsLiveScope,
    live: Option<(u64, aterm_types::macos_setup::LiveKeyboardMapping)>,
) -> bool {
    let Ok(activity) = SETUP_ACTIVITY.lock() else {
        return false;
    };
    let Ok(mut rows) = OBSERVED_ROWS.lock() else {
        return false;
    };
    if !scope.current(&activity, &rows) || rows.caps_live == live {
        return false;
    }
    rows.caps_live = live;
    rows.generation += 1;
    true
}

/// Queue a read-only reading of `subjects` on its own worker, never the setup
/// slot, retiring their readings first: something may have changed them. Requests made while
/// one runs coalesce by ordinary refresh or explicit Check.
#[cfg(target_os = "macos")]
fn request_observation(
    plan: Option<aterm_types::macos_setup::Plan>,
    subjects: Vec<aterm_types::macos_setup::Subject>,
    forced: bool,
) -> bool {
    if subjects.is_empty() {
        return true;
    }
    if forced {
        // A pure Check retires current authority, but keeps its last useful values
        // separately for display. Real source/device/choice/effect changes clear both.
        let changed = if let Ok(mut activity) = SETUP_ACTIVITY.lock()
            && let Ok(mut rows) = OBSERVED_ROWS.lock()
        {
            activity.touch(&subjects);
            retire_observed_rows_with_history(&mut rows, &subjects, RetiredHistory::Capture)
        } else {
            false
        };
        if changed {
            publish_setup_readings();
        }
    } else {
        invalidate_setup_observations(&subjects);
    }
    queue_observation(ObserveRequest {
        plan,
        choices: OBSERVED_ROWS.lock().map_or(0, |rows| rows.choices),
        subjects,
        forced,
        pending_repositories: false,
    })
}

/// A finished setup's first look at this launch: no apply pass ends in it to refresh the
/// checklist, so it reads every row now through the ordinary observer, keeping its selected
/// account and network scope, as a refresh, since nothing is known to have changed: a private
/// row it may not read keeps what the last launch read of it ([`observe_rows`]).
#[cfg(target_os = "macos")]
fn look_at_finished_setup() -> bool {
    refresh_observation(aterm_types::macos_setup::Subject::all())
}

/// Queue a reading of `subjects` that nothing is known to have changed (a pass's last refresh,
/// a launch's first look): their readings stand until it reads them, and a private row it may
/// not read keeps its still-valid one ([`observe_rows`]).
#[cfg(target_os = "macos")]
fn refresh_observation(subjects: Vec<aterm_types::macos_setup::Subject>) -> bool {
    if subjects.is_empty() {
        return true;
    }
    queue_observation(ObserveRequest {
        plan: None,
        choices: OBSERVED_ROWS.lock().map_or(0, |rows| rows.choices),
        subjects,
        forced: false,
        pending_repositories: false,
    })
}

/// A converge child may have changed checkout state. Queue only; the observer reads and
/// validates the current pending scope off the child reaper and the event loop.
#[cfg(target_os = "macos")]
fn observe_pending_repositories() {
    let _ = queue_observation(ObserveRequest {
        plan: None,
        choices: OBSERVED_ROWS.lock().map_or(0, |rows| rows.choices),
        subjects: vec![aterm_types::macos_setup::Subject::Setting(
            aterm_types::macos_setup::Setting::Repositories,
        )],
        forced: false,
        pending_repositories: true,
    });
}

/// Enqueue the captured request; another choice or request may have arrived
/// between capturing its scope and acquiring the queue.
#[cfg(target_os = "macos")]
fn queue_observation(request: ObserveRequest) -> bool {
    queue_observation_with(request, start_observation)
}

#[cfg(target_os = "macos")]
type ObservationJob = Box<dyn FnOnce() + Send>;

#[cfg(target_os = "macos")]
fn start_observation(job: ObservationJob) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-setup-observe".into())
        .spawn(job)
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn observation_millis() -> u64 {
    static CLOCK: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(
        CLOCK
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// The existing window keeper retries only retained, never-started work. An idle
/// or running queue takes no clock reading and starts nothing, and publishes the readings only
/// while the window is owed them ([`dispatch_observation_publishing`]).
#[cfg(target_os = "macos")]
fn maintain_observation_dispatch() {
    dispatch_observation_with(None, observation_millis, start_observation);
}

#[cfg(target_os = "macos")]
fn queue_observation_with(
    request: ObserveRequest,
    spawn: impl FnOnce(ObservationJob) -> std::io::Result<()>,
) -> bool {
    dispatch_observation_with(Some(request), observation_millis, spawn)
}

#[cfg(target_os = "macos")]
fn dispatch_observation_with(
    request: Option<ObserveRequest>,
    now: impl FnOnce() -> u64,
    spawn: impl FnOnce(ObservationJob) -> std::io::Result<()>,
) -> bool {
    dispatch_observation_publishing(
        request,
        aterm_gui::initial_setup_readings_undelivered,
        publish_setup_readings,
        now,
        spawn,
    )
}

/// Enqueue `request`, if any, and start the reader a pending batch owes, saying each outcome
/// with `publish` from this thread. A dispatch with no request (the keeper's round,
/// [`maintain_observation_dispatch`]) that finds the queue idle or its reader running changed
/// nothing the readings show: it publishes only while `undelivered`, the window's loop having
/// taken no readings yet or dropped a publication since the last it took
/// ([`aterm_gui::initial_setup_readings_undelivered`]). A dispatch that finds a pending batch
/// and no reader running publishes once, whether that reader's start is held, made or refused.
#[cfg(target_os = "macos")]
fn dispatch_observation_publishing(
    request: Option<ObserveRequest>,
    undelivered: impl FnOnce() -> bool,
    publish: impl Fn(),
    now: impl FnOnce() -> u64,
    spawn: impl FnOnce(ObservationJob) -> std::io::Result<()>,
) -> bool {
    let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
        return false;
    };
    let enqueued = request.is_some();
    if let Some(request) = request {
        queue.enqueue(request);
    }
    if queue.running || (queue.next.is_none() && queue.check.is_none()) {
        drop(queue);
        if enqueued || undelivered() {
            publish();
        }
        return true;
    }
    let now = now();
    if queue.start_held(now) {
        drop(queue);
        publish();
        return true;
    }
    queue.running = true;
    // Keep the finite pending batch in the queue until its worker actually starts.
    // Holding this admission lock also serializes concurrent callers with spawn.
    let spawned = spawn(Box::new(|| {
        preload_setup_history();
        loop {
            let batch = {
                let Ok(mut queue) = OBSERVE_QUEUE.lock() else {
                    return;
                };
                queue.misses = 0;
                let batch = queue.take_pending();
                if batch.iter().all(Option::is_none) {
                    queue.running = false;
                    drop(queue);
                    publish_setup_readings();
                    return;
                }
                batch
            };
            publish_setup_readings();
            // A started read, Unknown included, is consumed once. Only new requests
            // enter the queue while this finite batch runs.
            for request in batch.into_iter().flatten() {
                if let Ok(mut queue) = OBSERVE_QUEUE.lock() {
                    queue.active.retain(|active| active != &request);
                    queue.current = Some(request.clone());
                }
                publish_setup_readings();
                observe_rows(request);
                if let Ok(mut queue) = OBSERVE_QUEUE.lock() {
                    queue.current = None;
                }
                publish_setup_readings();
            }
        }
    }));
    queue.running = spawned.is_ok();
    if let Err(why) = spawned {
        queue.misses = queue.misses.saturating_add(1);
        queue.missed_at = now;
        drop(queue);
        publish();
        aterm_log::warn!("System State could not read its rows: {why}");
        return false;
    }
    drop(queue);
    publish();
    true
}

/// Read, keep what no stage changed meanwhile, and publish the new readings.
#[cfg(target_os = "macos")]
fn observe_rows(request: ObserveRequest) {
    use aterm_types::macos_setup::{Observation, Setting, Standing, Subject};
    let repositories = Subject::Setting(Setting::Repositories);
    let mut pending = if request.pending_repositories {
        (setup_io().pending_repositories)().ok().flatten()
    } else {
        None
    };
    if request.pending_repositories && pending.is_none() {
        return;
    }
    let named = request
        .plan
        .clone()
        .or_else(|| pending.as_ref().map(|admission| admission.plan.clone()));
    let Some((plan, choices)) = observed_plan(named, request.choices) else {
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
        .filter(|subject| !request.pending_repositories || *subject == repositories)
        .filter(|subject| network || *subject != repositories)
        .filter(|subject| match subject {
            aterm_types::macos_setup::Subject::Setting(setting)
                if setting.group() == aterm_types::macos_setup::Group::Accounts =>
            {
                forced || plan.selection.contains(*setting)
            }
            _ => true,
        })
        .collect();
    // Progress names the plan actually admitted by observed_plan, not an old
    // queued choice or the other source in this worker's finite batch.
    if let Ok(mut queue) = OBSERVE_QUEUE.lock()
        && queue.current.is_some()
    {
        queue.current = Some(ObserveRequest {
            plan: Some(plan.clone()),
            choices,
            subjects: subjects.clone(),
            ..request.clone()
        });
    }
    publish_setup_readings();
    if network && subjects.contains(&repositories) && !request.pending_repositories {
        pending = (setup_io().pending_repositories)().ok().flatten();
    }
    let pending = pending.filter(|admission| {
        !admission.completed && admission.plan == plan && !setup_stop_requested(admission.revision)
    });
    if request.pending_repositories && pending.is_none() {
        return;
    }
    // Check again reads the Secrets vault row as the ALab vault answers now: a look for it
    // first, through the LAN broker.
    if forced
        && subjects.contains(&aterm_types::macos_setup::Subject::Setting(
            Setting::Secrets,
        ))
    {
        (setup_io().look_again)();
    }
    if network
        && subjects.contains(&aterm_types::macos_setup::Subject::Setting(
            Setting::Repositories,
        ))
    {
        // Before the read: a token that arrives during it moves the mark past this one.
        token_mark_moved(|| true);
    }
    let history_ticket = begin_setup_history(Some(&plan), Some(choices));
    let started = SETUP_ACTIVITY.lock().map_or(0, |activity| activity.epoch);
    // The journal as the read starts: a change any aterm process journals after it keeps what
    // this read finds out of the next launch's record ([`ObservedRows::fences`]).
    let fence = (setup_io().fence)();
    // Every reading is dated from before its read starts, so its age is never told as less than
    // it is (design §3.5).
    let read_at = (setup_io().now_secs)();
    let admission = if forced {
        aterm_cli::macos_doctor::ObservationRead::ExplicitCheck
    } else {
        aterm_cli::macos_doctor::ObservationRead::Ordinary {
            page_front: aterm_gui::system_state_on_screen,
        }
    };
    // Keep the last useful value before a read can supply Unknown or be skipped.
    // This is presentation history only, under the same choice/source scope.
    if let Ok(mut rows) = OBSERVED_ROWS.lock()
        && rows.choices == choices
    {
        let same_source = rows.vault_url.as_deref() == Some(plan.vault_url.trim());
        let remembered: Vec<_> = subjects
            .iter()
            .copied()
            .filter(|subject| !subject.uses_vault_source() || same_source)
            .collect();
        if remember_observed_values(&mut rows, &remembered) {
            rows.generation += 1;
        }
    }
    let read = std::panic::catch_unwind(|| (setup_io().read)(&plan, &subjects, network, admission))
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
    let read: Vec<Observation> = read.into_iter().map(|seen| seen.stamped(read_at)).collect();
    // A private row this process has no reading of, read withheld: what a previous launch kept
    // of it, looked for once (design §3.5), so a relaunch nobody watches does not leave a
    // finished Mac's rows unread.
    let remembered = if read
        .iter()
        .any(|seen| seen.is_withheld() && seen.subject.reads_privately())
        && OBSERVED_ROWS
            .lock()
            .is_ok_and(|mut rows| !std::mem::replace(&mut rows.remembered_looked, true))
    {
        (setup_io().remembered)()
    } else {
        Vec::new()
    };
    // Display history dates its values in milliseconds; `read_at` above stamps readings in seconds.
    let history_at = aterm_time::unix_millis().unwrap_or(0);
    // A child's status supplies no readiness. Only a fresh native reading for the same
    // still-pending plan/revision can settle its park, even when the displayed row was
    // already ready or had no previous reading. This never lifts a stage's ledger hold.
    let repository_ready_can_wake = pending.is_some_and(|before| {
        (setup_io().pending_repositories)()
            .ok()
            .flatten()
            .is_some_and(|now| {
                now.revision == before.revision
                    && !now.completed
                    && now.plan == plan
                    && !setup_stop_requested(now.revision)
            })
    });
    if request.pending_repositories && !repository_ready_can_wake {
        return;
    }
    let access_steps = aterm_cli::macos_doctor::initial_setup_access_revision()
        .ok()
        .zip(aterm_types::dirs::state_dir())
        .map_or(0, |(revision, dir)| {
            aterm_types::macos_setup::access_record::read(&dir, revision).len()
        });
    let (changed, wake, retained) = {
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
            let choices = rows.choices;
            drop(rows);
            drop(activity);
            queue_observation(ObserveRequest {
                plan: None,
                choices,
                subjects: request.subjects,
                forced: false,
                pending_repositories: request.pending_repositories,
            });
            return;
        }
        let source = plan.vault_url.trim();
        let mut changed = rows.vault_url.as_deref() != Some(source);
        if changed {
            rows.rows.retain(|row| !row.subject.uses_vault_source());
            rows.last_known
                .retain(|row| !row.subject.uses_vault_source());
            rows.retained_reads
                .retain(|row| !row.subject.uses_vault_source());
            rows.unconfirmed
                .retain(|subject| !subject.uses_vault_source());
            rows.vault_url = Some(source.to_owned());
        }
        rows.plan = Some(plan);
        let mut retained = false;
        // A row a person fixed, or an access step the window recorded, is what a
        // parked run waits for.
        let mut wake = rows.access_steps != access_steps;
        rows.access_steps = access_steps;
        // A reading taken while its own stage or Restore ran may be half of a change.
        let current_subject = |subject: &Subject| {
            !activity.subjects.contains(subject)
                && !(activity.input_readback && *subject == Subject::Setting(Setting::CapsLock))
                && activity
                    .touched
                    .iter()
                    .all(|(row, epoch)| row != subject || *epoch <= started)
        };
        for seen in read
            .into_iter()
            .filter(|seen| current_subject(&seen.subject))
        {
            // A withheld read ran no reader, so it supplies no evidence either way: the row's
            // unconfirmed mark and its display history stay as they were. An attempted read that
            // said nothing marks it.
            if !seen.is_withheld() {
                retained |= retain_completed_read(&mut rows, &seen, history_at);
                let was_unconfirmed = rows.unconfirmed.contains(&seen.subject);
                if seen.standing == Standing::Unknown {
                    if !was_unconfirmed {
                        rows.unconfirmed.push(seen.subject);
                        changed = true;
                    }
                } else if was_unconfirmed {
                    rows.unconfirmed.retain(|subject| *subject != seen.subject);
                    changed = true;
                }
            }
            let subject = seen.subject;
            wake |= repository_ready_can_wake
                && subject == repositories
                && seen.standing == Standing::InPlace;
            // A withheld read read nothing: the row's earlier reading stands, with its own time,
            // which says how old it is, unless a stage that could change the row ran since that
            // reading started (or nothing says when it started), which leaves the row unread.
            let kept_from = rows
                .read_epochs
                .iter()
                .find(|(row, _)| *row == subject)
                .map_or(0, |(_, epoch)| *epoch);
            let changed_since = activity
                .touched
                .iter()
                .any(|(row, epoch)| *row == subject && *epoch > kept_from);
            // What a previous launch kept stands for a withheld row this process has no
            // reading of, while no stage of this process touched the row: one that did may have
            // changed it after that reading.
            let (seen, fence) = match remembered.iter().find(|(kept, _)| kept.subject == subject) {
                Some((kept, kept_fence))
                    if seen.is_withheld()
                        && !rows.rows.iter().any(|row| row.subject == subject)
                        && !activity.touched.iter().any(|(row, _)| *row == subject) =>
                {
                    (kept.clone(), Some(*kept_fence))
                }
                _ => (seen, fence),
            };
            match rows.rows.iter_mut().find(|row| row.subject == subject) {
                Some(_) if seen.is_withheld() && !changed_since => continue,
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
            match rows.read_epochs.iter_mut().find(|(row, _)| *row == subject) {
                Some(kept) => kept.1 = started,
                None => rows.read_epochs.push((subject, started)),
            }
            rows.fence(subject, fence);
        }
        if changed {
            rows.generation += 1;
        }
        (changed, wake, retained)
    };
    if retained {
        persist_setup_history(history_ticket);
    }
    if changed {
        remember_private_readings();
        aterm_gui::publish_initial_setup_status(initial_setup_status());
    }
    if wake {
        wake_setup();
    }
}

/// Start an ordinary read-only refresh of every System State row for `plan`. Private readers
/// require the page still in front when their native batch starts.
#[cfg(target_os = "macos")]
fn observe_initial_setup(plan: aterm_types::macos_setup::Plan) -> bool {
    // A focus or navigation refresh replaces readings in place. Actual source, device,
    // choice and effect transitions still retire them through invalidate_setup_observations.
    queue_observation(ObserveRequest {
        plan: Some(plan),
        choices: OBSERVED_ROWS.lock().map_or(0, |rows| rows.choices),
        subjects: aterm_types::macos_setup::Subject::all(),
        forced: false,
        pending_repositories: false,
    })
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

/// System State's offer was pressed by a person ([`aterm_gui::InitialSetupHandlers::ask_access`]):
/// the access steps may run from now on, and the Access rows and a parked run read that at once.
#[cfg(target_os = "macos")]
fn person_asked_for_access() -> bool {
    use aterm_types::macos_setup::{Access, Subject};
    if let Err(why) = aterm_cli::macos_doctor::note_person_asked_for_access() {
        aterm_log::warn!("setup: a person's ask for macOS's grants was not saved: {why}");
        return false;
    }
    request_observation(None, Access::ALL.map(Subject::Access).to_vec(), false);
    wake_setup();
    true
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

/// System State's Join under the Secrets vault row's phrase field
/// ([`aterm_gui::InitialSetupHandlers::join_phrase`]): on its own worker (the join finds a member
/// on this network and runs the phrase with it), ALab lets this Mac in with the phrase a member
/// showed once its person pressed Allow, as `aterm members join <phrase>` does. The receipt says
/// whether it did, whether it was refused, or whether it reached no member, which spends no
/// phrase; why goes to the log, never the page. Then the Secrets and GitHub repositories rows
/// are read again.
#[cfg(target_os = "macos")]
fn join_alab_with_phrase(
    phrase: aterm_secrets::SecretBytes,
    receipt: aterm_gui::PhraseJoinReceipt,
) -> bool {
    use aterm_agent::members_cli::PhraseTry;
    use aterm_gui::PhraseOutcome;
    use aterm_types::macos_setup::{Setting, Subject};
    std::thread::Builder::new()
        .name("aterm-vault-phrase".into())
        .spawn(move || {
            let tried = aterm_agent::members_cli::join_with_phrase(&phrase);
            drop(phrase);
            receipt.finished(match tried {
                PhraseTry::Joined(said) => {
                    aterm_log::info!("system state: the phrase let this Mac in: {said:?}");
                    PhraseOutcome::Joined
                }
                PhraseTry::Refused(why) => {
                    aterm_log::warn!("system state: the phrase let nobody in: {why}");
                    PhraseOutcome::Refused
                }
                PhraseTry::NotReached(why) => {
                    aterm_log::info!(
                        "system state: the phrase reached no ALab Mac, so it stays good: {why}"
                    );
                    PhraseOutcome::NotReached
                }
                PhraseTry::Dropped(words) => {
                    aterm_log::warn!("system state: no phrase lets this Mac in: {words}");
                    PhraseOutcome::Dropped
                }
            });
            request_observation(
                None,
                vec![
                    Subject::Setting(Setting::Secrets),
                    Subject::Setting(Setting::Repositories),
                ],
                false,
            );
        })
        .is_ok()
}

/// The admitted revision whose worker is alive in this process, if any.
#[cfg(target_os = "macos")]
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
    // A run admitted: an older aterm's note that setup waited for its review goes.
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
    invalidate_workstation_observations();
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
/// update: what a Stop owes the records is saved first.
#[cfg(target_os = "macos")]
fn settle_initial_setup() {
    if !settle_setup_stop(STOP_SETTLE_LIMIT) {
        aterm_log::warn!("System State quit before what its Stop owes the records was saved");
    }
}

/// [`aterm_gui::InitialSetupHandlers::cut_off`]: the app is quitting, or an update's Commit is
/// about to be written (`going`), so a stage running now is cut off by no crash and the next
/// launch resumes it without counting toward the park two crashes in one stage earn; or that
/// Commit did not go, and the note is taken back (design §6.5).
#[cfg(target_os = "macos")]
fn cut_off_initial_setup(going: bool) {
    let noted = if going {
        aterm_cli::macos_doctor::note_setup_interrupted()
    } else {
        aterm_cli::macos_doctor::take_back_setup_interrupted()
    };
    if let Err(why) = noted {
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
    /// This exact automatic admission kept display history before any effect.
    keeps_workstation_history: bool,
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
    keep_history: bool,
) -> SetupChoice {
    let replaced = if replacing {
        SETUP_RUN.lock().ok().and_then(|mut run| run.take())
    } else {
        None
    };
    let subjects: Vec<_> = aterm_types::macos_setup::Setting::ALL
        .into_iter()
        .filter(|setting| setting.is_workstation())
        .map(aterm_types::macos_setup::Subject::Setting)
        .collect();
    // This is still a new admission: old workstation readings lose current authority. Only an
    // automatic retry of this exact plan can carry useful workstation display history to it.
    let mut activity = SETUP_ACTIVITY.lock().ok();
    let (choices, changed, keeps_workstation_history) =
        OBSERVED_ROWS.lock().map_or((0, false, false), |mut rows| {
            let keep_history = keep_history
                && rows.plan.as_ref() == Some(plan)
                && rows
                    .vault_url
                    .as_deref()
                    .is_none_or(|source| source == plan.vault_url.as_str());
            if keep_history {
                rows.last_known
                    .retain(|row| subjects.contains(&row.subject));
            } else {
                rows.last_known.clear();
            }
            rows.choices += 1;
            rows.plan = Some(plan.clone());
            let changed = activity.as_mut().is_some_and(|activity| {
                activity.touch(&subjects);
                retire_observed_rows_with_history(
                    &mut rows,
                    &subjects,
                    if keep_history {
                        RetiredHistory::Capture
                    } else {
                        RetiredHistory::Discard
                    },
                )
            });
            rows.unconfirmed.clear();
            (rows.choices, changed, keep_history)
        });
    drop(activity);
    if changed {
        publish_setup_readings();
        remember_private_readings();
    }
    SetupChoice {
        plan: plan.clone(),
        generation,
        sent: send_admission(),
        replaced,
        choices,
        keeps_workstation_history,
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
    let subjects: Vec<_> = aterm_types::macos_setup::Setting::ALL
        .into_iter()
        .filter(|setting| setting.is_workstation())
        .map(aterm_types::macos_setup::Subject::Setting)
        .collect();
    let restored = if let Ok(mut activity) = SETUP_ACTIVITY.lock()
        && let Ok(mut rows) = OBSERVED_ROWS.lock()
        && rows.choices == choice.choices
    {
        let keep_history = choice.keeps_workstation_history
            && rows.plan.as_ref() == Some(&choice.plan)
            && rows
                .vault_url
                .as_deref()
                .is_none_or(|source| source == choice.plan.vault_url.as_str());
        if keep_history {
            rows.last_known
                .retain(|row| subjects.contains(&row.subject));
        } else {
            rows.last_known.clear();
        }
        rows.choices += 1;
        rows.plan = None;
        activity.touch(&subjects);
        retire_observed_rows_with_history(
            &mut rows,
            &subjects,
            if keep_history {
                RetiredHistory::Preserve
            } else {
                RetiredHistory::Discard
            },
        );
        rows.unconfirmed.clear();
        true
    } else {
        false
    };
    if restored {
        publish_setup_readings();
        remember_private_readings();
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
    let readings = setup_readings();
    status.observed = readings.observed;
    status.last_known = readings.last_known;
    status.unconfirmed_observations = readings.unconfirmed_observations;
    status.observing = readings.observing;
    status.observed_generation = readings.observed_generation;
    status.caps_live = readings.caps_live;
    status.observed_vault_url = readings.observed_vault_url;
    status.running = readings.running;
}

/// One coherent cache snapshot, with no record, native, privacy or healing read.
#[cfg(target_os = "macos")]
fn setup_readings() -> aterm_gui::SetupReadings {
    let requests = OBSERVE_QUEUE
        .lock()
        .map_or_else(|_| Vec::new(), |queue| queue.progress());
    if let Ok(activity) = SETUP_ACTIVITY.lock()
        && let Ok(rows) = OBSERVED_ROWS.lock()
    {
        let order = aterm_types::macos_setup::Subject::all();
        let mut observed = rows.rows.clone();
        observed
            .sort_unstable_by_key(|row| order.iter().position(|subject| *subject == row.subject));
        let mut running = activity.settings.clone();
        let mut observing = Vec::new();
        for (request, queued) in requests {
            if !queued && request.choices != rows.choices {
                continue;
            }
            let plan = request
                .plan
                .as_ref()
                .filter(|_| request.choices == rows.choices)
                .or(rows.plan.as_ref());
            for subject in request.subjects {
                let source = if subject.uses_vault_source() {
                    let Some(plan) = plan else { continue };
                    Some(plan.vault_url.trim().to_owned())
                } else {
                    None
                };
                let progress = (subject, source, queued);
                if !observing.contains(&progress) {
                    observing.push(progress);
                }
            }
        }
        if activity.input_readback
            && !running.contains(&aterm_types::macos_setup::Setting::CapsLock)
        {
            running.push(aterm_types::macos_setup::Setting::CapsLock);
        }
        return aterm_gui::SetupReadings {
            observed,
            last_known: rows.last_known.clone(),
            unconfirmed_observations: rows.unconfirmed.clone(),
            observing,
            observed_generation: rows.generation,
            caps_live: rows.caps_live.map(|(_, live)| live),
            observed_vault_url: rows.vault_url.clone(),
            running,
        };
    }
    aterm_gui::SetupReadings::default()
}

#[cfg(target_os = "macos")]
fn publish_setup_readings() {
    aterm_gui::publish_initial_setup_readings(setup_readings);
}

/// An actual keeper effect or failed native observation retires the old fact
/// for its entire batch, including readers that begin after the first touch.
/// This lease never initiates a full-status/private read or owns another stage.
#[cfg(target_os = "macos")]
struct InputReadbackActivity;

#[cfg(target_os = "macos")]
impl InputReadbackActivity {
    fn begin() -> Self {
        use aterm_types::macos_setup::{Setting, Subject};
        let subjects = [Subject::Setting(Setting::CapsLock)];
        if let Ok(mut activity) = SETUP_ACTIVITY.lock()
            && let Ok(mut rows) = OBSERVED_ROWS.lock()
        {
            activity.input_readback = true;
            activity.touch(&subjects);
            retire_observed_rows(&mut rows, &subjects);
        }
        publish_setup_readings();
        Self
    }
}

#[cfg(target_os = "macos")]
impl Drop for InputReadbackActivity {
    fn drop(&mut self) {
        if let Ok(mut activity) = SETUP_ACTIVITY.lock() {
            activity.input_readback = false;
            activity.touch(&[aterm_types::macos_setup::Subject::Setting(
                aterm_types::macos_setup::Setting::CapsLock,
            )]);
        }
        publish_setup_readings();
    }
}

#[cfg(target_os = "macos")]
fn initial_setup_status() -> aterm_gui::SetupStatus {
    let snapshot = aterm_cli::macos_doctor::initial_setup_snapshot();
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
        Ok(last) => initial_setup_status_with(snapshot, busy, &last),
        Err(_) => initial_setup_status_with(snapshot, busy, &InitialSetupRows::default()),
    };
    status.retrying = SETUP_RETRYING.load(std::sync::atomic::Ordering::Acquire) != 0;
    status.widened = SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire) != 0;
    let queued = SETUP_QUEUED.load(std::sync::atomic::Ordering::Acquire);
    status.queued = queued != 0;
    status.queued_widen =
        queued != 0 && queued == SETUP_QUEUED_WIDEN.load(std::sync::atomic::Ordering::Acquire);
    let startup_retrying = STARTUP_RETRY.lock().is_ok_and(|retry| retry.retrying());
    add_startup_retry(&mut status, startup_retrying);
    add_setup_readings(&mut status);
    if status.plan.as_ref().is_some_and(|plan| {
        plan.selection
            .contains(aterm_types::macos_setup::Setting::Secrets)
    }) {
        status
            .observed
            .push(doctor::ai_accounts_setup_observation());
    }
    add_vault_not_joined(
        &mut status,
        SETUP_VAULT_NOT_JOINED.load(std::sync::atomic::Ordering::Acquire),
    );
    // Only an ALab Mac signs out of Messages and FaceTime: the page says so on that line.
    status.alab = aterm_cli::macos_doctor::alab_mac();
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
    note_set_aside_in(&SET_ASIDE_SAID, set);
}

/// [`note_setup_set_aside`], raising `said` (the process's [`SET_ASIDE_SAID`]).
#[cfg(target_os = "macos")]
fn note_set_aside_in(
    said: &std::sync::atomic::AtomicBool,
    set: &aterm_cli::macos_doctor::SetAside,
) {
    let kept = set.kept.join(", ");
    aterm_log::warn!(
        "System State: saved setup records could not be read and were set aside as {kept}"
    );
    said.store(true, std::sync::atomic::Ordering::Release);
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
    set_aside_read_in(&SET_ASIDE_SAID, recovered, clear);
}

/// [`set_aside_as_read`] against `said` (the process's [`SET_ASIDE_SAID`]).
#[cfg(target_os = "macos")]
fn set_aside_read_in(
    said: &std::sync::atomic::AtomicBool,
    recovered: Option<bool>,
    clear: impl FnOnce(),
) {
    if recovered == Some(false) && said.swap(false, std::sync::atomic::Ordering::AcqRel) {
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
    let restorable = snapshot.as_ref().is_ok_and(|snapshot| snapshot.restorable);
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
        restorable,
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
    // A row that changed after setup had it as wanted is left as it is: no automatic pass
    // rewrites it, and the exceptions digest keeps the fact until a check reads nothing out of
    // place or a Restore puts the originals back.
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
    let keep_history = receipt.admission() == aterm_gui::InitialSetupAdmission::Automatic
        && !receipt.chosen()
        && receipt.changed() == Some(aterm_types::macos_setup::Selection::none())
        && !vault_typed
        && password.is_none();
    let generation = SETUP_APPLY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    // This launch's startup holds the automatic claim, or is asking for it:
    // the choice waits for it, counted from now, so the startup never shows
    // its offer over the choice, however soon it was about to (design §3.3).
    let awaiting = startup_holds_claim().then(AwaitingClaim::new);
    if take_initial_setup_slot_now() {
        // No pass runs: a run, if any, is parked or backing off, and this
        // choice replaces it once admitted.
        let choice = begin_setup_choice(&plan, true, generation, keep_history);
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
    let choice = begin_setup_choice(&plan, false, generation, keep_history);
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
#[path = "main_setup_restart_tests.rs"]
mod setup_restart_tests;

#[cfg(target_os = "macos")]
fn check_initial_setup(
    plan: aterm_types::macos_setup::Plan,
    by: aterm_gui::CheckBy,
) -> aterm_gui::CheckStarted {
    // Check again reads every row, GitHub repositories included; a press also lets a run
    // parked on a person look again, its parked stages lifted and its tries afresh (System
    // State design §3.5). The page's own reading wakes no run: the worker's own pauses do.
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
        move || {
            if by == aterm_gui::CheckBy::Press {
                wake_setup_for_check();
            }
        },
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
            retire_observed_rows(&mut rows, &guard.subjects);
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
/// next launch would (System State design §3.4), so Full Disk Access, Developer Tools and the
/// debugger go on in this launch with no press. The startup also runs when this
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
mod initial_setup_result_tests;

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
        .position(|a| matches!(a.to_str(), Some("-e" | "--command" | "--")))
        .unwrap_or(rest.len())
}

const MODE_FLAGS: &[&str] = &["--window", "--session"];

fn strip_flags(rest: &[OsString], flags: &[&str]) -> Vec<OsString> {
    let boundary = payload_boundary(rest);
    rest.iter()
        .enumerate()
        .filter(|(i, a)| *i >= boundary || a.to_str().is_none_or(|s| !flags.contains(&s)))
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
        .any(|a| a == NO_REROUTE_FLAG)
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
        .is_some_and(|a| matches!(a.to_str(), Some("-h" | "--help")))
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
    #[cfg(target_os = "macos")]
    if let Some(code) = aterm_gui::prepare_window_launch(
        &request.window_args(),
        std::env::args_os().skip(1).collect(),
    ) {
        return code;
    }
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
    let first = rest.first().map(|a| a.to_string_lossy());
    let mut non_verbose = rest
        .iter()
        .filter(|a| !matches!(a.to_str(), Some("-v" | "--verbose")));
    if non_verbose.next().is_some_and(|a| a == "identity") && non_verbose.next().is_none() {
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
        #[cfg(windows)]
        for line in UPDATE_HELP_WINDOWS {
            println!("{line}");
        }
        println!("`aterm help update` has the rest.");
        return ExitCode::SUCCESS;
    }
    #[cfg(windows)]
    if let Some(code) = windows_update_verb(rest, build) {
        return code;
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
    for arg in rest.iter().map(|a| a.to_string_lossy()) {
        match arg.as_ref() {
            "-v" | "--verbose" => verbose = true,
            _ if sub.is_none() && !arg.starts_with('-') => sub = Some(arg),
            _ => {
                // A word that is not ours (`claude --wait-lock 60`, `apply` on a Mac)
                // says what it is, and one of ours out of place says so, whatever follows it.
                if let Some(word) = sub
                    .as_deref()
                    .filter(|w| !matches!(*w, "status" | "check" | "retry" | "holds"))
                {
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
    if sub.as_deref() == Some("holds") {
        return update_holds(verbose);
    }
    let retrying = sub.as_deref() == Some("retry");
    if retrying {
        let checks = !cfg!(windows);
        let next = if checks { "; checking now" } else { "" };
        match aterm_update::retry() {
            Ok(Some(hold)) => {
                let held = if hold.version.is_empty() {
                    format!("build {}", hold.build)
                } else {
                    hold.version
                };
                println!("aterm update: {held} is no longer held{next}");
            }
            Ok(None) => println!("aterm update: nothing is held{next}"),
            Err(error) => {
                eprintln!("aterm update: {error}");
                return ExitCode::FAILURE;
            }
        }
        if !checks {
            return ExitCode::SUCCESS;
        }
    }
    let checking = retrying || sub.as_deref() == Some("check");
    let word = if retrying {
        "check"
    } else {
        sub.as_deref().unwrap_or("status")
    };
    let st = match word {
        "status" => aterm_update::status(build),
        "check" if cfg!(windows) => None,
        "check" => {
            // A check can download a whole release.
            if std::io::IsTerminal::is_terminal(&std::io::stderr())
                && aterm_update::status(build).is_some_and(|st| {
                    st.enabled && st.installable && !st.linux_install_waits_for_a_start()
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
        println!("{}", update_unreadable_line(aterm_stamp::app_version()));
        return if no_ledger_fails(checking) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    };
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let lane = aterm_update::apply_lane_report(build);
    let (line, trouble) = update_summary(
        aterm_stamp::app_version(),
        build,
        &st,
        lane.as_ref(),
        now,
        aterm_update_core::settings::update_auto_apply(),
        // Off only where a read said so: one with no answer says nothing of the setting. The
        // saved setting, which a fleet survey of this Mac says too (`aterm_update::fleet_fact`).
        aterm_update::automatic_as_saved() != Some(false),
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
    "usage: aterm update [status|check|retry|holds] [-v] | identity | enable | apply | rollback"
} else if cfg!(windows) {
    "usage: aterm update [status|check|retry|holds] [-v] | apply [--restart] | identity"
} else {
    "usage: aterm update [status|check|retry|holds] [-v] | identity"
};

const UPDATE_HELP: &[&str] = &[
    "  status     this copy's version and where it stands, one line (the default)",
    "  check      check now (a newer release downloads), then print that line;",
    "             exits 1 when this copy cannot update or checking fails",
    "  retry      end a hold on a newer build now and check, so it is tried again",
    "  holds      each lab Mac's update record: what it runs, what it was offered,",
    "             what it holds and until when (this copy's alone outside a lab)",
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

/// The Windows delivery's own words, under [`UPDATE_HELP`] on Windows alone.
#[cfg(windows)]
const UPDATE_HELP_WINDOWS: &[&str] = &[
    "  apply      install the staged build over this copy; nothing is stopped, open",
    "             windows keep running until they are restarted",
    "  --restart  with apply: save the open windows, install, and start aterm again",
    "On Windows check reads the signed release channel now: the newest release's",
    "zip is verified (roster, appcast, SHA-256) and staged, never installed. A",
    "window checks it every 30 minutes while [update] enabled is on. A build",
    "`aterm-winship update` staged on this machine is weighed against it.",
];

/// `aterm update status|check|apply` on Windows, where the update is a build staged on this
/// machine (`aterm_update::windows`): `None` for a word that is not one of those three.
#[cfg(windows)]
fn windows_update_verb(rest: &[OsString], build: u64) -> Option<ExitCode> {
    use aterm_update::windows as staged;
    let mut sub: Option<String> = None;
    let mut restart = false;
    let mut stray: Option<String> = None;
    for arg in rest.iter().map(|a| a.to_string_lossy()) {
        match arg.as_ref() {
            "-v" | "--verbose" => {}
            "--restart" => restart = true,
            word if sub.is_none() && !word.starts_with('-') => sub = Some(word.to_string()),
            other => {
                stray.get_or_insert_with(|| other.to_string());
            }
        }
    }
    let sub = sub.as_deref().unwrap_or("status");
    if !matches!(sub, "status" | "check" | "apply") {
        return None;
    }
    if let Some(stray) = stray {
        eprintln!("aterm update: unknown argument {stray:?} ({UPDATE_USAGE})");
        return Some(ExitCode::from(2));
    }
    if restart && sub != "apply" {
        eprintln!("aterm update: --restart goes with apply ({UPDATE_USAGE})");
        return Some(ExitCode::from(2));
    }
    let running = staged::Running {
        version: aterm_stamp::app_version(),
        commit: aterm_stamp::current().git_commit,
        build_number: build,
    };
    // `check` reads the release channel first, so the standing below weighs what it staged and
    // its last line is this check's outcome. A check that failed says why on stderr: it may
    // have failed before the lane recorded it (another check holding the lane, say).
    let channel = (sub == "check").then(|| staged::check_channel_now(running));
    if let Some(Err(why)) = &channel {
        eprintln!("aterm update: release channel: {why}");
    }
    let here = staged::here(running);
    if sub == "apply" {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                eprintln!("aterm update: cannot locate this executable ({error})");
                return Some(ExitCode::FAILURE);
            }
        };
        let mode = if restart {
            staged::ApplyMode::Restart
        } else {
            staged::ApplyMode::Staged
        };
        return Some(
            match staged::apply(&here.standing, &exe, mode, staged::InstallerOutput::Inherit) {
                Ok(applied) => {
                    let (name, dest) = (applied.staged.name(), applied.dest.display());
                    if restart {
                        // `install.ps1 -Restart` hands the work to a detached helper and
                        // returns: the install and the relaunch follow this line.
                        println!(
                            "aterm update: the installer took {name} \u{2014} it saves the open \
                             windows, installs into {dest} and starts aterm again"
                        );
                    } else {
                        println!(
                            "aterm update: installed {name} into {dest} \u{2014} open windows \
                             keep running build {build} until they are restarted; new windows \
                             and shells use the new build"
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    eprintln!("aterm update: {why}");
                    ExitCode::FAILURE
                }
            },
        );
    }
    let lines = staged::status_lines(running, &here);
    aterm_log::info!("aterm update: {}", lines.join(" | "));
    for line in lines {
        println!("{line}");
    }
    Some(
        if sub == "check"
            && (matches!(here.standing, staged::Standing::Unreadable(_))
                || matches!(channel, Some(Err(_))))
        {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        },
    )
}

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
            aterm_update::dev_channel::standing(aterm_stamp::app_version())
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

fn downloaded_update_line(
    running_version: &str,
    running_build: u64,
    staged_version: Option<&str>,
    staged_build: u64,
) -> String {
    let next: std::borrow::Cow<'_, str> = staged_version.map_or_else(
        || std::borrow::Cow::Owned(format!("build {staged_build}")),
        std::borrow::Cow::Borrowed,
    );
    if next == running_version {
        format!("aterm build {staged_build} is downloaded (running build {running_build})")
    } else {
        format!("aterm {next} is downloaded (running {running_version})")
    }
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
    automatic_install_configured: bool,
    automatic_checks: bool,
    dev: &DevCopy,
    log_hint: &str,
) -> (String, Option<String>) {
    use aterm_update::StatusArm;
    const CHECKS_OFF: &str =
        " \u{b7} automatic checks are configured off (Settings \u{25b8} Software Update)";
    const CHECKS_ON: &str = "; automatic checks are configured on";
    let next = if automatic_checks {
        CHECKS_ON
    } else {
        CHECKS_OFF
    };
    let checked_words = || {
        aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at)
            .map(|checked| format!(" \u{b7} checked {}", ago_words(checked, now)))
            .unwrap_or_default()
    };
    // The arm is the one a fleet survey says this copy's standing by (`UpdateStatus::arm`).
    let mut line = match st.arm(build, lane) {
        StatusArm::Off => {
            let remedy = st.outcome.trim();
            return (
                format!("This copy of aterm {version} doesn\u{2019}t update itself"),
                (!remedy.is_empty()).then(|| remedy.to_string()),
            );
        }
        StatusArm::LinuxDownloaded {
            build: staged,
            version: staged_version,
        } => {
            let downloaded = downloaded_update_line(version, build, staged_version, staged);
            return (
                format!("{downloaded} \u{2014} `aterm update apply` installs it"),
                None,
            );
        }
        StatusArm::Installed(native) => {
            let installed: std::borrow::Cow<'_, str> = if !native.installed_version.is_empty() {
                std::borrow::Cow::Borrowed(native.installed_version.as_str())
            } else if native.installed_build == build {
                std::borrow::Cow::Borrowed(version)
            } else {
                std::borrow::Cow::Owned(format!("build {}", native.installed_build))
            };
            return (
                format!(
                    "aterm {installed} is installed \u{2014} the next aterm you start finishes it; \
                     update checks wait until then"
                ),
                None,
            );
        }
        StatusArm::Cannot => {
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
        // A live hold outranks a download of the build it holds: the gates refuse it until it lapses.
        StatusArm::Held(hold) => return held_update_line(version, hold, now),
        // Bytes a window here saw fail to start: only a window launches them until this lapses.
        StatusArm::StartFailed(hold) => return start_failed_update_line(version, hold, now),
        StatusArm::Refused {
            build: staged,
            version: staged_version,
            refusal,
        } => {
            let downloaded = downloaded_update_line(version, build, staged_version, staged);
            return (
                format!("{downloaded}; the last install was refused"),
                Some(refusal.to_string()),
            );
        }
        StatusArm::NotInstalling {
            build: staged,
            version: staged_version,
            tries: failed,
        } => {
            let downloaded = downloaded_update_line(version, build, staged_version, staged);
            let tries = if failed == 1 {
                "1 try".to_string()
            } else {
                format!("{failed} tries")
            };
            return (
                format!("{downloaded} but didn\u{2019}t install ({tries})"),
                Some(log_hint.to_string()),
            );
        }
        StatusArm::Downloaded {
            build: staged,
            version: staged_version,
        } => {
            let downloaded = downloaded_update_line(version, build, staged_version, staged);
            return if automatic_install_configured {
                (
                    format!("{downloaded}; automatic installation is configured on"),
                    None,
                )
            } else {
                (
                    format!(
                        "{downloaded} \u{2014} install it from Settings \u{25b8} \
                         Software Update"
                    ),
                    None,
                )
            };
        }
        StatusArm::Unreadable => {
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
        // The one failure a person has to act on: a build that predates the channel's keys,
        // whose ledger sentence prescribes the reinstall.
        StatusArm::TooOld => {
            return (
                format!("aterm {version} is too old to check for updates"),
                Some(
                    "Reinstall aterm from the current release (drag it from the release DMG)."
                        .to_string(),
                ),
            );
        }
        StatusArm::Failing { class } => {
            let trouble = if class == "apply" {
                "the last updates didn\u{2019}t install".to_string()
            } else {
                match check_trouble_words(class) {
                    Some(words) => format!("update checks keep failing: {words}"),
                    None => "update checks keep failing".to_string(),
                }
            };
            return (
                format!("aterm {version} \u{b7} {trouble}{next}"),
                Some(log_hint.to_string()),
            );
        }
        StatusArm::CheckFailed { class } => {
            let why = check_trouble_words(class)
                .map(|words| format!(": {words}"))
                .unwrap_or_default();
            return (
                format!("aterm {version} \u{b7} the last check didn\u{2019}t finish{why}{next}"),
                Some(log_hint.to_string()),
            );
        }
        // Never "up to date" while a newer build is held (a lapsed hold is tried again) or offered.
        StatusArm::Retrying(hold) => {
            let again = if hold.peer_lifted {
                ": it started on another Mac"
            } else {
                ""
            };
            format!(
                "aterm {version} \u{b7} trying {} again{again}{}",
                hold.name(),
                checked_words()
            )
        }
        StatusArm::Behind => format!(
            "aterm {version} \u{b7} a newer release is out and not installed yet; the next \
             check downloads it{}",
            checked_words()
        ),
        StatusArm::Current { at } => format!(
            "aterm {version} is up to date \u{b7} checked {}",
            ago_words(at, now)
        ),
        StatusArm::Unchecked => format!("aterm {version} hasn\u{2019}t checked for updates yet"),
    };
    if !automatic_checks {
        line.push_str(CHECKS_OFF);
    }
    (line, None)
}

/// How long until `until`, from `now`, in plain words ([`update_record::left_words`]).
///
/// [`update_record::left_words`]: aterm_update_core::update_record::left_words
fn left_words(until: u64, now: i64) -> String {
    aterm_update_core::update_record::left_words(
        until.saturating_sub(u64::try_from(now).unwrap_or(0)),
    )
}

/// The line for a live hold on a newer build: what is held, for how long more, why, which try,
/// and the verb that ends it now; the hold's recorded reason goes to stderr.
fn held_update_line(
    version: &str,
    hold: &aterm_update::HoldView,
    now: i64,
) -> (String, Option<String>) {
    let line = format!(
        "aterm {version} \u{b7} {} is held for {} more ({}, try {}) \u{b7} `aterm update \
         retry` tries it now",
        hold.name(),
        left_words(hold.until, now),
        hold.cause.words(),
        hold.tries
    );
    let why = hold.why.trim();
    (line, (!why.is_empty()).then(|| why.to_string()))
}

/// The line for a live window-start failure of the staged build's bytes: what no start without a
/// window installs, for how long more, why, which try, and the verb that ends it now; how the
/// window saw it fail goes to stderr.
fn start_failed_update_line(
    version: &str,
    failed: &aterm_update::HoldView,
    now: i64,
) -> (String, Option<String>) {
    let line = format!(
        "aterm {version} \u{b7} {} is held for {} more from every start with no window ({}, try \
         {}) \u{b7} `aterm update retry` ends that now",
        failed.name(),
        left_words(failed.until, now),
        failed.cause.words(),
        failed.tries
    );
    let why = failed.why.trim();
    (line, (!why.is_empty()).then(|| why.to_string()))
}

/// `aterm update holds`: this copy's update record, then every other lab Mac's live `upd` row,
/// one line each. Exits 1 only when this copy's record cannot be read.
fn update_holds(verbose: bool) -> ExitCode {
    use aterm_update_core::update_record::UpdateRecord;
    let now = aterm_time::unix_secs().unwrap_or(0);
    let own = match aterm_update::record_path() {
        None => None,
        Some(path) => match UpdateRecord::read(&path) {
            Ok(record) => Some(record),
            Err(error) => {
                eprintln!(
                    "aterm update: the update record {} cannot be read ({error}), so it holds \
                     nothing",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
        },
    };
    match &own {
        Some(Some(record)) => println!("{}", record_line("this copy", record, now, verbose)),
        Some(None) => println!("this copy: no update record yet (it has not checked)"),
        None => println!("this copy: keeps no update record"),
    }
    let Some(rows) = aterm_update::lab::rows() else {
        println!("no lab here: other Macs' records are not read");
        return ExitCode::SUCCESS;
    };
    #[cfg(unix)]
    let name = lab_machine_names();
    #[cfg(not(unix))]
    let name = |uuid: &str| format!("uuid:{}", uuid.get(..8).unwrap_or(uuid));
    for row in rows {
        let who = if row.own {
            format!(
                "{} (this Mac, as the lab last heard it)",
                name(&row.machine)
            )
        } else {
            name(&row.machine)
        };
        println!("{}", record_line(&who, &row.record, now, verbose));
    }
    ExitCode::SUCCESS
}

/// How `aterm update holds` names a lab Mac by its Commons uuid: its registry label where one is
/// bound, else `uuid:<first 8 hex>`.
#[cfg(unix)]
fn lab_machine_names() -> impl Fn(&str) -> String {
    let view = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute())
        .zip(aterm_types::dirs::aterm_config_path())
        .and_then(|(home, config)| {
            aterm_db::alab::Registry::at(aterm_secrets::client::Paths::for_home(&home))
                .lab(&config, None)
                .ok()
                .flatten()
        })
        .and_then(|read| match read {
            aterm_db::alab::LabRead::Projected(view, _) => Some(*view),
            _ => None,
        })
        .unwrap_or_default();
    move |uuid| alab_seen::mac(&view, uuid)
}

/// One update record as `aterm update holds` prints it: who, what it runs, what it was offered,
/// its hold (live or lapsed) and, with `verbose`, the trial and how the last hold ended.
fn record_line(
    who: &str,
    record: &aterm_update_core::update_record::UpdateRecord,
    now: u64,
    verbose: bool,
) -> String {
    use std::fmt::Write as _;
    let mut line = if record.running == 0 {
        format!("{who}: no build noted yet")
    } else {
        format!(
            "{who}: runs build {}{}",
            record.running,
            if record.proven { "" } else { " (not proven)" }
        )
    };
    if let Some(offered) = record.offered.filter(|offered| *offered > record.running) {
        let _ = write!(line, ", offered build {offered}");
    }
    match &record.hold {
        Some(hold) if hold.live(now) => {
            let held = hold.name();
            let _ = write!(
                line,
                ", holds {held} for {} more ({}, try {})",
                left_words(hold.until, i64::try_from(now).unwrap_or(i64::MAX)),
                hold.cause.words(),
                hold.tries
            );
            if verbose && !hold.why.is_empty() {
                let _ = write!(line, ": {}", hold.why);
            }
        }
        Some(hold) => {
            let _ = write!(line, ", hold on build {} lapsed", hold.build);
        }
        None => line.push_str(", holds nothing"),
    }
    if let Some(failed) = record.live_start_failure(now) {
        let _ = write!(
            line,
            "; a window here saw {} fail to start, so a background start waits {} more",
            failed.name(),
            left_words(failed.until, i64::try_from(now).unwrap_or(i64::MAX))
        );
    }
    if verbose {
        if let Some(trial) = &record.trial {
            let _ = write!(
                line,
                "; trying build {} ({} missed)",
                trial.build, trial.misses
            );
        }
        if let Some(last) = &record.last {
            use aterm_update_core::update_record::EndHow;
            let how = match last.how {
                EndHow::Started => "it started",
                EndHow::Newer => "a newer build replaced it",
                EndHow::Expired => "it lapsed",
                EndHow::Peer => "it started on another Mac",
                EndHow::Person => "it was retried",
            };
            let _ = write!(line, "; the last hold ended: {how}");
        }
    }
    line
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

// How long before `now` the instant `at` was: the one copy a fleet reader of a peer's last check
// shares.
use aterm_types::update_standing::ago_words;

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
        if st.linux_install_waits_for_a_start() {
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
        .map_or(0, |staged| aterm_update::staged_tries(staged, lane));
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
            if report.last_failure.is_empty() && !report.last_refusal_detail.is_empty() {
                println!("  handoff detail: {}", report.last_refusal_detail);
            }
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
        && (scan
            .iter()
            .any(|a| matches!(a.to_str(), Some("--window" | "--headless" | "--diagnose")))
            || !stdin_tty)
}

fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

fn front_door_verbs() -> Vec<&'static str> {
    let mut verbs =
        Vec::with_capacity(1 + aterm_cli::Verb::ALL.len() + aterm_cli::DIAG_COMMANDS.len());
    verbs.push("help");
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

/// What a Proton mailbox the harness opened for a sign-in code tells the lab registry.
#[cfg(unix)]
mod alab_mailbox;
/// What a tab's CLI was observed to run on tells the lab registry (`account-obs`).
#[cfg(unix)]
mod alab_observed;
/// A Mac's self-registration in the lab registry, woken by the Commons runtime's repair rounds.
#[cfg(unix)]
mod alab_register;
/// `aterm db alab` with the harness's reading of the account sheet.
#[cfg(unix)]
mod alab_sheet;
/// `aterm lan serve | status`: the root LAN broker and its reader (hidden).
#[cfg(unix)]
mod lan_verb;
#[cfg(unix)]
mod shared_accounts;
#[cfg(unix)]
mod shared_login;

/// The Commons table account claims ride.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod account_claims;

/// The lab journal's sink for the harness's notes.
#[cfg(unix)]
mod alab_journal;

/// The update record's glue to the lab journal's `upd` rows.
#[cfg(unix)]
mod alab_update;

/// The node service's metrics publisher (one-mesh §4.14).
#[cfg(unix)]
mod metrics_publish;

/// LabView's cache: the harness's roster source from the lab registry.
#[cfg(unix)]
mod alab_view;

/// The journal's cache: the harness's account surfaces from the journal window.
#[cfg(unix)]
mod alab_seen;

/// `aterm init --converge`'s live ports.
#[cfg(unix)]
mod converge;

/// Local input changes that make the subscriber's existing converge job due.
#[cfg(unix)]
mod converge_inputs;

/// This Mac's fleet as LAN toolset sharing reads it: the members a download races by default.
#[cfg(unix)]
mod unit_peers;

/// When the Commons runtime and a process's automatic LAN uses may run: while `[swarm] enabled`
/// leaves sharing on.
#[cfg(unix)]
mod commons_lan;
#[cfg(unix)]
mod host_startup;

#[path = "doctor.rs"]
mod doctor;

/// Compiler-provenance probe: `build.rs` mounts it by `#[path]` to stamp [`COMPILED`]'s values; this
/// test-only mount runs the same code against real `-vV` fixtures.
#[cfg(test)]
mod compiler_probe;

#[cfg(all(test, target_os = "macos"))]
mod startup_scope_tests;

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "main_open_probe_alias_boundary_tests.rs"]
mod open_probe_alias_boundary_tests;
