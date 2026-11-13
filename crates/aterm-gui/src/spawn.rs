// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session spawning, PTY reader lifecycle, shell integration setup and child proxy provisioning.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use aterm_core::terminal::{ClipboardAccess, ClipboardOperation, Terminal};
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeTable, EdgeToken, LaunchNonce, Op, SessionId};
use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};

use crate::{
    Session, SessionCtx, Wake, WindowId, control, control_auth, notify, proxy, session_store,
    term_lock,
};

/// The integration loader's load-once guard, which the shipped zsh/bash scripts check and export.
pub(crate) const SHELL_INTEGRATION_LOADED_GUARD: &str = "ATERM_SHELL_INTEGRATION_INSTALLED";

/// The runtime outcome of the last shell-integration preparation (not the advertised capability).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationOutcome {
    /// The loader was written and injected.
    Prepared,
    /// No integration script exists for this shell (named as selected/`$SHELL`).
    UnknownShell(String),
    /// The loader cache could not be written (the `std::io::Error` text).
    WriteFailed(String),
    /// Config `shell_args` ask for a start no integration can stand in for (a command, a
    /// script, POSIX mode, …).
    ArgsBypass(String),
}

static SHELL_INTEGRATION_OUTCOME: std::sync::Mutex<Option<ShellIntegrationOutcome>> =
    std::sync::Mutex::new(None);

/// The recorded runtime outcome; `None` before any integrated spawn was attempted (an
/// `-e <cmd>`-only run has no interactive shell to integrate).
pub(crate) fn shell_integration_outcome() -> Option<ShellIntegrationOutcome> {
    SHELL_INTEGRATION_OUTCOME
        .lock()
        .expect("shell integration outcome lock")
        .clone()
}

static SHELL_INTEGRATION_SEEN: std::sync::Mutex<Vec<ShellIntegrationOutcome>> =
    std::sync::Mutex::new(Vec::new());

fn record_shell_integration_outcome(outcome: ShellIntegrationOutcome) -> bool {
    let first = first_sighting(&SHELL_INTEGRATION_SEEN, &outcome);
    *SHELL_INTEGRATION_OUTCOME
        .lock()
        .expect("shell integration outcome lock") = Some(outcome);
    first
}

fn first_sighting(
    seen: &std::sync::Mutex<Vec<ShellIntegrationOutcome>>,
    outcome: &ShellIntegrationOutcome,
) -> bool {
    let mut seen = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if seen.contains(outcome) {
        return false;
    }
    seen.push(outcome.clone());
    true
}

const AGENT_PRIME_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

static AGENT_PRIME: Mutex<PrimeSchedule> = Mutex::new(PrimeSchedule::AT_LAUNCH);

static AGENT_PRIME_RELEASED: AtomicBool = AtomicBool::new(false);

/// What asks for a primer pass ([`prime_agents_if_due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimeCue {
    /// A windowed aterm's launch, once it holds the sessions it will run and before the first
    /// of them is spawned or adopted.
    Launch,
    /// A session spawn: `adopted` when it re-adopts a running shell (a seamless update's
    /// handoff, the keeper), `spare` when it is a warm spare nobody has asked for yet
    /// ([`crate::spare`]).
    Spawn { adopted: bool, spare: bool },
    /// A warm spare adopted into a tab.
    SpareAdopted,
}

const fn cue_primes(cue: PrimeCue) -> bool {
    match cue {
        PrimeCue::Launch | PrimeCue::SpareAdopted => true,
        PrimeCue::Spawn { adopted, spare } => !adopted && !spare,
    }
}

static HEADLESS_INSTANCE: AtomicBool = AtomicBool::new(false);

/// Record that this process is a headless instance.
pub(crate) fn mark_headless_instance() {
    HEADLESS_INSTANCE.store(true, Ordering::Release);
}

const fn agent_prime_permitted(test_build: bool, headless: bool) -> bool {
    !test_build && !headless
}

/// The result of one auto-prime pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentPrimeOutcome {
    /// `agents_auto_prime = false`.
    Disabled,
    /// `$HOME` is unset or empty.
    NoHome,
    /// This process runs from an app bundle that is not the release `aterm.app` (a dev bundle
    /// from `ship dev-app`, a renamed copy).
    NotReleaseBundle,
    Ran(aterm_primer::AutoPrime),
}

fn due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(started) => now.saturating_duration_since(started) >= AGENT_PRIME_INTERVAL,
    }
}

fn take_pass(cue: PrimeCue, last: &mut Option<Instant>, now: Instant) -> bool {
    if !cue_primes(cue) || !due(*last, now) {
        return false;
    }
    *last = Some(now);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimeHold {
    Holding(Option<PrimeCue>),
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimeSchedule {
    last: Option<Instant>,
    hold: PrimeHold,
}

impl PrimeSchedule {
    const AT_LAUNCH: Self = Self {
        last: None,
        hold: PrimeHold::Holding(None),
    };

    fn admit(&mut self, cue: PrimeCue, now: Instant) -> Option<PrimeCue> {
        if !take_pass(cue, &mut self.last, now) {
            return None;
        }
        match &mut self.hold {
            PrimeHold::Released => Some(cue),
            PrimeHold::Holding(held) => {
                if *held != Some(PrimeCue::Launch) {
                    *held = Some(cue);
                }
                None
            }
        }
    }

    const fn release(&mut self) -> Option<PrimeCue> {
        match std::mem::replace(&mut self.hold, PrimeHold::Released) {
            PrimeHold::Holding(held) => held,
            PrimeHold::Released => None,
        }
    }
}

fn run_agent_prime(
    enabled: bool,
    non_release_bundle: bool,
    home: Option<std::path::PathBuf>,
) -> AgentPrimeOutcome {
    if !enabled {
        AgentPrimeOutcome::Disabled
    } else if non_release_bundle {
        AgentPrimeOutcome::NotReleaseBundle
    } else if let Some(home) = home {
        let pass = aterm_primer::auto_prime(&home);
        if pass.changed() {
            aterm_log::info!("{}", pass.summary);
        }
        for (agent, err) in pass.errors() {
            aterm_log::warn!(
                "agent primer: {agent}: {err} — `aterm agents` names the file; nothing was \
                 written to it"
            );
        }
        AgentPrimeOutcome::Ran(pass)
    } else {
        AgentPrimeOutcome::NoHome
    }
}

fn agent_prime_pass(
    committed: impl FnOnce() -> bool,
    prime: impl FnOnce() -> AgentPrimeOutcome,
) -> Option<AgentPrimeOutcome> {
    committed().then(prime)
}

fn handoff_committed() -> bool {
    aterm_update::wait_while_uncommitted_handoff_candidate(
        aterm_update::UNCOMMITTED_CANDIDATE_HOLD_BOUND,
    );
    let committed = !aterm_update::is_uncommitted_handoff_candidate();
    if !committed {
        aterm_log::warn!(
            "agent primer: this handoff was not committed in time; no agent's file was written"
        );
    }
    committed
}

/// Prime detected coding agents if `cue` starts a pass ([`PrimeSchedule::admit`]).
pub(crate) fn prime_agents_if_due(cue: PrimeCue) {
    // Unit tests reach spawn seams, and headless instances are tests' and agents' private
    // instances: neither may write the `$HOME` it inherited.
    if !agent_prime_permitted(cfg!(test), HEADLESS_INSTANCE.load(Ordering::Acquire)) {
        return;
    }
    let start = AGENT_PRIME
        .lock()
        .expect("agent prime schedule lock")
        .admit(cue, Instant::now());
    if let Some(cue) = start {
        start_agent_prime(cue);
    }
}

/// The launch's first frame is on glass: start the pass held until then (a headless instance,
/// which has none, calls this at its first park).
pub(crate) fn release_held_prime() {
    if AGENT_PRIME_RELEASED.load(Ordering::Acquire)
        || AGENT_PRIME_RELEASED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let held = AGENT_PRIME
        .lock()
        .expect("agent prime schedule lock")
        .release();
    if let Some(cue) = held {
        start_agent_prime(cue);
    }
}

fn start_agent_prime(_cue: PrimeCue) {
    let spawned = std::thread::Builder::new()
        .name("aterm-agent-prime".to_string())
        .spawn(move || {
            let _ = agent_prime_pass(handoff_committed, || {
                run_agent_prime(
                    crate::app_config::agents_auto_prime_setting(),
                    atpkg::hooks::runs_from_non_release_bundle(),
                    aterm_primer::home_dir(),
                )
            });
        });
    if let Err(e) = spawned {
        aterm_log::warn!("agent primer: could not start the installer thread: {e}");
    }
}

type ShellIntegrationSetup = (
    Vec<(String, String)>,
    Option<Vec<String>>,
    [u8; 32],
    aterm_core::shell_integration::ShellType,
);

/// The shell this tab will actually spawn, as the integration layer sees it.
pub(crate) fn detect_spawn_shell(
    shell_hint: Option<&str>,
) -> aterm_core::shell_integration::ShellType {
    spawn_shell_type(shell_hint, |hint| {
        aterm_pty::select_shell_with_origin(hint).0
    })
}

fn spawn_shell_type(
    shell_hint: Option<&str>,
    select: impl FnOnce(Option<&std::ffi::OsStr>) -> std::ffi::OsString,
) -> aterm_core::shell_integration::ShellType {
    let program = select(shell_hint.map(std::ffi::OsStr::new));
    aterm_core::shell_integration::ShellType::detect(&program.to_string_lossy())
}

/// The shell a session runs, as aterm launched it: the one it integrated, else the one it
/// spawns for `shell_hint`; for a shell adopted across a seamless update (`adopted`), the one
/// its process `pid` runs, read once ([`adopted_shell`]).
fn launched_shell(
    adopted: bool,
    pid: i32,
    integrated: Option<aterm_core::shell_integration::ShellType>,
    shell_hint: Option<&str>,
) -> aterm_core::shell_integration::ShellType {
    if adopted {
        return adopted_shell(crate::spare::shell_image(pid), || kept_exec_names(pid));
    }
    integrated.unwrap_or_else(|| detect_spawn_shell(shell_hint))
}

/// The names the kernel keeps for process `pid`'s exec ([`crate::command_ran::kernel_names`]).
#[cfg(unix)]
fn kept_exec_names(pid: i32) -> Vec<String> {
    crate::command_ran::kernel_names(pid)
        .iter()
        .map(str::to_owned)
        .collect()
}

/// No exec name is read off unix.
#[cfg(not(unix))]
fn kept_exec_names(_pid: i32) -> Vec<String> {
    Vec::new()
}

/// The shell an adopted session's process runs: the one its image's path (`image`) names; else,
/// its file gone since its exec (a PowerShell upgrade under a running pwsh: macOS's
/// `proc_pidpath` then names no path, and Linux's link only a ` (deleted)` one), the one a name
/// the kernel keeps for its exec (`kept`: [`crate::command_ran::kernel_names`]) names; `Unknown`
/// where none does, which runs jobs (fail closed).
fn adopted_shell(
    image: Option<std::path::PathBuf>,
    kept: impl FnOnce() -> Vec<String>,
) -> aterm_core::shell_integration::ShellType {
    use aterm_core::shell_integration::ShellType;
    match image {
        Some(image) => ShellType::detect(&image.to_string_lossy()),
        None => kept()
            .iter()
            .map(|name| ShellType::detect(name))
            .find(|shell| *shell != ShellType::Unknown)
            .unwrap_or(ShellType::Unknown),
    }
}

/// Whether `shell` runs each command as a job of its own ([`crate::command_ran::Ran::runs_jobs`]):
/// every shell but pwsh, which runs its children in its own group; one aterm does not know is
/// taken to, so an exec of its group never stands in for a command (fail closed).
const fn shell_runs_jobs(shell: aterm_core::shell_integration::ShellType) -> bool {
    !matches!(shell, aterm_core::shell_integration::ShellType::PowerShell)
}

#[cfg(windows)]
fn tab_shell_env(exec: bool, shell_override: Option<&str>) -> Option<(String, String)> {
    if exec {
        return None;
    }
    let (program, _) =
        aterm_pty::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new));
    Some((
        aterm_types::domain::ENV_TAB_SHELL.to_string(),
        program.to_string_lossy().into_owned(),
    ))
}

#[cfg(any(windows, test))]
fn colorfgbg_env(appearance: Option<aterm_types::Appearance>) -> (String, String) {
    let value = match appearance {
        Some(aterm_types::Appearance::Light) => "0;15",
        Some(aterm_types::Appearance::Dark) | None => "15;0",
    };
    ("COLORFGBG".to_string(), value.to_string())
}

fn prepare_shell_integration(
    shell_hint: Option<&str>,
    shell_args: Option<&[String]>,
) -> Option<ShellIntegrationSetup> {
    use aterm_core::shell_integration as si;
    let shell = detect_spawn_shell(shell_hint);
    let mut injection = match si::prepare(shell) {
        Ok(Some(injection)) => injection,
        Ok(None) => {
            let name = selected_shell_name(shell_hint);
            let outcome = ShellIntegrationOutcome::UnknownShell(name.clone());
            if record_shell_integration_outcome(outcome) {
                aterm_log::warn!(
                    "shell integration is not available for {name}: command blocks, cwd \
                     tracking and the ALab tools' PATH hook (shell.d) will not work in \
                     this shell — `aterm --diagnose` shows the runtime state"
                );
            }
            return None;
        }
        Err(e) => {
            let outcome = ShellIntegrationOutcome::WriteFailed(e.to_string());
            if record_shell_integration_outcome(outcome) {
                aterm_log::warn!(
                    "shell integration loader could not be written ({e}): command blocks, \
                     cwd tracking and the ALab tools' PATH hook (shell.d) will not work — \
                     `aterm --diagnose` shows the runtime state"
                );
            }
            return None;
        }
    };
    if let si::ArgsMerge::Bypass(why) =
        si::merge_shell_args(shell, &mut injection, shell_args.unwrap_or_default())
    {
        // Said once per sentence per process; every tab still records it.
        if record_shell_integration_outcome(ShellIntegrationOutcome::ArgsBypass(why.clone())) {
            aterm_log::warn!(
                "shell integration is off: {why} — command blocks, cwd tracking and the ALab \
                 tools' PATH hook (shell.d) will not work in this shell; `aterm --diagnose` \
                 shows the runtime state"
            );
        }
        return None;
    }
    record_shell_integration_outcome(ShellIntegrationOutcome::Prepared);
    let nonce = si::generate_nonce();
    si::augment_with_nonce(&mut injection, nonce.hex());
    Some((
        injection.env_add,
        injection.argv_override,
        nonce.into_parts().0,
        shell,
    ))
}

/// A fresh interactive shell's start, kept on its [`Session`].
#[derive(Clone, Debug)]
pub(crate) struct ShellStart {
    pub(crate) at: Instant,
    pub(crate) name: String,
    /// [`SinkWriter::driven_input`] before anything could write to the PTY.
    pub(crate) input: aterm_session::sink::DrivenInput,
}

fn records_shell_start(adopted: bool, exec_command: Option<&[std::ffi::OsString]>) -> bool {
    !adopted && exec_command.is_none_or(<[std::ffi::OsString]>::is_empty)
}

/// The shell a fresh spawn runs, named for a diagnosis.
pub(crate) fn selected_shell_name(shell_hint: Option<&str>) -> String {
    if let Some(hint) = shell_hint.filter(|s| !s.is_empty()) {
        return hint.to_string();
    }
    #[cfg(unix)]
    {
        aterm_pty::select_shell_with_origin(None)
            .0
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(unix))]
    std::env::var("SHELL").unwrap_or_else(|_| "the default shell".to_string())
}

/// The interactive shell a fresh spawn runs, resolved the way the spawn resolves it.
pub(crate) fn interactive_shell_program(shell_override: Option<&str>) -> std::path::PathBuf {
    aterm_pty::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new))
        .0
        .into()
}

/// The name [`interactive_shell_program`] goes by.
pub(crate) fn interactive_shell_name(shell_override: Option<&str>) -> String {
    interactive_shell_program(shell_override)
        .file_name()
        .map_or_else(|| "shell".to_string(), |n| n.to_string_lossy().into_owned())
}

/// A shell started in place of one that cannot start ([`substitute_shell`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellSubstitute {
    /// The program started instead, handed to the spawn as its `shell_override`.
    pub(crate) program: String,
    /// The line the pane shows above the substitute's first prompt.
    pub(crate) notice: String,
    /// `SHELL` for the child when the broken shell came from `$SHELL`, which it would inherit.
    pub(crate) shell_env: Option<(String, String)>,
}

#[cfg(unix)]
fn substitute_shell(
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
) -> Option<ShellSubstitute> {
    let (program, origin) =
        aterm_pty::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new));
    let why = aterm_pty::shell_cannot_start(&program, origin)?;
    let (instead, by) = aterm_pty::fallback_shell(&program)?;
    let instead = instead.into_string().ok()?;
    let which = match by {
        aterm_pty::ShellOrigin::Passwd => "your login shell",
        _ => "the system shell",
    };
    let without = if shell_args.is_some_and(|args| !args.is_empty()) {
        ", without `shell_args`"
    } else {
        ""
    };
    let substitute = ShellSubstitute {
        notice: format!("[aterm: {why}; started {instead}, {which}, instead{without}]"),
        shell_env: (origin == aterm_pty::ShellOrigin::Env)
            .then(|| ("SHELL".to_string(), instead.clone())),
        program: instead,
    };
    crate::logging::stderr_line!("aterm-gui: {}", substitute.notice);
    Some(substitute)
}

const fn shell_to_start<'a>(
    substitute: Option<&'a ShellSubstitute>,
    shell_override: Option<&'a str>,
    shell_args: Option<&'a [String]>,
) -> (Option<&'a str>, Option<&'a [String]>) {
    match substitute {
        Some(sub) => (Some(sub.program.as_str()), None),
        None => (shell_override, shell_args),
    }
}

#[cfg(not(unix))]
fn substitute_shell(
    _shell_override: Option<&str>,
    _shell_args: Option<&[String]>,
) -> Option<ShellSubstitute> {
    None
}

fn say_substitute(term: &Mutex<Terminal>, sub: &ShellSubstitute) {
    let mut line: String = sub.notice.chars().filter(|c| !c.is_control()).collect();
    line.push_str("\r\n");
    term_lock(term).process(line.as_bytes());
}

#[cfg(all(test, unix))]
#[path = "spawn_substitute_shell_tests.rs"]
mod substitute_shell_tests;

#[cfg(test)]
#[path = "spawn_records_shell_start_tests.rs"]
mod records_shell_start_tests;

/// Everything `spawn_session` needs to stand up a new tab's shell session, captured at startup.
#[derive(Clone)]
pub(crate) struct SessionFactory {
    pub(crate) spawn_cap: aterm_cap::Cap<aterm_cap::effects::Spawn>,
    pub(crate) sandbox_cap: aterm_cap::Cap<aterm_sandbox::Sandbox>,
    /// Terminal-identity env (term/colorterm/lang/…) shared by every tab.
    pub(crate) env_add: Vec<(String, String)>,
    /// `-e <cmd>`: run this instead of `$SHELL` (no shell integration), as the OS's argv gave it
    /// ([`crate::cli::Cli::exec_command`]).
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    /// A drive's plain shell must use its explicit folder, never a fallback.
    pub(crate) require_cwd: bool,
    /// Config `shell` / `--shell`.
    pub(crate) shell_override: Option<String>,
    /// Config `shell_args`: extra argv passed to `shell_override` (e.g. `-l -i`).
    pub(crate) shell_args: Option<Vec<String>>,
    /// `-d <dir>`: working directory for every tab's shell.
    pub(crate) cwd: Option<String>,
    /// OS-sandbox wrap (macOS Seatbelt sbpl).
    pub(crate) sandbox_wrap: Option<String>,
    /// Engine config (scrollback/cursor/theme/palette) applied to each tab's `Terminal`.
    pub(crate) terminal_config: Option<aterm_core::config::TerminalConfig>,
    /// Whether shell spawns may inject OSC 133/633 integration, with a fresh nonce per tab.
    /// A direct command never receives it, even when this policy permits worker shells.
    pub(crate) integrate: bool,
    /// Latency epoch shared across tabs.
    pub(crate) lat_epoch: Instant,
    /// The event loop's turn state ([`LoopIdle`]), stamped by the main thread and read by every
    /// tab's PTY reader ([`gated_output_wake`]).
    pub(crate) loop_idle: Arc<LoopIdle>,
    /// Desktop-notification channel shared by every tab (bounded, `notify::NOTIFY_QUEUE_CAP`).
    pub(crate) notify_tx: std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    /// Config `allow_kitty_file_transfer` (default false).
    pub(crate) allow_kitty_file_transfer: bool,
    /// Config `temporal_recording` (default [`crate::app_config::DEFAULT_TEMPORAL_RECORDING`]).
    pub(crate) temporal_recording: bool,
    /// Config `rewind_budget_mb`, in bytes: one recording's retained-byte budget.
    pub(crate) rewind_budget_bytes: usize,
    /// Overlap handoff (incoming side).
    pub(crate) defer_adopted_readers: bool,
    /// A tab spawned off the event loop (`tab_spawn`).
    pub(crate) reader_after_install: bool,
    /// The foreground-group probe each PTY gather samples for the foreground handback and the
    /// status sweep reads for the group in front (`App::observe_session_statuses`).
    pub(crate) fg_probe: fn(i32) -> i32,
    /// Whether a foreground group is gone: the liveness half of the foreground handback (a
    /// stopped job, or `gdb -tui` handing the terminal to its inferior, still runs and keeps its
    /// modes).
    pub(crate) fg_gone: crate::foreground_handback::FgGone,
    /// Whether the shell `pid` runs a program it started at or after a wall-clock instant (unix
    /// epoch ms) where the foreground group cannot show it.
    pub(crate) program_probe: fn(i32, u64) -> bool,
    /// The kernel's name for the leader of foreground group `pgid` when it is alone in its
    /// group: a file citation asks it whether the shell in front is the one a person types into
    /// (`path_link::runs_here`).
    pub(crate) lone_leader: fn(i32) -> Option<String>,
    /// Whether the leader of foreground group `pgid` has handed the terminal to another program
    /// in its group: a bare Ctrl-r/Ctrl-s asks it to decide whether an agent's composer owns the
    /// key (`App::front_agent_composer`), and a generic licence boundary whether it keeps the
    /// glow's hand on the composer's row (`App::boundary_keeps_hand`).
    pub(crate) handed_terminal: fn(i32) -> bool,
    /// The image of foreground group `pgid`'s leader (card engine §3.4), read by each PTY gather
    /// at a foreground edge (given no last image) and by the parse stage's byte arm (given the
    /// group's last, which the read keeps for an unchanged process).
    pub(crate) edge_image: crate::holder_image::ReadImage,
    /// The edged inbox (card engine §3.5).
    pub(crate) edge_inbox: crate::holder_image::EdgeInbox,
    /// Whether each PTY reader runs the byte-driven arm (card engine §3.6).
    pub(crate) byte_arm: bool,
    /// Each PTY reader's process watch on its shell (card engine §3.3), made at attach.
    #[cfg(unix)]
    pub(crate) proc_watch: fn(i32) -> Option<aterm_uds::procwatch::ProcWatch>,
    /// How a PTY reader with no process watch reads the leader of the group in front while a
    /// block runs ([`crate::fg_sample::FgSampler`]; `None`: it samples nothing, the test App's
    /// inert arm).
    #[cfg(unix)]
    pub(crate) fg_sight: Option<crate::fg_sample::SightSeam>,
    /// Whether a PTY reader with no process watch, its sampler running, asks Linux's proc
    /// connector for its session's `exec`s ([`crate::exec_edges::wire_connector`]; the test App's
    /// inert arm asks none).
    #[cfg(unix)]
    pub(crate) proc_connector: crate::exec_edges::ConnectorSeam,
    /// The birth of a process that has exec'd since its fork (`crate::fg_sample::execd_start`):
    /// a PTY reader with no process watch hears the image of a sampled edge's leader only once
    /// it has with that same birth ([`sampled_edge_image`]), and a restore notes a pwsh child
    /// only then, placed by that birth ([`adopt_restored_holder`]; the test App's inert arm reads
    /// none).
    #[cfg(unix)]
    pub(crate) leader_execd: fn(i32) -> Option<u64>,
    /// The shell's live children, read after a restore for the groups its carried stopped
    /// drawers lead and, where the shell runs its commands in its own group (pwsh), for the
    /// running command ([`shell_children`]).
    #[cfg(unix)]
    pub(crate) shell_children: fn(i32) -> Vec<i32>,
    /// How a Windows PTY reader lists its session's job for the programs its commands ran
    /// ([`crate::command_ran::RanJob`]; `None`: it lists nothing, the test App's inert arm).
    #[cfg(windows)]
    pub(crate) job_seam: Option<crate::command_ran::JobSeam>,
    /// Whether each PTY reader runs the dev `card-seen` probe
    /// ([`crate::holder_image::card_probe_requested`]).
    pub(crate) card_probe: bool,
    /// Overlap handoff (incoming side), armed like [`Self::defer_adopted_readers`].
    pub(crate) rekey_deferred: Option<Arc<crate::shell_rekey::Deferred>>,
    /// A warm spare's spawn (`crate::spare`), set only on the `aterm-spare` thread's factory
    /// copy, like [`Self::reader_after_install`].
    pub(crate) spare: Option<crate::spare::SpareSpawn>,
    /// The light/dark side of the theme this spawn's session will draw, where it draws one of
    /// its own (`App::spawn_shell_appearance`).
    #[cfg(windows)]
    pub(crate) shell_appearance: Option<aterm_types::Appearance>,
}

impl SessionFactory {
    /// Whether this spawn runs an interactive shell whose integration is permitted.
    pub(crate) fn integrates_shell(&self) -> bool {
        self.integrate && records_shell_start(false, self.exec_command.as_deref())
    }

    /// What a spare spawned by this factory shares with a cold tab spawned by it now
    /// (`crate::spare`'s gate G4).
    pub(crate) fn spawn_fingerprint(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let Self {
            spawn_cap: _,
            sandbox_cap: _,
            env_add,
            exec_command,
            require_cwd,
            shell_override,
            shell_args,
            cwd,
            sandbox_wrap,
            terminal_config: _,
            integrate,
            lat_epoch: _,
            loop_idle: _,
            notify_tx: _,
            allow_kitty_file_transfer,
            temporal_recording,
            rewind_budget_bytes,
            defer_adopted_readers: _,
            reader_after_install: _,
            fg_probe: _,
            fg_gone: _,
            program_probe: _,
            lone_leader: _,
            handed_terminal: _,
            edge_image: _,
            edge_inbox: _,
            byte_arm: _,
            #[cfg(unix)]
                proc_watch: _,
            #[cfg(unix)]
                fg_sight: _,
            #[cfg(unix)]
                proc_connector: _,
            #[cfg(unix)]
                leader_execd: _,
            #[cfg(unix)]
                shell_children: _,
            #[cfg(windows)]
                job_seam: _,
            card_probe: _,
            rekey_deferred: _,
            spare: _,
            #[cfg(windows)]
                shell_appearance: _,
        } = self;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "aterm-spawn-fingerprint-v1".hash(&mut h);
        #[cfg(windows)]
        self.shell_colorfgbg().hash(&mut h);
        env_add.hash(&mut h);
        exec_command.hash(&mut h);
        require_cwd.hash(&mut h);
        shell_override.hash(&mut h);
        shell_args.hash(&mut h);
        cwd.hash(&mut h);
        sandbox_wrap.hash(&mut h);
        integrate.hash(&mut h);
        temporal_recording.hash(&mut h);
        rewind_budget_bytes.hash(&mut h);
        allow_kitty_file_transfer.hash(&mut h);
        aterm_containment::mode_or_containment()
            .level()
            .hash(&mut h);
        daily_driver_limits().open_files.hash(&mut h);
        // A spare spawned before the git-and-friends shim was wanted (or after) has the PATH of
        // that moment, not the one a cold tab gets now.
        crate::clt_shim::wanted_dir().hash(&mut h);
        static LAUNCH_ENV: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        LAUNCH_ENV
            .get_or_init(|| {
                let mut env = std::collections::hash_map::DefaultHasher::new();
                let mut vars: Vec<_> = std::env::vars_os().collect();
                vars.sort();
                vars.hash(&mut env);
                env.finish()
            })
            .hash(&mut h);
        h.finish()
    }

    /// The `COLORFGBG` this factory's shell starts with ([`colorfgbg_env`]).
    #[cfg(windows)]
    pub(crate) fn shell_colorfgbg(&self) -> (String, String) {
        colorfgbg_env(
            self.shell_appearance
                .or_else(|| self.terminal_config.as_ref().and_then(|tc| tc.color_scheme)),
        )
    }
}

/// The commit latch a new session holds.
pub(crate) fn adopted_handoff_commit(
    adopted: bool,
    recovered: bool,
    rekey_deferred: Option<&Arc<crate::shell_rekey::Deferred>>,
) -> Option<Arc<crate::shell_rekey::Deferred>> {
    rekey_deferred.filter(|_| adopted && !recovered).cloned()
}

/// Whether `s` is a session id as [`SessionId::generate`] makes one: `s-` + 20 lower-case hex.
pub(crate) fn is_valid_session_id(s: &str) -> bool {
    s.len() == 22 && aterm_types::control_request::Sid::parse(s).is_some()
}

/// Parse an injected root identity from the recursion env values.
pub(crate) fn parse_injected_identity(
    sid: Option<&str>,
    nonce_hex: Option<&str>,
) -> Option<(SessionId, LaunchNonce)> {
    let sid = sid?;
    if !is_valid_session_id(sid) {
        return None;
    }
    let nonce = LaunchNonce::from_hex(nonce_hex?)?;
    Some((SessionId::new(sid), nonce))
}

fn adopt_injected_identity() -> Option<(SessionId, LaunchNonce)> {
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_SESSION_ID};
    let sid = std::env::var(ENV_SESSION_ID).ok();
    let nonce = std::env::var(ENV_LAUNCH_NONCE).ok();
    adopt_injected_identity_in(
        control_auth::socket_dir().as_deref(),
        sid.as_deref(),
        nonce.as_deref(),
        crate::session_log::Epochs::process(),
    )
}

fn adopt_injected_identity_in(
    dir: Option<&std::path::Path>,
    sid: Option<&str>,
    nonce_hex: Option<&str>,
    epochs: &crate::session_log::Epochs,
) -> Option<(SessionId, LaunchNonce)> {
    let (sid, nonce) = parse_injected_identity(sid, nonce_hex)?;
    let dir = dir?;
    if !crate::identity_claim::claim_for_adoption(dir, &sid) {
        return None;
    }
    if let Some(ep) = crate::identity_claim::recorded_epoch_in(dir, &sid) {
        epochs.note(ep);
    }
    Some((sid, nonce))
}

#[cfg(test)]
#[path = "spawn_injected_identity_adoption_tests.rs"]
mod injected_identity_adoption_tests;

/// The identity and capability tokens a parent mints for one child.
#[derive(Clone)]
pub(crate) struct ChildProvision {
    pub(crate) child_sid: SessionId,
    pub(crate) child_nonce: LaunchNonce,
    pub(crate) read: EdgeToken,
    pub(crate) write: EdgeToken,
    pub(crate) signal: EdgeToken,
}

/// The child's view of who spawned it.
pub(crate) fn provision_child_identity_env(parent_sid: &SessionId) -> Vec<(String, String)> {
    provision_child_identity_env_with(
        parent_sid,
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("STY").ok().as_deref(),
    )
}

/// Pure assembly of [`provision_child_identity_env`] for explicit `$TMUX` and `$STY` inputs.
pub(crate) fn provision_child_identity_env_with(
    parent_sid: &SessionId,
    tmux: Option<&str>,
    sty: Option<&str>,
) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_PARENT_SESSION_ID;
    vec![
        (
            ENV_PARENT_SESSION_ID.to_string(),
            parent_sid.as_str().to_string(),
        ),
        (
            aterm_types::domain::ENV_MUX_BASE.to_string(),
            format!("{}|{}", tmux.unwrap_or(""), sty.unwrap_or("")),
        ),
    ]
}

/// The controller-spawn observation hint.
pub(crate) fn provision_observe_env(origin_sid: &SessionId) -> Vec<(String, String)> {
    use aterm_types::domain::ENV_OBSERVE_SESSION_ID;
    vec![(
        ENV_OBSERVE_SESSION_ID.to_string(),
        origin_sid.as_str().to_string(),
    )]
}

/// Mint a fresh child identity and the three per-op capability edges (read/write/signal) this
/// session grants over the child it is about to host.
pub(crate) fn provision_child_recursion_env() -> (Vec<(String, String)>, ChildProvision) {
    use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_SESSION_ID};
    let prov = ChildProvision {
        child_sid: SessionId::generate(),
        child_nonce: LaunchNonce::generate(),
        read: EdgeToken::generate(),
        write: EdgeToken::generate(),
        signal: EdgeToken::generate(),
    };
    let env = vec![
        (
            ENV_SESSION_ID.to_string(),
            prov.child_sid.as_str().to_string(),
        ),
        (ENV_LAUNCH_NONCE.to_string(), prov.child_nonce.to_hex()),
    ];
    (env, prov)
}

fn append_edge_token_channel(
    env: &mut Vec<(String, String)>,
    prov: &ChildProvision,
    sock_dir: Option<&Path>,
) -> Option<proxy::EdgeFile> {
    let sock_dir = sock_dir?;
    let Some(file) = proxy::write_edge_tokens(
        sock_dir,
        &prov.child_sid,
        &prov.read.to_hex(),
        &prov.write.to_hex(),
        &prov.signal.to_hex(),
    ) else {
        aterm_log::warn!(
            "could not write the edge-token file under {}: the new shell's nested aterm \
             ({}) gets no parent edges",
            sock_dir.join("edges").display(),
            prov.child_sid.as_str()
        );
        return None;
    };
    env.push((
        aterm_types::domain::ENV_EDGE_TOKENS.to_string(),
        file.path().to_string_lossy().into_owned(),
    ));
    Some(file)
}

/// The whole recursion provisioning for one child.
pub(crate) fn provision_child_proxy(
    sock_dir: Option<&Path>,
) -> (Vec<(String, String)>, proxy::ChildProxy) {
    let (env, _, edge_file) = crate::spawn_prep::take_provision(sock_dir)
        .unwrap_or_else(|| provision_child_files(sock_dir));
    (env, proxy::ChildProxy::new(edge_file))
}

#[cfg(test)]
fn provision_child_proxy_from(
    (mut env, prov): (Vec<(String, String)>, ChildProvision),
    sock_dir: Option<&Path>,
) -> (Vec<(String, String)>, proxy::ChildProxy) {
    let edge_file = append_edge_token_channel(&mut env, &prov, sock_dir);
    (env, proxy::ChildProxy::new(edge_file))
}

/// The files half of [`provision_child_proxy`].
pub(crate) fn provision_child_files(
    sock_dir: Option<&Path>,
) -> (
    Vec<(String, String)>,
    ChildProvision,
    Option<proxy::EdgeFile>,
) {
    let (mut env, prov) = provision_child_recursion_env();
    let edge_file = append_edge_token_channel(&mut env, &prov, sock_dir);
    (env, prov, edge_file)
}

/// Insert the parent-preminted edges from the injected env values into a child-side
/// [`EdgeTable`], binding each to the child's `self_id` (dst) and `nonce`.
pub(crate) fn install_parent_edges(
    table: &mut EdgeTable,
    self_id: &SessionId,
    nonce: &LaunchNonce,
    parent_sid: Option<&str>,
    read_hex: Option<&str>,
    write_hex: Option<&str>,
    signal_hex: Option<&str>,
) -> usize {
    let Some(parent) = parent_sid.filter(|s| is_valid_session_id(s)) else {
        return 0;
    };
    let src = SessionId::new(parent);
    let mut n = 0;
    for (hex, op) in [
        (read_hex, Op::ReadScreen),
        (write_hex, Op::WriteInput),
        (signal_hex, Op::Signal),
    ] {
        if let Some(tok) = hex.and_then(EdgeToken::from_hex)
            && table.insert(tok, src.clone(), self_id.clone(), op, *nonce)
        {
            n += 1;
        }
    }
    n
}

fn register_injected_parent_edges(ctx: &SessionCtx) {
    use aterm_types::domain::{ENV_EDGE_TOKENS, ENV_PARENT_SESSION_ID};
    let parent = std::env::var(ENV_PARENT_SESSION_ID).ok();
    if parent.is_none() {
        return;
    }
    let (read, write, signal) = std::env::var(ENV_EDGE_TOKENS)
        .ok()
        .and_then(|path| proxy::read_edge_tokens(&path))
        .map_or((None, None, None), |(r, w, s)| (Some(r), Some(w), Some(s)));
    let mut table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
    let n = install_parent_edges(
        &mut table,
        &ctx.self_id,
        &ctx.nonce,
        parent.as_deref(),
        read.as_deref(),
        write.as_deref(),
        signal.as_deref(),
    );
    if n < 3 {
        crate::logging::stderr_line!(
            "aterm: ATERM_PARENT_SESSION_ID set but recorded only {n}/3 parent edges — \
             some ops have no authority (malformed/duplicate/partial edge tokens)"
        );
    }
}

/// A PTY master handed across a seamless re-exec and not yet adopted.
pub(crate) struct HandedMaster(i32);

impl HandedMaster {
    /// Take ownership of handed-off master `fd`.
    pub(crate) const fn new(fd: i32) -> Self {
        Self(fd)
    }

    /// The descriptor, still owned by this handle.
    pub(crate) const fn raw(&self) -> i32 {
        self.0
    }

    /// Adopt the master into its session byte sink ([`master_sink`]), re-arming `FD_CLOEXEC`
    /// first: the outgoing process cleared it to survive the exec, and left clear it would leak
    /// into every later subprocess.
    pub(crate) fn into_sink(self) -> Arc<SinkWriter> {
        #[cfg(unix)]
        let _ = aterm_pty::set_cloexec(self.0, true);
        let master = std::mem::ManuallyDrop::new(self).0;
        // SAFETY: `master` is the handed-off descriptor this handle owned solely
        // (`HandedMaster::new` takes ownership and nothing else closes it); the `ManuallyDrop`
        // above gave that ownership up to the sink.
        unsafe { master_sink(master) }
    }
}

impl Drop for HandedMaster {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.0 >= 3 {
            aterm_pty::close_fd(self.0);
        }
    }
}

/// # Safety
///
/// The caller owns `master` outright (an open descriptor on Unix, a ConPTY master key on Windows)
/// and gives that ownership up by this call.
unsafe fn master_sink(master: i32) -> Arc<SinkWriter> {
    // SAFETY: the caller's contract above — `master` is open and solely ours.
    #[cfg(unix)]
    let owned = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(master) };
    #[cfg(windows)]
    let owned = aterm_pty::OwnedMaster::adopt(master);
    Arc::new(SinkWriter::new_owned(owned))
}

/// A live session handed across a seamless-update re-exec.
pub(crate) struct Adopted {
    /// The outgoing session's pool id, so the incoming boot puts this shell back in its pane.
    pub local_id: u64,
    /// The PTY master inherited across the exec, owned until [`spawn_session`] moves it to a sink.
    pub master: HandedMaster,
    /// The (still-running) child shell's pid == pgid.
    pub pid: i32,
    /// The session's restored stable fabric identity (from the handoff manifest).
    pub sid: SessionId,
    /// The session's restored launch nonce.
    pub nonce: LaunchNonce,
    /// Screen carry: the outgoing engine's checkpoint, hydrated before the reader starts so the
    /// window shows the pre-update screen.
    pub checkpoint: Option<aterm_core::terminal::TerminalCheckpoint>,
    /// Control carry (`crate::handoff_carry`).
    pub control: Option<crate::handoff_carry::ControlCarry>,
    /// `checkpoint` is not the program's last screen.
    pub repaint: bool,
    /// This shell predates self-healing sessions (`session_store::predates_path_self_heal`).
    pub frozen_path: bool,
    /// Broadcast opt-ins: the record's validated `topic add` set as `"<topic> <since>"` rows,
    /// re-seeded onto the adopted `SessionFabric` so an update keeps an agent's topics.
    pub topics: Vec<String>,
    /// The process group the outgoing reader last saw in front (`SessionRecord::fg_holder`).
    pub fg_holder: i32,
    /// The record says this shell reads a re-key file at every prompt (`shell_rekey`).
    pub rekey: bool,
    /// The record says this shell runs an integration loader that reads a body pointer at
    /// every prompt (`shell_body`).
    pub loader: bool,
    /// History carry (`crate::handoff_history`).
    pub history: crate::handoff_history::AdoptedHistory,
    /// The handoff record's `"<origin> <reason>"` hold rows, unparsed, one per standing origin
    /// (`SessionRecord::hold_rows`, or a keeper meta's).
    pub holds: Vec<String>,
    /// The fallback title for a checkpoint that carried no title of its own.
    pub title: String,
    /// Another supervisor's lease, the record's `"<holder> <remaining_ms>"` row, unparsed.
    pub supervisor: Option<String>,
    /// The record's keyed escalations (`"<owner> <pct-text>"` attention rows), unparsed, seeded
    /// beside the lease.
    pub attention_owners: Vec<String>,
    /// Whether this build's in-GUI host holds off the session for its adopted-claim grace
    /// (`SessionRecord::claim_grace`).
    pub claim_grace: bool,
    /// The build that handed this shell across (the manifest's `outgoing_build`; `None` from a
    /// producer that wrote none), named by each recorder's handoff gap ([`mark_handoff_gap`]).
    pub outgoing_build: Option<u64>,
    /// The PTY keeper's copy.
    pub keeper: Option<crate::keeper_link::Claim>,
    /// The render debt the outgoing engine owed the alternate screen this session restores onto
    /// (rows moved, drawn columns cut; `SessionRecord::render_debt`), only when non-zero and the
    /// screen was carried.
    pub render_debt: Option<aterm_core::terminal::RenderDebt>,
    /// The mailbox the handoff carried.
    pub fabric: crate::fabric::FabricCarry,
    /// The record's newest timeline id.
    pub timeline_id: u64,
    /// The session log's epoch, node-id counters and seq count the record carried
    /// (`SessionRecord::log_ids`, `SessionRecord::log_seq`).
    pub log: crate::session_log::LogAdopt,
    /// The pair the crash journal image this window takes over at its Commit keys this shell
    /// by, when the outgoing process held an image not its own (`SessionRecord::journal_key`).
    pub journal_key: Option<(u32, u64)>,
    /// The record's task row (`SessionRecord::task`), unparsed.
    pub task: Option<String>,
    /// The record's Autopilot end row (`SessionRecord::autopilot_end`), unparsed.
    pub autopilot_end: Option<String>,
    /// The record's book of declared wakes (`SessionRecord::wakes`), unparsed.
    pub wakes: Option<String>,
    /// The record's confirmation of the session's agent (`SessionRecord::agent_act`), unparsed.
    pub agent_act: Option<String>,
}

/// Seed a seamless update's carried end, or a proven self-claim's carried face, onto an adopted
/// session's timeline before the session is registered
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2.3, item 8).
pub(crate) fn seed_adopted_end(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    row: Option<&str>,
) {
    let Some(told) = row
        .and_then(crate::harness_end_carry::EndCarry::parse)
        .and_then(|carry| {
            carry
                .end
                .map(crate::harness_end_carry::CarriedEnd::told)
                .or(carry.face)
        })
    else {
        return;
    };
    timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .seed_task_face_held(told.0, told.1);
}

/// A seamless update's carried rows onto adopted session `id`, before it is registered: its task
/// record, its Autopilot end, its book of declared wakes and the host's confirmation of its agent
/// (`agent_act`, [`crate::agent_act::adopt_carried`]).
pub(crate) fn seed_adopted_rows(
    ctx: &SessionCtx,
    id: u64,
    task: Option<&str>,
    end: Option<&str>,
    wakes: Option<&str>,
    agent_act: Option<&str>,
) {
    crate::task_record::seed_carried(ctx, task);
    seed_adopted_end(&ctx.timeline, end);
    crate::occupancy_wakes::seed_carried(id, ctx.self_id.as_str(), wakes);
    if let Some(row) = agent_act {
        crate::agent_act::adopt_carried(ctx.self_id.as_str(), row);
    }
}

/// Mark an adopted session's timeline, cast and temporal recorder as starting at the handoff
/// ([`crate::session_timeline::HandoffGap`]), since none of them crosses the update.
pub(crate) fn mark_handoff_gap(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    cast: &std::sync::Mutex<crate::cast::CastRecorder>,
    temporal: &std::sync::Mutex<crate::temporal::TemporalRecorder>,
    from_build: Option<u64>,
    timeline_id: u64,
    log: crate::session_log::LogAdopt,
    keeper: bool,
) {
    let gap = crate::session_timeline::HandoffGap { from_build };
    {
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.note_carried_glass();
        tl.continue_ids(timeline_id);
        let at = tl.slog().now();
        tl.slog_mut().adopt(log, keeper, from_build, at);
        tl.record("handoff");
    }
    cast.lock()
        .unwrap_or_else(|p| p.into_inner())
        .mark_handoff(gap);
    temporal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mark_handoff(gap);
}

/// Restore the broadcast opt-ins and standing halt a seamless update carried onto the adopting
/// session's `SessionFabric`.
pub(crate) fn seed_adopted_fabric(
    fabric: &crate::fabric::SessionFabric,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    topics: &[String],
    holds: &[String],
    carry: &crate::fabric::FabricCarry,
) {
    if !topics.is_empty() {
        fabric.set_topics(crate::fabric::parse_topics(topics));
    }
    fabric.seed_holds(
        crate::fabric::parse_holds(holds.iter().map(String::as_str)),
        timeline,
    );
    fabric.seed_carry(carry, timeline);
}

/// Authorize the adopted shell's nonce on the successor engine after `restore_checkpoint`
/// restored `require_shell_integration_nonce`.
pub(crate) fn authorize_adopted_shell_nonce(
    engine: &mut aterm_core::terminal::Terminal,
    cp: &aterm_core::terminal::TerminalCheckpoint,
    id: u64,
) {
    match cp.shell_integration_nonce {
        Some(nonce) => engine.authorize_shell_integration(nonce.0),
        None if engine.is_require_shell_integration_nonce() => {
            aterm_log::warn!(
                "adopted session {id}: the handoff carried no shell-integration nonce; \
                 its OSC 133/633 marks stay dropped (status integration=degraded)"
            );
        }
        None => {}
    }
}

/// An adopted shell with no checkpoint keeps its marks' requirement standing.
pub(crate) fn hold_adopted_shell_nonce_requirement(
    engine: &mut aterm_core::terminal::Terminal,
    id: u64,
) {
    engine.set_require_shell_integration_nonce(true);
    aterm_log::warn!(
        "adopted session {id}: adopted without a carried screen; its OSC 133/633 marks stay \
         dropped (status integration=degraded)"
    );
}

fn debug_lost_shell_nonce() -> bool {
    aterm_types::dev_seam!("ATERM_DEBUG_LOST_SHELL_NONCE")
        .is_some_and(|v| !v.is_empty() && v != "0")
}

/// Restore an adopted session's engine before its reader starts.
pub(crate) fn hydrate_adopted_engine(
    term: &Mutex<Terminal>,
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    control: Option<crate::handoff_carry::ControlCarry>,
    rekey: Option<&std::path::Path>,
    deferred: Option<&crate::shell_rekey::Deferred>,
    id: u64,
    history: &mut crate::handoff_history::AdoptedHistory,
) {
    let degraded = {
        let mut engine = term_lock(term);
        if let Some(cp) = checkpoint {
            engine.restore_checkpoint(cp);
            history.reserve_in(&mut engine);
            history.anchor_in(&engine);
            authorize_adopted_shell_nonce(&mut engine, cp, id);
        } else {
            hold_adopted_shell_nonce_requirement(&mut engine, id);
        }
        if let Some(mut control) = control {
            if checkpoint.is_none() {
                control.without_differ();
            }
            install_adopted_archive(&mut engine, control, id);
        }
        engine.shell_integration_posture()
            == aterm_core::terminal::ShellIntegrationPosture::Degraded
    };
    let Some(path) = rekey.filter(|_| degraded) else {
        return;
    };
    match deferred {
        Some(queue) => {
            let (pending, nonce) = crate::shell_rekey::mint(path, id);
            term_lock(term).authorize_shell_integration_on_first_mark(nonce);
            queue.hold(pending);
        }
        None => {
            if let Some(nonce) = crate::shell_rekey::hand_over(path, id) {
                term_lock(term).authorize_shell_integration_on_first_mark(nonce);
            }
        }
    }
}

/// The session log an adopted session continues.
pub(crate) fn adopted_log(
    log: &crate::session_log::LogAdopt,
    control: Option<&mut crate::handoff_carry::ControlCarry>,
) -> crate::session_log::LogAdopt {
    let (lines, mirror) = control.map_or((None, None), |c| (c.take_log(), c.take_log_mirror()));
    crate::session_log::LogAdopt {
        lines,
        mirror,
        ..log.clone()
    }
}

/// The adopted log meets its engine.
pub(crate) fn fit_adopted_log(
    term: &Mutex<Terminal>,
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
) {
    let (next_block, started) = {
        let engine = term_lock(term);
        (engine.next_block_id(), engine.started_command_seq())
    };
    timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .slog_fit_engine(next_block, started);
}

/// Install an adopted session's carried archive ([`hydrate_adopted_engine`]), logging rows it
/// refused (`AltArchive::import`: the archive is off here, or the indices reach past it) so a
/// fresh `offscreen` after the update is explained.
pub(crate) fn install_adopted_archive(
    engine: &mut aterm_core::terminal::Terminal,
    control: crate::handoff_carry::ControlCarry,
    id: u64,
) {
    let rows = control.archive.as_ref().map_or(0, |a| a.rows.len());
    let refused = matches!(
        control.install(engine),
        Some(aterm_core::terminal::AltArchiveImport::Refused)
    );
    if refused && rows > 0 {
        aterm_log::info!(
            "adopted session {id}: its carried archive tail ({rows} rows) was not installed \
             (the archive is off here, or the carry's indices reach past any archive)"
        );
    }
}

/// Require session `id`'s shell-integration nonce (authorizing it unless it was lost), then seed its recording when `record`, so the seed keyframe carries the nonce its shell signs marks with.
pub(crate) fn arm_shell_nonce_then_seed(
    term: &Mutex<Terminal>,
    temporal: &std::sync::Mutex<crate::temporal::TemporalRecorder>,
    nonce: Option<([u8; 32], bool)>,
    record: bool,
    id: u64,
) {
    if let Some((nonce, lost)) = nonce {
        let mut t = term_lock(term);
        if !lost {
            t.authorize_shell_integration(nonce);
        }
        t.set_require_shell_integration_nonce(true);
    }
    if record {
        seed_temporal_keyframe(term, temporal, id);
    }
}

/// Take the temporal recorder's first keyframe of session `id`'s engine before its reader runs.
pub(crate) fn seed_temporal_keyframe(
    term: &Mutex<Terminal>,
    temporal: &std::sync::Mutex<crate::temporal::TemporalRecorder>,
    id: u64,
) -> bool {
    let (checkpoint, at) = {
        let engine = term_lock(term);
        match seed_of(&engine, id) {
            Some(seed) => seed,
            None => return false,
        }
    };
    let mut rec = temporal.lock().unwrap_or_else(|p| p.into_inner());
    rec.record_keyframe(checkpoint);
    rec.seed_spine(at);
    true
}

/// The seed keyframe of session `id`'s `engine` and where its spine starts, or `None` (logged) inside a sequence no keyframe carries.
fn seed_of(
    engine: &Terminal,
    id: u64,
) -> Option<(crate::temporal::ReplayKeyframe, crate::temporal::SpinePre)> {
    let Some(checkpoint) = crate::temporal::keyframe_of(engine, usize::MAX) else {
        aterm_log::warn!(
            "temporal: session {id} starts inside a {} no keyframe can carry; its recording is \
             not seeded",
            engine
                .partial_sequence_state()
                .unwrap_or("partial sequence")
        );
        return None;
    };
    Some((checkpoint, crate::temporal::SpinePre::take(engine)))
}

/// Start recording promoted spare `id`'s running reader: [`seed_temporal_keyframe`]'s seed and the reader's queue, both under the terminal's lock, so the reader's next slice is the first recorded.
/// # Errors
/// The shared writer could not be started.
pub(crate) fn arm_promoted(session: &mut Session, id: u64) -> Result<(), String> {
    let tx = crate::temporal::open(&session.ctx.temporal, crate::temporal::QUEUE_CAP)?;
    let ui_tx = tx.clone();
    let drain = tx.drain();
    {
        let engine = term_lock(&session.term);
        let mut rec = session
            .ctx
            .temporal
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match seed_of(&engine, id) {
            Some((checkpoint, at)) => {
                rec.record_keyframe(checkpoint);
                rec.seed_spine(at);
            }
            None => rec.want_keyframe(),
        }
        drop(rec);
        session.temporal_arm.set(tx);
    }
    session.temporal_tx = Some(ui_tx);
    session.temporal_drain = Some(drain);
    crate::temporal::pool().join(id, &session.ctx.temporal);
    Ok(())
}

/// Install the fallback title (`record_title`: [`Adopted::title`], the park's layout title or
/// the handoff record's copy) when the checkpoint carried none, via `Terminal::restore_title`
/// (capped, controls stripped, title epoch bumped).
pub(crate) fn seed_adopted_title(
    term: &Mutex<Terminal>,
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    record_title: &str,
) -> bool {
    if record_title.is_empty() || checkpoint.is_some_and(|cp| cp.title.is_some()) {
        return false;
    }
    term_lock(term).restore_title(&aterm_core::terminal::TitleRepr {
        window: record_title.to_owned(),
        ..aterm_core::terminal::TitleRepr::default()
    });
    true
}

/// The console-host keyboard posture of the PTY behind `master`
/// ([`Terminal::set_console_host_posture`]), keyed on the resize policy the session resizes
/// under ([`crate::app_render::pty_resize_policy`]), so a `cast drift` replay under that policy
/// rebuilds the same engine.
pub(crate) fn apply_pty_keyboard_posture(term: &Mutex<Terminal>, master: i32) {
    let policy = crate::app_render::pty_resize_policy(master);
    let host = aterm_pty::conpty_host(master);
    crate::alt_screen_resync::apply_posture(&mut term_lock(term), policy, host);
}

/// The `rlimit` set a daily-driver (`user`) shell is spawned with.
pub(crate) fn daily_driver_limits() -> aterm_sandbox::Limits {
    #[cfg(unix)]
    let open_files = crate::descriptor_limit::launcher_soft_descriptor_limit();
    #[cfg(not(unix))]
    let open_files = None;
    aterm_sandbox::Limits {
        open_files,
        ..aterm_sandbox::Limits::inherit()
    }
}

/// Stand up one tab's shell session and start its PTY reader.
#[allow(
    clippy::too_many_arguments,
    reason = "threads the id/window/geometry/factory/proxy context plus the per-spawn \
              cwd override and the optional seamless-adopt handle; a wrapper struct would \
              relocate the list, not simplify it"
)]
pub(crate) fn spawn_session(
    id: u64,
    window: WindowId,
    rows: u16,
    cols: u16,
    cell_px: Option<(u16, u16)>,
    factory: &SessionFactory,
    proxy: &dyn WakeTarget,
    cwd_override: Option<&str>,
    observe: Option<&SessionId>,
    task: Option<&crate::task_launch::TaskSeed>,
    mut adopt: Option<Adopted>,
) -> std::io::Result<Session> {
    let spare = factory.spare.as_ref();
    prime_agents_if_due(PrimeCue::Spawn {
        adopted: adopt.is_some(),
        spare: spare.is_some(),
    });
    if adopt.is_none() {
        crate::tab_label::note_this_machine_name();
    }
    let mut keeper_claim = adopt.as_mut().and_then(|adopted| adopted.keeper.take());
    let restore_report = keeper_claim
        .as_mut()
        .and_then(crate::keeper_link::Claim::take_restore);
    let recovered = keeper_claim.is_some();
    let handoff_local_id = adopt.as_ref().map(|adopted| adopted.local_id);
    let journal_key = adopt.as_ref().and_then(|adopted| adopted.journal_key);
    let frozen_path = adopt.as_ref().is_some_and(|adopted| adopted.frozen_path);
    let mut rekey_channel = adopt.as_ref().is_some_and(|adopted| adopted.rekey);
    let carried_rev: Option<String> = adopt
        .as_ref()
        .and_then(|adopted| adopted.checkpoint.as_ref())
        .and_then(|cp| cp.shell_integration_rev.clone());
    let mut body_loader =
        adopt.as_ref().is_some_and(|adopted| adopted.loader) || carried_rev.is_some();
    let adopt_topics: Vec<String> = adopt
        .as_ref()
        .map(|adopted| adopted.topics.clone())
        .unwrap_or_default();
    let adopt_holds: Vec<String> = adopt
        .as_ref()
        .map(|adopted| adopted.holds.clone())
        .unwrap_or_default();
    let adopt_fabric: crate::fabric::FabricCarry = adopt
        .as_ref()
        .map(|adopted| adopted.fabric.clone())
        .unwrap_or_default();
    let adopt_timeline_id: u64 = adopt.as_ref().map_or(0, |adopted| adopted.timeline_id);
    let adopt_log: crate::session_log::LogAdopt = adopt
        .as_ref()
        .map(|adopted| adopted.log.clone())
        .unwrap_or_default();
    let adopt_from_build: Option<u64> = adopt.as_ref().and_then(|adopted| adopted.outgoing_build);
    let adopt_title: String = adopt
        .as_ref()
        .map(|adopted| adopted.title.clone())
        .unwrap_or_default();
    let (adopt_supervisor, adopt_attention): (Option<String>, Vec<String>) = adopt
        .as_ref()
        .map(|adopted| (adopted.supervisor.clone(), adopted.attention_owners.clone()))
        .unwrap_or_default();
    let adopt_task: Option<String> = adopt.as_ref().and_then(|adopted| adopted.task.clone());
    let adopt_end: Option<String> = adopt
        .as_ref()
        .and_then(|adopted| adopted.autopilot_end.clone());
    let adopt_wakes: Option<String> = adopt.as_ref().and_then(|adopted| adopted.wakes.clone());
    let adopt_agent_act: Option<String> =
        adopt.as_ref().and_then(|adopted| adopted.agent_act.clone());
    let substitute = records_shell_start(adopt.is_some(), factory.exec_command.as_deref())
        .then(|| {
            substitute_shell(
                factory.shell_override.as_deref(),
                factory.shell_args.as_deref(),
            )
        })
        .flatten();
    let (shell_override, shell_args) = shell_to_start(
        substitute.as_ref(),
        factory.shell_override.as_deref(),
        factory.shell_args.as_deref(),
    );
    let mut integrated_shell = None;
    let (mut env_add, argv_override, shell_nonce) = if adopt.is_some() {
        (Vec::new(), None, None)
    } else if factory.integrates_shell() {
        match prepare_shell_integration(shell_override, shell_args) {
            Some((si_env, argv_override, nonce, shell)) => {
                integrated_shell = Some(shell);
                let mut env = factory.env_add.clone();
                env.push((SHELL_INTEGRATION_LOADED_GUARD.to_string(), String::new()));
                env.extend(si_env);
                if let Some(pair) = aterm_core::shell_integration::wsl_cwd_env(
                    shell,
                    cwd_override.or(factory.cwd.as_deref()),
                ) {
                    env.push(pair);
                }
                (env, argv_override, Some(nonce))
            }
            None => (factory.env_add.clone(), None, None),
        }
    } else {
        (factory.env_add.clone(), None, None)
    };
    if adopt.is_none() {
        let runs_command = factory
            .exec_command
            .as_deref()
            .is_some_and(|argv| !argv.is_empty());
        crate::default_editor::settle_session(
            &mut env_add,
            crate::default_editor::decides(runs_command, integrated_shell),
            &|key| std::env::var_os(key),
        );
        // While Apple's Command Line Tools are missing the shim directory is wanted
        // (`App::settle_developer_tools_shim`): this shell gets it ahead of `/usr/bin`, named in
        // `ATERM_CLT_SHIM` for its integration to keep it there, and its developer directory in
        // `DEVELOPER_DIR` (design §6.3), decided now, so a tab opened before they exist reaches
        // it and one opened after does not.
        crate::clt_shim::settle_session_path(
            &mut env_add,
            &|key| std::env::var(key).ok(),
            crate::clt_shim::wanted_dir().as_deref(),
            crate::clt_install::developer_tools_ready(),
        );
    }

    if let Some(pair) = substitute.as_ref().and_then(|sub| sub.shell_env.clone()) {
        env_add.push(pair);
    }

    let (self_id, self_nonce, premint) = match &adopt {
        Some(a) => {
            crate::identity_claim::hold_transferred(&a.sid);
            (a.sid.clone(), a.nonce, false)
        }
        None if id == 0 => adopt_injected_identity().map_or_else(
            || (SessionId::generate(), LaunchNonce::generate(), false),
            |(sid, nonce)| (sid, nonce, true),
        ),
        None => (SessionId::generate(), LaunchNonce::generate(), false),
    };
    let lost_nonce = adopt.is_none() && debug_lost_shell_nonce();
    let offers_rekey = adopt.is_none()
        && !lost_nonce
        && integrated_shell.is_some_and(aterm_core::shell_integration::has_rekey_hook);
    let rekey_path = (offers_rekey || rekey_channel)
        .then(|| crate::shell_rekey::path_for(&self_id))
        .flatten();
    if offers_rekey && let Some(path) = &rekey_path {
        env_add.push((
            aterm_core::shell_integration::REKEY_PATH_VAR.to_string(),
            path.to_string_lossy().into_owned(),
        ));
        rekey_channel = true;
    }
    let spare_fifo = match spare {
        Some(_)
            if !(integrated_shell.is_some_and(aterm_core::shell_integration::has_spare_hold)
                && offers_rekey
                && rekey_path.is_some()) =>
        {
            return Err(std::io::Error::other("this shell cannot wait as a spare"));
        }
        Some(_) => Some(crate::spare::SpareFifo::make(&self_id)?),
        None => None,
    };
    let offers_body = adopt.is_none()
        && !lost_nonce
        && integrated_shell.is_some_and(aterm_core::shell_integration::has_body_loader);
    let body_path = (offers_body || body_loader)
        .then(|| crate::shell_body::path_for(&self_id))
        .flatten();
    if offers_body && let Some(path) = &body_path {
        env_add.push((
            crate::shell_body::POINTER_VAR.to_string(),
            path.to_string_lossy().into_owned(),
        ));
        body_loader = true;
    }
    let integration_frozen = adopt.is_some() && !body_loader;
    if adopt.is_none() {
        env_add.extend(provision_child_identity_env(&self_id));
        if let Some(origin) = observe {
            env_add.extend(provision_observe_env(origin));
        }
        if let Some(task) = task {
            let path = task
                .write_for(&self_id)
                .map_err(|e| std::io::Error::new(e.kind(), format!("the task file: {e}")))?;
            env_add.push((
                aterm_types::domain::ENV_TASK_FILE.to_string(),
                path.to_string_lossy().into_owned(),
            ));
        }
        #[cfg(windows)]
        env_add.extend(tab_shell_env(
            factory.exec_command.is_some(),
            factory.shell_override.as_deref(),
        ));
        #[cfg(windows)]
        env_add.push(factory.shell_colorfgbg());
    }
    let child_proxy = if adopt.is_some() {
        None
    } else if factory.exec_command.is_none() {
        let (recursion_env, child) = provision_child_proxy(control_auth::socket_dir().as_deref());
        env_add.extend(recursion_env);
        Some(child)
    } else {
        None
    };

    let adopted = adopt.is_some();
    let shell_started_at =
        records_shell_start(adopted, factory.exec_command.as_deref()).then(Instant::now);
    let carried_fg_holder = adopt.as_ref().map_or(0, |a| a.fg_holder);
    let carried_render_debt = adopt.as_ref().and_then(|a| a.render_debt);
    let mut shell_version = None;
    let (master, sink, pid, adopt_checkpoint, mut adopt_control, adopt_repaint, mut adopt_history) =
        match adopt {
            Some(a) => {
                let master = a.master.raw();
                (
                    master,
                    a.master.into_sink(),
                    a.pid,
                    a.checkpoint,
                    a.control,
                    a.repaint,
                    a.history,
                )
            }
            None => {
                let limits = {
                    use aterm_containment::ContainmentMode as Cm;
                    match aterm_containment::mode_or_containment() {
                        Cm::User => daily_driver_limits(),
                        Cm::Safety => aterm_sandbox::Limits::shell_default(),
                        _ => aterm_sandbox::Limits::contained(),
                    }
                };
                let start =
                    crate::spawn_folder::start_folder(cwd_override.or(factory.cwd.as_deref()));
                if factory.require_cwd
                    && (!cwd_override.is_some_and(|cwd| std::path::Path::new(cwd).is_absolute())
                        || start.fell_back.is_some())
                {
                    return Err(std::io::Error::other(
                        "the explicit shell folder is unavailable",
                    ));
                }
                if spare.is_some() && start.fell_back.is_some() {
                    return Err(std::io::Error::other(crate::spare::FolderFallsBack));
                }
                // A spare's shell reads its hold on the end handed to it here; when this
                // process ends, that read is end-of-file and the shell ends itself.
                #[cfg(unix)]
                let pass = spare_fifo.as_ref().map(|fifo| aterm_pty::PassedFd {
                    fd: fifo.shell_end(),
                    at: aterm_core::shell_integration::spare_fifo::HOLD_FD,
                });
                #[cfg(not(unix))]
                let pass = None;
                crate::watchdog::park_before_fork();
                let spawned = aterm_pty::spawn_shell_with_pid_cell_px(
                    rows,
                    cols,
                    &factory.spawn_cap,
                    &factory.sandbox_cap,
                    &env_add,
                    shell_override,
                    shell_args,
                    argv_override.as_deref(),
                    factory.exec_command.as_deref(),
                    start.cwd.as_deref(),
                    factory.sandbox_wrap.as_deref(),
                    limits,
                    cell_px,
                    pass,
                );
                if let Some(note) = aterm_pty::take_console_host_fallback() {
                    aterm_log::warn!("console host: {note}");
                }
                crate::spawn_prep::poke();
                let aterm_pty::SpawnedShell { master, pid } = spawned?;
                if integrated_shell.is_some() {
                    shell_version = crate::spare::shell_version(pid);
                }
                start.say();
                // SAFETY: `master` is this session's forkpty master, freshly returned and owned
                // solely here; the sink takes it over.
                let sink = unsafe { master_sink(master) };
                (
                    master,
                    sink,
                    pid,
                    None,
                    None,
                    false,
                    crate::handoff_history::AdoptedHistory::default(),
                )
            }
        };
    let shell_start = shell_started_at.map(|at| ShellStart {
        at,
        name: interactive_shell_name(shell_override),
        input: sink.driven_input(),
    });
    let cast = Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
        cols, rows,
    )));
    let temporal = Arc::new(std::sync::Mutex::new({
        let mut rec = crate::temporal::TemporalRecorder::with_budget(factory.rewind_budget_bytes);
        rec.set_replay_config(factory.terminal_config.clone().map(Arc::new));
        rec
    }));
    let byte_fanout = Arc::new(crate::cast::ByteFanout::new());
    let term = Arc::new(Mutex::new(new_live_terminal(
        rows,
        cols,
        factory.terminal_config.as_ref(),
        cell_px,
    )));

    let observed = crate::session_watch::Observed::new(if adopted {
        crate::handoff_carry::adopted_ledger(adopt_control.as_mut())
    } else {
        crate::turn_ledger::TurnLedger::default()
    });
    observed.watchers.bind_frame(&term);
    if let Some(sub) = &substitute {
        say_substitute(&term, sub);
    }

    let ctx = Arc::new(SessionCtx {
        sink: sink.clone(),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
        turn_lease: std::sync::Mutex::new(None),
        self_id,
        nonce: self_nonce,
        cast: cast.clone(),
        temporal: temporal.clone(),
        byte_fanout: byte_fanout.clone(),
        turns: observed.turns,
        meta: Arc::default(),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        screen_moves: Default::default(),
        job_feed: Default::default(),
        ran: {
            // A bare program word is looked up on the PATH the shell was given, where no exec's
            // own environment shows a later one.
            let ran = crate::command_ran::Ran::default();
            ran.runs_jobs(shell_runs_jobs(launched_shell(
                adopted,
                pid,
                integrated_shell,
                shell_override,
            )));
            let given = env_add.iter().rev().find(|(k, _)| k == "PATH");
            ran.spawned_with(
                given
                    .map(|(_, v)| v.clone())
                    .or_else(|| std::env::var("PATH").ok())
                    .as_deref(),
            );
            Arc::new(ran)
        },
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: crate::control::SpawnMark::at_spawn(adopted),
        witness: spare.map_or_else(Arc::default, |s| Arc::clone(&s.link.witness)),
    });
    if !adopted && integrated_shell.is_some() {
        ctx.witness.note_shell(pid, shell_version);
    }
    if let Some(fifo) = spare_fifo {
        ctx.witness.hold_fifo(fifo);
    }
    if id == 0 {
        register_injected_parent_edges(&ctx);
    }
    let adopt_log = adopted_log(&adopt_log, adopt_control.as_mut());
    if adopted {
        mark_handoff_gap(
            &ctx.timeline,
            &ctx.cast,
            &ctx.temporal,
            adopt_from_build,
            adopt_timeline_id,
            adopt_log,
            recovered,
        );
    }
    if premint || adopted {
        let ep = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .log()
            .ep();
        crate::identity_claim::record_epoch(&ctx.self_id, ep, premint);
    }
    seed_adopted_fabric(
        &ctx.fabric,
        &ctx.timeline,
        &adopt_topics,
        &adopt_holds,
        &adopt_fabric,
    );
    crate::session_timeline::seed_carried_claims(
        &ctx.meta,
        &ctx.timeline,
        adopt_supervisor.as_deref(),
        &adopt_attention,
        crate::metrics::now_us(),
    );
    seed_adopted_rows(
        &ctx,
        id,
        adopt_task.as_deref(),
        adopt_end.as_deref(),
        adopt_wakes.as_deref(),
        adopt_agent_act.as_deref(),
    );

    if adopt_checkpoint.is_some() || adopted {
        hydrate_adopted_engine(
            &term,
            adopt_checkpoint.as_ref(),
            adopt_control.take(),
            rekey_path.as_deref().filter(|_| rekey_channel),
            factory.rekey_deferred.as_deref().filter(|_| !recovered),
            id,
            &mut adopt_history,
        );
        adopt_history.watch_input(&ctx.output_echo);
        // The jobs feed holds a restored block that can be running before any reading, even
        // while this session's reader waits to start.
        ctx.job_feed
            .fed(&term_lock(&term), &[], false, Instant::now);
        if adopted {
            fit_adopted_log(&term, &ctx.timeline);
        }
    }
    #[cfg(unix)]
    if adopt_checkpoint.is_some() {
        adopt_restored_holder(
            &term,
            &ctx.timeline,
            &crate::command_ran::RanFeed::new(Arc::clone(&ctx.ran), pid),
            master,
            pid,
            factory.fg_probe,
            factory.edge_image,
            factory.shell_children,
            factory.leader_execd,
        );
        factory.edge_inbox.push(id);
    }
    if let Some(debt) = carried_render_debt {
        seed_adopted_render_debt(&term, &ctx.timeline, debt);
    }
    let _ = seed_adopted_title(&term, adopt_checkpoint.as_ref(), &adopt_title);
    apply_pty_keyboard_posture(&term, master);
    if adopted
        && body_loader
        && let Some(path) = &body_path
    {
        crate::shell_body::hand_over(
            path,
            id,
            carried_rev.as_deref(),
            factory.rekey_deferred.as_deref().filter(|_| !recovered),
        );
    }

    let reach = session_host_reach();
    match spare {
        Some(s) => {
            crate::spare::wire_spare_taint(
                &term,
                &s.link,
                reach,
                factory.allow_kitty_file_transfer,
            );
        }
        None => arm_host_reach(&term, reach, factory, proxy, id, window, &ctx),
    }

    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));

    configure_palette_refusal(&term);

    // A spare records nothing until a tab adopts it (`arm_promoted`).
    let record = factory.temporal_recording && spare.is_none();
    arm_shell_nonce_then_seed(
        &term,
        &temporal,
        shell_nonce.map(|nonce| (nonce, lost_nonce)),
        record,
        id,
    );
    match crate::keeper_link::journal_refusal() {
        #[cfg(unix)]
        None => match crate::crash_restore::SessionJournal::create(
            id,
            ctx.self_id.as_str(),
            &term,
            master,
            factory.terminal_config.as_ref(),
            Arc::clone(&ctx.reset_lane),
        ) {
            Ok(journal) => {
                journal.bind_recovery_count(
                    keeper_claim
                        .as_ref()
                        .and_then(crate::keeper_link::Claim::recovery_count),
                );
                journal.carry_ledger(&ctx.turns);
                journal.start_export(&term, adopted);
                ctx.crash_journal.install(journal);
            }
            Err(why) => {
                aterm_log::warn!(
                    "crash restore: session {id} runs without a journal ({why}); a crash \
                     restores it blank"
                );
                ctx.crash_journal.turn_off(why);
            }
        },
        #[cfg(not(unix))]
        None => {}
        Some(why) => ctx.crash_journal.turn_off(why.to_string()),
    }
    if let Some(report) = restore_report {
        ctx.crash_journal.note_restored(report);
    }

    let output_wake_pending = Arc::new(AtomicU64::new(0));

    let last_output_ns = Arc::new(AtomicU64::new(0));

    let latest_output_activity_ns = Arc::new(AtomicU64::new(0));

    let mut session = Session {
        child_reaped: std::sync::atomic::AtomicBool::new(false),
        teardown_started: std::sync::atomic::AtomicBool::new(false),
        child_exit: std::sync::OnceLock::new(),
        exit_watch: None,
        shell_start,
        pty_closed_at: Arc::default(),
        id,
        term,
        adopt_pulse_hold: Arc::new(AtomicBool::new(false)),
        master,
        pid,
        shell_identity: if adopted {
            aterm_pty::record_adopted_shell(pid, master)
        } else {
            aterm_pty::record_spawned_shell(pid)
        },
        handoff_local_id,
        journal_key,
        frozen_path,
        rekey_channel,
        body_loader,
        integration_frozen,
        ctx,
        child_proxy,
        output_wake_pending,
        last_output_ns,
        output_request_booked_ns: AtomicU64::new(0),
        latest_output_activity_ns,
        wake_wr: -1,
        reader_stop: Arc::new(AtomicBool::new(false)),
        reader_join: None,
        temporal_tx: None,
        temporal_drain: None,
        temporal_arm: Arc::default(),
        reflow_cancel: Arc::new(AtomicBool::new(false)),
        fg_holder: Arc::new(std::sync::atomic::AtomicI32::new(carried_fg_holder)),
        handoff_history: adopted.then_some(adopt_history),
        _pty_census: if spare.is_some() {
            crate::metrics::PtyCensus::spare()
        } else {
            crate::metrics::PtyCensus::counted()
        },
        handoff_commit: adopted_handoff_commit(adopted, recovered, factory.rekey_deferred.as_ref()),
    };
    if adopted {
        session.exit_watch = adopted_exit_watch(&session.shell_identity);
    }

    if !(adopted && factory.defer_adopted_readers && !recovered) && !factory.reader_after_install {
        match spare {
            Some(s) => attach_reader_with_post(&mut session, window, s.link.post(), factory),
            None => attach_reader_with_post(&mut session, window, proxy.wake_post(), factory),
        }
        .map_err(std::io::Error::other)?;
    }

    #[cfg(unix)]
    if adoption_needs_size_pulse(adopt_checkpoint.as_ref(), adopt_repaint) {
        let hold = if adopt_repaint {
            PulseHold::Held
        } else {
            PulseHold::BackToBack
        };
        pulse_adopted_pty(
            master,
            &session.term,
            &session.adopt_pulse_hold,
            cell_px,
            hold,
        );
    }
    #[cfg(unix)]
    if adopt_repaint {
        crate::app_nudge::note_repaint_adoption(id);
    }
    #[cfg(not(unix))]
    let _ = adopt_repaint;

    if spare.is_none() {
        crate::keeper_link::register(&session, keeper_claim);
    }

    Ok(session)
}

/// Whether an adoption pulse holds its transient size ([`pulse_adopted_pty`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PulseHold {
    /// Hold `rows-1` for [`ADOPTION_PULSE_HOLD`].
    Held,
    /// The two halves back to back.
    BackToBack,
}

/// The exit witness of a shell this process did not spawn.
pub(crate) fn adopted_exit_watch(
    identity: &aterm_pty::ShellIdentity,
) -> Option<aterm_uds::exitwatch::ExitWatch> {
    identity.birth?;
    let watch = aterm_uds::exitwatch::ExitWatch::watch(u32::try_from(identity.pid).ok()?)?;
    (identity.verify() || watch.exit_status().is_some()).then_some(watch)
}

/// A `wait(2)`-encoded status as the exit it records.
#[cfg(unix)]
pub(crate) fn child_exit_of_wait_status(status: i32) -> Option<aterm_pty::ChildExit> {
    if libc::WIFEXITED(status) {
        return Some(aterm_pty::ChildExit::Code(libc::WEXITSTATUS(status)));
    }
    if libc::WIFSIGNALED(status) {
        return u8::try_from(libc::WTERMSIG(status))
            .ok()
            .map(aterm_pty::ChildExit::Signal);
    }
    None
}

/// No `wait(2)` encoding off unix, and no exit watch reads one.
#[cfg(not(unix))]
pub(crate) fn child_exit_of_wait_status(_status: i32) -> Option<aterm_pty::ChildExit> {
    None
}

#[cfg(unix)]
fn pulse_adopted_pty(
    master: i32,
    term: &Arc<Mutex<Terminal>>,
    holding: &Arc<AtomicBool>,
    cell_px: Option<(u16, u16)>,
    hold: PulseHold,
) {
    pulse_adopted_pty_with(master, term, holding, cell_px, |restore| {
        if hold == PulseHold::BackToBack {
            restore.run();
            return;
        }
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-adopt-pulse",
            crate::qos::Role::Responsive,
            move || {
                std::thread::sleep(ADOPTION_PULSE_HOLD);
                restore.run();
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("adoption pulse: no hold thread ({error}); ending the pulse at once");
        }
    });
}

/// How long a [`PulseHold::Held`] adoption pulse holds the PTY at `rows-1`.
#[cfg(unix)]
pub(crate) const ADOPTION_PULSE_HOLD: std::time::Duration = std::time::Duration::from_millis(250);

/// [`pulse_adopted_pty`] with the hold injected.
#[cfg(unix)]
pub(crate) fn pulse_adopted_pty_with(
    master: i32,
    term: &Arc<Mutex<Terminal>>,
    holding: &Arc<AtomicBool>,
    cell_px: Option<(u16, u16)>,
    hold: impl FnOnce(PulseRestore),
) {
    use std::os::fd::AsRawFd;
    let (rows, cols, cell_px) = {
        let engine = term_lock(term);
        (
            engine.rows(),
            engine.cols(),
            engine.host_cell_pixel_size().or(cell_px),
        )
    };
    let transient = rows.saturating_sub(1).max(1);
    if transient == rows {
        aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);
        return;
    }
    let duplicate = match aterm_pty::dup_fd(master) {
        Ok(fd) => fd,
        Err(error) => {
            aterm_log::warn!(
                "adoption pulse: no duplicate of master {master} ({error}); pulsing back \
                 to back, so a size-comparing program may not repaint"
            );
            aterm_pty::resize_with_cell_px(master, transient, cols, cell_px);
            aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);
            return;
        }
    };
    {
        let _engine = term_lock(term);
        holding.store(true, Ordering::Release);
    }
    aterm_pty::resize_with_cell_px(duplicate.as_raw_fd(), transient, cols, cell_px);
    hold(PulseRestore {
        master: Some(duplicate),
        term: Arc::clone(term),
        holding: Arc::clone(holding),
        cell_px,
    });
}

/// The second half of an adoption pulse.
#[cfg(unix)]
pub(crate) struct PulseRestore {
    master: Option<std::os::fd::OwnedFd>,
    term: Arc<Mutex<Terminal>>,
    holding: Arc<AtomicBool>,
    cell_px: Option<(u16, u16)>,
}

#[cfg(unix)]
impl PulseRestore {
    /// End the pulse now.
    pub(crate) fn run(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        use std::os::fd::AsRawFd;
        let Some(master) = self.master.take() else {
            return;
        };
        let engine = term_lock(&self.term);
        let cell_px = engine.host_cell_pixel_size().or(self.cell_px);
        aterm_pty::resize_with_cell_px(master.as_raw_fd(), engine.rows(), engine.cols(), cell_px);
        self.holding.store(false, Ordering::Release);
        drop(engine);
    }
}

#[cfg(unix)]
impl Drop for PulseRestore {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Seed the restored engine with the render debt its predecessor carried
/// (`Adopted::render_debt`) and let the resize ledger look at it once, at attach.
pub(crate) fn seed_adopted_render_debt(
    term: &Arc<Mutex<aterm_core::terminal::Terminal>>,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    debt: aterm_core::terminal::RenderDebt,
) {
    let probe = {
        let mut engine = crate::term_lock(term);
        engine.seed_render_debt(debt);
        crate::resize_ledger::EngineProbe::sample(&engine)
    };
    timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(probe, crate::turn_ledger::now_ms());
}

/// Whether an adopted session's PTY gets the rows-1 → rows pulse that makes its program redraw.
#[cfg(any(unix, test))]
pub(crate) fn adoption_needs_size_pulse(
    checkpoint: Option<&aterm_core::terminal::TerminalCheckpoint>,
    repaint: bool,
) -> bool {
    repaint || checkpoint.is_some_and(|cp| cp.modes.alternate_screen)
}

#[cfg(test)]
#[path = "spawn_adoption_wiring_tests.rs"]
mod adoption_wiring_tests;

#[cfg(test)]
#[path = "spawn_temporal_seed_tests.rs"]
mod temporal_seed_tests;

#[cfg(test)]
#[path = "spawn_pty_keyboard_posture_tests.rs"]
mod pty_keyboard_posture_tests;

#[cfg(test)]
#[path = "spawn_adoption_pulse_tests.rs"]
mod adoption_pulse_tests;

/// The gate an overlap handoff's adopted readers park on until Commit opens it.
#[derive(Clone)]
pub(crate) struct DeferredReaderGate {
    inner: Arc<DeferredReaderGateInner>,
}

struct DeferredReaderGateInner {
    open: AtomicBool,
    lock: Mutex<()>,
    ready: std::sync::Condvar,
    #[cfg(test)]
    parked: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
struct ParkedOnGate<'a>(&'a std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl<'a> ParkedOnGate<'a> {
    fn enter(parked: &'a std::sync::atomic::AtomicUsize) -> Self {
        parked.fetch_add(1, Ordering::AcqRel);
        Self(parked)
    }
}

#[cfg(test)]
impl Drop for ParkedOnGate<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl DeferredReaderGate {
    #[must_use]
    pub(crate) fn closed() -> Self {
        Self {
            inner: Arc::new(DeferredReaderGateInner {
                open: AtomicBool::new(false),
                lock: Mutex::new(()),
                ready: std::sync::Condvar::new(),
                #[cfg(test)]
                parked: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn parked(&self) -> usize {
        self.inner.parked.load(Ordering::Acquire)
    }

    /// Point-of-no-failure release, after the parent's Commit or once a tab spawned off the
    /// loop is registered (`tab_spawn`).
    pub(crate) fn release(&self) {
        self.inner.open.store(true, Ordering::Release);
        self.inner.ready.notify_all();
    }

    #[must_use]
    pub(crate) fn is_released(&self) -> bool {
        self.inner.open.load(Ordering::Acquire)
    }

    /// Wake closed-gate threads after their session stop flags were raised.
    #[cfg(unix)]
    pub(crate) fn wake_stopped(&self) {
        self.inner.ready.notify_all();
    }

    pub(crate) fn wait_until_released(&self, stop: &AtomicBool) -> bool {
        let mut guard = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(test)]
        let mut parked = None;
        while !self.inner.open.load(Ordering::Acquire) {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            #[cfg(test)]
            parked.get_or_insert_with(|| ParkedOnGate::enter(&self.inner.parked));
            let waited = self
                .inner
                .ready
                .wait_timeout(guard, std::time::Duration::from_millis(10));
            guard = match waited {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }
}

#[cfg(test)]
#[path = "spawn_shell_integration_warn_tests.rs"]
mod shell_integration_warn_tests;

#[cfg(test)]
#[path = "spawn_agent_prime_tests.rs"]
mod agent_prime_tests;

#[cfg(all(test, windows))]
#[path = "spawn_tab_shell_env_tests.rs"]
mod tab_shell_env_tests;

#[cfg(test)]
#[path = "spawn_spawn_shell_type_tests.rs"]
mod spawn_shell_type_tests;

#[cfg(test)]
#[path = "spawn_colorfgbg_env_tests.rs"]
mod colorfgbg_env_tests;

#[cfg(test)]
#[path = "spawn_child_identity_env_tests.rs"]
mod child_identity_env_tests;

#[cfg(test)]
#[path = "spawn_capability_secrecy_conformance_tests.rs"]
mod capability_secrecy_conformance;

#[cfg(test)]
#[path = "spawn_deferred_reader_gate_tests.rs"]
mod deferred_reader_gate_tests;

/// (Re)attach the live byte pipeline to `session`.
pub(crate) fn attach_reader(
    session: &mut Session,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
    factory: &SessionFactory,
) -> Result<(), String> {
    let result = attach_reader_inner(session, window, post_to(proxy), factory, None);
    if let Err(error) = &result {
        aterm_log::error!(
            "could not attach PTY reader for session {}: {error}",
            session.id
        );
    }
    result
}

/// [`attach_reader`] with the reader's posts going to `post` (a warm spare's link,
/// `crate::spare::SpareLink::post`, or a spawn's [`WakeTarget`]).
pub(crate) fn attach_reader_with_post(
    session: &mut Session,
    window: WindowId,
    post: WakePost,
    factory: &SessionFactory,
) -> Result<(), String> {
    let result = attach_reader_inner(session, window, post, factory, None);
    if let Err(error) = &result {
        aterm_log::error!(
            "could not attach PTY reader for session {}: {error}",
            session.id
        );
    }
    result
}

/// Provision every adopted reader/helper resource behind one closed gate.
#[cfg(unix)]
pub(crate) fn prepare_deferred_reader(
    session: &mut Session,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
    factory: &SessionFactory,
    gate: &DeferredReaderGate,
) -> Result<(), String> {
    attach_reader_inner(session, window, post_to(proxy), factory, Some(gate.clone()))
}

fn reset_reader_latches(session: &Session) {
    session.reader_stop.store(false, Ordering::Release);
    session.output_wake_pending.store(0, Ordering::Relaxed);
}

/// Where a PTY reader posts its `Wake`s.
pub(crate) type WakePost = Arc<dyn Fn(Wake) -> bool + Send + Sync>;

/// The shipping [`WakePost`].
pub(crate) fn post_to(proxy: &EventLoopProxy<Wake>) -> WakePost {
    let proxy = proxy.clone();
    Arc::new(move |wake| proxy.send_event(wake).is_ok())
}

/// Where [`spawn_session`] wires a session's reader and host callbacks to post.
pub(crate) trait WakeTarget {
    fn wake_post(&self) -> WakePost;
}

impl WakeTarget for EventLoopProxy<Wake> {
    fn wake_post(&self) -> WakePost {
        post_to(self)
    }
}

fn attach_reader_inner(
    session: &mut Session,
    window: WindowId,
    post: WakePost,
    factory: &SessionFactory,
    start_gate: Option<DeferredReaderGate>,
) -> Result<(), String> {
    // A previous reader still winding down (a park that missed its deadline) is waited out:
    // two readers on one master interleave bytes and corrupt both engines.
    if let Some(join) = session.reader_join.take() {
        let _ = join.join();
    }
    {
        let (probe, shell) = (factory.program_probe, session.pid);
        let mut t = crate::term_lock(&session.term);
        t.set_program_probe(Some(Box::new(move |since_ms| probe(shell, since_ms))));
        // The jobs feed starts from the engine as it stands: a restored engine can hold a block
        // that runs before its program writes a byte.
        session.ctx.job_feed.fed(&t, &[], false, Instant::now);
    }
    let (cast_epoch, cast_watermark) = {
        let rec = session.ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        (rec.epoch(), rec.watermark())
    };
    session.temporal_tx = None;
    if let Some(old) = session.temporal_drain.take() {
        old.wait();
    }
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    let recording_costs;
    {
        let policy = crate::app_render::pty_resize_policy(session.master);
        let host_resyncs =
            crate::alt_screen_resync::host_resyncs(aterm_pty::conpty_host(session.master));
        let mut rec = session
            .ctx
            .temporal
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        rec.set_resize_policy(policy);
        rec.set_host_resyncs_alt_screen(host_resyncs);
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        {
            recording_costs = rec.spine_watermark().costs.handle();
        }
    }
    let (temporal_tx, spine_mark) = if factory.temporal_recording && factory.spare.is_none() {
        let tx = crate::temporal::open(&session.ctx.temporal, crate::temporal::QUEUE_CAP)?;
        let mark = crate::temporal::SpineMark::attach(tx.spine());
        crate::temporal::pool().join(session.id, &session.ctx.temporal);
        (Some(tx), mark)
    } else {
        (None, crate::temporal::SpineMark::default())
    };
    let session_temporal_tx = temporal_tx.clone();
    let temporal_drain = temporal_tx.as_ref().map(crate::temporal::TemporalTx::drain);
    let (reply_tx, _reply_writer_join) = spawn_reply_writer(
        session.ctx.sink.clone(),
        ReportRoute::of(&session.term, &session.ctx.modes),
    )?;
    {
        let post = Arc::clone(&post);
        let id = session.id;
        let _ = crate::input_stall::watch_input(&session.ctx.sink, move || {
            let _ = post(Wake::InputWatch { session: id });
        });
    }

    let compress_tx = CompressOffload::new(session.term.clone(), session.ctx.ui_waiting.clone());
    crate::term_lock(&session.term).set_compress_offload_active(true);

    let mut read_buf = Vec::new();
    read_buf
        .try_reserve_exact(65_536)
        .map_err(|error| format!("reserve PTY read buffer: {error}"))?;
    read_buf.resize(65_536, 0);
    let (wake_rd, wake_wr) = aterm_pty::make_wake_pipe().unwrap_or((-1, -1));
    reset_reader_latches(session);
    #[cfg(unix)]
    let proc_watch = (factory.proc_watch)(session.pid);
    #[cfg(unix)]
    let connector = {
        let (route, watching) = crate::exec_edges::wire_connector(
            factory.proc_connector,
            proc_watch.is_some(),
            factory.fg_sight.is_some(),
            session.pid,
        );
        session.ctx.ran.watched_by(watching);
        route
    };
    // A Windows session's programs are heard from its job, listed by its reader.
    #[cfg(windows)]
    let jobs = factory.job_seam.map(|seam| {
        session.ctx.ran.watched_by(crate::command_ran::Watching {
            watch: crate::command_ran::ExecWatch::Job,
            connector: crate::command_ran::Connector::NotHere,
        });
        crate::command_ran::RanJob::new(session.ctx.ran.clone(), session.pid, seam)
    });
    let reader = spawn_pty_reader(PtyReaderWiring {
        master: session.master,
        id: session.id,
        window,
        term: session.term.clone(),
        post,
        reply_tx,
        compress_tx,
        cast: session.ctx.cast.clone(),
        cast_epoch,
        cast_watermark,
        temporal_tx,
        spine_mark,
        temporal_arm: session.temporal_arm.clone(),
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        recording_costs,
        byte_fanout: session.ctx.byte_fanout.clone(),
        watchers: session.ctx.watchers.clone(),
        #[cfg(unix)]
        sink: session.ctx.sink.clone(),
        lat_epoch: factory.lat_epoch,
        loop_idle: factory.loop_idle.clone(),
        last_output_ns: session.last_output_ns.clone(),
        latest_output_activity_ns: session.latest_output_activity_ns.clone(),
        pty_closed_at: session.pty_closed_at.clone(),
        output_wake_pending: session.output_wake_pending.clone(),
        ui_waiting: session.ctx.ui_waiting.clone(),
        wake_rd,
        stop: session.reader_stop.clone(),
        #[cfg(unix)]
        shell_exit: shell_exit_watch(session),
        start_gate,
        read_buf,
        #[cfg(unix)]
        fg: ForegroundWiring {
            probe: factory.fg_probe,
            gone: factory.fg_gone,
            holder: session.fg_holder.clone(),
            shell: session.pid,
            image: factory.edge_image,
            inbox: factory.edge_inbox.clone(),
            byte_arm: factory.byte_arm,
            proc_watch,
            connector,
            sight: factory.fg_sight,
            execd: factory.leader_execd,
            ran: crate::command_ran::RanFeed::new(session.ctx.ran.clone(), session.pid),
        },
        #[cfg(windows)]
        jobs,
        timeline: session.ctx.timeline.clone(),
        screen_moves: session.ctx.screen_moves.clone(),
        job_feed: session.ctx.job_feed.clone(),
        card_probe: factory.card_probe,
        person: session.ctx.human_input.clone(),
        launch: session.ctx.launch.clone(),
        reset_lane: session.ctx.reset_lane.clone(),
        witness: session.ctx.witness.clone(),
        #[cfg(unix)]
        journal: session.ctx.crash_journal.get().cloned(),
        #[cfg(unix)]
        park: (
            session.ctx.update_parked.clone(),
            session.ctx.update_parked.mark(),
        ),
    });
    #[cfg(windows)]
    let _ = (
        factory.fg_probe,
        factory.fg_gone,
        &session.fg_holder,
        factory.edge_image,
        factory.byte_arm,
    );
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            if wake_rd >= 0 {
                aterm_pty::close_fd(wake_rd);
            }
            if wake_wr >= 0 {
                aterm_pty::close_fd(wake_wr);
            }
            crate::term_lock(&session.term).set_compress_offload_active(false);
            return Err(error);
        }
    };
    if session.wake_wr >= 0 {
        aterm_pty::close_fd(session.wake_wr);
    }
    session.wake_wr = wake_wr;
    session.temporal_tx = session_temporal_tx;
    session.temporal_drain = temporal_drain;
    session.reader_join = Some(reader);
    Ok(())
}

/// A session's PTY reader thread, and the end it signals as its last act.
///
/// `park_reader` sleeps on that end, which the reader's return wakes, instead of polling the
/// thread: a poll that yields is a `sched_yield`, and on macOS a thread that yields while another
/// is runnable on its core sinks to the lowest priority for a whole scheduler quantum (10 ms), so
/// on a busy machine a park that polled its readers one at a time took 10 ms a reader, however
/// promptly each one stopped.
pub(crate) struct ReaderJoin {
    thread: std::thread::JoinHandle<()>,
    /// Never sent on: disconnected once the reader's body has returned (or unwound) and dropped
    /// everything it held.
    #[cfg(any(unix, test))]
    ended: std::sync::mpsc::Receiver<std::convert::Infallible>,
    /// How many times a waiter looked at the reader (`ended_by`, `is_finished`).
    #[cfg(test)]
    looks: Arc<std::sync::atomic::AtomicUsize>,
}

impl ReaderJoin {
    /// Run `body` as a reader on a lane thread named `name`.
    pub(crate) fn spawn(
        name: String,
        role: crate::qos::Role,
        body: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<Self> {
        #[cfg(any(unix, test))]
        let (end, ended) = std::sync::mpsc::sync_channel::<std::convert::Infallible>(0);
        let thread = crate::qos::spawn_lane_thread(name, role, move || {
            // `body` drops what it captured as its call returns, so the end follows all of it.
            body();
            #[cfg(any(unix, test))]
            drop(end);
        })?;
        Ok(Self {
            thread,
            #[cfg(any(unix, test))]
            ended,
            #[cfg(test)]
            looks: Arc::default(),
        })
    }

    /// Whether the reader's thread has finished.
    #[must_use]
    pub(crate) fn is_finished(&self) -> bool {
        let finished = self.thread.is_finished();
        #[cfg(test)]
        self.looks.fetch_add(1, Ordering::AcqRel);
        finished
    }

    /// Wait for the reader's thread to finish: `Err` when the reader panicked.
    pub(crate) fn join(self) -> std::thread::Result<()> {
        self.thread.join()
    }

    /// Sleep until the reader has ended or `deadline` passes: whether it ended.
    #[cfg(any(unix, test))]
    pub(crate) fn ended_by(&self, deadline: Instant) -> bool {
        #[cfg(test)]
        self.looks.fetch_add(1, Ordering::AcqRel);
        match self
            .ended
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(never) => match never {},
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
        }
    }

    /// A reader running `body`, handed the count of the looks a waiter has taken at it.
    #[cfg(test)]
    pub(crate) fn watched_for_test(
        body: impl FnOnce(&std::sync::atomic::AtomicUsize) + Send + 'static,
    ) -> (Self, Arc<std::sync::atomic::AtomicUsize>) {
        let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&looks);
        let mut reader = Self::spawn(
            "aterm-test-reader".to_string(),
            crate::qos::Role::Responsive,
            move || body(&seen),
        )
        .expect("spawn a test reader");
        reader.looks = Arc::clone(&looks);
        (reader, looks)
    }

    /// A reader running `body`.
    #[cfg(test)]
    pub(crate) fn for_test(body: impl FnOnce() + Send + 'static) -> Self {
        Self::watched_for_test(move |_| body()).0
    }

    /// The count of the looks a waiter has taken at this reader.
    #[cfg(all(test, unix))]
    pub(crate) fn looks_for_test(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        Arc::clone(&self.looks)
    }
}

/// True when a parked or deferred session can be reattached without blocking the caller on its
/// old PTY reader or on the writer still writing what that reader queued.
#[must_use]
pub(crate) fn reader_attach_ready(session: &Session) -> bool {
    session
        .reader_join
        .as_ref()
        .is_none_or(ReaderJoin::is_finished)
        && session
            .temporal_drain
            .as_ref()
            .is_none_or(crate::temporal::TemporalDrain::is_finished)
}

/// Whether a session's parser is behind its program because of an update's park
/// (`SessionCtx::update_parked`).
#[derive(Debug, Default)]
pub(crate) struct UpdatePark(std::sync::atomic::AtomicU64);

#[cfg(any(unix, test))]
static UPDATE_PARK_MARKS: AtomicU64 = AtomicU64::new(1);

impl UpdatePark {
    /// Whether the parser may be behind the program by output a park left unread.
    pub(crate) fn is_parked(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    #[cfg(any(unix, test))]
    fn park(&self) {
        self.0.store(
            UPDATE_PARK_MARKS.fetch_add(1, Ordering::Relaxed),
            Ordering::Release,
        );
    }

    #[cfg(unix)]
    fn mark(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    #[cfg(unix)]
    fn caught_up(&self, mark: u64) -> bool {
        self.0
            .compare_exchange(mark, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn park_for_test(&self) {
        self.park();
    }

    #[cfg(test)]
    pub(crate) fn resume_for_test(&self) {
        self.0.store(0, Ordering::Release);
    }
}

/// Park a session's PTY reader, the overlap handoff's quiescence step.
#[cfg(any(unix, test))]
pub(crate) fn park_reader(session: &mut Session, deadline: Instant) -> bool {
    let _waiting = session
        .temporal_drain
        .as_ref()
        .map(crate::temporal::TemporalDrain::waiting);
    session.temporal_tx = None;
    let Some(reader) = session.reader_join.take() else {
        return true;
    };
    session.ctx.update_parked.park();
    session.ctx.watchers.ring_watchers();
    if session.wake_wr >= 0 {
        aterm_pty::wake(session.wake_wr);
    }
    session
        .reader_stop
        .store(true, std::sync::atomic::Ordering::Release);
    crate::watchdog::beat(crate::watchdog::Breadcrumb::UpdateHandoff);
    if !reader.ended_by(deadline) {
        session.reader_join = Some(reader);
        return false;
    }
    if reader.join().is_err() {
        return false;
    }
    if let Some(drain) = session.temporal_drain.take() {
        drain.request_urgency();
        crate::watchdog::beat(crate::watchdog::Breadcrumb::UpdateHandoff);
        if !drain.written_by(deadline) {
            session.temporal_drain = Some(drain);
            return false;
        }
    }
    true
}

#[cfg(all(test, unix))]
#[path = "dsu_quiescence_conformance.rs"]
mod dsu_quiescence_conformance;

#[cfg(all(test, unix))]
#[path = "foreground_handback_conformance.rs"]
mod foreground_handback_conformance;

#[cfg(all(test, unix))]
#[path = "cwd_report_reader_tests.rs"]
mod cwd_report_reader_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "act_barrier_reader_tests.rs"]
mod act_barrier_reader_tests;

#[cfg(all(test, unix))]
#[path = "holder_image_reader_tests.rs"]
mod holder_image_reader_tests;

#[cfg(all(test, unix))]
#[path = "spawn_adopted_rows_tests.rs"]
mod spawn_adopted_rows_tests;

#[cfg(all(test, unix))]
#[path = "spawn_spare_promotion_tests.rs"]
mod spare_promotion_tests;

#[cfg(all(test, unix))]
#[path = "spawn_shell_exit_tests.rs"]
mod shell_exit_tests;

#[cfg(test)]
#[path = "spawn_park_reader_tests.rs"]
mod park_reader_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "spawn_update_park_catch_up_tests.rs"]
mod update_park_catch_up_tests;

/// Build the engine for a fresh live session.
pub(crate) fn new_live_terminal(
    rows: u16,
    cols: u16,
    cfg: Option<&aterm_core::config::TerminalConfig>,
    cell_px: Option<(u16, u16)>,
) -> Terminal {
    let mut t = Terminal::with_scrollback(
        rows,
        cols,
        live_scrollback_ring_lines(cols),
        aterm_core::scrollback::Scrollback::with_defaults(),
    );
    if let Some(tc) = cfg {
        t.apply_config(tc);
    } else {
        t.set_scrollback_line_limit(Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT));
    }
    if let Some((cw, ch)) = cell_px {
        t.set_cell_pixel_size(cw.max(1), ch.max(1));
    }
    t.set_host_alternate_scroll(true);
    t.set_alt_archive_origin(alt_archive_origin());
    t.set_alt_archive_shared(true);
    t
}

/// This process's alt-screen archive origin, stamped on every live session's archive
/// ([`new_live_terminal`]) and printed by `offscreen origin=` and the turn ledger's
/// `arch=<origin>:<last>` marks.
pub(crate) fn alt_archive_origin() -> u64 {
    static ORIGIN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(|| {
        let nanos = aterm_time::since_epoch()
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let origin = (u64::from(std::process::id()) << 32) ^ nanos;
        origin.max(1)
    })
}

const LIVE_SCROLLBACK_RING_BYTES: usize = 4 * 1024 * 1024;

const LIVE_SCROLLBACK_RING_LINES_MAX: usize = 10_000;

const LIVE_SCROLLBACK_RING_LINES_MIN: usize = 512;

fn live_scrollback_ring_lines(cols: u16) -> usize {
    let per_line = usize::from(cols)
        .saturating_mul(std::mem::size_of::<aterm_core::grid::Cell>())
        .saturating_add(45)
        .max(1);
    (LIVE_SCROLLBACK_RING_BYTES / per_line).clamp(
        LIVE_SCROLLBACK_RING_LINES_MIN,
        LIVE_SCROLLBACK_RING_LINES_MAX,
    )
}

struct ClipWrite {
    clipboard: Option<Arc<str>>,
    primary: Option<Arc<str>>,
}

impl ClipWrite {
    fn merge_latest(&mut self, newer: Self) {
        if newer.clipboard.is_some() {
            self.clipboard = newer.clipboard;
        }
        if newer.primary.is_some() {
            self.primary = newer.primary;
        }
    }
}

struct ClipMailbox {
    state: Mutex<ClipMailboxState>,
    ready: Condvar,
}

struct ClipMailboxState {
    pending: Option<ClipWrite>,
    sender_open: bool,
}

struct ClipMailboxSender {
    mailbox: Arc<ClipMailbox>,
}

struct ClipMailboxReceiver {
    mailbox: Arc<ClipMailbox>,
}

fn clip_mailbox() -> (ClipMailboxSender, ClipMailboxReceiver) {
    let mailbox = Arc::new(ClipMailbox {
        state: Mutex::new(ClipMailboxState {
            pending: None,
            sender_open: true,
        }),
        ready: Condvar::new(),
    });
    (
        ClipMailboxSender {
            mailbox: mailbox.clone(),
        },
        ClipMailboxReceiver { mailbox },
    )
}

impl ClipMailboxSender {
    fn publish(&self, write: ClipWrite) {
        let mut state = self
            .mailbox
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(pending) = state.pending.as_mut() {
            pending.merge_latest(write);
        } else {
            state.pending = Some(write);
        }
        drop(state);
        self.mailbox.ready.notify_one();
    }
}

impl Drop for ClipMailboxSender {
    fn drop(&mut self) {
        let mut state = self
            .mailbox
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.sender_open = false;
        drop(state);
        self.mailbox.ready.notify_one();
    }
}

impl ClipMailboxReceiver {
    fn recv(&self) -> Option<ClipWrite> {
        let mut state = self
            .mailbox
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(write) = state.pending.take() {
                return Some(write);
            }
            if !state.sender_open {
                return None;
            }
            state = self
                .mailbox
                .ready
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

fn route_osc52_write(
    selections: &[aterm_core::terminal::ClipboardSelection],
    content: &str,
    primary_supported: bool,
) -> ClipWrite {
    use aterm_core::terminal::ClipboardSelection as Sel;
    let wants_primary = selections.iter().any(|s| matches!(s, Sel::Primary));
    let wants_clipboard = selections.is_empty()
        || selections.iter().any(|s| !matches!(s, Sel::Primary))
        || (wants_primary && !primary_supported);
    let content: Arc<str> = Arc::from(content);
    ClipWrite {
        clipboard: wants_clipboard.then(|| content.clone()),
        primary: (wants_primary && primary_supported).then_some(content),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "linux", test))]
enum QuerySource {
    Clipboard,
    Primary,
}

#[cfg(any(target_os = "linux", test))]
fn route_osc52_query(
    selections: &[aterm_core::terminal::ClipboardSelection],
    primary_supported: bool,
) -> QuerySource {
    use aterm_core::terminal::ClipboardSelection as Sel;
    let only_primary =
        !selections.is_empty() && selections.iter().all(|s| matches!(s, Sel::Primary));
    if only_primary && primary_supported {
        QuerySource::Primary
    } else {
        QuerySource::Clipboard
    }
}

/// What aterm, running outside any sandbox, does at a session's program's request, decided at
/// spawn by [`host_reach`] from the containment mode its shell runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostReach {
    /// The platform.
    Platform,
    /// Nothing.
    Refused,
}

const fn host_reach(mode: aterm_containment::ContainmentMode) -> HostReach {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::User | Cm::Safety => HostReach::Platform,
        _ => HostReach::Refused,
    }
}

fn configure_clipboard(
    term: &Arc<Mutex<Terminal>>,
    reach: HostReach,
    witness: &Arc<crate::spare::Witness>,
) {
    let witness = Arc::clone(witness);
    install_clipboard(term, reach, move |write| {
        witness.host_effects.fetch_add(1, Ordering::Relaxed);
        deliver_clip_write(write);
    });
}

/// What this process's sessions reach for their programs ([`host_reach`] of their containment).
pub(crate) fn session_host_reach() -> HostReach {
    host_reach(aterm_containment::mode_or_containment())
}

/// A session's host wiring (design `docs/DESIGN-warm-spare-2026-10-16.md` §5.2).
pub(crate) fn arm_host_reach(
    term: &Arc<Mutex<Terminal>>,
    reach: HostReach,
    factory: &SessionFactory,
    proxy: &dyn WakeTarget,
    id: u64,
    window: WindowId,
    ctx: &crate::SessionCtx,
) {
    let witness = &ctx.witness;
    configure_clipboard(term, reach, witness);
    if reach == HostReach::Platform
        && factory
            .terminal_config
            .as_ref()
            .is_some_and(|c| c.allow_osc52_query)
    {
        term_lock(term).authorize_clipboard_access(ClipboardAccess::Query);
    }
    configure_notifications(term, reach, &factory.notify_tx, id, witness);
    configure_kitty_file_transfer(term, reach, factory.allow_kitty_file_transfer);
    configure_bell(term, proxy, id, window, witness);
    #[cfg(target_os = "linux")]
    configure_window_ops(term, proxy, id, window);
}

fn deliver_clip_write(write: ClipWrite) {
    if let Some(text) = write.clipboard {
        control::pbcopy(&text);
    }
    #[cfg(target_os = "linux")]
    if let Some(text) = write.primary {
        control::primary_set(&text);
    }
}

fn install_clipboard(
    term: &Arc<Mutex<Terminal>>,
    reach: HostReach,
    deliver: impl FnMut(ClipWrite) + Send + 'static,
) {
    if reach == HostReach::Refused {
        let mut t = term_lock(term);
        t.revoke_clipboard_access(ClipboardAccess::Write);
        t.revoke_clipboard_access(ClipboardAccess::Query);
        return;
    }
    let (clip_tx, clip_rx) = clip_mailbox();
    let mut idle = Some((clip_rx, deliver));
    let mut start_worker = move || {
        let Some((clip_rx, mut deliver)) = idle.take() else {
            return;
        };
        #[cfg(test)]
        crate::work_counts::clipboard_worker_started();
        let _ = std::thread::Builder::new()
            .name("aterm-clipboard".into())
            .spawn(move || {
                while let Some(write) = clip_rx.recv() {
                    deliver(write);
                }
            });
    };
    let mut t = term_lock(term);
    t.authorize_clipboard_access(ClipboardAccess::Write);
    let primary_supported = cfg!(target_os = "linux");
    t.set_clipboard_callback(move |op| match op {
        ClipboardOperation::Set {
            selections,
            content,
        } => {
            clip_tx.publish(route_osc52_write(&selections, &content, primary_supported));
            start_worker();
            None
        }
        ClipboardOperation::Clear { selections } => {
            clip_tx.publish(route_osc52_write(&selections, "", primary_supported));
            start_worker();
            None
        }
        ClipboardOperation::Query { selections } => {
            #[cfg(not(target_os = "linux"))]
            {
                let _ = &selections;
                crate::host_io_prefetch::take_clipboard_query()
            }
            #[cfg(target_os = "linux")]
            {
                match route_osc52_query(&selections, primary_supported) {
                    QuerySource::Primary => crate::control::primary_get_owned(),
                    QuerySource::Clipboard => crate::control::pbpaste_owned(),
                }
            }
        }
    });
}

pub(crate) fn configure_notifications(
    term: &Arc<Mutex<Terminal>>,
    reach: HostReach,
    notify_tx: &std::sync::mpsc::SyncSender<notify::NotifyMsg>,
    id: u64,
    witness: &Arc<crate::spare::Witness>,
) {
    if reach == HostReach::Refused {
        return;
    }
    let mut t = term_lock(term);
    let tx = notify_tx.clone();
    let w = Arc::clone(witness);
    t.set_notification_callback(move |body| {
        w.host_effects.fetch_add(1, Ordering::Relaxed);
        let _ = tx.try_send(notify::NotifyMsg::program(id, None, body.to_string()));
    });
    let tx = notify_tx.clone();
    let w = Arc::clone(witness);
    t.set_advanced_notification_callback(move |n| {
        if !n.has_content() {
            return;
        }
        w.host_effects.fetch_add(1, Ordering::Relaxed);
        let _ = tx.try_send(notify::NotifyMsg::program(
            id,
            n.title,
            n.body.unwrap_or_default(),
        ));
    });
}

static PALETTE_REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether a session refused a palette set since the last call.
pub(crate) fn take_palette_refused() -> bool {
    PALETTE_REFUSED.load(Ordering::Relaxed) && PALETTE_REFUSED.swap(false, Ordering::AcqRel)
}

fn configure_palette_refusal(term: &Arc<Mutex<Terminal>>) {
    term_lock(term).set_palette_refused_callback(|| {
        PALETTE_REFUSED.store(true, Ordering::Release);
    });
}

fn configure_bell(
    term: &Arc<Mutex<Terminal>>,
    proxy: &dyn WakeTarget,
    id: u64,
    window: WindowId,
    witness: &Arc<crate::spare::Witness>,
) {
    let post = proxy.wake_post();
    let witness = Arc::clone(witness);
    term_lock(term).set_bell_callback(move || {
        witness.host_effects.fetch_add(1, Ordering::Relaxed);
        let _ = post(Wake::Bell {
            session: id,
            window,
        });
    });
}

#[cfg(target_os = "linux")]
fn configure_window_ops(
    term: &Arc<Mutex<Terminal>>,
    proxy: &dyn WakeTarget,
    id: u64,
    window: WindowId,
) {
    let post = proxy.wake_post();
    term_lock(term).set_window_callback(move |op| {
        if let Some(wake) = window_op_wake(op, id, window) {
            let _ = post(wake);
        }
        None
    });
}

/// The pure half of [`configure_window_ops`].
#[cfg(target_os = "linux")]
pub(crate) fn window_op_wake(
    op: aterm_types::WindowOperation,
    session: u64,
    window: WindowId,
) -> Option<Wake> {
    use aterm_types::WindowOperation as W;
    match op {
        op @ (W::DeIconify
        | W::Iconify
        | W::RaiseWindow
        | W::LowerWindow
        | W::RefreshWindow
        | W::ResizeWindowPixels { .. }
        | W::ResizeWindowCells { .. }
        | W::RestoreMaximized
        | W::MaximizeWindow
        | W::MaximizeVertically
        | W::MaximizeHorizontally
        | W::UndoFullscreen
        | W::EnterFullscreen
        | W::ToggleFullscreen) => Some(Wake::WindowOp {
            session,
            window,
            op,
        }),
        _ => None,
    }
}

/// Maximum bytes a Kitty non-direct medium may supply (the engine's `MAX_KITTY_IMAGE_BYTES`).
pub(crate) const MAX_KITTY_MEDIUM_BYTES: u64 = 4 * 1024 * 1024;

/// Install the Kitty medium resolver where `allow` (`allow_kitty_file_transfer`) and `reach` admit
/// it, and clear any other: an adopted spare's taint resolver must not outlive its adoption.
fn configure_kitty_file_transfer(term: &Arc<Mutex<Terminal>>, reach: HostReach, allow: bool) {
    let mut t = term_lock(term);
    if kitty_media_admitted(reach, allow) {
        t.set_kitty_file_resolver(crate::host_io_prefetch::take_kitty_medium);
    } else {
        t.clear_kitty_file_resolver();
    }
}

/// Whether a session's program may name a Kitty file medium for aterm to read: the setting
/// (`allow_kitty_file_transfer`) allows it and the session's reach is not [`HostReach::Refused`].
pub(crate) fn kitty_media_admitted(reach: HostReach, allow: bool) -> bool {
    allow && reach != HostReach::Refused
}

/// One Kitty non-direct medium's bytes, read under [`configure_kitty_file_transfer`]'s policy.
pub(crate) fn read_kitty_medium(
    medium: aterm_core::terminal::kitty_graphics::KittyMedium,
    name: &str,
) -> Option<Vec<u8>> {
    use aterm_core::terminal::kitty_graphics::KittyMedium;
    match medium {
        KittyMedium::File | KittyMedium::TempFile => read_kitty_file(Path::new(name)),
        KittyMedium::SharedMemory => read_posix_shm(name),
        _ => None,
    }
}

/// Consume a medium the engine took ([`read_kitty_medium`]'s read of it; `read` is whether that
/// read returned bytes).
pub(crate) fn consume_kitty_medium(
    medium: aterm_core::terminal::kitty_graphics::KittyMedium,
    name: &str,
    read: bool,
) {
    use aterm_core::terminal::kitty_graphics::KittyMedium;
    match medium {
        KittyMedium::TempFile if read => consume_kitty_temp_file(Path::new(name)),
        KittyMedium::SharedMemory => unlink_posix_shm(name),
        _ => {}
    }
}

fn read_kitty_file(path: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_KITTY_MEDIUM_BYTES {
        return None;
    }
    use std::io::Read as _;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    (&file)
        .take(MAX_KITTY_MEDIUM_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_KITTY_MEDIUM_BYTES {
        return None;
    }
    Some(bytes)
}

const KITTY_TEMP_FILE_MARKER: &str = "tty-graphics-protocol";

fn consume_kitty_temp_file(path: &Path) {
    consume_kitty_temp_file_with(path, &kitty_temp_dirs(), || {});
}

/// Delete the client's temp file `path` where the protocol allows it, judged against the
/// canonical temporary directories `temps`; `before_unlink` runs between judgement and unlink.
fn consume_kitty_temp_file_with(
    path: &Path,
    temps: &[std::path::PathBuf],
    before_unlink: impl FnOnce(),
) {
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        consume_kitty_temp_entry(dir, name, temps, before_unlink);
    }
}

/// Whether the canonical `entry` may be deleted: its path names the protocol's marker and lies
/// under one of `temps`.
fn kitty_temp_entry_deletable(entry: &Path, temps: &[std::path::PathBuf]) -> bool {
    entry.to_string_lossy().contains(KITTY_TEMP_FILE_MARKER)
        && temps.iter().any(|tmp| entry.starts_with(tmp))
}

#[cfg(unix)]
fn consume_kitty_temp_entry(
    dir: &Path,
    name: &std::ffi::OsStr,
    temps: &[std::path::PathBuf],
    before_unlink: impl FnOnce(),
) {
    use aterm_dirfd::{AtFlags, CWD, FileType, Mode, OFlags, fstat, openat, unlinkat};
    use std::path::Component;

    let Ok(dir) = std::fs::canonicalize(dir) else {
        return;
    };
    let walk = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(mut held) = openat(CWD, "/", walk, Mode::empty()) else {
        return;
    };
    for component in dir.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => match openat(&held, part, walk, Mode::empty()) {
                Ok(next) => held = next,
                Err(_) => return,
            },
            _ => return,
        }
    }
    if !kitty_temp_entry_deletable(&dir.join(name), temps) {
        return;
    }
    let leaf = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let regular = openat(&held, name, leaf, Mode::empty())
        .and_then(fstat)
        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode).is_file());
    if regular {
        before_unlink();
        let _ = unlinkat(&held, name, AtFlags::empty());
    }
}

#[cfg(not(unix))]
fn consume_kitty_temp_entry(
    dir: &Path,
    name: &std::ffi::OsStr,
    temps: &[std::path::PathBuf],
    before_unlink: impl FnOnce(),
) {
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return;
    };
    let entry = dir.join(name);
    if kitty_temp_entry_deletable(&entry, temps) {
        before_unlink();
        let _ = std::fs::remove_file(entry);
    }
}

/// The protocol's temporary directories on this machine, canonical: /tmp, /dev/shm, `$TMPDIR`
/// and the platform's temporary directory, where each exists.
fn kitty_temp_dirs() -> Vec<std::path::PathBuf> {
    let named = [
        Some(std::path::PathBuf::from("/tmp")),
        Some(std::path::PathBuf::from("/dev/shm")),
        std::env::var_os("TMPDIR").map(std::path::PathBuf::from),
        Some(std::env::temp_dir()),
    ];
    named
        .into_iter()
        .flatten()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect()
}

#[cfg(unix)]
fn read_posix_shm(name: &str) -> Option<Vec<u8>> {
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY: `cname` is a valid NUL-terminated C string; `shm_open` with O_RDONLY returns a
    // valid fd or -1, which is checked.
    let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return None;
    }
    let result = (|| {
        // SAFETY: `fd` is a valid open fd; `fstat` fills a zeroed stat or returns -1.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return None;
        }
        let len = st.st_size;
        if len <= 0 || len as u64 > MAX_KITTY_MEDIUM_BYTES {
            return None;
        }
        let len = len as usize;
        // SAFETY: mapping `len` (>0, capped) read-only from the valid fd at offset 0.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: `addr`/`len` describe a valid read-only mapping just established; the bytes
        // are copied out before unmapping.
        let bytes = unsafe { std::slice::from_raw_parts(addr.cast::<u8>(), len) }.to_vec();
        // SAFETY: unmapping the exact `addr`/`len` we mapped above.
        unsafe { libc::munmap(addr, len) };
        Some(bytes)
    })();
    // SAFETY: `fd` is valid; closing once.
    unsafe { libc::close(fd) };
    result
}

#[cfg(not(unix))]
fn read_posix_shm(_name: &str) -> Option<Vec<u8>> {
    None
}

#[cfg(unix)]
fn unlink_posix_shm(name: &str) {
    let Ok(cname) = std::ffi::CString::new(name) else {
        return;
    };
    // SAFETY: `cname` is a valid C string; `shm_unlink` tolerates an absent name.
    unsafe { libc::shm_unlink(cname.as_ptr()) };
}

#[cfg(not(unix))]
fn unlink_posix_shm(_name: &str) {}

const REPLY_QUEUE_CAP: usize = 1024;

/// What the reply-writer thread is asked to do.
pub(crate) enum ReplyJob {
    Bytes(std::sync::Arc<[u8]>, aterm_session::sink::ReplyPermit),
    Report(std::sync::Arc<[u8]>, aterm_session::sink::ReplyPermit),
    /// A non-parking writer spilled with no drainer live.
    ArrangeSpillDrainer,
}

/// The reader's end of the reply lane.
#[derive(Debug)]
pub(crate) struct ReplyLane {
    tx: std::sync::mpsc::SyncSender<ReplyJob>,
    budget: aterm_session::sink::ReplyBudget,
}

impl ReplyLane {
    /// Queue one reply: reserve its bytes against the sink's reply budget, then `try_send`.
    pub(crate) fn submit(&self, reply: std::sync::Arc<[u8]>) -> bool {
        self.queue(reply, ReplyJob::Bytes)
    }

    /// [`Self::submit`] for a report the engine sent unasked, which the writer hands to its
    /// [`ReportRoute`] instead of writing it as a reply.
    pub(crate) fn submit_report(&self, report: std::sync::Arc<[u8]>) -> bool {
        self.queue(report, ReplyJob::Report)
    }

    fn queue(
        &self,
        bytes: std::sync::Arc<[u8]>,
        job: fn(std::sync::Arc<[u8]>, aterm_session::sink::ReplyPermit) -> ReplyJob,
    ) -> bool {
        let queued = self
            .budget
            .try_reserve(bytes.len())
            .is_some_and(|permit| self.tx.try_send(job(bytes, permit)).is_ok());
        if !queued {
            crate::metrics::note_reply_dropped();
        }
        queued
    }
}

/// Where the reply writer sends a report the engine sent unasked ([`ReplyJob::Report`]).
#[derive(Clone, Default)]
pub(crate) struct ReportRoute {
    #[cfg(windows)]
    input: Option<(
        std::sync::Weak<Mutex<Terminal>>,
        Arc<aterm_core::terminal::ModeMirror>,
    )>,
}

impl ReportRoute {
    /// The route of the session whose engine and mode mirror these are.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub(crate) fn of(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<aterm_core::terminal::ModeMirror>,
    ) -> Self {
        Self {
            #[cfg(windows)]
            input: Some((Arc::downgrade(term), Arc::clone(modes))),
        }
    }

    #[cfg_attr(not(windows), allow(unused_variables))]
    fn send(&self, sink: &Arc<SinkWriter>, report: &[u8]) -> bool {
        #[cfg(windows)]
        if let Some((term, modes)) = &self.input
            && let Some(term) = term.upgrade()
        {
            let ev = crate::input::InputEvent::KeySequence(report.to_vec());
            crate::app_input::write_ui_report(&term, modes, sink, &ev);
            return true;
        }
        false
    }
}

fn spawn_reply_writer(
    sink: Arc<SinkWriter>,
    reports: ReportRoute,
) -> Result<(ReplyLane, std::thread::JoinHandle<()>), String> {
    let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel::<ReplyJob>(REPLY_QUEUE_CAP);
    let weak = Arc::downgrade(&sink);
    let join = spawn_reply_writer_thread(move || {
        crate::qos::set_self(crate::qos::Role::Interactive);
        while let Ok(job) = reply_rx.recv() {
            let Some(sink) = weak.upgrade() else { break };
            match job {
                ReplyJob::Bytes(resp, permit) => {
                    let _ = aterm_pty::as_reply(|| sink.write_reply(&resp));
                    drop(permit);
                }
                ReplyJob::Report(report, permit) => {
                    if !reports.send(&sink, &report) {
                        let _ = aterm_pty::as_reply(|| sink.write_reply(&report));
                    }
                    drop(permit);
                }
                ReplyJob::ArrangeSpillDrainer => sink.arrange_pending_drainer(),
            }
        }
    })
    .map_err(|error| format!("spawn reply writer: {error}"))?;
    {
        let poke = reply_tx.clone();
        sink.install_spill_arranger(move || {
            let _ = poke.try_send(ReplyJob::ArrangeSpillDrainer);
        });
    }
    Ok((
        ReplyLane {
            tx: reply_tx,
            budget: sink.reply_budget(),
        },
        join,
    ))
}

/// Test seam: a real reply writer over `sink`, for the egress-contract tests that drive replies
/// and pastes through one session's sink.
#[cfg(all(test, unix))]
pub(crate) fn spawn_reply_writer_for_test(
    sink: Arc<SinkWriter>,
) -> (ReplyLane, std::thread::JoinHandle<()>) {
    spawn_reply_writer(sink, ReportRoute::default()).expect("spawn the reply writer")
}

fn spawn_reply_writer_thread(
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    if take_injected_reply_writer_spawn_fault() {
        return Err(std::io::Error::other("injected reply-writer spawn failure"));
    }
    std::thread::Builder::new()
        .name("aterm-reply-writer".into())
        .spawn(body)
}

#[cfg(test)]
thread_local! {
    static REPLY_WRITER_SPAWN_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn take_injected_reply_writer_spawn_fault() -> bool {
    REPLY_WRITER_SPAWN_FAULT.with(|f| f.replace(false))
}

#[cfg(not(test))]
const fn take_injected_reply_writer_spawn_fault() -> bool {
    false
}

#[cfg(all(test, windows))]
#[path = "spawn_report_route_tests.rs"]
mod report_route_tests;

#[cfg(all(test, unix))]
#[path = "spawn_reply_writer_lifetime_tests.rs"]
mod reply_writer_lifetime_tests;

const COMPRESS_SIGNAL_CAP: usize = 1;
const COMPRESS_SIGNAL_AT: usize = 900;
const COMPRESS_BUDGET: usize = 256;
const COMPRESS_LOW_WATER: usize = 256;
const COMPRESS_QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);
const COMPRESS_TRICKLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

struct CompressOffload {
    term: Arc<Mutex<Terminal>>,
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
    tx: std::sync::OnceLock<Option<std::sync::mpsc::SyncSender<()>>>,
}

impl CompressOffload {
    const fn new(
        term: Arc<Mutex<Terminal>>,
        ui_waiting: Arc<std::sync::atomic::AtomicU32>,
    ) -> Self {
        Self {
            term,
            ui_waiting,
            tx: std::sync::OnceLock::new(),
        }
    }

    fn signal(&self) {
        let maybe_tx = self.tx.get_or_init(|| {
            let tx = spawn_compress_worker(self.term.clone(), self.ui_waiting.clone());
            if tx.is_none() {
                crate::term_lock(&self.term).set_compress_offload_active(false);
            }
            tx
        });
        if let Some(tx) = maybe_tx {
            let _ = tx.try_send(());
        }
    }
}

fn spawn_compress_worker(
    term: Arc<Mutex<Terminal>>,
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
) -> Option<std::sync::mpsc::SyncSender<()>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(COMPRESS_SIGNAL_CAP);
    crate::qos::spawn_lane_thread(
        "aterm-scrollback-compress",
        crate::qos::Role::Responsive,
        move || {
            while rx.recv().is_ok() {
                let mut last_trickle = std::time::Instant::now();
                relieve_flood_pressure(&term, &ui_waiting);
                while let Ok(()) = rx.recv_timeout(COMPRESS_QUIET_WINDOW) {
                    relieve_flood_pressure(&term, &ui_waiting);
                    if last_trickle.elapsed() >= COMPRESS_TRICKLE_INTERVAL {
                        term_lock(&term).trickle_lazy_bounded(COMPRESS_BUDGET);
                        last_trickle = std::time::Instant::now();
                        crate::yield_to_ui_waiter(&ui_waiting);
                    }
                }
                let mut prev = usize::MAX;
                loop {
                    let remaining = {
                        let mut t = term_lock(&term);
                        t.drain_lazy_bounded(COMPRESS_BUDGET)
                    };
                    if remaining <= COMPRESS_LOW_WATER || remaining >= prev {
                        break;
                    }
                    prev = remaining;
                    crate::yield_to_ui_waiter(&ui_waiting);
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the compress worker's give-way between its bounded batches, so the PTY reader (which never registers) can take the lock: the worker waits for nobody, and no deadline waits on its drain"
                    )]
                    std::thread::yield_now();
                }
            }
        },
    )
    .ok()
    .map(|_| tx)
}

fn relieve_flood_pressure(term: &Mutex<Terminal>, ui_waiting: &std::sync::atomic::AtomicU32) {
    let mut owed = 0;
    loop {
        let promoted = {
            let mut t = term_lock(term);
            if owed == 0 {
                owed = t.main_grid().flood_promotion_due();
                if owed == 0 {
                    return;
                }
            }
            let before = t.lazy_backlog_len();
            before.saturating_sub(t.drain_lazy_pressure_bounded(owed.min(COMPRESS_BUDGET)))
        };
        if promoted == 0 {
            return;
        }
        owed = owed.saturating_sub(promoted);
        crate::yield_to_ui_waiter(ui_waiting);
        #[expect(
            clippy::disallowed_methods,
            reason = "the compress worker's give-way between its bounded relief batches, so the PTY reader (which never registers) can take the lock: the worker waits for nobody, and no deadline waits on its relief"
        )]
        std::thread::yield_now();
    }
}

#[cfg(test)]
#[path = "spawn_flood_pressure_relief_tests.rs"]
mod flood_pressure_relief_tests;

#[cfg(unix)]
const GATHER_RING_BUFFERS: usize = 4;

#[cfg(unix)]
const EXIT_DRAIN_MAX: usize = 1024 * 1024;

#[cfg(unix)]
fn shell_exit_watch(session: &Session) -> Option<ShellExit> {
    if session.child_reaped.load(Ordering::Acquire) {
        return None;
    }
    shell_exit_of(session.shell_identity)
}

/// [`shell_exit_watch`] of a shell not yet reaped. A shell of this process's own that no watch
/// can be made for keeps a [`ShellExit`] with none: the kernel refuses an exit watch (`ESRCH`)
/// on a process that has begun to exit but cannot be collected yet, as a shell that fails at
/// once often is by the time a loaded machine attaches its reader. Its parent still waits for
/// it ([`ShellExit::await_retired`]), so the exit is not posted before its status can be read
/// (a shell killed at start would read as a clean exit).
#[cfg(unix)]
fn shell_exit_of(identity: aterm_pty::ShellIdentity) -> Option<ShellExit> {
    let pid = u32::try_from(identity.pid).ok().filter(|pid| *pid > 1)?;
    let watch = match aterm_uds::exitwatch::exit_fd(pid) {
        Some(watch) => Some(watch),
        None if identity.own_child => None,
        None => return None,
    };
    identity.verify().then_some(ShellExit { watch, pid })
}

/// The most the gather waits, once its shell's PTY has closed or its exit fired, for the kernel
/// to retire the shell before the exit is posted: a hang detector, never a latency budget (a
/// short bound reads a starved shell that failed at start as a clean exit). A stop ends the
/// wait at once ([`ShellExit::await_retired`]).
#[cfg(unix)]
pub(crate) const SHELL_RETIRE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// How often [`ShellExit::await_retired`] looks again for a shell that has exited, or could not
/// be watched, and is not collectable yet: a nap, never a spin that takes the CPU the exiting
/// process needs.
#[cfg(unix)]
const RETIRE_LOOK: std::time::Duration = std::time::Duration::from_millis(1);

/// How often [`ShellExit::await_retired`] of a gather with no wake pipe (fd exhaustion) reads
/// its stop flag while the exit watch is silent: a stop is all such a gather has during a wait
/// of up to [`SHELL_RETIRE_HANG`].
#[cfg(unix)]
const RETIRE_STOP_LOOK: std::time::Duration = std::time::Duration::from_millis(10);

/// A session's shell's exit watch ([`shell_exit_watch`]) and the pid it watches.
#[cfg(unix)]
pub(crate) struct ShellExit {
    /// The kernel's exit watch; `None` for a shell of this process's own that was already
    /// exiting when its reader attached ([`shell_exit_of`]).
    watch: Option<std::os::fd::OwnedFd>,
    pid: u32,
}

/// Whether retirement was observed, stayed unconfirmed (the bound passed, or an error), or was
/// cancelled by the reader's stop or wake.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellRetirement {
    Retired,
    Unconfirmed,
    Cancelled,
}

#[cfg(unix)]
impl ShellRetirement {
    fn gather_message(self, ending: GatherMsg) -> GatherMsg {
        match self {
            Self::Cancelled => GatherMsg::Wake,
            Self::Retired | Self::Unconfirmed => ending,
        }
    }
}

#[cfg(unix)]
impl ShellExit {
    fn as_raw_fd(&self) -> i32 {
        self.watch
            .as_ref()
            .map_or(-1, std::os::fd::AsRawFd::as_raw_fd)
    }

    fn await_retired_at_exit(&self, wake_rd: i32, stop: &AtomicBool) -> ShellRetirement {
        self.await_retired(SHELL_RETIRE_HANG, wake_rd, stop)
    }

    fn await_retired(
        &self,
        within: std::time::Duration,
        wake_rd: i32,
        stop: &AtomicBool,
    ) -> ShellRetirement {
        let now = Instant::now();
        let deadline = now
            .checked_add(within)
            .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
        let stopped = || stop.load(Ordering::Acquire);
        if self.watch.is_some() {
            loop {
                if stopped() {
                    return ShellRetirement::Cancelled;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return ShellRetirement::Unconfirmed;
                }
                let step = if wake_rd < 0 {
                    left.min(RETIRE_STOP_LOOK)
                } else {
                    left
                };
                let mut fds = [
                    libc::pollfd {
                        fd: self.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake_rd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let ms = libc::c_int::try_from(step.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
                // SAFETY: two live pollfds for the length of the call.
                let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
                if rc < 0 {
                    return ShellRetirement::Unconfirmed;
                }
                if fds[1].revents != 0 || stopped() {
                    return ShellRetirement::Cancelled;
                }
                if fds[0].revents != 0 {
                    break;
                }
            }
        }
        let Ok(pid) = libc::pid_t::try_from(self.pid) else {
            return ShellRetirement::Unconfirmed;
        };
        loop {
            if stopped() {
                return ShellRetirement::Cancelled;
            }
            // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            // SAFETY: `waitid` filled a `CLD_*` siginfo or, with nothing waitable under
            // `WNOHANG`, left it zeroed; `si_pid` is defined in both.
            let collectable = rc == 0 && unsafe { info.si_pid() } == pid;
            let finished = rc != 0 || collectable || Instant::now() >= deadline;
            let mut wake = libc::pollfd {
                fd: wake_rd,
                events: libc::POLLIN,
                revents: 0,
            };
            let nap = if finished {
                0
            } else {
                libc::c_int::try_from(RETIRE_LOOK.as_millis()).unwrap_or(1)
            };
            // SAFETY: one live pollfd for the length of the call.
            if unsafe { libc::poll(&mut wake, 1, nap) } > 0 || stopped() {
                return ShellRetirement::Cancelled;
            }
            if collectable {
                return ShellRetirement::Retired;
            }
            if finished {
                return ShellRetirement::Unconfirmed;
            }
        }
    }
}

fn term_busy(term: &Mutex<Terminal>) -> bool {
    matches!(term.try_lock(), Err(std::sync::TryLockError::WouldBlock))
}

fn parse_hold_wait(waited: Option<Instant>) -> u64 {
    waited.map_or(0, |at| {
        u64::try_from(at.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .max(1)
    })
}

#[test]
fn the_parse_hold_probe_tells_a_held_term_lock_from_a_free_one() {
    let term = Mutex::new(Terminal::new(4, 10));
    assert!(!term_busy(&term), "free");
    assert!(!term_busy(&term), "still free after a probe");
    let held = term_lock(&term);
    assert!(term_busy(&term), "held");
    drop(held);
    assert_eq!(parse_hold_wait(None), 0);
    assert!(
        parse_hold_wait(Some(Instant::now())) > 0,
        "a wait books at least 1 ns"
    );
}

/// The image a gather with no process watch reads for a sampled foreground edge to `to`, read
/// with `edge_image` and noted in `ran` once `execd` says its leader has exec'd since its fork:
/// where nothing hears `exec`s (Linux, a watch that could not open), the in-front image each edge
/// reads is the session's record of what ran, and a leader that has not exec'd (a shell's child
/// between its fork and its exec, or one whose command was not found) still maps the shell's
/// image, which no command ran. The exec evidence is birth-bracketed before the image and must
/// match the holder image's birth, so a reused PID never lends its old affirmative flag.
#[cfg(unix)]
pub(crate) fn sampled_edge_image(
    edge_image: crate::holder_image::ReadImage,
    execd: fn(i32) -> Option<u64>,
    ran: &crate::command_ran::RanFeed,
    to: i32,
    filled: usize,
) -> Option<Arc<crate::holder_image::HolderImage>> {
    (to > 0).then(|| {
        let exec_start = execd(to);
        let image = edge_image(to, None);
        if exec_start == Some(image.start_us) {
            ran.image(&image, filled);
        }
        Arc::new(image)
    })
}

/// [`spawn_session`]'s holder adoption for a restored engine (card engine §3.8, lane A4).
#[cfg(unix)]
#[expect(
    clippy::too_many_arguments,
    reason = "the session's engine, timeline and ran log beside the factory's four probes"
)]
pub(crate) fn adopt_restored_holder(
    term: &Mutex<Terminal>,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    ran: &crate::command_ran::RanFeed,
    master: i32,
    root_pgid: i32,
    fg_probe: fn(i32) -> i32,
    edge_image: crate::holder_image::ReadImage,
    children: fn(i32) -> Vec<i32>,
    execd: fn(i32) -> Option<u64>,
) -> aterm_core::terminal::HolderAdoption {
    let cursor = {
        let timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let log = timeline.slog();
        log.opened_wall()
            .filter(|_| log.log().is_open(aterm_slog::Node::SESSION))
            .map(|since| (log.block_cursor(), Some(since)))
    };
    let (running, pgid) = {
        let term = term_lock(term);
        let running =
            crate::session_status::executing_block(&term).map(|(block, window)| (block.id, window));
        (running, fg_probe(master))
    };
    let image = (pgid > 0).then(|| Arc::new(edge_image(pgid, None)));
    if let Some(image) = image.as_deref().filter(|_| pgid != root_pgid) {
        // The restored command's program ran before this process heard anything: its image,
        // read now while it runs, is noted as the running block's job's, with its file's
        // identity and steers, before the blocks are read, so the read the restore logs and the
        // next sweep confirm the word the person typed (a `claude` through its link).
        ran.restored(image, running.map(|(block, _)| block));
    } else if pgid == root_pgid {
        // The shell's own group in front: the shell itself is no command's, but pwsh runs its
        // commands in that group, so the running command is one of its live children that has
        // exec'd, each placed by its birth (nothing in a shell that runs jobs).
        ran.restored_in_shell_group(running, children, edge_image, execd);
    }
    let blocks = cursor.map(|(from, since_wall)| {
        crate::session_status::block_read(&term_lock(term), from, since_wall, ran.ran())
    });
    let (token, exact) = image.as_deref().map_or((0, false), |image| {
        (
            image.token.filter(|_| image.drawer.is_some()).unwrap_or(0),
            image.drawer.is_some()
                && matches!(
                    image.gate_build(|| None),
                    Some(crate::holder_image::GateBuild::Exact(_))
                ),
        )
    });
    let (adoption, marks) = {
        let mut t = term_lock(term);
        let adoption = t.adopt_holder_after_restore(token, exact);
        (adoption, t.holder_fence().tokens().collect::<Vec<u64>>())
    };
    let led = restored_drawer_groups((pgid, token), &marks, root_pgid, edge_image, children);
    {
        let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let groups = timeline.drawer_groups_mut();
        for led in led {
            groups.note(Some(led), &marks);
        }
        groups.note(None, &marks);
    }
    if let Some(image) = image {
        let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(blocks) = blocks {
            timeline.sweep_slog(&blocks, false, root_pgid, pgid);
        } else {
            timeline.note_root_group(root_pgid);
            timeline.note_foreground_group(pgid);
        }
        let _ = timeline.offer_holder_image(image, crate::holder_image::EdgeKind::Group);
        if crate::holder_image::DEV_ROWS {
            timeline.record_row(
                "holder-adopt",
                format!(
                    "adoption={adoption:?} token={} exact={} pgid={pgid} {}",
                    crate::holder_image::token_word((token != 0).then_some(token)),
                    u8::from(exact),
                    crate::holder_image::dev_tail()
                ),
            );
        }
    }
    adoption
}

/// The groups a restored fence's marks lead ([`crate::holder_image::DrawerGroups`]): the adopted
/// holder's, and each carried stopped drawer's among the shell's `children` (a job-control shell
/// makes each job's leader its child), read again by `edge_image` only when such a mark exists.
#[cfg(unix)]
fn restored_drawer_groups(
    (pgid, token): (i32, u64),
    marks: &[u64],
    shell: i32,
    edge_image: crate::holder_image::ReadImage,
    children: fn(i32) -> Vec<i32>,
) -> Vec<(i32, u64)> {
    let mut led: Vec<(i32, u64)> = Vec::new();
    if token != 0 {
        led.push((pgid, token));
    }
    if marks.iter().any(|&mark| mark != token) {
        for child in children(shell).into_iter().filter(|&child| child != pgid) {
            let image = edge_image(child, None);
            if let Some(stopped) = image
                .token
                .filter(|t| image.drawer.is_some() && *t != token && marks.contains(t))
            {
                led.push((child, stopped));
            }
        }
    }
    led
}

/// The shell's live children, which a restore's carried stopped drawers lead and among which a
/// pwsh session's running command runs (`SessionFactory::shell_children` as shipped; the process
/// watch, and so a retirement, exists only on Darwin, and elsewhere a restore reads none).
#[cfg(unix)]
pub(crate) fn shell_children(shell: i32) -> Vec<i32> {
    #[cfg(target_vendor = "apple")]
    return aterm_uds::process::child_pids(shell).unwrap_or_default();
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = shell;
        Vec::new()
    }
}

/// Apply a holder cut whose incoming image leads `group`, booking in `notes` what it leaves the
/// session's [`crate::holder_image::DrawerGroups`] to note once the term lock is released.
fn note_cut(
    t: &mut Terminal,
    cut: aterm_core::terminal::HolderCut,
    group: i32,
    notes: &mut crate::holder_image::GroupNotes,
) {
    if cut.to.is_some() || t.holder_fence().current_token().is_some() {
        t.note_holder_cut(cut);
        crate::metrics::note_holder_cut();
        notes.cut(
            cut.to.map(|token| (group, token)),
            t.holder_fence().tokens(),
        );
    }
}

/// The process watch heard `pid` exit: when it led a stopped drawer's group, that drawer's mark
/// goes (card engine §3.7, marks retired by liveness). A current drawer's mark stays for its
/// cut, which drops it.
#[cfg(unix)]
fn retire_gone(
    term: &Mutex<Terminal>,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    temporal_arm: &crate::temporal::Arm,
    temporal_lane: &mut (
        Option<crate::temporal::TemporalTx>,
        crate::temporal::SpineMark,
    ),
    pid: i32,
) {
    let Some(token) = timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .drawer_groups()
        .led_by(pid)
    else {
        return;
    };
    let marks: Vec<u64> = {
        let mut t = term_lock(term);
        t.retire_holder_mark(token);
        crate::temporal::pick_up(temporal_arm, &mut temporal_lane.0, &mut temporal_lane.1);
        if let Some(tx) = &temporal_lane.0 {
            temporal_lane.1.flush_host(tx, &t);
        }
        t.holder_fence().tokens().collect()
    };
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.drawer_groups_mut().note(None, &marks);
    if crate::holder_image::DEV_ROWS {
        timeline.record_row(
            "holder-retire",
            format!(
                "token={} pid={pid} retired={}",
                crate::holder_image::token_word(Some(token)),
                u8::from(!marks.contains(&token))
            ),
        );
    }
}

/// Batch messages from the gather stage to the parse stage (unix pipeline).
#[cfg(unix)]
pub(crate) enum GatherMsg {
    /// The deferred start gate released.
    Started,
    /// Every byte the master held when this reader started, which a park left unread
    /// ([`UpdatePark`]), went out in the batches before this one.
    CaughtUp,
    /// One gathered batch: buffer, filled length, the foreground change it carries, the group
    /// whose bytes end it (`edge.to` with an edge), with an edge `edge.to`'s image as the gather
    /// read it (`crate::holder_image`), and the offset of its end in the session's output, which
    /// the parse stage hands the session's ran log (`crate::command_ran::Ran::places_in`, then
    /// `crate::command_ran::Ran::parsed_through` once parsed).
    Data(
        Vec<u8>,
        usize,
        Option<crate::foreground_handback::FgEdge>,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
        u64,
    ),
    /// A foreground edge with no byte in flight (card engine §3.3).
    Edge(
        crate::foreground_handback::FgEdge,
        i32,
        Option<Arc<crate::holder_image::HolderImage>>,
    ),
    /// The process watch heard this process exit ([`retire_gone`]).
    Gone(i32),
    /// The master EOF'd on its own (shell exited).
    Eof,
    /// The session's shell exited (its exit watch fired before the master's EOF), and what the
    /// master held then has been sent.
    Exited,
    /// Session teardown (wake pipe / stop flag).
    Wake,
    Reset(crate::manual_reset::ManualReset),
    /// A host path changed the engine with no output to follow (a resize, a config or cell size).
    Journal,
    /// Every byte the master held when the gather took these act barriers (off macOS, where its
    /// queue cannot be read, every byte the gather had read) went out in the batches before this
    /// one (`crate::act_barrier`): the parse stage answers each once it holds them.
    Barrier(Vec<std::sync::mpsc::SyncSender<()>>),
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn spawn_pty_gather(
    master: i32,
    wake_rd: i32,
    shell_exit: Option<ShellExit>,
    id: u64,
    lat_epoch: Instant,
    latest_output_activity_ns: Arc<AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    start_gate: Option<DeferredReaderGate>,
    first_buf: Vec<u8>,
    free_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    filled_tx: std::sync::mpsc::SyncSender<GatherMsg>,
    parse_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    sink: Arc<SinkWriter>,
    fg_probe: fn(i32) -> i32,
    fg_holder: Arc<std::sync::atomic::AtomicI32>,
    edge_image: crate::holder_image::ReadImage,
    proc_watch: Option<aterm_uds::procwatch::ProcWatch>,
    connector: Option<aterm_uds::proc_connector::Route>,
    sight: Option<crate::fg_sample::SightSeam>,
    execd: fn(i32) -> Option<u64>,
    ran: crate::command_ran::RanFeed,
    lane_guard: crate::manual_reset::LaneGuard,
    journal: Option<Arc<crate::crash_restore::SessionJournal>>,
) -> Result<(), String> {
    crate::qos::spawn_lane_thread(
        format!("aterm-pty-gather-{id}"),
        crate::qos::Role::Responsive,
        move || {
            let mut lane_guard = lane_guard;
            let mut barriers = crate::act_barrier::Countdown::default();
            if let Some(gate) = &start_gate
                && !gate.wait_until_released(&stop)
            {
                let _ = filled_tx.send(GatherMsg::Wake);
                if wake_rd >= 0 {
                    aterm_pty::close_fd(wake_rd);
                }
                return;
            }
            let carried = fg_holder.load(Ordering::Acquire);
            let seed = if carried > 0 {
                carried
            } else {
                fg_probe(master)
            };
            let mut cutter = crate::foreground_handback::FgCutter::new(seed);
            let mut exec = proc_watch.map(|watch| {
                crate::exec_edges::ExecEdges::new(watch, edge_image, seed, ran.clone())
                    .reading_queue(aterm_pty::output_queue_len, master)
            });
            let proc_rd = exec.as_ref().map_or(-1, crate::exec_edges::ExecEdges::fd);
            // Where no watch hears execs, the group in front is sampled while a block runs.
            let mut sampler = sight
                .filter(|_| exec.is_none())
                .map(crate::fg_sample::FgSampler::new);
            // And on Linux the proc connector's route hands its sampler the session's execs,
            // read between batches (`crate::fg_sample::FgSampler::connected`).
            let route = connector.filter(|_| sampler.is_some());
            let woken_rd = route
                .as_ref()
                .map_or(proc_rd, aterm_uds::proc_connector::Route::fd);
            let mut routed = Vec::new();
            let send_byte_free = |cutter: &mut crate::foreground_handback::FgCutter,
                                  exec: &mut crate::exec_edges::ExecEdges|
             -> bool {
                loop {
                    if let Some(edge) = cutter.take_edge() {
                        let holder = cutter.holder();
                        if holder > 0 {
                            fg_holder.store(holder, Ordering::Release);
                        }
                        if filled_tx
                            .send(GatherMsg::Edge(edge, holder, exec.take_image()))
                            .is_err()
                        {
                            return false;
                        }
                    }
                    if !exec.has_deferred() {
                        return true;
                    }
                    cutter.begin();
                    exec.replay(cutter, || fg_probe(master));
                }
            };
            let send_gone = |exec: &mut crate::exec_edges::ExecEdges| -> bool {
                exec.take_gone()
                    .all(|pid| filled_tx.send(GatherMsg::Gone(pid)).is_ok())
            };
            let _ = filled_tx.send(GatherMsg::Started);
            let mut owed = aterm_pty::output_queue_len(master).unwrap_or(0);
            if owed == 0 {
                let _ = filled_tx.send(GatherMsg::CaughtUp);
            }
            let nonblock = aterm_pty::set_nonblocking(master, true).is_ok();
            sink.note_master_nonblocking(nonblock);
            let mut buf = first_buf;
            let mut exit_rd = shell_exit.as_ref().map_or(-1, ShellExit::as_raw_fd);
            let mut after_exit: Option<usize> = None;
            let mut exit_reported = false;
            loop {
                // Stop only before a read, never between a read and its hand-off: bytes drained
                // from the kernel must reach the engine, and the parse stage processes every
                // batch it was sent.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = filled_tx.send(GatherMsg::Wake);
                    break;
                }
                let outcome = match after_exit {
                    None => aterm_pty::read_or_poke(
                        master,
                        &mut buf,
                        wake_rd,
                        exit_rd,
                        woken_rd,
                        lane_guard.poke_rd,
                        sampler.as_ref().map_or(-1, |s| s.wait_ms(Instant::now())),
                    ),
                    Some(left) => {
                        let room = left.min(buf.len());
                        match aterm_pty::drain_more(master, &mut buf[..room], 0) {
                            0 => {
                                after_exit = None;
                                exit_rd = -1;
                                let message =
                                    shell_exit.as_ref().map_or(GatherMsg::Exited, |exit| {
                                        exit.await_retired_at_exit(wake_rd, &stop)
                                            .gather_message(GatherMsg::Exited)
                                    });
                                let cancelled = matches!(message, GatherMsg::Wake);
                                if filled_tx.send(message).is_err() || cancelled {
                                    break;
                                }
                                exit_reported = true;
                                continue;
                            }
                            n => aterm_pty::ReadOutcome::Data(n),
                        }
                    }
                };
                match outcome {
                    aterm_pty::ReadOutcome::Exited => {
                        after_exit = Some(EXIT_DRAIN_MAX);
                    }
                    aterm_pty::ReadOutcome::Proc => {
                        if let Some(x) = exec.as_mut() {
                            cutter.begin();
                            // What the shell wrote before an exec heard now is handed on
                            // already or still in the PTY: the exec waits for the parse to
                            // reach where it stood (`crate::command_ran::Spot`).
                            x.heard(&mut cutter, || fg_probe(master), 0);
                            if !send_byte_free(&mut cutter, x) || !send_gone(x) {
                                break;
                            }
                        } else if let (Some(r), Some(s)) = (route.as_ref(), sampler.as_mut()) {
                            // Every byte read so far has gone out, as at a sample.
                            if r.drain(&mut routed) {
                                s.connector_lost(&ran, || aterm_pty::has_unread(master));
                            }
                            let mut sent = true;
                            for event in routed.drain(..) {
                                match event {
                                    aterm_uds::proc_connector::Event::Exec(e) => {
                                        s.connected(
                                            &e,
                                            &ran,
                                            || fg_probe(master),
                                            || aterm_pty::has_unread(master),
                                        );
                                    }
                                    aterm_uds::proc_connector::Event::Exit { pid } => {
                                        sent = sent && filled_tx.send(GatherMsg::Gone(pid)).is_ok();
                                    }
                                }
                            }
                            if !sent {
                                break;
                            }
                        }
                    }
                    aterm_pty::ReadOutcome::Wake => {
                        let _ = filled_tx.send(GatherMsg::Wake);
                        break;
                    }
                    aterm_pty::ReadOutcome::Poke => {
                        // The top of the loop: every byte read so far has gone out, so what the
                        // master holds now is all the program wrote before the ask that is not.
                        if !aterm_pty::drain_pokes(lane_guard.poke_rd) {
                            aterm_pty::close_fd(lane_guard.poke_rd);
                            lane_guard.poke_rd = -1;
                        }
                        let asked = lane_guard.lane.take_asked(lane_guard.generation);
                        let queued = aterm_pty::output_queue_len(master);
                        if let Some(now) = barriers.ask(asked, queued)
                            && filled_tx.send(GatherMsg::Barrier(now)).is_err()
                        {
                            break;
                        }
                    }
                    aterm_pty::ReadOutcome::Idle => {
                        // Wake-pipe-less fallback (fd exhaustion), or the sampler's bound: honor
                        // the stop flag.
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            let _ = filled_tx.send(GatherMsg::Wake);
                            break;
                        }
                        // The sampler's wait ended with nothing to read: every byte read so far
                        // has gone out (`crate::command_ran::RanFeed::sampled`).
                        if let Some(s) = sampler.as_mut() {
                            s.tick(
                                Instant::now(),
                                &ran,
                                || fg_probe(master),
                                || aterm_pty::has_unread(master),
                            );
                        }
                    }
                    aterm_pty::ReadOutcome::Eof => {
                        let message = if exit_reported {
                            GatherMsg::Wake
                        } else {
                            shell_exit.as_ref().map_or(GatherMsg::Eof, |exit| {
                                exit.await_retired_at_exit(wake_rd, &stop)
                                    .gather_message(GatherMsg::Eof)
                            })
                        };
                        let _ = filled_tx.send(message);
                        break;
                    }
                    aterm_pty::ReadOutcome::Data(n) => {
                        #[cfg(test)]
                        crate::holder_image::hold_hook::after_read(id);
                        cutter.begin();
                        let filled = if nonblock {
                            aterm_pty::drain_more_nonblocking(
                                master,
                                &mut buf,
                                n,
                                wake_rd,
                                proc_rd,
                                Some(parse_in_flight.as_ref()),
                                crate::input_hint::pending,
                                &mut |f, heard| match exec.as_mut() {
                                    Some(x) if heard => {
                                        x.heard(&mut cutter, || fg_probe(master), f)
                                    }
                                    _ => cutter.sample(fg_probe(master), f),
                                },
                            )
                        } else {
                            aterm_pty::drain_more_watched(master, &mut buf, n, proc_rd, &mut |f| {
                                exec.as_mut()
                                    .is_some_and(|x| x.heard(&mut cutter, || fg_probe(master), f))
                            })
                        };
                        let mut edge = cutter.finish(|| fg_probe(master), filled);
                        let image = match (edge.as_mut(), exec.as_mut()) {
                            (Some(e), Some(x)) => x.take_image().or_else(|| {
                                (e.to > 0).then(|| {
                                    let absorbed = x.absorb(e.to, filled, || fg_probe(master));
                                    if absorbed {
                                        e.lay = filled;
                                    }
                                    x.read_edge(e.to, absorbed)
                                })
                            }),
                            (Some(e), None) => {
                                sampled_edge_image(edge_image, execd, &ran, e.to, filled)
                            }
                            (None, _) => None,
                        };
                        let holder = cutter.holder();
                        if holder > 0 {
                            fg_holder.store(holder, Ordering::Release);
                        }
                        let output_activity_now =
                            u64::try_from(lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        latest_output_activity_ns
                            .store(output_activity_now.max(1), Ordering::Release);
                        parse_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if let Some(journal) = &journal {
                            journal.gathered(filled);
                        }
                        let end = ran.ran().handed_on(filled);
                        if filled_tx
                            .send(GatherMsg::Data(buf, filled, edge, holder, image, end))
                            .is_err()
                        {
                            break;
                        }
                        if let Some(s) = sampler.as_mut() {
                            s.handed(Instant::now());
                        }
                        if owed > 0 {
                            owed = owed.saturating_sub(filled);
                            if owed == 0 {
                                let _ = filled_tx.send(GatherMsg::CaughtUp);
                            }
                        }
                        let due = barriers
                            .sent(filled, || aterm_pty::output_queue_len(master) == Some(0));
                        if !due.is_empty() && filled_tx.send(GatherMsg::Barrier(due)).is_err() {
                            break;
                        }
                        if let Some(x) = exec.as_mut()
                            && x.has_deferred()
                        {
                            cutter.begin();
                            x.replay(&mut cutter, || fg_probe(master));
                            if !send_byte_free(&mut cutter, x) {
                                break;
                            }
                        }
                        if let Some(x) = exec.as_mut()
                            && !send_gone(x)
                        {
                            break;
                        }
                        if let Some(left) = after_exit.as_mut() {
                            *left = left.saturating_sub(filled);
                        }
                        buf = match free_rx.recv() {
                            Ok(b) => b,
                            Err(_) => break,
                        };
                    }
                }
            }
            aterm_pty::close_fd(wake_rd);
        },
    )
    .map(|_| ())
    .map_err(|error| format!("spawn PTY gather: {error}"))
}

#[cfg(unix)]
struct ForegroundWiring {
    probe: fn(i32) -> i32,
    gone: crate::foreground_handback::FgGone,
    holder: Arc<std::sync::atomic::AtomicI32>,
    shell: i32,
    image: crate::holder_image::ReadImage,
    inbox: crate::holder_image::EdgeInbox,
    byte_arm: bool,
    proc_watch: Option<aterm_uds::procwatch::ProcWatch>,
    connector: Option<aterm_uds::proc_connector::Route>,
    sight: Option<crate::fg_sample::SightSeam>,
    execd: fn(i32) -> Option<u64>,
    ran: crate::command_ran::RanFeed,
}

struct PtyReaderWiring {
    master: i32,
    id: u64,
    window: WindowId,
    term: Arc<Mutex<Terminal>>,
    post: WakePost,
    reply_tx: ReplyLane,
    compress_tx: CompressOffload,
    cast: Arc<std::sync::Mutex<crate::cast::CastRecorder>>,
    cast_epoch: Instant,
    cast_watermark: Arc<crate::cast::CastWatermark>,
    temporal_tx: Option<crate::temporal::TemporalTx>,
    spine_mark: crate::temporal::SpineMark,
    temporal_arm: Arc<crate::temporal::Arm>,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    recording_costs: Option<Arc<crate::recording_cost::Costs>>,
    byte_fanout: Arc<crate::cast::ByteFanout>,
    watchers: Arc<crate::session_watch::Watchers>,
    #[cfg(unix)]
    sink: Arc<SinkWriter>,
    lat_epoch: Instant,
    loop_idle: Arc<LoopIdle>,
    last_output_ns: Arc<AtomicU64>,
    latest_output_activity_ns: Arc<AtomicU64>,
    pty_closed_at: Arc<std::sync::OnceLock<Instant>>,
    output_wake_pending: Arc<AtomicU64>,
    ui_waiting: Arc<std::sync::atomic::AtomicU32>,
    wake_rd: i32,
    stop: Arc<AtomicBool>,
    #[cfg(unix)]
    shell_exit: Option<ShellExit>,
    start_gate: Option<DeferredReaderGate>,
    read_buf: Vec<u8>,
    #[cfg(unix)]
    fg: ForegroundWiring,
    /// What a Windows reader hears of its session's job for the programs its commands ran
    /// (`None`: no job seam, the test App's).
    #[cfg(windows)]
    jobs: Option<crate::command_ran::RanJob>,
    timeline: Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
    screen_moves: Arc<crate::vitals_host::ScreenMoves>,
    job_feed: Arc<crate::vitals_jobs::JobFeed>,
    card_probe: bool,
    person: Arc<crate::human_input::HumanInputStamp>,
    launch: Arc<crate::launch_witness::LaunchWitness>,
    reset_lane: Arc<crate::manual_reset::ResetLane>,
    witness: Arc<crate::spare::Witness>,
    #[cfg(unix)]
    journal: Option<Arc<crate::crash_restore::SessionJournal>>,
    #[cfg(unix)]
    park: (Arc<UpdatePark>, u64),
}

/// The dev `card-seen` probe over one parsed slice's visible rows and the fence they left
/// (`mutations`: the screen mutations before the slice; a compose pass whose snapshot counted
/// more shows what it drew). It dates only a card [`crate::claude_card::admit`] admits under
/// the drawer's gate with no build policy (`GateBuild::ProvenanceOnly`; `card-seen` holds the
/// card to the image's build itself), so a card an earlier run of the same build left above
/// the drawer's start is never dated as the drawer's. At most
/// [`crate::holder_image::CARD_PROBE_BATCHES`] batches look for one token's card.
fn note_card_seen_in(
    session: u64,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    budget: &mut Option<(u64, u16)>,
    probed_batch: &mut bool,
    (rows, fence): (&[String], &crate::claude_card::FenceFrame),
) {
    let unseen = timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .card_unseen();
    let Some((token, drawer)) = unseen else {
        return;
    };
    let left = match *budget {
        Some((t, left)) if t == token => left,
        _ => crate::holder_image::CARD_PROBE_BATCHES,
    };
    if left == 0 {
        return;
    }
    if !*probed_batch || budget.is_none_or(|(t, _)| t != token) {
        *budget = Some((token, left - 1));
        *probed_batch = true;
    }
    let reading = crate::claude_card::read_card(session, rows, None);
    let gate = drawer.gate(token, crate::holder_image::GateBuild::ProvenanceOnly);
    if let Some(card) = reading
        .read
        .as_ref()
        .filter(|card| crate::claude_card::admit(&gate, fence, card).is_ok())
    {
        let row = fence
            .base_y
            .saturating_add(i64::try_from(card.place.top).unwrap_or(0));
        timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .note_card_seen(
                token,
                reading.rows_kind(),
                &card.version,
                row,
                fence.mutations,
            );
    }
}

#[cfg(all(test, any(debug_assertions, feature = "dev-seams")))]
#[path = "spawn_card_seen_probe_tests.rs"]
mod card_seen_probe_tests;

fn spawn_pty_reader(w: PtyReaderWiring) -> Result<ReaderJoin, String> {
    let PtyReaderWiring {
        master,
        id,
        window,
        term,
        post,
        reply_tx,
        compress_tx,
        cast,
        cast_epoch,
        cast_watermark,
        temporal_tx,
        spine_mark,
        temporal_arm,
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        recording_costs,
        byte_fanout,
        watchers,
        #[cfg(unix)]
        sink,
        lat_epoch,
        loop_idle,
        last_output_ns,
        latest_output_activity_ns,
        pty_closed_at,
        output_wake_pending,
        ui_waiting,
        wake_rd,
        stop,
        #[cfg(unix)]
        shell_exit,
        start_gate,
        read_buf,
        #[cfg(unix)]
        fg,
        #[cfg(windows)]
        mut jobs,
        timeline,
        screen_moves,
        job_feed,
        card_probe: card_probe_on,
        person,
        launch,
        reset_lane,
        witness,
        #[cfg(unix)]
        journal,
        #[cfg(unix)]
        park,
    } = w;
    #[cfg(not(unix))]
    let _ = (&reset_lane, &witness);
    #[cfg(unix)]
    let (fg_gone, fg_shell) = (fg.gone, fg.shell);
    #[cfg(not(unix))]
    let (fg_gone, fg_shell): (crate::foreground_handback::FgGone, i32) = (|_, _, _| false, 0);
    #[cfg(unix)]
    let ran = Arc::clone(fg.ran.ran());
    #[cfg(unix)]
    let (fg_image, edge_inbox, byte_arm, exec_edges) = (
        fg.image,
        fg.inbox.clone(),
        fg.byte_arm,
        fg.proc_watch.is_some(),
    );
    #[cfg(not(unix))]
    let (fg_image, edge_inbox, byte_arm, exec_edges): (
        crate::holder_image::ReadImage,
        crate::holder_image::EdgeInbox,
        bool,
        bool,
    ) = (
        crate::holder_image::unread,
        Default::default(),
        false,
        false,
    );
    #[cfg(unix)]
    let (filled_rx, free_tx, parse_in_flight, lane_generation, probe_sink, start_gather) = {
        let (filled_tx, filled_rx) =
            std::sync::mpsc::sync_channel::<GatherMsg>(GATHER_RING_BUFFERS + 4);
        let (poke_rd, poke_wr) = aterm_pty::make_wake_pipe().unzip();
        let lane_generation = reset_lane.open(filled_tx.clone(), poke_wr);
        let lane_guard = crate::manual_reset::LaneGuard {
            lane: reset_lane.clone(),
            generation: lane_generation,
            poke_rd: poke_rd.unwrap_or(-1),
        };
        let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(GATHER_RING_BUFFERS);
        for _ in 1..GATHER_RING_BUFFERS {
            let _ = free_tx.try_send(vec![0u8; 65_536]);
        }
        let parse_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe_sink = Arc::downgrade(&sink);
        let (activity, stop, in_flight, gather_journal) = (
            latest_output_activity_ns.clone(),
            stop.clone(),
            parse_in_flight.clone(),
            journal.clone(),
        );
        let start_gather = move || {
            spawn_pty_gather(
                master,
                wake_rd,
                shell_exit,
                id,
                lat_epoch,
                activity,
                stop,
                start_gate,
                read_buf,
                free_rx,
                filled_tx,
                in_flight,
                sink,
                fg.probe,
                fg.holder,
                fg.image,
                fg.proc_watch,
                fg.connector,
                fg.sight,
                fg.execd,
                fg.ran,
                lane_guard,
                gather_journal,
            )
        };
        (
            filled_rx,
            free_tx,
            parse_in_flight,
            lane_generation,
            probe_sink,
            start_gather,
        )
    };
    #[cfg(not(unix))]
    let probe_sink = std::sync::Weak::<SinkWriter>::new();
    let parse = ReaderJoin::spawn(
        format!("aterm-pty-reader-{id}"),
        crate::qos::Role::Responsive,
        move || {
            #[cfg(windows)]
            let mut buf = read_buf;
            #[cfg(windows)]
            if let Some(gate) = &start_gate
                && !gate.wait_until_released(&stop)
            {
                if wake_rd >= 0 {
                    aterm_pty::close_fd(wake_rd);
                }
                return;
            }
            let mut temporal_lane = (temporal_tx, spine_mark);
            #[cfg(windows)]
            let _ = post(Wake::Ready {
                session: id,
                window,
            });
            let pending_reset: std::cell::Cell<Option<crate::manual_reset::ManualReset>> =
                std::cell::Cell::new(None);
            let mut fg_owners = crate::foreground_handback::FgOwners::default();
            let mut prefetch = crate::host_io_prefetch::Prefetch::default();
            let mut cadence = PostCadence::default();
            let output_wake = |cadence: &mut PostCadence| {
                let now = (lat_epoch.elapsed().as_nanos() as u64).max(1);
                let gate = gated_output_wake(
                    &output_wake_pending,
                    now,
                    loop_idle.since_ns(),
                    cadence.streaming(now),
                    || {
                        post(Wake::Output {
                            session: id,
                            window,
                        })
                    },
                    || lat_epoch.elapsed().as_nanos() as u64,
                );
                match gate {
                    WakeGate::Posted | WakeGate::Reposted => cadence.posted(now),
                    WakeGate::Held => cadence.held(now),
                    WakeGate::Coalesced => {}
                }
                if gate == WakeGate::Reposted {
                    crate::metrics::note_output_wake_repost();
                    aterm_log::info!(
                        "session {id}: re-posted an output wake stranded while the event \
                         loop slept (posted after its last turn, none since, older than {} ms)",
                        WAKE_LATCH_EXPIRY_NS / 1_000_000
                    );
                }
            };
            let mut arm = byte_arm.then(|| {
                let executing = {
                    let t = term_lock(&term);
                    t.is_require_shell_integration_nonce()
                        && t.shell_state() == aterm_core::terminal::ShellState::Executing
                };
                let arm = crate::holder_image::ByteArm::seeded(&timeline, executing);
                if exec_edges { arm.kernel_edges() } else { arm }
            });
            let mut card_probe: Option<(u64, u16)> = None;
            let publish_holder = |image: Arc<crate::holder_image::HolderImage>,
                                  kind: crate::holder_image::EdgeKind,
                                  arm: Option<&mut crate::holder_image::ByteArm>,
                                  cadence: &mut PostCadence| {
                let offer = timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .offer_holder_image(Arc::clone(&image), kind);
                if let Some(arm) = arm {
                    arm.note_read(&image);
                }
                if offer == crate::holder_image::Offer::Published {
                    edge_inbox.push(id);
                    output_wake(cadence);
                }
            };
            // The places held execs wait for in the batch being parsed (`None` for a parse of no
            // gathered batch): the ingest ends a chunk at each and reads the block running there.
            let places = std::cell::RefCell::new(None::<crate::command_ran::Places>);
            let mut ingest = |temporal_lane: &mut (
                Option<crate::temporal::TemporalTx>,
                crate::temporal::SpineMark,
            ),
                              buf: &[u8],
                              edge: Option<crate::foreground_handback::FgEdge>,
                              holder: i32,
                              image: Option<Arc<crate::holder_image::HolderImage>>,
                              read_at: Option<Instant>| {
                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                let _cost = crate::recording_cost::begin(
                    recording_costs.as_deref().map(|cost| &cost.ingest),
                    u64::try_from(buf.len()).ok(),
                );
                let arrived = (!buf.is_empty()).then(|| read_at.unwrap_or_else(Instant::now));
                if let Some(at) = arrived {
                    stamp_output_arrival(
                        &last_output_ns,
                        (at.saturating_duration_since(lat_epoch).as_nanos() as u64).max(1),
                    );
                    job_feed.read(buf);
                    crate::echo_rtt::note_output_burst(id);
                }
                let mut from_role = crate::foreground_handback::FgRole::Holder;
                let edge = edge.or_else(|| {
                    let missed = fg_owners.missed_reclaim(holder, fg_shell, |pgid| {
                        let master = if probe_sink.upgrade().is_some() {
                            master
                        } else {
                            -1
                        };
                        fg_gone(master, pgid, crate::foreground_handback::FgRole::Bystander)
                    })?;
                    aterm_log::info!(
                        "session {id}: group {} armed an input mode and is gone, the shell \
                         holds, and no edge from it was sampled: its death is the edge",
                        missed.from
                    );
                    // Found gone by the bystander rule, so its handback judges it by that rule.
                    from_role = crate::foreground_handback::FgRole::Bystander;
                    Some(missed)
                });
                if edge.is_some() {
                    cadence.foreground_edge(&output_wake_pending);
                }
                let mut backlog = 0usize;
                let mut handback: Option<aterm_core::terminal::ForegroundHandback> = None;
                let mut launch_unread: Option<crate::launch_witness::Unread> = None;
                let mut resync_due = false;
                let mut manual_run: Option<(
                    crate::manual_reset::ManualReset,
                    crate::manual_reset::ResetReport,
                    Vec<u8>,
                )> = None;
                let mut cuts = crate::cast::CastCuts::default();
                let mut emitted = 0usize;
                let spine_copy: Option<std::sync::Arc<[u8]>> = temporal_lane
                    .0
                    .as_ref()
                    .filter(|tx| temporal_lane.1.needs_input(tx))
                    .map(|_| {
                        #[cfg(any(debug_assertions, feature = "dev-seams"))]
                        let _cost = crate::recording_cost::begin(
                            recording_costs.as_deref().map(|cost| &cost.copy),
                            u64::try_from(buf.len()).ok(),
                        );
                        std::sync::Arc::from(buf)
                    });
                #[cfg(unix)]
                let mut journal_keyframe: Option<crate::crash_restore::Captured> = None;
                let (response, reports) = {
                    let bytes: &[u8] = spine_copy.as_deref().unwrap_or(buf);
                    if let Some(req) = pending_reset.take() {
                        let mut t = {
                            #[cfg(any(debug_assertions, feature = "dev-seams"))]
                            let _cost = crate::recording_cost::begin(
                                recording_costs.as_deref().map(|cost| &cost.reader_lock),
                                Some(1),
                            );
                            term_lock(&term)
                        };
                        crate::temporal::pick_up(
                            &temporal_arm,
                            &mut temporal_lane.0,
                            &mut temporal_lane.1,
                        );
                        cuts.observe(emitted, &t, &cast_watermark);
                        let spine_pre = temporal_lane
                            .0
                            .as_ref()
                            .map(|_| crate::temporal::SpinePre::take(&t));
                        #[cfg(unix)]
                        let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                        let clock = aterm_core::terminal::ClockReading::now();
                        let (report, reset_bytes) = crate::manual_reset::apply(&mut t, clock);
                        emitted += reset_bytes.len();
                        fg_owners.observe(holder, t.program_evidence(), t.take_evidence_asserted());
                        if !reset_bytes.is_empty()
                            && let (Some(tx), Some(pre)) = (&temporal_lane.0, spine_pre)
                        {
                            let input = crate::temporal::InputSlice::at(&reset_bytes[..], clock);
                            temporal_lane.1.record(tx, pre, &t, input);
                        } else if let Some(tx) = &temporal_lane.0 {
                            temporal_lane.1.flush_host(tx, &t);
                        }
                        #[cfg(unix)]
                        if let (Some(j), Some(pre)) = (&journal, journal_pre.as_mut()) {
                            j.record(&t, pre, &reset_bytes, true);
                            journal_keyframe = j.decide(&t, pre);
                        }
                        manual_run = Some((req, report, reset_bytes));
                    }
                    #[cfg(unix)]
                    if let (Some(j), Some(captured)) = (&journal, journal_keyframe.take()) {
                        j.write(captured);
                    }
                    let mut acc: Option<Vec<u8>> = None;
                    let mut reports: Option<Vec<u8>> = None;
                    let mut off = 0;
                    let mut handback_at = edge.map(|e| e.at.min(bytes.len()));
                    let mut lay_at = edge
                        .map(|e| e.lay.min(bytes.len()))
                        .filter(|lay| Some(*lay) > handback_at);
                    let drawer_to = image
                        .as_ref()
                        .filter(|i| i.drawer.is_some())
                        .and_then(|i| i.token);
                    let edge_group = image.as_ref().map_or(0, |i| i.pgid);
                    let mut slice_holder = edge.map_or(holder, |e| e.from);
                    if let Some(a) = arm.as_mut() {
                        if a.scans() {
                            let crate::holder_image::ByteArm { scan, ends, .. } = a;
                            scan.ends(bytes, ends);
                        }
                        if !bytes.is_empty() {
                            a.begin_batch();
                        }
                    }
                    let mut probed_batch = false;
                    while off < bytes.len() || handback_at == Some(off) || lay_at == Some(off) {
                        let limit = match handback_at {
                            Some(at) if at > off => at,
                            _ => bytes.len(),
                        };
                        let limit = match lay_at {
                            Some(lay) if lay > off => limit.min(lay),
                            _ => limit,
                        };
                        let limit = places
                            .borrow()
                            .as_ref()
                            .and_then(|p| p.next_cut(off))
                            .map_or(limit, |cut| cut.min(limit));
                        let limit = arm
                            .as_ref()
                            .and_then(|a| a.next_cut(off))
                            .filter(|_| {
                                #[cfg(test)]
                                {
                                    !crate::holder_image::hold_hook::no_c_cut(id)
                                }
                                #[cfg(not(test))]
                                {
                                    true
                                }
                            })
                            .map_or(limit, |cut| cut.min(limit));
                        let end = off
                            + ingest_chunk_width(
                                || {
                                    crate::input_hint::pending()
                                        || ui_waiting.load(Ordering::Relaxed) != 0
                                },
                                limit - off,
                            );
                        let fetched = prefetch.reads(&term, &bytes[off..end]);
                        let end = off + fetched.covered();
                        let verdicts = match (handback_at, edge) {
                            (Some(at), Some(e)) if at == off && e.from == e.to => {
                                Some(crate::foreground_handback::FgVerdicts::default())
                            }
                            (Some(at), Some(e)) if at == off => {
                                let held = probe_sink.upgrade();
                                let master = if held.is_some() { master } else { -1 };
                                Some(fg_owners.probe_edge(e.from, from_role, e.to, |pgid, role| {
                                    fg_gone(master, pgid, role)
                                }))
                            }
                            _ => None,
                        };
                        let offered = if let (Some(e), true) = (edge, verdicts.is_some()) {
                            image.clone().map(|i| (i, e.kind))
                        } else {
                            arm.as_mut().and_then(|a| {
                                let image = a.read_due(slice_holder, fg_image)?;
                                let kind = if a.last_of(image.pgid).is_some() {
                                    crate::holder_image::EdgeKind::Exec
                                } else {
                                    crate::holder_image::EdgeKind::Group
                                };
                                Some((Arc::new(image), kind))
                            })
                        };
                        let arm_cut = match (&offered, arm.as_ref()) {
                            (Some((read, _)), Some(a)) if verdicts.is_none() => {
                                let to = read.drawer.and(read.token);
                                let cut = match a.last_of(read.pgid) {
                                    Some(last) if last.same_image(read) => None,
                                    Some(_) => Some(aterm_core::terminal::HolderCut {
                                        to,
                                        kind: aterm_core::terminal::CutKind::Exec,
                                        from_alive: false,
                                    }),
                                    None => Some(aterm_core::terminal::HolderCut {
                                        to,
                                        kind: aterm_core::terminal::CutKind::Group,
                                        from_alive: true,
                                    }),
                                };
                                cut.map(|cut| (cut, read.pgid))
                            }
                            _ => None,
                        };
                        let offered = offered.and_then(|(image, kind)| {
                            #[cfg(test)]
                            if crate::holder_image::hold_hook::publish_after_hold(id) {
                                return Some((image, kind));
                            }
                            publish_holder(image, kind, arm.as_mut(), &mut cadence);
                            None
                        });
                        let card_rows_wanted = crate::holder_image::DEV_ROWS
                            && card_probe_on
                            && end > off
                            && (offered.is_some()
                                || timeline
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .card_unseen()
                                    .is_some());
                        let mut card_rows: Option<(Vec<String>, crate::claude_card::FenceFrame)> =
                            None;
                        let (c_signed, shell_executing);
                        let mut group_notes = crate::holder_image::GroupNotes::default();
                        {
                            let waited = term_busy(&term).then(Instant::now);
                            let mut t = {
                                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                                let _cost = crate::recording_cost::begin(
                                    recording_costs.as_deref().map(|cost| &cost.reader_lock),
                                    Some(1),
                                );
                                term_lock(&term)
                            };
                            crate::metrics::note_parse_hold(parse_hold_wait(waited));
                            crate::temporal::pick_up(
                                &temporal_arm,
                                &mut temporal_lane.0,
                                &mut temporal_lane.1,
                            );
                            let clock = aterm_core::terminal::ClockReading::now();
                            let card_from = card_rows_wanted.then(|| t.screen_mutations());
                            t.holder_rings();
                            cuts.observe(emitted, &t, &cast_watermark);
                            #[cfg(unix)]
                            let mut journal_pre = journal.as_ref().map(|j| j.pre(&t));
                            if let (Some(e), Some(v)) = (edge, verdicts)
                                && e.from != e.to
                            {
                                let spine_pre = temporal_lane
                                    .0
                                    .as_ref()
                                    .map(|_| crate::temporal::SpinePre::take(&t));
                                handback_at = None;
                                fg_owners.observe(
                                    e.from,
                                    t.program_evidence(),
                                    t.take_evidence_asserted(),
                                );
                                let orphaned = fg_owners.orphaned_by(e.to, |p| v.gone(p)).is_some();
                                let run = if orphaned || v.gone(e.from) {
                                    t.foreground_handback_scoped_at(orphaned, clock)
                                } else {
                                    None
                                };
                                slice_holder = e.to;
                                fg_owners.observe(
                                    e.to,
                                    t.program_evidence(),
                                    t.take_evidence_asserted(),
                                );
                                if let Some(h) = run {
                                    emitted += h.bytes.len();
                                    if let (Some(tx), Some(pre)) = (&temporal_lane.0, spine_pre) {
                                        let input =
                                            crate::temporal::InputSlice::at(&h.bytes[..], clock);
                                        temporal_lane.1.record(tx, pre, &t, input);
                                    }
                                    #[cfg(unix)]
                                    if let (Some(j), Some(pre)) = (&journal, journal_pre.as_mut()) {
                                        j.record(&t, pre, &h.bytes, true);
                                    }
                                    handback = Some(h);
                                    crate::metrics::note_fg_handback();
                                }
                            }
                            if let (Some(e), Some(v)) = (edge, verdicts) {
                                handback_at = None;
                                slice_holder = e.to;
                                note_cut(
                                    &mut t,
                                    aterm_core::terminal::HolderCut {
                                        to: drawer_to.filter(|_| lay_at.is_none()),
                                        kind: match e.kind {
                                            crate::holder_image::EdgeKind::Group => {
                                                aterm_core::terminal::CutKind::Group
                                            }
                                            crate::holder_image::EdgeKind::Exec => {
                                                aterm_core::terminal::CutKind::Exec
                                            }
                                        },
                                        from_alive: !v.gone(e.from),
                                    },
                                    edge_group,
                                    &mut group_notes,
                                );
                            } else if lay_at == Some(off) {
                                lay_at = None;
                                note_cut(
                                    &mut t,
                                    aterm_core::terminal::HolderCut {
                                        to: drawer_to,
                                        kind: aterm_core::terminal::CutKind::Group,
                                        from_alive: true,
                                    },
                                    edge_group,
                                    &mut group_notes,
                                );
                            }
                            if let Some((cut, group)) = arm_cut {
                                note_cut(&mut t, cut, group, &mut group_notes);
                            }
                            t.set_foreground_job(slice_holder > 0 && slice_holder != fg_shell);
                            #[cfg(test)]
                            crate::holder_image::hold_hook::at_hold(
                                id,
                                off,
                                &bytes[off..end],
                                t.holder_fence().current_token(),
                                || {
                                    timeline
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .holder_image()
                                        .and_then(|i| i.token)
                                },
                            );
                            let slice_pre = temporal_lane
                                .0
                                .as_ref()
                                .map(|_| crate::temporal::SpinePre::take(&t));
                            let started_before = t.started_command_seq();
                            let block_before = crate::telemetry_host::mark(&t);
                            let unread = {
                                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                                let _cost = crate::recording_cost::begin(
                                    recording_costs.as_deref().map(|cost| &cost.parse),
                                    u64::try_from(end - off).ok(),
                                );
                                launch.process(&mut t, &bytes[off..end], clock, &person)
                            };
                            if let Some(unread) = unread {
                                launch_unread = Some(unread);
                            }
                            c_signed = t.started_command_seq() != started_before
                                && t.is_require_shell_integration_nonce();
                            shell_executing =
                                t.shell_state() == aterm_core::terminal::ShellState::Executing;
                            if let Some(places) = places.borrow_mut().as_mut() {
                                let running = t
                                    .current_block()
                                    .filter(|b| b.state == aterm_types::BlockState::Executing);
                                places.parsed(end, running.map(|b| b.id));
                            }
                            resync_due |= t.take_alt_screen_resync_request();
                            #[cfg(unix)]
                            crate::spare::witness_after_process(&witness, &mut t, master, || {
                                probe_sink.upgrade().map(|sink| sink.driven_input())
                            });
                            prefetch.held(&t);
                            emitted += end - off;
                            fg_owners.observe(
                                slice_holder,
                                t.program_evidence(),
                                t.take_evidence_asserted(),
                            );
                            let (slice_reports, slice_response) =
                                (t.take_reports(), t.take_response());
                            let slice_reply = temporal_lane.0.as_ref().and_then(|_| {
                                crate::temporal::reply_of(
                                    slice_response.as_deref(),
                                    slice_reports.as_deref(),
                                )
                            });
                            if let Some(r) = slice_reports {
                                match &mut reports {
                                    Some(a) => a.extend_from_slice(&r),
                                    None => reports = Some(r),
                                }
                            }
                            if let Some(r) = slice_response {
                                match &mut acc {
                                    Some(a) => a.extend_from_slice(&r),
                                    None => acc = Some(r),
                                }
                            }
                            backlog = t.lazy_backlog_len();
                            let moved = screen_moves.fed(crate::control::screen_gen(&t));
                            job_feed.fed(&t, &bytes[off..end], moved, Instant::now);
                            job_feed.meter().slice(block_before, &t, fg_shell);
                            if let (Some(tx), Some(pre)) = (&temporal_lane.0, slice_pre)
                                && temporal_lane.1.needs_input(tx)
                            {
                                let input = match &spine_copy {
                                    Some(copy) => crate::temporal::InputSlice::new(
                                        copy.clone(),
                                        off..end,
                                        clock,
                                    ),
                                    // Armed mid-batch: this batch was not copied.
                                    None => {
                                        crate::temporal::InputSlice::at(&bytes[off..end], clock)
                                    }
                                }
                                .replying(slice_reply);
                                temporal_lane.1.record(tx, pre, &t, input);
                            }
                            #[cfg(unix)]
                            if let (Some(j), Some(pre)) = (&journal, journal_pre.as_mut()) {
                                j.record(&t, pre, &bytes[off..end], false);
                                journal_keyframe = j.decide(&t, pre);
                            }
                            if let Some(from) = card_from {
                                card_rows = Some((
                                    (0..usize::from(t.rows()))
                                        .map(|r| crate::control::visible_row(&t, r))
                                        .collect(),
                                    crate::claude_card::FenceFrame {
                                        holder: t.holder_view(),
                                        base_y: i64::try_from(t.grid().base_y())
                                            .unwrap_or(i64::MAX),
                                        alt: t.is_alternate_screen(),
                                        mutations: from,
                                    },
                                ));
                            }
                        }
                        #[cfg(unix)]
                        if let (Some(j), Some(captured)) = (&journal, journal_keyframe.take()) {
                            j.write(captured);
                        }
                        if !group_notes.is_empty() {
                            group_notes.apply(
                                timeline
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .drawer_groups_mut(),
                            );
                        }
                        if let Some((image, kind)) = offered {
                            publish_holder(image, kind, arm.as_mut(), &mut cadence);
                        }
                        if let Some(a) = arm.as_mut() {
                            a.note_shell(c_signed, shell_executing);
                        }
                        if let Some((rows, fence)) = card_rows {
                            note_card_seen_in(
                                id,
                                &timeline,
                                &mut card_probe,
                                &mut probed_batch,
                                (&rows, &fence),
                            );
                        }
                        drop(fetched);
                        off = end;
                        crate::yield_to_ui_waiter(&ui_waiting);
                    }
                    (acc, reports)
                };
                if let (Some(h), Some(e)) = (&handback, edge) {
                    let payload = crate::foreground_handback::record_restored(
                        &mut timeline.lock().unwrap_or_else(|p| p.into_inner()),
                        e,
                        &h.reverted,
                        h.bytes.len(),
                    );
                    aterm_log::info!("session {id}: foreground handback: {payload}");
                }
                if let Some((req, report, _)) = &manual_run {
                    crate::manual_reset::record(id, &timeline, req.source, report);
                    if let Some(reply) = &req.reply {
                        let _ = reply.try_send(report.clone());
                    }
                }
                if backlog >= COMPRESS_SIGNAL_AT {
                    compress_tx.signal();
                }
                if let Some(resp) = response {
                    let resp: std::sync::Arc<[u8]> = resp.into();
                    if let Some(tx) = &temporal_lane.0 {
                        temporal_lane.1.reply(tx);
                    }
                    let _ = reply_tx.submit(resp);
                }
                if let Some(report) = reports {
                    let report: std::sync::Arc<[u8]> = report.into();
                    if let Some(tx) = &temporal_lane.0 {
                        temporal_lane.1.reply(tx);
                    }
                    let _ = reply_tx.submit_report(report);
                }
                let woke = Instant::now();
                output_wake(&mut cadence);
                if let Some(unread) = launch_unread {
                    launch.read_screen(unread);
                }
                let buf: std::borrow::Cow<'_, [u8]> = match &manual_run {
                    Some((_, _, reset_bytes)) if !reset_bytes.is_empty() => {
                        crate::foreground_handback::splice_at(buf, 0, reset_bytes).into()
                    }
                    _ => std::borrow::Cow::Borrowed(buf),
                };
                let manual_len = manual_run.as_ref().map_or(0, |(_, _, b)| b.len());
                let burst: std::sync::Arc<[u8]> = match (&handback, edge) {
                    (Some(h), Some(e)) => {
                        crate::foreground_handback::splice_at(&buf, e.at + manual_len, &h.bytes)
                            .into()
                    }
                    _ => match (&buf, spine_copy) {
                        (std::borrow::Cow::Borrowed(_), Some(copy)) => copy,
                        _ => std::sync::Arc::from(&buf[..]),
                    },
                };
                debug_assert_eq!(emitted, burst.len(), "cast cuts address the spliced burst");
                let at = arrived.unwrap_or(woke);
                if !burst.is_empty() || cuts.held() {
                    cast.lock().unwrap_or_else(|p| p.into_inner()).record_burst(
                        burst.clone(),
                        cuts,
                        at.saturating_duration_since(cast_epoch),
                    );
                }
                publish_burst(&byte_fanout, &burst, crate::metrics::us_at(at), || {
                    watchers.ring_watchers()
                });
                if resync_due {
                    crate::alt_screen_resync::start(&term, master);
                }
            };
            #[cfg(windows)]
            loop {
                // Stop only before a read: bytes out of the kernel queue must reach the engine.
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let r = match aterm_pty::read_or_wake(master, &mut buf, wake_rd, -1, -1) {
                    aterm_pty::ReadOutcome::Wake => break,
                    aterm_pty::ReadOutcome::Idle => {
                        // Wake-pipe-less fallback (fd exhaustion): honor the fd-free stop flag
                        // `Session::drop` raises, so an orphan-pinned reader still exits.
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        continue;
                    }
                    aterm_pty::ReadOutcome::Proc => continue,
                    aterm_pty::ReadOutcome::Eof | aterm_pty::ReadOutcome::Exited => {
                        let _ = pty_closed_at.set(Instant::now());
                        let _ = post(Wake::Exit {
                            session: id,
                            window,
                        });
                        break;
                    }
                    aterm_pty::ReadOutcome::Data(n) => {
                        let read_at = Instant::now();
                        let mut filled = aterm_pty::drain_more(master, &mut buf, n);
                        let since_epoch = |at: Instant| {
                            u64::try_from(at.saturating_duration_since(lat_epoch).as_nanos())
                                .unwrap_or(u64::MAX)
                        };
                        let read_ns = since_epoch(read_at);
                        latest_output_activity_ns.store(read_ns.max(1), Ordering::Release);
                        if let Some(until) = begin_held_gather(
                            &output_wake_pending,
                            read_ns,
                            filled,
                            crate::input_hint::pending(),
                        ) {
                            filled = gather_held_output(
                                &mut buf,
                                filled,
                                until,
                                || since_epoch(Instant::now()),
                                |b, f| aterm_pty::drain_available(master, b, f),
                                std::thread::sleep,
                                || {
                                    !stop.load(std::sync::atomic::Ordering::Acquire)
                                        && !crate::input_hint::pending()
                                },
                            );
                            latest_output_activity_ns
                                .store(since_epoch(Instant::now()).max(1), Ordering::Release);
                        }
                        (filled, read_at)
                    }
                };
                ingest(&mut temporal_lane, &buf[..r.0], None, 0, None, Some(r.1));
                // Parsed, and the term lock let go: list the job on this thread, at its pace or
                // at a block's start (`crate::command_ran::RanJob::batch`).
                if let Some(jobs) = jobs.as_mut() {
                    jobs.batch(&buf[..r.0], Instant::now(), crate::command_ran::wall_ms());
                }
            }
            #[cfg(unix)]
            loop {
                let next = match journal.as_ref().filter(|j| j.dirty()) {
                    Some(j) => match filled_rx.recv_timeout(crate::crash_restore::QUIET) {
                        Ok(msg) => Ok(msg),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            j.on_quiet(&term);
                            continue;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            Err(std::sync::mpsc::RecvError)
                        }
                    },
                    None => filled_rx.recv(),
                };
                match next {
                    Ok(GatherMsg::Started) => {
                        let _ = post(Wake::Ready {
                            session: id,
                            window,
                        });
                    }
                    Ok(GatherMsg::CaughtUp) => {
                        if park.0.caught_up(park.1) {
                            watchers.ring_watchers();
                        }
                    }
                    Ok(GatherMsg::Edge(edge, holder, image)) => {
                        ingest(&mut temporal_lane, &[], Some(edge), holder, image, None);
                    }
                    Ok(GatherMsg::Gone(pid)) => {
                        #[cfg(test)]
                        if crate::holder_image::hold_hook::no_retire(id) {
                            continue;
                        }
                        retire_gone(&term, &timeline, &temporal_arm, &mut temporal_lane, pid);
                    }
                    Ok(GatherMsg::Data(batch, len, edge, holder, image, end)) => {
                        // The parse stops at each place an exec waits for, and reads the block
                        // running there: every C and D the shell wrote before it is parsed, and
                        // the exec counts from now, placed in that block.
                        let mut batch_places = crate::command_ran::Places::default();
                        ran.places_in(end, len, &mut batch_places);
                        *places.borrow_mut() = Some(batch_places);
                        ingest(&mut temporal_lane, &batch[..len], edge, holder, image, None);
                        let batch_places = places.borrow_mut().take().unwrap_or_default();
                        ran.parsed_through(end, len, &batch_places);
                        let _ = free_tx.try_send(batch);
                        parse_in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(GatherMsg::Eof) => {
                        let _ = pty_closed_at.set(Instant::now());
                        let _ = post(Wake::Exit {
                            session: id,
                            window,
                        });
                        break;
                    }
                    Ok(GatherMsg::Exited) => {
                        let _ = pty_closed_at.set(Instant::now());
                        let _ = post(Wake::Exit {
                            session: id,
                            window,
                        });
                    }
                    Ok(GatherMsg::Wake) | Err(_) => break,
                    Ok(GatherMsg::Reset(req)) => {
                        pending_reset.set(Some(req));
                        ingest(&mut temporal_lane, &[], None, 0, None, None);
                    }
                    Ok(GatherMsg::Journal) => {
                        if let Some(j) = &journal {
                            j.on_nudge(&term);
                        }
                    }
                    Ok(GatherMsg::Barrier(asked)) => {
                        for waiter in asked {
                            let _ = waiter.try_send(());
                        }
                    }
                }
            }
            #[cfg(unix)]
            reset_lane.close_if(lane_generation);
            #[cfg(windows)]
            aterm_pty::close_fd(wake_rd);
        },
    )
    .map_err(|error| format!("spawn PTY reader: {error}"))?;
    #[cfg(unix)]
    start_gather()?;
    Ok(parse)
}

fn ingest_chunk_width(pending: impl FnOnce() -> bool, remaining: usize) -> usize {
    const PROCESS_CHUNK: usize = 8 * 1024;
    const IDLE_CHUNK: usize = 16 * 1024;
    if remaining <= PROCESS_CHUNK {
        remaining
    } else if pending() {
        PROCESS_CHUNK
    } else {
        IDLE_CHUNK.min(remaining)
    }
}

fn stamp_output_arrival(last_output_ns: &AtomicU64, now_ns: u64) {
    // Load first: a flood between two presents finds the edge already stamped, and a plain
    // load leaves the cache line shared instead of a failing RMW.
    if last_output_ns.load(Ordering::Relaxed) == 0 {
        let _ = last_output_ns.compare_exchange(0, now_ns, Ordering::Relaxed, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "spawn_output_edge_stamp_tests.rs"]
mod output_edge_stamp_tests;

/// The age at which a posted `Wake::Output` still waiting for its handler is booked late
/// (`metrics` `output_wake_late`, a debug-level note), and the delay after which an idle event
/// loop turns once more to look at an armed latch.
pub(crate) const WAKE_LATCH_EXPIRY_NS: u64 = 100_000_000;

/// Bit 63 of an armed latch word.
pub(crate) const WAKE_LATCH_SENT: u64 = 1 << 63;

/// Bit 62 of a latch word.
pub(crate) const WAKE_LATCH_HELD: u64 = 1 << 62;

/// Bit 61 of a held latch word.
pub(crate) const WAKE_LATCH_DIRTY: u64 = 1 << 61;

/// Bit 60 of an armed latch word.
pub(crate) const WAKE_LATCH_STREAMING: u64 = 1 << 60;

/// The stamp bits of a latch word.
pub(crate) const WAKE_LATCH_STAMP: u64 = WAKE_LATCH_STREAMING - 1;

/// The shortest interval between two `Wake::Output` passes for one streaming session.
pub(crate) const OUTPUT_CADENCE_NS: u64 = 4_000_000;

/// How long a burst of posts, each under [`OUTPUT_CADENCE_NS`] after the one before, runs
/// before its session counts as streaming.
pub(crate) const STREAM_AFTER_NS: u64 = 16_000_000;

/// How many posts make a burst's session streaming even short of [`STREAM_AFTER_NS`].
pub(crate) const STREAM_AFTER_POSTS: u32 = 16;

/// The event loop's turn state as the PTY readers see it.
#[derive(Debug, Default)]
pub(crate) struct LoopIdle(AtomicU64);

impl LoopIdle {
    /// A turn is running (`new_events`, `window_event`).
    pub(crate) fn turn_began(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// `about_to_wait` is done and the loop is about to sleep.
    pub(crate) fn went_idle(&self, pass_ns: u64) {
        self.0.store(pass_ns.max(1), Ordering::Relaxed);
    }

    /// The idle stamp, `0` while a turn runs.
    pub(crate) fn since_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// What [`gated_output_wake`] did with one chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeGate {
    /// Nothing posted: the latch was armed (the chunk rides the wake already posted), another
    /// reader won the arm, or the send failed.
    Coalesced,
    /// A fresh wake on the latch's clear->armed edge.
    Posted,
    /// A stranded wake re-posted (see [`wake_stranded`]).
    Reposted,
    /// Nothing posted: the UI thread holds the latch ([`WAKE_LATCH_HELD`]) and the chunk left
    /// it dirty, so the hold's release runs the pass.
    Held,
}

/// Whether an armed latch `word` is a wake stranded with its loop wake-up.
pub(crate) const fn wake_stranded(word: u64, idle_since_ns: u64, now_ns: u64) -> bool {
    let at = word & WAKE_LATCH_STAMP;
    word & WAKE_LATCH_SENT != 0
        && idle_since_ns != 0
        && idle_since_ns < at
        && now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS
}

fn publish_burst(
    fanout: &crate::cast::ByteFanout,
    burst: &Arc<[u8]>,
    at_us: u64,
    wake: impl FnOnce(),
) {
    if !burst.is_empty() {
        fanout.tee(burst, at_us);
    }
    wake();
}

#[cfg(test)]
pub(crate) fn gated_output_wake_for_test(
    flag: &AtomicU64,
    now_ns: u64,
    send: impl FnOnce() -> bool,
) -> WakeGate {
    gated_output_wake(flag, now_ns, 0, false, send, || now_ns)
}

fn gated_output_wake(
    flag: &AtomicU64,
    now_ns: u64,
    idle_since_ns: u64,
    streaming: bool,
    send: impl FnOnce() -> bool,
    sent_ns: impl FnOnce() -> u64,
) -> WakeGate {
    let flags = if streaming { WAKE_LATCH_STREAMING } else { 0 };
    let arm = (now_ns & WAKE_LATCH_STAMP).max(1) | flags;
    let mut word = flag.load(Ordering::Relaxed);
    let posted = loop {
        let (from, posted) = if word == 0 {
            (0, WakeGate::Posted)
        } else if word & WAKE_LATCH_HELD != 0 {
            if word & WAKE_LATCH_DIRTY != 0 {
                return WakeGate::Held;
            }
            match flag.compare_exchange(
                word,
                word | WAKE_LATCH_DIRTY,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return WakeGate::Held,
                Err(now) => {
                    word = now;
                    continue;
                }
            }
        } else if wake_stranded(word, idle_since_ns, now_ns) {
            (word, WakeGate::Reposted)
        } else {
            return WakeGate::Coalesced;
        };
        match flag.compare_exchange(from, arm, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break posted,
            Err(now) if now & WAKE_LATCH_HELD != 0 => word = now,
            Err(_) => return WakeGate::Coalesced,
        }
    };
    if send() {
        let stamp =
            (sent_ns() & WAKE_LATCH_STAMP).max(arm & WAKE_LATCH_STAMP) | WAKE_LATCH_SENT | flags;
        let _ = flag.compare_exchange(arm, stamp, Ordering::Relaxed, Ordering::Relaxed);
        posted
    } else {
        let _ = flag.compare_exchange(arm, 0, Ordering::Relaxed, Ordering::Relaxed);
        WakeGate::Coalesced
    }
}

/// A reader's memory of its own output (`lat_epoch` ns).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PostCadence {
    last_ns: u64,
    burst_from_ns: u64,
    posts: u32,
}

impl PostCadence {
    fn continues(&self, now_ns: u64) -> bool {
        if self.last_ns == 0 {
            return false;
        }
        let quiet = now_ns.saturating_sub(self.last_ns);
        let streamed =
            self.dense() || self.last_ns.saturating_sub(self.burst_from_ns) >= STREAM_AFTER_NS;
        quiet < OUTPUT_CADENCE_NS || (streamed && quiet < STREAM_AFTER_NS)
    }

    const fn dense(&self) -> bool {
        self.posts >= STREAM_AFTER_POSTS
    }

    /// Whether a post at `now_ns` is a streaming session's.
    pub(crate) fn streaming(&self, now_ns: u64) -> bool {
        self.continues(now_ns)
            && (self.dense() || now_ns.saturating_sub(self.burst_from_ns) >= STREAM_AFTER_NS)
    }

    /// A post went out at `now_ns`.
    pub(crate) fn posted(&mut self, now_ns: u64) {
        if !self.continues(now_ns) {
            self.burst_from_ns = now_ns;
            self.posts = 0;
        }
        self.posts = self.posts.saturating_add(1);
        self.last_ns = now_ns;
    }

    /// A chunk fed at `now_ns` left a held latch dirty ([`WakeGate::Held`]).
    pub(crate) const fn held(&mut self, now_ns: u64) {
        self.last_ns = now_ns;
    }

    /// The batch about to be fed carries a foreground edge (a program started or ended).
    pub(crate) fn foreground_edge(&mut self, latch: &AtomicU64) {
        *self = Self::default();
        let mut word = latch.load(Ordering::Relaxed);
        while word & WAKE_LATCH_HELD != 0 {
            match latch.compare_exchange(word, 0, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(now) => word = now,
            }
        }
    }
}

/// The latch word a `Wake::Output` pass stores before its work, from the `word` it found, so a
/// chunk fed after the store posts again or leaves the hold dirty.
pub(crate) fn output_latch_after_pass(word: u64, now_ns: u64) -> u64 {
    if word & (WAKE_LATCH_STREAMING | WAKE_LATCH_HELD) == 0 {
        return 0;
    }
    WAKE_LATCH_HELD | (now_ns.saturating_add(OUTPUT_CADENCE_NS) & WAKE_LATCH_STAMP).max(1)
}

/// What [`release_output_hold`] found on one session's latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldRelease {
    /// Not held: clear, armed or posted, which is the wake protocol's business.
    NotHeld,
    /// Held until `due_ns` (`lat_epoch` ns).
    Pending { due_ns: u64 },
    /// Due, and nothing was fed since the pass that took it.
    Released,
    /// Due and dirty: the caller runs the session's pass, which takes the hold again
    /// ([`output_latch_after_pass`]).
    Due,
}

/// Release one session's output hold if it is due at `now_ns`.
pub(crate) fn release_output_hold(latch: &AtomicU64, now_ns: u64) -> HoldRelease {
    let mut word = latch.load(Ordering::Relaxed);
    loop {
        if word & WAKE_LATCH_HELD == 0 {
            return HoldRelease::NotHeld;
        }
        let due_ns = word & WAKE_LATCH_STAMP;
        if now_ns < due_ns {
            return HoldRelease::Pending { due_ns };
        }
        if word & WAKE_LATCH_DIRTY != 0 {
            return HoldRelease::Due;
        }
        match latch.compare_exchange(word, 0, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return HoldRelease::Released,
            Err(now) => word = now,
        }
    }
}

/// A read at least this big is fed as it is, and a held gather ([`gather_held_output`]) stops
/// once it holds this much.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_ENOUGH: usize = 16 * 1024;

/// How long before its hold falls due a held gather is fed, so its bytes are in the engine
/// when the hold's release runs the session's pass.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MARGIN_NS: u64 = 500_000;

/// The shortest held gather.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_MIN_NS: u64 = 1_000_000;

/// How often a held gather looks at the pipe while it waits.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_STEP_NS: u64 = 1_000_000;

/// How far past its request a held gather's wait is taken to wake, so the gather asks that
/// much less and ends on time.
#[cfg(any(windows, test))]
pub(crate) const HELD_GATHER_WAIT_SLACK_NS: u64 = 500_000;

/// Whether a reader that has just read `filled` bytes at `now_ns` gathers more before it feeds
/// them ([`gather_held_output`]), and until when (`lat_epoch` ns).
#[cfg(any(windows, test))]
pub(crate) fn begin_held_gather(
    latch: &AtomicU64,
    now_ns: u64,
    filled: usize,
    typing: bool,
) -> Option<u64> {
    if typing || filled >= HELD_GATHER_ENOUGH {
        return None;
    }
    let mut word = latch.load(Ordering::Relaxed);
    while word & WAKE_LATCH_DIRTY == 0 {
        if word & WAKE_LATCH_HELD == 0 {
            return None;
        }
        match latch.compare_exchange(
            word,
            word | WAKE_LATCH_DIRTY,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(now) => word = now,
        }
    }
    if word & WAKE_LATCH_HELD == 0 {
        return None;
    }
    let due_ns = word & WAKE_LATCH_STAMP;
    Some(due_ns.saturating_sub(HELD_GATHER_MARGIN_NS).clamp(
        now_ns.saturating_add(HELD_GATHER_MIN_NS),
        now_ns.saturating_add(OUTPUT_CADENCE_NS),
    ))
}

/// Gather a held session's output into `buf[..filled]` until `until_ns` ([`begin_held_gather`]).
#[cfg(any(windows, test))]
pub(crate) fn gather_held_output(
    buf: &mut [u8],
    mut filled: usize,
    until_ns: u64,
    mut now_ns: impl FnMut() -> u64,
    mut top_up: impl FnMut(&mut [u8], usize) -> usize,
    mut wait: impl FnMut(std::time::Duration),
    mut go_on: impl FnMut() -> bool,
) -> usize {
    let enough = HELD_GATHER_ENOUGH.min(buf.len());
    loop {
        filled = top_up(buf, filled);
        if filled >= enough || !go_on() {
            return filled;
        }
        let left = until_ns.saturating_sub(now_ns());
        if left <= HELD_GATHER_WAIT_SLACK_NS {
            return filled;
        }
        wait(std::time::Duration::from_nanos(
            (left - HELD_GATHER_WAIT_SLACK_NS).min(HELD_GATHER_STEP_NS),
        ));
    }
}

/// What the watchdog makes of one session's `Wake::Output` latch word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputWakeVerdict {
    /// No wake outstanding.
    Clear,
    /// The wake was posted before `drained_through_ns`, every wake posted before that instant
    /// has been handed out, and the latch is still armed.
    Lost { age_ns: u64 },
    /// Outstanding but not proven lost.
    Waiting { recheck_ns: u64, late: bool },
}

/// The watchdog's verdict on one latch word, on evidence rather than age.
pub(crate) fn judge_output_wake(
    word: u64,
    drained_through_ns: u64,
    now_ns: u64,
) -> OutputWakeVerdict {
    if word == 0 || word & WAKE_LATCH_HELD != 0 {
        return OutputWakeVerdict::Clear;
    }
    let at = word & WAKE_LATCH_STAMP;
    let age_ns = now_ns.saturating_sub(at);
    let late = age_ns > WAKE_LATCH_EXPIRY_NS;
    if word & WAKE_LATCH_SENT == 0 {
        return OutputWakeVerdict::Waiting {
            recheck_ns: now_ns.saturating_add(age_ns.max(WAKE_LATCH_EXPIRY_NS)),
            late,
        };
    }
    if at < drained_through_ns {
        return OutputWakeVerdict::Lost { age_ns };
    }
    OutputWakeVerdict::Waiting {
        recheck_ns: at
            .saturating_add(WAKE_LATCH_EXPIRY_NS + 1)
            .max(now_ns.saturating_add(WAKE_LATCH_EXPIRY_NS)),
        late,
    }
}

/// The lost-wake watchdog's memory between `about_to_wait` passes.
#[derive(Debug, Default)]
pub(crate) struct OutputWakeWatch {
    /// The pass before the latest turn whose drain emptied the user-event queue: every wake
    /// posted before it has been handed out.
    drained_through_ns: u64,
    last_pass_ns: u64,
    late_booked: Vec<(u64, u64)>,
}

/// One watchdog pass's outcome.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputWakePass {
    /// Latches proven lost and cleared this pass.
    pub(crate) healed: u32,
    /// Wakes newly booked late this pass (no action taken).
    pub(crate) late: u32,
    /// The earliest recheck any still-outstanding wake asks for.
    pub(crate) recheck_ns: Option<u64>,
}

impl OutputWakeWatch {
    /// [`Self::pass`] at the end of the turn `el` is about to wait after, the one way
    /// `App::about_to_wait` reaches the watch: whether that turn's drain emptied the user-event
    /// queue is the loop's own word ([`ActiveEventLoop::user_events_drained`]).
    pub(crate) fn pass_for_turn<'a>(
        &mut self,
        el: &ActiveEventLoop,
        now_ns: u64,
        latches: impl IntoIterator<Item = (u64, &'a AtomicU64)>,
    ) -> OutputWakePass {
        self.pass(now_ns, el.user_events_drained(), latches)
    }

    /// Judge every session's latch at `now_ns` (this `about_to_wait`'s stamp), heal the ones
    /// proven lost (a CAS from the observed word, so a concurrent handler clear or reader
    /// re-stamp is never clobbered), and book the late ones once. `drained` is whether this
    /// turn's drain emptied the user-event queue (`ActiveEventLoop::user_events_drained`): only
    /// then has every wake posted before the previous pass been handed out, so that pass
    /// becomes the drained-through instant (a drain that stopped part-way may leave such a wake
    /// queued).
    fn pass<'a>(
        &mut self,
        now_ns: u64,
        drained: bool,
        latches: impl IntoIterator<Item = (u64, &'a AtomicU64)>,
    ) -> OutputWakePass {
        if drained {
            self.drained_through_ns = self.last_pass_ns;
        }
        let drained_through_ns = self.drained_through_ns;
        let mut out = OutputWakePass::default();
        let mut still_late = Vec::new();
        for (session, latch) in latches {
            let word = latch.load(Ordering::Relaxed);
            match judge_output_wake(word, drained_through_ns, now_ns) {
                OutputWakeVerdict::Clear => {}
                OutputWakeVerdict::Lost { age_ns } => {
                    if latch
                        .compare_exchange(word, 0, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        out.healed += 1;
                        crate::metrics::note_wake_heal();
                        aterm_log::warn!(
                            "watchdog healed a lost output wake (latch stale {} ms; session {}: \
                             a user-event drain that emptied the queue ran after it was posted \
                             and no handler cleared it)",
                            age_ns / 1_000_000,
                            session
                        );
                    }
                }
                OutputWakeVerdict::Waiting { recheck_ns, late } => {
                    out.recheck_ns = Some(out.recheck_ns.map_or(recheck_ns, |r| r.min(recheck_ns)));
                    if late {
                        if !self.late_booked.contains(&(session, word)) {
                            out.late += 1;
                            crate::metrics::note_output_wake_late();
                            aterm_log::debug!(
                                "output wake late ({} ms; session {}): queued behind the \
                                 main thread's turn, not lost",
                                now_ns.saturating_sub(word & WAKE_LATCH_STAMP) / 1_000_000,
                                session
                            );
                        }
                        still_late.push((session, word));
                    }
                }
            }
        }
        self.late_booked = still_late;
        self.last_pass_ns = now_ns;
        out
    }
}

/// One session's [`OutputWakeWatch`], handed each turn as `App::about_to_wait` hands it
/// ([`OutputWakeWatch::pass_for_turn`]), for `tests/headless_event_loop.rs`: its display-free
/// loop ends a turn whose drain met a send in progress, which no unit test can build.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct OutputWakeWatchProbe {
    watch: OutputWakeWatch,
    latch: AtomicU64,
}

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
impl OutputWakeWatchProbe {
    /// Post the session's output wake at `sent_ns` (`lat_epoch` ns) as its PTY reader posts one,
    /// and leave it unhandled: whether it was posted (the latch was clear).
    pub fn post(&self, sent_ns: u64) -> bool {
        gated_output_wake(&self.latch, sent_ns, 0, false, || true, || sent_ns) == WakeGate::Posted
    }

    /// End the turn `el` is about to wait after at `now_ns`: whether the watch healed the
    /// session's wake as lost.
    pub fn end_turn(&mut self, el: &ActiveEventLoop, now_ns: u64) -> bool {
        let pass = self.watch.pass_for_turn(el, now_ns, [(0, &self.latch)]);
        pass.healed > 0
    }
}

impl crate::App {
    /// Register a session's live handle in the process-wide registry.
    pub(crate) fn register_session(
        store: &session_store::Store,
        session: &Session,
        parent: Option<SessionId>,
    ) {
        let handle = session_store::SessionHandle {
            sid: session.ctx.self_id.clone(),
            nonce: session.ctx.nonce,
            local_id: session.id,
            parent,
            state: session_store::SessionState::Spawning,
            title: term_lock(&session.term).title().to_string(),
            term: session.term.clone(),
            master: session.master,
            shell_identity: session.shell_identity,
            ctx: session.ctx.clone(),
        };
        crate::fabric::register_on_log(&session.ctx, || {
            let mut registry = store.write().unwrap_or_else(|p| p.into_inner());
            registry.register(handle);
            if session.frozen_path {
                registry.mark_frozen_path(session.id);
            }
            if session.rekey_channel {
                registry.mark_rekey_channel(session.id);
            }
            if session.body_loader {
                registry.mark_body_loader(session.id);
            }
            if session.integration_frozen {
                registry.mark_integration_frozen(session.id);
            }
            if let Some(history) = session.handoff_history.as_ref() {
                registry.add_history_lost(session.id, history.lost);
            }
            drop(registry);
        });
        crate::proxy::publish_session(&session.ctx.self_id, &session.ctx.nonce);
        if session.handoff_local_id.is_some() {
            let _ = crate::program_worker::resume(store, session);
        }
    }
}

/// The directory holding aterm's co-located CLI tools (`aterm-ctl`, `atpkg`), put on child
/// sessions' path so the bundled Help's workflows run with no install step: a macOS .app
/// bundle's `Contents/MacOS`, else [`windows_app_dir`] or [`linux_store_dir`].
pub(crate) fn bundle_dir(exe: Option<&std::path::Path>) -> Option<String> {
    if cfg!(windows) {
        return windows_app_dir(exe?, |path| path.is_file());
    }
    if cfg!(target_os = "linux") {
        return linux_store_dir(exe?, |path| std::fs::canonicalize(path).ok());
    }
    let dir = exe?.parent()?;
    if !dir.ends_with("Contents/MacOS") {
        return None;
    }
    dir.to_str().map(str::to_owned)
}

/// [`bundle_dir`]'s Windows rule.
pub(crate) fn windows_app_dir(
    exe: &std::path::Path,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> Option<String> {
    let dir = exe.parent()?;
    if dir.as_os_str().is_empty() || !is_file(&dir.join("aterm.exe")) {
        return None;
    }
    dir.to_str().map(str::to_owned)
}

/// [`bundle_dir`]'s Linux rule.
pub(crate) fn linux_store_dir(
    exe: &std::path::Path,
    resolve: impl Fn(&std::path::Path) -> Option<std::path::PathBuf>,
) -> Option<String> {
    atpkg::hooks::store_dir_of(exe, resolve)?
        .to_str()
        .map(str::to_owned)
}

/// The two environment variables that make a no-terminal `sudo` reach aterm's askpass helper
/// (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.1).
pub(crate) fn askpass_env(
    bundle: Option<&str>,
    contained: bool,
    inherited_askpass: Option<&str>,
    inherited_display: Option<&str>,
) -> Vec<(String, String)> {
    if !cfg!(target_os = "macos") || contained {
        return Vec::new();
    }
    let Some(bundle) = bundle else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let set_askpass = match inherited_askpass {
        None => true,
        Some(v) => v.ends_with("/aterm-askpass"),
    };
    if set_askpass {
        out.push((
            "SUDO_ASKPASS".to_string(),
            format!("{bundle}/aterm-askpass"),
        ));
    }
    if inherited_display.is_none() {
        out.push(("DISPLAY".to_string(), String::new()));
    }
    out
}

/// The one `("PATH", value)` pair the baseline `env_add` carries, composed because
/// `aterm_pty::build_child_env` applies `env_add` by key overwrite
/// (`docs/DESIGN-toolchain-reroute-2026-09-07.md` §"Reaching PATH").
pub(crate) fn reroute_path_env(
    reroute_dir: Option<&str>,
    agents_dir: Option<&str>,
    bundle_dir: Option<&str>,
    inherited: Option<&str>,
    bin_dir: Option<&str>,
) -> Option<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut entries: Vec<&str> = inherited
        .filter(|p| !p.is_empty())
        .map(|p| p.split(sep).collect())
        .unwrap_or_default();
    let mut injected = false;
    if let Some(bundle) = bundle_dir
        && !entries.contains(&bundle)
    {
        entries.insert(0, bundle);
        injected = true;
    }
    if let Some(bin) = bin_dir {
        entries.retain(|entry| *entry != bin);
        entries.insert(0, bin);
        injected = true;
    }
    if let Some(agents) = agents_dir {
        entries.retain(|entry| *entry != agents);
        entries.insert(0, agents);
        injected = true;
    }
    if let Some(reroute) = reroute_dir {
        entries.retain(|entry| *entry != reroute);
        entries.insert(0, reroute);
        injected = true;
    }
    injected.then(|| ("PATH".to_string(), entries.join(&sep.to_string())))
}

/// The managed `agents/` directory every session gets in front of its path, created at launch
/// if missing (one `mkdir -p`; nothing blocks the first frame), so the first tab on a fresh
/// machine already has it on path and finds the twins atpkg lays later.
pub(crate) fn managed_agents_dir(layout: &atpkg::store::Layout) -> Option<String> {
    match layout.ensure_agents_dir() {
        Ok(dir) => dir.to_str().map(str::to_owned),
        Err(error) => {
            eprintln!(
                "{}",
                atpkg::store::agents_dir_refusal_line(
                    &layout.agents_dir(),
                    &error,
                    layout.is_system_prefix(),
                    "this window's sessions"
                )
            );
            None
        }
    }
}

/// [`atpkg_child_path`], resolved once per process.
pub(crate) fn atpkg_child_path_once() -> &'static str {
    ATPKG_CHILD_PATH.get_or_init(atpkg_child_path)
}

static ATPKG_CHILD_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The path the package lane hands an update child (`PkgLane::child_path`).
pub(crate) fn atpkg_update_path(waited: bool) -> std::borrow::Cow<'static, str> {
    update_path_with(
        waited,
        ATPKG_CHILD_PATH.get().map(String::as_str),
        atpkg_child_path_once,
        atpkg_fallback_path,
    )
}

fn update_path_with(
    waited: bool,
    ready: Option<&'static str>,
    wait: impl FnOnce() -> &'static str,
    fallback: impl FnOnce() -> String,
) -> std::borrow::Cow<'static, str> {
    match ready {
        Some(path) => std::borrow::Cow::Borrowed(path),
        None if waited => std::borrow::Cow::Owned(fallback()),
        None => std::borrow::Cow::Borrowed(wait()),
    }
}

/// The path the co-located atpkg children run with.
pub(crate) fn atpkg_child_path() -> String {
    login_shell_path().unwrap_or_else(atpkg_fallback_path)
}

fn atpkg_fallback_path() -> String {
    let layout = atpkg::store::resolve_configured();
    let managed = layout.as_ref().and_then(|l| {
        Some((
            l.agents_dir().to_str()?.to_owned(),
            l.bin_dir().to_str()?.to_owned(),
        ))
    });
    fallback_child_path(
        std::env::var("PATH").ok().as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
        managed.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
    )
}

#[cfg(not(windows))]
const LOGIN_PATH_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(any(not(windows), test))]
const LOGIN_PATH_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

fn login_shell_path() -> Option<String> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    {
        let (shell, _) = aterm_pty::select_shell_with_origin(None);
        let shell = path_probe_shell(&shell.to_string_lossy(), cfg!(target_os = "macos"));
        login_shell_path_with(
            &shell,
            &["-l", "-i", "-c", "printf '%s\\n' \"$PATH\""],
            LOGIN_PATH_BUDGET,
        )
    }
}

#[cfg(not(windows))]
fn path_probe_shell(account_shell: &str, macos: bool) -> String {
    let name = std::path::Path::new(account_shell)
        .file_name()
        .and_then(|n| n.to_str());
    let posix_family = matches!(name, Some("sh" | "dash" | "ksh" | "mksh"));
    if matches!(name, Some("zsh" | "bash" | "fish")) || (!macos && posix_family) {
        account_shell.to_string()
    } else if macos {
        "/bin/zsh".to_string()
    } else {
        "/bin/sh".to_string()
    }
}

#[cfg(test)]
thread_local! {
    static LOGIN_SHELL_ASKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(not(windows), test))]
fn login_shell_path_with(
    program: &str,
    args: &[&str],
    budget: std::time::Duration,
) -> Option<String> {
    use aterm_uds::spawnfd::Tether as _;
    #[cfg(test)]
    LOGIN_SHELL_ASKS.with(|asks| asks.set(asks.get() + 1));
    let mut command = crate::qos::command(crate::qos::Role::Background, program);
    // Tethered: a login shell can wait on its rc files for ever, and only `budget` ends it.
    command
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .tether();
    #[cfg(unix)]
    command.setsid(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (chunks_tx, chunks) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = [0_u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if chunks_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(budget)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
    #[cfg(unix)]
    let mut exit_watch = aterm_uds::exitwatch::exit_fd(child.id());
    let answered = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                #[cfg(unix)]
                if let Some(watch) = exit_watch.as_ref() {
                    use std::os::fd::AsRawFd as _;
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    let fired = crate::control_listener::poll_readable(
                        watch.as_raw_fd(),
                        left.max(std::time::Duration::from_millis(1)),
                    );
                    let spent = match fired {
                        Ok(true) => matches!(child.try_wait(), Ok(None)),
                        Ok(false) => false,
                        Err(_) => true,
                    };
                    if spent {
                        exit_watch = None;
                    }
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                // The probe shell leads its own session (`setsid`) and is unreaped here, so the
                // sweep also ends a child an rc file was forking as the deadline's signal landed.
                #[cfg(unix)]
                if let Ok(group) = libc::pid_t::try_from(child.id()) {
                    let _ = aterm_uds::process::kill_group_until_empty(group);
                }
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !answered {
        return None;
    }
    let hard_stop = deadline.max(std::time::Instant::now()) + LOGIN_PATH_GRACE;
    let mut output = Vec::new();
    loop {
        let remaining = hard_stop.saturating_duration_since(std::time::Instant::now());
        let wait = if pick_login_path(&String::from_utf8_lossy(&output)).is_some() {
            LOGIN_PATH_GRACE.min(remaining)
        } else {
            remaining
        };
        match chunks.recv_timeout(wait) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    pick_login_path(&String::from_utf8(output).ok()?)
}

/// The path line out of a login shell's output.
#[cfg(any(not(windows), test))]
pub(crate) fn pick_login_path(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/') && line.contains(':'))
        .map(str::to_string)
}

/// The fallback child path.
pub(crate) fn fallback_child_path(
    process_path: Option<&str>,
    home: Option<&std::path::Path>,
    managed: Option<(&str, &str)>,
) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut entries: Vec<String> = process_path
        .filter(|p| !p.is_empty())
        .map(|p| p.split(sep).map(str::to_string).collect())
        .unwrap_or_default();
    let mut extra: Vec<String> = Vec::new();
    if let Some(home) = home {
        extra.push(home.join(".local/bin").to_string_lossy().into_owned());
    }
    if cfg!(target_os = "macos") {
        extra.push("/opt/homebrew/bin".to_string());
    }
    if cfg!(unix) {
        extra.push("/usr/local/bin".to_string());
    }
    for dir in extra {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    if let Some((agents, bin)) = managed {
        entries.retain(|e| e != agents && e != bin);
        entries.insert(0, bin.to_string());
        entries.insert(0, agents.to_string());
    }
    entries.join(&sep.to_string())
}

#[cfg(test)]
#[path = "spawn_reroute_path_env_tests.rs"]
mod reroute_path_env_tests;

#[cfg(test)]
#[path = "spawn_clipboard_coalesce_tests.rs"]
mod clipboard_coalesce_tests;

#[cfg(test)]
#[path = "spawn_host_reach_tests.rs"]
mod host_reach_tests;

#[cfg(test)]
#[path = "spawn_publish_burst_tests.rs"]
mod publish_burst_tests;

#[cfg(test)]
#[path = "spawn_held_gather_tests.rs"]
mod held_gather_tests;

#[cfg(test)]
#[path = "spawn_output_wake_coalesce_tests.rs"]
mod output_wake_coalesce_tests;

#[cfg(test)]
#[path = "spawn_kitty_transfer_tests.rs"]
mod kitty_transfer_tests;

#[cfg(test)]
#[path = "spawn_adoption_render_debt_tests.rs"]
mod adoption_render_debt_tests;

#[cfg(test)]
#[path = "spawn_askpass_env_tests.rs"]
mod askpass_env_tests;
